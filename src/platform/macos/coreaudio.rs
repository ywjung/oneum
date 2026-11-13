//! CoreAudio device control that CPAL does not offer: exclusive access (hog mode), the
//! nominal sample rate and the physical stream format (bit depth) of an output device.
//!
//! Everything here is a plain property read or write on the audio HAL; it may be called
//! from any thread. Nothing is cached: the HAL is the source of truth, because the user
//! (Audio MIDI Setup) and other apps change the same properties.
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, AudioStreamRangedDescription,
    kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyHogMode,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreams,
    kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioStreamPropertyAvailablePhysicalFormats,
    kAudioStreamPropertyPhysicalFormat,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, AudioValueRange, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsNonMixable, kAudioFormatLinearPCM,
};
use objc2_core_foundation::CFString;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// Prefix of CPAL's device id strings on macOS; the rest is the CoreAudio device UID.
const CPAL_ID_PREFIX: &str = "coreaudio:";
const POLL: Duration = Duration::from_millis(10);

/// One physical (hardware-side) stream format of a device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalFormat {
    /// 0 when the format is offered over a range of rates (see `min_rate`/`max_rate`).
    pub sample_rate: u32,
    /// Rates this format is offered at (equal for a discrete format).
    pub min_rate: u32,
    pub max_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub float: bool,
    /// Linear PCM (anything else, e.g. AC-3 pass-through, is never chosen).
    pub pcm: bool,
    /// The system mixer can use the format. Devices list every format a second time as
    /// "non-mixable"; choosing one of those shuts every other app out of the device.
    pub mixable: bool,
    asbd: AudioStreamBasicDescription,
}

impl PhysicalFormat {
    fn from_asbd(asbd: AudioStreamBasicDescription, range: Option<AudioValueRange>) -> Self {
        let rate = asbd.mSampleRate.round().max(0.0) as u32;
        let (min_rate, max_rate) = match range {
            Some(r) if r.mMaximum > 0.0 => (
                r.mMinimum.round().max(0.0) as u32,
                r.mMaximum.round().max(0.0) as u32,
            ),
            _ => (rate, rate),
        };
        Self {
            sample_rate: rate,
            min_rate,
            max_rate,
            bits: asbd.mBitsPerChannel,
            channels: asbd.mChannelsPerFrame,
            float: asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0,
            pcm: asbd.mFormatID == kAudioFormatLinearPCM,
            mixable: asbd.mFormatFlags & kAudioFormatFlagIsNonMixable == 0,
            asbd,
        }
    }

    pub fn supports_rate(&self, rate: u32) -> bool {
        (self.min_rate..=self.max_rate).contains(&rate)
    }
}

/// A connected audio device, addressed by its HAL object id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDevice {
    id: AudioObjectID,
}

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property. `T` must be a plain C value type for which all-zero bytes
/// are valid (the callers below use f64, i32, u32 and CoreAudio structs of those).
fn get<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<T> {
    let addr = address(selector, scope);
    let mut size = size_of::<T>() as u32;
    // SAFETY: `T` is plain old data (see above), so zeroed memory is a valid value.
    let mut value: T = unsafe { std::mem::zeroed() };
    // SAFETY: the address, the size and the output buffer are valid for the call, and the
    // buffer is exactly `size` bytes. The HAL writes at most `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0 && size as usize == size_of::<T>()).then_some(value)
}

/// Reads a variable-length property as a list of `T` (same requirement on `T` as [`get`]).
fn get_list<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Vec<T> {
    let addr = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: valid address and size pointers; no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    let count = size as usize / size_of::<T>().max(1);
    if status != 0 || count == 0 {
        return Vec::new();
    }
    // SAFETY: `T` is plain old data, so zeroed elements are valid values.
    let mut items: Vec<T> = vec![unsafe { std::mem::zeroed() }; count];
    let mut size = (count * size_of::<T>()) as u32;
    // SAFETY: `items` holds `size` writable bytes; the HAL reports back how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(items.as_mut_ptr()).map_or(NonNull::dangling(), NonNull::cast),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    // The list may have shrunk between the two calls.
    items.truncate(size as usize / size_of::<T>().max(1));
    items
}

fn set<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    value: &T,
) -> bool {
    let addr = address(selector, scope);
    // A copy of our own: for some properties (hog mode) the HAL writes the resulting
    // value back through this pointer, which must not be the caller's shared reference.
    let mut data: T = *value;
    // SAFETY: the address and the value are valid for the call; the size matches the
    // value, and `data` is a local the HAL may write `size_of::<T>()` bytes into.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            size_of::<T>() as u32,
            NonNull::from(&mut data).cast(),
        )
    };
    status == 0
}

/// Polls `done` until it holds or `timeout` passes. Property changes on the HAL are
/// asynchronous: a successful "set" only means the request was accepted.
fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

impl AudioDevice {
    /// Finds the device behind a CPAL device id (`"coreaudio:<uid>"`) or a bare UID.
    pub fn from_cpal_id(id: &str) -> Option<Self> {
        let uid = id.strip_prefix(CPAL_ID_PREFIX).unwrap_or(id);
        let cf_uid = CFString::from_str(uid);
        let qualifier: *const CFString = &*cf_uid;
        let addr = address(
            kAudioHardwarePropertyTranslateUIDToDevice,
            kAudioObjectPropertyScopeGlobal,
        );
        let mut device: AudioObjectID = kAudioObjectUnknown;
        let mut size = size_of::<AudioObjectID>() as u32;
        // SAFETY: the qualifier is a pointer to a CFStringRef that stays alive for the
        // call (`cf_uid` is dropped after it); the output buffer is one AudioObjectID.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&addr),
                size_of::<*const CFString>() as u32,
                (&raw const qualifier).cast::<c_void>(),
                NonNull::from(&mut size),
                NonNull::from(&mut device).cast(),
            )
        };
        (status == 0 && device != kAudioObjectUnknown).then_some(Self { id: device })
    }

    /// The device clock rate in Hz.
    pub fn nominal_rate(&self) -> Option<u32> {
        get::<f64>(
            self.id,
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        )
        .filter(|r| r.is_finite() && *r >= 1.0)
        .map(|r| r.round() as u32)
    }

    /// Rates the device can run at, as inclusive `(min, max)` ranges. Hardware reports
    /// discrete rates (`min == max`); virtual devices sometimes report real ranges.
    pub fn rate_ranges(&self) -> Vec<(u32, u32)> {
        get_list::<AudioValueRange>(
            self.id,
            kAudioDevicePropertyAvailableNominalSampleRates,
            kAudioObjectPropertyScopeGlobal,
        )
        .into_iter()
        .filter(|r| r.mMinimum.is_finite() && r.mMaximum.is_finite() && r.mMaximum >= 1.0)
        .map(|r| {
            (
                r.mMinimum.round().max(1.0) as u32,
                r.mMaximum.round() as u32,
            )
        })
        .collect()
    }

    /// Asks the device to run at `rate` and waits until it does.
    pub fn set_nominal_rate(&self, rate: u32, timeout: Duration) -> bool {
        if self.nominal_rate() == Some(rate) {
            return true;
        }
        set(
            self.id,
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
            &f64::from(rate),
        ) && wait_until(timeout, || self.nominal_rate() == Some(rate))
    }

    /// Process that has exclusive access to the device, if any. `None` also when the
    /// device does not answer (unplugged).
    pub fn hog_owner(&self) -> Option<i32> {
        get::<i32>(
            self.id,
            kAudioDevicePropertyHogMode,
            kAudioObjectPropertyScopeGlobal,
        )
        .filter(|pid| *pid >= 0)
    }

    /// True when this process has exclusive access.
    pub fn is_hogged_by_us(&self) -> bool {
        self.hog_owner() == Some(std::process::id() as i32)
    }

    /// Takes exclusive access for this process. Returns whether we hold it afterwards;
    /// false when another process holds it or the device refuses.
    pub fn take_hog(&self) -> bool {
        if self.is_hogged_by_us() {
            return true;
        }
        if self.hog_owner().is_some() {
            return false; // somebody else's
        }
        // Writing the property toggles ownership for the calling process; the value
        // written is ignored by the HAL (it reports the new owner back).
        let pid = std::process::id() as i32;
        set(
            self.id,
            kAudioDevicePropertyHogMode,
            kAudioObjectPropertyScopeGlobal,
            &pid,
        ) && wait_until(Duration::from_millis(500), || self.is_hogged_by_us())
    }

    /// Gives exclusive access back, if this process holds it.
    pub fn release_hog(&self) {
        if !self.is_hogged_by_us() {
            return; // writing would *take* it (the property is a toggle)
        }
        let free: i32 = -1;
        if set(
            self.id,
            kAudioDevicePropertyHogMode,
            kAudioObjectPropertyScopeGlobal,
            &free,
        ) {
            wait_until(Duration::from_millis(500), || !self.is_hogged_by_us());
        }
    }

    /// The device's first output stream (the one carrying the front channels).
    fn output_stream(&self) -> Option<AudioObjectID> {
        get_list::<AudioObjectID>(
            self.id,
            kAudioDevicePropertyStreams,
            kAudioObjectPropertyScopeOutput,
        )
        .into_iter()
        .next()
    }

    /// The format the hardware is running in right now.
    pub fn physical_format(&self) -> Option<PhysicalFormat> {
        let stream = self.output_stream()?;
        get::<AudioStreamBasicDescription>(
            stream,
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
        )
        .map(|asbd| PhysicalFormat::from_asbd(asbd, None))
    }

    /// Every format the hardware offers.
    pub fn physical_formats(&self) -> Vec<PhysicalFormat> {
        let Some(stream) = self.output_stream() else {
            return Vec::new();
        };
        get_list::<AudioStreamRangedDescription>(
            stream,
            kAudioStreamPropertyAvailablePhysicalFormats,
            kAudioObjectPropertyScopeGlobal,
        )
        .into_iter()
        .map(|d| PhysicalFormat::from_asbd(d.mFormat, Some(d.mSampleRateRange)))
        .collect()
    }

    /// Switches the hardware to `format` at `rate` and waits until it runs that way.
    /// `format` must come from [`AudioDevice::physical_formats`] or
    /// [`AudioDevice::physical_format`] of this device.
    pub fn set_physical_format(
        &self,
        format: &PhysicalFormat,
        rate: u32,
        timeout: Duration,
    ) -> bool {
        let Some(stream) = self.output_stream() else {
            return false;
        };
        let mut asbd = format.asbd;
        asbd.mSampleRate = f64::from(rate);
        let reached = || {
            self.physical_format().is_some_and(|now| {
                now.sample_rate == rate
                    && now.bits == format.bits
                    && now.float == format.float
                    && now.mixable == format.mixable
            }) && self.nominal_rate() == Some(rate)
        };
        if reached() {
            return true;
        }
        set(
            stream,
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
            &asbd,
        ) && wait_until(timeout, reached)
    }
}

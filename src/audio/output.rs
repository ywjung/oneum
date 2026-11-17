//! Output devices and the CPAL stream that hosts the [`Renderer`].
//!
//! Streams are always opened on a *specific* device. CPAL's default-device stream follows
//! the system default automatically, which would move playback to the speakers when
//! headphones are unplugged; we want to pause instead.
use super::render::{RenderLink, RenderShared, Renderer, render_pair};
use crate::platform::device::{AudioDevice, PhysicalFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// How long a device may take to switch its rate or format. USB DACs that click a relay
/// need a moment; the built-in output answers within milliseconds.
const FORMAT_SWITCH_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a device may take to change its sample format once the rate is right.
const BIT_DEPTH_SWITCH_TIMEOUT: Duration = Duration::from_millis(1500);

/// How the output device is used (docs/ARCHITECTURE.md, "출력 품질 옵션"). Everything is
/// off by default: the device is then left exactly as the system has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSettings {
    /// Run the device at the track's own sample rate (and its widest sample format)
    /// instead of converting the track to whatever rate the device happens to be at.
    pub match_sample_rate: bool,
    /// Take exclusive access to the device while playing: no other app can play through
    /// it or change its format.
    pub exclusive: bool,
    /// Load each file into memory while it plays (and the next one ahead of time), so
    /// playback does not depend on the disk.
    pub memory_playback: bool,
    /// Run the device at the highest whole multiple of the track's sample rate it
    /// offers (44.1 kHz → 352.8 or 176.4 kHz), and raise the track to it here.
    pub upsample: bool,
    /// The filter a track's rate is raised with when the output runs at a whole
    /// multiple of it (whether this setting or the device made it so).
    pub filter: super::upsample::Filter,
}

impl OutputSettings {
    /// True when the settings ask for anything that touches the device itself.
    pub fn touches_device(&self) -> bool {
        self.match_sample_rate || self.exclusive || self.upsample
    }

    /// The part of the settings that decides how the device is opened and what a
    /// track is turned into on its way there: when it changes, what plays starts
    /// over at the place it is at.
    pub fn of_the_stream(&self) -> (bool, bool, bool, super::upsample::Filter) {
        (
            self.match_sample_rate,
            self.exclusive,
            self.upsample,
            self.filter,
        )
    }
}

/// The highest rate upsampling goes to.
pub const MAX_UPSAMPLED_RATE: u32 = 384_000;

/// The rate to run a device at for a track of `source` Hz that is to be upsampled: the
/// highest of eight, four and two times the track's rate that the device offers (up
/// to 384 kHz). `None` when it offers none of them: then the rate is chosen as usual.
pub fn choose_raised_rate(source: u32, ranges: &[(u32, u32)]) -> Option<u32> {
    let offered = |rate: u32| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&rate));
    [8u32, 4, 2]
        .iter()
        .filter_map(|k| source.checked_mul(*k))
        .find(|rate| *rate <= MAX_UPSAMPLED_RATE && offered(*rate))
}

/// The rate to run a device at for a track of `source` Hz, given the rates the device
/// offers as inclusive `(min, max)` ranges. `None` when there is nothing to choose from.
///
/// Order of preference: the track's own rate (no conversion at all); the smallest whole
/// multiple of it; the nearest rate above; the highest rate there is. In other words a
/// rate that keeps everything the file holds while there is one, and otherwise the
/// one that keeps the most. Whether the two rates are in a simple ratio does not come
/// into it: the converter is as clean at any ratio (see the tests in `resample.rs`).
pub fn choose_rate(source: u32, ranges: &[(u32, u32)]) -> Option<u32> {
    if source == 0 || ranges.is_empty() {
        return None;
    }
    let offered = |rate: u32| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&rate));
    if offered(source) {
        return Some(source);
    }
    const RATIOS: [u32; 5] = [2, 3, 4, 6, 8];
    if let Some(rate) = RATIOS
        .iter()
        .filter_map(|k| source.checked_mul(*k))
        .find(|r| offered(*r))
    {
        return Some(rate);
    }
    let above = ranges
        .iter()
        .filter(|(_, hi)| *hi >= source)
        .map(|(lo, _)| (*lo).max(source))
        .min();
    above.or_else(|| ranges.iter().map(|(_, hi)| *hi).max())
}

/// What matters about a hardware format when choosing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatTraits {
    /// Usable by the system mixer (see `PhysicalFormat::mixable`).
    pub mixable: bool,
    pub supports_rate: bool,
    pub bits: u32,
    pub channels: u32,
    pub float: bool,
    pub pcm: bool,
}

/// Index of the hardware format to use: linear PCM at the wanted rate, with the channel
/// count the device is using now if possible, and the widest samples on offer. Our
/// pipeline is 32-bit float, so nothing wider than 32 bits is of any use; between equally
/// wide formats the kind the device is using now (integer or float) wins, so that a
/// device is not reconfigured for nothing.
pub fn best_format(formats: &[FormatTraits], channels_now: u32, float_now: bool) -> Option<usize> {
    formats
        .iter()
        .enumerate()
        .filter(|(_, f)| f.pcm && f.mixable && f.supports_rate && f.channels > 0 && f.bits > 0)
        .max_by_key(|(i, f)| {
            (
                f.channels == channels_now,
                f.bits.min(32),
                f.float == float_now,
                // Earlier entries win ties: `max_by_key` would otherwise take the last.
                usize::MAX - i,
            )
        })
        .map(|(i, _)| i)
}

/// What the device was like before we changed it.
struct Original {
    rate: Option<u32>,
    format: Option<PhysicalFormat>,
}

/// Our claim on an output device: exclusive access and/or a changed format. Dropping it
/// puts the device back the way it was found.
pub struct DeviceHold {
    /// CPAL id of the device.
    pub device_id: String,
    device: AudioDevice,
    hogging: bool,
    original: Option<Original>,
}

impl DeviceHold {
    /// `None` when the device cannot be found through the audio HAL.
    pub fn acquire(device_id: &str, exclusive: bool) -> Option<Self> {
        let device = AudioDevice::from_cpal_id(device_id)?;
        let mut hold = Self {
            device_id: device_id.to_string(),
            device,
            hogging: false,
            original: None,
        };
        hold.set_exclusive(exclusive);
        Some(hold)
    }

    /// Takes or gives back exclusive access. Failing to get it is not an error: playback
    /// goes on shared, and [`DeviceHold::is_exclusive`] says so.
    pub fn set_exclusive(&mut self, exclusive: bool) {
        if exclusive && !self.hogging {
            self.hogging = self.device.take_hog();
            if !self.hogging {
                tracing::warn!("exclusive access to the output device was refused");
            }
        } else if !exclusive {
            // Also when we believe we never got it: a grant that arrived after we
            // stopped waiting for it would otherwise never be given back.
            self.device.release_hog();
            self.hogging = false;
        }
    }

    pub fn is_exclusive(&self) -> bool {
        self.hogging
    }

    pub fn rate_ranges(&self) -> Vec<(u32, u32)> {
        self.device.rate_ranges()
    }

    /// The hardware format right now: `(bits, is_float)`.
    pub fn hardware_bits(&self) -> Option<(u32, bool)> {
        self.device.physical_format().map(|f| (f.bits, f.float))
    }

    pub fn nominal_rate(&self) -> Option<u32> {
        self.device.nominal_rate()
    }

    /// The widest hardware format the device offers at `rate`, if that is wider than
    /// what it is using now.
    fn wider_format(&self, rate: u32) -> Option<PhysicalFormat> {
        let current = self.device.physical_format()?;
        let formats = self.device.physical_formats();
        let traits: Vec<FormatTraits> = formats
            .iter()
            .map(|f| FormatTraits {
                mixable: f.mixable,
                supports_rate: f.supports_rate(rate),
                bits: f.bits,
                channels: f.channels,
                float: f.float,
                pcm: f.pcm,
            })
            .collect();
        let best = formats[best_format(&traits, current.channels, current.float)?];
        (best.bits.min(32) > current.bits.min(32)).then_some(best)
    }

    /// Runs the device at `rate`, in its widest sample format. Returns false when the
    /// device did not follow (it then stays usable at whatever rate it has).
    pub fn apply_rate(&mut self, rate: u32) -> bool {
        let rate_now = self.device.nominal_rate();
        let wider = self.wider_format(rate);
        if rate_now == Some(rate) && wider.is_none() {
            return true;
        }
        if self.original.is_none() {
            self.original = Some(Original {
                rate: rate_now,
                format: self.device.physical_format(),
            });
        }
        // The clock rate first: every device understands that, and it is what decides
        // whether we have to resample. Wider samples are asked for separately, and a
        // device that does not follow there is still running at the right rate.
        let ok = self.device.set_nominal_rate(rate, FORMAT_SWITCH_TIMEOUT);
        if ok
            && let Some(format) = wider.or_else(|| self.wider_format(rate))
            && !self
                .device
                .set_physical_format(&format, rate, BIT_DEPTH_SWITCH_TIMEOUT)
        {
            tracing::info!(
                bits = format.bits,
                "the output device kept its sample format"
            );
        }
        tracing::info!(
            rate,
            ok,
            bits = self.device.physical_format().map(|f| f.bits),
            "output device format requested"
        );
        ok
    }

    /// Puts rate and format back the way they were before [`DeviceHold::apply_rate`].
    fn restore(&mut self) {
        let Some(original) = self.original.take() else {
            return;
        };
        // An unplugged device answers nothing; one taken over by another app is theirs.
        if self.device.nominal_rate().is_none()
            || self
                .device
                .hog_owner()
                .is_some_and(|pid| pid != std::process::id() as i32)
        {
            return;
        }
        let mut restored = original
            .rate
            .is_none_or(|rate| self.device.set_nominal_rate(rate, FORMAT_SWITCH_TIMEOUT));
        if let Some(format) = original.format
            && format.sample_rate > 0
            && self.device.physical_format().is_some_and(|now| {
                now.bits != format.bits
                    || now.float != format.float
                    || now.mixable != format.mixable
            })
        {
            restored &= self.device.set_physical_format(
                &format,
                format.sample_rate,
                BIT_DEPTH_SWITCH_TIMEOUT,
            );
        }
        tracing::info!(restored, "output device format put back");
    }
}

impl Drop for DeviceHold {
    fn drop(&mut self) {
        self.restore();
        // Unconditionally (it only acts when this process holds the device): see
        // `set_exclusive`.
        self.device.release_hog();
        self.hogging = false;
    }
}

/// Rates `device_id` can run at; empty when the device cannot be asked.
pub fn device_rate_ranges(device_id: &str) -> Vec<(u32, u32)> {
    AudioDevice::from_cpal_id(device_id).map_or_else(Vec::new, |d| d.rate_ranges())
}

/// The clock rate `device_id` is running at right now.
pub fn device_nominal_rate(device_id: &str) -> Option<u32> {
    AudioDevice::from_cpal_id(device_id)?.nominal_rate()
}

/// The hardware sample format of `device_id` right now: `(bits, is_float)`.
pub fn device_hardware_bits(device_id: &str) -> Option<(u32, bool)> {
    AudioDevice::from_cpal_id(device_id)?
        .physical_format()
        .map(|f| (f.bits, f.float))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    /// Stable identifier (CPAL device id string).
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputError {
    /// No output device exists at all.
    NoDevice,
    /// The requested device is not connected.
    DeviceNotFound,
    /// The device refused the stream (busy, unsupported format, backend error).
    Backend(String),
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no output device"),
            Self::DeviceNotFound => f.write_str("output device not found"),
            Self::Backend(detail) => write!(f, "output error: {detail}"),
        }
    }
}

impl std::error::Error for OutputError {}

/// What the stream's error callback has to tell the engine.
///
/// The callback can run on the device's real-time thread, so it only flips atomics:
/// no allocation, no lock, no channel. The engine reads them from its poll.
#[derive(Debug, Default)]
pub struct OutputSignals {
    lost: AtomicBool,
    invalidated: AtomicBool,
    other_errors: AtomicU32,
}

/// One reading of [`OutputSignals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputSignal {
    /// The device disappeared; the stream is dead.
    pub lost: bool,
    /// The stream configuration changed (e.g. sample rate); it must be rebuilt.
    pub invalidated: bool,
    /// Other backend errors since the last reading (xruns reported by the OS, etc.).
    pub other_errors: u32,
}

impl OutputSignals {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn signal_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }
    pub fn signal_invalidated(&self) {
        self.invalidated.store(true, Ordering::Release);
    }
    fn signal_other(&self) {
        self.other_errors.fetch_add(1, Ordering::Relaxed);
    }
    /// Reads and clears everything signalled so far.
    pub fn take(&self) -> OutputSignal {
        OutputSignal {
            lost: self.lost.swap(false, Ordering::AcqRel),
            invalidated: self.invalidated.swap(false, Ordering::AcqRel),
            other_errors: self.other_errors.swap(0, Ordering::Relaxed),
        }
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().trim().to_string())
        .unwrap_or_else(|_| "Unknown device".to_string())
}

/// Id of the current system default output device.
pub fn default_output_id() -> Option<String> {
    cpal::default_host()
        .default_output_device()?
        .id()
        .ok()
        .map(|id| id.to_string())
}

/// Lists connected output devices (cheap CoreAudio property queries; safe on any thread).
pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default_id = default_output_id();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            Some(OutputDeviceInfo {
                name: device_name(&d),
                is_default: default_id.as_deref() == Some(&id),
                id,
            })
        })
        .collect()
}

/// Whether the device is connected. `None` when the system could not be asked, which
/// says nothing about the device and must not be read as "gone".
pub fn device_present(id: &str) -> Option<bool> {
    let host = cpal::default_host();
    let devices = host.output_devices().ok()?;
    // The system's default output is there for as long as the system has one, also
    // where it is not one of the listed devices (Android, see `OutputStream::open`).
    let mut found = host
        .default_output_device()
        .is_some_and(|device| device.id().is_ok_and(|d| d.to_string() == id));
    for device in devices {
        if device.id().is_ok_and(|d| d.to_string() == id) {
            found = true;
        }
    }
    Some(found)
}

pub struct OutputStream {
    stream: cpal::Stream,
    pub link: RenderLink,
    pub device_id: String,
    pub device_name: String,
    pub channels: u16,
    running: bool,
}

impl OutputStream {
    /// Opens a stream on `device_id`, or on the current default device when `None`.
    /// The stream starts paused; call [`OutputStream::set_running`].
    pub fn open(
        device_id: Option<&str>,
        shared: Arc<RenderShared>,
        signals: Arc<OutputSignals>,
    ) -> Result<Self, OutputError> {
        // On an iPhone the app's audio session has to be the active one before a stream
        // can start (a no-op elsewhere).
        crate::platform::sound::activate();
        let host = cpal::default_host();
        let wanted = match device_id {
            Some(id) => id.to_string(),
            None => default_output_id().ok_or(OutputError::NoDevice)?,
        };
        // Resolve by id so the stream is pinned to this device (never "follow default").
        let parsed: cpal::DeviceId = wanted.parse().map_err(|_| OutputError::DeviceNotFound)?;
        let device = match host.device_by_id(&parsed) {
            Some(device) => device,
            // On Android the default output is not one of the listed devices but a
            // device of its own, which the system routes (speaker, headphones): it is
            // found only by asking for the default.
            None => host
                .default_output_device()
                .filter(|device| device.id().is_ok_and(|id| id.to_string() == wanted))
                .ok_or(OutputError::DeviceNotFound)?,
        };
        let supported = device.default_output_config().map_err(|e| match e.kind() {
            cpal::ErrorKind::DeviceNotAvailable => OutputError::DeviceNotFound,
            _ => OutputError::Backend(e.to_string()),
        })?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels;
        let sample_rate = config.sample_rate;
        let (renderer, link) = render_pair(shared.clone(), sample_rate);
        let on_error = move |e: cpal::Error| match e.kind() {
            cpal::ErrorKind::DeviceNotAvailable => signals.signal_lost(),
            cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::DeviceChanged => {
                signals.signal_invalidated();
            }
            _ => signals.signal_other(),
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_f32(&device, config, renderer, shared, on_error),
            cpal::SampleFormat::I16 => {
                build_converting::<i16>(&device, config, renderer, shared, on_error)
            }
            cpal::SampleFormat::I32 => {
                build_converting::<i32>(&device, config, renderer, shared, on_error)
            }
            cpal::SampleFormat::U16 => {
                build_converting::<u16>(&device, config, renderer, shared, on_error)
            }
            other => Err(OutputError::Backend(format!(
                "unsupported sample format {other}"
            ))),
        }?;
        // Some backends start streams immediately; make the initial state explicit.
        let _ = stream.pause();
        Ok(Self {
            stream,
            link,
            device_id: wanted,
            device_name: device_name(&device),
            channels,
            running: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.link.sample_rate
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Stops the callback without the engine's knowledge, the way the backend does before
    /// it reports a format notification. For the device self-test only.
    pub fn stop_like_the_backend(&mut self) {
        let _ = self.stream.pause();
    }

    /// Starts the callback again after the backend stopped it on its own: CPAL pauses a
    /// stream before it reports that the device's format changed, whether or not the
    /// stream is still good.
    pub fn resume_after_notification(&mut self) -> Result<(), OutputError> {
        if !self.running {
            return Ok(());
        }
        self.running = false;
        self.set_running(true)
    }

    /// Starts or stops the device callback. A stopped stream lets the device (and the Mac) sleep.
    pub fn set_running(&mut self, run: bool) -> Result<(), OutputError> {
        if run == self.running {
            return Ok(());
        }
        let result = if run {
            self.stream.play()
        } else {
            self.stream.pause()
        };
        result.map_err(|e| match e.kind() {
            cpal::ErrorKind::DeviceNotAvailable => OutputError::DeviceNotFound,
            _ => OutputError::Backend(e.to_string()),
        })?;
        self.running = run;
        Ok(())
    }
}

fn store_latency(shared: &RenderShared, info: &cpal::OutputCallbackInfo, sample_rate: u32) {
    let ts = info.timestamp();
    let latency = ts.playback.duration_since(ts.callback);
    let frames = (latency.as_secs_f64() * f64::from(sample_rate)) as u32;
    shared.set_latency_frames(frames);
}

fn build_f32(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut renderer: Renderer,
    shared: Arc<RenderShared>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, OutputError> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                renderer.render(data, channels);
                store_latency(&shared, info, rate);
            },
            on_error,
            None,
        )
        .map_err(|e| OutputError::Backend(e.to_string()))
}

/// For integer devices: render into a preallocated f32 scratch, then convert.
fn build_converting<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut renderer: Renderer,
    shared: Arc<RenderShared>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, OutputError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    // Allocated once here; the callback processes larger buffers in pieces.
    let mut scratch = vec![0f32; 4096 * channels];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                for piece in data.chunks_mut(scratch.len()) {
                    let buf = &mut scratch[..piece.len()];
                    renderer.render(buf, channels);
                    for (dst, src) in piece.iter_mut().zip(buf.iter()) {
                        *dst = T::from_sample(*src);
                    }
                }
                store_latency(&shared, info, rate);
            },
            on_error,
            None,
        )
        .map_err(|e| OutputError::Backend(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discrete(rates: &[u32]) -> Vec<(u32, u32)> {
        rates.iter().map(|r| (*r, *r)).collect()
    }

    #[test]
    fn the_tracks_own_rate_wins_when_the_device_offers_it() {
        let dac = discrete(&[44_100, 48_000, 88_200, 96_000, 176_400, 192_000]);
        for rate in [44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            assert_eq!(choose_rate(rate, &dac), Some(rate));
        }
        // A virtual device offering a continuous range.
        assert_eq!(choose_rate(44_100, &[(8_000, 384_000)]), Some(44_100));
        // Upsampling: the highest of ×8, ×4 and ×2 the device has, up to 384 kHz.
        assert_eq!(choose_raised_rate(44_100, &dac), Some(176_400));
        assert_eq!(choose_raised_rate(48_000, &dac), Some(192_000));
        assert_eq!(choose_raised_rate(96_000, &dac), Some(192_000));
        assert_eq!(choose_raised_rate(192_000, &dac), None);
        let wide = [(8_000, 768_000)];
        assert_eq!(choose_raised_rate(44_100, &wide), Some(352_800));
        assert_eq!(choose_raised_rate(48_000, &wide), Some(384_000));
        assert_eq!(choose_raised_rate(96_000, &wide), Some(384_000));
        assert_eq!(choose_raised_rate(192_000, &wide), Some(384_000));
        assert_eq!(choose_raised_rate(352_800, &wide), None);
        assert_eq!(
            choose_raised_rate(44_100, &[(44_100, 44_100), (96_000, 96_000)]),
            None
        );
        assert_eq!(
            choose_raised_rate(48_000, &[(44_100, 96_000)]),
            Some(96_000)
        );
        assert_eq!(choose_raised_rate(0, &dac), None);
        assert_eq!(choose_raised_rate(44_100, &[]), None);
    }

    #[test]
    fn otherwise_the_rate_that_keeps_the_most_is_chosen() {
        // Built-in output: nothing above 96 kHz, so that is what keeps the most.
        let builtin = discrete(&[44_100, 48_000, 88_200, 96_000]);
        assert_eq!(choose_rate(176_400, &builtin), Some(96_000));
        assert_eq!(choose_rate(192_000, &builtin), Some(96_000));
        assert_eq!(choose_rate(352_800, &builtin), Some(96_000));
        // Low rates go up by a whole factor: 22.05 → 44.1, 32 → 96 (3×), 8 → 48 (6×).
        assert_eq!(choose_rate(22_050, &builtin), Some(44_100));
        assert_eq!(choose_rate(32_000, &builtin), Some(96_000));
        assert_eq!(choose_rate(8_000, &builtin), Some(48_000));
        // A 48 kHz-only device and a CD: no multiple fits, take the nearest rate above.
        assert_eq!(choose_rate(44_100, &discrete(&[48_000])), Some(48_000));
        assert_eq!(
            choose_rate(44_100, &discrete(&[48_000, 96_000])),
            Some(48_000)
        );
        // An older USB DAC without the 88.2 kHz family: an 88.2 kHz file goes up to
        // 96 kHz, not down to 44.1 kHz where half of what it holds would be cut off.
        let usb1 = discrete(&[44_100, 48_000, 96_000]);
        assert_eq!(choose_rate(88_200, &usb1), Some(96_000));
        assert_eq!(choose_rate(176_400, &usb1), Some(96_000));
        assert_eq!(choose_rate(192_000, &usb1), Some(96_000));
        // Nothing above either: the highest there is.
        assert_eq!(choose_rate(96_000, &discrete(&[44_100])), Some(44_100));
        assert_eq!(
            choose_rate(50_000, &discrete(&[44_100, 48_000])),
            Some(48_000)
        );
    }

    #[test]
    fn no_choice_without_a_rate_or_a_device() {
        assert_eq!(choose_rate(0, &discrete(&[44_100])), None);
        assert_eq!(choose_rate(44_100, &[]), None);
    }

    fn format(bits: u32, float: bool, supports_rate: bool) -> FormatTraits {
        FormatTraits {
            mixable: true,
            supports_rate,
            bits,
            channels: 2,
            float,
            pcm: true,
        }
    }

    #[test]
    fn the_widest_format_at_the_wanted_rate_is_chosen() {
        // A typical USB DAC: 16/24/32-bit integer.
        let dac = [
            format(16, false, true),
            format(24, false, true),
            format(32, false, true),
        ];
        assert_eq!(best_format(&dac, 2, false), Some(2));
        // Wider formats that are not offered at this rate do not count.
        let partial = [
            format(16, false, true),
            format(24, false, true),
            format(32, false, false),
        ];
        assert_eq!(best_format(&partial, 2, false), Some(1));
        // Built-in output: float only.
        assert_eq!(best_format(&[format(32, true, true)], 2, true), Some(0));
        // Equally wide: stay with the kind in use, and with the first of equals.
        let mixed = [format(32, false, true), format(32, true, true)];
        assert_eq!(best_format(&mixed, 2, true), Some(1));
        assert_eq!(best_format(&mixed, 2, false), Some(0));
        let twins = [format(24, false, true), format(24, false, true)];
        assert_eq!(best_format(&twins, 2, false), Some(0));
        // 64-bit float is no better than 32-bit for a 32-bit pipeline.
        let wide = [format(32, true, true), format(64, true, true)];
        assert_eq!(best_format(&wide, 2, true), Some(0));
    }

    #[test]
    fn formats_that_cannot_be_used_are_never_chosen() {
        let ac3 = FormatTraits {
            pcm: false,
            ..format(16, false, true)
        };
        assert_eq!(best_format(&[ac3], 2, false), None);
        assert_eq!(best_format(&[format(24, false, false)], 2, false), None);
        // The non-mixable twin of a format would lock every other app out of the device:
        // it is never chosen, even when it is listed first or is the wider one.
        let locked = |bits| FormatTraits {
            mixable: false,
            ..format(bits, false, true)
        };
        assert_eq!(
            best_format(&[locked(24), format(24, false, true)], 2, false),
            Some(1)
        );
        assert_eq!(
            best_format(&[locked(32), format(16, false, true)], 2, false),
            Some(1)
        );
        assert_eq!(best_format(&[locked(24)], 2, false), None);
        assert_eq!(best_format(&[], 2, false), None);
        // The channel count in use is kept when a format with it exists.
        let multi = FormatTraits {
            channels: 8,
            ..format(32, false, true)
        };
        assert_eq!(
            best_format(&[multi, format(24, false, true)], 2, false),
            Some(1)
        );
        assert_eq!(best_format(&[multi], 2, false), Some(0));
    }

    #[test]
    fn default_settings_leave_the_device_alone() {
        let settings = OutputSettings::default();
        assert!(!settings.touches_device());
        assert!(!settings.memory_playback);
        let json = serde_json::to_string(&settings).expect("serializes");
        assert_eq!(
            serde_json::from_str::<OutputSettings>(&json).expect("parses"),
            settings
        );
        assert_eq!(
            serde_json::from_str::<OutputSettings>("{}").expect("parses"),
            settings
        );
    }
}

//! The output "device" of a phone: the audio session.
//!
//! An iPhone has no device to choose and none to take for oneself. What an app may do
//! is ask the system to run the output at a sample rate: the built-in output can run
//! at 44.1 or 48 kHz, a USB DAC at what it offers. The system decides (the sessions of
//! other apps count too) and says what it chose. That is enough for "play at the
//! song's sample rate": ask for the song's rate, see whether the system followed, and
//! convert as usual where it did not.
pub use crate::platform::no_device::PhysicalFormat;
use objc2_avf_audio::AVAudioSession;
use std::time::{Duration, Instant};

/// How long the system is given to change the output's rate. It answers at once or
/// not at all; a song must not wait long for a rate the output does not have.
const FOLLOW_TIMEOUT: Duration = Duration::from_millis(700);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDevice;

impl AudioDevice {
    /// There is one output, whatever it is called: where the system sends the sound.
    pub fn from_cpal_id(_id: &str) -> Option<Self> {
        Some(Self)
    }

    /// The rate the output runs at now.
    pub fn nominal_rate(&self) -> Option<u32> {
        // SAFETY: a plain getter of the shared session.
        let rate = unsafe { AVAudioSession::sharedInstance().sampleRate() };
        (rate.is_finite() && rate >= 1.0).then(|| rate.round() as u32)
    }

    /// What the output could run at is not something the system tells. Any rate of a
    /// song may be asked for; what comes of it is seen afterwards.
    pub fn rate_ranges(&self) -> Vec<(u32, u32)> {
        vec![(8_000, 768_000)]
    }

    /// Asks the system to run the output at `rate`. True when it does so now.
    pub fn set_nominal_rate(&self, rate: u32, timeout: Duration) -> bool {
        if self.nominal_rate() == Some(rate) {
            return true;
        }
        // A wish only counts for a session that is the active one.
        super::sound::activate();
        // SAFETY: the shared session and a plain setter.
        let asked = unsafe {
            AVAudioSession::sharedInstance().setPreferredSampleRate_error(f64::from(rate))
        };
        if let Err(error) = asked {
            tracing::warn!(rate, "the audio session refused the sample rate: {error}");
            return false;
        }
        let until = Instant::now() + timeout.min(FOLLOW_TIMEOUT);
        loop {
            if self.nominal_rate() == Some(rate) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn hog_owner(&self) -> Option<i32> {
        None
    }

    pub fn is_hogged_by_us(&self) -> bool {
        false
    }

    /// The output is the system's and every app's: it cannot be had alone.
    pub fn take_hog(&self) -> bool {
        false
    }

    pub fn release_hog(&self) {}

    pub fn physical_format(&self) -> Option<PhysicalFormat> {
        None
    }

    pub fn physical_formats(&self) -> Vec<PhysicalFormat> {
        Vec::new()
    }

    pub fn set_physical_format(
        &self,
        _format: &PhysicalFormat,
        _rate: u32,
        _timeout: Duration,
    ) -> bool {
        false
    }
}

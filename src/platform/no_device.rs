//! The stand-in for output device control where there is none (Windows and iOS, see
//! `DEVICE_CONTROL`): no device is ever found here, so the engine leaves the device as
//! the system has it.
use std::time::Duration;

/// One hardware stream format of a device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalFormat {
    pub sample_rate: u32,
    pub min_rate: u32,
    pub max_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub float: bool,
    pub pcm: bool,
    pub mixable: bool,
}

impl PhysicalFormat {
    pub fn supports_rate(&self, rate: u32) -> bool {
        (self.min_rate..=self.max_rate).contains(&rate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDevice;

impl AudioDevice {
    pub fn from_cpal_id(_id: &str) -> Option<Self> {
        None
    }

    pub fn nominal_rate(&self) -> Option<u32> {
        None
    }

    pub fn rate_ranges(&self) -> Vec<(u32, u32)> {
        Vec::new()
    }

    pub fn set_nominal_rate(&self, _rate: u32, _timeout: Duration) -> bool {
        false
    }

    pub fn hog_owner(&self) -> Option<i32> {
        None
    }

    pub fn is_hogged_by_us(&self) -> bool {
        false
    }

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

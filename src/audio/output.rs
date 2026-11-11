//! Output devices and the CPAL stream that hosts the [`Renderer`].
//!
//! Streams are always opened on a *specific* device. CPAL's default-device stream follows
//! the system default automatically, which would move playback to the speakers when
//! headphones are unplugged; we want to pause instead.
use super::render::{RenderLink, RenderShared, Renderer, render_pair};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::mpsc::Sender;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    /// The device disappeared; the stream is dead.
    DeviceLost,
    /// The stream configuration changed (e.g. sample rate); it must be rebuilt.
    Invalidated,
    /// Informational backend message (xrun reported by the OS, etc.).
    Notice(String),
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
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

pub fn device_exists(id: &str) -> bool {
    list_output_devices().iter().any(|d| d.id == id)
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
        events: Sender<OutputEvent>,
    ) -> Result<Self, OutputError> {
        let host = cpal::default_host();
        let wanted = match device_id {
            Some(id) => id.to_string(),
            None => default_output_id().ok_or(OutputError::NoDevice)?,
        };
        // Resolve by id so the stream is pinned to this device (never "follow default").
        let parsed: cpal::DeviceId = wanted.parse().map_err(|_| OutputError::DeviceNotFound)?;
        let device = host
            .device_by_id(&parsed)
            .ok_or(OutputError::DeviceNotFound)?;
        let supported = device.default_output_config().map_err(|e| match e.kind() {
            cpal::ErrorKind::DeviceNotAvailable => OutputError::DeviceNotFound,
            _ => OutputError::Backend(e.to_string()),
        })?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels;
        let sample_rate = config.sample_rate;
        let (renderer, link) = render_pair(shared.clone(), sample_rate);
        let err_events = events.clone();
        let on_error = move |e: cpal::Error| {
            let event = match e.kind() {
                cpal::ErrorKind::DeviceNotAvailable => OutputEvent::DeviceLost,
                cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::DeviceChanged => {
                    OutputEvent::Invalidated
                }
                _ => OutputEvent::Notice(e.to_string()),
            };
            let _ = err_events.send(event);
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

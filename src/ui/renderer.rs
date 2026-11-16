//! Choosing the graphics adapter the window is drawn with.
//!
//! eframe asks the system for one adapter and gives up when a device cannot be made on
//! it. On machines with an unusual graphics setup (a virtual machine, remote desktop,
//! an old or missing driver) the first choice is often the one that fails while
//! another would work: Direct3D 12 on Windows' software renderer, Vulkan, OpenGL.
//! Here every adapter that can show the window is tried, in order of preference, and
//! the first on which a device can really be made is used. The trial runs on a
//! throw-away instance, because an adapter makes only one device. What is asked of the
//! device is the little that drawing the interface needs, not wgpu's defaults, which
//! some adapters do not meet.
//!
//! The device is watched afterwards ([`watch`]): a driver can take it away at any time
//! (Android does), and what then happens must not be left to the UI library.
use eframe::egui_wgpu::{self, wgpu};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

/// Set once the device the window is drawn with is gone (see [`watch`]).
static LOST: AtomicBool = AtomicBool::new(false);
/// How many errors of the device went in the log (see [`watch`]).
static ERRORS: AtomicU32 = AtomicU32::new(0);
/// A device that keeps failing says so every frame: the log takes the first of it.
const ERRORS_WRITTEN: u32 = 20;
/// The device: asked about its memory ([`gpu_memory`]), and taken away by a
/// verification run ([`lose_device`]).
static DEVICE: OnceLock<wgpu::Device> = OnceLock::new();
/// What the window is drawn with: the adapter in words, and its interface.
static ADAPTER: OnceLock<(String, wgpu::Backend)> = OnceLock::new();
/// A verification run asked for the loss not to be told ([`lose_device`]).
static UNTOLD: AtomicBool = AtomicBool::new(false);

/// Keeps an eye on the device the window is drawn with.
///
/// Left alone, wgpu ends the app on an error of the device that nothing asked about,
/// and says nothing at all when the device is lost (the driver gave up on it, or
/// answered with something wgpu does not know): every later call then quietly does
/// nothing, and the UI library ends the app at the next frame with "Failed to create
/// staging buffer", which tells nobody what happened. Here both go in the log under
/// "graphics:", an error does not end the app, and a lost device is remembered for
/// [`device_lost`], which the app asks before every frame.
pub fn watch(state: &egui_wgpu::RenderState) {
    let info = state.adapter.get_info();
    let adapter = summary(&info);
    tracing::info!(%adapter, "graphics: the window is drawn with this adapter");
    let _ = ADAPTER.set((adapter.clone(), info.backend));
    let _ = DEVICE.set(state.device.clone());
    state
        .device
        .on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            let count = ERRORS.fetch_add(1, Ordering::Relaxed);
            if count < ERRORS_WRITTEN {
                tracing::error!("graphics: {error}");
            } else if count == ERRORS_WRITTEN {
                tracing::error!("graphics: more errors follow; they are not written down");
            }
        }));
    state
        .device
        .set_device_lost_callback(move |reason, message| {
            match reason {
                // Also at the end of every run, when the device is given back.
                wgpu::DeviceLostReason::Destroyed => {
                    tracing::info!(%adapter, %message, "graphics: the device was given up");
                }
                wgpu::DeviceLostReason::Unknown => {
                    tracing::error!(%adapter, "graphics: the device is lost: {message}");
                }
            }
            LOST.store(true, Ordering::Release);
        });
}

/// What the window is drawn with (once [`watch`] was called): the adapter in words,
/// and the interface it is reached through.
pub fn adapter() -> Option<&'static (String, wgpu::Backend)> {
    ADAPTER.get()
}

/// The window is not drawn through wgpu but by the UI framework's OpenGL renderer:
/// what OpenGL says of itself is kept for [`adapter`] (there is no device to watch,
/// and no count of its memory).
#[cfg(target_os = "android")]
pub fn note_glow(gl: &eframe::glow::Context) {
    use eframe::glow::HasContext as _;
    // SAFETY: the context is current on this thread (the framework made it so before
    // it called the app), and these are plain queries.
    let (renderer, version) = unsafe {
        (
            gl.get_parameter_string(eframe::glow::RENDERER),
            gl.get_parameter_string(eframe::glow::VERSION),
        )
    };
    let adapter = format!("{renderer} ({version}; glow)");
    tracing::info!(%adapter, "graphics: the window is drawn with OpenGL, without wgpu");
    let _ = ADAPTER.set((adapter, wgpu::Backend::Gl));
}

/// How many objects of each kind the graphics device holds at the moment, as the
/// graphics library counts them. Something that is made for every frame and never
/// given back shows here as a number that keeps growing.
pub fn gpu_objects() -> Option<super::memory::Objects> {
    let counters = DEVICE.get()?.get_internal_counters().hal;
    let count = |value: isize| value.max(0) as usize;
    Some(super::memory::Objects {
        buffers: count(counters.buffers.read()),
        textures: count(counters.textures.read()),
        views: count(counters.texture_views.read()),
        bind_groups: count(counters.bind_groups.read()),
        encoders: count(counters.command_encoders.read()),
        allocations: count(counters.memory_allocations.read()),
    })
}

/// The graphics memory as the graphics library's allocator counts it. `None` where it
/// keeps no such count (Metal) or before a device exists.
pub fn gpu_memory() -> Option<super::memory::Gpu> {
    const MB: u64 = 1024 * 1024;
    let report = DEVICE.get()?.generate_allocator_report()?;
    Some(super::memory::Gpu {
        blocks: report.blocks.len(),
        reserved_mb: report.total_reserved_bytes / MB,
        pieces: report.allocations.len(),
        used_mb: report.total_allocated_bytes / MB,
    })
}

/// Whether the device is gone: nothing can be drawn with it any more, and a frame
/// that is drawn all the same ends the app (see [`watch`]).
pub fn device_lost() -> bool {
    LOST.load(Ordering::Acquire) && !UNTOLD.load(Ordering::Relaxed)
}

/// For a verification run: takes the device away, as a driver does that loses it.
/// With `told` false the app is not told ([`device_lost`] stays false), which shows
/// what a loss does to an app that does not look out for it.
pub fn lose_device(told: bool) {
    if let Some(device) = DEVICE.get() {
        UNTOLD.store(!told, Ordering::Relaxed);
        device.destroy();
        // What is under way is finished first; then the device says that it is gone.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }
}

/// Which interface to the graphics hardware is tried first, where a system has more
/// than one that works (Android: Vulkan and OpenGL ES).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefer {
    /// The order of [`rank`].
    #[default]
    System,
    Vulkan,
    OpenGl,
    /// Not wgpu at all: the UI framework's OpenGL renderer (Android, see
    /// `GraphicsChoice::Glow`). For the choice of a wgpu adapter it is as `System`.
    Glow,
}

impl Prefer {
    /// The choice of the settings, and for "automatic" the interface a run lost its
    /// device on (`lost_on`, as [`backend_word`] names it): the other one comes first.
    pub fn from_choice(
        choice: crate::app::settings::GraphicsChoice,
        lost_on: Option<&str>,
    ) -> Self {
        use crate::app::settings::GraphicsChoice;
        match (choice, lost_on) {
            (GraphicsChoice::Vulkan, _) => Self::Vulkan,
            (GraphicsChoice::OpenGl, _) => Self::OpenGl,
            (GraphicsChoice::Glow, _) => Self::Glow,
            (GraphicsChoice::Auto, Some("vulkan")) => Self::OpenGl,
            (GraphicsChoice::Auto, Some("gl")) => Self::Vulkan,
            (GraphicsChoice::Auto, _) => Self::System,
        }
    }

    fn wants(self, backend: wgpu::Backend) -> bool {
        matches!(
            (self, backend),
            (Self::Vulkan, wgpu::Backend::Vulkan) | (Self::OpenGl, wgpu::Backend::Gl)
        )
    }
}

/// The interface the window is drawn through, as the note of a run that lost its
/// device names it: "vulkan", "gl", or nothing for the others (which have no
/// alternative to go to).
pub fn backend_word() -> Option<&'static str> {
    // (Only a wgpu device can be lost; without one there is nothing to turn from.)
    DEVICE.get()?;
    match ADAPTER.get()?.1 {
        wgpu::Backend::Vulkan => Some("vulkan"),
        wgpu::Backend::Gl => Some("gl"),
        _ => None,
    }
}

/// eframe's graphics settings with the adapter chosen as described above; `prefer`
/// puts one interface before the others.
pub fn wgpu_options(prefer: Prefer) -> egui_wgpu::WgpuConfiguration {
    let backends = wgpu::Backends::from_env().unwrap_or(match prefer {
        // On Android a window belongs to one interface: a Vulkan surface that exists
        // for it keeps OpenGL ES from getting one ("already connected to another
        // API"), even if nothing is drawn through Vulkan. So the interface that is
        // preferred there is the only one that is set up.
        Prefer::OpenGl if cfg!(target_os = "android") => wgpu::Backends::GL,
        Prefer::Vulkan if cfg!(target_os = "android") => wgpu::Backends::VULKAN,
        _ => wgpu::Backends::PRIMARY | wgpu::Backends::GL,
    });
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends;
    setup.instance_descriptor.flags = instance_flags();
    setup.device_descriptor = Arc::new(device_descriptor);
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        select(adapters, surface, backends, prefer)
    }));
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// What a graphics instance is made with: wgpu's choice for the build (checks and
/// names for its objects in a debug build), but none of that on Android, where the
/// emulator's Vulkan driver crashes when an object is given a name.
fn instance_flags() -> wgpu::InstanceFlags {
    if cfg!(target_os = "android") {
        wgpu::InstanceFlags::empty()
    } else {
        wgpu::InstanceFlags::from_build_config().with_env()
    }
}

fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let supported = adapter.limits();
    wgpu::DeviceDescriptor {
        label: Some("OnEum"),
        required_limits: wgpu::Limits {
            // Large enough for a window on a 4K+ display, where the adapter allows it.
            max_texture_dimension_2d: supported.max_texture_dimension_2d.min(8192),
            ..wgpu::Limits::downlevel_webgl2_defaults()
        },
        // Graphics memory is taken from the driver in blocks. wgpu's own choice starts
        // with blocks of 64 and 128 MB and keeps each of them mapped whole, which is
        // sized for a game; an interface needs a few megabytes. This starts with 4
        // and 8 MB and grows only as far as is used.
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }
}

/// Lower is preferred: real graphics hardware first, the software renderer last; the
/// system's own interface first (Direct3D 12 on Windows, Metal on a Mac).
fn rank(device_type: wgpu::DeviceType, backend: wgpu::Backend) -> (u8, u8) {
    let kind = match device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    let interface = match backend {
        wgpu::Backend::Dx12 | wgpu::Backend::Metal => 0,
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Gl => 2,
        _ => 3,
    };
    (kind, interface)
}

fn summary(info: &wgpu::AdapterInfo) -> String {
    format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
}

fn same_adapter(a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo) -> bool {
    a.name == b.name
        && a.vendor == b.vendor
        && a.device == b.device
        && a.backend == b.backend
        && a.device_type == b.device_type
}

fn select(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    backends: wgpu::Backends,
    prefer: Prefer,
) -> Result<wgpu::Adapter, String> {
    for adapter in adapters {
        tracing::info!(adapter = %summary(&adapter.get_info()), "graphics adapter found");
    }
    let mut candidates: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .collect();
    candidates.sort_by_key(|adapter| {
        let info = adapter.get_info();
        (
            !prefer.wants(info.backend),
            rank(info.device_type, info.backend),
        )
    });

    let trial = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: instance_flags(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let twins = block_on(trial.enumerate_adapters(backends));
    let mut failures = Vec::new();
    for adapter in candidates {
        let info = adapter.get_info();
        let outcome = match twins
            .iter()
            .find(|twin| same_adapter(&twin.get_info(), &info))
        {
            Some(twin) => block_on(twin.request_device(&device_descriptor(twin)))
                .map(drop)
                .map_err(|e| e.to_string()),
            // Not seen by the trial instance: it cannot be tried apart, so it is taken
            // as it is (eframe reports the error if it fails).
            None => Ok(()),
        };
        match outcome {
            Ok(()) => {
                tracing::info!(adapter = %summary(&info), "drawing with this graphics adapter");
                return Ok(adapter.clone());
            }
            Err(error) => {
                tracing::warn!(adapter = %summary(&info), %error, "graphics adapter cannot be used");
                failures.push(format!("{}: {error}", summary(&info)));
            }
        }
    }
    Err(if adapters.is_empty() {
        "no graphics adapter was found (Direct3D 12, Vulkan or OpenGL 3.3 is needed)".to_string()
    } else if failures.is_empty() {
        let found: Vec<String> = adapters.iter().map(|a| summary(&a.get_info())).collect();
        format!(
            "none of the graphics adapters can show the window: {}",
            found.join(", ")
        )
    } else {
        format!(
            "no graphics adapter could be started: {}",
            failures.join("; ")
        )
    })
}

/// Runs a wgpu future to completion. On native systems wgpu's futures are ready at once.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_comes_before_software_and_the_system_interface_first() {
        use wgpu::{Backend, DeviceType};
        let mut order = [
            (DeviceType::Cpu, Backend::Dx12),
            (DeviceType::IntegratedGpu, Backend::Gl),
            (DeviceType::IntegratedGpu, Backend::Vulkan),
            (DeviceType::DiscreteGpu, Backend::Vulkan),
            (DeviceType::IntegratedGpu, Backend::Dx12),
            (DeviceType::VirtualGpu, Backend::Dx12),
        ];
        order.sort_by_key(|&(kind, backend)| rank(kind, backend));
        assert_eq!(
            order,
            [
                (DeviceType::DiscreteGpu, Backend::Vulkan),
                (DeviceType::IntegratedGpu, Backend::Dx12),
                (DeviceType::IntegratedGpu, Backend::Vulkan),
                (DeviceType::IntegratedGpu, Backend::Gl),
                (DeviceType::VirtualGpu, Backend::Dx12),
                (DeviceType::Cpu, Backend::Dx12),
            ]
        );
    }

    #[test]
    fn the_settings_and_a_lost_device_decide_which_interface_comes_first() {
        use crate::app::settings::GraphicsChoice::{Auto, OpenGl, Vulkan};
        assert_eq!(Prefer::from_choice(Auto, None), Prefer::System);
        // "Automatic" turns away from the interface a run lost its device on.
        assert_eq!(Prefer::from_choice(Auto, Some("vulkan")), Prefer::OpenGl);
        assert_eq!(Prefer::from_choice(Auto, Some("gl")), Prefer::Vulkan);
        assert_eq!(Prefer::from_choice(Auto, Some("metal")), Prefer::System);
        // A choice that was made is kept, whatever was lost.
        assert_eq!(Prefer::from_choice(Vulkan, Some("vulkan")), Prefer::Vulkan);
        assert_eq!(Prefer::from_choice(OpenGl, Some("gl")), Prefer::OpenGl);
        assert_eq!(
            Prefer::from_choice(crate::app::settings::GraphicsChoice::Glow, Some("vulkan")),
            Prefer::Glow
        );
        assert!(
            !Prefer::Glow.wants(wgpu::Backend::Gl),
            "no wgpu adapter is preferred for it"
        );

        assert!(Prefer::OpenGl.wants(wgpu::Backend::Gl));
        assert!(!Prefer::OpenGl.wants(wgpu::Backend::Vulkan));
        assert!(Prefer::Vulkan.wants(wgpu::Backend::Vulkan));
        assert!(!Prefer::System.wants(wgpu::Backend::Vulkan));
    }

    /// On this machine: an adapter is found and a device is made with what is asked.
    #[test]
    fn an_adapter_of_this_machine_is_chosen() {
        let backends = wgpu::Backends::PRIMARY | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapters = block_on(instance.enumerate_adapters(backends));
        if adapters.is_empty() {
            // A build machine without graphics: the error says so.
            let error = select(&adapters, None, backends, Prefer::System).unwrap_err();
            assert!(error.contains("no graphics adapter"));
            return;
        }
        let chosen =
            select(&adapters, None, backends, Prefer::System).expect("an adapter is usable");
        let (_device, _queue) = block_on(chosen.request_device(&device_descriptor(&chosen)))
            .expect("the chosen adapter makes a device");
    }

    /// What ended the app on a phone (2026-11-15): once a device is gone, a buffer
    /// write comes back empty and no error is raised for it, which the UI library
    /// answers by ending the app. The device says that it is gone, though, to who asks.
    #[test]
    fn a_device_that_is_gone_says_so_and_takes_no_more_writes() {
        let backends = wgpu::Backends::PRIMARY | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapters = block_on(instance.enumerate_adapters(backends));
        let Ok(chosen) = select(&adapters, None, backends, Prefer::System) else {
            return; // a build machine without graphics
        };
        let (device, queue) = block_on(chosen.request_device(&device_descriptor(&chosen)))
            .expect("the chosen adapter makes a device");
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 1024,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let size = wgpu::BufferSize::new(64).unwrap();
        assert!(
            queue.write_buffer_with(&buffer, 0, size).is_some(),
            "a write is taken while the device is there"
        );

        let told = Arc::new(AtomicBool::new(false));
        let flag = told.clone();
        device.set_device_lost_callback(move |_, _| flag.store(true, Ordering::Release));
        // No handler for errors is set: one that was raised would end this test.
        device.destroy();
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            told.load(Ordering::Acquire),
            "the device says that it is gone"
        );
        assert!(
            queue.write_buffer_with(&buffer, 0, size).is_none(),
            "and takes no more writes, without raising an error"
        );
    }
}

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
/// What the window is drawn with: the adapter in words, and its interface as the note
/// of a run that lost its device names it ([`backend_word`]).
static ADAPTER: OnceLock<(String, Option<&'static str>)> = OnceLock::new();
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
    let _ = ADAPTER.set((adapter.clone(), interface_word(info.backend)));
    let _ = DEVICE.set(state.device.clone());
    state
        .device
        .on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            note_error(format_args!("{error}"));
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

/// An error of what the window is drawn with goes in the log under "graphics:": the
/// first of them, and then one line that more follow.
fn note_error(error: std::fmt::Arguments<'_>) {
    let count = ERRORS.fetch_add(1, Ordering::Relaxed);
    if count < ERRORS_WRITTEN {
        tracing::error!("graphics: {error}");
    } else if count == ERRORS_WRITTEN {
        tracing::error!("graphics: more errors follow; they are not written down");
    }
}

/// How many errors what the window is drawn with has told of in this run (a lost
/// device is not one of them).
pub fn errors() -> u32 {
    ERRORS.load(Ordering::Relaxed)
}

/// What the window is drawn with, in words (once [`watch`] or [`note_glow`] was
/// called).
pub fn adapter() -> Option<&'static str> {
    ADAPTER.get().map(|(words, _)| words.as_str())
}

/// The window is not drawn through wgpu but by the UI framework's OpenGL renderer:
/// what OpenGL says of itself is kept for [`adapter`]. There is no device to watch
/// and no count of its memory; what the driver refuses is asked about after every
/// frame instead ([`note_gl_errors`]).
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
    // (No interface is named for a lost device: there is no other one to turn to
    // while the settings say "glow".)
    let _ = ADAPTER.set((adapter, None));
}

/// OpenGL's errors by their numbers (the bindings themselves are only in the Android
/// build).
#[cfg(any(target_os = "android", test))]
const GL_OUT_OF_MEMORY: u32 = 0x0505;
#[cfg(any(target_os = "android", test))]
const GL_CONTEXT_LOST: u32 = 0x0507;
/// OpenGL keeps one flag for each kind of error: this many questions empty them.
#[cfg(target_os = "android")]
const GL_ERRORS_ASKED: usize = 8;
/// How many passes of the UI there had been when OpenGL was last asked
/// ([`note_gl_errors`]).
#[cfg(target_os = "android")]
static GL_ASKED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// A verification run asked for a call that OpenGL refuses ([`ask_for_gl_mistake`]).
#[cfg(target_os = "android")]
static GL_MISTAKE: AtomicBool = AtomicBool::new(false);

#[cfg(any(target_os = "android", test))]
fn gl_error_words(code: u32) -> &'static str {
    match code {
        0x0500 => "invalid enum",
        0x0501 => "invalid value",
        0x0502 => "invalid operation",
        0x0503 => "stack overflow",
        0x0504 => "stack underflow",
        GL_OUT_OF_MEMORY => "out of memory",
        0x0506 => "invalid framebuffer operation",
        GL_CONTEXT_LOST => "the context is lost",
        _ => "an error without a name",
    }
}

/// Whether an error of OpenGL leaves nothing to draw with: after "out of memory" the
/// state of the context is undefined, and a lost context takes no more calls. (A lost
/// context is told this way only by a context that was made to tell of it, which the
/// UI framework's is not: its loss shows when the frame cannot be shown, which ends
/// the event loop, see `ui::lost_after_drawing`.)
#[cfg(any(target_os = "android", test))]
fn gl_error_is_a_loss(code: u32) -> bool {
    matches!(code, GL_OUT_OF_MEMORY | GL_CONTEXT_LOST)
}

/// Asks OpenGL what went wrong while the last frame was drawn (`drawn`: how many
/// passes of the UI there have been, each of which is followed by a frame). The UI
/// framework's OpenGL renderer asks only in a debug build, so in the app as it is
/// installed nothing would tell of a driver that refuses something. Each error goes in
/// the log under "graphics:", as the errors of a wgpu device do ([`watch`]); one that
/// leaves nothing to draw with is a lost device ([`device_lost`]).
#[cfg(target_os = "android")]
pub fn note_gl_errors(gl: &eframe::glow::Context, drawn: u64) {
    use eframe::glow::HasContext as _;
    // Once after every pass of the UI, not in every call: where a pass without a UI
    // follows another (a window that is covered), nothing was drawn between the two.
    // The context is this thread's here: the framework makes it so before every pass
    // that has a UI, and calls nothing of the app while Android has taken the window.
    if GL_ASKED_AT.swap(drawn, Ordering::Relaxed) == drawn {
        return;
    }
    if GL_MISTAKE.swap(false, Ordering::Relaxed) {
        // SAFETY: a number that names no capability: OpenGL refuses the call ("invalid
        // enum") and changes nothing.
        unsafe { gl.enable(0) };
    }
    for _ in 0..GL_ERRORS_ASKED {
        // SAFETY: a plain query on the context the frame before was drawn with.
        let code = unsafe { gl.get_error() };
        if code == eframe::glow::NO_ERROR {
            break;
        }
        let words = gl_error_words(code);
        if gl_error_is_a_loss(code) {
            if !LOST.swap(true, Ordering::AcqRel) {
                tracing::error!("graphics: the device is lost: OpenGL: {words} (0x{code:04X})");
            }
            // (A lost context answers every question with the same.)
            break;
        }
        note_error(format_args!("OpenGL: {words} (0x{code:04X})"));
    }
}

/// For a verification run: before OpenGL is next asked what went wrong
/// ([`note_gl_errors`]), it is given a call that it refuses and that changes nothing.
/// That the refusal then stands in the log shows that the question reaches the driver
/// (a log without errors says nothing otherwise). Nothing comes of it where the window
/// is not drawn by the UI framework's OpenGL renderer.
pub fn ask_for_gl_mistake() {
    #[cfg(target_os = "android")]
    GL_MISTAKE.store(true, Ordering::Relaxed);
}

/// For a verification run under the UI framework's OpenGL renderer: the surface the
/// window is drawn on is taken away behind the framework's back, as a driver does that
/// gives up. The frame that follows cannot be shown, which ends the event loop (see
/// `ui::lost_after_drawing`). Only where the verification channel is built in.
#[cfg(all(target_os = "android", feature = "automation"))]
pub fn lose_surface() {
    use std::ffi::c_void;
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentDisplay() -> *mut c_void;
        fn eglGetCurrentSurface(readdraw: i32) -> *mut c_void;
        fn eglDestroySurface(display: *mut c_void, surface: *mut c_void) -> u32;
    }
    const EGL_DRAW: i32 = 0x3059;
    // SAFETY: plain calls of EGL on what is current on this thread (nothing, under
    // another renderer: the call then fails and changes nothing). The framework finds
    // its surface gone at the next frame, which is what the step is for.
    let destroyed = unsafe {
        let display = eglGetCurrentDisplay();
        let surface = eglGetCurrentSurface(EGL_DRAW);
        eglDestroySurface(display, surface) != 0
    };
    tracing::error!(
        destroyed,
        "graphics: the surface was taken away by a verification run"
    );
}

#[cfg(not(all(target_os = "android", feature = "automation")))]
pub fn lose_surface() {}

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
    UNTOLD.store(!told, Ordering::Relaxed);
    match DEVICE.get() {
        Some(device) => {
            device.destroy();
            // What is under way is finished first; then the device says that it is gone.
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
        // The UI framework's OpenGL renderer has no device that could be taken away:
        // the loss is said as [`note_gl_errors`] says it of a driver that gave up.
        None if told => {
            tracing::error!("graphics: the device is lost: taken away by a verification run");
            LOST.store(true, Ordering::Release);
        }
        // Not told, there is nothing to show: no device is gone, and the app draws on.
        // (Nothing is marked lost: a real loss later in the run must still be seen.)
        None => {
            UNTOLD.store(false, Ordering::Relaxed);
            tracing::warn!("graphics: there is no device to take away; nothing was done");
        }
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
    ADAPTER.get()?.1
}

fn interface_word(backend: wgpu::Backend) -> Option<&'static str> {
    match backend {
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

        // What a lost device is written down as: only the two there is a choice between.
        assert_eq!(interface_word(wgpu::Backend::Vulkan), Some("vulkan"));
        assert_eq!(interface_word(wgpu::Backend::Gl), Some("gl"));
        assert_eq!(interface_word(wgpu::Backend::Metal), None);
    }

    /// Under the UI framework's OpenGL renderer the driver is asked about errors: two
    /// of them leave nothing to draw with, the others are mistakes that are written
    /// down and lived with.
    #[test]
    fn of_opengls_errors_only_lost_memory_and_a_lost_context_are_a_lost_device() {
        assert!(gl_error_is_a_loss(GL_OUT_OF_MEMORY));
        assert!(gl_error_is_a_loss(GL_CONTEXT_LOST));
        for code in [0x0500, 0x0501, 0x0502, 0x0503, 0x0504, 0x0506, 0x9999] {
            assert!(!gl_error_is_a_loss(code), "0x{code:04X}");
        }
        assert_eq!(gl_error_words(GL_OUT_OF_MEMORY), "out of memory");
        assert_eq!(gl_error_words(GL_CONTEXT_LOST), "the context is lost");
        assert_eq!(gl_error_words(0x0502), "invalid operation");
        assert_eq!(gl_error_words(0x9999), "an error without a name");
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

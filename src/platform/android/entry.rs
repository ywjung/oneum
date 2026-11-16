//! Where the app starts on Android. The activity loads the app as a shared library and
//! runs `android_main` on a thread of its own; that thread is the app's main thread
//! from then on (the window, the drawing). Folders the app uses are told through the
//! environment, which the activity sets before the library is loaded.
use crate::app::Core;
use std::sync::{Arc, OnceLock};
use winit::platform::android::activity::AndroidApp;

/// The running app, for the moment the activity goes away (see [`end`]).
static CORE: OnceLock<Arc<Core>> = OnceLock::new();

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    super::java::set_app(app.clone());
    install_panic_report();
    let startup = match crate::app::start() {
        Ok(startup) => startup,
        Err(error) => {
            tracing::error!("startup failed: {error}");
            super::fatal_alert(
                "OnEum",
                &format!("{error}\n\n{}", crate::app::log_dir_hint()),
            );
            return;
        }
    };
    let core = startup.core.clone();
    let _ = CORE.set(core.clone());
    let prefer = graphics_preference(&startup);
    let options = eframe::NativeOptions {
        android_app: Some(app),
        // The framework's OpenGL renderer where that was chosen, else wgpu.
        renderer: if prefer == crate::ui::renderer::Prefer::Glow {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        // Every graphics adapter that can show the window is tried, not only the first.
        wgpu_options: crate::ui::renderer::wgpu_options(prefer),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OnEum",
        options,
        Box::new(move |cc| Ok(Box::new(crate::ui::OneumApp::new(cc, startup)))),
    );
    if let Err(error) = result {
        tracing::error!("event loop failed: {error}");
        super::fatal_alert(
            "OnEum",
            &format!("{error}\n\n{}", crate::app::log_dir_hint()),
        );
    }
    core.shutdown();
    // The process must not outlive its one window: a later start in the same process
    // could not make another (and would find this one's state).
    std::process::exit(0);
}

/// Which interface to the graphics hardware this run tries first: what the settings
/// say, and while they leave it to the app, the other one than a run lost its device
/// on. An interface that is not the system's first choice is tried on probation (see
/// `app::relaunch::begin_trial`): if the run before was trying it and did not get far,
/// this run goes by the system's order.
fn graphics_preference(startup: &crate::app::Startup) -> crate::ui::renderer::Prefer {
    use crate::app::relaunch;
    use crate::ui::renderer::Prefer;
    let dir = &startup.core.paths.data_dir;
    let lost_on = relaunch::read(dir).and_then(|note| note.lost_on);
    // A verification run may name the interface, so that all of it can be run on
    // either (not on trial: such a run is ended and started again at any moment).
    if cfg!(feature = "automation") {
        match std::env::var("ONEUM_GRAPHICS").as_deref() {
            Ok("gl") => return Prefer::OpenGl,
            Ok("vulkan") => return Prefer::Vulkan,
            Ok("glow") => return Prefer::Glow,
            _ => {}
        }
    }
    let prefer = Prefer::from_choice(startup.settings.graphics, lost_on);
    let word = match prefer {
        Prefer::System => return prefer,
        Prefer::Vulkan => "vulkan",
        Prefer::OpenGl => "gl",
        Prefer::Glow => "glow",
    };
    if relaunch::begin_trial(dir, word) {
        tracing::info!(interface = word, "graphics: this interface is tried first");
        prefer
    } else {
        tracing::error!(
            interface = word,
            "graphics: the last start with this interface did not get far; \
             the system's order is used"
        );
        Prefer::System
    }
}

/// The activity is gone for good: the session is saved and the process ends.
pub(super) fn end() {
    tracing::info!("the activity is gone: ending");
    if let Some(core) = CORE.get() {
        // An end of this kind is no failure of an interface that was on trial.
        crate::app::relaunch::end_trial(&core.paths.data_dir);
        core.shutdown();
    }
    tracing::info!("OnEum stopped");
    std::process::exit(0);
}

/// Panics go in the log, whatever thread they happen on.
fn install_panic_report() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        tracing::error!(thread = name, %location, "panic: {message}");
        crate::app::note_panic(format!("{message} ({location})"));
        previous(info);
    }));
}

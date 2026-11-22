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
    let options = eframe::NativeOptions {
        android_app: Some(app),
        // Drawn through Vulkan, and through OpenGL ES only on a phone without a
        // Vulkan that can show the window (`renderer::wgpu_options`). There is no
        // choice of it: what was measured and mended on a real phone is Vulkan.
        wgpu_options: crate::ui::renderer::wgpu_options(graphics_for_verification()),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OnEum",
        options,
        Box::new(move |cc| Ok(Box::new(crate::ui::OneumApp::new(cc, startup)))),
    );
    match result {
        Err(error) => {
            tracing::error!("event loop failed: {error}");
            super::fatal_alert(
                "OnEum",
                &format!("{error}\n\n{}", crate::app::log_dir_hint()),
            );
        }
        // The framework ends its loop, and says nothing, when something about the
        // window is told before the window is there: the screen turns, or a split
        // screen is resized, while the app starts. Nothing was drawn and nothing is
        // wrong, but the loop cannot be entered again in this process: the app is
        // opened once more, as after a lost device.
        Ok(()) if crate::ui::nothing_drawn() => {
            let dir = &core.paths.data_dir;
            let again = crate::app::relaunch::note_early_end(dir, crate::app::relaunch::now())
                && super::open_again_after_exit();
            tracing::warn!(again, "the window's loop ended before the window was there");
        }
        Ok(()) => {}
    }
    core.shutdown();
    // The process must not outlive its one window: a later start in the same process
    // could not make another (and would find this one's state).
    std::process::exit(0);
}

/// A verification run may name the interface to the graphics hardware (`ONEUM_GRAPHICS`:
/// "vulkan" or "gl"), so that the app can be run on the one a phone without Vulkan
/// would get. Only where the verification channel is built in; otherwise the order of
/// `renderer::rank` holds.
fn graphics_for_verification() -> crate::ui::renderer::Prefer {
    use crate::ui::renderer::Prefer;
    if cfg!(feature = "automation") {
        match std::env::var("ONEUM_GRAPHICS").as_deref() {
            Ok("gl") => return Prefer::OpenGl,
            Ok("vulkan") => return Prefer::Vulkan,
            _ => {}
        }
    }
    Prefer::System
}

/// The activity is gone: the session is saved and the process ends. `finishing`: for
/// good. Otherwise the system is making the screen anew, for a change it does not
/// leave to the app (the colours of a new wallpaper, an overlay, a font), and the
/// app's shell has it opened again once this process is gone (`MainActivity`'s
/// `onDestroy`): music that was playing is noted, and goes on in the run that
/// follows.
pub(super) fn end(finishing: bool) {
    tracing::info!(finishing, "the activity is gone: ending");
    if let Some(core) = CORE.get() {
        if !finishing {
            let playing = matches!(
                core.player.snapshot().state,
                crate::playback::PlayState::Playing | crate::playback::PlayState::Loading
            );
            crate::app::relaunch::note_remade(
                &core.paths.data_dir,
                playing,
                crate::app::relaunch::now(),
            );
        }
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

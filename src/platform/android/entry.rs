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
        // Every graphics adapter that can show the window is tried, not only the first.
        wgpu_options: crate::ui::renderer::wgpu_options(),
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

/// The activity is gone for good: the session is saved and the process ends.
pub(super) fn end() {
    tracing::info!("the activity is gone: ending");
    if let Some(core) = CORE.get() {
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
        previous(info);
    }));
}

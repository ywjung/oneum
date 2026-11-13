//! OnEum — a local music player for macOS and Windows.
// A Windows GUI program: no console window behind it.
#![cfg_attr(windows, windows_subsystem = "windows")]
use eframe::egui;
use oneum::{app, platform::app_hooks, ui::OneumApp};

fn main() {
    // Must be registered before the event loop exists so that files the app is launched
    // with (double-click in Finder) are delivered.
    app_hooks::install_before_launch();
    let startup = match app::start() {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("OnEum could not start: {error}");
            tracing::error!("startup failed: {error}");
            // Launched from Finder nobody sees stderr: say it in a dialog.
            startup_alert(&error.to_string());
            std::process::exit(1);
        }
    };
    let core = startup.core.clone();
    // Restore the last window size (clamped to something usable); macOS picks the position.
    let size = startup
        .settings
        .window_size
        .filter(|[w, h]| w.is_finite() && h.is_finite())
        .map_or([1180.0, 760.0], |[w, h]| {
            [w.clamp(760.0, 6000.0), h.clamp(480.0, 4000.0)]
        });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OnEum")
            .with_inner_size(size)
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true)
            .with_icon(window_icon()),
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            // The menu bar is built by the app (platform::macos::menu).
            builder.with_default_menu(false);
        })),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OnEum",
        options,
        Box::new(move |cc| Ok(Box::new(OneumApp::new(cc, startup)))),
    );
    if let Err(error) = result {
        tracing::error!("event loop failed: {error}");
        startup_alert(&error.to_string());
        core.shutdown();
        std::process::exit(1);
    }
    // Normally `on_exit` has already done this (⌘Q terminates the process from inside
    // the event loop); it is idempotent.
    core.shutdown();
}

/// The icon of the window and the taskbar button. On a Mac the icon comes from the app
/// bundle, so none is set here.
fn window_icon() -> egui::IconData {
    #[cfg(not(target_os = "macos"))]
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-160.png")) {
        return icon;
    }
    egui::IconData::default()
}

/// Tells the user why the app is not coming up, in the system language.
fn startup_alert(detail: &str) {
    use oneum::i18n::Lang;
    let lang = if oneum::platform::locale::prefers_korean() {
        Lang::Ko
    } else {
        Lang::En
    };
    let s = lang.strings();
    let log = oneum::app::log_dir_hint();
    oneum::platform::fatal_alert(
        s.startup_failed_title,
        &format!(
            "{}\n\n{detail}\n\n{}: {log}",
            s.startup_failed_body, s.startup_failed_log
        ),
    );
}

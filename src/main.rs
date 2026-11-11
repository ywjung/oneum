//! OnEum — a local music player for macOS.
use eframe::egui;
use oneum::{app, platform::macos::app_hooks, ui::OneumApp};

fn main() {
    // Must be registered before the event loop exists so that files the app is launched
    // with (double-click in Finder) are delivered.
    app_hooks::install_before_launch();
    let startup = match app::start() {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("OnEum could not start: {error}");
            tracing::error!("startup failed: {error}");
            std::process::exit(1);
        }
    };
    let core = startup.core.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OnEum")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true),
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
    }
    // Normally `on_exit` has already done this (⌘Q terminates the process from inside
    // the event loop); it is idempotent.
    core.shutdown();
}

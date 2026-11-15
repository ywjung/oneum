//! OnEum — a local music player for macOS, Windows and the iPhone.
// A Windows GUI program: no console window behind it.
#![cfg_attr(windows, windows_subsystem = "windows")]
use eframe::egui;
use oneum::{app, platform::app_hooks, ui::OneumApp};

fn main() {
    install_panic_report();
    // Must be registered before the event loop exists so that files the app is launched
    // with (double-click in Finder) are delivered.
    app_hooks::install_before_launch();
    let startup = match app::start() {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("OnEum could not start: {error}");
            tracing::error!("startup failed: {error}");
            // Launched from Finder nobody sees stderr: say it in a dialog.
            startup_alert(Problem::Library, &error.to_string());
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
        // Every graphics adapter that can show the window is tried, not only the first.
        wgpu_options: oneum::ui::renderer::wgpu_options(oneum::ui::renderer::Prefer::System),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OnEum",
        options,
        Box::new(move |cc| Ok(Box::new(OneumApp::new(cc, startup)))),
    );
    if let Err(error) = result {
        tracing::error!("event loop failed: {error}");
        let problem = if matches!(error, eframe::Error::Wgpu(_)) {
            Problem::Graphics
        } else {
            Problem::Window
        };
        startup_alert(problem, &error.to_string());
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

/// What kept the app from coming up (or ended it).
#[derive(Clone, Copy)]
enum Problem {
    Library,
    Graphics,
    Window,
    Crash,
}

/// Tells the user why the app is not coming up (or is going away), in the system
/// language, with the details and where the log is.
fn startup_alert(problem: Problem, detail: &str) {
    use oneum::i18n::Lang;
    let lang = if oneum::platform::locale::prefers_korean() {
        Lang::Ko
    } else {
        Lang::En
    };
    let s = lang.strings();
    let (title, body) = match problem {
        Problem::Library => (s.startup_failed_title, s.startup_failed_body),
        Problem::Graphics => (s.startup_failed_title, s.startup_graphics_body),
        Problem::Window => (s.startup_failed_title, s.startup_window_body),
        Problem::Crash => (s.crashed_title, s.crashed_body),
    };
    let log = oneum::app::log_dir_hint();
    let copy = if cfg!(windows) {
        format!("\n\n{}", s.alert_copy_hint)
    } else {
        String::new()
    };
    oneum::platform::fatal_alert(
        title,
        &format!(
            "{body}\n\n{detail}\n\n{}: {log}{copy}",
            s.startup_failed_log
        ),
    );
}

/// Panics go in the log, whatever thread they happen on (many are caught there: a scan
/// or a decoder that fails on one file). One on the main thread ends the app (on
/// Windows the window system aborts the process right after), so that one is also told
/// in a dialog rather than being a silent exit.
fn install_panic_report() {
    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
        oneum::app::note_panic(format!("{message} ({location})"));
        previous(info);
        // A panic that is caught where it happens (`app::panics::catch`) does not end
        // the app: no dialog for it.
        if name == "main"
            && !oneum::app::panics::is_caught()
            && !SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            startup_alert(Problem::Crash, &format!("{message}\n({location})"));
        }
    }));
}

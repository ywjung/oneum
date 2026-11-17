//! Windows integration. Mirrors the names of the `macos` module so the rest of the app is
//! the same on both systems.
//!
//! What Windows does differently, and what is not implemented there:
//! - The menu bar is drawn inside the window (`ui::menubar`); there is no system menu.
//! - Closing the window quits (there is no Dock icon to bring a hidden window back).
//! - Files are handed to the app on its command line (Explorer's "Open with"). A second
//!   start hands its files to the window that is already open and ends (`app_hooks`).
//! - Media keys and the system's media overlay go through the System Media Transport
//!   Controls of the main window (`nowplaying`).
//! - The output device is always used in shared mode at the rate Windows runs it at:
//!   exclusive access and sample-rate matching are macOS features (`DEVICE_CONTROL`).

use std::path::{Path, PathBuf};

/// Runs `f` on the main thread soon (from any thread): it is handed to a hidden window
/// of that thread (see `hidden`). Without that window it runs here.
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    hidden::run_on_main(Box::new(f));
}

/// A hidden message-only window on the main thread. It runs work handed to the main
/// thread, and it is how a second start of the app finds the first one: the second
/// sends its files there (`WM_COPYDATA`) and ends once the first has acknowledged them.
mod hidden {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, Ordering};

    type Hwnd = isize;
    type Job = Box<dyn FnOnce() + Send>;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[repr(C)]
    struct CopyData {
        data: usize,
        len: u32,
        ptr: *const u8,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class: *const u16,
            name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Hwnd,
            menu: isize,
            instance: isize,
            param: *mut core::ffi::c_void,
        ) -> Hwnd;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn FindWindowExW(parent: Hwnd, after: Hwnd, class: *const u16, name: *const u16) -> Hwnd;
        fn SendMessageTimeoutW(
            hwnd: Hwnd,
            msg: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
        fn AllowSetForegroundWindow(process: u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }

    const HWND_MESSAGE: Hwnd = -3;
    const WM_APP: u32 = 0x8000;
    const WM_COPYDATA: u32 = 0x004A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;
    const ASFW_ANY: u32 = u32::MAX;
    /// Marks the files message, and what the receiving app answers to it.
    const OPEN_FILES: usize = 0x4F50_454E;
    const RECEIVED: isize = 0x004F_4E45;
    const CLASS: &str = "OnEumMessageWindow";

    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The window's name: one per data folder, so that test profiles (`ONEUM_DATA_DIR`)
    /// run side by side with the user's app.
    fn window_name() -> String {
        let folder = std::env::var("ONEUM_DATA_DIR").unwrap_or_default();
        format!("OnEum {:016x}", crate::library::fnv1a64(folder.as_bytes()))
    }

    pub(super) fn run_on_main(job: Job) {
        let window = WINDOW.load(Ordering::Acquire);
        if window == 0 {
            job();
            return;
        }
        JOBS.lock().unwrap_or_else(|e| e.into_inner()).push(job);
        // SAFETY: plain message to a window of this process; no pointers are passed.
        if unsafe { PostMessageW(window, WM_APP, 0, 0) } == 0 {
            // Not posted: run what is waiting here rather than never.
            let jobs = std::mem::take(&mut *JOBS.lock().unwrap_or_else(|e| e.into_inner()));
            for job in jobs {
                job();
            }
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        match msg {
            WM_APP => {
                let jobs = std::mem::take(&mut *JOBS.lock().unwrap_or_else(|e| e.into_inner()));
                for job in jobs {
                    // A panic must not cross into the system's code (that aborts).
                    let _ = crate::app::panics::catch(job);
                }
                0
            }
            WM_COPYDATA if lparam != 0 => {
                // SAFETY: for WM_COPYDATA the system passes a valid COPYDATASTRUCT whose
                // buffer lives until this call returns.
                let data = unsafe { &*(lparam as *const CopyData) };
                if data.data != OPEN_FILES || data.ptr.is_null() {
                    return 0;
                }
                let bytes = unsafe { std::slice::from_raw_parts(data.ptr, data.len as usize) };
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes(*b))
                    .collect();
                let paths: Vec<PathBuf> = units
                    .split(|&u| u == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| PathBuf::from(String::from_utf16_lossy(part)))
                    .collect();
                let _ = crate::app::panics::catch(|| {
                    super::app_hooks::receive(paths);
                    super::window::bring_to_front();
                });
                RECEIVED
            }
            // SAFETY: the default handling for everything else, with the same arguments.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// Hands `paths` to an OnEum that is open already. True if one took them.
    pub(super) fn hand_to_running(paths: &[PathBuf]) -> bool {
        let (class, name) = (wide(CLASS), wide(&window_name()));
        // SAFETY: NUL-terminated strings that outlive the call.
        let other = unsafe { FindWindowExW(HWND_MESSAGE, 0, class.as_ptr(), name.as_ptr()) };
        if other == 0 {
            return false;
        }
        let mut payload: Vec<u16> = Vec::new();
        for path in paths {
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            payload.extend(absolute.to_string_lossy().encode_utf16());
            payload.push(0);
        }
        payload.push(0);
        let data = CopyData {
            data: OPEN_FILES,
            len: (payload.len() * 2) as u32,
            ptr: payload.as_ptr().cast(),
        };
        let mut answer: usize = 0;
        // SAFETY: `data` and `payload` live until the call returns (the system copies
        // them to the other process); the answer goes to a local.
        let sent = unsafe {
            // The running app may then bring its window to the front.
            AllowSetForegroundWindow(ASFW_ANY);
            SendMessageTimeoutW(
                other,
                WM_COPYDATA,
                0,
                (&raw const data) as isize,
                SMTO_ABORTIFHUNG,
                5_000,
                &mut answer,
            )
        };
        sent != 0 && answer as isize == RECEIVED
    }

    /// Creates the window (on the main thread, before the event loop starts).
    pub(super) fn create() {
        let (class, name) = (wide(CLASS), wide(&window_name()));
        // SAFETY: the class structure and the strings outlive the calls; the window
        // procedure is a plain function of this program.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WndClassW {
                style: 0,
                wnd_proc: window_proc,
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: 0,
                cursor: 0,
                background: 0,
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            RegisterClassW(&wc);
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                instance,
                std::ptr::null_mut(),
            );
            if window == 0 {
                tracing::warn!(
                    "no message window: work for the main thread runs where it is handed over"
                );
            }
            WINDOW.store(window, Ordering::Release);
        }
    }
}

/// Lets `duration` pass on the main thread (the counterpart of the macOS run loop pump:
/// nothing on Windows is served through it).
pub fn pump_main_loop(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// Text in the Korean code page (CP949) as Unicode. `None` if the bytes are not that.
pub fn decode_cp949(bytes: &[u8]) -> Option<String> {
    const CP949: u32 = 949;
    /// Fail on bytes that are not valid in the code page instead of replacing them.
    const MB_ERR_INVALID_CHARS: u32 = 8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            bytes: *const u8,
            byte_count: i32,
            wide: *mut u16,
            wide_count: i32,
        ) -> i32;
    }
    let count = i32::try_from(bytes.len()).ok()?;
    if count == 0 {
        return Some(String::new());
    }
    // SAFETY: pointer and length describe `bytes`; with no output buffer the function
    // only reports how many UTF-16 units the text needs.
    let needed = unsafe {
        MultiByteToWideChar(
            CP949,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            count,
            std::ptr::null_mut(),
            0,
        )
    };
    let mut wide = vec![0u16; usize::try_from(needed).ok().filter(|n| *n > 0)?];
    // SAFETY: as above, and the output buffer has exactly the `needed` units.
    let written = unsafe {
        MultiByteToWideChar(
            CP949,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            count,
            wide.as_mut_ptr(),
            needed,
        )
    };
    (written == needed)
        .then(|| String::from_utf16(&wide).ok())
        .flatten()
}

/// Windows' own Explorer, by its full path: a program of that name lying next to
/// OnEum.exe (the app can be unzipped into any folder) must never be started instead.
fn explorer() -> std::process::Command {
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .filter(|root| root.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    std::process::Command::new(system.join("explorer.exe"))
}

/// Hands a web address (already checked by the caller) to the default browser.
pub(super) fn open_in_browser(address: &str) -> bool {
    // Explorer opens a web address with whatever handles web addresses.
    explorer().arg(address).spawn().is_ok()
}

/// Shows a blocking error message (start-up failures).
pub fn fatal_alert(title: &str, body: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(body)
        .show();
}

pub mod app_hooks {
    use super::PathBuf;
    use std::sync::{Mutex, OnceLock};

    static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

    /// Remembers the files the app was started with (double-click, "Open with", a file
    /// dropped on the program). If OnEum is open already, they go to it and this start
    /// ends here; otherwise this one becomes the one that later starts hand files to.
    pub fn install_before_launch() {
        let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        if super::hidden::hand_to_running(&paths) {
            std::process::exit(0);
        }
        super::hidden::create();
        receive(paths);
    }

    pub fn install_now() -> bool {
        true
    }

    /// `waker` is called when files arrive from another start of the app.
    pub fn set_waker(waker: impl Fn() + Send + Sync + 'static) {
        let _ = WAKER.set(Box::new(waker));
    }

    pub(super) fn receive(paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        OPENED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(paths);
        if let Some(wake) = WAKER.get() {
            wake();
        }
    }

    /// The files to open, once.
    pub fn take_opened_paths() -> Vec<PathBuf> {
        std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub mod finder {
    use super::Path;

    /// Shows `path` in File Explorer: a file is selected in its folder, a folder is opened.
    pub fn reveal(path: &Path) -> bool {
        let mut explorer = super::explorer();
        if path.is_dir() {
            explorer.arg(path);
        } else {
            // Explorer parses its own command line and splits it at commas: the path
            // goes in quotes of its own (a file name cannot contain a quote), written
            // as it is rather than quoted as a whole by the standard library.
            use std::os::windows::process::CommandExt;
            let mut select = std::ffi::OsString::from("/select,\"");
            select.push(path);
            select.push("\"");
            explorer.raw_arg(select);
        }
        // Explorer returns a non-zero status even when it worked; starting it is enough.
        explorer.spawn().is_ok()
    }

    /// `DRIVE_REMOTE` of `GetDriveTypeW`.
    const DRIVE_REMOTE: u32 = 4;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }

    /// Whether `path` is on a drive of this computer rather than on a network share.
    /// `None` if that cannot be told from the path.
    pub fn is_on_local_volume(path: &Path) -> Option<bool> {
        use std::path::{Component, Prefix};
        let prefix = match path.components().next()? {
            Component::Prefix(prefix) => prefix.kind(),
            _ => return None,
        };
        let letter = match prefix {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return Some(false),
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return None,
        };
        let root: Vec<u16> = [letter, b':', b'\\', 0]
            .iter()
            .map(|&b| u16::from(b))
            .collect();
        // SAFETY: `root` is a NUL-terminated wide string that outlives the call.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        Some(kind != DRIVE_REMOTE)
    }
}

pub mod locale {
    /// Primary language id of Korean (`LANG_KOREAN`).
    const LANG_KOREAN: u16 = 0x12;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    /// True when Windows is set to Korean.
    pub fn prefers_korean() -> bool {
        // SAFETY: the function takes no arguments, cannot fail and only reads the
        // user's language setting.
        let language = unsafe { GetUserDefaultUILanguage() };
        // The low ten bits are the primary language.
        language & 0x3FF == LANG_KOREAN
    }
}

pub mod menu {
    use crate::i18n::Strings;
    use crate::platform::MenuAction;

    /// There is no system menu on Windows: the menu bar is part of the window and is
    /// drawn (and its shortcuts handled) by `ui::menubar`. This type only keeps the
    /// same shape as the macOS one.
    pub struct AppMenu;

    impl AppMenu {
        pub fn install(
            _s: &Strings,
            _version: &str,
            _on_action: impl Fn(MenuAction) + Send + Sync + 'static,
        ) -> Option<Self> {
            Some(Self)
        }
    }
}

pub mod nowplaying {
    use super::PathBuf;

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum RemoteCommand {
        Play,
        Pause,
        Toggle,
        Stop,
        Next,
        Previous,
        /// Seek to this many seconds.
        Seek(f64),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct NowPlayingInfo {
        pub title: String,
        pub artist: String,
        pub album: String,
        pub duration_s: f64,
        pub position_s: f64,
        pub playing: bool,
        /// How fast the song goes, as a multiple of its own speed. (Windows shows
        /// the position it is told and does not move it by itself.)
        pub rate: f64,
        /// Cached cover image file, if any.
        pub art_path: Option<PathBuf>,
    }

    use std::sync::Arc;
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButton,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
    use windows::core::{HSTRING, Ref};

    /// The System Media Transport Controls of the main window: the media keys of the
    /// keyboard and the media overlay of Windows. Without a main window (or where the
    /// system refuses them) there are none and nothing happens.
    pub struct MediaSession {
        controls: Option<SystemMediaTransportControls>,
    }

    impl MediaSession {
        pub fn install(send: impl Fn(RemoteCommand) + Send + Sync + 'static) -> Self {
            let controls = super::window::main_window().and_then(|hwnd| {
                open(hwnd, Arc::new(send))
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "Windows media controls are unavailable"),
                    )
                    .ok()
            });
            Self { controls }
        }

        pub fn update(&mut self, info: Option<&NowPlayingInfo>) {
            if let Some(controls) = &self.controls
                && let Err(e) = show(controls, info)
            {
                tracing::debug!(error = %e, "could not update the Windows media controls");
            }
        }
    }

    fn open(
        hwnd: isize,
        send: Arc<dyn Fn(RemoteCommand) + Send + Sync>,
    ) -> windows::core::Result<SystemMediaTransportControls> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // SAFETY: `hwnd` is the main window of this process, alive while the app runs.
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(hwnd as *mut core::ffi::c_void)) }?;
        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        let on_button = send.clone();
        controls.ButtonPressed(&TypedEventHandler::new(
            move |_: Ref<SystemMediaTransportControls>,
                  args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                let command = match args.ok()?.Button()? {
                    SystemMediaTransportControlsButton::Play => RemoteCommand::Play,
                    SystemMediaTransportControlsButton::Pause => RemoteCommand::Pause,
                    SystemMediaTransportControlsButton::Stop => RemoteCommand::Stop,
                    SystemMediaTransportControlsButton::Next => RemoteCommand::Next,
                    SystemMediaTransportControlsButton::Previous => RemoteCommand::Previous,
                    _ => return Ok(()),
                };
                on_button(command);
                Ok(())
            },
        ))?;
        controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_: Ref<SystemMediaTransportControls>,
                  args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                let at = args.ok()?.RequestedPlaybackPosition()?;
                send(RemoteCommand::Seek(
                    at.Duration.max(0) as f64 / 10_000_000.0,
                ));
                Ok(())
            },
        ))?;
        Ok(controls)
    }

    /// Seconds as a WinRT time span (units of 100 ns).
    fn span(seconds: f64) -> TimeSpan {
        TimeSpan {
            Duration: (seconds.max(0.0) * 10_000_000.0) as i64,
        }
    }

    fn show(
        controls: &SystemMediaTransportControls,
        info: Option<&NowPlayingInfo>,
    ) -> windows::core::Result<()> {
        let Some(info) = info else {
            controls.SetPlaybackStatus(MediaPlaybackStatus::Stopped)?;
            return controls.DisplayUpdater()?.ClearAll();
        };
        controls.SetPlaybackStatus(if info.playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        })?;
        let updater = controls.DisplayUpdater()?;
        updater.SetType(MediaPlaybackType::Music)?;
        let music = updater.MusicProperties()?;
        music.SetTitle(&HSTRING::from(info.title.as_str()))?;
        music.SetArtist(&HSTRING::from(info.artist.as_str()))?;
        music.SetAlbumTitle(&HSTRING::from(info.album.as_str()))?;
        updater.Update()?;
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
        timeline.SetStartTime(span(0.0))?;
        timeline.SetMinSeekTime(span(0.0))?;
        timeline.SetEndTime(span(info.duration_s))?;
        timeline.SetMaxSeekTime(span(info.duration_s))?;
        timeline.SetPosition(span(info.position_s))?;
        controls.UpdateTimelineProperties(&timeline)
    }
}

pub mod power {
    /// Sleep is not observed on Windows: the output stream is invalidated by the system
    /// when the device goes away, which the engine handles like any device change.
    pub fn observe_sleep(_on_sleep: impl Fn() + Send + Sync + 'static) {}
}

pub mod responder {
    /// Edit commands that a system menu would send. Windows has no such menu: the
    /// keyboard shortcuts reach the text fields directly.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EditAction {
        Cut,
        Copy,
        Paste,
        SelectAll,
        Undo,
        Redo,
    }

    pub fn install(_waker: impl Fn() + Send + Sync + 'static) -> bool {
        true
    }

    pub fn drain() -> Vec<EditAction> {
        Vec::new()
    }

    pub fn set_text_input_active(_active: bool) {}

    pub fn pasteboard_text() -> Option<String> {
        None
    }
}

pub mod system {
    /// `MEMORYSTATUSEX` of `GlobalMemoryStatusEx`.
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatus) -> i32;
    }

    /// Installed memory in bytes.
    pub fn physical_memory_bytes() -> Option<u64> {
        let mut status = MemoryStatus {
            length: std::mem::size_of::<MemoryStatus>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: `status` is a `MEMORYSTATUSEX` with its length set, as the function
        // requires; it only writes into it.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0 && status.total_phys > 0).then_some(status.total_phys)
    }
}

pub mod testing {
    //! The scripted-verification helpers that drive macOS itself have no counterpart.

    pub fn post_key(_key_code: u16, _command: bool, _shift: bool, _option: bool) -> bool {
        false
    }

    pub fn app_is_active() -> bool {
        false
    }

    pub fn menu_key(_key_code: u16, _command: bool, _shift: bool, _option: bool) -> bool {
        false
    }

    pub fn select_input_source(_id: &str) -> bool {
        false
    }

    pub fn current_input_source() -> Option<String> {
        None
    }
}

pub mod window {
    //! The window is never hidden on Windows (closing it quits), so there is nothing to
    //! hide or bring back. Its handle is kept for the media controls and for bringing it
    //! to the front when another start of the app hands files over.
    use std::sync::atomic::{AtomicIsize, Ordering};

    static MAIN: AtomicIsize = AtomicIsize::new(0);

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
    }
    const SW_RESTORE: i32 = 9;

    /// Remembers the main window (called once it exists).
    pub fn remember_main_window(handle: winit::raw_window_handle::RawWindowHandle) {
        if let winit::raw_window_handle::RawWindowHandle::Win32(h) = handle {
            MAIN.store(h.hwnd.get(), Ordering::Release);
        }
    }

    pub(super) fn main_window() -> Option<isize> {
        Some(MAIN.load(Ordering::Acquire)).filter(|h| *h != 0)
    }

    /// Restores the main window if minimised and brings it to the front.
    pub(super) fn bring_to_front() {
        if let Some(hwnd) = main_window() {
            // SAFETY: a window handle of this process; the calls only change its state.
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
            }
        }
    }

    /// A window on a desk has no margins the system asks to keep free.
    pub fn safe_area_insets() -> Option<[f32; 4]> {
        None
    }

    /// There is no on-screen keyboard over a window on a desk.
    pub fn keyboard_height() -> f32 {
        0.0
    }

    /// The window system tells egui whether the system is dark; nothing to add.
    pub fn system_is_dark() -> Option<bool> {
        None
    }

    /// The window's title bar follows the system by itself.
    pub fn set_appearance(_dark: Option<bool>) {}

    pub fn hide_main_window() {}

    pub fn show_main_window() {}

    pub fn order_front_without_activating() {}

    pub fn send_back() {}

    pub fn is_main_window_visible() -> bool {
        true
    }
}

pub use super::dialogs as files;
pub use super::no_device as device;

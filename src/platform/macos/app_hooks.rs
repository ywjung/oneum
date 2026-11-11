//! Application-level events that winit does not surface: files opened from Finder
//! (double-click, "Open With", drop on the Dock icon) and clicking the Dock icon while
//! the window is hidden.
//!
//! winit installs its own `NSApplicationDelegate` and requires it to stay installed, so
//! the two optional delegate methods we need are added to winit's delegate class at
//! runtime. This must happen before AppKit delivers the launch-time "open documents"
//! event, hence the `willFinishLaunching` observer.
use block2::RcBlock;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{MainThreadMarker, ffi, sel};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSURL};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static INSTALLED: AtomicBool = AtomicBool::new(false);
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Called (on the main thread) whenever new paths arrive or the Dock icon is clicked.
pub fn set_waker(waker: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(waker));
}

/// Paths the system asked us to open since the last call.
pub fn take_opened_paths() -> Vec<PathBuf> {
    OPENED
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

extern "C-unwind" fn application_open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    urls: *mut AnyObject,
) {
    if urls.is_null() {
        return;
    }
    // SAFETY: AppKit calls `application:openURLs:` with a valid `NSArray<NSURL *> *`
    // that stays alive for the duration of the call.
    let urls: &NSArray<NSURL> = unsafe { &*(urls.cast::<NSArray<NSURL>>()) };
    let paths: Vec<PathBuf> = urls
        .iter()
        .filter_map(|url| url.path())
        .map(|p| PathBuf::from(p.to_string()))
        .collect();
    if paths.is_empty() {
        return;
    }
    if let Ok(mut queue) = OPENED.lock() {
        queue.extend(paths);
    }
    // Opening a file should bring the (possibly hidden) window back.
    super::window::show_main_window();
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

extern "C-unwind" fn application_should_handle_reopen(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    super::window::show_main_window();
    if let Some(wake) = WAKER.get() {
        wake();
    }
    // We handled it; AppKit should not try to create an untitled window.
    Bool::NO
}

#[cfg(target_arch = "aarch64")]
const REOPEN_TYPES: &std::ffi::CStr = c"B@:@B";
#[cfg(not(target_arch = "aarch64"))]
const REOPEN_TYPES: &std::ffi::CStr = c"c@:@c";

/// Adds the delegate methods to winit's delegate class and makes AppKit notice them.
/// Idempotent. Returns false if winit's delegate class does not exist (yet).
pub fn install_now() -> bool {
    if INSTALLED.load(Ordering::Relaxed) {
        return true;
    }
    let Some(cls) = AnyClass::get(c"WinitApplicationDelegate") else {
        return false;
    };
    let cls = std::ptr::from_ref(cls).cast_mut();
    type OpenUrls = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);
    type Reopen = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool;
    // SAFETY: `cls` is a registered class. Each IMP matches the Objective-C signature of
    // the delegate method it is registered for (`void (id, SEL, NSApplication *, NSArray *)`
    // and `BOOL (id, SEL, NSApplication *, BOOL)`). winit's delegate does not implement
    // these selectors, so no existing behaviour is replaced.
    unsafe {
        ffi::class_addMethod(
            cls,
            sel!(application:openURLs:),
            std::mem::transmute::<OpenUrls, Imp>(application_open_urls),
            c"v@:@@".as_ptr(),
        );
        ffi::class_addMethod(
            cls,
            sel!(applicationShouldHandleReopen:hasVisibleWindows:),
            std::mem::transmute::<Reopen, Imp>(application_should_handle_reopen),
            REOPEN_TYPES.as_ptr(),
        );
    }
    // AppKit caches which optional methods a delegate implements when the delegate is
    // set, so set the same delegate again.
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        if let Some(delegate) = app.delegate() {
            app.setDelegate(None);
            app.setDelegate(Some(&delegate));
        }
    }
    INSTALLED.store(true, Ordering::Relaxed);
    true
}

/// Arranges for [`install_now`] to run just before the app finishes launching, which is
/// before AppKit delivers files the app was launched with. Call once from `main` before
/// the event loop is created.
pub fn install_before_launch() {
    let block = RcBlock::new(|_n: NonNull<NSNotification>| {
        if !install_now() {
            tracing::warn!("winit delegate class not found; Finder open events are unavailable");
        }
    });
    // SAFETY: the notification name is a valid AppKit constant; the block is copied by
    // the notification center and only touches process-global state.
    let token = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
            None,
            &block,
        )
    };
    // The observer is needed for the whole process lifetime.
    std::mem::forget(token);
}

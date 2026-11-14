//! What macOS and iOS have in common: Foundation, CoreFoundation and the MediaPlayer
//! framework. The modules of the two systems export these under their own names.
pub mod locale;
pub mod nowplaying;
pub mod system;

use block2::RcBlock;
use objc2_foundation::NSOperationQueue;
use std::cell::Cell;
use std::path::Path;

/// Runs `f` on the main thread soon (from any thread).
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    let cell = Cell::new(Some(f));
    let block = RcBlock::new(move || {
        if let Some(f) = cell.take() {
            f();
        }
    });
    // SAFETY: the block is copied by the queue and run once on the main thread; the
    // closure it wraps is `Send`.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

/// Text in the Korean code page of older Windows (CP949, a superset of EUC-KR) as
/// Unicode. `None` if the bytes are not that.
pub fn decode_cp949(bytes: &[u8]) -> Option<String> {
    use objc2_core_foundation::CFString;
    /// `kCFStringEncodingDOSKorean`.
    const CP949: u32 = 0x0422;
    let length = isize::try_from(bytes.len()).ok()?;
    // SAFETY: pointer and length describe `bytes`, which is alive for the call;
    // CoreFoundation copies what it converts and keeps nothing of ours.
    let string = unsafe { CFString::with_bytes(None, bytes.as_ptr(), length, CP949, false) }?;
    Some(string.to_string())
}

/// Runs the main thread's run loop for `duration`. The app has a run loop of its own;
/// command-line tools that open audio streams need this, because CoreAudio serves some
/// requests and all its notifications through the main run loop and calls made while
/// that loop is not running can wait for ever. Main thread only.
pub fn pump_main_loop(duration: std::time::Duration) {
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    // SAFETY: the constant is initialised by CoreFoundation before any code of ours runs.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, duration.as_secs_f64(), false);
}

/// Whether `path` is on a volume of this computer rather than on a network share
/// (where the system's file events miss changes made from other computers). `None`
/// if the system cannot tell (the path does not exist, or is not UTF-8).
pub fn is_on_local_volume(path: &Path) -> Option<bool> {
    use objc2_foundation::{NSArray, NSNumber, NSString, NSURL, NSURLVolumeIsLocalKey};
    let path = path.to_str()?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // SAFETY: a constant string that Foundation defines; it lives as long as the process.
    let key = unsafe { NSURLVolumeIsLocalKey };
    let values = url
        .resourceValuesForKeys_error(&NSArray::from_slice(&[key]))
        .ok()?;
    let local = values.objectForKey(key)?.downcast::<NSNumber>().ok()?;
    Some(local.boolValue())
}

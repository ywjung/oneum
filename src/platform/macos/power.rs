//! Sleep notification: playback is paused and the output stream released before the Mac
//! sleeps, so that waking up never resumes sound unexpectedly or on a stale device.
use block2::RcBlock;
use objc2_app_kit::{NSWorkspace, NSWorkspaceWillSleepNotification};
use objc2_foundation::NSNotification;
use std::ptr::NonNull;

/// Calls `on_sleep` (on the main thread) when the system is about to sleep.
pub fn observe_sleep(on_sleep: impl Fn() + Send + Sync + 'static) {
    let block = RcBlock::new(move |_n: NonNull<NSNotification>| on_sleep());
    // SAFETY: valid AppKit notification name; the block is copied by the center.
    let token = unsafe {
        NSWorkspace::sharedWorkspace()
            .notificationCenter()
            .addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceWillSleepNotification),
                None,
                None,
                &block,
            )
    };
    // Observed for the whole process lifetime.
    std::mem::forget(token);
}

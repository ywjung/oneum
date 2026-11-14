//! "Show in Finder", and what the system knows about the volume a path is on.
pub use crate::platform::apple::is_on_local_volume;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSArray, NSString, NSURL};
use std::path::Path;

/// Opens a Finder window with `path` selected. Returns false if the path is not valid UTF-8.
pub fn reveal(path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
    true
}

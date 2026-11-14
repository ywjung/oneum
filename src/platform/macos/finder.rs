//! "Show in Finder", and what the system knows about the volume a path is on.
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSArray, NSNumber, NSString, NSURL, NSURLVolumeIsLocalKey};
use std::path::Path;

/// Whether `path` is on a volume of this computer rather than on a network share
/// (where the system's file events miss changes made from other computers). `None`
/// if the system cannot tell (the path does not exist, or is not UTF-8).
pub fn is_on_local_volume(path: &Path) -> Option<bool> {
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

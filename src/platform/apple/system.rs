//! Facts about the computer.
use objc2_foundation::NSProcessInfo;

/// Installed memory in bytes.
pub fn physical_memory_bytes() -> Option<u64> {
    let bytes = NSProcessInfo::processInfo().physicalMemory();
    (bytes > 0).then_some(bytes)
}

//! The clock on the wall: how far local time is from UTC. The app keeps times as
//! seconds since 1970 (UTC); where a year begins for the listener is a matter of the
//! time zone the device is set to.

/// Seconds that local time is ahead of UTC now (Seoul: 32,400). 0 where it cannot be
/// told.
#[cfg(unix)]
pub fn utc_offset_secs() -> i32 {
    offset_at(None)
}

/// Seconds that local time was (or will be) ahead of UTC at `at` (Unix seconds):
/// where clocks are moved for the summer, it is not the same all year.
#[cfg(unix)]
pub fn utc_offset_secs_at(at: i64) -> i32 {
    offset_at(Some(at))
}

#[cfg(unix)]
fn offset_at(at: Option<i64>) -> i32 {
    // What C's library calls a broken-down time, as macOS, iOS and Android lay it out
    // (nine ints, then the offset and the zone's name).
    #[repr(C)]
    struct BrokenDown {
        fields: [std::ffi::c_int; 9],
        offset: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    unsafe extern "C" {
        fn time(at: *mut i64) -> i64;
        fn localtime_r(at: *const i64, into: *mut BrokenDown) -> *mut BrokenDown;
    }
    let mut local = BrokenDown {
        fields: [0; 9],
        offset: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: both are plain calls of the C library every program is linked with;
    // `localtime_r` writes into the struct it is given, which has its layout.
    let told = unsafe {
        let at = at.unwrap_or_else(|| time(std::ptr::null_mut()));
        !localtime_r(&at, &mut local).is_null()
    };
    if !told {
        return 0;
    }
    i32::try_from(local.offset)
        .ok()
        .filter(|offset| offset.abs() <= 18 * 3600)
        .unwrap_or(0)
}

/// Seconds that local time is ahead of UTC (the zone's standard time: a summer hour is
/// not counted). 0 where it cannot be told.
#[cfg(windows)]
pub fn utc_offset_secs() -> i32 {
    unsafe extern "C" {
        fn _tzset();
        fn _get_timezone(seconds_west: *mut std::ffi::c_long) -> std::ffi::c_int;
    }
    let mut west: std::ffi::c_long = 0;
    // SAFETY: plain calls of the C runtime; the second writes one number.
    let told = unsafe {
        _tzset();
        _get_timezone(&mut west) == 0
    };
    if !told {
        return 0;
    }
    i32::try_from(-i64::from(west))
        .ok()
        .filter(|offset| offset.abs() <= 18 * 3600)
        .unwrap_or(0)
}

/// As [`utc_offset_secs`]: the zone's standard time, whatever the moment. (A year
/// then begins an hour off where the new year falls into the summer's time.)
#[cfg(windows)]
pub fn utc_offset_secs_at(_at: i64) -> i32 {
    utc_offset_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On this machine: an offset that a time zone can have, in whole quarters of an
    /// hour (every zone there is).
    #[test]
    fn the_offset_is_one_a_time_zone_can_have() {
        let offset = utc_offset_secs();
        assert!(offset.abs() <= 14 * 3600, "{offset}");
        assert_eq!(offset % 900, 0, "{offset}");
        // At a moment that is named: an offset as well, and for now the one of now.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        assert_eq!(utc_offset_secs_at(now), offset);
        for at in [0i64, 1_767_225_600, 1_782_864_000] {
            let then = utc_offset_secs_at(at);
            assert!(then.abs() <= 14 * 3600 && then % 900 == 0, "{then}");
        }
    }
}

//! The driver's clock.
//!
//! IOKit stamps every input report with a mach absolute time. The pipeline compares those stamps
//! with "now" (contact expiry, the report census, gesture timing), so "now" must be read from the
//! same clock and converted the same way. Both live here, once.

/// Nanoseconds since boot, on the clock IOKit stamps reports with.
#[cfg(target_os = "macos")]
pub fn now_ns() -> u64 {
    // SAFETY: neither call has preconditions; `mach_absolute_time` never fails.
    unsafe { nanoseconds(mach2::mach_time::mach_absolute_time()) }
}

/// Converts a mach absolute timestamp into nanoseconds, through the mach timebase.
#[cfg(target_os = "macos")]
pub fn nanoseconds(ticks: u64) -> u64 {
    static TIMEBASE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        // SAFETY: `info` is a valid, writable timebase record; the call fills it.
        let info: mach2::mach_time::mach_timebase_info_data_t = unsafe {
            let mut info = std::mem::zeroed();
            mach2::mach_time::mach_timebase_info(&mut info);
            info
        };
        // The call does not fail in practice; 1:1 is the Apple Silicon timebase and keeps the
        // conversion well-defined if it ever did.
        let denom = if info.denom == 0 { 1 } else { u64::from(info.denom) };
        (u64::from(info.numer), denom)
    });
    ticks * numer / denom
}

/// Nanoseconds since the process started, on hosts whose backend delivers no reports.
#[cfg(not(target_os = "macos"))]
pub fn now_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Identity: the stub backend stamps no reports.
#[cfg(not(target_os = "macos"))]
pub fn nanoseconds(ticks: u64) -> u64 {
    ticks
}

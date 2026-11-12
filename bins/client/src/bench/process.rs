//! The process's memory, for the real-time streaming bench (#162).

/// What the system says of the process's resident memory, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resident {
    /// What is resident now: the working set on Windows, `VmRSS` on Linux.
    pub now: u64,
    /// The most that has been resident since the process began (the peak working set on Windows,
    /// `VmHWM` on Linux). It cannot be reset on Windows, so it covers everything the process did
    /// before: the normal bench's generation of the Planet, and the earlier runs.
    pub peak_since_start: u64,
}

/// The process's resident memory, or None where the system does not say.
pub fn resident() -> Option<Resident> {
    read()
}

#[cfg(target_os = "linux")]
fn read() -> Option<Resident> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = |key: &str| -> Option<u64> {
        let line = status.lines().find_map(|l| l.strip_prefix(key))?;
        line.split_whitespace().next()?.parse::<u64>().ok()
    };
    let now = kib("VmRSS:")? * 1024;
    // The kernel batches the counters that `VmHWM` is made of, so it can read a little below `VmRSS`
    // (seen on a 7.0 kernel): the peak is never below what is resident now.
    Some(Resident {
        now,
        peak_since_start: (kib("VmHWM:")? * 1024).max(now),
    })
}

#[cfg(windows)]
fn read() -> Option<Resident> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    counters.cb = size;
    // SAFETY: `counters` is a PROCESS_MEMORY_COUNTERS that is `size` bytes, which is what the call
    // writes at most, and the pseudo-handle of the current process needs no closing.
    let read = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (read != 0).then_some(Resident {
        now: counters.WorkingSetSize as u64,
        peak_since_start: counters.PeakWorkingSetSize as u64,
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read() -> Option<Resident> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn the_resident_set_is_what_the_system_says_and_the_peak_is_never_below_it() {
        let held = vec![1u8; 32 << 20];
        // The pages are touched, so they are resident.
        assert!(held.iter().map(|&b| u64::from(b)).sum::<u64>() > 0);
        let resident = resident().expect("this system reports its memory");
        assert!(resident.now >= 32 << 20, "{resident:?}");
        assert!(resident.peak_since_start >= resident.now, "{resident:?}");
    }
}

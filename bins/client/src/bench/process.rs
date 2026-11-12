//! The process's peak memory, for the real-time streaming bench (#162).

/// The most memory the process has had resident, in bytes: the peak working set on Windows, the
/// high-water mark of the resident set (`VmHWM`) on Linux. It is the peak since the process began,
/// so it only grows over the runs of one process. None where the system does not say.
pub fn peak_bytes() -> Option<u64> {
    peak()
}

#[cfg(target_os = "linux")]
fn peak() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(windows)]
fn peak() -> Option<u64> {
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
    (read != 0).then_some(counters.PeakWorkingSetSize as u64)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn peak() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn the_peak_is_what_the_system_says_and_is_at_least_what_is_held_now() {
        let held = vec![1u8; 32 << 20];
        // The pages are touched, so they are resident.
        assert!(held.iter().map(|&b| u64::from(b)).sum::<u64>() > 0);
        let peak = peak_bytes().expect("this system reports its peak");
        assert!(peak >= 32 << 20, "{peak}");
    }
}

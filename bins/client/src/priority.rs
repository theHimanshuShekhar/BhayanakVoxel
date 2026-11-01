//! Lowers the priority of the calling thread, so that the loader's workers never take a core from
//! the simulation or the frame thread (#86). The only `unsafe` in the workspace: the standard
//! library has no way to do this.

/// Makes the calling thread the lowest priority the platform has for an ordinary thread. Does
/// nothing where the platform will not (the thread keeps its priority).
#[allow(unsafe_code)]
pub fn lower_current_thread() {
    #[cfg(unix)]
    // SAFETY: `setpriority` takes plain integers. On Linux, who = 0 is the calling thread, whose
    // nice value is its own; elsewhere it is the process, which the loader's threads share with
    // nothing that matters more than the frame thread.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    #[cfg(windows)]
    // SAFETY: the pseudo-handle of the calling thread is always valid.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[allow(unsafe_code)]
    fn nice() -> i32 {
        // SAFETY: reads the calling thread's nice value.
        unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }
    }

    #[test]
    fn a_thread_that_lowers_itself_is_nice_19_and_the_others_are_not_touched() {
        let before = nice();
        let lowered = std::thread::spawn(|| {
            super::lower_current_thread();
            nice()
        })
        .join()
        .unwrap();
        assert_eq!(lowered, 19);
        assert_eq!(nice(), before, "only the thread that asked");
    }
}

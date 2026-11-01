//! Lowers the priority of the calling thread, so that the loader's workers never take a core from
//! the simulation or the frame thread (#86). The only `unsafe` in the workspace: the standard
//! library has no way to do this.

use std::sync::Once;

/// Says once that the priority could not be lowered, and why: the workers then compete with the
/// frame thread, which is slower but not wrong.
fn failed(what: &str) {
    static SAID: Once = Once::new();
    SAID.call_once(|| eprintln!("warning: could not lower the ring loader's priority: {what}"));
}

/// Makes the calling thread the lowest priority the platform has for an ordinary thread. Does
/// nothing on platforms where that is not done for a thread alone (a Unix other than Linux sets
/// the priority of the whole process), and says once if the platform refuses.
pub fn lower_current_thread() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `setpriority` takes plain integers. On Linux, who = 0 is the calling thread,
        // whose nice value is its own.
        let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) };
        if result != 0 {
            failed(&std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        // SAFETY: the pseudo-handle of the calling thread is always valid.
        let result = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
        if result == 0 {
            failed(&std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    failed("this platform has no priority for a thread alone");
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
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

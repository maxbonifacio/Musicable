//! Real-time priority for worker threads.

/// Raises the calling thread's priority for audio work, as far as the
/// platform allows without special rights. The thread goes back to normal
/// when the guard drops.
pub(crate) fn raise() -> Raised {
    imp::raise()
}

pub(crate) use imp::Raised;

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[link(name = "avrt")]
    unsafe extern "system" {
        fn AvSetMmThreadCharacteristicsW(
            task_name: *const u16,
            task_index: *mut u32,
        ) -> *mut c_void;
        fn AvSetMmThreadPriority(avrt_handle: *mut c_void, priority: i32) -> i32;
        fn AvRevertMmThreadCharacteristics(avrt_handle: *mut c_void) -> i32;
    }

    /// `AVRT_PRIORITY_CRITICAL`.
    const CRITICAL: i32 = 2;

    /// Membership of the MMCSS "Pro Audio" task, which the Multimedia Class
    /// Scheduler runs ahead of ordinary threads. Null if joining failed.
    pub struct Raised(*mut c_void);

    pub fn raise() -> Raised {
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0;
        // SAFETY: `task` is a NUL-terminated UTF-16 string and `index` a
        // valid place for the task index.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
        if !handle.is_null() {
            // At the audio thread's priority within the task (crates/audio
            // sets the same): a block waits for its slowest thread. Without
            // it the worker still runs in the task, at normal priority.
            // SAFETY: the handle came from the call above.
            unsafe { AvSetMmThreadPriority(handle, CRITICAL) };
        }
        Raised(handle)
    }

    impl Drop for Raised {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the handle came from AvSetMmThreadCharacteristicsW
                // on this thread, which the guard never leaves.
                unsafe { AvRevertMmThreadCharacteristics(self.0) };
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// Nothing yet. Linux needs real-time scheduling rights, and macOS
    /// threads should join the audio device's workgroup, which needs the
    /// audio backend.
    pub struct Raised;

    pub fn raise() -> Raised {
        Raised
    }
}

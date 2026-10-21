//! Real-time priority for worker threads.

/// Raises the calling thread's priority for audio work, as far as the
/// platform allows without special rights. The thread goes back to normal
/// when the guard drops.
pub(crate) fn raise() -> Raised {
    imp::raise()
}

pub(crate) use imp::{Raised, cap};

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

    /// The most workers to start by default. Windows lets a process have 32
    /// threads in MMCSS at once and leaves any more at normal priority, where
    /// a block that shares work with one of them can wait on it. The audio
    /// thread takes one place, and audio drivers and plugins may take a few,
    /// so the workers leave them eight.
    pub const MAX_WORKERS: usize = 24;

    /// How many of `workers` to start by default.
    pub fn cap(workers: usize) -> usize {
        workers.min(MAX_WORKERS)
    }

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

    #[cfg(test)]
    mod tests {
        use std::io;
        use std::sync::mpsc;
        use std::thread;

        use super::{MAX_WORKERS, raise};

        /// Counts the threads Windows lets one process have in MMCSS, and
        /// says what it tells the first one it turns away. Run it on its own,
        /// since other tests' workers take places too:
        ///
        /// ```text
        /// cargo test -p musicable-engine --lib mmcss_places -- --ignored --nocapture
        /// ```
        #[test]
        #[ignore = "measures the system, so run it by hand"]
        #[allow(clippy::disallowed_macros)] // Printing the measurement, off the audio thread.
        fn mmcss_places() {
            const THREADS: usize = 64;
            let mut threads = Vec::new();
            let mut joined = 0;
            let mut refused = None;
            for n in 1..=THREADS {
                let (report, reported) = mpsc::channel();
                let (release, released) = mpsc::channel::<()>();
                threads.push((
                    release,
                    thread::spawn(move || {
                        let raised = raise();
                        let error = io::Error::last_os_error();
                        report.send((!raised.0.is_null(), error)).unwrap();
                        // Keep the place until every thread has asked.
                        let _ = released.recv();
                        drop(raised);
                    }),
                ));
                // One thread at a time, so they ask in order.
                let (ok, error) = reported.recv().unwrap();
                if ok {
                    joined += 1;
                } else if refused.is_none() {
                    refused = Some((n, error));
                }
            }
            for (release, thread) in threads {
                drop(release);
                thread.join().unwrap();
            }
            println!("{joined} of {THREADS} threads joined MMCSS.");
            match refused {
                Some((n, error)) => println!("Thread {n} was the first turned away: {error}."),
                None => println!("None was turned away."),
            }
            assert!(
                joined > MAX_WORKERS,
                "MAX_WORKERS workers and the audio thread don't fit"
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// How many of `workers` to start by default: all of them, since other
    /// platforms don't raise their priority yet.
    pub fn cap(workers: usize) -> usize {
        workers
    }

    /// Nothing yet. Linux needs real-time scheduling rights, and macOS
    /// threads should join the audio device's workgroup, which needs the
    /// audio backend.
    pub struct Raised;

    pub fn raise() -> Raised {
        Raised
    }
}

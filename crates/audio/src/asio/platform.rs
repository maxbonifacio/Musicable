//! What the host thread needs from the system: a COM apartment for the
//! driver, and a way for the callbacks to wake it. Elsewhere than on Windows
//! these are stand-ins, for the tests.

pub(super) use imp::{Apartment, Wake};

#[cfg(windows)]
mod imp {
    use std::marker::PhantomData;
    use std::time::Duration;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage,
    };

    use crate::Error;

    /// COM on this thread, in a single-threaded apartment as ASIO drivers
    /// expect, while the value lives.
    pub struct Apartment(PhantomData<*const ()>);

    impl Apartment {
        pub fn enter() -> Result<Apartment, Error> {
            // SAFETY: balanced by `CoUninitialize` on drop.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                .ok()
                .map_err(|e| super::super::registry::os("CoInitializeEx", &e))?;
            Ok(Apartment(PhantomData))
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: `enter` succeeded on this thread; the value isn't `Send`.
            unsafe { CoUninitialize() }
        }
    }

    /// Wakes the host thread: an auto-reset event.
    pub struct Wake(HANDLE);

    // SAFETY: event handles can be set and closed from any thread.
    unsafe impl Send for Wake {}
    unsafe impl Sync for Wake {}

    impl Wake {
        pub fn new() -> Result<Wake, Error> {
            // SAFETY: a plain unnamed event.
            let event = unsafe { CreateEventW(None, false, false, None) }
                .map_err(|e| super::super::registry::os("CreateEventW", &e))?;
            Ok(Wake(event))
        }

        /// Real-time safe.
        pub fn notify(&self) {
            // SAFETY: the handle is open until drop.
            let _ = unsafe { SetEvent(self.0) };
        }

        /// Waits up to `timeout` for a notification. Drivers in the thread's
        /// apartment can have window messages and calls from other threads
        /// waiting: they're handled meanwhile.
        pub fn wait(&self, timeout: Duration) {
            let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: the event stays open for the wait.
            unsafe {
                MsgWaitForMultipleObjectsEx(
                    Some(&[self.0]),
                    millis,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            let mut message = MSG::default();
            // SAFETY: handles the thread's own queue, a message at a time.
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                // SAFETY: a message just taken from the queue.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }

    impl Drop for Wake {
        fn drop(&mut self) {
            // SAFETY: the handle is ours and closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::{Condvar, Mutex, PoisonError};
    use std::time::Duration;

    use crate::Error;

    pub struct Apartment;

    impl Apartment {
        pub fn enter() -> Result<Apartment, Error> {
            Ok(Apartment)
        }
    }

    #[derive(Default)]
    pub struct Wake {
        woken: Mutex<bool>,
        condvar: Condvar,
    }

    impl Wake {
        pub fn new() -> Result<Wake, Error> {
            Ok(Wake::default())
        }

        pub fn notify(&self) {
            *self.woken.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.condvar.notify_one();
        }

        pub fn wait(&self, timeout: Duration) {
            let woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut woken, _) = self
                .condvar
                .wait_timeout_while(woken, timeout, |woken| !*woken)
                .unwrap_or_else(PoisonError::into_inner);
            *woken = false;
        }
    }
}

//! COM, Win32 handles and errors.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::thread;

use windows::Win32::Foundation::{CloseHandle, ERROR_NOT_FOUND, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_ENDPOINT_CREATE_FAILED,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_RESOURCES_INVALIDATED,
    AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    AVRT_PRIORITY_CRITICAL, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    AvSetMmThreadPriority, CreateEventW, SetEvent,
};
use windows::core::{HRESULT, w};

use crate::Error;
use crate::period::HNS_PER_SECOND;

/// COM on this thread, in the multithreaded apartment, while the value
/// lives. WASAPI objects made in the MTA work from any MTA thread.
pub(super) struct Com(PhantomData<*const ()>);

impl Com {
    pub fn init() -> Result<Com, Error> {
        // SAFETY: balanced by `CoUninitialize` on drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| os("CoInitializeEx", e))?;
        Ok(Com(PhantomData))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `init` succeeded on this thread; the value isn't `Send`.
        unsafe { CoUninitialize() }
    }
}

/// Runs `f` on a thread of its own in the MTA. The caller's thread may be a
/// UI thread in a single-threaded apartment, and is left as it is.
pub(super) fn in_mta<T: Send>(f: impl FnOnce() -> Result<T, Error> + Send) -> Result<T, Error> {
    thread::scope(|scope| {
        let thread = thread::Builder::new()
            .name("musicable-audio-query".into())
            .spawn_scoped(scope, || {
                let _com = Com::init()?;
                f()
            })
            .map_err(|e| Error::Os {
                call: "spawning a thread",
                message: e.to_string(),
            })?;
        thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Memory COM handed over, freed with `CoTaskMemFree`.
pub(super) struct TaskMem<T>(pub *mut T);

impl<T> Drop for TaskMem<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: COM allocated it, and nothing else frees it.
            unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) }
        }
    }
}

/// A Win32 event object.
pub(super) struct Event(pub HANDLE);

// SAFETY: event handles can be used and closed from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    /// Resets itself when a wait returns, as WASAPI's event callback needs.
    pub fn auto_reset() -> Result<Event, Error> {
        // SAFETY: a plain unnamed event.
        let handle =
            unsafe { CreateEventW(None, false, false, None) }.map_err(|e| os("CreateEventW", e))?;
        Ok(Event(handle))
    }

    /// Stays set once set.
    pub fn manual_reset() -> Result<Event, Error> {
        // SAFETY: a plain unnamed event.
        let handle =
            unsafe { CreateEventW(None, true, false, None) }.map_err(|e| os("CreateEventW", e))?;
        Ok(Event(handle))
    }

    pub fn set(&self) {
        // SAFETY: the handle is open until drop.
        let _ = unsafe { SetEvent(self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// This thread's MMCSS registration as "Pro Audio", while the value lives.
pub(super) struct Mmcss(HANDLE);

impl Mmcss {
    /// Registers the thread, or returns `None` if MMCSS refused. The thread
    /// still works then, at normal priority.
    pub fn join() -> Option<Mmcss> {
        let mut task = 0;
        // SAFETY: registers the calling thread; reverted on drop.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task) }.ok()?;
        // Ahead of the task's other threads. Without it the thread is still
        // in the task, at its normal priority.
        // SAFETY: the handle came from the call above.
        let _ = unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) };
        Some(Mmcss(handle))
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: on the thread that registered, which the value never leaves.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

/// The performance counter in 100 ns units: the time base and unit of the
/// QPC times `IAudioClock::GetPosition` reports.
pub(super) struct Qpc {
    frequency: u64,
}

impl Qpc {
    pub fn new() -> Qpc {
        let mut frequency = 0;
        // SAFETY: writes one i64. It can't fail on Windows XP or later.
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        Qpc {
            frequency: (frequency as u64).max(1),
        }
    }

    pub fn now(&self) -> u64 {
        let mut count = 0;
        // SAFETY: writes one i64. It can't fail on Windows XP or later.
        let _ = unsafe { QueryPerformanceCounter(&mut count) };
        let hns =
            u128::from(count as u64) * u128::from(HNS_PER_SECOND) / u128::from(self.frequency);
        hns as u64
    }
}

/// A failed call: what failed, and the error. Making the error message
/// allocates, so the audio thread passes these out of its real-time section
/// and turns them into [`Error`]s there.
pub(super) struct Failed {
    pub call: &'static str,
    pub error: windows::core::Error,
}

impl Failed {
    pub fn code(&self) -> HRESULT {
        self.error.code()
    }
}

impl From<Failed> for Error {
    fn from(failed: Failed) -> Error {
        os(failed.call, failed.error)
    }
}

pub(super) trait Call<T> {
    /// Names the call that failed.
    fn call(self, call: &'static str) -> Result<T, Failed>;
}

impl<T> Call<T> for windows::core::Result<T> {
    fn call(self, call: &'static str) -> Result<T, Failed> {
        self.map_err(|error| Failed { call, error })
    }
}

/// Whether an error means the device or the audio service went away.
pub(super) fn device_gone(code: HRESULT) -> bool {
    [
        AUDCLNT_E_DEVICE_INVALIDATED,
        AUDCLNT_E_RESOURCES_INVALIDATED,
        AUDCLNT_E_SERVICE_NOT_RUNNING,
        AUDCLNT_E_ENDPOINT_CREATE_FAILED,
    ]
    .contains(&code)
}

/// Our error for what WASAPI reported when `call` failed.
pub(super) fn os(call: &'static str, error: windows::core::Error) -> Error {
    let code = error.code();
    if device_gone(code) {
        Error::DeviceLost
    } else if code == AUDCLNT_E_DEVICE_IN_USE {
        Error::DeviceBusy
    } else if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
        Error::ExclusiveNotAllowed
    } else if code == AUDCLNT_E_UNSUPPORTED_FORMAT {
        Error::UnsupportedFormat
    } else if code == ERROR_NOT_FOUND.to_hresult() {
        Error::DeviceNotFound
    } else {
        Error::Os {
            call,
            message: format!("{} ({:#010X})", error.message(), code.0 as u32),
        }
    }
}

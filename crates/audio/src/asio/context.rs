//! The driver's callbacks, and what they reach of the open stream.
//!
//! ASIO's callbacks take no pointer to the host's state, so a stream
//! publishes its context in a static for as long as it has the driver's
//! buffers. That makes one ASIO stream at a time, which is all ASIO offers a
//! process anyway.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use musicable_engine::rt;

use super::platform::Wake;
use super::render::Render;
use super::sys::{self, Bool, Long, Time, message};
use crate::Error;
use crate::stats::Counters;

/// The published context, while a stream has the driver's buffers.
static CURRENT: AtomicPtr<Context> = AtomicPtr::new(ptr::null_mut());
/// Callbacks running now, which may still be using the context.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// The callbacks every stream hands the driver.
pub(super) static CALLBACKS: sys::Callbacks = sys::Callbacks {
    buffer_switch,
    sample_rate_changed,
    message: on_message,
    buffer_switch_time_info,
};

/// Requests for the host thread: bits of [`Shared::take_requests`].
pub(super) const STOP: u32 = 1;
/// The driver asked to be reset, or its sample rate changed.
pub(super) const RESET: u32 = 1 << 1;
pub(super) const LATENCIES: u32 = 1 << 2;
/// Show the driver's own settings.
pub(super) const SETTINGS: u32 = 1 << 3;

/// What the control thread, the host thread and the callbacks share.
pub(super) struct Shared {
    pub counters: Counters,
    requests: AtomicU32,
    /// Overloads and resync requests the driver reported, not yet counted.
    reported: AtomicU32,
    /// The stream's sample rate, to tell a real change of rate from a
    /// repeat.
    rate: AtomicU32,
    /// The output latency the driver reports, in frames.
    latency: AtomicU32,
    /// The input latency the driver reports, in frames: how old the first
    /// frame of an input half is at its buffer switch. A buffer at least.
    input_latency: AtomicU32,
    wake: Wake,
}

impl Shared {
    pub fn new() -> Result<Shared, Error> {
        Ok(Shared {
            counters: Counters::default(),
            requests: AtomicU32::new(0),
            reported: AtomicU32::new(0),
            rate: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            input_latency: AtomicU32::new(0),
            wake: Wake::new()?,
        })
    }

    /// Asks the host thread for something. Real-time safe.
    pub fn request(&self, requests: u32) {
        self.requests.fetch_or(requests, Ordering::AcqRel);
        self.wake.notify();
    }

    pub fn take_requests(&self) -> u32 {
        self.requests.swap(0, Ordering::AcqRel)
    }

    /// Takes the dropouts the driver reported. Real-time safe.
    pub fn take_reported(&self) -> u32 {
        self.reported.swap(0, Ordering::Relaxed)
    }

    pub fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    /// The output latency the driver reports, in frames, for the callbacks
    /// and the stats.
    pub fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
        self.counters.queued(u64::from(frames));
    }

    /// Real-time safe.
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// The input latency the driver reports, in frames, for the callbacks.
    pub fn set_input_latency(&self, frames: u32) {
        self.input_latency.store(frames, Ordering::Relaxed);
    }

    /// Real-time safe.
    pub fn input_latency(&self) -> u32 {
        self.input_latency.load(Ordering::Relaxed)
    }

    /// Waits up to `timeout` for a request.
    pub fn wait(&self, timeout: Duration) {
        self.wake.wait(timeout);
    }
}

/// A stream, as the callbacks reach it.
pub(super) struct Context {
    pub shared: Arc<Shared>,
    /// The callbacks' half of the stream, once the driver has created the
    /// buffers.
    pub render: TryLock<Option<Render>>,
}

/// A context published to the callbacks, until dropped.
pub(super) struct Published(NonNull<Context>);

impl Published {
    pub fn new(context: Context) -> Result<Published, Error> {
        let context = NonNull::from(Box::leak(Box::new(context)));
        let published = CURRENT.compare_exchange(
            ptr::null_mut(),
            context.as_ptr(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        if published.is_err() {
            // SAFETY: just leaked, and never published.
            drop(unsafe { Box::from_raw(context.as_ptr()) });
            return Err(Error::Os {
                call: "opening an ASIO driver",
                message: "another ASIO stream is open, and there can only be one".into(),
            });
        }
        Ok(Published(context))
    }
}

impl Deref for Published {
    type Target = Context;

    fn deref(&self) -> &Context {
        // SAFETY: ours until drop, and only ever shared.
        unsafe { self.0.as_ref() }
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        CURRENT.store(ptr::null_mut(), Ordering::SeqCst);
        // A callback that found the context before it went is done with it
        // once it's no longer running.
        while RUNNING.load(Ordering::SeqCst) > 0 {
            thread::yield_now();
        }
        // SAFETY: leaked in `new`, and no callback can reach it now.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// Runs `f` with the published context, if there is one.
fn with_context(f: impl FnOnce(&Context)) {
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        }
    }
    RUNNING.fetch_add(1, Ordering::SeqCst);
    let _running = Running;
    let context = CURRENT.load(Ordering::SeqCst);
    // SAFETY: a published context lives until it's unpublished and no
    // callback that could have found it still runs (`Published::drop`).
    if let Some(context) = unsafe { context.as_ref() } {
        f(context);
    }
}

/// Renders the half `index`. A callback never waits: one that comes while
/// another runs, from a driver that calls again before the last call
/// returned, finds the lock taken and leaves the half as it is.
fn serve(index: Long, position: Option<u64>) {
    with_context(|context| {
        if let Some(mut render) = context.render.try_lock()
            && let Some(render) = render.as_mut()
        {
            rt::scope(|| render.serve(&context.shared, index, position));
        }
    });
}

unsafe extern "C" fn buffer_switch(index: Long, _direct: Bool) {
    // The driver knows where the half starts if it's asked.
    serve(index, None);
}

unsafe extern "C" fn buffer_switch_time_info(
    time: *mut Time,
    index: Long,
    _direct: Bool,
) -> *mut Time {
    let position = if time.is_null() {
        None
    } else {
        // SAFETY: the driver's time info for this call, packed as drivers
        // pack it.
        let info = unsafe { time.read_unaligned() }.info;
        (info.flags & sys::SAMPLE_POSITION_VALID != 0).then(|| info.sample_position.get())
    };
    serve(index, position);
    // No time code to hand back.
    ptr::null_mut()
}

unsafe extern "C" fn sample_rate_changed(rate: f64) {
    with_context(|context| {
        let shared = &context.shared;
        if rate.round() as u32 != shared.rate.load(Ordering::Relaxed) {
            // The engine needs building again for the new rate.
            shared.request(RESET);
        }
    });
}

unsafe extern "C" fn on_message(
    selector: Long,
    value: Long,
    _message: *mut c_void,
    _opt: *mut f64,
) -> Long {
    match selector {
        message::SELECTOR_SUPPORTED => Long::from(matches!(
            value,
            message::ENGINE_VERSION
                | message::RESET_REQUEST
                | message::BUFFER_SIZE_CHANGE
                | message::RESYNC_REQUEST
                | message::LATENCIES_CHANGED
                | message::SUPPORTS_TIME_INFO
                | message::OVERLOAD
        )),
        message::ENGINE_VERSION => 2,
        message::SUPPORTS_TIME_INFO => 1,
        // The host thread closes the driver, once this has returned to it.
        message::RESET_REQUEST | message::BUFFER_SIZE_CHANGE => {
            with_context(|context| context.shared.request(RESET));
            1
        }
        message::LATENCIES_CHANGED => {
            with_context(|context| context.shared.request(LATENCIES));
            1
        }
        message::RESYNC_REQUEST | message::OVERLOAD => {
            with_context(|context| {
                context.shared.reported.fetch_add(1, Ordering::Relaxed);
            });
            1
        }
        _ => 0,
    }
}

/// A lock the callbacks only ever try, so they never wait for the host
/// thread. The host thread takes it when no callback can hold it for long:
/// before the stream starts, and after it stops.
pub(super) struct TryLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the value is only reached through a guard, and one thread at a
// time has the guard.
unsafe impl<T: Send> Sync for TryLock<T> {}

impl<T> TryLock<T> {
    pub fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn try_lock(&self) -> Option<Locked<'_, T>> {
        (!self.locked.swap(true, Ordering::Acquire)).then_some(Locked(self))
    }

    /// Takes the lock, waiting for a callback that has it to finish.
    pub fn lock(&self) -> Locked<'_, T> {
        loop {
            if let Some(locked) = self.try_lock() {
                return locked;
            }
            thread::yield_now();
        }
    }
}

pub(super) struct Locked<'a, T>(&'a TryLock<T>);

impl<T> Deref for Locked<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard is the only way in, and there's one at a time.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> DerefMut for Locked<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as for `deref`.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for Locked<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

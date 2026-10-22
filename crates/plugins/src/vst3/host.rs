//! What a VST3 instance shares with its nodes and with the objects the
//! plugin calls back.

use std::cell::UnsafeCell;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering,
};

use vst3::Steinberg::Vst::IAudioProcessor;

use super::node::Audio;
use crate::threads::{Busy, BusyFlag, INACTIVE, is_audio_thread};
use crate::{Error, Waker};

/// What a plugin instance shares with its nodes and with the plugin's
/// calls back to the host, for as long as any of them can use it.
pub(crate) struct Shared {
    /// The plugin's audio processor, which the instance holds, or null once
    /// it's let go of.
    processor: AtomicPtr<IAudioProcessor>,
    /// [`INACTIVE`], [`ACTIVE`](crate::threads::ACTIVE) and so on.
    lifecycle: AtomicU8,
    /// The plugin's latency, in frames, as it said when it last activated.
    latency: AtomicUsize,
    /// Whoever holds it has `audio`.
    busy: BusyFlag,
    pub(crate) requests: Requests,
    pub(crate) audio: UnsafeCell<Audio>,
    /// What the plugin calls itself, for errors.
    pub(crate) name: String,
    pub(crate) waker: Option<Waker>,
}

// SAFETY: `audio` is only touched by whoever holds `busy`, and the
// processor by the threads VST3's rules allow. The rest is atomic or locked.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

/// What a plugin asked of the host, from whatever thread, for the main
/// thread to act on.
#[derive(Debug)]
pub(crate) struct Requests {
    /// Every `RestartFlags` the plugin asked to restart with since the last
    /// poll.
    pub(crate) restart: AtomicI32,
    /// The plugin said its state changed.
    pub(crate) dirty: AtomicBool,
    /// Edits made in the plugin's window, oldest first.
    pub(crate) edits: Mutex<Vec<Edit>>,
    /// The size the window asks for, as `width << 32 | height`, or
    /// [`NO_RESIZE`].
    pub(crate) window_resize: AtomicU64,
}

pub(crate) const NO_RESIZE: u64 = u64::MAX;

/// What someone did in a plugin's window, as its controller says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Edit {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

impl Shared {
    pub(crate) fn new(name: &str, audio: Audio, waker: Option<Waker>) -> Self {
        Self {
            processor: AtomicPtr::default(),
            lifecycle: AtomicU8::new(INACTIVE),
            latency: AtomicUsize::new(0),
            busy: BusyFlag::default(),
            requests: Requests {
                restart: AtomicI32::new(0),
                dirty: AtomicBool::new(false),
                edits: Mutex::new(Vec::new()),
                window_resize: AtomicU64::new(NO_RESIZE),
            },
            audio: UnsafeCell::new(audio),
            name: name.to_owned(),
            waker,
        }
    }

    pub(crate) fn processor(&self) -> *mut IAudioProcessor {
        self.processor.load(Ordering::Acquire)
    }

    pub(crate) fn set_processor(&self, processor: *mut IAudioProcessor) {
        self.processor.store(processor, Ordering::Release);
    }

    pub(crate) fn lifecycle(&self) -> u8 {
        self.lifecycle.load(Ordering::Acquire)
    }

    pub(crate) fn set_lifecycle(&self, lifecycle: u8) {
        self.lifecycle.store(lifecycle, Ordering::Release);
    }

    pub(crate) fn latency(&self) -> usize {
        self.latency.load(Ordering::Acquire)
    }

    pub(crate) fn set_latency(&self, latency: usize) {
        self.latency.store(latency, Ordering::Release);
    }

    /// Takes `busy` if no one has it, as a node does to process.
    pub(crate) fn try_enter(&self) -> Option<Busy<'_>> {
        self.busy.try_enter()
    }

    /// Whether a node is processing, or the main thread has `busy`.
    pub(crate) fn is_busy(&self) -> bool {
        self.busy.is_held()
    }

    /// Takes `busy` on the main thread, waiting for a node that's
    /// processing to finish its block.
    pub(crate) fn enter(&self) -> Result<Busy<'_>, Error> {
        self.busy.enter(&self.name)
    }

    /// Asks the main thread for a window `width` by `height`.
    pub(crate) fn request_resize(&self, width: u32, height: u32) {
        let size = u64::from(width) << 32 | u64::from(height);
        self.requests.window_resize.store(size, Ordering::Release);
        self.wake();
    }

    /// Tells the app the plugin needs the main thread.
    pub(crate) fn wake(&self) {
        // Waking runs the app's code, which isn't real-time safe. The app
        // polls often anyway.
        if !is_audio_thread()
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }
}

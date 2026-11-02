//! What a VST3 instance shares with its nodes and with the objects the
//! plugin calls back.

use std::cell::UnsafeCell;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering,
};

use vst3::Steinberg::Vst::IAudioProcessor;

use super::node::Audio;
use crate::threads::{Busy, BusyFlag, INACTIVE, is_audio_thread, thread_mark};
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
    /// Whether a node renders the plugin: from the first block a node plays
    /// until the node stops, as its device is bypassed.
    pub(crate) running: AtomicBool,
    /// Whoever holds it has `audio`.
    busy: BusyFlag,
    pub(crate) requests: Requests,
    pub(crate) audio: UnsafeCell<Audio>,
    /// What the plugin calls itself, for errors.
    pub(crate) name: String,
    pub(crate) waker: Option<Waker>,
    /// The plugin's UI thread, which made the instance, as [`thread_mark`]
    /// gives it.
    main_thread: usize,
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
    /// Edits made in the plugin's window, oldest first. It has room for
    /// [`EDITS`] without allocating.
    pub(crate) edits: Mutex<Vec<Edit>>,
    /// The size the window asks for, as `width << 32 | height`, or
    /// [`NO_RESIZE`].
    pub(crate) window_resize: AtomicU64,
    /// The plugin asked for that size on another thread than the UI
    /// thread, so its view takes it as the main thread polls.
    pub(crate) view_resize: AtomicBool,
}

pub(crate) const NO_RESIZE: u64 = u64::MAX;

/// How many edits [`Requests::edits`] holds between polls without
/// allocating: all a plugin that edits on an audio thread, as VST3 forbids,
/// gets in.
pub(crate) const EDITS: usize = 1024;

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
            running: AtomicBool::new(false),
            busy: BusyFlag::default(),
            requests: Requests {
                restart: AtomicI32::new(0),
                dirty: AtomicBool::new(false),
                edits: Mutex::new(Vec::with_capacity(EDITS)),
                window_resize: AtomicU64::new(NO_RESIZE),
                view_resize: AtomicBool::new(false),
            },
            audio: UnsafeCell::new(audio),
            name: name.to_owned(),
            waker,
            main_thread: thread_mark(),
        }
    }

    /// Whether this is the thread that made the instance, the plugin's UI
    /// thread.
    pub(crate) fn on_main_thread(&self) -> bool {
        thread_mark() == self.main_thread
    }

    /// Takes an edit made in the plugin's window. VST3 has the controller
    /// edit on the UI thread, but some edit as they process: on an audio
    /// thread, an edit neither waits for the lock nor allocates, and one
    /// there's no room for is dropped.
    pub(crate) fn edit(&self, edit: Edit) {
        let edits = &self.requests.edits;
        if is_audio_thread() {
            if let Ok(mut edits) = edits.try_lock()
                && edits.len() < edits.capacity()
            {
                edits.push(edit);
            }
        } else {
            edits.lock().unwrap_or_else(|e| e.into_inner()).push(edit);
        }
        self.wake();
    }

    /// The edits made in the plugin's window since this was last asked,
    /// oldest first.
    pub(crate) fn take_edits(&self) -> Vec<Edit> {
        let mut edits = self
            .requests
            .edits
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        edits.drain(..).collect()
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

#[cfg(test)]
mod tests {
    use musicable_engine::rt;

    use super::*;
    use crate::threads::AudioThread;

    #[test]
    fn edits_on_an_audio_thread_neither_wait_nor_allocate() {
        let (to_audio, from_main) = rtrb::RingBuffer::new(1);
        let (to_main, _from_audio) = rtrb::RingBuffer::new(1);
        let shared = Shared::new("Test", Audio::new(from_main, to_main), None);
        let _keep = to_audio;
        rt::scope(|| {
            let _audio_thread = AudioThread::enter();
            // Past the room it has, edits are dropped.
            for _ in 0..EDITS + 10 {
                shared.edit(Edit::Begin(1));
            }
        });
        assert_eq!(shared.take_edits().len(), EDITS);
        // With the lock held elsewhere, an edit is dropped, not waited for.
        let held = shared.requests.edits.lock().unwrap();
        rt::scope(|| {
            let _audio_thread = AudioThread::enter();
            shared.edit(Edit::End(1));
        });
        drop(held);
        assert!(shared.take_edits().is_empty());
        // Elsewhere, edits wait for the lock.
        shared.edit(Edit::End(1));
        assert_eq!(shared.take_edits(), [Edit::End(1)]);
    }
}

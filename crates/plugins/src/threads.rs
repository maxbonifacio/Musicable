//! Which thread a plugin is on, and taking turns with it: what hosting CLAP
//! and VST3 plugins share.

use std::cell::Cell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Error;

/// Created and initialized, but not active.
pub(crate) const INACTIVE: u8 = 0;
/// Active, and not yet processing: the next node to process it starts it.
pub(crate) const ACTIVE: u8 = 1;
pub(crate) const PROCESSING: u8 = 2;
/// Active, but it failed to start processing: its nodes play silence.
pub(crate) const FAILED: u8 = 3;
/// Destroyed: nothing calls it any more.
pub(crate) const DESTROYED: u8 = 4;

/// Set while a node processes a plugin, or while the main thread changes
/// how it's set up. Whoever sets it has what processing the plugin needs.
#[derive(Debug, Default)]
pub(crate) struct BusyFlag(AtomicBool);

impl BusyFlag {
    /// Takes the flag if no one has it, as a node does to process.
    pub(crate) fn try_enter(&self) -> Option<Busy<'_>> {
        self.0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Busy(&self.0))
    }

    /// Whether someone has the flag.
    pub(crate) fn is_held(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Takes the flag on the main thread, waiting for a node that's
    /// processing to finish its block. Nodes that try to process meanwhile
    /// play silence. `name` is the plugin's, for saying which one a node
    /// is stuck in.
    pub(crate) fn enter(&self, name: &str) -> Result<Busy<'_>, Error> {
        let start = Instant::now();
        loop {
            if let Some(busy) = self.try_enter() {
                return Ok(busy);
            }
            if start.elapsed() > Duration::from_secs(5) {
                return Err(Error::new(format!(
                    "{name} has been processing one block for over five seconds"
                )));
            }
            std::thread::yield_now();
        }
    }
}

/// [`BusyFlag`], held: released when dropped.
pub(crate) struct Busy<'a>(&'a AtomicBool);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

thread_local! {
    /// Only its address matters: each thread's is its own.
    static MARK: u8 = const { 0 };
    /// How many plugin calls the thread is inside that count as on an audio
    /// thread: processing, and CLAP's thread pool tasks.
    static AUDIO: Cell<u32> = const { Cell::new(0) };
}

/// Tells this thread from every other thread alive. Unlike
/// `thread::current().id()`, it never allocates, so it's safe on any thread.
pub(crate) fn thread_mark() -> usize {
    MARK.with(|mark| ptr::from_ref(mark) as usize)
}

/// Whether this thread is in a plugin call that counts as on an audio
/// thread.
pub(crate) fn is_audio_thread() -> bool {
    AUDIO.with(Cell::get) > 0
}

/// Marks this thread as an audio thread until dropped.
pub(crate) struct AudioThread(());

impl AudioThread {
    pub(crate) fn enter() -> Self {
        AUDIO.with(|depth| depth.set(depth.get() + 1));
        Self(())
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        AUDIO.with(|depth| depth.set(depth.get() - 1));
    }
}

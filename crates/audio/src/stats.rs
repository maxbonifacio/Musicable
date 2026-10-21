//! Stream counters: written by the audio thread, read by the control thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

/// A running stream's counters, from its start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Times the device asked for audio.
    pub wakeups: u64,
    /// Frames written to the device.
    pub frames: u64,
    /// Dropouts: times the device ran out of audio. Each one is also counted
    /// in the engine's `Stats::xruns`.
    pub underruns: u64,
    /// The audio the dropouts lost, in frames, as the device clock measures
    /// it.
    pub lost_frames: u64,
    /// Wake-ups more than one and a half periods after the one before. They
    /// eat into the buffered audio, but aren't dropouts unless it runs out.
    pub late_wakeups: u64,
    /// The longest time between two wake-ups.
    pub max_interval: Duration,
    /// The longest a wake-up took, rendering included.
    pub max_busy: Duration,
    /// Frames queued ahead of the device's play position at the last
    /// wake-up, before it wrote: the output latency as the device's clock
    /// sees it.
    pub queued: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    wakeups: AtomicU64,
    frames: AtomicU64,
    underruns: AtomicU64,
    lost_frames: AtomicU64,
    late_wakeups: AtomicU64,
    max_interval_ns: AtomicU64,
    max_busy_ns: AtomicU64,
    queued: AtomicU64,
    ended: AtomicBool,
}

impl Counters {
    pub fn wakeup(&self) {
        self.wakeups.fetch_add(1, Relaxed);
    }

    pub fn wrote(&self, frames: u32) {
        self.frames.fetch_add(u64::from(frames), Relaxed);
    }

    pub fn underrun(&self) {
        self.underruns.fetch_add(1, Relaxed);
    }

    // cpal doesn't say how much audio an xrun lost.
    #[cfg(windows)]
    pub fn lost(&self, frames: u64) {
        if frames > 0 {
            self.lost_frames.fetch_add(frames, Relaxed);
        }
    }

    pub fn late_wakeup(&self) {
        self.late_wakeups.fetch_add(1, Relaxed);
    }

    pub fn interval(&self, interval: Duration) {
        self.max_interval_ns.fetch_max(nanos(interval), Relaxed);
    }

    pub fn busy(&self, busy: Duration) {
        self.max_busy_ns.fetch_max(nanos(busy), Relaxed);
    }

    pub fn queued(&self, frames: u64) {
        self.queued.store(frames, Relaxed);
    }

    pub fn end(&self) {
        self.ended.store(true, Relaxed);
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Relaxed)
    }

    pub fn snapshot(&self) -> StreamStats {
        StreamStats {
            wakeups: self.wakeups.load(Relaxed),
            frames: self.frames.load(Relaxed),
            underruns: self.underruns.load(Relaxed),
            lost_frames: self.lost_frames.load(Relaxed),
            late_wakeups: self.late_wakeups.load(Relaxed),
            max_interval: Duration::from_nanos(self.max_interval_ns.load(Relaxed)),
            max_busy: Duration::from_nanos(self.max_busy_ns.load(Relaxed)),
            queued: self.queued.load(Relaxed),
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

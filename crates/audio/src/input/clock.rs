//! When the output plays each engine frame: published by the stream's audio
//! thread at every wake-up, for the threads that line inputs up with it.

use std::hint;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicU64, fence};

/// Nanoseconds in a second.
pub(crate) const NANOS: f64 = 1e9;

/// The output's latest reading of its device clock: engine frame `frame`
/// was heard at `time`, in nanoseconds on the backend's clock (the
/// performance counter on Windows). One thread publishes, any reads.
#[derive(Debug, Default)]
pub(crate) struct Playback {
    /// Odd while a reading is being written.
    seq: AtomicU64,
    frame: AtomicU64,
    time: AtomicU64,
    /// The output's sample rate, 0 until the first reading.
    rate: AtomicU32,
}

/// One reading: engine frame `frame` was heard at `time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reading {
    pub frame: u64,
    pub time: u64,
    pub rate: u32,
}

impl Reading {
    /// The engine frame heard at `time`, from this reading: fractional,
    /// and before the reading's for an earlier time. Assumes the output
    /// runs at exactly its rate, which over the time between readings is
    /// close enough.
    pub fn frame_at(&self, time: u64) -> f64 {
        let since = time as f64 - self.time as f64;
        self.frame as f64 + since * f64::from(self.rate) / NANOS
    }
}

impl Playback {
    /// Publishes a reading. Real-time safe, and for one thread only.
    pub fn publish(&self, frame: u64, time: u64, rate: u32) {
        let seq = self.seq.load(Relaxed);
        self.seq.store(seq.wrapping_add(1), Relaxed);
        fence(Release);
        self.frame.store(frame, Relaxed);
        self.time.store(time, Relaxed);
        self.rate.store(rate, Relaxed);
        self.seq.store(seq.wrapping_add(2), Release);
    }

    /// The latest reading, or `None` before the first. Real-time safe: it
    /// retries a few times if a reading is being published, and gives up
    /// after that.
    pub fn reading(&self) -> Option<Reading> {
        for _ in 0..64 {
            let before = self.seq.load(Acquire);
            if before % 2 == 1 {
                hint::spin_loop();
                continue;
            }
            let reading = Reading {
                frame: self.frame.load(Relaxed),
                time: self.time.load(Relaxed),
                rate: self.rate.load(Relaxed),
            };
            fence(Acquire);
            if self.seq.load(Relaxed) == before {
                return (reading.rate > 0).then_some(reading);
            }
        }
        None
    }
}

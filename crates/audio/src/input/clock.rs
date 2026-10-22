//! When the output plays each engine frame: published by the stream's audio
//! thread at every wake-up, for the threads that line inputs up with it.

use std::hint;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicU64, fence};

/// Nanoseconds in a second.
pub(crate) const NANOS: f64 = 1e9;
/// Readings a [`Smoother`] keeps: a few seconds of shared-mode wake-ups.
const KEPT: usize = 256;
/// Readings a [`Smoother`] keeps as they come before it holds any back: by
/// then the highest is close to the truth even when readings come in steps.
const WARM: usize = 32;
/// How far a reading can be above all those kept, or below them all, before
/// it counts as the clock moving rather than as noise, in frames.
const GUARD: f64 = 64.0;
/// Readings in a row that must say the clock moved before it's believed.
const CONFIRM: u32 = 3;

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

    /// Publishes a reading of a fractional frame, as the whole frame before
    /// it and when that was heard. Real-time safe, and for one thread only.
    pub fn publish_fractional(&self, frame: f64, time: u64, rate: u32) {
        let frame = frame.max(0.0);
        let whole = frame.floor();
        let earlier = ((frame - whole) * NANOS / f64::from(rate)).round() as u64;
        self.publish(whole as u64, time.saturating_sub(earlier), rate);
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

/// Smooths an output's readings of its device clock. Some drivers move the
/// position they report only in steps, as the RME HDSPe's does in shared
/// mode, 256 frames at a time, so a reading lags the frame being heard by
/// anything up to a step, and the offsets that line inputs up with it
/// wander by tens of frames. Less the steady advance at the output's rate,
/// the highest of the recent readings lags least, so that's the one
/// followed, once enough readings have come that it lags little: a few
/// seconds' worth for readings in steps, and a few hundred milliseconds'
/// otherwise. Readings that keep time come through within a frame.
/// Real-time safe.
///
/// On a device clock slow against the performance counter, the highest is
/// the oldest, so the smoothed position runs ahead by the drift over the
/// readings kept: a frame for 10 ppm over 2.56 s at 44.1 kHz, and the same
/// every time.
#[derive(Debug)]
pub(crate) struct Smoother {
    /// Frames per nanosecond, at the output's rate.
    rate: f64,
    /// The first reading: frames played, and when.
    origin: Option<(u64, u64)>,
    /// The readings kept, less the advance since the first.
    kept: [f64; KEPT],
    len: usize,
    next: usize,
    /// Readings in a row above all those kept, and below them all.
    above: u32,
    below: u32,
    /// Whether enough readings have come.
    settled: bool,
}

impl Smoother {
    /// A smoother for an output at `rate` Hz.
    pub fn new(rate: u32) -> Smoother {
        Smoother {
            rate: f64::from(rate) / NANOS,
            origin: None,
            kept: [0.0; KEPT],
            len: 0,
            next: 0,
            above: 0,
            below: 0,
            settled: false,
        }
    }

    /// Takes a reading, that the device had played `played` frames at
    /// `time`, in nanoseconds, and returns how many it had played then,
    /// smoothed: fractional. `None` until enough readings have come.
    pub fn smooth(&mut self, played: u64, time: u64) -> Option<f64> {
        let (first, start) = *self.origin.get_or_insert((played, time));
        let advance = (time as f64 - start as f64) * self.rate;
        let reading = played as f64 - first as f64 - advance;
        let (top, bottom) = self.range();
        if self.len < WARM {
            self.above = 0;
            self.below = 0;
            self.keep(reading);
        } else if reading > top + GUARD {
            self.above += 1;
            self.below = 0;
            if self.above >= CONFIRM {
                self.restart(reading);
            }
        } else if reading < bottom - GUARD {
            self.below += 1;
            self.above = 0;
            if self.below >= CONFIRM {
                self.restart(reading);
            }
        } else {
            self.above = 0;
            self.below = 0;
            self.keep(reading);
        }
        let (top, bottom) = self.range();
        // The highest of n readings that lag by anything up to their spread
        // lags by about the spread over n.
        self.settled |= self.len >= WARM && (top - bottom <= self.len as f64 || self.len == KEPT);
        self.settled.then_some(first as f64 + advance + top)
    }

    fn keep(&mut self, reading: f64) {
        self.kept[self.next] = reading;
        self.next = (self.next + 1) % KEPT;
        self.len = (self.len + 1).min(KEPT);
    }

    /// Starts again from `reading`, after the clock moved.
    fn restart(&mut self, reading: f64) {
        self.len = 0;
        self.next = 0;
        self.above = 0;
        self.below = 0;
        self.keep(reading);
    }

    /// The highest and the lowest of the readings kept.
    fn range(&self) -> (f64, f64) {
        self.kept[..self.len]
            .iter()
            .fold((f64::NEG_INFINITY, f64::INFINITY), |(top, bottom), &r| {
                (top.max(r), bottom.min(r))
            })
    }
}

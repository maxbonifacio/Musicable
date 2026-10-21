//! Dropout detection from the device's clock.
//!
//! A dropout is the device running out of audio. At each wake-up, before
//! writing, the backend reads the stream's position from the device clock
//! (`IAudioClock`, with the QPC time of the reading) and looks for one of two
//! signs:
//!
//! - **The device clock fell behind the wall clock.** Between two wake-ups
//!   the position should advance as much as QPC time does. While the audio
//!   engine has nothing of ours to play it stops counting our position, and
//!   so do some exclusive-mode drivers. As in Chromium's WASAPI output, a gap
//!   of more than half a period counts. In shared mode it counts only if the
//!   buffer had run low (less than a period queued) since the wake-up before
//!   last: at a wake-up, or as a write landed, which is where a render that
//!   took too long shows. With audio still queued the device can't have run
//!   out, and the gap was jitter in the clock readings.
//! - **The device played past what was written.** Most exclusive-mode
//!   drivers keep counting while the device replays whatever its buffer held,
//!   so their clock never falls behind. Their position overtakes the frames
//!   written instead.
//!
//! A dropout takes a few wake-ups to recover from while the buffer refills,
//! and the clock readings can lag by a wake-up. Signs in the
//! [`RECOVERY_WAKEUPS`] after the first count towards the same dropout.

use crate::period::{frames_to_hns, hns_to_frames};

/// How long a dropout's recovery can last, in wake-ups.
pub(crate) const RECOVERY_WAKEUPS: u32 = 10;

/// What the backend saw at one wake-up, before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reading {
    /// Frames the device has played, from its clock.
    pub played: u64,
    /// When `played` was read, in 100 ns QPC units.
    pub qpc: u64,
    /// Frames written to the device before this wake-up.
    pub written: u64,
    /// Shared mode: frames still queued in the buffer.
    pub padding: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// A new dropout began.
    pub underrun: bool,
    /// Frames lost, new dropout or not.
    pub lost: u64,
}

#[derive(Debug)]
pub(crate) struct Detector {
    exclusive: bool,
    /// Frames per wake-up: the engine period in shared mode, each buffer half
    /// in exclusive mode.
    period: u64,
    rate: u32,
    last: Option<Reading>,
    /// Shared mode: readings left in which a low buffer backs up a gap.
    low: u32,
    /// Wake-ups left in the current dropout's recovery: 0 when there is none.
    recovering: u32,
    /// Exclusive mode: where the device position stands against the frames
    /// written at a punctual wake-up, minus one period. Set at the first
    /// reading, then corrected: drivers count from different points of their
    /// pipeline, and a dropout can leave the stream a buffer behind for good.
    offset: Option<i64>,
    /// Exclusive mode, during a recovery: the lowest and highest `overrun`
    /// seen.
    floor: i64,
    peak: i64,
}

impl Detector {
    pub fn new(exclusive: bool, period: u32, rate: u32) -> Self {
        Self {
            exclusive,
            period: u64::from(period.max(1)),
            rate,
            last: None,
            low: 0,
            recovering: 0,
            offset: None,
            floor: 0,
            peak: 0,
        }
    }

    /// Forgets the last reading, so the next one starts the comparison
    /// afresh. For when a reading failed.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Shared mode: a write landed with `queued` frames still queued ahead
    /// of it. Real-time safe.
    pub fn landed(&mut self, queued: u64) {
        if queued < self.period {
            // The next two readings.
            self.low = 2;
        }
    }

    /// Looks for a dropout since the last reading. Real-time safe.
    pub fn observe(&mut self, now: Reading) -> Outcome {
        // The clock can stand at zero for a moment after the stream starts,
        // and nothing can be compared until it moves.
        if now.played == 0 {
            self.last = None;
            return Outcome::default();
        }
        let period = self.period as i64;
        let Some(last) = self.last.replace(now) else {
            if self.exclusive && self.offset.is_none() {
                self.offset = Some(now.played as i64 - now.written as i64 + period);
            }
            return Outcome::default();
        };
        if now.played < last.played || now.qpc < last.qpc {
            // A driver bug or a reset clock: start again from here.
            return Outcome::default();
        }

        // How much longer the wall clock ran than the device clock, in frames.
        let wall = now.qpc - last.qpc;
        let device = frames_to_hns(now.played - last.played, self.rate);
        let gap = if wall > device {
            hns_to_frames(wall - device, self.rate) as i64
        } else {
            -(hns_to_frames(device - wall, self.rate) as i64)
        };
        let behind = gap * 2 > period;

        let mut overrun = i64::MIN;
        let mut overtook = false;
        if self.exclusive {
            let offset = self
                .offset
                .get_or_insert(now.played as i64 - now.written as i64 + period);
            // Frames played beyond those written: around minus a period when
            // punctual, positive once the device has replayed old audio.
            overrun = now.played as i64 - now.written as i64 - *offset;
            let slack = period / 8;
            overtook = overrun > slack;
            if overrun < -(period + slack) {
                // The offset was set from a late wake-up: this one is earlier.
                *offset += overrun + period;
                overrun = -period;
            }
        } else if now.padding < self.period {
            // This reading and the next.
            self.low = 2;
        }

        let outcome = if self.recovering > 0 {
            self.recovering -= 1;
            let mut lost = gap.max(0) as u64;
            if self.exclusive {
                lost = lost.max((overrun - self.peak).max(0) as u64);
                self.peak = self.peak.max(overrun);
                self.floor = self.floor.min(overrun);
                if self.recovering == 0 {
                    self.rebase();
                }
            }
            Outcome {
                underrun: false,
                lost,
            }
        } else if (behind && (self.exclusive || self.low > 0)) || overtook {
            self.recovering = RECOVERY_WAKEUPS;
            self.low = 0;
            self.floor = i64::MAX;
            self.peak = overrun;
            Outcome {
                underrun: true,
                lost: gap.max(overrun).max(0) as u64,
            }
        } else {
            Outcome::default()
        };
        self.low = self.low.saturating_sub(1);
        outcome
    }

    /// At the end of a recovery in exclusive mode: a device that skipped
    /// ahead for good plays past what was written even at punctual wake-ups,
    /// so take where it stands as the new normal.
    fn rebase(&mut self) {
        let period = self.period as i64;
        if let Some(offset) = &mut self.offset {
            if self.floor > -period + period / 8 {
                *offset += self.floor + period;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const PERIOD: u64 = 128;

    fn qpc(frames: u64) -> u64 {
        // An arbitrary QPC origin.
        1_000_000_000 + frames_to_hns(frames, RATE)
    }

    /// A shared-mode stream: the audio engine takes a period from the buffer
    /// at each multiple of the period, and plays what it took in real time.
    /// The stream tops the buffer up to two periods at each wake-up.
    struct Shared {
        detector: Detector,
        /// Engine passes done.
        passes: u64,
        queued: u64,
        written: u64,
        /// Frames played in earlier passes, and taken at the latest.
        played: u64,
        taken: u64,
        underruns: u64,
        lost: u64,
    }

    impl Shared {
        fn new() -> Self {
            Self {
                detector: Detector::new(false, PERIOD as u32, RATE),
                passes: 0,
                queued: 0,
                written: 0,
                played: 0,
                taken: 0,
                underruns: 0,
                lost: 0,
            }
        }

        /// A wake-up `late` frames after pass `pass`, with a quick render.
        fn wake(&mut self, pass: u64, late: u64) {
            self.wake_rendering(pass, late, 1);
        }

        /// A wake-up `late` frames after pass `pass`, whose write lands
        /// `render` frames later.
        fn wake_rendering(&mut self, pass: u64, late: u64, render: u64) {
            let now = pass * PERIOD + late;
            self.run_until(now);
            let in_pass = now - (self.passes - 1) * PERIOD;
            let outcome = self.detector.observe(Reading {
                played: self.played + self.taken.min(in_pass),
                qpc: qpc(now),
                written: self.written,
                padding: self.queued,
            });
            self.underruns += u64::from(outcome.underrun);
            self.lost += outcome.lost;
            let frames = (2 * PERIOD).saturating_sub(self.queued);
            self.run_until(now + render);
            self.detector.landed(self.queued);
            self.queued += frames;
            self.written += frames;
        }

        /// Runs the engine passes due by frame `time`.
        fn run_until(&mut self, time: u64) {
            while self.passes * PERIOD <= time {
                self.played += self.taken;
                self.taken = self.queued.min(PERIOD);
                self.queued -= self.taken;
                self.passes += 1;
            }
        }
    }

    #[test]
    fn punctual_shared_wakeups_are_clean() {
        let mut stream = Shared::new();
        for pass in 0..2000 {
            stream.wake(pass, 3 + pass % 7);
        }
        assert_eq!((stream.underruns, stream.lost), (0, 0));
    }

    #[test]
    fn late_but_covered_by_the_buffer_is_not_a_dropout() {
        let mut stream = Shared::new();
        for pass in 0..100 {
            // Almost a full period late every tenth wake-up.
            stream.wake(pass, if pass % 10 == 5 { 120 } else { 2 });
        }
        assert_eq!(stream.underruns, 0);
    }

    #[test]
    fn a_shared_mode_stall_is_one_dropout() {
        let mut stream = Shared::new();
        for pass in 0..50 {
            stream.wake(pass, 2);
        }
        // Blocked for five periods with two queued: three are lost.
        stream.wake(54, 2);
        for pass in 55..200 {
            stream.wake(pass, 2);
        }
        assert_eq!(stream.underruns, 1);
        assert!(
            (3 * PERIOD - 8..=3 * PERIOD + 8).contains(&stream.lost),
            "lost {}",
            stream.lost
        );
    }

    #[test]
    fn a_render_that_takes_too_long_is_one_dropout() {
        let mut stream = Shared::new();
        for pass in 0..50 {
            stream.wake(pass, 2);
        }
        // A punctual wake-up whose render takes five periods. The period
        // that was queued plays, and the next four passes find nothing.
        stream.wake_rendering(50, 2, 5 * PERIOD);
        // Signalled meanwhile, the next wake-up comes as soon as it lands.
        stream.wake(55, 3);
        for pass in 56..200 {
            stream.wake(pass, 2);
        }
        assert_eq!(stream.underruns, 1);
        assert!(
            (4 * PERIOD - 8..=4 * PERIOD + 8).contains(&stream.lost),
            "lost {}",
            stream.lost
        );
    }

    #[test]
    fn a_render_that_lands_just_in_time_is_not_a_dropout() {
        let mut stream = Shared::new();
        for pass in 0..200 {
            // Every tenth wake-up is late and its render runs past the next
            // pass, so the buffer runs low, but the write lands before the
            // pass after.
            match pass % 10 {
                5 => stream.wake_rendering(pass, 100, 100),
                _ => stream.wake(pass, 2),
            }
        }
        assert_eq!((stream.underruns, stream.lost), (0, 0));
    }

    #[test]
    fn stalls_far_apart_count_separately() {
        let mut stream = Shared::new();
        let mut pass = 0;
        for block in 0..6 {
            if block > 0 {
                // Three wake-ups missed: two periods lost.
                pass += 3;
            }
            for _ in 0..40 {
                stream.wake(pass, 2);
                pass += 1;
            }
        }
        assert_eq!(stream.underruns, 5);
        assert!(
            (10 * PERIOD - 20..=10 * PERIOD + 20).contains(&stream.lost),
            "lost {}",
            stream.lost
        );
    }

    #[test]
    fn a_clock_gap_with_audio_queued_is_jitter() {
        let mut detector = Detector::new(false, PERIOD as u32, RATE);
        let reading = |frames: u64, wall: u64| Reading {
            played: frames,
            qpc: qpc(wall),
            written: frames + 2 * PERIOD,
            padding: PERIOD,
        };
        detector.observe(reading(1000, 1000));
        // The clock reads a period behind the wall clock, but a period was
        // still queued.
        let outcome = detector.observe(reading(1128, 1256));
        assert_eq!(outcome, Outcome::default());
    }

    #[test]
    fn a_gap_the_wakeup_after_a_low_buffer_still_counts() {
        let mut detector = Detector::new(false, PERIOD as u32, RATE);
        let reading = |played: u64, wall: u64, padding: u64| Reading {
            played,
            qpc: qpc(wall),
            written: played + padding,
            padding,
        };
        detector.observe(reading(1000, 1000, 2 * PERIOD));
        // The buffer ran dry, but the clock hasn't caught up with it yet.
        assert!(!detector.observe(reading(1128, 1128, 0)).underrun);
        // It shows at the next wake-up, when the buffer has been refilled.
        let outcome = detector.observe(reading(1128, 1256, 2 * PERIOD));
        assert!(outcome.underrun);
        assert_eq!(outcome.lost, PERIOD);
    }

    /// An exclusive-mode stream on a driver whose clock runs on whatever the
    /// device plays. Wake-up `n` asks for the buffer that plays from frame
    /// `(n + 1) * PERIOD`; `writes` says how many buffers have been written.
    fn free_running(detector: &mut Detector, now: u64, writes: u64) -> Outcome {
        detector.observe(Reading {
            played: now,
            qpc: qpc(now),
            written: writes * PERIOD,
            padding: 0,
        })
    }

    #[test]
    fn punctual_exclusive_wakeups_are_clean() {
        let mut detector = Detector::new(true, PERIOD as u32, RATE);
        for n in 1..2000 {
            // Up to 100 frames late, inside the period.
            let now = n * PERIOD + (n * 37) % 100;
            let outcome = free_running(&mut detector, now, n + 1);
            assert_eq!(outcome, Outcome::default(), "wake-up {n}");
        }
    }

    #[test]
    fn exclusive_playback_past_the_written_audio_is_a_dropout() {
        let mut detector = Detector::new(true, PERIOD as u32, RATE);
        let mut underruns = 0;
        for n in 1..20 {
            underruns += u64::from(free_running(&mut detector, n * PERIOD + 5, n + 1).underrun);
        }
        // Wake-up 20 comes one and a half periods late: buffer 21 should have
        // started playing half a period ago.
        let outcome = free_running(&mut detector, 20 * PERIOD + 192, 21);
        assert!(outcome.underrun);
        assert!((55..=70).contains(&outcome.lost), "lost {}", outcome.lost);
        // The next signal came while it was late, so wake-up 21 is at once.
        underruns += u64::from(free_running(&mut detector, 20 * PERIOD + 200, 22).underrun);
        for n in 22..100 {
            underruns += u64::from(free_running(&mut detector, n * PERIOD + 5, n + 1).underrun);
        }
        assert_eq!(underruns, 0);
    }

    #[test]
    fn a_stream_left_a_buffer_behind_is_one_dropout() {
        let mut detector = Detector::new(true, PERIOD as u32, RATE);
        for n in 1..20 {
            free_running(&mut detector, n * PERIOD + 5, n + 1);
        }
        assert!(free_running(&mut detector, 20 * PERIOD + 192, 21).underrun);
        // Two signals came while it was late and woke it once: from here on
        // it writes one buffer behind.
        let mut later = 0;
        for n in 22..200 {
            later += u64::from(free_running(&mut detector, n * PERIOD + 5, n).underrun);
        }
        assert_eq!(later, 0);
        // A later dropout still shows.
        assert!(free_running(&mut detector, 200 * PERIOD + 300, 200).underrun);
    }

    #[test]
    fn a_clock_that_follows_the_writes_shows_a_stall_as_a_gap() {
        let mut detector = Detector::new(true, PERIOD as u32, RATE);
        // The clock stops wherever the written audio runs out.
        let mut wake = |now: u64, writes: u64| {
            detector.observe(Reading {
                played: now.min(writes * PERIOD),
                qpc: qpc(now),
                written: writes * PERIOD,
                padding: 0,
            })
        };
        for n in 1..20 {
            assert!(!wake(n * PERIOD + 5, n + 1).underrun);
        }
        let stalled = wake(20 * PERIOD + 400, 21);
        assert!(stalled.underrun);
        assert!(stalled.lost >= PERIOD, "lost {}", stalled.lost);
    }

    #[test]
    fn a_clock_still_at_zero_after_starting_is_not_a_dropout() {
        let mut detector = Detector::new(true, PERIOD as u32, RATE);
        for n in 0..5 {
            let outcome = detector.observe(Reading {
                played: 0,
                qpc: qpc(n * PERIOD),
                written: (n + 1) * PERIOD,
                padding: 0,
            });
            assert_eq!(outcome, Outcome::default());
        }
        for n in 5..50 {
            assert!(!free_running(&mut detector, n * PERIOD + 3, n + 1).underrun);
        }
    }

    #[test]
    fn a_clock_going_backwards_starts_over() {
        let mut detector = Detector::new(false, PERIOD as u32, RATE);
        let reading = |played: u64, wall: u64| Reading {
            played,
            qpc: qpc(wall),
            written: played + PERIOD,
            padding: 0,
        };
        detector.observe(reading(10_000, 10_000));
        assert!(!detector.observe(reading(5_000, 10_128)).underrun);
        assert!(!detector.observe(reading(5_128, 10_256)).underrun);
    }

    #[test]
    fn a_failed_reading_starts_the_comparison_again() {
        let mut detector = Detector::new(false, PERIOD as u32, RATE);
        let reading = |played: u64, wall: u64| Reading {
            played,
            qpc: qpc(wall),
            written: played + PERIOD,
            padding: 0,
        };
        detector.observe(reading(1000, 1000));
        detector.reset();
        // Compared with the reading before the reset, this would be a gap.
        assert!(!detector.observe(reading(1128, 1500)).underrun);
        assert!(!detector.observe(reading(1256, 1628)).underrun);
        assert!(detector.observe(reading(1384, 1884)).underrun);
    }

    #[test]
    fn observing_uses_no_heap() {
        let mut detector = Detector::new(false, PERIOD as u32, RATE);
        musicable_engine::rt::scope(|| {
            for n in 1..100 {
                detector.observe(Reading {
                    played: n * PERIOD,
                    qpc: qpc(n * PERIOD),
                    written: (n + 2) * PERIOD,
                    padding: PERIOD,
                });
            }
        });
    }
}

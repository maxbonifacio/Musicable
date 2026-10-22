//! Counting dropouts on an ASIO stream.
//!
//! A driver calls the host once per buffer half, to have it filled while the
//! device plays the other. A dropout is a half the device played before the
//! host had written it. At each callback the stream looks for the signs:
//!
//! - **The driver reported one**, with an overload message, or by asking
//!   the host to resynchronize because it lost audio.
//! - **The sample position jumped** more than one and a half buffers since
//!   the last callback: the driver skipped buffer switches while the host
//!   ran late.
//! - **The driver asked for the same half twice in a row**, having skipped
//!   the switch to the other.
//! - **The last callback took longer than a buffer**, so the device played
//!   the half it was writing before it was done.
//! - **The callback came after its half started playing**: more than a
//!   buffer and the output latency the driver reports after the last one.
//!   Buffer switches come a buffer apart, and the half asked for at one
//!   plays the latency after it. ASIO asks drivers to call back for every
//!   half, late or not, and then the position and the halves keep step: only
//!   the time shows the dropout.
//!
//! A driver can say that it reports every dropout itself, and then, as the
//! specification asks, the stream counts only what the driver reports. The
//! first callbacks fill the device's buffers before it starts, so their
//! positions and timing mean nothing yet: the checks begin after
//! [`STARTUP`] of them. The next [`SETTLING`] show whether the driver calls
//! back evenly. One that calls back twice within a quarter of a buffer then
//! does it in bursts, from buffers of its own, so the time between its
//! callbacks says nothing about the device, and late ones don't count. Signs
//! in the [`RECOVERY_WAKEUPS`] callbacks after a dropout count towards it, as
//! on the other backends.

use crate::xrun::RECOVERY_WAKEUPS;

/// Callbacks before the stream's timing means anything.
pub(super) const STARTUP: u64 = 8;

/// Callbacks after [`STARTUP`] that show whether the driver calls back
/// evenly, before a late one counts.
pub(super) const SETTLING: u64 = 64;

/// What the stream saw at one callback, before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Callback {
    /// The buffer half the driver asked for: 0 or 1.
    pub index: usize,
    /// The sample position at the start of the half, if the driver said.
    pub position: Option<u64>,
    /// The time since the callback before, in frames: None for the first.
    pub interval: Option<u64>,
    /// The output latency the driver reports, in frames: from a buffer
    /// switch until the half asked for then plays.
    pub latency: u64,
    /// Whether the callback before took longer than a buffer.
    pub overran: bool,
    /// Overloads and resync requests the driver reported since the callback
    /// before.
    pub reported: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Outcome {
    /// A new dropout began.
    pub underrun: bool,
    /// The audio lost, in frames, new dropout or not: what the sample
    /// position skipped, or a buffer's worth when nothing says more.
    pub lost: u64,
}

#[derive(Debug)]
pub(super) struct Detector {
    period: u64,
    /// The driver reports every dropout itself.
    driver_reports: bool,
    /// The driver called back in bursts while the stream settled.
    bursts: bool,
    callbacks: u64,
    last: Option<Callback>,
    /// Callbacks left in the current dropout's recovery: 0 when there is
    /// none.
    recovering: u32,
}

impl Detector {
    pub fn new(period: u32, driver_reports: bool) -> Self {
        Self {
            period: u64::from(period.max(1)),
            driver_reports,
            bursts: false,
            callbacks: 0,
            last: None,
            recovering: 0,
        }
    }

    /// Looks for a dropout since the last callback. Real-time safe.
    pub fn observe(&mut self, now: Callback) -> Outcome {
        self.callbacks += 1;
        let last = self.last.replace(now);
        let mut sign = now.reported > 0;
        let mut lost = self.period * u64::from(now.reported);
        if let Some(last) = last
            && !self.driver_reports
            && self.callbacks > STARTUP
        {
            if let (Some(before), Some(position)) = (last.position, now.position)
                // A position that went back was reset: nothing to compare.
                && position >= before
                && (position - before) * 2 > self.period * 3
            {
                sign = true;
                lost = lost.max(position - before - self.period);
            }
            if now.index == last.index || now.overran {
                sign = true;
                lost = lost.max(self.period);
            }
            if let Some(interval) = now.interval {
                // The latest a callback can come after the last one and
                // still be in time for its half. A half plays a buffer after
                // it's asked for at the soonest, whatever the driver reports.
                let latest = self.period + now.latency.max(self.period);
                if self.callbacks <= STARTUP + SETTLING {
                    self.bursts |= interval * 4 < self.period;
                } else if !self.bursts && interval > latest {
                    sign = true;
                    lost = lost.max(self.period);
                }
            }
        }
        if self.recovering > 0 {
            self.recovering -= 1;
            Outcome {
                underrun: false,
                lost: if sign { lost } else { 0 },
            }
        } else if sign {
            self.recovering = RECOVERY_WAKEUPS;
            Outcome {
                underrun: true,
                lost,
            }
        } else {
            Outcome::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: u64 = 256;
    /// What the RME HDSPe AIO reports at 256 frames: a buffer, and its
    /// converters' and the driver's safety offset.
    const LATENCY: u64 = 321;

    /// A driver calling back once per half, with the sample position.
    struct Stream {
        detector: Detector,
        index: usize,
        position: u64,
        /// The time until the next callback, in frames: None before the
        /// first.
        interval: Option<u64>,
        underruns: u32,
        lost: u64,
    }

    impl Stream {
        fn new(driver_reports: bool) -> Self {
            let mut stream = Self {
                detector: Detector::new(PERIOD as u32, driver_reports),
                index: 0,
                position: 0,
                interval: None,
                underruns: 0,
                lost: 0,
            };
            // Past the start and the settling, punctually.
            stream.run((STARTUP + SETTLING) as usize);
            stream
        }

        fn callback(&mut self, overran: bool, reported: u32) {
            let outcome = self.detector.observe(Callback {
                index: self.index,
                position: Some(self.position),
                interval: self.interval,
                latency: LATENCY,
                overran,
                reported,
            });
            self.underruns += u32::from(outcome.underrun);
            self.lost += outcome.lost;
            self.index ^= 1;
            self.position += PERIOD;
            self.interval = Some(PERIOD);
        }

        fn run(&mut self, callbacks: usize) {
            for _ in 0..callbacks {
                self.callback(false, 0);
            }
        }

        /// The driver skips `halves` buffer switches while the host is late.
        fn skip(&mut self, halves: u64) {
            self.position += halves * PERIOD;
            if halves % 2 == 1 {
                self.index ^= 1;
            }
        }

        /// The next callback comes `frames` after the last, as when the
        /// driver's thread ran late, with the position and the halves in
        /// step.
        fn late(&mut self, frames: u64) {
            self.interval = Some(frames);
        }
    }

    #[test]
    fn punctual_callbacks_have_no_dropouts() {
        let mut stream = Stream::new(false);
        stream.run(10_000);
        assert_eq!((stream.underruns, stream.lost), (0, 0));
    }

    #[test]
    fn a_skipped_switch_is_one_dropout() {
        let mut stream = Stream::new(false);
        stream.run(100);
        stream.skip(1);
        stream.run(100);
        // The position jumped a buffer, and the same half came twice.
        assert_eq!((stream.underruns, stream.lost), (1, PERIOD));
    }

    #[test]
    fn a_long_stall_loses_what_the_position_skipped() {
        let mut stream = Stream::new(false);
        stream.run(100);
        stream.skip(4);
        stream.run(100);
        assert_eq!((stream.underruns, stream.lost), (1, 4 * PERIOD));
    }

    #[test]
    fn a_callback_that_overran_is_a_dropout_even_when_the_driver_kept_up() {
        let mut stream = Stream::new(false);
        stream.run(100);
        stream.callback(true, 0);
        stream.run(100);
        assert_eq!((stream.underruns, stream.lost), (1, PERIOD));
    }

    #[test]
    fn a_callback_after_its_half_started_playing_is_a_dropout() {
        let mut stream = Stream::new(false);
        // Late, but in time for its half.
        stream.late(PERIOD + LATENCY);
        stream.run(100);
        assert_eq!(stream.underruns, 0);
        // The 18 ms that went uncounted on the RME at 256 frames, during a
        // shadow copy.
        stream.late(805);
        stream.run(100);
        assert_eq!((stream.underruns, stream.lost), (1, PERIOD));
    }

    #[test]
    fn a_driver_that_calls_back_in_bursts_is_not_timed() {
        // Four halves at once, every four buffers, as a driver feeding the
        // device from buffers of its own can.
        let mut detector = Detector::new(PERIOD as u32, false);
        for n in 0..1000 {
            let outcome = detector.observe(Callback {
                index: (n % 2) as usize,
                position: Some(n * PERIOD),
                interval: (n > 0).then_some(if n % 4 == 0 { 4 * PERIOD } else { 0 }),
                latency: LATENCY,
                overran: false,
                reported: 0,
            });
            assert_eq!(outcome, Outcome::default(), "callback {n}");
        }
    }

    #[test]
    fn signs_during_a_recovery_count_towards_it() {
        let mut stream = Stream::new(false);
        stream.run(100);
        stream.callback(true, 0);
        // The render that overran made the driver skip a switch too.
        stream.skip(1);
        stream.run(3);
        stream.callback(true, 1);
        stream.run(100);
        assert_eq!(stream.underruns, 1);
        // An overrun, then a skipped switch, then an overrun and a report:
        // a buffer each.
        assert_eq!(stream.lost, 3 * PERIOD);
        // A while later, a new one.
        stream.callback(true, 0);
        assert_eq!(stream.underruns, 2);
    }

    #[test]
    fn a_driver_that_reports_dropouts_is_taken_at_its_word() {
        let mut stream = Stream::new(true);
        stream.run(100);
        // Ignored: the driver didn't report them.
        stream.skip(3);
        stream.callback(true, 0);
        stream.late(10 * PERIOD);
        stream.run(100);
        assert_eq!((stream.underruns, stream.lost), (0, 0));
        stream.callback(false, 1);
        stream.run(100);
        assert_eq!((stream.underruns, stream.lost), (1, PERIOD));
    }

    #[test]
    fn the_first_callbacks_fill_the_buffers_and_count_for_nothing() {
        let mut detector = Detector::new(PERIOD as u32, false);
        // Prefilled halves at once, with the position standing still, then
        // the hardware starting a few buffers in.
        let positions = [
            0,
            0,
            0,
            3 * PERIOD,
            4 * PERIOD,
            5 * PERIOD,
            6 * PERIOD,
            7 * PERIOD,
        ];
        let intervals = [None, Some(0), Some(0), Some(3 * PERIOD)];
        for (n, position) in positions.into_iter().enumerate() {
            let outcome = detector.observe(Callback {
                index: n % 2,
                position: Some(position),
                interval: intervals.get(n).copied().unwrap_or(Some(PERIOD)),
                latency: LATENCY,
                overran: false,
                reported: 0,
            });
            assert_eq!(outcome, Outcome::default(), "callback {n}");
        }
        // The halves that came at once didn't make it a driver that calls
        // back in bursts: once settled, a late callback counts.
        let mut position = 7 * PERIOD;
        let mut next = |interval| {
            position += PERIOD;
            detector.observe(Callback {
                index: (position / PERIOD % 2) as usize,
                position: Some(position),
                interval: Some(interval),
                latency: LATENCY,
                overran: false,
                reported: 0,
            })
        };
        for _ in 0..SETTLING {
            assert_eq!(next(PERIOD), Outcome::default());
        }
        assert!(next(4 * PERIOD).underrun);

        // A driver report counts even then.
        let mut detector = Detector::new(PERIOD as u32, false);
        let outcome = detector.observe(Callback {
            index: 0,
            position: None,
            interval: None,
            latency: LATENCY,
            overran: false,
            reported: 1,
        });
        assert!(outcome.underrun);
    }

    #[test]
    fn a_position_that_goes_back_or_is_missing_is_not_compared() {
        let mut stream = Stream::new(false);
        stream.run(100);
        stream.position = 0;
        stream.run(100);
        assert_eq!(stream.underruns, 0);

        let mut detector = Detector::new(PERIOD as u32, false);
        for n in 0..100 {
            let outcome = detector.observe(Callback {
                index: n % 2,
                position: None,
                interval: Some(PERIOD),
                latency: LATENCY,
                overran: false,
                reported: 0,
            });
            assert_eq!(outcome, Outcome::default());
        }
    }

    #[test]
    fn detecting_uses_no_heap() {
        musicable_engine::rt::scope(|| {
            let mut stream = Stream::new(false);
            stream.run(100);
            stream.skip(1);
            stream.late(4 * PERIOD);
            stream.run(100);
        });
    }
}

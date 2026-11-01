//! Where the playhead is drawn between the engine's reports of it.
//!
//! The engine reports its position once a period of the audio device, a
//! few hundred times a second, unevenly, and the window draws up to 144
//! times a second. Drawn as reported, the playhead stutters, and a view
//! that keeps it still, as Continuous Follow does, shakes. So between
//! reports it moves on by the time since the last one, as the audio does,
//! and snaps to each report, to a locate, a stop and a loop's wrap. It
//! never gets more than a period ahead of the last report, so the next one
//! is never behind it, and it never runs past the loop's end.

use std::time::Instant;

/// The playhead between the engine's reports, in frames from the start of
/// the song.
#[derive(Debug, Default)]
pub(crate) struct Playhead {
    /// The last position reported, and when it came.
    last: Option<(u64, Instant)>,
}

impl Playhead {
    /// Where to draw the playhead at `now`, `reported` being where the
    /// engine last said it was: while it `playing`, at `rate` frames a
    /// second, moved on by the time since that report, at most `period`
    /// frames, and short of `loop_end` if the song loops there and it's
    /// before it.
    pub fn at(
        &mut self,
        reported: u64,
        playing: bool,
        now: Instant,
        rate: u32,
        period: u64,
        loop_end: Option<u64>,
    ) -> u64 {
        if !playing {
            self.last = None;
            return reported;
        }
        let since = match self.last {
            Some((last, since)) if last == reported => since,
            _ => {
                self.last = Some((reported, now));
                return reported;
            }
        };
        let elapsed = now.saturating_duration_since(since).as_secs_f64();
        let ahead = ((elapsed * f64::from(rate)) as u64).min(period);
        let at = reported + ahead;
        match loop_end {
            Some(end) if reported < end => at.min(end - 1),
            _ => at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const RATE: u32 = 48_000;
    const PERIOD: u64 = 480;

    #[test]
    fn it_moves_on_between_reports_and_snaps_to_each() {
        let mut playhead = Playhead::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(playhead.at(1000, true, at(0), RATE, PERIOD, None), 1000);
        // 5 ms later, 240 frames on.
        assert_eq!(playhead.at(1000, true, at(5), RATE, PERIOD, None), 1240);
        // Never more than a period ahead of the report.
        assert_eq!(playhead.at(1000, true, at(50), RATE, PERIOD, None), 1480);
        // The next report is never behind what was drawn.
        assert_eq!(playhead.at(1480, true, at(51), RATE, PERIOD, None), 1480);
        // A locate back snaps to it.
        assert_eq!(playhead.at(200, true, at(52), RATE, PERIOD, None), 200);
        // Stopped, it's where it's reported.
        assert_eq!(playhead.at(200, false, at(60), RATE, PERIOD, None), 200);
        assert_eq!(playhead.at(200, true, at(61), RATE, PERIOD, None), 200);
    }

    #[test]
    fn it_never_runs_past_the_loop_s_end() {
        let mut playhead = Playhead::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let end = Some(1100);
        playhead.at(1000, true, at(0), RATE, PERIOD, end);
        assert_eq!(playhead.at(1000, true, at(10), RATE, PERIOD, end), 1099);
        // The wrap snaps back to the loop's start.
        assert_eq!(playhead.at(40, true, at(11), RATE, PERIOD, end), 40);
        // Past the loop's end already, as when it plays on from after it,
        // the loop doesn't hold it.
        playhead.at(2000, true, at(12), RATE, PERIOD, end);
        assert_eq!(playhead.at(2000, true, at(14), RATE, PERIOD, end), 2096);
    }
}

//! Period arithmetic for WASAPI, apart from the system calls so it can be
//! tested anywhere.

/// 100-nanosecond units per second: WASAPI's `REFERENCE_TIME`, and the unit
/// `IAudioClock::GetPosition` gives QPC times in.
pub(crate) const HNS_PER_SECOND: u64 = 10_000_000;

/// `frames` at `rate` in 100 ns units, rounded to the nearest.
pub(crate) fn frames_to_hns(frames: u64, rate: u32) -> u64 {
    let rate = u128::from(rate.max(1));
    let hns = (u128::from(frames) * u128::from(HNS_PER_SECOND) + rate / 2) / rate;
    u64::try_from(hns).unwrap_or(u64::MAX)
}

/// A time in 100 ns units as frames at `rate`, rounded to the nearest.
pub(crate) fn hns_to_frames(hns: u64, rate: u32) -> u64 {
    let hns_per_second = u128::from(HNS_PER_SECOND);
    let frames = (u128::from(hns) * u128::from(rate) + hns_per_second / 2) / hns_per_second;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// The shared-mode engine periods IAudioClient3 offers for a format, in
/// frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnginePeriods {
    pub default: u32,
    /// Periods other than the default are multiples of this.
    pub fundamental: u32,
    pub min: u32,
    pub max: u32,
}

impl EnginePeriods {
    /// Whether the driver offers periods below the default.
    pub fn low_latency(&self) -> bool {
        self.min < self.default
    }

    /// The period for a request: the smallest by default, otherwise the one
    /// requested rounded up to a multiple of the fundamental period and kept
    /// in range.
    pub fn pick(&self, requested: Option<u32>) -> u32 {
        match requested {
            None => self.min,
            Some(frames) => {
                let fundamental = self.fundamental.max(1);
                let rounded = frames.div_ceil(fundamental).saturating_mul(fundamental);
                rounded.clamp(self.min, self.max.max(self.min))
            }
        }
    }
}

/// The exclusive-mode period for a request, in 100 ns units: the device's
/// minimum by default, and never less.
pub(crate) fn exclusive_period(requested: Option<u32>, rate: u32, min_hns: u64) -> u64 {
    requested
        .map_or(min_hns, |frames| frames_to_hns(u64::from(frames), rate))
        .max(min_hns)
}

/// How many frames a shared-mode stream keeps queued: two periods, so a
/// wake-up can come almost a period late without a dropout, or the whole
/// buffer if it holds less. Filling the whole buffer would only add latency.
pub(crate) fn fill_target(period: u32, buffer: u32) -> u32 {
    period.saturating_mul(2).min(buffer)
}

/// The period a driver really runs at, from wake-up intervals in 100 ns
/// units, if it's well above the `period` it set up. Some drivers keep to
/// their own buffer setting whatever they report, and writing less than they
/// play breaks the audio up. Sorts `intervals`.
pub(crate) fn delivered_period(intervals: &mut [u64], period: u32, rate: u32) -> Option<u32> {
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_unstable();
    // The median: the first wake-ups after a start can come in a burst.
    let median = intervals[intervals.len() / 2];
    let delivered = u32::try_from(hns_to_frames(median, rate)).unwrap_or(u32::MAX);
    (u64::from(delivered) * 10 > u64::from(period) * 13).then_some(delivered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_hns_convert_with_rounding() {
        assert_eq!(frames_to_hns(48_000, 48_000), HNS_PER_SECOND);
        assert_eq!(frames_to_hns(128, 48_000), 26_667);
        assert_eq!(frames_to_hns(132, 44_100), 29_932);
        assert_eq!(hns_to_frames(26_667, 48_000), 128);
        assert_eq!(hns_to_frames(100_000, 44_100), 441);
        assert_eq!(hns_to_frames(frames_to_hns(256, 44_100), 44_100), 256);
    }

    #[test]
    fn shared_periods_round_up_to_the_fundamental() {
        let periods = EnginePeriods {
            default: 480,
            fundamental: 32,
            min: 128,
            max: 480,
        };
        assert!(periods.low_latency());
        assert_eq!(periods.pick(None), 128);
        assert_eq!(periods.pick(Some(64)), 128, "below the minimum");
        assert_eq!(periods.pick(Some(150)), 160);
        assert_eq!(periods.pick(Some(256)), 256);
        assert_eq!(periods.pick(Some(4096)), 480, "above the maximum");

        let fixed = EnginePeriods {
            default: 441,
            fundamental: 441,
            min: 441,
            max: 441,
        };
        assert!(!fixed.low_latency());
        assert_eq!(fixed.pick(Some(64)), 441);
    }

    #[test]
    fn exclusive_periods_never_go_below_the_minimum() {
        let min = 30_000; // 3 ms
        assert_eq!(exclusive_period(None, 44_100, min), min);
        assert_eq!(exclusive_period(Some(64), 44_100, min), min);
        assert_eq!(exclusive_period(Some(256), 44_100, min), 58_050);
    }

    #[test]
    fn shared_streams_keep_two_periods_queued() {
        assert_eq!(fill_target(128, 288), 256);
        assert_eq!(fill_target(441, 970), 882);
        assert_eq!(fill_target(128, 200), 200, "a buffer under two periods");
    }

    #[test]
    fn a_driver_running_at_its_own_buffer_size_is_caught() {
        // The RME HDSPe AIO: 132 frames set up, woken every 5.8 ms (256
        // frames at 44.1 kHz), with a burst at the start.
        let mut intervals = vec![58_050; 20];
        intervals.extend([1_000, 1_200, 58_300, 57_800]);
        assert_eq!(delivered_period(&mut intervals, 132, 44_100), Some(256));

        // A driver that keeps to what it set up, with some jitter.
        let mut intervals: Vec<u64> = (0..24).map(|i| 26_667 + (i % 5) * 300).collect();
        assert_eq!(delivered_period(&mut intervals, 128, 48_000), None);
        assert_eq!(delivered_period(&mut [], 128, 48_000), None);
    }
}

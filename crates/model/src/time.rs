//! Musical time: positions in beats, tempo and time signature, and conversion
//! between samples, beats and bars.
//!
//! A beat is a quarter note whatever the time signature, and tempo counts
//! quarter notes per minute, as plugin APIs do. A time signature's own beats
//! are the notes its denominator names: six eighth notes to the bar in 6/8.
//!
//! [`Beats`] holds musical positions exactly, as whole ticks, so musical
//! arithmetic never drifts. Converting to samples goes through floating
//! point, and a musical position plays on the frame nearest its exact time.
//!
//! The [`TempoMap`] holds tempo and time signature changes. Tier 0 only uses
//! a constant tempo and time signature, but the map stores lists of changes,
//! so automation fits later without changing the project format.

use std::fmt;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

/// A musical position or length, in beats (quarter notes).
///
/// Positions count from the start of the song, and can be negative, before
/// it. The value is exact: a beat is [`TICKS_PER_BEAT`](Self::TICKS_PER_BEAT)
/// ticks, so tuplets and grid positions add up without rounding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Beats(i64);

impl Beats {
    /// Ticks in a beat. It divides by 1024 and by every tuplet up to 10, and a
    /// tick is shorter than a sample at any tempo up to 999 BPM and any sample
    /// rate up to 768 kHz, so a position taken from a sample plays back on
    /// that sample.
    pub const TICKS_PER_BEAT: i64 = 322_560;

    pub const ZERO: Beats = Beats(0);

    /// Whole beats.
    pub const fn new(beats: i64) -> Self {
        Self(beats * Self::TICKS_PER_BEAT)
    }

    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// `numerator / denominator` beats, to the nearest tick. Exact for
    /// tuplets up to 10 and for binary fractions down to 1/1024.
    ///
    /// # Panics
    ///
    /// If `denominator` is zero.
    pub fn from_ratio(numerator: i64, denominator: i64) -> Self {
        assert!(denominator != 0, "a ratio of beats needs a denominator");
        let n = i128::from(numerator) * i128::from(Self::TICKS_PER_BEAT);
        let d = i128::from(denominator);
        // Halfway rounds away from zero, as `f64::round` does.
        let magnitude = (2 * n.abs() + d.abs()) / (2 * d.abs());
        let ticks = if (n < 0) != (d < 0) {
            -magnitude
        } else {
            magnitude
        };
        Self(ticks.clamp(i64::MIN.into(), i64::MAX.into()) as i64)
    }

    /// Fractional beats, to the nearest tick.
    pub fn from_f64(beats: f64) -> Self {
        Self((beats * Self::TICKS_PER_BEAT as f64).round() as i64)
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::TICKS_PER_BEAT as f64
    }
}

impl fmt::Debug for Beats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Beats({})", self.to_f64())
    }
}

impl Add for Beats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Beats {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for Beats {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl SubAssign for Beats {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl Neg for Beats {
    type Output = Self;

    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Mul<i64> for Beats {
    type Output = Self;

    fn mul(self, rhs: i64) -> Self {
        Self(self.0 * rhs)
    }
}

/// A stretch of musical time, from `start` up to but not including `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BeatRange {
    pub start: Beats,
    pub end: Beats,
}

impl BeatRange {
    pub const fn new(start: Beats, end: Beats) -> Self {
        Self { start, end }
    }

    /// Zero when the range is empty.
    pub fn len(&self) -> Beats {
        (self.end - self.start).max(Beats::ZERO)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn contains(&self, at: Beats) -> bool {
        self.start <= at && at < self.end
    }
}

/// A time signature, such as 4/4 or 6/8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeSignature {
    numerator: u8,
    denominator: u8,
}

impl TimeSignature {
    pub const FOUR_FOUR: TimeSignature = TimeSignature {
        numerator: 4,
        denominator: 4,
    };

    /// `None` unless the numerator is at least 1 and the denominator is a
    /// power of two up to 64.
    pub const fn new(numerator: u8, denominator: u8) -> Option<Self> {
        if numerator > 0 && denominator.is_power_of_two() && denominator <= 64 {
            Some(Self {
                numerator,
                denominator,
            })
        } else {
            None
        }
    }

    /// How many of the signature's beats make a bar.
    pub const fn numerator(self) -> u8 {
        self.numerator
    }

    /// The note the signature counts: 4 for quarter notes, 8 for eighths.
    pub const fn denominator(self) -> u8 {
        self.denominator
    }

    /// The length of one of the signature's beats: half a beat, an eighth
    /// note, in 6/8.
    pub const fn beat_length(self) -> Beats {
        Beats::from_ticks(4 * Beats::TICKS_PER_BEAT / self.denominator as i64)
    }

    pub const fn bar_length(self) -> Beats {
        Beats::from_ticks(self.beat_length().ticks() * self.numerator as i64)
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self::FOUR_FOUR
    }
}

/// A tempo that holds from `at` until the next change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoChange {
    pub at: Beats,
    /// Quarter notes per minute.
    pub bpm: f64,
    /// Seconds from the start of the song to `at`.
    seconds: f64,
}

/// A time signature that holds from bar `bar` until the next change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureChange {
    pub bar: i64,
    pub signature: TimeSignature,
    /// Where `bar` starts.
    at: Beats,
}

impl SignatureChange {
    /// Where the change's first bar starts.
    pub fn at(&self) -> Beats {
        self.at
    }
}

/// A position in bars and the time signature's beats. Both count from zero:
/// the song starts at bar 0, beat 0. Displays usually count from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarPosition {
    pub bar: i64,
    /// The signature's beats since the bar started.
    pub beat: u32,
    /// How far past that beat.
    pub offset: Beats,
}

/// One of the time signature's beats, as a metronome counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarBeat {
    pub at: Beats,
    pub bar: i64,
    /// Beat 0 is the downbeat.
    pub beat: u32,
}

/// Tempo and time signature over the song: where every beat and bar falls
/// in time.
///
/// Both are lists of changes. A tempo holds until the next tempo change
/// (there are no ramps yet), and each time signature change starts a bar.
/// The first tempo and the first time signature also cover the time before
/// the song starts.
///
/// Queries don't allocate, so the engine uses the map on the audio thread.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    /// Sorted by position, the first at zero.
    tempos: Vec<TempoChange>,
    /// Sorted by bar, the first at bar zero.
    signatures: Vec<SignatureChange>,
}

impl TempoMap {
    pub const MIN_BPM: f64 = 20.0;
    pub const MAX_BPM: f64 = 999.0;

    /// A constant tempo and time signature. The tempo is clamped to
    /// [`MIN_BPM`](Self::MIN_BPM)..=[`MAX_BPM`](Self::MAX_BPM).
    ///
    /// # Panics
    ///
    /// If `bpm` is NaN.
    pub fn new(bpm: f64, signature: TimeSignature) -> Self {
        Self {
            tempos: vec![TempoChange {
                at: Beats::ZERO,
                bpm: clamp_bpm(bpm),
                seconds: 0.0,
            }],
            signatures: vec![SignatureChange {
                bar: 0,
                signature,
                at: Beats::ZERO,
            }],
        }
    }

    /// The tempo changes, in order. The first is at zero.
    pub fn tempos(&self) -> &[TempoChange] {
        &self.tempos
    }

    /// The time signature changes, in order. The first is at bar zero.
    pub fn signatures(&self) -> &[SignatureChange] {
        &self.signatures
    }

    /// Makes the tempo `bpm` for the whole song, removing any tempo changes.
    /// Clamped like [`new`](Self::new).
    ///
    /// # Panics
    ///
    /// If `bpm` is NaN.
    pub fn set_tempo(&mut self, bpm: f64) {
        self.tempos.truncate(1);
        self.tempos[0].bpm = clamp_bpm(bpm);
    }

    /// Changes the tempo to `bpm` from `at` on, replacing a change already
    /// there. At zero or before, it sets the tempo the song starts with.
    /// Clamped like [`new`](Self::new).
    ///
    /// # Panics
    ///
    /// If `bpm` is NaN.
    pub fn insert_tempo(&mut self, at: Beats, bpm: f64) {
        let at = at.max(Beats::ZERO);
        let bpm = clamp_bpm(bpm);
        match self.tempos.binary_search_by_key(&at, |t| t.at) {
            Ok(i) => self.tempos[i].bpm = bpm,
            Err(i) => self.tempos.insert(
                i,
                TempoChange {
                    at,
                    bpm,
                    seconds: 0.0,
                },
            ),
        }
        for i in 1..self.tempos.len() {
            let previous = self.tempos[i - 1];
            let length = self.tempos[i].at - previous.at;
            self.tempos[i].seconds =
                previous.seconds + length.ticks() as f64 * 60.0 / (previous.bpm * TICKS);
        }
    }

    /// Makes `signature` the time signature for the whole song, removing any
    /// time signature changes.
    pub fn set_signature(&mut self, signature: TimeSignature) {
        self.signatures.truncate(1);
        self.signatures[0].signature = signature;
    }

    /// Changes the time signature to `signature` from bar `bar` on, replacing
    /// a change already there. At bar zero or before, it sets the time
    /// signature the song starts with. Later changes keep their bar numbers.
    pub fn insert_signature(&mut self, bar: i64, signature: TimeSignature) {
        let bar = bar.max(0);
        match self.signatures.binary_search_by_key(&bar, |s| s.bar) {
            Ok(i) => self.signatures[i].signature = signature,
            Err(i) => self.signatures.insert(
                i,
                SignatureChange {
                    bar,
                    signature,
                    at: Beats::ZERO,
                },
            ),
        }
        for i in 1..self.signatures.len() {
            let previous = self.signatures[i - 1];
            let bars = self.signatures[i].bar - previous.bar;
            self.signatures[i].at = previous.at + previous.signature.bar_length() * bars;
        }
    }

    pub fn bpm_at(&self, at: Beats) -> f64 {
        self.tempo_at(at).bpm
    }

    pub fn signature_at(&self, at: Beats) -> TimeSignature {
        let i = self.signatures.partition_point(|s| s.at <= at);
        self.signatures[i.saturating_sub(1)].signature
    }

    /// When `at` happens, in samples from the start of the song at
    /// `sample_rate`. Fractional: most beats fall between samples.
    pub fn sample_at(&self, at: Beats, sample_rate: u32) -> f64 {
        let rate = f64::from(sample_rate);
        let tempo = self.tempo_at(at);
        // Divide last, so a position that falls exactly on a sample comes out
        // exact rather than a hair off it.
        tempo.seconds * rate + (at - tempo.at).ticks() as f64 * 60.0 * rate / (tempo.bpm * TICKS)
    }

    /// The frame `at` plays on: the song position, in samples, nearest to
    /// when it happens. Halfway rounds away from zero.
    pub fn frame_at(&self, at: Beats, sample_rate: u32) -> i64 {
        self.sample_at(at, sample_rate).round() as i64
    }

    /// The musical position at a sample time, in fractional beats. The
    /// inverse of [`sample_at`](Self::sample_at).
    pub fn beats_at(&self, sample: f64, sample_rate: u32) -> f64 {
        let rate = f64::from(sample_rate);
        let i = self.tempos.partition_point(|t| t.seconds * rate <= sample);
        let tempo = &self.tempos[i.saturating_sub(1)];
        tempo.at.to_f64() + (sample - tempo.seconds * rate) * tempo.bpm / (60.0 * rate)
    }

    /// Where bar `bar` starts.
    pub fn bar_start(&self, bar: i64) -> Beats {
        let change = self.bar_signature(bar);
        change.at + change.signature.bar_length() * (bar - change.bar)
    }

    /// Where `at` falls in bars and beats.
    pub fn bar_at(&self, at: Beats) -> BarPosition {
        let i = self.signatures.partition_point(|s| s.at <= at);
        let change = &self.signatures[i.saturating_sub(1)];
        let bar_length = change.signature.bar_length().ticks();
        let beat_length = change.signature.beat_length().ticks();
        let since = (at - change.at).ticks();
        let into_bar = since.rem_euclid(bar_length);
        BarPosition {
            bar: change.bar + since.div_euclid(bar_length),
            beat: (into_bar / beat_length) as u32,
            offset: Beats::from_ticks(into_bar % beat_length),
        }
    }

    /// The time signature's beats in `range`, in order.
    pub fn beats_in(&self, range: BeatRange) -> impl Iterator<Item = BarBeat> + '_ {
        let start = self.bar_at(range.start);
        let mut first = BarBeat {
            at: range.start - start.offset,
            bar: start.bar,
            beat: start.beat,
        };
        if start.offset > Beats::ZERO {
            first = self.next_beat(first);
        }
        std::iter::successors(Some(first), |&beat| Some(self.next_beat(beat)))
            .take_while(move |beat| beat.at < range.end)
    }

    fn tempo_at(&self, at: Beats) -> &TempoChange {
        let i = self.tempos.partition_point(|t| t.at <= at);
        &self.tempos[i.saturating_sub(1)]
    }

    /// The time signature change that bar `bar` falls under.
    fn bar_signature(&self, bar: i64) -> &SignatureChange {
        let i = self.signatures.partition_point(|s| s.bar <= bar);
        &self.signatures[i.saturating_sub(1)]
    }

    fn next_beat(&self, beat: BarBeat) -> BarBeat {
        let signature = self.bar_signature(beat.bar).signature;
        if beat.beat + 1 < u32::from(signature.numerator()) {
            BarBeat {
                at: beat.at + signature.beat_length(),
                bar: beat.bar,
                beat: beat.beat + 1,
            }
        } else {
            BarBeat {
                at: self.bar_start(beat.bar + 1),
                bar: beat.bar + 1,
                beat: 0,
            }
        }
    }
}

impl Default for TempoMap {
    /// 120 BPM in 4/4.
    fn default() -> Self {
        Self::new(120.0, TimeSignature::FOUR_FOUR)
    }
}

const TICKS: f64 = Beats::TICKS_PER_BEAT as f64;

fn clamp_bpm(bpm: f64) -> f64 {
    assert!(!bpm.is_nan(), "the tempo is NaN");
    bpm.clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature::new(numerator, denominator).unwrap()
    }

    fn beats(numerator: i64, denominator: i64) -> Beats {
        Beats::from_ratio(numerator, denominator)
    }

    #[test]
    fn tuplets_add_up_exactly() {
        for tuplet in 1..=10 {
            let note = beats(1, tuplet);
            assert_eq!(note * tuplet, Beats::new(1), "1/{tuplet}");
        }
        assert_eq!(beats(1, 1024) * 1024, Beats::new(1));
        assert_eq!(beats(1, 3) + beats(2, 3), Beats::new(1));
        assert_eq!(beats(-7, 2), -beats(7, 2));
        assert_eq!(Beats::from_f64(2.25), beats(9, 4));
        assert_eq!(beats(9, 4).to_f64(), 2.25);
    }

    #[test]
    fn ratios_round_to_the_nearest_tick() {
        let tick = Beats::from_ticks(1);
        let ticks = Beats::TICKS_PER_BEAT;
        // A third of a tick rounds down, two thirds round up.
        assert_eq!(beats(1, 3 * ticks), Beats::ZERO);
        assert_eq!(beats(2, 3 * ticks), tick);
        assert_eq!(beats(1, 2 * ticks), tick);
        assert_eq!(beats(-1, 2 * ticks), -tick);
        assert_eq!(beats(1, -2 * ticks), -tick);
    }

    #[test]
    fn time_signatures_must_make_sense() {
        assert_eq!(TimeSignature::new(0, 4), None);
        assert_eq!(TimeSignature::new(4, 0), None);
        assert_eq!(TimeSignature::new(4, 3), None);
        assert_eq!(TimeSignature::new(4, 128), None);
        assert_eq!(sig(4, 4).bar_length(), Beats::new(4));
        assert_eq!(sig(6, 8).beat_length(), beats(1, 2));
        assert_eq!(sig(6, 8).bar_length(), Beats::new(3));
        assert_eq!(sig(7, 8).bar_length(), beats(7, 2));
        assert_eq!(sig(5, 16).bar_length(), beats(5, 4));
        assert_eq!(sig(3, 2).bar_length(), Beats::new(6));
        assert_eq!(sig(1, 64).bar_length(), beats(1, 16));
    }

    #[test]
    fn a_constant_tempo_converts_both_ways() {
        let map = TempoMap::new(120.0, TimeSignature::FOUR_FOUR);
        // Half a second per beat.
        assert_eq!(map.sample_at(Beats::new(1), 48_000), 24_000.0);
        assert_eq!(map.frame_at(Beats::new(3), 44_100), 66_150);
        assert_eq!(map.beats_at(24_000.0, 48_000), 1.0);
        assert_eq!(map.beats_at(-12_000.0, 48_000), -0.5);
        assert_eq!(map.frame_at(beats(-1, 2), 48_000), -12_000);

        let map = TempoMap::new(133.0, TimeSignature::FOUR_FOUR);
        // 60 / 133 seconds per beat: a beat lands on a whole sample every
        // 133 beats.
        assert_eq!(map.sample_at(Beats::new(133), 48_000), 2_880_000.0);
        assert_eq!(map.frame_at(Beats::new(1), 48_000), 21_654);
        assert_eq!(map.frame_at(Beats::new(2), 48_000), 43_308);
    }

    #[test]
    fn frames_round_halfway_up() {
        // 128 BPM at 44.1 kHz is 20671.875 samples per beat.
        let map = TempoMap::new(128.0, TimeSignature::FOUR_FOUR);
        assert_eq!(map.sample_at(Beats::new(4), 44_100), 82_687.5);
        assert_eq!(map.frame_at(Beats::new(4), 44_100), 82_688);
    }

    #[test]
    fn positions_taken_from_samples_play_back_on_them() {
        for (bpm, rate) in [
            (120.0, 48_000),
            (133.0, 44_100),
            (999.0, 768_000),
            (20.0, 8_000),
        ] {
            let map = TempoMap::new(bpm, TimeSignature::FOUR_FOUR);
            for sample in (0..10_000_000).step_by(997) {
                let at = Beats::from_f64(map.beats_at(sample as f64, rate));
                assert_eq!(map.frame_at(at, rate), sample, "{bpm} BPM at {rate} Hz");
            }
        }
    }

    #[test]
    fn tempos_are_clamped() {
        let mut map = TempoMap::new(5.0, TimeSignature::FOUR_FOUR);
        assert_eq!(map.bpm_at(Beats::ZERO), TempoMap::MIN_BPM);
        map.set_tempo(f64::INFINITY);
        assert_eq!(map.bpm_at(Beats::ZERO), TempoMap::MAX_BPM);
    }

    #[test]
    fn tempo_changes_hold_until_the_next() {
        let mut map = TempoMap::default();
        map.insert_tempo(Beats::new(8), 60.0);
        map.insert_tempo(Beats::new(12), 240.0);
        assert_eq!(map.tempos().len(), 3);
        assert_eq!(map.bpm_at(Beats::new(7)), 120.0);
        assert_eq!(map.bpm_at(Beats::new(8)), 60.0);
        // 8 beats at 120 take 4 seconds, 4 at 60 take 4 more, and then a
        // beat takes a quarter of a second.
        let rate = 1_000;
        assert_eq!(map.sample_at(Beats::new(8), rate), 4_000.0);
        assert_eq!(map.sample_at(Beats::new(10), rate), 6_000.0);
        assert_eq!(map.sample_at(Beats::new(12), rate), 8_000.0);
        assert_eq!(map.sample_at(Beats::new(14), rate), 8_500.0);
        for beat in [0.0, 3.5, 8.0, 9.25, 12.0, 20.0] {
            let sample = map.sample_at(Beats::from_f64(beat), rate);
            assert_eq!(map.beats_at(sample, rate), beat);
        }

        // Replacing a change, then removing them all.
        map.insert_tempo(Beats::new(8), 90.0);
        assert_eq!(map.tempos().len(), 3);
        assert_eq!(map.bpm_at(Beats::new(9)), 90.0);
        map.set_tempo(100.0);
        assert_eq!(map, TempoMap::new(100.0, TimeSignature::FOUR_FOUR));
        // The starting tempo can be changed like any other.
        map.insert_tempo(Beats::new(-4), 110.0);
        assert_eq!(map, TempoMap::new(110.0, TimeSignature::FOUR_FOUR));
    }

    #[test]
    fn bars_follow_the_time_signature() {
        let mut map = TempoMap::default();
        assert_eq!(map.bar_start(3), Beats::new(12));
        assert_eq!(
            map.bar_at(Beats::new(13) + beats(1, 3)),
            BarPosition {
                bar: 3,
                beat: 1,
                offset: beats(1, 3),
            }
        );
        // Before the song starts.
        assert_eq!(
            map.bar_at(beats(-1, 2)),
            BarPosition {
                bar: -1,
                beat: 3,
                offset: beats(1, 2),
            }
        );

        // Two bars of 4/4, then 6/8 from bar 2 and 7/8 from bar 4.
        map.insert_signature(4, sig(7, 8));
        map.insert_signature(2, sig(6, 8));
        assert_eq!(map.bar_start(2), Beats::new(8));
        assert_eq!(map.bar_start(4), Beats::new(14));
        assert_eq!(map.bar_start(5), Beats::new(14) + beats(7, 2));
        assert_eq!(map.signatures()[2].at(), Beats::new(14));
        assert_eq!(map.signature_at(Beats::new(13)), sig(6, 8));
        // Bar 3 starts at beat 11, and counts eighth notes.
        assert_eq!(
            map.bar_at(Beats::new(12) + beats(1, 4)),
            BarPosition {
                bar: 3,
                beat: 2,
                offset: beats(1, 4),
            }
        );
        for bar in -3..10 {
            let start = map.bar_start(bar);
            assert_eq!(
                map.bar_at(start),
                BarPosition {
                    bar,
                    beat: 0,
                    offset: Beats::ZERO,
                }
            );
        }

        // Changing an earlier signature moves the later bars.
        map.insert_signature(0, sig(3, 4));
        assert_eq!(map.bar_start(2), Beats::new(6));
        assert_eq!(map.bar_start(4), Beats::new(12));
        map.set_signature(sig(5, 4));
        assert_eq!(map.signatures().len(), 1);
        assert_eq!(map.bar_start(4), Beats::new(20));
    }

    #[test]
    fn the_metronome_counts_the_signatures_beats() {
        let mut map = TempoMap::default();
        map.insert_signature(1, sig(6, 8));
        map.insert_signature(2, sig(2, 4));
        let counted: Vec<(f64, i64, u32)> = map
            .beats_in(BeatRange::new(beats(5, 2), Beats::new(10)))
            .map(|b| (b.at.to_f64(), b.bar, b.beat))
            .collect();
        assert_eq!(
            counted,
            [
                (3.0, 0, 3),
                (4.0, 1, 0),
                (4.5, 1, 1),
                (5.0, 1, 2),
                (5.5, 1, 3),
                (6.0, 1, 4),
                (6.5, 1, 5),
                (7.0, 2, 0),
                (8.0, 2, 1),
                (9.0, 3, 0),
            ]
        );
        // A range that starts on a beat includes it.
        let first = map.beats_in(BeatRange::new(Beats::new(4), Beats::new(5)));
        assert_eq!(first.count(), 2);
        assert_eq!(map.beats_in(BeatRange::default()).count(), 0);
        // Before the song starts.
        let count_in: Vec<i64> = TempoMap::default()
            .beats_in(BeatRange::new(Beats::new(-4), Beats::ZERO))
            .map(|b| b.bar)
            .collect();
        assert_eq!(count_in, [-1; 4]);
    }

    #[test]
    fn ranges() {
        let range = BeatRange::new(Beats::new(2), Beats::new(5));
        assert_eq!(range.len(), Beats::new(3));
        assert!(range.contains(Beats::new(2)) && !range.contains(Beats::new(5)));
        let backwards = BeatRange::new(Beats::new(5), Beats::new(2));
        assert!(backwards.is_empty());
        assert_eq!(backwards.len(), Beats::ZERO);
    }
}

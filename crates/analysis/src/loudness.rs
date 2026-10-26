//! Loudness as ITU-R BS.1770-4 measures it and EBU R 128 reports it: the
//! integrated loudness of a whole file, its loudness range (EBU Tech 3342),
//! the loudest momentary (0.4 s) and short-term (3 s) loudness, and the
//! true peak. A [`Meter`] takes the samples as they come, so a long file
//! never has to be in memory, and [`Meter::finish`] gives its [`Levels`],
//! with the sample peak, RMS level, DC offset and clipped samples too.
//!
//! Loudness is the mean square of K-weighted samples, summed over the
//! channels. Momentary and short-term loudness slide along in steps of
//! 10 ms, as a file-based meter's must to pass EBU Tech 3341's tests; the
//! integrated loudness and the loudness range gate blocks that start every
//! 100 ms, as BS.1770 and Tech 3342 have them. A block that the end of the
//! file cuts short doesn't count.
//!
//! The true peak is the loudest of the samples and of the points between
//! them that interpolating 4 times over finds (twice over from 96 kHz, and
//! not at all from 192 kHz), with a Kaiser-windowed sinc of 24 taps. Points
//! between samples read the highest frequencies a little low: by 0.01 dB
//! at 40% of the sample rate, and up to 1.7 dB at 45%.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// Loudness below this is silence, which gating leaves out.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Integrated loudness leaves out blocks this far below the loudness of
/// those above the absolute gate, and loudness range short-term loudness
/// this far below.
const RELATIVE_GATE_LU: f64 = -10.0;
const RANGE_GATE_LU: f64 = -20.0;
/// Loudness range spans these percentiles of the gated short-term
/// loudness.
const RANGE_LOW: f64 = 0.10;
const RANGE_HIGH: f64 = 0.95;
/// Loudness is measured in blocks of 10 ms: momentary loudness over 40 of
/// them, short-term over 300, and gating in steps of 10.
pub const BLOCKS_PER_SECOND: u32 = 100;
const MOMENTARY_BLOCKS: usize = 40;
const SHORT_TERM_BLOCKS: usize = 300;
const STEP_BLOCKS: u64 = 10;
/// Gating blocks are 4 steps long, and short-term loudness 30.
const GATING_STEPS: usize = 4;
const SHORT_TERM_STEPS: usize = 30;
/// The true peak's interpolation takes this many samples either side of a
/// point, through a Kaiser window of this β.
const HALF_TAPS: usize = 12;
const TAPS: usize = 2 * HALF_TAPS;
const KAISER_BETA: f64 = 8.0;
/// Filter states this small are flushed to zero now and then, as
/// subnormal floats are slow.
const TINY: f64 = 1e-30;

/// The loudness, in LUFS, of a mean square of K-weighted samples summed
/// over channels: BS.1770's −0.691 dB takes off what K-weighting adds at
/// 1 kHz. Silence has none.
pub fn energy_to_lufs(energy: f64) -> Option<f64> {
    (energy > 0.0).then(|| -0.691 + 10.0 * energy.log10())
}

/// The frame that 10 ms block `block` starts at, at `sample_rate`. Blocks
/// are as near 10 ms as whole frames make them.
pub fn block_start(sample_rate: u32, block: u64) -> u64 {
    (block * u64::from(sample_rate) + u64::from(BLOCKS_PER_SECOND / 2))
        / u64::from(BLOCKS_PER_SECOND)
}

/// What a [`Meter`] measured.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Levels {
    pub frames: u64,
    /// The integrated loudness: none without a block of 0.4 s above the
    /// absolute gate of −70 LUFS.
    pub integrated_lufs: Option<f64>,
    /// The loudness range: none without 3 s above the absolute gate.
    pub range_lu: Option<f64>,
    /// The loudest momentary and short-term loudness, of sounds at least
    /// 0.4 s and 3 s long.
    pub momentary_max_lufs: Option<f64>,
    pub short_term_max_lufs: Option<f64>,
    /// None for silence, as with all the levels in dB.
    pub sample_peak_dbfs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    /// The RMS level of every sample: a full-scale sine reads −3.01 dBFS.
    pub rms_dbfs: Option<f64>,
    /// The mean of the channel whose mean is furthest from 0.
    pub dc_offset: f64,
    /// The samples at full scale or beyond: see [`Meter::with_full_scale`].
    pub clipped_samples: u64,
    /// Each 10 ms block, the last one cut short if the file ends within
    /// it, if the meter [kept them](Meter::keeping_blocks).
    pub blocks: Vec<Block>,
}

/// A 10 ms block, as a [`Meter`] keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Block {
    /// The mean square of its K-weighted samples, summed over channels:
    /// see [`energy_to_lufs`].
    pub energy: f64,
    /// Its loudest sample.
    pub peak: f32,
}

/// Measures loudness and levels from samples as they come.
#[derive(Debug)]
pub struct Meter {
    sample_rate: u32,
    channels: usize,
    weights: Vec<f64>,
    /// Each channel's K-weighting: a high shelf, then a high-pass.
    filters: Vec<[Biquad; 2]>,
    full_scale: f64,
    frames: u64,
    /// The block being filled: the sum of its K-weighted squares, weighted
    /// by channel, its loudest sample, its index, and the frame it ends at.
    sum: f64,
    block_peak: f64,
    block: u64,
    block_end: u64,
    /// The sums of the last blocks, newest last, for momentary and
    /// short-term loudness.
    recent: VecDeque<f64>,
    momentary_max: f64,
    short_term_max: f64,
    /// The sum of the step being filled, and each step's before it.
    step: f64,
    steps: Vec<f64>,
    peak: f64,
    squares: f64,
    /// Each channel's sum, for its mean.
    sums: Vec<f64>,
    clipped: u64,
    true_peak: Option<TruePeak>,
    kept: Option<Vec<Block>>,
}

impl Meter {
    /// A meter for `channels` channels at `sample_rate`. One to three
    /// channels count the same; four are L, R, Ls and Rs, five L, R, C, Ls
    /// and Rs, and six or more start L, R, C, LFE, Ls and Rs, as WAV files
    /// order them. The LFE doesn't count, and surrounds count 1.41 times
    /// (+1.5 dB).
    ///
    /// # Panics
    ///
    /// If `channels` or `sample_rate` is 0.
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        assert!(sample_rate > 0, "a meter needs a sample rate");
        assert!(channels > 0, "a meter needs a channel");
        let weights = (0..channels)
            .map(|channel| match (channels, channel) {
                (4, 2 | 3) | (5, 3 | 4) | (6.., 4 | 5) => 1.41,
                (6.., 3) => 0.0,
                _ => 1.0,
            })
            .collect();
        Self {
            sample_rate,
            channels,
            weights,
            filters: vec![k_weighting(sample_rate); channels],
            full_scale: 1.0,
            frames: 0,
            sum: 0.0,
            block_peak: 0.0,
            block: 0,
            block_end: block_start(sample_rate, 1),
            recent: VecDeque::with_capacity(SHORT_TERM_BLOCKS + 1),
            momentary_max: 0.0,
            short_term_max: 0.0,
            step: 0.0,
            steps: Vec::new(),
            peak: 0.0,
            squares: 0.0,
            sums: vec![0.0; channels],
            clipped: 0,
            true_peak: Some(TruePeak::new(sample_rate, channels)),
            kept: None,
        }
    }

    /// Counts samples as clipped from `level` up, either way, rather than
    /// from 1.0: from an integer encoding's largest sample, say, which is a
    /// step below 1.0.
    pub fn with_full_scale(mut self, level: f64) -> Self {
        self.full_scale = level;
        self
    }

    /// Keeps each 10 ms block's loudness and peak in the [`Levels`], to
    /// find stretches by.
    pub fn keeping_blocks(mut self) -> Self {
        self.kept = Some(Vec::new());
        self
    }

    /// Leaves out the true peak, which takes longer to measure than the
    /// rest: the levels have none.
    pub fn without_true_peak(mut self) -> Self {
        self.true_peak = None;
        self
    }

    /// Adds `samples`: whole frames, the channels interleaved.
    ///
    /// # Panics
    ///
    /// If `samples` isn't whole frames.
    pub fn add(&mut self, samples: &[f64]) {
        let channels = self.channels;
        assert!(
            samples.len().is_multiple_of(channels),
            "{} samples aren't whole frames of {channels} channels",
            samples.len()
        );
        for (i, frame) in samples.chunks_exact(channels).enumerate() {
            let mut weighted = 0.0;
            for (c, &sample) in frame.iter().enumerate() {
                let [shelf, high_pass] = &mut self.filters[c];
                let filtered = high_pass.process(shelf.process(sample));
                weighted += self.weights[c] * filtered * filtered;
                let size = sample.abs();
                self.block_peak = self.block_peak.max(size);
                self.squares += sample * sample;
                self.sums[c] += sample;
                if size >= self.full_scale {
                    self.clipped += 1;
                }
            }
            self.sum += weighted;
            self.frames += 1;
            if self.frames == self.block_end {
                self.end_block();
            }
            if i % 4096 == 4095 {
                self.flush_filters();
            }
        }
        self.flush_filters();
        if let Some(true_peak) = &mut self.true_peak {
            // The true peak is at least the sample peak, which saves
            // working out points that can't be louder.
            true_peak.peak = true_peak.peak.max(self.peak.max(self.block_peak));
            true_peak.add(samples);
        }
    }

    fn flush_filters(&mut self) {
        for filters in &mut self.filters {
            for filter in filters {
                filter.flush();
            }
        }
    }

    /// Ends the block being filled, which is whole.
    fn end_block(&mut self) {
        let sum = std::mem::take(&mut self.sum);
        let peak = std::mem::take(&mut self.block_peak);
        self.peak = self.peak.max(peak);
        let start = block_start(self.sample_rate, self.block);
        if let Some(kept) = &mut self.kept {
            kept.push(Block {
                energy: sum / (self.block_end - start) as f64,
                peak: peak as f32,
            });
        }
        self.recent.push_back(sum);
        if self.recent.len() > SHORT_TERM_BLOCKS {
            self.recent.pop_front();
        }
        self.block += 1;
        // The last momentary and short-term windows, ending here.
        let mean = |blocks: usize| {
            let start = block_start(self.sample_rate, self.block - blocks as u64);
            let sum: f64 = self.recent.iter().rev().take(blocks).sum();
            sum / (self.block_end - start) as f64
        };
        if self.recent.len() >= MOMENTARY_BLOCKS {
            self.momentary_max = self.momentary_max.max(mean(MOMENTARY_BLOCKS));
        }
        if self.recent.len() == SHORT_TERM_BLOCKS {
            self.short_term_max = self.short_term_max.max(mean(SHORT_TERM_BLOCKS));
        }
        self.step += sum;
        if self.block.is_multiple_of(STEP_BLOCKS) {
            self.steps.push(std::mem::take(&mut self.step));
        }
        self.block_end = block_start(self.sample_rate, self.block + 1);
    }

    /// What the meter measured.
    pub fn finish(mut self) -> Levels {
        // A block cut short counts for the levels, not the loudness.
        let start = block_start(self.sample_rate, self.block);
        if self.frames > start {
            self.peak = self.peak.max(self.block_peak);
            if let Some(kept) = &mut self.kept {
                kept.push(Block {
                    energy: self.sum / (self.frames - start) as f64,
                    peak: self.block_peak as f32,
                });
            }
        }
        let gating = self.windows(GATING_STEPS);
        let short_terms = self.windows(SHORT_TERM_STEPS);
        let samples = self.frames as f64 * self.channels as f64;
        let decibels = |level: f64| (level > 0.0).then(|| 20.0 * level.log10());
        let dc_offset = (self.sums.iter())
            .map(|sum| {
                if self.frames > 0 {
                    sum / self.frames as f64
                } else {
                    0.0
                }
            })
            .fold(0.0, |furthest: f64, mean| {
                if mean.abs() > furthest.abs() {
                    mean
                } else {
                    furthest
                }
            });
        let true_peak = (self.true_peak).map(|true_peak| true_peak.finish().max(self.peak));
        Levels {
            frames: self.frames,
            integrated_lufs: integrated(&gating),
            range_lu: range(&short_terms),
            momentary_max_lufs: energy_to_lufs(self.momentary_max),
            short_term_max_lufs: energy_to_lufs(self.short_term_max),
            sample_peak_dbfs: decibels(self.peak),
            true_peak_dbtp: true_peak.and_then(decibels),
            rms_dbfs: (self.squares > 0.0).then(|| 10.0 * (self.squares / samples).log10()),
            dc_offset,
            clipped_samples: self.clipped,
            blocks: self.kept.unwrap_or_default(),
        }
    }

    /// The mean squares of windows of `steps` steps, one starting at each.
    fn windows(&self, steps: usize) -> Vec<f64> {
        let blocks = steps as u64 * STEP_BLOCKS;
        (0..(self.steps.len() + 1).saturating_sub(steps))
            .map(|first| {
                let sum: f64 = self.steps[first..first + steps].iter().sum();
                let start = block_start(self.sample_rate, first as u64 * STEP_BLOCKS);
                let end = block_start(self.sample_rate, first as u64 * STEP_BLOCKS + blocks);
                sum / (end - start) as f64
            })
            .collect()
    }
}

/// Loudness from a mean square, as gating compares it: silence is the
/// quietest.
fn lufs(energy: f64) -> f64 {
    energy_to_lufs(energy).unwrap_or(f64::NEG_INFINITY)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// The integrated loudness of gating blocks of mean squares `blocks`.
fn integrated(blocks: &[f64]) -> Option<f64> {
    let loud: Vec<f64> = (blocks.iter().copied())
        .filter(|&block| lufs(block) > ABSOLUTE_GATE_LUFS)
        .collect();
    if loud.is_empty() {
        return None;
    }
    let gate = lufs(mean(&loud)) + RELATIVE_GATE_LU;
    // The loudest block is above the mean, so some always pass.
    let gated: Vec<f64> = loud
        .into_iter()
        .filter(|&block| lufs(block) > gate)
        .collect();
    energy_to_lufs(mean(&gated))
}

/// The loudness range of short-term mean squares `windows`, as Tech
/// 3342's reference code has it.
fn range(windows: &[f64]) -> Option<f64> {
    let loud: Vec<f64> = (windows.iter().copied())
        .filter(|&window| lufs(window) >= ABSOLUTE_GATE_LUFS)
        .collect();
    if loud.is_empty() {
        return None;
    }
    let gate = lufs(mean(&loud)) + RANGE_GATE_LU;
    let mut gated: Vec<f64> = (loud.into_iter().map(lufs))
        .filter(|&loudness| loudness >= gate)
        .collect();
    gated.sort_by(f64::total_cmp);
    let percentile = |at: f64| gated[((gated.len() - 1) as f64 * at).round() as usize];
    Some(percentile(RANGE_HIGH) - percentile(RANGE_LOW))
}

/// A second-order filter, in transposed direct form II.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    state: [f64; 2],
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.state[0];
        self.state[0] = self.b[1] * x - self.a[0] * y + self.state[1];
        self.state[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    fn flush(&mut self) {
        for state in &mut self.state {
            if state.abs() < TINY {
                *state = 0.0;
            }
        }
    }
}

/// BS.1770's K-weighting at `sample_rate`: a high shelf of about +4 dB
/// from 1.5 kHz, for the head, then the RLB high-pass. BS.1770 gives the
/// coefficients for 48 kHz only; these are from the analogue filters they
/// come from, through the bilinear transform, as libebur128 derives them,
/// and match BS.1770's at 48 kHz.
fn k_weighting(sample_rate: u32) -> [Biquad; 2] {
    let rate = f64::from(sample_rate);
    let (f0, gain_db, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        state: [0.0; 2],
    };
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        state: [0.0; 2],
    };
    [shelf, high_pass]
}

/// The loudest point between samples, interpolated.
#[derive(Debug)]
struct TruePeak {
    /// The taps for each point between two samples, none without
    /// oversampling.
    phases: Vec<[f64; TAPS]>,
    /// The most a phase's taps make of samples no louder than 1.
    gain: f64,
    /// Each channel's samples still to interpolate between, with
    /// `HALF_TAPS` before them: the next point is between the samples at
    /// `HALF_TAPS - 1` and `HALF_TAPS`.
    pending: Vec<Vec<f64>>,
    peak: f64,
}

impl TruePeak {
    fn new(sample_rate: u32, channels: usize) -> Self {
        let factor = match sample_rate {
            ..96_000 => 4,
            96_000..192_000 => 2,
            _ => 1,
        };
        let phases: Vec<[f64; TAPS]> = (1..factor)
            .map(|phase| taps(f64::from(phase) / f64::from(factor)))
            .collect();
        let gain = (phases.iter())
            .map(|taps| taps.iter().map(|tap| tap.abs()).sum::<f64>())
            .fold(0.0, f64::max);
        Self {
            phases,
            gain,
            // Silence before the start.
            pending: vec![vec![0.0; HALF_TAPS]; channels],
            peak: 0.0,
        }
    }

    fn add(&mut self, samples: &[f64]) {
        if self.phases.is_empty() {
            return;
        }
        let channels = self.pending.len();
        for (c, pending) in self.pending.iter_mut().enumerate() {
            pending.extend(samples.iter().skip(c).step_by(channels));
        }
        for c in 0..channels {
            self.scan(c);
        }
    }

    /// Interpolates channel `c` as far as its samples go.
    fn scan(&mut self, c: usize) {
        let samples = &self.pending[c];
        let Some(points) = (samples.len() + 1).checked_sub(TAPS) else {
            return;
        };
        let mut peak = self.peak;
        for first in (0..points).step_by(64) {
            let last = (first + 64).min(points);
            // Points no louder than the peak so far needn't be worked out.
            let loudest =
                (samples[first..last + TAPS - 1].iter()).fold(0.0, |m: f64, s| m.max(s.abs()));
            if loudest * self.gain <= peak {
                continue;
            }
            for point in first..last {
                let window = &samples[point..point + TAPS];
                for taps in &self.phases {
                    let value: f64 = window.iter().zip(taps).map(|(s, t)| s * t).sum();
                    peak = peak.max(value.abs());
                }
            }
        }
        self.peak = peak;
        self.pending[c].drain(..points);
    }

    /// The true peak, with silence after the end.
    fn finish(mut self) -> f64 {
        if !self.phases.is_empty() {
            for c in 0..self.pending.len() {
                self.pending[c].extend([0.0; HALF_TAPS]);
                self.scan(c);
            }
        }
        self.peak
    }
}

/// The taps that interpolate the point `offset` of the way from one sample
/// to the next, from the `HALF_TAPS` samples before it and after: a sinc
/// through a Kaiser window, scaled to keep DC as it is.
fn taps(offset: f64) -> [f64; TAPS] {
    let half = HALF_TAPS as f64;
    let mut taps = [0.0; TAPS];
    for (i, tap) in taps.iter_mut().enumerate() {
        // How far the tap's sample is from the point.
        let t = offset + half - 1.0 - i as f64;
        let sinc = if t == 0.0 {
            1.0
        } else {
            (PI * t).sin() / (PI * t)
        };
        let window = bessel_i0(KAISER_BETA * (1.0 - (t / half).powi(2)).max(0.0).sqrt())
            / bessel_i0(KAISER_BETA);
        *tap = sinc * window;
    }
    let sum: f64 = taps.iter().sum();
    taps.map(|tap| tap / sum)
}

/// The modified Bessel function of the first kind, of order 0.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..100 {
        term *= (x / 2.0 / f64::from(k)).powi(2);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// A stereo 1 kHz sine in phase in both channels, in stretches of
    /// seconds at a peak level in dBFS each, interleaved: Tech 3341's test
    /// signals. A level of none is silence.
    fn tones(stretches: &[(f64, Option<f64>)]) -> Vec<f64> {
        channels_of_tones(2, stretches, |_| 0.0)
    }

    /// `channels` channels of tones, each channel's level `offset(channel)`
    /// dB from the stretch's.
    fn channels_of_tones(
        channels: usize,
        stretches: &[(f64, Option<f64>)],
        offset: impl Fn(usize) -> f64,
    ) -> Vec<f64> {
        let mut samples = Vec::new();
        let mut frame = 0u64;
        for &(seconds, level) in stretches {
            let frames = (seconds * f64::from(RATE)).round() as u64;
            for _ in 0..frames {
                let sine = (2.0 * PI * 1000.0 * frame as f64 / f64::from(RATE)).sin();
                for c in 0..channels {
                    let gain = level.map_or(0.0, |level| 10f64.powf((level + offset(c)) / 20.0));
                    samples.push(gain * sine);
                }
                frame += 1;
            }
        }
        samples
    }

    fn measure(channels: usize, samples: &[f64]) -> Levels {
        measure_with(Meter::new(RATE, channels), samples)
    }

    fn measure_with(mut meter: Meter, samples: &[f64]) -> Levels {
        let channels = meter.channels;
        // In uneven pieces, as files are read.
        let mut rest = samples;
        for piece in [7, 4410, 65_536, 1].iter().cycle() {
            let take = (piece * channels).min(rest.len());
            meter.add(&rest[..take]);
            rest = &rest[take..];
            if rest.is_empty() {
                break;
            }
        }
        meter.finish()
    }

    #[track_caller]
    fn near(value: Option<f64>, expected: f64, tolerance: f64) {
        let value = value.expect("a value");
        assert!(
            (value - expected).abs() <= tolerance,
            "{value} isn't {expected} ±{tolerance}"
        );
    }

    #[test]
    fn the_filters_are_bs_1770_s_at_48_khz() {
        let [shelf, high_pass] = k_weighting(48_000);
        let expected = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
            -1.690_659_293_182_41,
            0.732_480_774_215_85,
        ];
        let got = [shelf.b[0], shelf.b[1], shelf.b[2], shelf.a[0], shelf.a[1]];
        for (got, expected) in got.iter().zip(expected) {
            assert!((got - expected).abs() < 1e-9, "{got} isn't {expected}");
        }
        assert_eq!(high_pass.b, [1.0, -2.0, 1.0]);
        assert!((high_pass.a[0] - -1.990_047_454_833_98).abs() < 1e-9);
        assert!((high_pass.a[1] - 0.990_072_250_366_21).abs() < 1e-9);
    }

    #[test]
    fn steady_tones_read_their_level() {
        // Tech 3341's cases 1 and 2.
        for level in [-23.0, -33.0] {
            let levels = measure(2, &tones(&[(20.0, Some(level))]));
            near(levels.integrated_lufs, level, 0.1);
            near(levels.momentary_max_lufs, level, 0.1);
            near(levels.short_term_max_lufs, level, 0.1);
            near(levels.sample_peak_dbfs, level, 0.01);
            near(levels.true_peak_dbtp, level, 0.01);
            // A sine's RMS is 3 dB below its peak.
            near(levels.rms_dbfs, level - 3.01, 0.01);
            assert_eq!(levels.frames, 20 * 48_000);
            assert_eq!(levels.clipped_samples, 0);
            assert!(levels.dc_offset.abs() < 1e-6);
        }
    }

    #[test]
    fn gating_leaves_out_what_s_quiet() {
        // Tech 3341's cases 3, 4 and 5.
        let cases: [&[(f64, Option<f64>)]; 3] = [
            &[
                (10.0, Some(-36.0)),
                (60.0, Some(-23.0)),
                (10.0, Some(-36.0)),
            ],
            &[
                (10.0, Some(-72.0)),
                (10.0, Some(-36.0)),
                (60.0, Some(-23.0)),
                (10.0, Some(-36.0)),
                (10.0, Some(-72.0)),
            ],
            &[
                (20.0, Some(-26.0)),
                (20.1, Some(-20.0)),
                (20.0, Some(-26.0)),
            ],
        ];
        for stretches in cases {
            let meter = Meter::new(RATE, 2).without_true_peak();
            let levels = measure_with(meter, &tones(stretches));
            near(levels.integrated_lufs, -23.0, 0.1);
            assert_eq!(levels.true_peak_dbtp, None);
        }
    }

    #[test]
    fn surround_channels_count_more() {
        // Tech 3341's case 6: L, R, C, Ls and Rs.
        let samples = channels_of_tones(5, &[(20.0, Some(0.0))], |c| match c {
            0 | 1 => -28.0,
            2 => -24.0,
            _ => -30.0,
        });
        let meter = Meter::new(RATE, 5).without_true_peak();
        near(measure_with(meter, &samples).integrated_lufs, -23.0, 0.1);
    }

    #[test]
    fn short_term_and_momentary_loudness_slide() {
        // Tech 3341's case 9: steady at −23 after 3 s, so that's its most.
        let pattern = [(1.34, Some(-20.0)), (1.66, Some(-30.0))];
        let levels = measure(2, &tones(&pattern.repeat(5)));
        near(levels.short_term_max_lufs, -23.0, 0.1);
        // Case 12, for momentary loudness after 1 s.
        let pattern = [(0.18, Some(-20.0)), (0.22, Some(-30.0))];
        let levels = measure(2, &tones(&pattern.repeat(25)));
        near(levels.momentary_max_lufs, -23.0, 0.1);
    }

    #[test]
    fn the_loudest_window_is_found_wherever_it_starts() {
        // Tech 3341's cases 10 and 13, which a file-based meter measures as
        // 20 files each.
        for i in [0, 1, 2, 7, 19] {
            let samples = tones(&[(i as f64 * 0.15, None), (3.0, Some(-23.0)), (1.0, None)]);
            near(measure(2, &samples).short_term_max_lufs, -23.0, 0.1);
            let samples = tones(&[(i as f64 * 0.02, None), (0.4, Some(-23.0)), (1.0, None)]);
            near(measure(2, &samples).momentary_max_lufs, -23.0, 0.1);
        }
    }

    #[test]
    fn loudness_range_spans_the_tones() {
        // Tech 3342's cases 1 to 4.
        let cases: [(&[f64], f64); 4] = [
            (&[-20.0, -30.0], 10.0),
            (&[-20.0, -15.0], 5.0),
            (&[-40.0, -20.0], 20.0),
            (&[-50.0, -35.0, -20.0, -35.0, -50.0], 15.0),
        ];
        for (levels, range) in cases {
            let stretches: Vec<(f64, Option<f64>)> =
                levels.iter().map(|&level| (20.0, Some(level))).collect();
            let meter = Meter::new(RATE, 2).without_true_peak();
            near(measure_with(meter, &tones(&stretches)).range_lu, range, 1.0);
        }
    }

    /// A stereo sine at `fraction` of the sample rate, of `amplitude`, at
    /// `phase` degrees, faded in and out over 10 ms.
    fn high_sine(fraction: f64, amplitude: f64, phase: f64) -> Vec<f64> {
        let frames = RATE as usize / 2;
        let fade = RATE as usize / 100;
        (0..frames)
            .flat_map(|i| {
                let envelope = (i.min(frames - 1 - i) as f64 / fade as f64).min(1.0);
                let angle = 2.0 * PI * fraction * i as f64 + phase.to_radians();
                let sample = amplitude * envelope * angle.sin();
                [sample, sample]
            })
            .collect()
    }

    #[test]
    fn true_peaks_between_samples() {
        // Tech 3341's cases 15 to 19, within +0.2 and −0.4 dB.
        for (fraction, amplitude, phase, peak) in [
            (0.25, 0.5, 0.0, -6.0),
            (0.25, 0.5, 45.0, -6.0),
            (1.0 / 6.0, 0.5, 60.0, -6.0),
            (0.125, 0.5, 67.5, -6.0),
            (0.25, 1.41, 45.0, 3.0),
        ] {
            let levels = measure(2, &high_sine(fraction, amplitude, phase));
            let true_peak = levels.true_peak_dbtp.unwrap();
            assert!(
                (peak - 0.4..=peak + 0.2).contains(&true_peak),
                "{fraction} at {phase}°: {true_peak}"
            );
        }
        // A sine at 45° has samples 3 dB below its peak.
        let levels = measure(2, &high_sine(0.25, 0.5, 45.0));
        near(levels.sample_peak_dbfs, -9.03, 0.01);
    }

    #[test]
    fn a_burst_between_samples_peaks_at_full_scale() {
        // Tech 3341's cases 20 to 23: a sine at a sixth of the sample rate,
        // with one period of a quarter of it at full scale, made 4 times
        // over and filtered to the sample rate, then taken from each of 4
        // offsets.
        let (before, period) = (24 * 200, 16);
        let fine: Vec<f64> = (0..2 * before + period)
            .map(|i| {
                let (angle, amplitude) = match i {
                    i if i < before => (2.0 * PI * i as f64 / 24.0, 0.5),
                    i if i < before + period => (2.0 * PI * (i - before) as f64 / 16.0, 1.0),
                    i => (2.0 * PI * (i - before - period) as f64 / 24.0, 0.5),
                };
                amplitude * angle.sin()
            })
            .collect();
        // A long windowed sinc, cutting off at the sample rate's Nyquist.
        let half = 256;
        let taps: Vec<f64> = (-half..=half)
            .map(|k| {
                let t = f64::from(k) / 4.0;
                let sinc = if k == 0 {
                    1.0
                } else {
                    (PI * t).sin() / (PI * t)
                };
                let window =
                    bessel_i0(12.0 * (1.0 - (f64::from(k) / f64::from(half)).powi(2)).sqrt())
                        / bessel_i0(12.0);
                sinc * window
            })
            .collect();
        let gain: f64 = taps.iter().sum();
        for offset in 0..4 {
            let samples: Vec<f64> = (offset..fine.len())
                .step_by(4)
                .flat_map(|i| {
                    let sample: f64 = (taps.iter().enumerate())
                        .filter_map(|(k, tap)| {
                            let at = (i + k).checked_sub(half as usize)?;
                            fine.get(at).map(|s| s * tap)
                        })
                        .sum::<f64>()
                        / gain;
                    [sample, sample]
                })
                .collect();
            let true_peak = measure(2, &samples).true_peak_dbtp.unwrap();
            assert!(
                (-0.4..=0.2).contains(&true_peak),
                "offset {offset}: {true_peak}"
            );
        }
    }

    #[test]
    fn silence_and_short_sounds_have_no_loudness() {
        let silence = measure(2, &vec![0.0; 2 * 48_000]);
        assert_eq!(silence.integrated_lufs, None);
        assert_eq!(silence.range_lu, None);
        assert_eq!(silence.momentary_max_lufs, None);
        assert_eq!(silence.sample_peak_dbfs, None);
        assert_eq!(silence.true_peak_dbtp, None);
        assert_eq!(silence.rms_dbfs, None);
        // Shorter than a block of 0.4 s: levels, but no loudness.
        let short = measure(2, &tones(&[(0.3, Some(-20.0))]));
        assert_eq!(short.integrated_lufs, None);
        assert_eq!(short.momentary_max_lufs, None);
        near(short.sample_peak_dbfs, -20.0, 0.01);
        // Shorter than 3 s: no short-term loudness or range.
        let short = measure(2, &tones(&[(2.0, Some(-20.0))]));
        near(short.integrated_lufs, -20.0, 0.1);
        assert_eq!(short.short_term_max_lufs, None);
        assert_eq!(short.range_lu, None);
        assert_eq!(measure(1, &[]), Levels::default());
    }

    #[test]
    fn clipping_offsets_and_blocks() {
        let mut samples = vec![0.25; 4410];
        samples[100] = 1.0;
        samples[200] = -1.5;
        samples[300] = 32_767.0 / 32_768.0;
        let mut meter = Meter::new(44_100, 1).keeping_blocks();
        meter.add(&samples);
        let levels = meter.finish();
        assert_eq!(levels.clipped_samples, 2);
        near(levels.sample_peak_dbfs, 20.0 * 1.5f64.log10(), 1e-9);
        assert!((levels.dc_offset - 0.25).abs() < 0.001);
        // Ten blocks of 441 frames.
        assert_eq!(levels.blocks.len(), 10);
        assert_eq!(levels.blocks[0].peak, 1.5);
        assert_eq!(levels.blocks[1].peak, 0.25);
        // A step below 1.0 is full scale for 16-bit integers.
        let mut meter = Meter::new(44_100, 1).with_full_scale(32_767.0 / 32_768.0);
        meter.add(&samples);
        assert_eq!(meter.finish().clipped_samples, 3);
        // A block cut short at the end is kept too.
        let mut meter = Meter::new(44_100, 2).keeping_blocks();
        meter.add(&vec![0.5; 2 * 500]);
        let blocks = meter.finish().blocks;
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].peak, 0.5);
    }

    #[test]
    fn blocks_start_as_near_10_ms_as_frames_go() {
        assert_eq!(block_start(44_100, 1), 441);
        assert_eq!(block_start(22_050, 1), 221);
        assert_eq!(block_start(22_050, 2), 441);
        assert_eq!(block_start(48_000, 100), 48_000);
        assert_eq!(block_start(11_025, 4), 441);
    }
}

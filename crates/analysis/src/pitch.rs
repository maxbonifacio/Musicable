//! Pitch marks: where each period of a sound's pitch starts, wherever it
//! has one, for Granular to fit its grains to with Follow pitch on, as
//! Live's Tones does.
//!
//! The pitch is tracked on 10 ms frames by YIN (de Cheveigné and Kawahara,
//! 2002), on the sound filtered and decimated to about 11 kHz as its
//! samples come, in the first pass. A frame has a pitch where YIN's dip is
//! deep and the frame isn't quiet, and frames with a pitch make stretches,
//! broken where the pitch jumps. Each stretch is then marked at the sound's
//! own rate, read from anywhere: the first mark at the loudest sample of
//! its first period, and each next one where the period after the last
//! mark matches the period before it best, near where that period puts
//! it, so every mark falls at the same point of its period, through
//! vibrato and glides. A stretch ends early where the periods stop
//! matching, and marking starts again after it.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::AnalysisError;
use crate::audio::{Audio, CHUNK, read_padded};

/// The lowest and highest pitches found, in hertz: from below a five-string
/// bass's low B to above a soprano's high C.
const LOWEST: f64 = 30.0;
const HIGHEST: f64 = 1500.0;
/// The rate the pitch is tracked at, about: the sound is decimated to it by
/// a whole number.
const TRACKING_RATE: f64 = 11_025.0;
/// The decimating filter passes up to this share of the tracking rate's
/// Nyquist frequency, with this many taps for each sample it decimates.
const PASSBAND: f64 = 0.85;
const TAPS_EACH: usize = 8;
/// Frames a second, about.
const FRAMES_A_SECOND: f64 = 100.0;
/// YIN's threshold: the first dip below it is the period.
const THRESHOLD: f32 = 0.15;
/// A frame whose deepest dip is no lower than this has no pitch.
const VOICED: f32 = 0.25;
/// Nor has a frame quieter than this share of the loudest frame's level,
/// 40 dB down, or than this level, whatever the loudest.
const QUIET: f32 = 0.01;
const SILENT: f32 = 3e-4;
/// A period this many times longer or shorter than the frame before's
/// starts a new stretch.
const JUMP: f32 = 1.2;
/// A stretch lasts this many frames at least.
const SHORTEST_STRETCH: usize = 3;
/// Each mark is looked for this share of a period either side of where
/// the period before puts it, two frames at least...
const SEARCH: f64 = 0.05;
/// ...and its period must match the one before at least this well, as a
/// normalised correlation...
const MATCH: f32 = 0.7;
/// ...and be no more than this share longer or shorter than the tracked
/// pitch's, or the stretch ends.
const DRIFT: f64 = 0.25;
/// The periods compared are at most this many frames long.
const LONGEST_MATCH: usize = 512;
/// A run has this many marks at least: three periods.
const FEWEST_MARKS: usize = 4;
/// The audio read at once around the marks, in frames.
const READ: usize = 1 << 16;
/// Marks are kept in this many parts of a frame.
pub const SUBFRAMES: f64 = 64.0;

/// Where a sound's pitch periods start, in the stretches of it that have a
/// pitch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PitchMarks {
    /// In order, apart.
    pub runs: Vec<PitchRun>,
}

/// One stretch of a sound with a pitch: its marks, in
/// [`SUBFRAMES`]ths of a frame, as the first and the step from each to the
/// next, a period each, so the marks of a long sound are short to keep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PitchRun {
    pub start: u64,
    pub periods: Vec<u32>,
}

impl PitchRun {
    /// The run of `marks`, frames of the sound, fractional, rising.
    fn new(marks: &[f64]) -> Self {
        let kept: Vec<u64> = marks
            .iter()
            .map(|&m| (m * SUBFRAMES).round().max(0.0) as u64)
            .collect();
        Self {
            start: kept[0],
            periods: kept.windows(2).map(|w| (w[1] - w[0]) as u32).collect(),
        }
    }

    /// Its marks, as frames of the sound, fractional: four at least, rising.
    pub fn marks(&self) -> Vec<f64> {
        let mut at = self.start;
        let mut marks = Vec::with_capacity(self.periods.len() + 1);
        marks.push(at as f64 / SUBFRAMES);
        for &period in &self.periods {
            at += u64::from(period);
            marks.push(at as f64 / SUBFRAMES);
        }
        marks
    }
}

/// The pitch marks of `audio`, read from start to end and then around its
/// stretches with a pitch, calling `step` between the pieces, as
/// [`analyse`](crate::analyse) finds them.
pub fn pitch_marks(
    audio: &mut dyn Audio,
    step: &mut dyn FnMut() -> bool,
) -> Result<PitchMarks, AnalysisError> {
    let mut step = || match step() {
        true => Ok(()),
        false => Err(AnalysisError::Stopped),
    };
    let mut tracker = Tracker::new(audio.sample_rate());
    let mut chunk = vec![0.0; CHUNK];
    let mut at = 0;
    while at < audio.frames() {
        let read = audio.read(at, &mut chunk)?;
        if read == 0 {
            break;
        }
        tracker.push(&chunk[..read]);
        at += read as u64;
        step()?;
    }
    mark(audio, &tracker.finish(), &mut step)
}

/// One frame of the pitch track.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// The period, in frames of the sound, fractional.
    period: f32,
    /// YIN's dip there: 0 for a sound that repeats exactly.
    dip: f32,
    /// The frame's RMS level.
    level: f32,
}

/// A sound's pitch, as the stretches with one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Track {
    /// Frames of the sound from one frame of the track to the next: frame
    /// `n` is centred on frame `n × hop` of the sound.
    hop: usize,
    stretches: Vec<Stretch>,
}

/// Frames of the track in a row with a pitch.
#[derive(Debug, Clone, PartialEq)]
struct Stretch {
    first: usize,
    /// Each frame's period, in frames of the sound.
    periods: Vec<f32>,
}

impl Stretch {
    /// The period at frame `frame` of the sound, between the track's
    /// frames, and as the first or the last beyond them.
    fn period_at(&self, frame: f64, hop: usize) -> f64 {
        let x = frame / hop as f64 - self.first as f64;
        let last = self.periods.len() - 1;
        if x <= 0.0 {
            return f64::from(self.periods[0]);
        }
        let k = x.floor() as usize;
        if k >= last {
            return f64::from(self.periods[last]);
        }
        let t = x - k as f64;
        f64::from(self.periods[k]) * (1.0 - t) + f64::from(self.periods[k + 1]) * t
    }
}

/// Tracks a sound's pitch as its samples come, so a long file needn't be in
/// memory at once. Its frames come out the same however the samples are
/// split.
pub(crate) struct Tracker {
    /// How many of the sound's samples make one of the decimated sound's.
    factor: usize,
    /// The filter before decimating, centred, an odd number of taps.
    taps: Vec<f32>,
    /// Samples the filter isn't done with: `input[0]` is the sound's sample
    /// `input_first`, negative for the silence before it.
    input: Vec<f32>,
    input_first: i64,
    pushed: u64,
    /// The decimated sound not yet framed for good: `work[0]` is its sample
    /// `work_first`, negative for the silence before it.
    work: Vec<f32>,
    work_first: i64,
    /// Decimated samples made: sample `k` is the filter's output centred on
    /// the sound's sample `k × factor`.
    made: i64,
    /// Decimated samples from one frame to the next.
    hop: usize,
    /// The difference function's window, and the shortest and longest
    /// periods it looks at, in decimated samples.
    window: usize,
    shortest: usize,
    longest: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    windowed: Vec<f32>,
    whole: Vec<f32>,
    spectrum: Vec<Complex32>,
    other: Vec<Complex32>,
    scratch: Vec<Complex32>,
    correlation: Vec<f32>,
    normalised: Vec<f32>,
    frames: Vec<Frame>,
}

impl Tracker {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let factor = ((rate / TRACKING_RATE).floor() as usize).max(1);
        let tracking = rate / factor as f64;
        let taps = lowpass(factor);
        let half = taps.len() / 2;
        let hop = (tracking / FRAMES_A_SECOND).round().max(1.0) as usize;
        let longest = (tracking / LOWEST).ceil() as usize;
        let shortest = ((tracking / HIGHEST).floor() as usize).max(2);
        let window = longest;
        let size = (window + longest + 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        let span = window + longest;
        Self {
            factor,
            taps,
            input: vec![0.0; half],
            input_first: -(half as i64),
            pushed: 0,
            work: vec![0.0; span / 2],
            work_first: -((span / 2) as i64),
            made: 0,
            hop,
            window,
            shortest,
            longest,
            windowed: forward.make_input_vec(),
            whole: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            other: forward.make_output_vec(),
            scratch: vec![Complex32::default(); scratch_len],
            correlation: inverse.make_output_vec(),
            normalised: vec![0.0; longest + 1],
            forward,
            inverse,
            frames: Vec::new(),
        }
    }

    /// Takes the sound's next samples.
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.input.extend_from_slice(samples);
        self.pushed += samples.len() as u64;
        self.decimate();
        self.frame_what_it_can();
    }

    /// The pitch, once every sample has come: a frame for every hop of the
    /// decimated sound, and one more.
    pub(crate) fn finish(mut self) -> Track {
        // Silence after the end, for the filter and the frames that reach
        // past it.
        let decimated = self.pushed.div_ceil(self.factor as u64) as i64;
        self.input.resize(self.input.len() + self.taps.len(), 0.0);
        while self.made < decimated {
            self.decimate();
            self.input.resize(self.input.len() + self.taps.len(), 0.0);
        }
        self.work
            .truncate((decimated - self.work_first).max(0) as usize);
        let frames = decimated as usize / self.hop + 1;
        let span = self.window + self.longest;
        while self.frames.len() < frames {
            self.work.resize(self.work.len() + span, 0.0);
            self.frame_what_it_can();
        }
        self.frames.truncate(frames);
        track(&self.frames, self.hop * self.factor)
    }

    /// Filters and decimates what the input has room for.
    fn decimate(&mut self) {
        let half = (self.taps.len() / 2) as i64;
        let available = self.input_first + self.input.len() as i64;
        let decimated = self.pushed.div_ceil(self.factor as u64) as i64;
        loop {
            let centre = self.made * self.factor as i64;
            if centre + half >= available || self.made >= decimated {
                break;
            }
            let start = (centre - half - self.input_first) as usize;
            let input = &self.input[start..start + self.taps.len()];
            let sum = input.iter().zip(&self.taps).map(|(x, t)| x * t).sum();
            self.work.push(sum);
            self.made += 1;
        }
        // What the next output needn't keep.
        let next = self.made * self.factor as i64 - half;
        let done = (next - self.input_first).clamp(0, self.input.len() as i64) as usize;
        self.input.drain(..done);
        self.input_first += done as i64;
    }

    fn frame_what_it_can(&mut self) {
        let span = self.window + self.longest;
        let mut done = 0;
        loop {
            let n = self.frames.len();
            let centre = (n * self.hop) as i64;
            let start = (centre - (span / 2) as i64 - self.work_first) as usize;
            if start + span > self.work.len() {
                break;
            }
            self.frame(start);
            done = start + self.hop;
        }
        let done = done.min(self.work.len());
        self.work.drain(..done);
        self.work_first += done as i64;
    }

    /// YIN on the frame whose span starts at `work[start]`: its cumulative
    /// mean normalised difference, the first dip below the threshold, or
    /// else the deepest, and the period there, between samples.
    fn frame(&mut self, start: usize) {
        let (window, longest) = (self.window, self.longest);
        let span = &self.work[start..start + window + longest];
        self.windowed.fill(0.0);
        self.windowed[..window].copy_from_slice(&span[..window]);
        self.whole.fill(0.0);
        self.whole[..span.len()].copy_from_slice(span);
        // Every buffer has the length the plans asked for.
        let _ = (self.forward).process_with_scratch(
            &mut self.windowed,
            &mut self.spectrum,
            &mut self.scratch,
        );
        let _ = (self.forward).process_with_scratch(
            &mut self.whole,
            &mut self.other,
            &mut self.scratch,
        );
        for (a, b) in self.spectrum.iter_mut().zip(&self.other) {
            *a = a.conj() * b;
        }
        let last = self.spectrum.len() - 1;
        self.spectrum[0].im = 0.0;
        self.spectrum[last].im = 0.0;
        let _ = (self.inverse).process_with_scratch(
            &mut self.spectrum,
            &mut self.correlation,
            &mut self.scratch,
        );
        let scale = 1.0 / self.correlation.len() as f64;
        let energy: f64 = span[..window].iter().map(|&x| f64::from(x * x)).sum();
        let mut moving = energy;
        let mut running = 0.0;
        self.normalised[0] = 1.0;
        for tau in 1..=longest {
            let (gone, come) = (span[tau - 1], span[tau - 1 + window]);
            moving += f64::from(come * come) - f64::from(gone * gone);
            let cross = f64::from(self.correlation[tau]) * scale;
            let difference = (energy + moving - 2.0 * cross).max(0.0);
            running += difference;
            self.normalised[tau] = match running > 0.0 {
                true => (difference * tau as f64 / running) as f32,
                false => 1.0,
            };
        }
        let d = &self.normalised;
        let below = (self.shortest..longest).find(|&tau| d[tau] < THRESHOLD);
        let tau = match below {
            Some(mut tau) => {
                while tau + 1 < longest && d[tau + 1] < d[tau] {
                    tau += 1;
                }
                tau
            }
            None => (self.shortest..longest)
                .min_by(|&a, &b| d[a].total_cmp(&d[b]))
                .unwrap_or(self.shortest),
        };
        let (shift, dip) = parabola(d[tau - 1], d[tau], d[tau + 1]);
        self.frames.push(Frame {
            period: ((tau as f32 + shift) * self.factor as f32),
            dip,
            level: (energy / window as f64).sqrt() as f32,
        });
    }
}

/// A windowed-sinc low-pass for decimating by `factor`, passing up to
/// [`PASSBAND`] of the new Nyquist frequency, its taps adding up to 1.
fn lowpass(factor: usize) -> Vec<f32> {
    if factor == 1 {
        return vec![1.0];
    }
    let half = TAPS_EACH * factor / 2;
    let cutoff = PASSBAND * 0.5 / factor as f64;
    let taps: Vec<f64> = (0..=2 * half)
        .map(|j| {
            let x = j as f64 - half as f64;
            let sinc = match x == 0.0 {
                true => 2.0 * cutoff,
                false => (std::f64::consts::TAU * cutoff * x).sin() / (std::f64::consts::PI * x),
            };
            // Blackman.
            let w = std::f64::consts::TAU * j as f64 / (2 * half) as f64;
            sinc * (0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos())
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter().map(|t| (t / sum) as f32).collect()
}

/// Where the parabola through three values a step apart has its turning
/// point, from the middle one, within half a step, and its value there.
fn parabola(before: f32, middle: f32, after: f32) -> (f32, f32) {
    let bend = before - 2.0 * middle + after;
    if bend.abs() < 1e-12 {
        return (0.0, middle);
    }
    let shift = (0.5 * (before - after) / bend).clamp(-0.5, 0.5);
    (shift, middle - 0.25 * (before - after) * shift)
}

/// The stretches with a pitch among `frames`, `hop` frames of the sound
/// apart: frames with a deep dip that aren't quiet, a frame without one
/// between two with nearly the same period counting as one too, each
/// period the middle of it and its neighbours', and new stretches where the
/// period jumps.
fn track(frames: &[Frame], hop: usize) -> Track {
    let loudest = frames.iter().fold(0.0f32, |a, f| a.max(f.level));
    let quiet = SILENT.max(QUIET * loudest);
    let mut voiced: Vec<bool> = (frames.iter())
        .map(|f| f.dip < VOICED && f.level > quiet && f.period > 0.0)
        .collect();
    let mut periods: Vec<f32> = frames.iter().map(|f| f.period).collect();
    let near = |a: f32, b: f32| a.max(b) / a.min(b) < JUMP;
    for n in 1..frames.len().saturating_sub(1) {
        if !voiced[n] && voiced[n - 1] && voiced[n + 1] && near(periods[n - 1], periods[n + 1]) {
            voiced[n] = true;
            periods[n] = 0.5 * (periods[n - 1] + periods[n + 1]);
        }
    }
    let mut middles = periods.clone();
    for n in 1..frames.len().saturating_sub(1) {
        if voiced[n - 1] && voiced[n] && voiced[n + 1] {
            let mut three = [periods[n - 1], periods[n], periods[n + 1]];
            three.sort_by(f32::total_cmp);
            middles[n] = three[1];
        }
    }
    let mut stretches = Vec::new();
    let mut current: Option<Stretch> = None;
    for (n, (&voiced, &period)) in voiced.iter().zip(&middles).enumerate() {
        let continues =
            (current.as_ref()).is_some_and(|s| voiced && near(*s.periods.last().unwrap(), period));
        if continues {
            current.as_mut().unwrap().periods.push(period);
            continue;
        }
        stretches.extend(
            current
                .take()
                .filter(|s| s.periods.len() >= SHORTEST_STRETCH),
        );
        if voiced {
            current = Some(Stretch {
                first: n,
                periods: vec![period],
            });
        }
    }
    stretches.extend(current.filter(|s| s.periods.len() >= SHORTEST_STRETCH));
    Track { hop, stretches }
}

/// Marks each stretch of `track` in `audio`, at the sound's own rate,
/// calling `step` between stretches and reads.
pub(crate) fn mark(
    audio: &mut dyn Audio,
    track: &Track,
    step: &mut dyn FnMut() -> Result<(), AnalysisError>,
) -> Result<PitchMarks, AnalysisError> {
    let frames = audio.frames() as f64;
    let hop = track.hop as f64;
    let mut sound = Sound::default();
    let mut runs = Vec::new();
    for stretch in &track.stretches {
        let start = ((stretch.first as f64 - 0.5) * hop).max(0.0);
        let last = stretch.first + stretch.periods.len() - 1;
        let end = ((last as f64 + 0.5) * hop).min(frames);
        let mut from = start;
        // Marking again from just after where it stopped, while there's
        // room for a run.
        while end - from > FEWEST_MARKS as f64 * stretch.period_at(from, track.hop) {
            let (marks, stopped) = run(audio, &mut sound, stretch, track.hop, from, end)?;
            if marks.len() >= FEWEST_MARKS {
                runs.push(PitchRun::new(&marks));
            }
            from = stopped.max(from + 1.0);
            step()?;
        }
    }
    Ok(PitchMarks { runs })
}

/// The marks of `stretch` from frame `from` of the sound to `end`, and where
/// they stopped: just after the last mark's period, or at `end`.
fn run(
    audio: &mut dyn Audio,
    sound: &mut Sound,
    stretch: &Stretch,
    hop: usize,
    from: f64,
    end: f64,
) -> Result<(Vec<f64>, f64), AnalysisError> {
    let mut period = stretch.period_at(from, hop);
    // The first mark: the loudest sample of the first period.
    let (first, length) = (from.ceil() as i64, (period.round() as i64).max(1));
    sound.cover(audio, first, first + length)?;
    let seed = (first..first + length)
        .max_by(|&a, &b| sound.at(a).total_cmp(&sound.at(b)))
        .unwrap_or(first);
    let mut mark = seed as f64;
    let mut marks = vec![mark];
    loop {
        let tracked = stretch.period_at(mark + period, hop);
        if mark + period > end {
            return Ok((marks, end));
        }
        let base = mark.round() as i64;
        let compared = (period.round() as usize).clamp(2, LONGEST_MATCH);
        let half = (compared / 2) as i64;
        let reach = (SEARCH * period).max(2.0);
        let (lo, hi) = (
            ((period - reach).floor() as i64).max(1),
            (period + reach).ceil() as i64,
        );
        sound.cover(audio, base - half, base + hi + 1 - half + compared as i64)?;
        let this = base - half;
        let match_at = |lag: i64| sound.matching(this, this + lag, compared);
        let Some((best, quality)) = (lo..=hi)
            .map(|lag| (lag, match_at(lag)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
        else {
            return Ok((marks, mark + period));
        };
        if quality < MATCH {
            return Ok((marks, mark + period));
        }
        let (shift, _) = parabola(match_at(best - 1), quality, match_at(best + 1));
        let lag = best as f64 + f64::from(shift);
        if (lag / tracked - 1.0).abs() > DRIFT {
            return Ok((marks, mark + period));
        }
        mark += lag;
        period = lag;
        marks.push(mark);
    }
}

/// A stretch of the sound in memory, and the running sum of its samples'
/// squares, for comparing periods.
#[derive(Default)]
struct Sound {
    first: i64,
    samples: Vec<f32>,
    /// `squares[k]` sums the squares of the samples before `samples[k]`.
    squares: Vec<f64>,
}

impl Sound {
    /// Reads the sound around frames `from` up to `to`, if it doesn't hold
    /// them all, silent before the start and past the end.
    fn cover(&mut self, audio: &mut dyn Audio, from: i64, to: i64) -> Result<(), AnalysisError> {
        let end = self.first + self.samples.len() as i64;
        if from >= self.first && to <= end {
            return Ok(());
        }
        let length = ((to - from) as usize).max(READ);
        self.first = from;
        self.samples.resize(length, 0.0);
        read_padded(audio, from, &mut self.samples)?;
        self.squares.clear();
        self.squares.push(0.0);
        let mut sum = 0.0;
        for &x in &self.samples {
            sum += f64::from(x * x);
            self.squares.push(sum);
        }
        Ok(())
    }

    fn at(&self, frame: i64) -> f32 {
        self.samples[(frame - self.first) as usize]
    }

    /// How well the `length` frames from `a` match those from `b`, as a
    /// normalised correlation: 1 for the same shape at any level.
    fn matching(&self, a: i64, b: i64, length: usize) -> f32 {
        let (a, b) = ((a - self.first) as usize, (b - self.first) as usize);
        let energy = |k: usize| self.squares[k + length] - self.squares[k];
        let denominator = (energy(a) * energy(b)).sqrt();
        if denominator <= 0.0 {
            return 0.0;
        }
        let cross = dot(&self.samples[a..a + length], &self.samples[b..b + length]);
        (cross / denominator) as f32
    }
}

/// The dot product of two equally long slices, in eight lanes.
fn dot(a: &[f32], b: &[f32]) -> f64 {
    let mut lanes = [0.0f32; 8];
    let ((a_chunks, a_tail), (b_chunks, b_tail)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for k in 0..8 {
            lanes[k] += x[k] * y[k];
        }
    }
    lanes.iter().map(|&l| f64::from(l)).sum::<f64>() + f64::from(tail)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;
    use crate::audio::Samples;

    /// A voice-like tone `seconds` long at `rate`: harmonics up to 4 kHz
    /// falling 6 dB an octave, at `hz` with vibrato of `depth`, a share of
    /// its pitch, five and a half times a second. With it, the phase of
    /// each sample, in periods.
    fn voice(rate: u32, seconds: f64, hz: f64, depth: f64) -> (Vec<f32>, Vec<f64>) {
        let frames = (seconds * f64::from(rate)) as usize;
        let mut phase = 0.0;
        let mut samples = Vec::with_capacity(frames);
        let mut phases = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = i as f64 / f64::from(rate);
            let f = hz * (1.0 + depth * (TAU * 5.5 * t).sin());
            let harmonics = (4000.0 / f) as usize;
            let x: f64 = (1..=harmonics)
                .map(|h| (TAU * h as f64 * phase).sin() / h as f64)
                .sum();
            samples.push((0.3 * x) as f32);
            phases.push(phase);
            phase += f / f64::from(rate);
        }
        (samples, phases)
    }

    fn marks_of(samples: &[f32], rate: u32) -> PitchMarks {
        let mut audio = Samples {
            samples,
            sample_rate: rate,
        };
        pitch_marks(&mut audio, &mut || true).unwrap()
    }

    /// The phase of each mark, in periods, between samples.
    fn phases_at(marks: &[f64], phases: &[f64]) -> Vec<f64> {
        marks
            .iter()
            .map(|&m| {
                let k = m.floor() as usize;
                let t = m - k as f64;
                phases[k] * (1.0 - t) + phases[k + 1] * t
            })
            .collect()
    }

    /// The largest distance of any of `phases` from their circular mean, in
    /// periods.
    fn spread(phases: &[f64]) -> f64 {
        let (s, c) = phases.iter().fold((0.0, 0.0), |(s, c), p| {
            (s + (TAU * p).sin(), c + (TAU * p).cos())
        });
        let mean = s.atan2(c) / TAU;
        phases
            .iter()
            .map(|p| {
                let d = (p - mean).rem_euclid(1.0);
                d.min(1.0 - d)
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn a_voice_with_vibrato_is_marked_at_one_point_of_each_period() {
        let rate = 48_000;
        let (samples, phases) = voice(rate, 1.5, 180.0, 0.03);
        let pitch = marks_of(&samples, rate);
        assert_eq!(pitch.runs.len(), 1, "{:?}", pitch.runs.len());
        let marks = pitch.runs[0].marks();
        let seconds = |frame: f64| frame / f64::from(rate);
        assert!(seconds(marks[0]) < 0.05, "starts at {}", seconds(marks[0]));
        let last = *marks.last().unwrap();
        assert!(seconds(last) > 1.45, "ends at {}", seconds(last));
        // A period apart: the phase moves on by one from each to the next.
        let at = phases_at(&marks, &phases);
        for (k, w) in at.windows(2).enumerate() {
            assert!(
                (w[1] - w[0] - 1.0).abs() < 0.01,
                "period {k}: {}",
                w[1] - w[0]
            );
        }
        assert!(spread(&at) < 0.02, "spread {}", spread(&at));
    }

    #[test]
    fn a_low_bass_line_and_a_high_voice_are_marked() {
        for (rate, hz) in [(44_100, 41.2), (48_000, 98.0), (44_100, 880.0)] {
            let (samples, phases) = voice(rate, 1.0, hz, 0.0);
            let pitch = marks_of(&samples, rate);
            assert_eq!(pitch.runs.len(), 1, "{hz} Hz");
            let marks = pitch.runs[0].marks();
            let at = phases_at(&marks, &phases);
            let periods = at[at.len() - 1] - at[0];
            assert!(periods > 0.9 * hz, "{hz} Hz: {periods} periods");
            assert!(spread(&at) < 0.02, "{hz} Hz: spread {}", spread(&at));
        }
    }

    #[test]
    fn noise_and_silence_have_no_pitch() {
        let mut noise = 1u32;
        let samples: Vec<f32> = (0..96_000)
            .map(|_| {
                noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                0.5 * (noise as f32 / u32::MAX as f32 - 0.5)
            })
            .collect();
        assert_eq!(marks_of(&samples, 48_000), PitchMarks::default());
        assert_eq!(marks_of(&[0.0; 48_000], 48_000), PitchMarks::default());
        assert_eq!(marks_of(&[], 48_000), PitchMarks::default());
    }

    #[test]
    fn two_notes_apart_make_two_runs() {
        let rate = 44_100;
        let (mut samples, _) = voice(rate, 0.5, 220.0, 0.0);
        samples.extend(std::iter::repeat_n(0.0, 22_050));
        let (second, _) = voice(rate, 0.5, 330.0, 0.0);
        samples.extend(second);
        let pitch = marks_of(&samples, rate);
        assert_eq!(pitch.runs.len(), 2);
        for (run, hz) in pitch.runs.iter().zip([220.0, 330.0]) {
            let marks = run.marks();
            let period = (marks[marks.len() - 1] - marks[0]) / (marks.len() - 1) as f64;
            assert!((period * hz / f64::from(rate) - 1.0).abs() < 0.002, "{hz}");
        }
        // Each within its note.
        let first_end = *pitch.runs[0].marks().last().unwrap();
        let second_start = pitch.runs[1].marks()[0];
        assert!(first_end < 22_050.0 + 100.0, "{first_end}");
        assert!(second_start >= 44_100.0, "{second_start}");
    }

    #[test]
    fn however_the_samples_come_the_track_is_the_same() {
        let rate = 44_100;
        let (samples, _) = voice(rate, 0.7, 150.0, 0.05);
        let whole = {
            let mut tracker = Tracker::new(rate);
            tracker.push(&samples);
            tracker.finish()
        };
        assert!(!whole.stretches.is_empty());
        for size in [1, 441, 1000, 16_384] {
            let mut tracker = Tracker::new(rate);
            for chunk in samples.chunks(size) {
                tracker.push(chunk);
            }
            assert_eq!(tracker.finish(), whole, "chunks of {size}");
        }
    }

    #[test]
    fn marks_keep_to_a_64th_of_a_frame() {
        let marks = [10.0, 12.5, 15.017, 17.9999];
        let kept = PitchRun::new(&marks).marks();
        for (kept, mark) in kept.iter().zip(marks) {
            assert!((kept - mark).abs() <= 0.5 / SUBFRAMES);
        }
    }
}

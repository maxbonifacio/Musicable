//! The reverb: see [`musicable_model::native::reverb`].
//!
//! A feedback delay network (Jot and Chaigne's) behind a diffuser, much as
//! Geraint Luff builds one in "Let's Write a Reverb" (ADC 2021). The
//! signal, low cut and pre-delayed, spreads over eight channels, and four
//! steps of short delays, each mixed by a Hadamard matrix, smear it into a
//! dense wash. That feeds eight delay lines, whose outputs go round again
//! through a Householder matrix. Both matrices are orthogonal, so the
//! network loses only what each line takes away on its way round: a gain
//! that makes the tail fall by 60 dB over the decay, the same per second
//! whatever the line's length, and a one-pole low pass that takes as much
//! again at the damping frequency, and more above it. The tail is the
//! lines' outputs, with the diffuser's for its first reflections.

use std::f64::consts::{LN_10, PI};

use musicable_model::native::delay::mix_levels;
use musicable_model::native::eq::HIGHEST;
use musicable_model::native::reverb::{
    DAMPING, DECAY, LOW_CUT, MIX, PREDELAY, SIZE, WIDTH, room_seconds,
};
use musicable_model::native::{NativeDevice, NativeKind, delay};

use super::delay::read;
use super::svf::{Coefficients, State};
use super::{Params, Span};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The network's channels.
const CHANNELS: usize = 8;
/// The diffuser's steps.
const STEPS: usize = 4;
/// The lines whose lengths follow the size: the diffuser's, a step's
/// channels at a time, then the network's.
const LINES: usize = STEPS * CHANNELS + CHANNELS;

/// The network's lines' lengths against the room's: the eighth roots of 2
/// to the power `k + frac(k / φ)`, spread unevenly from 1 to nearly 2 so
/// their echoes seldom line up.
const RATIOS: [f64; CHANNELS] = [1.0, 1.1505, 1.2138, 1.3964, 1.4733, 1.5543, 1.7882, 1.8866];

/// How fast a new size glides: no line's length moves by more than this
/// many frames a frame, which bends the tail's pitch by about a semitone.
const GLIDE: f64 = 1.0 / 16.0;

/// The level of the network's outputs in the tail at 50% size. Other sizes
/// make up for their echoes' coming further apart or closer together, so a
/// room of any size is about as loud as that one with the same decay.
const LATE: f64 = 0.45;
/// The level of the diffuser's outputs in the tail, its first reflections.
const EARLY: f32 = 0.2;

/// Which of the diffuser's channels each step turns upside down, by bit.
const FLIPS: [u8; STEPS] = [0b0010_1101, 0b1100_0110, 0b0101_0011, 0b1001_1010];

/// The reverb's node.
#[derive(Debug, Clone)]
pub struct Reverb {
    params: Params,
    space: Space,
}

impl Reverb {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            space: Space {
                memory: Vec::new(),
                rings: [Ring::default(); 2 + LINES],
                write: 0,
                lengths: Lengths::new(),
                absorb: Absorb::default(),
                damped: [0.0; CHANNELS],
                low_cut: [State::default(); 2],
                cut: None,
                sample_rate: 48_000.0,
                started: false,
            },
        }
    }
}

impl Node for Reverb {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.space.prepare(f64::from(config.sample_rate));
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        self.params.automate(param, automation)
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            self.space.take_over(&mut previous.space);
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self { params, space } = self;
        params.run(ctx, |span| space.render(&span, output));
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.space.reset();
    }
}

/// Where a line's frames are in the memory: `mask + 1` of them from
/// `start`.
#[derive(Debug, Clone, Copy, Default)]
struct Ring {
    start: usize,
    mask: usize,
}

/// The reverb's lines and filters.
#[derive(Debug, Clone)]
struct Space {
    /// Every line's frames, one line after another.
    memory: Vec<f32>,
    /// The pre-delay's two lines, then the [`LINES`].
    rings: [Ring; 2 + LINES],
    /// Where every line's next frame goes, before its mask.
    write: usize,
    lengths: Lengths,
    absorb: Absorb,
    /// The network's lines' low passes' memories.
    damped: [f32; CHANNELS],
    low_cut: [State; 2],
    /// The low cut's frequency and coefficients, once tuned.
    cut: Option<(f32, Coefficients)>,
    sample_rate: f64,
    /// Whether it has rendered a frame: until it has, a new size applies
    /// at once.
    started: bool,
}

impl Space {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        // Each line has room for its longest, at the biggest size, and the
        // frames around it that reading needs.
        let predelay = NativeKind::Reverb.params()[PREDELAY].max / 1000.0 * sample_rate;
        let longest = targets(100.0, sample_rate);
        let mut start = 0;
        for (r, ring) in self.rings.iter_mut().enumerate() {
            let longest = if r < 2 { predelay } else { longest[r - 2] };
            let len = (longest.ceil() as usize + 4).next_power_of_two();
            *ring = Ring {
                start,
                mask: len - 1,
            };
            start += len;
        }
        self.memory = vec![0.0; start];
        self.absorb = Absorb::default();
        self.cut = None;
    }

    /// Empties the lines and the filters, for a size set afresh.
    fn reset(&mut self) {
        self.memory.fill(0.0);
        self.write = 0;
        self.lengths = Lengths::new();
        self.damped = [0.0; CHANNELS];
        self.low_cut = [State::default(); 2];
        self.started = false;
    }

    /// Carries on from `previous`, the reverb this one replaces, so the
    /// tail rings on.
    fn take_over(&mut self, previous: &mut Space) {
        if previous.memory.len() != self.memory.len() || previous.sample_rate != self.sample_rate {
            return;
        }
        std::mem::swap(&mut self.memory, &mut previous.memory);
        self.write = previous.write;
        self.lengths = previous.lengths;
        self.damped = previous.damped;
        self.low_cut = previous.low_cut;
        self.started = previous.started;
    }

    /// Renders one span of the output, in place.
    fn render(&mut self, span: &Span<'_>, output: &mut AudioMut<'_>) {
        let values = span.values;
        let frames = span.frames.len();
        if frames == 0 {
            return;
        }
        let mut out = output.range_mut(span.frames.clone());
        let sample_rate = self.sample_rate;
        let steady_absorb = values.steady(DECAY) && values.steady(DAMPING);
        let steady_cut = values.steady(LOW_CUT);
        if steady_cut {
            self.tune(values.first(LOW_CUT));
        }
        let (predelay, decay, damping, low_cut, width, mix) = (
            values.frames(PREDELAY),
            values.frames(DECAY),
            values.frames(DAMPING),
            values.frames(LOW_CUT),
            values.frames(WIDTH),
            values.frames(MIX),
        );
        let longest_predelay = (self.rings[0].mask - 3) as f64;
        let stereo = out.channels() >= 2;
        for i in 0..frames {
            // Automation can move the size on any frame.
            let size = values.target(SIZE, i);
            self.lengths.aim(size, sample_rate, !self.started);
            self.started = true;
            let gliding = self.lengths.glide();
            if gliding || !steady_absorb || i == 0 {
                let network = self.lengths.now[STEPS * CHANNELS..].try_into().unwrap();
                self.absorb.tune(network, decay[i], damping[i], sample_rate);
            }
            if !steady_cut {
                self.tune(low_cut[i]);
            }
            let left = out.channel(0)[i];
            let x = [left, if stereo { out.channel(1)[i] } else { left }];

            // The low cut, into the pre-delay.
            let (_, cut) = self.cut.expect("tuned");
            let predelay =
                (f64::from(predelay[i]) * sample_rate / 1000.0).clamp(2.0, longest_predelay);
            let mut delayed = [0.0; 2];
            for c in 0..2 {
                let filtered = self.low_cut[c].tick(&cut, f64::from(x[c])) as f32;
                delayed[c] = self.read(c, predelay);
                self.write_to(c, filtered);
            }

            // Spread over the channels, a side to every other one, and
            // smeared by the diffuser's steps.
            let mut v: [f32; CHANNELS] = std::array::from_fn(|k| 0.5 * delayed[k % 2]);
            for (step, flips) in FLIPS.iter().enumerate() {
                for (k, sample) in v.iter_mut().enumerate() {
                    let line = step * CHANNELS + k;
                    let out = self.tap(line);
                    self.write_to(2 + line, *sample);
                    *sample = if flips >> k & 1 == 1 { -out } else { out };
                }
                v.rotate_left(step + 1);
                hadamard(&mut v);
            }

            // The network: each line's output, losing what it should, mixed
            // with the others' and the diffuser's into its input.
            let y: [f32; CHANNELS] = std::array::from_fn(|k| self.tap(STEPS * CHANNELS + k));
            let mut z = [0.0; CHANNELS];
            for k in 0..CHANNELS {
                let damped = &mut self.damped[k];
                *damped = y[k] + self.absorb.poles[k] * (*damped - y[k]);
                z[k] = self.absorb.gains[k] * *damped;
            }
            // Householder: each less a quarter of them all.
            let shared = 0.25 * z.iter().sum::<f32>();
            for k in 0..CHANNELS {
                self.write_to(2 + STEPS * CHANNELS + k, z[k] - shared + v[k]);
            }
            self.write = self.write.wrapping_add(1);

            let side = |signal: &[f32; CHANNELS], c: usize| -> f32 {
                signal.iter().skip(c).step_by(2).sum()
            };
            let late = self.absorb.level;
            let wet = [0, 1].map(|c| late * side(&y, c) + EARLY * side(&v, c));
            let mid = 0.5 * (wet[0] + wet[1]);
            let spread = 0.5 * (wet[0] - wet[1]) * width[i] / 100.0;
            let wet = [mid + spread, mid - spread];
            let [dry_level, wet_level] = mix_levels(mix[i]);
            out.channel_mut(0)[i] = dry_level * x[0] + wet_level * wet[0];
            if stereo {
                out.channel_mut(1)[i] = dry_level * x[1] + wet_level * wet[1];
            }
        }
    }

    /// Line `r`'s signal `delay` frames back.
    fn read(&self, r: usize, delay: f64) -> f32 {
        let Ring { start, mask } = self.rings[r];
        read(&self.memory[start..=start + mask], mask, self.write, delay)
    }

    /// What comes out of the end of [`LINES`] line `line`, as long as it
    /// is now.
    #[inline]
    fn tap(&self, line: usize) -> f32 {
        if self.lengths.settled() {
            let Ring { start, mask } = self.rings[2 + line];
            let back = self.lengths.whole[line];
            self.memory[start + (self.write.wrapping_sub(back) & mask)]
        } else {
            self.read(2 + line, self.lengths.now[line])
        }
    }

    /// Puts `sample` into line `r` as its next frame.
    fn write_to(&mut self, r: usize, sample: f32) {
        let Ring { start, mask } = self.rings[r];
        self.memory[start + (self.write & mask)] = sample;
    }

    /// Tunes the low cut to `freq` Hz, unless it's there already.
    fn tune(&mut self, freq: f32) {
        if matches!(self.cut, Some((tuned, _)) if tuned == freq) {
            return;
        }
        let band = delay::low_cut(f64::from(freq));
        let section = band.sections().next().expect("a 12 dB cut is one section");
        let cut = Coefficients::new(&section, band.corner(self.sample_rate), self.sample_rate);
        self.cut = Some((freq, cut));
    }
}

/// Each line's length in frames, which glides to the lengths for a new
/// size.
#[derive(Debug, Clone, Copy)]
struct Lengths {
    from: [f64; LINES],
    /// Whole frames.
    to: [f64; LINES],
    now: [f64; LINES],
    /// `to` as counts, for reading once the glide is over.
    whole: [usize; LINES],
    /// How far along the glide from `from` to `to` they are, from 0.0 to
    /// 1.0, and how far they go a frame.
    along: f64,
    step: f64,
    /// The size `to` is for.
    size: f32,
}

impl Lengths {
    /// For no size yet.
    fn new() -> Self {
        Self {
            from: [0.0; LINES],
            to: [0.0; LINES],
            now: [0.0; LINES],
            whole: [0; LINES],
            along: 1.0,
            step: 0.0,
            size: f32::NAN,
        }
    }

    /// Glides to the lengths for `size` percent, or jumps there.
    fn aim(&mut self, size: f32, sample_rate: f64, jump: bool) {
        if size == self.size && !jump {
            return;
        }
        self.size = size;
        self.from = self.now;
        self.to = targets(f64::from(size), sample_rate);
        self.whole = self.to.map(|length| length as usize);
        let furthest = self
            .from
            .iter()
            .zip(&self.to)
            .map(|(from, to)| (to - from).abs())
            .fold(0.0, f64::max);
        if jump || furthest == 0.0 {
            self.now = self.to;
            self.along = 1.0;
        } else {
            self.along = 0.0;
            self.step = GLIDE / furthest;
        }
    }

    /// Whether the lengths are where they're going, in whole frames.
    fn settled(&self) -> bool {
        self.along >= 1.0
    }

    /// Moves a frame along the glide, if there is one, and returns whether
    /// there was.
    fn glide(&mut self) -> bool {
        if self.along >= 1.0 {
            return false;
        }
        self.along = (self.along + self.step).min(1.0);
        if self.along >= 1.0 {
            self.now = self.to;
        } else {
            for ((now, from), to) in self.now.iter_mut().zip(&self.from).zip(&self.to) {
                *now = from + (to - from) * self.along;
            }
        }
        true
    }
}

/// Each line's length at `size` percent, in whole frames.
fn targets(size: f64, sample_rate: f64) -> [f64; LINES] {
    let room = room_seconds(size) * sample_rate;
    std::array::from_fn(|line| {
        let length = if line < STEPS * CHANNELS {
            // A step's delays reach across its span, one in each eighth of
            // it, at an uneven place there. The spans double, step by step,
            // up to the room's length.
            let (step, k) = (line / CHANNELS, line % CHANNELS);
            let span = room / f64::from(1 << (STEPS - 1 - step));
            let place = ((line + 1) as f64 * 0.618_033_988_749_894_9).fract();
            span * (k as f64 + place) / CHANNELS as f64
        } else {
            room * RATIOS[line - STEPS * CHANNELS]
        };
        length.round().max(2.0)
    })
}

/// What the network's lines take away on their way round, each one's gain
/// and its low pass's pole, and the level of their outputs, for the decay,
/// damping and lengths last tuned to.
#[derive(Debug, Clone, Copy, Default)]
struct Absorb {
    gains: [f32; CHANNELS],
    poles: [f32; CHANNELS],
    level: f32,
    tuned: Option<([f64; CHANNELS], f32, f32)>,
}

impl Absorb {
    /// For lines `lengths` frames long, a tail of `decay` seconds, and
    /// damping from `damping` Hz.
    fn tune(&mut self, lengths: &[f64; CHANNELS], decay: f32, damping: f32, sample_rate: f64) {
        let key = (*lengths, decay, damping);
        if self.tuned == Some(key) {
            return;
        }
        let decay = f64::from(decay).max(1e-3) * sample_rate;
        // Kept in its range, as far as the sample rate allows: at 0 Hz the
        // low pass would hold its last value for ever.
        let lowest = NativeKind::Reverb.params()[DAMPING].min;
        let damping = f64::from(damping).max(lowest).min(HIGHEST * sample_rate);
        let omega = 2.0 * PI * damping / sample_rate;
        let cos = omega.cos();
        for (k, &length) in lengths.iter().enumerate() {
            // 60 dB over the decay: 3 of the gain's decades.
            let gain = (-3.0 * LN_10 * length / decay).exp();
            self.gains[k] = gain as f32;
            self.poles[k] = damping_pole(gain, cos) as f32;
        }
        // The tail's power is the first pass's over 1 - g², for the lines'
        // gain g on the way round.
        let power = |length: f64| 1.0 - (-6.0 * LN_10 * length / decay).exp();
        let mean = lengths.iter().sum::<f64>() / CHANNELS as f64;
        let half = room_seconds(50.0) * sample_rate * RATIOS.iter().sum::<f64>() / CHANNELS as f64;
        self.level = (LATE * (power(mean) / power(half)).sqrt()) as f32;
        self.tuned = Some(key);
    }
}

/// The pole of the one-pole low pass, `(1 - p) / (1 - p z⁻¹)`, whose gain
/// is `gain`, from 0 to 1, at the frequency of `cos` = cos ω, ω in radians
/// a frame: so a line losing `gain` on its way round loses as much again
/// there, and its tail falls twice as fast.
fn damping_pole(gain: f64, cos: f64) -> f64 {
    let g2 = gain * gain;
    if g2 >= 1.0 {
        return 0.0;
    }
    // |H|² = (1 - p)² / (1 - 2p cos ω + p²) = g² is p² - 2βp + 1 = 0,
    // whose root below 1 is 1 / (β + √(β² - 1)).
    let beta = (1.0 - g2 * cos) / (1.0 - g2);
    1.0 / (beta + (beta * beta - 1.0).max(0.0).sqrt())
}

/// Mixes the channels by a Hadamard matrix, scaled to keep their power.
fn hadamard(v: &mut [f32; CHANNELS]) {
    let mut half = 1;
    while half < CHANNELS {
        for block in (0..CHANNELS).step_by(2 * half) {
            for i in block..block + half {
                let (a, b) = (v[i], v[i + half]);
                v[i] = a + b;
                v[i + half] = a - b;
            }
        }
        half *= 2;
    }
    let scale = 1.0 / (CHANNELS as f32).sqrt();
    v.iter_mut().for_each(|x| *x *= scale);
}

#[cfg(test)]
mod tests {
    use musicable_model::automation::Point;
    use musicable_model::time::Beats;

    use super::*;
    use crate::devices::testing::{Run, db, gain_at, modulation, param};

    const FS: f64 = 48_000.0;

    /// A reverb set as `settings`, the others at their defaults.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut reverb = NativeDevice::new(NativeKind::Reverb);
        for &(param, value) in settings {
            reverb.set(param, value);
        }
        reverb
    }

    fn run(settings: &[(usize, f64)]) -> Run {
        Run::new(Box::new(Reverb::new(&device(settings))))
    }

    /// Only the tail, from a low cut that lets nearly everything by.
    fn wet(settings: &[(usize, f64)]) -> Vec<(usize, f64)> {
        let mut wet = vec![(MIX, 100.0), (LOW_CUT, 20.0)];
        wet.extend_from_slice(settings);
        wet
    }

    /// `frames` of silence on both sides, but for a click on frame 0.
    fn click(frames: usize) -> [Vec<f32>; 2] {
        let mut click = vec![0.0; frames];
        click[0] = 1.0;
        [click.clone(), click]
    }

    /// A sine at `freq` Hz for `seconds`, faded in and out over 10 ms, and
    /// then `then` seconds of silence, at 48 kHz.
    fn burst(freq: f64, seconds: f64, then: f64) -> Vec<f32> {
        let frames = (seconds * FS) as usize;
        let fade = 0.01 * FS;
        (0..frames + (then * FS) as usize)
            .map(|i| {
                if i >= frames {
                    return 0.0;
                }
                let edge = (i.min(frames - i) as f64 / fade).min(1.0);
                ((2.0 * PI * freq * i as f64 / FS).sin() * 0.5 * edge) as f32
            })
            .collect()
    }

    /// Something to reverberate that changes all the time.
    fn noise(frames: usize) -> [Vec<f32>; 2] {
        let wave = |seed: usize| -> Vec<f32> {
            (0..frames)
                .map(|i| ((i * 7919 + seed) % 2000) as f32 / 2000.0 - 0.5)
                .collect()
        };
        [wave(0), wave(1013)]
    }

    /// Each frame's power over both channels.
    fn power(output: &[Vec<f32>]) -> Vec<f64> {
        output[0]
            .iter()
            .zip(&output[1])
            .map(|(&l, &r)| f64::from(l).powi(2) + f64::from(r).powi(2))
            .collect()
    }

    /// `signal` through the cookbook's band pass at `freq` Hz, an octave
    /// or so wide, with a peak gain of 1.
    fn band(signal: &[f32], freq: f64, sample_rate: f64) -> Vec<f64> {
        let w = 2.0 * PI * freq / sample_rate;
        let alpha = w.sin() / (2.0 * 1.4);
        let a0 = 1.0 + alpha;
        let (b0, b2) = (alpha / a0, -alpha / a0);
        let (a1, a2) = (-2.0 * w.cos() / a0, (1.0 - alpha) / a0);
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        signal
            .iter()
            .map(|&x| {
                let x = f64::from(x);
                let y = b0 * x + b2 * x2 - a1 * y1 - a2 * y2;
                (x2, x1, y2, y1) = (x1, x, y1, y);
                y
            })
            .collect()
    }

    /// How long a signal whose power is `power` takes to fall by 60 dB:
    /// the slope of its energy decay curve (Schroeder's backward integral)
    /// from 5 to 35 dB down, fitted by least squares.
    fn decay_time(power: &[f64], sample_rate: f64) -> f64 {
        let mut curve = vec![0.0; power.len()];
        let mut sum = 0.0;
        for i in (0..power.len()).rev() {
            sum += power[i];
            curve[i] = sum;
        }
        let points: Vec<(f64, f64)> = curve
            .iter()
            .enumerate()
            .map(|(i, &e)| (i as f64 / sample_rate, 10.0 * (e / curve[0]).log10()))
            .filter(|&(_, db)| (-35.0..=-5.0).contains(&db))
            .collect();
        let n = points.len() as f64;
        let (t, d) = points
            .iter()
            .fold((0.0, 0.0), |(t, d), &(ti, di)| (t + ti / n, d + di / n));
        let (covariance, variance) = points.iter().fold((0.0, 0.0), |(c, v), &(ti, di)| {
            (c + (ti - t) * (di - d), v + (ti - t) * (ti - t))
        });
        -60.0 / (covariance / variance)
    }

    /// How long the tail takes to fall by 60 dB at `freq` Hz, from sine
    /// bursts there and a percent either side, their tails' power added up
    /// to smooth out how few of the network's resonances one burst rings.
    fn decay_at(settings: &[(usize, f64)], freq: f64, longest: f64) -> f64 {
        let mut tail = Vec::new();
        for spread in [0.99, 1.0, 1.01] {
            let input = burst(freq * spread, 0.25, 0.7 * longest);
            let output = run(&wet(settings)).render(&[input.clone(), input], &[]);
            let start = (0.25 * FS) as usize;
            let power = power(&[output[0][start..].to_vec(), output[1][start..].to_vec()]);
            if tail.is_empty() {
                tail = power;
            } else {
                tail.iter_mut().zip(power).for_each(|(t, p)| *t += p);
            }
        }
        decay_time(&tail, FS)
    }

    /// The reference test for the decay: the tail falls by 60 dB over the
    /// decay, whatever the size, to within a few percent, which is as
    /// close as a decay curve's slope can be read, measured in the octave
    /// around 250 Hz, well below the damping.
    #[test]
    fn the_tail_falls_60_db_over_the_decay() {
        for (size, decay, sample_rate) in [
            (0.0, 1.0, 48_000),
            (50.0, 2.0, 44_100),
            (100.0, 3.0, 48_000),
            (30.0, 1.5, 96_000),
        ] {
            let fs = f64::from(sample_rate);
            let settings = wet(&[(SIZE, size), (DECAY, decay), (DAMPING, 20_000.0)]);
            let output = run(&settings)
                .with_sample_rate(sample_rate)
                .render(&click((fs * decay * 1.2) as usize), &[]);
            let low = [0, 1].map(|c| band(&output[c], 250.0, fs));
            let power: Vec<f64> = low[0]
                .iter()
                .zip(&low[1])
                .map(|(l, r)| l * l + r * r)
                .collect();
            let measured = decay_time(&power, fs);
            assert!(
                (measured / decay - 1.0).abs() < 0.03,
                "size {size}%, {decay} s at {sample_rate} Hz: {measured} s"
            );
        }
    }

    /// At the damping frequency the tail falls twice as fast, and faster
    /// still above it.
    #[test]
    fn the_tail_falls_twice_as_fast_at_the_damping_frequency() {
        for (size, decay, damping) in [(50.0, 2.0, 3000.0), (0.0, 1.0, 8000.0)] {
            let settings = [(SIZE, size), (DECAY, decay), (DAMPING, damping)];
            let at = decay_at(&settings, damping, decay / 2.0);
            assert!(
                (at / (decay / 2.0) - 1.0).abs() < 0.05,
                "{damping} Hz: {at} s against {decay} s"
            );
            let above = decay_at(&settings, damping * 2.0, decay / 2.0);
            assert!(
                above < 0.8 * at,
                "{above} s an octave up, {at} s at {damping} Hz"
            );
        }
    }

    #[test]
    fn the_damping_filter_takes_as_much_again() {
        for gain in [0.999, 0.9, 0.5, 0.01] {
            for omega in [0.05, 0.4, 1.5, 3.0] {
                let p: f64 = damping_pole(gain, f64::cos(omega));
                let magnitude = (1.0 - p) / (1.0 - 2.0 * p * omega.cos() + p * p).sqrt();
                assert!((magnitude - gain).abs() < 1e-12, "{gain} at {omega}");
                assert!((0.0..1.0).contains(&p));
            }
        }
    }

    #[test]
    fn a_new_reverb_passes_the_signal_until_its_tail() {
        let input = noise(4800);
        let output = run(&[]).render(&input, &[]);
        // 10 ms of pre-delay, and more on the way through the diffuser.
        assert_eq!(output[0][..480], input[0][..480]);
        assert_eq!(output[1][..480], input[1][..480]);
        assert_ne!(output, input);
        // And none at all mixed in at 0%, bit for bit.
        assert_eq!(run(&[(MIX, 0.0)]).render(&input, &[]), input);
    }

    #[test]
    fn the_pre_delay_delays_the_tail() {
        let at = |ms| run(&wet(&[(PREDELAY, ms)])).render(&click(12_000), &[]);
        // 50 ms more is 2400 frames later, bit for bit.
        let (soon, late) = (at(20.0), at(70.0));
        for c in 0..2 {
            assert!(late[c][..960 + 2400].iter().all(|&s| s == 0.0));
            assert_eq!(late[c][2400..], soon[c][..12_000 - 2400]);
        }
    }

    #[test]
    fn width_narrows_the_tail_to_mono() {
        let input = noise(9600);
        let at = |width| run(&wet(&[(WIDTH, width)])).render(&input, &[]);
        let (full, half, mono) = (at(100.0), at(50.0), at(0.0));
        assert_eq!(mono[0], mono[1]);
        for i in 0..9600 {
            let mid = |o: &[Vec<f32>]| o[0][i] + o[1][i];
            let side = |o: &[Vec<f32>]| o[0][i] - o[1][i];
            assert!((mid(&half) - mid(&full)).abs() < 1e-6);
            assert!((side(&half) - 0.5 * side(&full)).abs() < 1e-6);
        }
        // At full width the sides differ: a click in the middle rings
        // with little of its tail in both.
        let full = run(&wet(&[])).render(&click(48_000), &[]);
        let tail = |c: usize| full[c].iter().map(|&s| f64::from(s));
        let both: f64 = tail(0).zip(tail(1)).map(|(l, r)| l * r).sum();
        let each = |c: usize| tail(c).map(|s| s * s).sum::<f64>().sqrt();
        let correlation = both / (each(0) * each(1));
        assert!(correlation.abs() < 0.05, "{correlation}");
    }

    /// The reference test for the low cut: with a steady tone in, the
    /// tail's level against another low cut's is the two cuts' design
    /// gains' ratio, to within a thousandth of a dB.
    #[test]
    fn the_low_cut_takes_the_lows_out_of_the_tail() {
        let tone = burst(100.0, 1.5, 0.0);
        let level = |cut: f64| {
            let settings = wet(&[(LOW_CUT, cut), (SIZE, 0.0), (DECAY, 0.3)]);
            let output = run(&settings).render(&[tone.clone(), tone.clone()], &[]);
            // Steady by now: 75 whole periods, before the fade out.
            gain_at(&output[0][30_000..66_000], 100.0, FS)
        };
        let design = |cut: f64| delay::low_cut(cut).response(100.0, FS);
        for cut in [50.0, 100.0, 400.0] {
            let measured = db(level(cut) / level(20.0));
            let expected = db(design(cut) / design(20.0));
            assert!(
                (measured - expected).abs() < 1e-3,
                "{cut} Hz: {measured} dB, {expected} dB"
            );
        }
    }

    #[test]
    fn rooms_of_any_size_ring_about_as_loud() {
        // With the damping out of the way: a big room loses more of its
        // highs each time round, a small one more often, and the two only
        // even out in the long run.
        let energies: Vec<(f64, Option<usize>)> = [0.0, 25.0, 50.0, 75.0, 100.0]
            .iter()
            .map(|&size| {
                let settings = [
                    (SIZE, size),
                    (DECAY, 2.0),
                    (DAMPING, 20_000.0),
                    (PREDELAY, 0.0),
                ];
                let output = run(&wet(&settings)).render(&click(96_000), &[]);
                let energy = power(&output).iter().sum::<f64>();
                (energy, output[0].iter().position(|&s| s != 0.0))
            })
            .collect();
        let (middle, _) = energies[2];
        for (energy, _) in &energies {
            let difference = 10.0 * (energy / middle).log10();
            assert!(difference.abs() < 1.0, "{energies:?}");
        }
        // Bigger rooms answer later.
        assert!(energies.windows(2).all(|w| w[0].1 < w[1].1), "{energies:?}");
    }

    #[test]
    fn a_new_size_glides_there_without_clicks() {
        // A steady tone's tail, while the room grows and then shrinks.
        let tone = burst(440.0, 3.0, 0.0);
        let events = [param(48_000, SIZE, 80.0), param(96_000, SIZE, 10.0)];
        let output = run(&wet(&[(DECAY, 1.0)])).render(&[tone.clone(), tone], &events);
        for channel in &output {
            for part in channel[24_000..140_000].chunks(4800) {
                let loudest = part.iter().fold(0.0, |m: f32, s| m.max(s.abs()));
                let step = part
                    .windows(2)
                    .map(|w| (w[1] - w[0]).abs())
                    .fold(0.0, f32::max);
                // A sine bent a semitone up moves about this far a frame at
                // most; a click jumps by about as far as the tail reaches.
                let steepest = loudest * (2.0 * std::f32::consts::PI * 440.0 * 1.06 / 48_000.0);
                assert!(step < 1.5 * steepest, "{step} against {steepest}");
            }
        }
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let input = noise(20_000);
        let events = [
            param(700, PREDELAY, 33.3),
            modulation(2300, DECAY, -1.5),
            param(5000, SIZE, 90.0),
            modulation(6001, DAMPING, 2000.0),
            param(9000, LOW_CUT, 500.0),
            param(12_000, WIDTH, 30.0),
            modulation(15_000, PREDELAY, 7.7),
            param(17_000, MIX, 60.0),
        ];
        let render = |block| run(&[]).with_block(block).render(&input, &events);
        let reference = render(512);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    /// A lane's step on the size glides there as setting the size on the
    /// step's frame does.
    #[test]
    fn a_lane_on_the_size_plays_as_setting_it_does() {
        let input = noise(48_000);
        // Beat 1 at 120 bpm: half a second in.
        let set = run(&[])
            .playing()
            .render(&input, &[param(24_000, SIZE, 80.0)]);
        let mut reverb = Reverb::new(&device(&[]));
        let step =
            [(0, 50.0), (1, 50.0), (1, 80.0)].map(|(at, size)| Point::new(Beats::new(at), size));
        let lane = Automation::new(&step, NativeKind::Reverb.params()[SIZE].scale).unwrap();
        assert!(reverb.automate(ParamId(SIZE as u32), lane).is_none());
        let automated = Run::new(Box::new(reverb)).playing().render(&input, &[]);
        assert_eq!(automated, set);
        assert_ne!(automated, run(&[]).playing().render(&input, &[]));
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let input = noise(12_000);
        let new = || Box::new(Reverb::new(&device(&[])));
        let events = [param(2000, SIZE, 70.0)];
        let whole = Run::new(new()).render(&input, &events);
        let mut run = Run::new(new());
        let first = input.each_ref().map(|c| c[..3000].to_vec());
        let second = input.each_ref().map(|c| c[3000..].to_vec());
        let mut swapped = run.render(&first, &events);
        // Gliding to the new size, a node made from the project, which has
        // it.
        run.swap(Box::new(Reverb::new(&device(&[(SIZE, 70.0)]))));
        let rest = run.render(&second, &[]);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }
}

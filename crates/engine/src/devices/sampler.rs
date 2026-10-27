//! The sampler: see [`musicable_model::native::sampler`].
//!
//! A voice reads its sample at a position that moves on by the note's
//! speed each frame, between the sample's frames: a Kaiser-windowed sinc
//! weighs the frames around the position, as [`resample`](crate::resample)
//! does for audio clips, but narrower, so a voice is quick to play. The
//! sinc passes the sample's frames through exactly where a position falls
//! on one. Reading faster than the sample's own rate, the sinc widens, so
//! what wouldn't fit the device's rate is filtered out rather than folded
//! back; and a note pitched an octave or more up reads a copy of the sample
//! at half its rate, or a quarter and so on, so the sinc is never more than
//! twice as wide.

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2};
use std::fmt;
use std::sync::{Arc, OnceLock};

use musicable_model::clip::{Controller, Dimension};
use musicable_model::native::sampler::{
    ATTACK, CUTOFF, DECAY, DETUNE, END, FILTER, LOOP, LOOP_FADE, LOOP_START, MODE, RELEASE,
    RESONANCE, ROOT, START, SUSTAIN, TRANSPOSE, VELOCITY, VOICES, VOLUME,
};
use musicable_model::native::{NativeDevice, ParamInfo, decibels_to_gain};

use super::instrument::{
    Cursor, Envelope, Lane, Playing, Rates, SLOTS, Stage, State, Svf, Tuning, Voices, resonance_q,
    split, warp,
};
use super::{Params, Values};
use crate::event::{NodeEvent, NodeEventKind, NoteEvent, NoteId};
use crate::media::AudioData;
use crate::resample::{Resampler, kaiser, sinc};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The interpolating sinc's zero crossings each side of its centre, at the
/// sample's own rate or slower.
const HALF_WIDTH: usize = 16;
/// Points in its table for each frame of the sample.
const RESOLUTION: usize = 256;
/// Its Kaiser window's shape, for about 80 dB down outside the transition
/// band, which is centred on half the rate the voice plays at.
const BETA: f64 = 7.86;
/// The most the sinc widens. A voice faster than this reads a copy of the
/// sample at a lower rate.
const MAX_STRETCH: f64 = 2.0;
/// The copies of the sample at lower rates, the first at its own: a note up
/// to this many octaves above the sample's rate keeps what wouldn't fit
/// out. Higher notes than that fold some back.
const LEVELS: usize = 8;
/// Zeros either side of each copy's frames, so reading near its ends needs
/// no checks: more than the widest sinc reaches.
const PAD: usize = 2 * HALF_WIDTH + 1;

/// The filter types, by their place in
/// [`FILTERS`](musicable_model::native::sampler::FILTERS): off, then the
/// state variable filter's outputs.
const FILTER_OFF: usize = 0;
const LOW_PASS: usize = 1;
const HIGH_PASS: usize = 2;
const BAND_PASS: usize = 3;

/// The windowed sinc, tabulated from `-(HALF_WIDTH + 1)` frames to
/// `HALF_WIDTH + 1` and zero beyond `HALF_WIDTH`, so a widened sinc's reach
/// stays in the table. Each point holds the sinc there and how far it is
/// to the next point, to go between them in a straight line.
struct Kernel {
    points: Box<[[f32; 2]]>,
}

impl Kernel {
    /// The one table every sample shares, made the first time one is.
    fn get() -> &'static Kernel {
        static KERNEL: OnceLock<Kernel> = OnceLock::new();
        KERNEL.get_or_init(|| {
            let edge = (HALF_WIDTH + 1) as f64;
            let at = |i: usize| {
                let t = i as f64 / RESOLUTION as f64 - edge;
                // Zero on every frame but the centre, so a position on a
                // frame reads it exactly.
                if t.abs() >= HALF_WIDTH as f64 || (t != 0.0 && t.fract() == 0.0) {
                    0.0
                } else {
                    (sinc(t) * kaiser(t / HALF_WIDTH as f64, BETA)) as f32
                }
            };
            let points = (0..2 * (HALF_WIDTH + 1) * RESOLUTION + 1)
                .map(|i| [at(i), at(i + 1) - at(i)])
                .collect();
            Kernel { points }
        })
    }
}

/// The sinc widened for a speed: the table's points from one frame to the
/// next, a whole number, so every frame it weighs falls as far between
/// two points; and the frames each side of a position it weighs.
#[derive(Debug, Clone, Copy)]
struct Stretch {
    stride: usize,
    reach: usize,
}

impl Stretch {
    /// Widened `by`, from 1.0 to [`MAX_STRETCH`], or near it: to a whole
    /// number of points a frame, within 0.4%.
    fn new(by: f64) -> Self {
        let stride = (RESOLUTION as f64 / by).round() as usize;
        Self {
            stride,
            reach: (HALF_WIDTH * RESOLUTION).div_ceil(stride),
        }
    }
}

/// A sample, ready for the sampler to play at any pitch: the file's audio,
/// its first two channels, and copies of it at half the rate, a quarter
/// and so on, each filtered first so what wouldn't fit is out.
pub struct Sample {
    sample_rate: u32,
    frames: u64,
    kernel: &'static Kernel,
    /// Each copy's channels, with [`PAD`] zeros before and after.
    levels: Vec<Vec<Box<[f32]>>>,
}

impl Sample {
    /// `audio`, ready to play. Works out the copies at lower rates, which
    /// takes a while for a long file, and allocates, so call it off the
    /// audio thread.
    pub fn new(audio: &AudioData) -> Self {
        let mut level: Vec<Vec<f32>> = (0..audio.channels().min(2))
            .map(|c| audio.channel(c).to_vec())
            .collect();
        let mut levels = Vec::with_capacity(LEVELS);
        loop {
            levels.push(level.iter().map(|samples| padded(samples)).collect());
            if levels.len() == LEVELS || level[0].len() < 2 {
                break;
            }
            level = level.iter().map(|samples| halve(samples)).collect();
        }
        Self {
            sample_rate: audio.sample_rate(),
            frames: audio.frames(),
            kernel: Kernel::get(),
            levels,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// One or two: a file with more plays its first two.
    pub fn channels(&self) -> usize {
        self.levels[0].len()
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The frame at `position` frames into copy `level`, between frames
    /// through the sinc widened by `stretch`: the left channel and the
    /// right, the same for a mono sample.
    #[inline]
    fn read(&self, level: usize, position: f64, stretch: Stretch) -> [f64; 2] {
        let Stretch { stride, reach } = stretch;
        let channels = &self.levels[level];
        let x = position + PAD as f64;
        let whole = x.floor();
        let fraction = x - whole;
        let whole = whole as usize;
        // On a frame, unwidened, the sinc weighs that frame alone.
        if fraction == 0.0 && stride == RESOLUTION {
            let left = f64::from(channels[0][whole]);
            return [left, channels.get(1).map_or(left, |r| f64::from(r[whole]))];
        }
        // The frames weighed, from the first, and where the first falls in
        // the table: the next ones are `stride` points before it each.
        let taps = 2 * reach;
        let first = whole + 1 - reach;
        let start = (fraction + (reach - 1) as f64) * stride as f64
            + ((HALF_WIDTH + 1) * RESOLUTION) as f64;
        let point = start.floor();
        let between = (start - point) as f32;
        let mut weights = [0.0; 2 * MAX_REACH];
        let weights = &mut weights[..taps];
        let points = self.kernel.points[..=point as usize].iter().rev();
        for (weight, [at, rise]) in weights.iter_mut().zip(points.step_by(stride)) {
            *weight = at + rise * between;
        }
        let sum = dot(weights, &[1.0; 2 * MAX_REACH][..taps]);
        let left = f64::from(dot(weights, &channels[0][first..first + taps]) / sum);
        match channels.get(1) {
            Some(right) => [
                left,
                f64::from(dot(weights, &right[first..first + taps]) / sum),
            ],
            None => [left, left],
        }
    }
}

/// The most frames each side of a position the widest sinc weighs.
const MAX_REACH: usize = 2 * HALF_WIDTH;

/// The sum of the products of `a`'s and `b`'s samples, which are as many:
/// in eight sums side by side, so it's quick, always in the same order.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let ((a8, a_tail), (b8, b_tail)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    let mut lanes = [0.0; 8];
    for (x, y) in a8.iter().zip(b8) {
        for lane in 0..8 {
            lanes[lane] += x[lane] * y[lane];
        }
    }
    let [a, b, c, d, e, f, g, h] = lanes;
    ((a + b) + (c + d)) + ((e + f) + (g + h)) + tail
}

impl fmt::Debug for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sample")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels())
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

/// `samples` with [`PAD`] zeros before and after.
fn padded(samples: &[f32]) -> Box<[f32]> {
    let mut padded = vec![0.0; samples.len() + 2 * PAD];
    padded[PAD..PAD + samples.len()].copy_from_slice(samples);
    padded.into_boxed_slice()
}

/// `samples` at half their rate, filtered first, as converting an audio
/// clip's rate does: frame `n` falls where frame `2 * n` was.
fn halve(samples: &[f32]) -> Vec<f32> {
    let resampler = Resampler::with_step(2.0);
    let mut weights = vec![0.0; 2 * resampler.reach()];
    // Every frame of the copy falls on a frame, so they all weigh the
    // frames around them alike.
    let first = resampler.weights(0.0, &mut weights);
    (0..samples.len().div_ceil(2))
        .map(|n| {
            let from = 2 * n as i64 + first;
            match usize::try_from(from)
                .ok()
                .and_then(|from| samples.get(from..from + weights.len()))
            {
                Some(frames) => dot(&weights, frames),
                // Near the ends, beyond them is silence.
                None => (weights.iter().enumerate())
                    .map(|(j, &w)| {
                        let at = usize::try_from(from + j as i64).ok();
                        at.and_then(|at| samples.get(at)).map_or(0.0, |&s| s * w)
                    })
                    .sum(),
            }
        })
        .collect()
}

/// The sampler's node.
#[derive(Debug)]
pub struct Sampler {
    params: Params,
    info: &'static [ParamInfo],
    /// What it plays. Without a sample, notes start no voices.
    sample: Option<Arc<Sample>>,
    voices: Voices<Voice>,
    sample_rate: f64,
    /// The voices' signal over a span, the left channel and the right.
    sum: [Vec<f32>; 2],
}

impl Sampler {
    pub fn new(device: &NativeDevice, sample: Option<Arc<Sample>>) -> Self {
        Self {
            params: Params::new(device),
            info: device.params(),
            sample,
            voices: Voices::new(),
            sample_rate: 48_000.0,
            sum: [Vec::new(), Vec::new()],
        }
    }
}

impl Node for Sampler {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.sample_rate = f64::from(config.sample_rate);
        self.sum = [vec![0.0; config.max_block], vec![0.0; config.max_block]];
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        self.params.automate(param, automation)
    }

    fn modulated(&mut self, param: ParamId) {
        self.params.modulated(param);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            // Voices carry on with the sample they play, not another.
            let same = match (&self.sample, &previous.sample) {
                (Some(this), Some(that)) => Arc::ptr_eq(this, that),
                (None, None) => true,
                _ => false,
            };
            if same {
                self.voices.take_over(&previous.voices);
            }
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self {
            params,
            info,
            sample,
            voices,
            sample_rate,
            sum,
        } = self;
        voices.start_block();
        let rates = Rates::new(*sample_rate);
        let mut cursor = Cursor::default();
        params.run(ctx, |span| {
            let Some(events) = cursor.events(ctx, &span) else {
                return;
            };
            for event in events {
                handle(event, span.values, info, voices, sample.is_some());
            }
            let Some(sample) = sample.as_deref() else {
                return;
            };
            if !voices.any() {
                return;
            }
            let [left, right] = sum;
            let (left, right) = (
                &mut left[..span.frames.len()],
                &mut right[..span.frames.len()],
            );
            left.fill(0.0);
            right.fill(0.0);
            for slot in 0..SLOTS {
                let voice = &mut voices.slots[slot];
                if voice.playing.state != State::Free
                    && !voice.render(span.values, info, sample, [&mut *left, &mut *right], &rates)
                {
                    voices.end(slot);
                }
            }
            let mut out = output.range_mut(span.frames);
            for (c, channel) in out.channels_mut().enumerate() {
                let voiced: &[f32] = if c == 0 { left } else { right };
                for (sample, &voiced) in channel.iter_mut().zip(voiced) {
                    *sample += voiced;
                }
            }
        });
    }

    fn ended_voices(&self) -> &[NoteId] {
        self.voices.ended()
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.voices.reset();
    }
}

/// Applies a note, modulation for one or the sustain pedal, with the
/// values on its frame. Without a sample, notes start no voices.
fn handle(
    event: &NodeEvent,
    values: &Values,
    info: &[ParamInfo],
    voices: &mut Voices<Voice>,
    has_sample: bool,
) {
    let one_shot = values.first(MODE) >= 0.5;
    match event.kind {
        NodeEventKind::Note(NoteEvent::On {
            note,
            key,
            velocity,
        }) => {
            for voice in voices.with_note(note) {
                voice.release(one_shot);
                voice.playing.note = None;
            }
            if has_sample {
                let limit = values.first(VOICES).round() as usize;
                let slot = voices.start(note, key, velocity, limit);
                voices.slots[slot].amp.start();
            }
        }
        NodeEventKind::Note(NoteEvent::Off { note, .. }) => {
            voices.let_go(note, |voice| voice.release(one_shot));
        }
        NodeEventKind::Note(NoteEvent::Expression {
            note,
            dimension: Dimension::PitchBend,
            value,
        }) => {
            for voice in voices.with_note(note) {
                voice.playing.bend = value;
            }
        }
        NodeEventKind::Modulation {
            param,
            amount,
            note: Some(note),
        } => voices.modulate(note, param.0 as usize, amount as f32, info),
        NodeEventKind::Controller {
            controller: Controller::SUSTAIN,
            value,
        } => voices.pedal(value, |voice| voice.release(one_shot)),
        _ => {}
    }
}

/// One of the sampler's voices.
#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    playing: Playing,
    /// Whether it has worked out where it starts, which it does on its
    /// first frame, with its note's own modulation.
    placed: bool,
    /// Where it reads, in frames of the sample.
    position: f64,
    pass: Pass,
    /// The filter's memory, for each channel.
    filter: [Svf; 2],
    amp: Envelope,
    memo: Memo,
}

impl super::instrument::Voice for Voice {
    fn playing(&self) -> &Playing {
        &self.playing
    }

    fn playing_mut(&mut self) -> &mut Playing {
        &mut self.playing
    }
}

/// The stretch of the sample a voice plays before it jumps back or ends,
/// as the settings were when the stretch started, so moving them never
/// jumps a voice: where it ends, where a loop jumps back to, and over how
/// many frames before its end it crossfades into the jump. All in frames
/// of the sample.
#[derive(Debug, Clone, Copy, Default)]
struct Pass {
    end: f64,
    back: Option<f64>,
    fade: f64,
}

/// Results a voice keeps for the next frame, worked out again only when
/// what they're from changes. Each is a function of what it's from alone,
/// so a voice renders the same whichever frames work them out.
#[derive(Debug, Clone, Copy)]
struct Memo {
    /// A pitch in semitones from the sample's own, and how the voice reads
    /// at it.
    speed: (f64, Speed),
    /// A cutoff and a resonance, and the filter's tuning.
    tuning: ([f32; 2], Tuning),
    /// A volume in dB, and its gain.
    volume: (f32, f64),
}

impl Default for Memo {
    fn default() -> Self {
        // Not a number is never the same as what comes, so the first frame
        // works everything out.
        Self {
            speed: (f64::NAN, Speed::default()),
            tuning: ([f32::NAN; 2], Tuning::new(1.0, 1.0)),
            volume: (f32::NAN, 0.0),
        }
    }
}

/// How a voice reads its sample at a pitch: how many frames of it it moves
/// on a frame, which copy of it it reads, and the sinc it reads through.
#[derive(Debug, Clone, Copy)]
struct Speed {
    frames: f64,
    level: usize,
    /// What positions in the sample scale by to fall in the copy.
    scale: f64,
    stretch: Stretch,
}

impl Default for Speed {
    fn default() -> Self {
        Self {
            frames: 1.0,
            level: 0,
            scale: 1.0,
            stretch: Stretch::new(1.0),
        }
    }
}

impl Speed {
    /// Reading `frames` of the sample a frame, of a sample with `levels`
    /// copies.
    fn new(frames: f64, levels: usize) -> Self {
        let level = if frames < MAX_STRETCH {
            0
        } else {
            (frames.log2().floor() as usize).min(levels - 1)
        };
        let scale = 0.5_f64.powi(level as i32);
        Self {
            frames,
            level,
            scale,
            stretch: Stretch::new((frames * scale).clamp(1.0, MAX_STRETCH)),
        }
    }
}

impl Voice {
    /// Lets the note go, if it's held: its envelope releases, but for a
    /// one-shot, which plays on to its end.
    fn release(&mut self, one_shot: bool) {
        if self.playing.state == State::Held {
            self.playing.state = State::Released;
            if !one_shot {
                self.amp.release();
            }
        }
    }

    /// Adds the voice's signal over a span of `values` to `out`, the left
    /// channel and the right. Returns whether it plays on after the span.
    fn render(
        &mut self,
        values: &Values,
        info: &[ParamInfo],
        sample: &Sample,
        [left, right]: [&mut [f32]; 2],
        rates: &Rates,
    ) -> bool {
        let lanes: [Lane<'_>; 19] = std::array::from_fn(|p| self.playing.lane(values, info, p));
        let lane = |p: usize| lanes[p];
        let (root, transpose, detune) = (lane(ROOT), lane(TRANSPOSE), lane(DETUNE));
        let points = Points {
            start: lane(START),
            end: lane(END),
            looped: lane(LOOP),
            loop_start: lane(LOOP_START),
            mode: lane(MODE),
            fade: lane(LOOP_FADE),
            frames: sample.frames() as f64,
            frames_per_ms: f64::from(sample.sample_rate()) / 1000.0,
        };
        let amp_stages = [ATTACK, DECAY, SUSTAIN, RELEASE].map(lane);
        let (velocity, volume) = (lane(VELOCITY), lane(VOLUME));
        let (filter, cutoff, resonance) = (lane(FILTER), lane(CUTOFF), lane(RESONANCE));
        let key = f64::from(self.playing.key);
        // The sample's frames a frame at its own pitch.
        let rate = f64::from(sample.sample_rate()) / rates.sample_rate;
        let levels = sample.levels.len();
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            if !self.placed {
                self.placed = true;
                self.position = points.start(i);
                self.pass = points.pass(i);
                if self.position >= self.pass.end {
                    return false;
                }
            }

            let semitones = key - f64::from(root.at(i))
                + f64::from(transpose.at(i))
                + f64::from(detune.at(i)) / 100.0
                + f64::from(self.playing.bend);
            let memo = &mut self.memo;
            if memo.speed.0 != semitones {
                let frames = rate * (semitones / 12.0).exp2();
                memo.speed = (semitones, Speed::new(frames, levels));
            }
            let speed = memo.speed.1;
            let read =
                |position: f64| sample.read(speed.level, position * speed.scale, speed.stretch);
            let mut frame = read(self.position);
            let Pass { end, back, fade } = self.pass;
            if let Some(back) = back
                && self.position > end - fade
            {
                // Over to where the jump back lands, as far before it.
                let along = (self.position - (end - fade)) / fade;
                let (out, into) = ((along * FRAC_PI_2).cos(), (along * FRAC_PI_2).sin());
                let landing = read(self.position - (end - back));
                frame = [0, 1].map(|c| frame[c] * out + landing[c] * into);
            }

            let ty = filter.at(i);
            if ty > 0.0 {
                let settings = [cutoff.at(i), resonance.at(i)];
                if memo.tuning.0 != settings {
                    let q = resonance_q(f64::from(settings[1]) / 100.0, FRAC_1_SQRT_2);
                    let g = warp(f64::from(settings[0]), rates.sample_rate);
                    memo.tuning = (settings, Tuning::new(g, q));
                }
                let (a, t) = split(ty, BAND_PASS);
                for (c, x) in frame.iter_mut().enumerate() {
                    let dry = *x;
                    let outputs = self.filter[c].tick(&memo.tuning.1, dry);
                    let pick = |ty: usize| match ty {
                        FILTER_OFF => dry,
                        LOW_PASS => outputs.low,
                        HIGH_PASS => outputs.high,
                        _ => outputs.band,
                    };
                    *x = if t == 0.0 {
                        pick(a)
                    } else {
                        pick(a) + (pick(a + 1) - pick(a)) * t
                    };
                }
            } else {
                // Off, it starts afresh when it comes on.
                self.filter = Default::default();
            }

            let stages = amp_stages.map(|l| l.at(i));
            let stages = [stages[0], stages[1], stages[2] / 100.0, stages[3]];
            let level = self.amp.next(stages, rates.frames_per_ms);
            let db = volume.at(i);
            if memo.volume.0 != db {
                memo.volume = (db, decibels_to_gain(f64::from(db)));
            }
            let amount = f64::from(velocity.at(i)) / 100.0;
            let velocity = 1.0 - amount * (1.0 - f64::from(self.playing.velocity));
            let gain = level * velocity * memo.volume.1 * f64::from(self.playing.fade);
            *l += (frame[0] * gain) as f32;
            *r += (frame[1] * gain) as f32;

            if self.amp.stage == Stage::Off {
                return false;
            }
            if self.playing.state == State::Stolen {
                self.playing.fade -= rates.steal;
                if self.playing.fade <= 0.0 {
                    return false;
                }
            }
            self.position += speed.frames;
            // At the end of a pass, a loop jumps back, as far past where it
            // lands as the voice went past the end, and a new pass starts.
            let mut jumps = 0;
            while self.position >= self.pass.end {
                let (Some(back), true) = (self.pass.back, jumps < 2) else {
                    return false;
                };
                jumps += 1;
                let over = (self.position - self.pass.end) % (self.pass.end - back);
                self.position = back + over;
                self.pass = points.pass(i);
            }
        }
        true
    }
}

/// Where a voice plays from and to, as a span's values set it.
struct Points<'a> {
    start: Lane<'a>,
    end: Lane<'a>,
    looped: Lane<'a>,
    loop_start: Lane<'a>,
    mode: Lane<'a>,
    fade: Lane<'a>,
    /// The sample's length, and its frames in a ms.
    frames: f64,
    frames_per_ms: f64,
}

impl Points<'_> {
    /// The frame `percent` of the way into the sample.
    fn frame(&self, percent: f32) -> f64 {
        (f64::from(percent) / 100.0 * self.frames)
            .round()
            .clamp(0.0, self.frames)
    }

    /// Where a voice that starts on frame `i` of the span starts.
    fn start(&self, i: usize) -> f64 {
        self.frame(self.start.at(i))
    }

    /// The pass that starts on frame `i` of the span. A loop jumps back
    /// only in the Classic mode, to a frame before the end; and its
    /// crossfade is as long as the loop at most, and as what comes before
    /// it.
    fn pass(&self, i: usize) -> Pass {
        let end = self.frame(self.end.at(i));
        let loops = self.looped.at(i) >= 0.5 && self.mode.at(i) < 0.5;
        let back = Some(self.frame(self.loop_start.at(i))).filter(|&back| loops && back < end);
        let fade = back.map_or(0.0, |back| {
            let fade = (f64::from(self.fade.at(i)) * self.frames_per_ms).round();
            fade.min(end - back).min(back)
        });
        Pass { end, back, fade }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{PI, TAU};

    use musicable_model::native::NativeKind;

    use super::*;
    use crate::devices::instrument::tests::{
        bend, for_note, frequency, hz, level_at, off, on, pedal,
    };
    use crate::devices::testing::{Run, context, modulation, param};

    /// A sample of `channels` at `rate`.
    fn sample(rate: u32, channels: Vec<Vec<f32>>) -> Arc<Sample> {
        Arc::new(Sample::new(&AudioData::new(rate, channels)))
    }

    /// `seconds` of a sine at `freq` Hz, at `rate`.
    fn sine(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let frames = (seconds * f64::from(rate)) as usize;
        (0..frames)
            .map(|i| (TAU * freq * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    /// Noise, the same every time for a seed.
    fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9) | 1;
        (0..frames)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (f64::from(state) / f64::from(u32::MAX) * 2.0 - 1.0) as f32
            })
            .collect()
    }

    /// A sampler set as given: rooted on A4, at full level from the start
    /// of each note, without velocity, unless `settings` say otherwise.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut sampler = NativeDevice::new(NativeKind::Sampler);
        for (param, value) in [
            (ROOT, 69.0),
            (ATTACK, 0.1),
            (SUSTAIN, 100.0),
            (VELOCITY, 0.0),
            (VOLUME, 0.0),
        ]
        .into_iter()
        .chain(settings.iter().copied())
        {
            sampler.set(param, value);
        }
        sampler
    }

    /// `frames` of `device` playing `sample` with `events`: the left
    /// channel and the right.
    fn play(
        device: &NativeDevice,
        sample: &Arc<Sample>,
        frames: usize,
        events: &[NodeEvent],
    ) -> Vec<Vec<f32>> {
        let input = [vec![0.0; frames], vec![0.0; frames]];
        let node = Sampler::new(device, Some(sample.clone()));
        Run::new(Box::new(node)).render(&input, events)
    }

    /// The left channel of [`play`], checking it's the right one too.
    fn mono(
        device: &NativeDevice,
        sample: &Arc<Sample>,
        frames: usize,
        events: &[NodeEvent],
    ) -> Vec<f32> {
        let mut out = play(device, sample, frames, events);
        assert_eq!(out[0], out[1]);
        out.swap_remove(0)
    }

    fn level(signal: &[f32], freq: f64) -> f64 {
        let signal: Vec<f64> = signal.iter().map(|&s| f64::from(s)).collect();
        level_at(&signal, freq, 48_000.0)
    }

    /// Frames the 0.1 ms attack takes at 48 kHz, before a note is at full
    /// level.
    const ATTACKED: usize = 5;

    #[test]
    fn a_note_on_the_root_plays_the_sample_as_it_is() {
        let (left, right) = (noise(2000, 1), noise(2000, 2));
        let stereo = sample(48_000, vec![left.clone(), right.clone()]);
        let out = play(&device(&[]), &stereo, 3000, &[on(100, 1, 69, 1.0)]);
        assert!(out[0][..100].iter().all(|&s| s == 0.0));
        assert_eq!(out[0][100 + ATTACKED..2100], left[ATTACKED..]);
        assert_eq!(out[1][100 + ATTACKED..2100], right[ATTACKED..]);
        // And it ends at the sample's end, held or not.
        assert!(out[0][2100..].iter().all(|&s| s == 0.0));
        // A mono sample plays on both sides; a third channel doesn't play.
        let one = sample(48_000, vec![left.clone()]);
        let out = mono(&device(&[]), &one, 3000, &[on(0, 1, 69, 1.0)]);
        assert_eq!(out[ATTACKED..2000], left[ATTACKED..]);
        let three = sample(48_000, vec![left.clone(), right.clone(), noise(2000, 3)]);
        assert_eq!(three.channels(), 2);
    }

    #[test]
    fn notes_play_the_sample_at_their_pitch() {
        let a = sample(48_000, vec![sine(440.0, 48_000, 2.0)]);
        let pitch = |settings: &[(usize, f64)], key: u8, events: &[NodeEvent]| {
            let mut events = events.to_vec();
            events.insert(0, on(0, 1, key, 1.0));
            let out = mono(&device(settings), &a, 24_000, &events);
            frequency(&out[480..], 48_000.0)
        };
        let close = |measured: f64, expected: f64| (measured / expected - 1.0).abs() < 1e-5;
        assert!(close(pitch(&[], 69, &[]), 440.0));
        assert!(close(pitch(&[], 76, &[]), hz(76.0)));
        assert!(close(pitch(&[], 45, &[]), 110.0));
        // Transposed down a fifth and detuned up 20 cents.
        let moved = [(TRANSPOSE, -7.0), (DETUNE, 20.0)];
        assert!(close(pitch(&moved, 69, &[]), hz(69.0 - 6.8)));
        // Rooted on another note, and bent.
        assert!(close(pitch(&[(ROOT, 57.0)], 57, &[]), 440.0));
        assert!(close(pitch(&[], 69, &[bend(0, 1, -12.0)]), 220.0));
        // A sample at 44.1 kHz plays at its pitch at 48 kHz.
        let slower = sample(44_100, vec![sine(440.0, 44_100, 2.0)]);
        let out = mono(&device(&[]), &slower, 24_000, &[on(0, 1, 69, 1.0)]);
        assert!(close(frequency(&out[480..], 48_000.0), 440.0));
    }

    /// The reference test for reading between frames. A tone that fits
    /// plays at its level, to within a thousandth of a dB, however fast or
    /// slow the voice reads; and what reading faster would fold back below
    /// half the device's rate, or reading slower would image there, is 80
    /// dB down.
    #[test]
    fn reading_between_frames_keeps_tones_and_leaves_out_what_doesnt_fit() {
        // A tone of `freq` Hz in a sample at `rate`, played `semitones`
        // from the root: in dB, the tone where it plays, and what plays at
        // `stray` Hz.
        let play_tone = |freq: f64, rate: u32, semitones: i32, stray: f64| {
            let speed = (f64::from(semitones) / 12.0).exp2();
            let tone = sample(rate, vec![sine(freq, rate, speed + 0.2)]);
            let key = (69 + semitones) as u8;
            let out = mono(&device(&[]), &tone, 48_000, &[on(0, 1, key, 1.0)]);
            let out = &out[2400..45_600];
            let db = |freq: f64| 20.0 * level(out, freq).log10();
            (db(freq * speed), db(stray))
        };
        let fifth = (7.0_f64 / 12.0).exp2();
        // Tones that fit, and what images: at 44.1 kHz on a 48 kHz device,
        // the image at 34.1 kHz folds to 13.9 kHz; an octave down, the
        // image at 32 kHz plays at 16 kHz.
        for (freq, rate, semitones, stray) in [
            (10_000.0, 44_100, 0, 48_000.0 - 34_100.0),
            (16_000.0, 48_000, -12, 16_000.0),
            (5000.0, 48_000, 7, 48_000.0 - 20_000.0 * fifth),
            (5000.0, 48_000, 12, 16_000.0),
            (4000.0, 48_000, 19, 48_000.0 - 20_000.0 * fifth),
        ] {
            let (tone, stray) = play_tone(freq, rate, semitones, stray);
            let at = format!("{freq} Hz at {rate} Hz, {semitones} semitones");
            assert!(tone.abs() < 1e-3, "{at}: {tone} dB");
            assert!(stray < -80.0, "{at}: {stray} dB at the stray");
        }
        // What would fold back: a fifth up, 20 kHz would play at 30 kHz and
        // fold to 18 kHz; an octave up, reading the copy at half the rate,
        // 16 kHz would fold to 16 kHz; and an octave and a fifth up, 10 kHz
        // would play at 30 kHz.
        for (freq, semitones, folded) in [
            (20_000.0, 7, 48_000.0 - 20_000.0 * fifth),
            (16_000.0, 12, 16_000.0),
            (10_000.0, 19, 48_000.0 - 20_000.0 * fifth),
        ] {
            let (_, folded) = play_tone(freq, 48_000, semitones, folded);
            assert!(
                folded < -80.0,
                "{freq} Hz, {semitones} semitones: {folded} dB"
            );
        }
    }

    /// A sample whose frames count up: frame `i` is `i / 1000`.
    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| i as f32 / 1000.0).collect()
    }

    #[test]
    fn notes_play_from_the_start_to_the_end_and_loop_back() {
        let counting = sample(48_000, vec![ramp(1000)]);
        // From frame 100 to 500, looping back to 300, without a crossfade.
        let looped = device(&[
            (START, 10.0),
            (END, 50.0),
            (LOOP, 1.0),
            (LOOP_START, 30.0),
            (LOOP_FADE, 0.0),
        ]);
        let out = mono(&looped, &counting, 1200, &[on(0, 1, 69, 1.0)]);
        for (i, &s) in out.iter().enumerate().skip(ATTACKED) {
            let frame = if i < 400 {
                100 + i
            } else {
                300 + (i - 400) % 200
            };
            assert_eq!(s, frame as f32 / 1000.0, "frame {i}");
        }
        // Without the loop, it stops at the end, though the note's held.
        let once = device(&[(START, 10.0), (END, 50.0)]);
        let out = mono(&once, &counting, 1200, &[on(0, 1, 69, 1.0)]);
        assert_eq!(out[399], 499.0 / 1000.0);
        assert!(out[400..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_loop_crossfades_into_its_jump_back() {
        let counting = sample(48_000, vec![ramp(1000)]);
        // Looping from frame 500 back to 300, crossfading over 1 ms, 48
        // frames, with the 48 before frame 300.
        let looped = device(&[
            (END, 50.0),
            (LOOP, 1.0),
            (LOOP_START, 30.0),
            (LOOP_FADE, 1.0),
        ]);
        let out = mono(&looped, &counting, 900, &[on(0, 1, 69, 1.0)]);
        for (i, &s) in out.iter().enumerate().skip(ATTACKED) {
            let at = if i < 500 { i } else { 300 + (i - 500) % 200 };
            let frame = |at: usize| f64::from(at as f32 / 1000.0);
            let expected = if at > 452 {
                let along = (at - 452) as f64 / 48.0 * PI / 2.0;
                frame(at) * along.cos() + frame(at - 200) * along.sin()
            } else {
                frame(at)
            };
            assert!((f64::from(s) - expected).abs() < 1e-6, "frame {i}");
        }
        // A loop of a tone that doesn't hold whole cycles jumps back to
        // another point in its cycle, which clicks, unless it crossfades:
        // then no frame moves further than the tone does.
        let tone = sample(48_000, vec![sine(1000.0, 48_000, 0.1)]);
        let largest = |fade: f64| {
            // 1212 frames from frame 1200: 25 cycles and a quarter.
            let looped = [
                (END, 50.25),
                (LOOP, 1.0),
                (LOOP_START, 25.0),
                (LOOP_FADE, fade),
            ];
            let out = mono(&device(&looped), &tone, 4800, &[on(0, 1, 69, 1.0)]);
            let steps = out[ATTACKED..].windows(2).map(|w| (w[1] - w[0]).abs());
            steps.fold(0.0_f32, f32::max)
        };
        let natural = (TAU / 48.0) as f32;
        assert!(largest(0.0) > 2.0 * natural, "{}", largest(0.0));
        assert!(largest(10.0) < 1.01 * natural, "{}", largest(10.0));
    }

    #[test]
    fn a_one_shot_plays_to_its_end_however_short_the_note() {
        let counting = sample(48_000, vec![ramp(1000)]);
        let shot = device(&[(MODE, 1.0), (LOOP, 1.0), (RELEASE, 1.0)]);
        let out = mono(&shot, &counting, 1200, &[on(0, 1, 69, 1.0), off(10, 1)]);
        // Let go at once, it plays on, and doesn't loop.
        for (i, &s) in out[..1000].iter().enumerate().skip(ATTACKED) {
            assert_eq!(s, i as f32 / 1000.0, "frame {i}");
        }
        assert!(out[1000..].iter().all(|&s| s == 0.0));
        // In the Classic mode, the note's 1 ms release, 48 frames, ends it.
        let classic = device(&[(LOOP, 1.0), (RELEASE, 1.0)]);
        let out = mono(&classic, &counting, 1200, &[on(0, 1, 69, 1.0), off(10, 1)]);
        assert!(out[10 + 46] > 0.0);
        assert!(out[10 + 47..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_sustain_pedal_holds_notes_let_go_until_it_comes_up() {
        let counting = sample(48_000, vec![ramp(1000)]);
        let classic = device(&[(LOOP, 1.0), (LOOP_START, 50.0), (RELEASE, 1.0)]);
        // Let go at once with the pedal down, a note loops on until the
        // pedal comes up, as one let go then does.
        let late = mono(
            &classic,
            &counting,
            3000,
            &[on(0, 1, 69, 1.0), off(2000, 1)],
        );
        assert!(late[2000 + 46] > 0.0);
        assert!(late[2000 + 47..].iter().all(|&s| s == 0.0));
        let events = [
            pedal(0, true),
            on(0, 1, 69, 1.0),
            off(10, 1),
            pedal(2000, false),
        ];
        assert_eq!(mono(&classic, &counting, 3000, &events), late);
        // A one-shot let go plays to its end either way.
        let shot = device(&[(MODE, 1.0), (RELEASE, 1.0)]);
        let short = [on(0, 1, 69, 1.0), off(10, 1)];
        let events = [pedal(0, true), on(0, 1, 69, 1.0), off(10, 1)];
        let out = mono(&shot, &counting, 1200, &events);
        assert_eq!(out, mono(&shot, &counting, 1200, &short));
        assert!(out[999] > 0.0 && out[1000..].iter().all(|&s| s == 0.0));
    }

    /// The amp envelope, velocity and volume set each note's level: with a
    /// sample that holds at 0.5, on every frame.
    #[test]
    fn notes_follow_their_envelope_velocity_and_volume() {
        let held = sample(48_000, vec![vec![0.5; 12_000]]);
        // A 10 ms attack, a 20 ms decay to half and a 30 ms release; half
        // the way from full at half velocity, and 6 dB down.
        let shaped = device(&[
            (ATTACK, 10.0),
            (DECAY, 20.0),
            (SUSTAIN, 50.0),
            (RELEASE, 30.0),
            (VELOCITY, 50.0),
            (VOLUME, -6.0),
        ]);
        let out = mono(&shaped, &held, 9600, &[on(0, 1, 69, 0.5), off(4800, 1)]);
        let gain = 0.5 * 0.75 * decibels_to_gain(-6.0);
        let mut envelope = Envelope::default();
        envelope.start();
        for (i, &s) in out.iter().enumerate() {
            if i == 4800 {
                envelope.release();
            }
            let level = envelope.next([10.0, 20.0, 0.5, 30.0], 48.0);
            assert!((f64::from(s) - level * gain).abs() < 1e-6, "frame {i}");
        }
        // Silent from the last of the release's 30 ms, its 1440th frame.
        assert!(out[4800 + 1438] > 0.0);
        assert!(out[4800 + 1439..].iter().all(|&s| s == 0.0));
    }

    /// The filter's types, under the bilinear transform: the low and the
    /// high pass 3 dB down at the cutoff with no resonance, and at the Q
    /// the resonance gives otherwise, and the band pass flat at it.
    #[test]
    fn the_filter_sits_at_its_cutoff() {
        let a = sample(48_000, vec![sine(440.0, 48_000, 1.2)]);
        let peaked = (25.0 * FRAC_1_SQRT_2).sqrt();
        for (filter, resonance, expected) in [
            (LOW_PASS, 0.0, FRAC_1_SQRT_2),
            (HIGH_PASS, 0.0, FRAC_1_SQRT_2),
            (BAND_PASS, 0.0, 1.0),
            (LOW_PASS, 50.0, peaked),
        ] {
            let settings = [
                (FILTER, filter as f64),
                (CUTOFF, 440.0),
                (RESONANCE, resonance),
            ];
            let out = mono(&device(&settings), &a, 48_000, &[on(0, 1, 69, 1.0)]);
            let measured = level(&out[4800..], 440.0);
            assert!(
                (measured / expected - 1.0).abs() < 1e-3,
                "type {filter}: {measured}"
            );
        }
    }

    /// The reference test for the copies at lower rates: each holds what
    /// fits its rate, to within a thousandth of a dB, and what doesn't is
    /// 85 dB down.
    #[test]
    fn each_copy_holds_what_fits_its_rate() {
        let tones = sample(
            48_000,
            vec![sine(1000.0, 48_000, 1.0), sine(15_000.0, 48_000, 1.0)],
        );
        assert_eq!(tones.levels.len(), LEVELS);
        let mut frames = 48_000;
        for (level, channels) in tones.levels.iter().enumerate() {
            let rate = 48_000.0 / f64::from(1 << level);
            assert_eq!(channels[0].len(), frames + 2 * PAD);
            let signal = |c: usize| -> Vec<f64> {
                let channel = &channels[c][PAD..PAD + frames];
                let middle = &channel[frames / 10..frames - frames / 10];
                middle.iter().map(|&s| f64::from(s)).collect()
            };
            let db = |c: usize, freq: f64| 20.0 * level_at(&signal(c), freq, rate).log10();
            // 1 kHz fits the first five, and 15 kHz only the first.
            if level < 5 {
                assert!(
                    db(0, 1000.0).abs() < 1e-3,
                    "level {level}: {}",
                    db(0, 1000.0)
                );
            }
            if level == 0 {
                assert!(db(1, 15_000.0).abs() < 1e-3);
            } else {
                let folded = (15_000.0 % rate).min(rate - 15_000.0 % rate);
                assert!(db(1, folded) < -85.0, "level {level}: {}", db(1, folded));
            }
            frames = frames.div_ceil(2);
        }
    }

    #[test]
    fn a_note_far_above_the_root_reads_within_the_sample() {
        let short = sample(48_000, vec![noise(300, 6), noise(300, 7)]);
        let low_root = device(&[(ROOT, 0.0), (TRANSPOSE, 48.0)]);
        let out = play(&low_root, &short, 100, &[on(0, 1, 127, 1.0)]);
        assert!(out.iter().flatten().all(|s| s.is_finite()));
    }

    #[test]
    fn the_engine_hears_which_notes_voices_end() {
        let short = sample(48_000, vec![noise(300, 8)]);
        let mut node = Sampler::new(&device(&[]), Some(short));
        node.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 512,
        });
        let mut out = vec![0.0; 2 * 512];
        let mut block = |node: &mut Sampler, events: &[NodeEvent]| {
            let ctx = context(512, events);
            let mut output = AudioMut::from_planar(&mut out, 2);
            node.process(&ctx, Inputs::from_views(&[]), &mut output);
            node.ended_voices().to_vec()
        };
        // The sample's 300 frames end the first note in its block.
        assert_eq!(block(&mut node, &[on(0, 7, 69, 1.0)]), [NoteId(7)]);
        assert_eq!(block(&mut node, &[on(400, 8, 69, 1.0)]), []);
        assert_eq!(block(&mut node, &[]), [NoteId(8)]);
    }

    #[test]
    fn without_a_sample_audio_passes_through_and_notes_play_nothing() {
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let node = Sampler::new(&device(&[]), None);
        let events = [on(0, 1, 69, 1.0)];
        let output = Run::new(Box::new(node)).render(&[input.clone(), input.clone()], &events);
        assert_eq!(output, [input.clone(), input]);
    }

    /// Notes coming and going, and settings moving, on a stereo sample at
    /// another rate, with notes reading copies at lower rates.
    fn busy() -> (NativeDevice, Arc<Sample>, Vec<NodeEvent>) {
        let mut sampler = NativeDevice::new(NativeKind::Sampler);
        sampler.set(LOOP, 1.0);
        sampler.set(LOOP_START, 20.0);
        sampler.set(END, 80.0);
        sampler.set(FILTER, 1.0);
        sampler.set(RESONANCE, 40.0);
        sampler.set(VOICES, 3.0);
        let noisy = sample(44_100, vec![noise(6000, 4), noise(6000, 5)]);
        let events = vec![
            on(0, 1, 60, 0.8),
            on(0, 2, 67, 0.5),
            param(300, CUTOFF, 400.0),
            on(700, 3, 72, 1.0),
            bend(900, 3, 7.5),
            modulation(1200, DETUNE, 30.0),
            on(1500, 4, 84, 0.3),
            for_note(1500, 4, CUTOFF, 3000.0),
            pedal(1800, true),
            off(2100, 1),
            param(2500, FILTER, 2.0),
            param(2600, LOOP_START, 50.0),
            off(3001, 3),
            on(3001, 5, 96, 0.9),
            param(4000, VOLUME, -12.0),
            off(5000, 4),
            off(5000, 5),
            off(5001, 2),
            pedal(5500, false),
        ];
        (sampler, noisy, events)
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let (sampler, noisy, events) = busy();
        let silence = [vec![0.0; 9000], vec![0.0; 9000]];
        let render = |block| {
            Run::new(Box::new(Sampler::new(&sampler, Some(noisy.clone()))))
                .with_block(block)
                .render(&silence, &events)
        };
        let reference = render(512);
        assert!(reference[0].iter().any(|&s| s.abs() > 0.1));
        assert_ne!(reference[0], reference[1]);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let (sampler, noisy, events) = busy();
        let silence = [vec![0.0; 9000], vec![0.0; 9000]];
        let new = |sampler: &NativeDevice, sample: &Arc<Sample>| {
            Box::new(Sampler::new(sampler, Some(sample.clone())))
        };
        let whole = Run::new(new(&sampler, &noisy)).render(&silence, &events);
        let (first, second): (Vec<NodeEvent>, Vec<NodeEvent>) =
            events.iter().partition(|e| e.offset < 2000);
        let later: Vec<NodeEvent> = (second.iter())
            .map(|e| NodeEvent {
                offset: e.offset - 2000,
                ..*e
            })
            .collect();
        // A node made from the project with the value set before the swap,
        // and the same sample, carries on just as the old one would have,
        // and so does the modulation of a parameter something still
        // modulates.
        let mut moved = sampler.clone();
        moved.set(CUTOFF, 400.0);
        let mut run = Run::new(new(&sampler, &noisy));
        let mut swapped = run.render(&[vec![0.0; 2000], vec![0.0; 2000]], &first);
        let mut next = new(&moved, &noisy);
        next.modulated(ParamId(DETUNE as u64));
        run.swap(next);
        let rest = run.render(&[vec![0.0; 7000], vec![0.0; 7000]], &later);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
        // With another sample, the notes playing stop: the new one plays
        // the notes that start after.
        let other = sample(44_100, vec![noise(6000, 9)]);
        let mut run = Run::new(new(&sampler, &noisy));
        run.render(&[vec![0.0; 2000], vec![0.0; 2000]], &first);
        run.swap(new(&moved, &other));
        let rest = run.render(&[vec![0.0; 1000], vec![0.0; 1000]], &[]);
        assert!(rest.iter().flatten().all(|&s| s == 0.0));
    }
}

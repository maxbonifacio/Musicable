//! The synth: see [`musicable_model::native::synth`].

use std::f64::consts::{FRAC_1_SQRT_2, TAU};

use musicable_model::clip::{Controller, Dimension};
use musicable_model::native::synth::{
    AMP_ATTACK, AMP_DECAY, AMP_RELEASE, AMP_SUSTAIN, CUTOFF, FILTER, FILTER_ATTACK, FILTER_DECAY,
    FILTER_ENV, FILTER_RELEASE, FILTER_SUSTAIN, GLIDE, KEY_TRACK, NOISE, OSC1_LEVEL, OSC1_OCTAVE,
    OSC1_WAVE, OSC2_DETUNE, OSC2_LEVEL, OSC2_PITCH, OSC2_WAVE, RESONANCE, VELOCITY, VOICES, VOLUME,
};
use musicable_model::native::{NativeDevice, ParamInfo, decibels_to_gain};

use super::instrument::{
    Cursor, Envelope, Lane, Playing, Rates, SLOTS, Stage, State, Svf, Tuning, Voices, resonance_q,
    split, warp,
};
use super::{Params, Values};
use crate::event::{NodeEvent, NodeEventKind, NoteEvent, NoteId};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The waves, by their place in [`WAVES`](musicable_model::native::synth::WAVES).
const SINE: usize = 0;
const TRIANGLE: usize = 1;
const SAW: usize = 2;
const SQUARE: usize = 3;

/// The filter types, by their place in
/// [`FILTERS`](musicable_model::native::synth::FILTERS).
const LOW_PASS_24: usize = 1;
const HIGH_PASS_12: usize = 2;
const BAND_PASS_12: usize = 3;

/// The Qs of a fourth-order Butterworth filter's two sections.
const BUTTERWORTH_4: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_376_5];

/// The most an oscillator moves in a frame, in cycles: a little short of
/// half, where the band-limiting corrections would overlap.
const MAX_STEP: f64 = 0.49;

/// The synth's node.
#[derive(Debug, Clone)]
pub struct Synth {
    params: Params,
    info: &'static [ParamInfo],
    voices: Voices<Voice>,
    /// The last note started, which the next one glides from.
    last: Option<Last>,
    sample_rate: f64,
    /// The voices' signal over a span, before it goes to both channels.
    sum: Vec<f32>,
}

/// The last note started: its voice's slot, which note it was there, and
/// its key.
#[derive(Debug, Clone, Copy)]
struct Last {
    slot: usize,
    started: u64,
    key: u8,
}

impl Synth {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            info: device.params(),
            voices: Voices::new(),
            last: None,
            sample_rate: 48_000.0,
            sum: Vec::new(),
        }
    }
}

impl Node for Synth {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.sample_rate = f64::from(config.sample_rate);
        self.sum = vec![0.0; config.max_block];
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
            self.voices.take_over(&previous.voices);
            self.last = previous.last;
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
            voices,
            last,
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
                handle(event, span.values, info, voices, last);
            }
            if !voices.any() {
                return;
            }
            let sum = &mut sum[..span.frames.len()];
            sum.fill(0.0);
            for slot in 0..SLOTS {
                let voice = &mut voices.slots[slot];
                if voice.playing.state != State::Free
                    && !voice.render(span.values, info, sum, &rates)
                {
                    voices.end(slot);
                }
            }
            let mut out = output.range_mut(span.frames);
            for channel in out.channels_mut() {
                for (sample, &voiced) in channel.iter_mut().zip(sum.iter()) {
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
        self.last = None;
    }
}

/// Applies a note, modulation for one or the sustain pedal, with the
/// values on its frame.
fn handle(
    event: &NodeEvent,
    values: &Values,
    info: &[ParamInfo],
    voices: &mut Voices<Voice>,
    last: &mut Option<Last>,
) {
    match event.kind {
        NodeEventKind::Note(NoteEvent::On {
            note,
            key,
            velocity,
        }) => {
            for voice in voices.with_note(note) {
                voice.release();
                voice.playing.note = None;
            }
            // From the last note's pitch, where it has got to if it's still
            // playing.
            let from = match *last {
                Some(last) => {
                    let voice = &voices.slots[last.slot];
                    let playing = voice.playing.state != State::Free;
                    if playing && voice.playing.started == last.started {
                        voice.pitch()
                    } else {
                        f64::from(last.key)
                    }
                }
                None => f64::from(key),
            };
            let limit = values.first(VOICES).round() as usize;
            let slot = voices.start(note, key, velocity, limit);
            let voice = &mut voices.slots[slot];
            voice.glide_from = from;
            voice.noise = noise_seed(voice.playing.started);
            voice.amp.start();
            voice.filter_env.start();
            *last = Some(Last {
                slot,
                started: voice.playing.started,
                key,
            });
        }
        NodeEventKind::Note(NoteEvent::Off { note, .. }) => voices.let_go(note, Voice::release),
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
        } => voices.pedal(value, Voice::release),
        _ => {}
    }
}

/// One of the synth's voices.
#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    playing: Playing,
    /// The pitch its glide starts from, in semitones, and how far along the
    /// glide it is, from 0.0 to 1.0.
    glide_from: f64,
    glided: f64,
    /// Each oscillator's phase, in cycles from 0.0 to 1.0.
    phases: [f64; 2],
    /// The noise generator's state.
    noise: u32,
    /// The filter's two sections' memory: the second only plays in the 24
    /// dB low pass, but always runs, so it's ready.
    filter: [Svf; 2],
    filter_env: Envelope,
    amp: Envelope,
    /// What it last worked out, for frames whose settings are the same.
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

/// Results a voice keeps for the next frame, worked out again only when
/// what they're from changes. Each is a function of what it's from alone,
/// so a voice renders the same whichever frames work them out.
#[derive(Debug, Clone, Copy)]
struct Memo {
    /// A pitch in semitones, and its frequency in Hz.
    hz: (f64, f64),
    /// Oscillator 2's pitch and detune, and its frequency's ratio to
    /// oscillator 1's.
    ratio: ([f32; 2], f64),
    /// A resonance, and the Qs it gives the 12 dB filters and the 24 dB
    /// low pass's second section.
    resonance: (f32, [f64; 2]),
    /// A cutoff in Hz, a resonance and a filter type, and the sections'
    /// tunings.
    tunings: ([f64; 3], [Tuning; 2]),
    /// A volume in dB, and its gain.
    volume: (f32, f64),
}

impl Default for Memo {
    fn default() -> Self {
        // Not a number is never the same as what comes, so the first frame
        // works everything out.
        let none = Tuning::new(1.0, 1.0);
        Self {
            hz: (f64::NAN, 0.0),
            ratio: ([f32::NAN; 2], 0.0),
            resonance: (f32::NAN, [0.0; 2]),
            tunings: ([f64::NAN; 3], [none; 2]),
            volume: (f32::NAN, 0.0),
        }
    }
}

impl Voice {
    /// Lets the note go, if it's held.
    fn release(&mut self) {
        if self.playing.state == State::Held {
            self.playing.state = State::Released;
            self.amp.release();
            self.filter_env.release();
        }
    }

    /// Its pitch where its glide has got to, without its bend, in
    /// semitones.
    fn pitch(&self) -> f64 {
        let key = f64::from(self.playing.key);
        self.glide_from + (key - self.glide_from) * self.glided
    }

    /// Adds the voice's signal over a span of `values` to `out`. Returns
    /// whether it plays on after the span.
    fn render(
        &mut self,
        values: &Values,
        info: &[ParamInfo],
        out: &mut [f32],
        rates: &Rates,
    ) -> bool {
        let lanes: [Lane<'_>; 25] = std::array::from_fn(|p| self.playing.lane(values, info, p));
        let lane = |p: usize| lanes[p];
        let (wave1, octave1, level1) = (lane(OSC1_WAVE), lane(OSC1_OCTAVE), lane(OSC1_LEVEL));
        let (wave2, pitch2, detune2, level2) = (
            lane(OSC2_WAVE),
            lane(OSC2_PITCH),
            lane(OSC2_DETUNE),
            lane(OSC2_LEVEL),
        );
        let (noise, filter, cutoff, resonance) =
            (lane(NOISE), lane(FILTER), lane(CUTOFF), lane(RESONANCE));
        let (filter_env, key_track) = (lane(FILTER_ENV), lane(KEY_TRACK));
        let filter_stages = [FILTER_ATTACK, FILTER_DECAY, FILTER_SUSTAIN, FILTER_RELEASE].map(lane);
        let amp_stages = [AMP_ATTACK, AMP_DECAY, AMP_SUSTAIN, AMP_RELEASE].map(lane);
        let (volume, velocity, glide) = (lane(VOLUME), lane(VELOCITY), lane(GLIDE));
        let stages = |lanes: &[Lane<'_>; 4], i: usize| {
            let [attack, decay, sustain, release] = lanes.map(|l| l.at(i));
            [attack, decay, sustain / 100.0, release]
        };
        let key = f64::from(self.playing.key);
        let sample_rate = rates.sample_rate;
        for (i, sample) in out.iter_mut().enumerate() {
            let glide_ms = f64::from(glide.at(i));
            self.glided = if glide_ms > 0.0 {
                (self.glided + 1.0 / (glide_ms * rates.frames_per_ms)).min(1.0)
            } else {
                1.0
            };
            let note = self.glide_from + (key - self.glide_from) * self.glided;
            let pitch = note + f64::from(self.playing.bend);

            let memo = &mut self.memo;
            let pitch1 = pitch + 12.0 * f64::from(octave1.at(i));
            if memo.hz.0 != pitch1 {
                memo.hz = (pitch1, 440.0 * ((pitch1 - 69.0) / 12.0).exp2());
            }
            let offset2 = [pitch2.at(i), detune2.at(i)];
            if memo.ratio.0 != offset2 {
                let semitones = f64::from(offset2[0]) + f64::from(offset2[1]) / 100.0;
                memo.ratio = (offset2, (semitones / 12.0).exp2());
            }
            let step1 = memo.hz.1 / sample_rate;
            let steps = [step1, step1 * memo.ratio.1].map(|step| step.min(MAX_STEP));
            let mut mix = 0.0;
            for (osc, (wave, level)) in [(wave1, level1), (wave2, level2)].iter().enumerate() {
                let level = f64::from(level.at(i)) / 100.0;
                if level != 0.0 {
                    mix += level * oscillator(wave.at(i), self.phases[osc], steps[osc]);
                }
                let phase = self.phases[osc] + steps[osc];
                self.phases[osc] = if phase >= 1.0 { phase - 1.0 } else { phase };
            }
            let noise = f64::from(noise.at(i)) / 100.0;
            if noise != 0.0 {
                mix += noise * white(&mut self.noise);
            }

            let envelope = self
                .filter_env
                .next(stages(&filter_stages, i), rates.frames_per_ms);
            let track = f64::from(key_track.at(i)) / 100.0;
            let semitones = f64::from(filter_env.at(i)) * envelope + track * (pitch - 60.0);
            let freq = f64::from(cutoff.at(i)) * (semitones / 12.0).exp2();
            let ty = filter.at(i);
            let res = resonance.at(i);
            if memo.resonance.0 != res {
                let r = f64::from(res) / 100.0;
                let qs = [
                    resonance_q(r, FRAC_1_SQRT_2),
                    resonance_q(r, BUTTERWORTH_4[1]),
                ];
                memo.resonance = (res, qs);
            }
            let key = [freq, f64::from(res), f64::from(ty)];
            if memo.tunings.0 != key {
                memo.tunings = (key, tunings(freq, ty, memo.resonance.1, sample_rate));
            }
            let [first, second] = memo.tunings.1;
            let one = self.filter[0].tick(&first, mix);
            let two = self.filter[1].tick(&second, one.low);
            let pick = |ty: usize| match ty {
                LOW_PASS_24 => two.low,
                HIGH_PASS_12 => one.high,
                BAND_PASS_12 => one.band,
                _ => one.low,
            };
            let (a, t) = split(ty, BAND_PASS_12);
            let filtered = if t == 0.0 {
                pick(a)
            } else {
                pick(a) + (pick(a + 1) - pick(a)) * t
            };

            let level = self.amp.next(stages(&amp_stages, i), rates.frames_per_ms);
            let db = volume.at(i);
            if memo.volume.0 != db {
                memo.volume = (db, decibels_to_gain(f64::from(db)));
            }
            let amount = f64::from(velocity.at(i)) / 100.0;
            let velocity = 1.0 - amount * (1.0 - f64::from(self.playing.velocity));
            let gain = level * velocity * memo.volume.1 * f64::from(self.playing.fade);
            *sample += (filtered * gain) as f32;

            if self.amp.stage == Stage::Off {
                return false;
            }
            if self.playing.state == State::Stolen {
                self.playing.fade -= rates.steal;
                if self.playing.fade <= 0.0 {
                    return false;
                }
            }
        }
        true
    }
}

/// The filter's sections tuned to `freq` Hz with type `ty` (between two
/// types, as a switch ramps), and `qs`, the resonance's Qs for the 12 dB
/// types and for the 24 dB low pass's second section.
fn tunings(freq: f64, ty: f32, [q12, q24]: [f64; 2], sample_rate: f64) -> [Tuning; 2] {
    let g = warp(freq, sample_rate);
    // The first section's Q goes over between the types either side, in
    // damping: Q's reciprocal.
    let damping = |ty: usize| {
        if ty == LOW_PASS_24 {
            1.0 / BUTTERWORTH_4[0]
        } else {
            1.0 / q12
        }
    };
    let (a, t) = split(ty, BAND_PASS_12);
    let k = if t == 0.0 {
        damping(a)
    } else {
        damping(a) + (damping(a + 1) - damping(a)) * t
    };
    [Tuning::new(g, 1.0 / k), Tuning::new(g, q24)]
}

/// Oscillator `wave` (a wave's place in the list, or between two as it
/// ramps), at `phase` cycles and moving `step` cycles a frame.
fn oscillator(wave: f32, phase: f64, step: f64) -> f64 {
    let (a, t) = split(wave, SQUARE);
    let first = shape(a, phase, step);
    if t == 0.0 {
        first
    } else {
        first + (shape(a + 1, phase, step) - first) * t
    }
}

/// One of the waves, from -1.0 to 1.0, band-limited with polynomial
/// corrections where it jumps (the saw and the square) and where it turns
/// (the triangle): two-sample PolyBLEPs and PolyBLAMPs (Välimäki,
/// Pekonen and Nam, 2012; Esqueda, Välimäki and Bilbao, 2016).
fn shape(wave: usize, phase: f64, step: f64) -> f64 {
    match wave {
        SINE => (TAU * phase).sin(),
        TRIANGLE => {
            let naive = 1.0 - 4.0 * (phase - 0.5).abs();
            let turns = blamp(phase, step) - blamp((phase + 0.5).fract(), step);
            naive + 8.0 * step * turns
        }
        SAW => 2.0 * phase - 1.0 - blep(phase, step),
        // SQUARE
        _ => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            naive + blep(phase, step) - blep((phase + 0.5).fract(), step)
        }
    }
}

/// The correction for a jump of 2.0 at phase 0, at `phase`: the difference
/// between a step smoothed over the samples either side and the step.
fn blep(phase: f64, step: f64) -> f64 {
    if phase < step {
        let x = phase / step;
        2.0 * x - x * x - 1.0
    } else if phase > 1.0 - step {
        let x = (phase - 1.0) / step;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// The correction for a turn at phase 0, its slope rising by one a sample,
/// at `phase`: the smoothed step's integral, less the turn.
fn blamp(phase: f64, step: f64) -> f64 {
    let cube = |x: f64| x * x * x / 6.0;
    if phase < step {
        cube(1.0 - phase / step)
    } else if phase > 1.0 - step {
        cube((phase - 1.0) / step + 1.0)
    } else {
        0.0
    }
}

/// The noise generator's first state for a note: anything but zero, and
/// different for each note.
fn noise_seed(started: u64) -> u32 {
    (started as u32).wrapping_mul(0x9E37_79B9) | 1
}

/// White noise from -1.0 to 1.0: Marsaglia's xorshift.
fn white(state: &mut u32) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use musicable_model::native::NativeKind;

    use super::*;
    use crate::devices::instrument::tests::{
        bend, controller, cycles, for_note, frequency, gain_at, hz, level_at, off, on, pedal,
    };
    use crate::devices::testing::{Run, context, modulation, param};

    /// A synth set as given: a plain sine through an open filter, which
    /// sustains at full level with no velocity, unless `settings` say
    /// otherwise.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut synth = NativeDevice::new(NativeKind::Synth);
        for (param, value) in [
            (OSC1_WAVE, SINE as f64),
            (FILTER, 0.0),
            (CUTOFF, 20_000.0),
            (RESONANCE, 0.0),
            (FILTER_ENV, 0.0),
            (KEY_TRACK, 0.0),
            (AMP_ATTACK, 0.1),
            (AMP_SUSTAIN, 100.0),
            (VOLUME, 0.0),
            (VELOCITY, 0.0),
        ]
        .into_iter()
        .chain(settings.iter().copied())
        {
            synth.set(param, value);
        }
        synth
    }

    /// `frames` of `device` playing `events`, the left channel.
    fn play(device: &NativeDevice, frames: usize, events: &[NodeEvent]) -> Vec<f32> {
        let input = [vec![0.0; frames], vec![0.0; frames]];
        let mut output = Run::new(Box::new(Synth::new(device))).render(&input, events);
        assert_eq!(output[0], output[1]);
        output.swap_remove(0)
    }

    fn to_f64(signal: &[f32]) -> Vec<f64> {
        signal.iter().map(|&s| f64::from(s)).collect()
    }

    /// The level of the tone at `freq` Hz in `signal`, at 48 kHz.
    fn level(signal: &[f32], freq: f64) -> f64 {
        level_at(&to_f64(signal), freq, 48_000.0)
    }

    #[test]
    fn notes_play_at_their_pitch() {
        let a = play(&device(&[]), 48_000, &[on(0, 1, 69, 1.0)]);
        assert!((frequency(&a[480..], 48_000.0) - 440.0).abs() < 0.01);
        // Oscillator 2 alone, an octave down, a fifth and 10 cents up.
        let moved = device(&[
            (OSC1_LEVEL, 0.0),
            (OSC1_OCTAVE, -1.0),
            (OSC2_WAVE, SINE as f64),
            (OSC2_LEVEL, 100.0),
            (OSC2_PITCH, 7.0),
            (OSC2_DETUNE, 10.0),
        ]);
        let fifth = play(&moved, 48_000, &[on(0, 1, 69, 1.0)]);
        let expected = hz(69.0 - 12.0 + 7.1);
        assert!((frequency(&fifth[480..], 48_000.0) - expected).abs() < 0.01);
    }

    #[test]
    fn each_note_bends_on_its_own() {
        // Middle C, and a G bent up a whole tone, to an A.
        let events = [on(0, 1, 60, 1.0), on(0, 2, 67, 1.0), bend(0, 2, 2.0)];
        let out = &play(&device(&[]), 48_000, &events)[480..];
        assert!((level(out, hz(60.0)) - 1.0).abs() < 1e-3);
        assert!((level(out, hz(69.0)) - 1.0).abs() < 1e-3);
        assert!(level(out, hz(67.0)) < 1e-4);
    }

    /// A second of `wave` at `freq` Hz, at 48 kHz.
    fn wave(wave: usize, freq: f64) -> Vec<f64> {
        let step = freq / 48_000.0;
        let mut phase = 0.0;
        (0..48_000)
            .map(|_| {
                let s = shape(wave, phase, step);
                phase = (phase + step).fract();
                s
            })
            .collect()
    }

    /// The reference test for the band-limited waves. Each is its ideal
    /// wave with its jumps or turns smoothed by a kernel two samples wide,
    /// a triangle, as two-point PolyBLEPs and PolyBLAMPs make it, so its
    /// harmonics are the ideal wave's, each times the kernel's response at
    /// its frequency, to within a thousandth of a dB: sinc² of the
    /// frequency as a part of the sample rate. The kernel keeps down what
    /// folds back from above half the sample rate: for a D#6, what lands
    /// below 5 kHz is 69 dB down for the saw and the square, and 100 dB
    /// for the triangle.
    #[test]
    fn the_waves_are_their_ideal_waves_smoothed() {
        // D#6. Not a divisor of the sample rate, so what folds back falls
        // between the harmonics.
        let f0 = 1234.5;
        let kernel = |freq: f64| {
            let x = PI * freq / 48_000.0;
            (x.sin() / x).powi(2)
        };
        // Each ideal wave's harmonics, from -1.0 to 1.0.
        let ideal = |wave: usize, n: usize| -> f64 {
            let n = n as f64;
            match wave {
                SAW => 2.0 / (PI * n),
                _ if n % 2.0 == 0.0 => 0.0,
                TRIANGLE => 8.0 / (PI * n).powi(2),
                _ => 4.0 / (PI * n),
            }
        };
        for (shape, aliasing) in [(TRIANGLE, -95.0), (SAW, -65.0), (SQUARE, -65.0)] {
            let signal = wave(shape, f0);
            let level = |freq: f64| level_at(&signal, freq, 48_000.0);
            for n in 1..20 {
                let freq = n as f64 * f0;
                let expected = ideal(shape, n) * kernel(freq);
                let measured = level(freq);
                if expected == 0.0 {
                    assert!(measured < 1e-5, "wave {shape}, harmonic {n}: {measured}");
                } else {
                    let error = 20.0 * (measured / expected).log10();
                    assert!(error.abs() < 1e-3, "wave {shape}, harmonic {n}: {error} dB");
                }
            }
            // The strongest of what folds back below 5 kHz, from the
            // fundamental.
            let mut worst: f64 = 0.0;
            for n in 20..2000 {
                let folded = (n as f64 * f0) % 48_000.0;
                let folded = folded.min(48_000.0 - folded);
                if folded < 5000.0 && ideal(shape, n) > 0.0 {
                    worst = worst.max(level(folded));
                }
            }
            let worst = 20.0 * (worst / level(f0)).log10();
            assert!(worst < aliasing, "wave {shape} aliases at {worst} dB");
        }
    }

    /// The 24 dB low pass with no resonance is a fourth-order Butterworth
    /// filter, under the bilinear transform: flat, then falling at 24 dB an
    /// octave from 3 dB down at the cutoff.
    #[test]
    fn the_24_db_low_pass_is_a_butterworth_filter() {
        let qs = [
            resonance_q(0.0, FRAC_1_SQRT_2),
            resonance_q(0.0, BUTTERWORTH_4[1]),
        ];
        let cutoff = 1000.0;
        let [first, second] = tunings(cutoff, LOW_PASS_24 as f32, qs, 48_000.0);
        let mut filter = [Svf::default(); 2];
        let impulse: Vec<f64> = (0..1 << 15)
            .map(|n| {
                let one = filter[0].tick(&first, if n == 0 { 1.0 } else { 0.0 });
                filter[1].tick(&second, one.low).low
            })
            .collect();
        let warped = (PI * cutoff / 48_000.0).tan();
        let mut freq: f64 = 20.0;
        while freq < 20_000.0 {
            let w = (PI * freq / 48_000.0).tan() / warped;
            let design = 1.0 / (1.0 + w.powi(8)).sqrt();
            let measured = gain_at(&impulse, freq, 48_000.0);
            let error = 20.0 * (measured / design).log10();
            assert!(error.abs() < 1e-3, "{freq} Hz: {error} dB");
            freq *= 1.05;
        }
    }

    /// Where the filter's cutoff is: a sine at the cutoff comes through the
    /// 12 dB low pass 3 dB down with no resonance, and at the Q the
    /// resonance gives otherwise.
    #[test]
    fn the_cutoff_follows_the_filter_envelope_and_the_keyboard() {
        // C5: an octave up from C4 with full key tracking, and an octave up
        // with the filter envelope sustaining at its peak.
        let c5 = hz(72.0);
        for (resonance, q) in [(0.0, FRAC_1_SQRT_2), (50.0, (25.0 * FRAC_1_SQRT_2).sqrt())] {
            let synth = device(&[
                (CUTOFF, c5 / 4.0),
                (RESONANCE, resonance),
                (KEY_TRACK, 100.0),
                (FILTER_ENV, 12.0),
                (FILTER_ATTACK, 0.1),
                (FILTER_SUSTAIN, 100.0),
            ]);
            let out = play(&synth, 48_000, &[on(0, 1, 72, 1.0)]);
            let measured = level(&out[4800..], c5);
            assert!(
                (measured / q - 1.0).abs() < 1e-3,
                "{resonance}%: {measured}"
            );
        }
    }

    #[test]
    fn velocity_and_volume_set_the_level() {
        // Half the way from full at half velocity, and 6 dB down.
        let synth = device(&[(VELOCITY, 50.0), (VOLUME, -6.0)]);
        let out = play(&synth, 48_000, &[on(0, 1, 69, 0.5)]);
        let expected = 0.75 * decibels_to_gain(-6.0);
        assert!((level(&out[480..], 440.0) / expected - 1.0).abs() < 1e-3);
    }

    /// The amp envelope shapes each note. With a square wave so slow it
    /// holds at 1.0 for the whole test, the synth plays the envelope's
    /// level on every frame.
    #[test]
    fn notes_follow_their_envelope_and_end_their_release_after_letting_go() {
        // A 10 ms attack, a 20 ms decay to half and a 30 ms release.
        let synth = device(&[
            (OSC1_WAVE, SQUARE as f64),
            (OSC1_OCTAVE, -3.0),
            (AMP_ATTACK, 10.0),
            (AMP_DECAY, 20.0),
            (AMP_SUSTAIN, 50.0),
            (AMP_RELEASE, 30.0),
        ]);
        // The lowest key three octaves down: 1 Hz, high for half a second.
        let out = play(&synth, 9600, &[on(0, 1, 0, 1.0), off(4800, 1)]);
        let mut envelope = Envelope::default();
        envelope.start();
        for (i, &sample) in out.iter().enumerate() {
            if i == 4800 {
                envelope.release();
            }
            let level = envelope.next([10.0, 20.0, 0.5, 30.0], 48.0);
            // Past the first frames, where the square's rise rings through
            // the filter.
            if i >= 50 {
                assert!((f64::from(sample) - level).abs() < 1e-6, "frame {i}");
            }
        }
        // Silent from the last of the release's 30 ms, its 1440th frame.
        assert!(out[4800 + 1438] > 0.0);
        assert!(out[4800 + 1439..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_engine_hears_which_notes_voices_end() {
        let synth = device(&[(AMP_RELEASE, 1.0)]);
        let mut node = Synth::new(&synth);
        node.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 512,
        });
        let mut out = vec![0.0; 2 * 512];
        let mut block = |node: &mut Synth, events: &[NodeEvent]| {
            let ctx = context(512, events);
            let mut output = AudioMut::from_planar(&mut out, 2);
            node.process(&ctx, Inputs::from_views(&[]), &mut output);
            node.ended_voices().to_vec()
        };
        assert_eq!(
            block(&mut node, &[on(0, 7, 60, 1.0), on(10, 8, 64, 1.0)]),
            []
        );
        // Let go, it rings for 1 ms, 48 frames.
        assert_eq!(block(&mut node, &[off(100, 7)]), [NoteId(7)]);
        assert_eq!(block(&mut node, &[]), []);
    }

    #[test]
    fn a_note_past_the_voices_takes_the_oldest_ones_place_without_a_click() {
        let synth = device(&[(VOICES, 2.0)]);
        let mut node = Synth::new(&synth);
        node.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 4800,
        });
        let mut out = vec![0.0; 2 * 4800];
        let events = [
            on(0, 1, 60, 1.0),
            on(0, 2, 64, 1.0),
            off(1000, 2),
            on(2000, 3, 67, 1.0),
            on(3000, 4, 72, 1.0),
        ];
        let ctx = context(4800, &events);
        let mut output = AudioMut::from_planar(&mut out, 2);
        node.process(&ctx, Inputs::from_views(&[]), &mut output);
        // The third note takes the second's voice, let go, and the fourth
        // the first's, the oldest held: each fades out in 5 ms.
        assert_eq!(node.ended_voices(), [NoteId(2), NoteId(1)]);
        let mut sounding: Vec<u8> = (node.voices.slots.iter())
            .filter(|v| v.playing.is_sounding())
            .map(|v| v.playing.key)
            .collect();
        sounding.sort();
        assert_eq!(sounding, [67, 72]);
        // Nothing jumps: the most a frame moves is what the tones do.
        let steps = out[..4800].windows(2).map(|w| (w[1] - w[0]).abs());
        let largest = steps.fold(0.0_f32, f32::max);
        assert!(largest < 0.25, "{largest}");
    }

    #[test]
    fn the_sustain_pedal_holds_notes_let_go_until_it_comes_up() {
        let synth = device(&[(OSC2_LEVEL, 50.0), (NOISE, 20.0), (AMP_RELEASE, 30.0)]);
        // Let go with the pedal down, a note plays on, as one let go when
        // the pedal comes up does.
        let late = play(&synth, 9600, &[on(0, 1, 60, 1.0), off(6000, 1)]);
        let events = [
            on(0, 1, 60, 1.0),
            pedal(500, true),
            off(1000, 1),
            pedal(6000, false),
        ];
        assert_eq!(play(&synth, 9600, &events), late);
        // Let go before the pedal goes down, or with another controller
        // all the way up, a note stops as it would without them.
        let early = play(&synth, 9600, &[on(0, 1, 60, 1.0), off(1000, 1)]);
        assert_ne!(early, late);
        let events = [on(0, 1, 60, 1.0), off(1000, 1), pedal(2000, true)];
        assert_eq!(play(&synth, 9600, &events), early);
        let wheel = controller(500, Controller::MOD_WHEEL, 1.0);
        assert_eq!(
            play(&synth, 9600, &[on(0, 1, 60, 1.0), wheel, off(1000, 1)]),
            early
        );
    }

    #[test]
    fn a_note_past_the_voices_takes_one_the_pedal_holds_before_a_held_one() {
        let synth = device(&[(VOICES, 2.0)]);
        let mut node = Synth::new(&synth);
        node.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 4800,
        });
        let mut out = vec![0.0; 2 * 4800];
        let events = [
            on(0, 1, 60, 1.0),
            pedal(0, true),
            on(100, 2, 64, 1.0),
            off(200, 2),
            on(300, 3, 67, 1.0),
        ];
        let ctx = context(4800, &events);
        let mut output = AudioMut::from_planar(&mut out, 2);
        node.process(&ctx, Inputs::from_views(&[]), &mut output);
        // The third note takes the second's voice, which the pedal holds
        // on, rather than the first's, which is older but held.
        assert_eq!(node.ended_voices(), [NoteId(2)]);
        let mut sounding: Vec<u8> = (node.voices.slots.iter())
            .filter(|v| v.playing.is_sounding())
            .map(|v| v.playing.key)
            .collect();
        sounding.sort();
        assert_eq!(sounding, [60, 67]);
    }

    #[test]
    fn notes_glide_from_the_last_notes_pitch() {
        // One voice, gliding over 100 ms: 4800 frames.
        let synth = device(&[(VOICES, 1.0), (GLIDE, 100.0)]);
        let out = play(&synth, 19_200, &[on(0, 1, 57, 1.0), on(9600, 2, 69, 1.0)]);
        // From A3 to A4, evenly in semitones, each cycle at the pitch of
        // its middle; but for the 5 ms the first note's voice fades out.
        let fading = 9600.0..9600.0 + 240.0;
        for (start, end) in cycles(&out) {
            if start < fading.end && end > fading.start {
                continue;
            }
            let along = (((start + end) / 2.0 - 9600.0) / 4800.0).clamp(0.0, 1.0);
            let expected = hz(57.0 + 12.0 * along);
            let measured = 48_000.0 / (end - start);
            assert!(
                (measured / expected - 1.0).abs() < 1e-3,
                "{start}: {measured} Hz"
            );
        }
    }

    #[test]
    fn modulation_for_a_note_moves_its_voice_alone() {
        let events = [
            on(0, 1, 60, 1.0),
            on(0, 2, 72, 1.0),
            // Note 2's level down by half; a choice can't be modulated.
            for_note(0, 2, OSC1_LEVEL, -50.0),
            for_note(0, 2, OSC1_WAVE, 2.0),
        ];
        let out = &play(&device(&[]), 48_000, &events)[480..];
        assert!((level(out, hz(60.0)) - 1.0).abs() < 1e-3);
        assert!((level(out, hz(72.0)) - 0.5).abs() < 1e-3);
        // Still a sine: nothing at its octave.
        assert!(level(out, hz(84.0)) < 1e-4);
    }

    /// Every setting moving, and notes coming and going.
    fn busy() -> (NativeDevice, Vec<NodeEvent>) {
        let mut synth = NativeDevice::new(NativeKind::Synth);
        synth.set(OSC2_LEVEL, 60.0);
        synth.set(OSC2_DETUNE, 7.0);
        synth.set(NOISE, 10.0);
        synth.set(RESONANCE, 70.0);
        synth.set(GLIDE, 30.0);
        synth.set(VOICES, 3.0);
        let events = vec![
            on(0, 1, 48, 0.8),
            on(0, 2, 55, 0.5),
            param(300, CUTOFF, 400.0),
            on(700, 3, 60, 1.0),
            bend(900, 3, -1.5),
            modulation(1200, RESONANCE, 20.0),
            on(1500, 4, 67, 0.3),
            for_note(1500, 4, CUTOFF, 3000.0),
            pedal(1800, true),
            off(2100, 1),
            param(2500, FILTER, 3.0),
            param(2600, OSC1_WAVE, 3.0),
            off(3001, 3),
            on(3001, 5, 72, 0.9),
            param(4000, VOLUME, -12.0),
            off(5000, 4),
            off(5000, 5),
            off(5001, 2),
            pedal(5500, false),
        ];
        (synth, events)
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let (synth, events) = busy();
        let silence = [vec![0.0; 9000], vec![0.0; 9000]];
        let render = |block| {
            Run::new(Box::new(Synth::new(&synth)))
                .with_block(block)
                .render(&silence, &events)
        };
        let reference = render(512);
        assert!(reference[0].iter().any(|&s| s.abs() > 0.1));
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let (synth, events) = busy();
        let silence = [vec![0.0; 9000], vec![0.0; 9000]];
        let whole = Run::new(Box::new(Synth::new(&synth))).render(&silence, &events);
        let (first, second): (Vec<NodeEvent>, Vec<NodeEvent>) =
            events.iter().partition(|e| e.offset < 2000);
        let later: Vec<NodeEvent> = (second.iter())
            .map(|e| NodeEvent {
                offset: e.offset - 2000,
                ..*e
            })
            .collect();
        // A node made from the project with the value set before the swap
        // carries on just as the old one would have, and so does the
        // modulation of a parameter something still modulates.
        let mut moved = synth.clone();
        moved.set(CUTOFF, 400.0);
        let mut run = Run::new(Box::new(Synth::new(&synth)));
        let mut swapped = run.render(&[vec![0.0; 2000], vec![0.0; 2000]], &first);
        let mut new = Box::new(Synth::new(&moved));
        new.modulated(ParamId(RESONANCE as u64));
        run.swap(new);
        let rest = run.render(&[vec![0.0; 7000], vec![0.0; 7000]], &later);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }

    #[test]
    fn audio_coming_in_passes_through() {
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let synth = NativeDevice::new(NativeKind::Synth);
        let output =
            Run::new(Box::new(Synth::new(&synth))).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(output, [input.clone(), input]);
    }
}

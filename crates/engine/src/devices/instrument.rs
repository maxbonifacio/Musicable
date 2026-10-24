//! What the synth and the sampler share: their voices and the notes the
//! voices play, with modulation for one note, their envelopes and their
//! filter.
//!
//! Each voice plays a note, from its start until its sound ends. While the
//! sustain pedal is down, a note let go plays on as if held until the pedal
//! comes up. A note past the instrument's `voices` takes the place of the
//! voice let go longest ago, then of one the pedal holds on, or else of the
//! oldest, and that voice fades out over [`STEAL_SECONDS`] beside it, so
//! taking it doesn't click: an instrument has a slot for each voice it
//! plays and as many again for voices fading out.

use std::f64::consts::PI;

use musicable_model::clip::Controller;
use musicable_model::native::ParamInfo;

use super::{Span, Values};
use crate::ProcessContext;
use crate::event::{NodeEvent, NoteId};

/// The most voices an instrument plays at once: the top of its `voices`.
pub(crate) const MAX_VOICES: usize = 32;

/// Voice slots: one for each voice, and as many for voices fading out.
pub(crate) const SLOTS: usize = 2 * MAX_VOICES;

/// How long a voice fades out once a new note takes its place.
const STEAL_SECONDS: f64 = 0.005;

/// How many of an instrument's parameters a voice has modulation of its
/// own for: the first this many in its table.
pub(crate) const VOICE_PARAMS: usize = 48;

/// The most notes [`Voices::ended`] holds for a block. Past them, the
/// notes whose voices end aren't told.
const ENDED: usize = 2 * SLOTS;

/// What the sample rate makes of times and the like.
pub(crate) struct Rates {
    pub(crate) sample_rate: f64,
    pub(crate) frames_per_ms: f64,
    /// How far a stolen voice fades in a frame.
    pub(crate) steal: f32,
}

impl Rates {
    pub(crate) fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            frames_per_ms: sample_rate / 1000.0,
            steal: (1.0 / (STEAL_SECONDS * sample_rate)) as f32,
        }
    }
}

/// Where a voice is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum State {
    /// Its slot is free.
    #[default]
    Free,
    /// Its note is held.
    Held,
    /// Its note was let go, and it rings out.
    Released,
    /// A new note took its place, and it fades out.
    Stolen,
}

/// What every voice has, whatever the instrument: its note, and the
/// modulation for that note alone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Playing {
    pub(crate) state: State,
    /// Its note, until another note with the same id starts.
    pub(crate) note: Option<NoteId>,
    /// Which note it was, counting from the first, to tell which voice is
    /// oldest.
    pub(crate) started: u64,
    /// The MIDI note number.
    pub(crate) key: u8,
    /// From 0.0 to 1.0.
    pub(crate) velocity: f32,
    /// The note's pitch bend, in semitones.
    pub(crate) bend: f32,
    /// Whether its note was let go while the sustain pedal was down, so it
    /// plays on as if held until the pedal comes up.
    pedal: bool,
    /// Its level while it fades out once stolen, from 1.0 to 0.0.
    pub(crate) fade: f32,
    /// The modulation for its note alone of each of the first
    /// [`VOICE_PARAMS`] parameters, on top of the device's own.
    modulation: [f32; VOICE_PARAMS],
    /// Whether any of it isn't 0.
    modulated: bool,
}

impl Default for Playing {
    fn default() -> Self {
        Self {
            state: State::Free,
            note: None,
            started: 0,
            key: 0,
            velocity: 0.0,
            bend: 0.0,
            pedal: false,
            fade: 1.0,
            modulation: [0.0; VOICE_PARAMS],
            modulated: false,
        }
    }
}

impl Playing {
    /// Whether it's playing a note that sounds: held or ringing out, but
    /// not fading out for a new note.
    pub(crate) fn is_sounding(&self) -> bool {
        matches!(self.state, State::Held | State::Released)
    }

    /// Parameter `param`'s values over `values`' span for this voice:
    /// `info`'s parameter's, with this voice's own modulation added.
    pub(crate) fn lane<'a>(
        &self,
        values: &'a Values,
        info: &[ParamInfo],
        param: usize,
    ) -> Lane<'a> {
        let offset = match self.modulated {
            true => self.modulation.get(param).copied().unwrap_or(0.0),
            false => 0.0,
        };
        Lane {
            frames: values.frames(param),
            offset,
            min: info[param].min as f32,
            max: info[param].max as f32,
        }
    }
}

/// One parameter's values over a span for one voice: the device's, with
/// the voice's own modulation added and kept in the range, as
/// [`Param`](super::Param) keeps the device's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lane<'a> {
    frames: &'a [f32],
    offset: f32,
    min: f32,
    max: f32,
}

impl Lane<'_> {
    /// The value on frame `i` of the span.
    #[inline]
    pub(crate) fn at(&self, i: usize) -> f32 {
        let base = self.frames[i];
        if self.offset == 0.0 {
            base
        } else {
            (base + self.offset).clamp(self.min.min(base), self.max.max(base))
        }
    }
}

/// A voice of an instrument.
pub(crate) trait Voice: Copy + Default {
    fn playing(&self) -> &Playing;

    fn playing_mut(&mut self) -> &mut Playing;
}

/// An instrument's voices, in their slots.
#[derive(Debug, Clone)]
pub(crate) struct Voices<V> {
    pub(crate) slots: [V; SLOTS],
    /// Notes started so far, to tell which voice is oldest.
    started: u64,
    /// Whether the sustain pedal is down.
    sustain: bool,
    /// The notes whose voices ended in the block being rendered.
    ended: Vec<NoteId>,
}

impl<V: Voice> Voices<V> {
    /// Allocates, so call it off the audio thread.
    pub(crate) fn new() -> Self {
        Self {
            slots: [V::default(); SLOTS],
            started: 0,
            sustain: false,
            ended: Vec::with_capacity(ENDED),
        }
    }

    /// From [`Node::adopt`](crate::Node::adopt): carries on with the
    /// voices of the instrument this one replaces.
    pub(crate) fn take_over(&mut self, previous: &Self) {
        self.slots = previous.slots;
        self.started = previous.started;
        self.sustain = previous.sustain;
    }

    /// The notes whose voices ended in the block last rendered.
    pub(crate) fn ended(&self) -> &[NoteId] {
        &self.ended
    }

    /// Starts a block: the notes whose voices end in it are told afresh.
    pub(crate) fn start_block(&mut self) {
        self.ended.clear();
    }

    /// The voices playing `note`.
    pub(crate) fn with_note(&mut self, note: NoteId) -> impl Iterator<Item = &mut V> {
        (self.slots.iter_mut())
            .filter(move |v| v.playing().state != State::Free && v.playing().note == Some(note))
    }

    /// Takes a slot for `note`, which starts on `key` at `velocity`, with
    /// at most `limit` voices sounding, counting it. The voices beyond
    /// that are stolen: the one let go longest ago, then one the sustain
    /// pedal holds on, or else the oldest. Returns the slot, whose voice
    /// is set up but for what's the instrument's own, which the caller
    /// sets up. A voice already playing the note must be let go first: a
    /// note's id means the new voice from now on.
    pub(crate) fn start(&mut self, note: NoteId, key: u8, velocity: f32, limit: usize) -> usize {
        let limit = limit.clamp(1, MAX_VOICES);
        loop {
            let sounding = (self.slots.iter())
                .filter(|v| v.playing().is_sounding())
                .count();
            if sounding < limit {
                break;
            }
            let oldest = |stealable: fn(&Playing) -> bool| {
                (0..SLOTS)
                    .filter(|&s| stealable(self.slots[s].playing()))
                    .min_by_key(|&s| self.slots[s].playing().started)
            };
            let stolen = oldest(|p| p.state == State::Released)
                .or_else(|| oldest(|p| p.state == State::Held && p.pedal))
                .or_else(|| oldest(|p| p.state == State::Held))
                .expect("a voice sounds");
            self.slots[stolen].playing_mut().state = State::Stolen;
        }
        // A free slot, or else the fading voice nearest its end, cut short.
        let slot = match (0..SLOTS).find(|&s| self.slots[s].playing().state == State::Free) {
            Some(free) => free,
            None => {
                let fading = (0..SLOTS)
                    .min_by(|&a, &b| {
                        let fade = |s: usize| self.slots[s].playing().fade;
                        fade(a).total_cmp(&fade(b))
                    })
                    .expect("an instrument has slots");
                self.end(fading);
                fading
            }
        };
        self.started += 1;
        let mut voice = V::default();
        *voice.playing_mut() = Playing {
            state: State::Held,
            note: Some(note),
            started: self.started,
            key,
            velocity,
            ..Playing::default()
        };
        self.slots[slot] = voice;
        slot
    }

    /// Lets `note` go: its voices release, with `release`, or play on while
    /// the sustain pedal is down, until it comes up.
    pub(crate) fn let_go(&mut self, note: NoteId, mut release: impl FnMut(&mut V)) {
        let sustain = self.sustain;
        for voice in self.with_note(note) {
            if sustain && voice.playing().state == State::Held {
                voice.playing_mut().pedal = true;
            } else {
                release(voice);
            }
        }
    }

    /// Moves the sustain pedal to `value`, from 0.0 to 1.0: it's down from
    /// halfway, as MIDI's switches are. Coming up, it lets go of the notes
    /// it held on, with `release`.
    pub(crate) fn pedal(&mut self, value: f32, mut release: impl FnMut(&mut V)) {
        self.sustain = Controller::is_on(value);
        if !self.sustain {
            for voice in self.slots.iter_mut().filter(|v| v.playing().pedal) {
                voice.playing_mut().pedal = false;
                release(voice);
            }
        }
    }

    /// Modulates parameter `param` of the voices playing `note` by
    /// `amount`, replacing the last amount for the note, if `info` says
    /// modulators can move it.
    pub(crate) fn modulate(&mut self, note: NoteId, param: usize, amount: f32, info: &[ParamInfo]) {
        if param >= VOICE_PARAMS || !info.get(param).is_some_and(|p| p.modulated) {
            return;
        }
        for voice in self.with_note(note) {
            let playing = voice.playing_mut();
            playing.modulation[param] = amount;
            playing.modulated = playing.modulation.iter().any(|&m| m != 0.0);
        }
    }

    /// Ends the voice in `slot`, telling its note, and frees the slot.
    pub(crate) fn end(&mut self, slot: usize) {
        if let Some(note) = self.slots[slot].playing().note
            && self.ended.len() < self.ended.capacity()
        {
            self.ended.push(note);
        }
        self.slots[slot] = V::default();
    }

    /// Whether any voice is playing.
    pub(crate) fn any(&self) -> bool {
        (self.slots.iter()).any(|v| v.playing().state != State::Free)
    }
}

/// Picks out the events each span with frames starts with.
///
/// [`Params::run`](super::Params::run) hands each event to a span of its
/// own. An event on the same frame as the next one gets a span without
/// frames, whose values aren't worked out. An instrument handles each
/// frame's events together, with that frame's values, so it takes them
/// with the next span that has frames: the one with the frame's last
/// event, which follows at once.
#[derive(Debug, Default)]
pub(crate) struct Cursor {
    /// The spans with an event so far, which are the block's events so far.
    seen: usize,
    /// The first event on this frame, if it came with a span without
    /// frames.
    pending: Option<usize>,
}

impl Cursor {
    /// The events `span`, of the block `ctx` renders, starts with, in
    /// order: none for a span without an event, and every event on its
    /// first frame for one with an event. `None` for a span without
    /// frames, whose events come with the next span.
    pub(crate) fn events<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        span: &Span<'_>,
    ) -> Option<&'a [NodeEvent]> {
        let Some(event) = span.event else {
            return Some(&[]);
        };
        self.seen += 1;
        let at = self.seen - 1;
        debug_assert!(std::ptr::eq(event, &ctx.events[at]));
        if span.frames.is_empty() {
            self.pending.get_or_insert(at);
            return None;
        }
        let from = self.pending.take().unwrap_or(at);
        Some(&ctx.events[from..self.seen])
    }
}

/// An envelope's stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Stage {
    /// Silent, before it starts and once it has finished.
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// An attack, decay, sustain and release envelope, from 0.0 to 1.0.
///
/// Each stage takes its time exactly: the attack rises from where it starts
/// to the peak, the decay falls from the peak to the sustain level and the
/// release from where it starts to silence. Each curves as an analog
/// envelope's does, heading for a point a little past its end and stopping
/// there: the attack for 30% past the peak, so it slows as it gets there,
/// and the decay and the release for 0.01% past their ends, so they fall
/// fast at first and slow towards the end, by 40 dB in the first half of
/// the release. A stage's time can change as it goes, and it carries on at
/// the new rate.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Envelope {
    pub(crate) stage: Stage,
    level: f64,
    /// The level the stage started from.
    from: f64,
    /// The last stage time worked out and its coefficient, for the attack,
    /// the decay and the release.
    rates: [(f32, f64); 3],
}

/// How far past their ends the stages head, for the attack and for the
/// decay and the release: each as a part of the stage's distance.
const ATTACK_OVERSHOOT: f64 = 0.3;
const DECAY_OVERSHOOT: f64 = 0.0001;

/// How near its end a stage has to get to have got there, for the
/// rounding on the way, so it takes its time to the frame.
const ROUNDING: f64 = 1e-9;

impl Envelope {
    /// Starts the attack, from the level it's at.
    pub(crate) fn start(&mut self) {
        self.stage = Stage::Attack;
        self.from = self.level;
    }

    /// Starts the release, from the level it's at, unless it has finished.
    pub(crate) fn release(&mut self) {
        if self.stage != Stage::Off {
            self.stage = Stage::Release;
            self.from = self.level;
        }
    }

    /// The level on the next frame, with the stages' times in ms, the
    /// sustain level from 0.0 to 1.0, and `frames_per_ms` frames in a ms.
    #[inline]
    pub(crate) fn next(
        &mut self,
        [attack, decay, sustain, release]: [f32; 4],
        frames_per_ms: f64,
    ) -> f64 {
        let sustain = f64::from(sustain).clamp(0.0, 1.0);
        match self.stage {
            Stage::Off => self.level = 0.0,
            Stage::Attack => {
                let target = 1.0 + ATTACK_OVERSHOOT * (1.0 - self.from);
                let rate = self.rate(0, attack, ATTACK_OVERSHOOT, frames_per_ms);
                self.level = target + (self.level - target) * rate;
                if self.level >= 1.0 - ROUNDING {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let target = sustain - DECAY_OVERSHOOT * (1.0 - sustain);
                let rate = self.rate(1, decay, DECAY_OVERSHOOT, frames_per_ms);
                self.level = target + (self.level - target) * rate;
                if self.level <= sustain + ROUNDING {
                    self.level = sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = sustain,
            Stage::Release => {
                let target = -DECAY_OVERSHOOT * self.from;
                let rate = self.rate(2, release, DECAY_OVERSHOOT, frames_per_ms);
                self.level = target + (self.level - target) * rate;
                if self.level <= ROUNDING {
                    self.level = 0.0;
                    self.stage = Stage::Off;
                }
            }
        }
        self.level
    }

    /// How much of the distance to its target stage `which` keeps each
    /// frame, for it to get from its start to its end in `ms`, heading
    /// `overshoot` past.
    fn rate(&mut self, which: usize, ms: f32, overshoot: f64, frames_per_ms: f64) -> f64 {
        let (time, rate) = &mut self.rates[which];
        if *time != ms || *rate == 0.0 {
            let frames = (f64::from(ms) * frames_per_ms).max(1.0);
            *time = ms;
            *rate = (-((1.0 + overshoot) / overshoot).ln() / frames).exp();
        }
        *rate
    }
}

/// A choice's value, which can be between two choices as it ramps from one
/// to another: the choice at or below it, from 0 to `last`, and how far it
/// is to the next, from 0.0 to 1.0.
pub(crate) fn split(value: f32, last: usize) -> (usize, f64) {
    let value = f64::from(value).clamp(0.0, last as f64);
    let below = value.floor();
    (below as usize, value - below)
}

/// The Q of a filter at `resonance`, from 0.0 to 1.0: `flat` with none,
/// for no peak, up to [`MAX_Q`] at full, evenly in ratios between.
pub(crate) fn resonance_q(resonance: f64, flat: f64) -> f64 {
    flat * (MAX_Q / flat).powf(resonance.clamp(0.0, 1.0))
}

/// The Q at full resonance: a peak 28 dB up, ringing for long but short of
/// oscillating.
pub(crate) const MAX_Q: f64 = 25.0;

/// The lowest and highest a filter's cutoff goes, the highest as a part of
/// the sample rate.
const LOWEST_CUTOFF: f64 = 10.0;
const HIGHEST_CUTOFF: f64 = 0.49;

/// A filter's cutoff of `freq` Hz, kept to where the filter works, warped
/// for the trapezoidal integrators: `g` in Simper's SVF.
pub(crate) fn warp(freq: f64, sample_rate: f64) -> f64 {
    let freq = freq.clamp(LOWEST_CUTOFF, HIGHEST_CUTOFF * sample_rate);
    (PI * freq / sample_rate).tan()
}

/// A state variable filter's coefficients: Andrew Simper's trapezoidal SVF,
/// as the EQ's (`svf.rs`), here with all three of its outputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tuning {
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
}

impl Tuning {
    /// For a cutoff warped to `g` ([`warp`]) and a Q of `q`.
    pub(crate) fn new(g: f64, q: f64) -> Self {
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

/// What a state variable filter puts out for a sample: the low pass, the
/// band pass, 0 dB at its peak, and the high pass. Each is the analog
/// prototype's under the bilinear transform, warped to match at the
/// cutoff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Outputs {
    pub(crate) low: f64,
    pub(crate) band: f64,
    pub(crate) high: f64,
}

/// One channel's state variable filter memory.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Svf {
    ic1eq: f64,
    ic2eq: f64,
}

impl Svf {
    /// Filters one sample.
    #[inline]
    pub(crate) fn tick(&mut self, t: &Tuning, v0: f64) -> Outputs {
        let v3 = v0 - self.ic2eq;
        let v1 = t.a1 * self.ic1eq + t.a2 * v3;
        let v2 = self.ic2eq + t.a2 * self.ic1eq + t.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        Outputs {
            low: v2,
            band: t.k * v1,
            high: v0 - t.k * v1 - v2,
        }
    }
}

/// What the instruments' tests share: notes, and measuring what they play.
#[cfg(test)]
pub(crate) mod tests {
    use std::f64::consts::FRAC_1_SQRT_2;

    use musicable_model::clip::Dimension;
    use musicable_model::native::{NativeDevice, NativeKind};

    use super::*;
    use crate::devices::Params;
    use crate::devices::testing::{context, param};
    use crate::event::{NodeEventKind, NoteEvent, ParamId};

    /// Note `note` starting on frame `at`, on `key` at `velocity`.
    pub(crate) fn on(at: usize, note: u64, key: u8, velocity: f32) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Note(NoteEvent::On {
                note: NoteId(note),
                key,
                velocity,
            }),
        }
    }

    /// Note `note` let go on frame `at`.
    pub(crate) fn off(at: usize, note: u64) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Note(NoteEvent::Off {
                note: NoteId(note),
                velocity: 0.5,
            }),
        }
    }

    /// Note `note` bent by `semitones` from frame `at`.
    pub(crate) fn bend(at: usize, note: u64, semitones: f32) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Note(NoteEvent::Expression {
                note: NoteId(note),
                dimension: Dimension::PitchBend,
                value: semitones,
            }),
        }
    }

    /// Controller `controller` moving to `value` on frame `at`.
    pub(crate) fn controller(at: usize, controller: Controller, value: f32) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Controller { controller, value },
        }
    }

    /// The sustain pedal going down on frame `at`, or coming up.
    pub(crate) fn pedal(at: usize, down: bool) -> NodeEvent {
        controller(at, Controller::SUSTAIN, if down { 1.0 } else { 0.0 })
    }

    /// Modulation of parameter `param` by `amount` for note `note` alone,
    /// from frame `at`.
    pub(crate) fn for_note(at: usize, note: u64, param: usize, amount: f64) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Modulation {
                param: ParamId(param as u32),
                amount,
                note: Some(NoteId(note)),
            },
        }
    }

    /// The frequency of MIDI note `key`, in Hz.
    pub(crate) fn hz(key: f64) -> f64 {
        440.0 * ((key - 69.0) / 12.0).exp2()
    }

    /// Each cycle of the tone in `signal`, from one rising zero crossing to
    /// the next: where it starts and ends, in frames, between frames.
    pub(crate) fn cycles(signal: &[f32]) -> Vec<(f64, f64)> {
        let mut crossings = Vec::new();
        for (i, pair) in signal.windows(2).enumerate() {
            if pair[0] < 0.0 && pair[1] >= 0.0 {
                let fraction = f64::from(pair[0] / (pair[0] - pair[1]));
                crossings.push(i as f64 + fraction);
            }
        }
        crossings.windows(2).map(|w| (w[0], w[1])).collect()
    }

    /// The frequency of the tone in `signal`, from its first rising zero
    /// crossing to its last.
    pub(crate) fn frequency(signal: &[f32], sample_rate: f64) -> f64 {
        let cycles = cycles(signal);
        let (first, last) = (cycles[0].0, cycles[cycles.len() - 1].1);
        cycles.len() as f64 * sample_rate / (last - first)
    }

    /// The gain of `impulse` at `freq` Hz: its discrete-time Fourier
    /// transform there, as a linear magnitude.
    pub(crate) fn gain_at(impulse: &[f64], freq: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &h) in impulse.iter().enumerate() {
            let phase = w * n as f64;
            re += h * phase.cos();
            im -= h * phase.sin();
        }
        re.hypot(im)
    }

    /// A Blackman-Harris window's weight at `n` of `len`.
    fn window(n: usize, len: usize) -> f64 {
        let x = 2.0 * PI * n as f64 / (len - 1) as f64;
        0.358_75 - 0.488_29 * x.cos() + 0.141_28 * (2.0 * x).cos() - 0.011_68 * (3.0 * x).cos()
    }

    /// The amplitude of the tone at `freq` Hz in `signal`: 1.0 for a sine
    /// from -1.0 to 1.0. It's measured through a window, so tones a few Hz
    /// away hardly count, and the signal needn't hold whole cycles.
    pub(crate) fn level_at(signal: &[f64], freq: f64, sample_rate: f64) -> f64 {
        let len = signal.len();
        let windowed: Vec<f64> = (signal.iter().enumerate())
            .map(|(n, &s)| s * window(n, len))
            .collect();
        let sum: f64 = (0..len).map(|n| window(n, len)).sum();
        2.0 * gain_at(&windowed, freq, sample_rate) / sum
    }

    /// The reference test for the filter: each output's measured response
    /// is its analog prototype's under the bilinear transform, matched at
    /// the cutoff, to within a thousandth of a dB.
    #[test]
    fn the_filters_responses_match_their_analog_prototypes() {
        let sample_rate = 48_000.0;
        for (cutoff, q) in [(500.0, FRAC_1_SQRT_2), (2000.0, 4.0), (12_000.0, 0.5)] {
            let tuning = Tuning::new(warp(cutoff, sample_rate), q);
            let mut svf = Svf::default();
            let impulse: Vec<Outputs> = (0..1 << 15)
                .map(|n| svf.tick(&tuning, if n == 0 { 1.0 } else { 0.0 }))
                .collect();
            let warped = (PI * cutoff / sample_rate).tan();
            let mut freq: f64 = 20.0;
            while freq < 20_000.0 {
                // s on the unit circle, mapped back through the transform.
                let w = (PI * freq / sample_rate).tan() / warped;
                let denominator = (1.0 - w * w).hypot(w / q);
                let designs = [1.0 / denominator, w / q / denominator, w * w / denominator];
                let outputs: [fn(&Outputs) -> f64; 3] = [|o| o.low, |o| o.band, |o| o.high];
                for (design, output) in designs.into_iter().zip(outputs) {
                    let response: Vec<f64> = impulse.iter().map(output).collect();
                    let measured = gain_at(&response, freq, sample_rate);
                    let error = 20.0 * (measured / design).log10();
                    assert!(error.abs() < 1e-3, "{cutoff} Hz Q {q}, at {freq:.0} Hz");
                }
                freq *= 1.05;
            }
        }
    }

    /// An envelope's levels at 1 kHz, with `settings` (the times and the
    /// sustain level): `held` frames with its note held, then `released`.
    fn envelope(settings: [f32; 4], held: usize, released: usize) -> Vec<f64> {
        let mut envelope = Envelope::default();
        envelope.start();
        let mut levels: Vec<f64> = (0..held).map(|_| envelope.next(settings, 1.0)).collect();
        envelope.release();
        levels.extend((0..released).map(|_| envelope.next(settings, 1.0)));
        levels
    }

    #[test]
    fn each_stage_takes_its_time() {
        // At 1 kHz, a frame a ms: a 10 ms attack, a 20 ms decay to half,
        // and a 30 ms release.
        let levels = envelope([10.0, 20.0, 0.5, 30.0], 100, 40);
        assert!(levels[8] < 1.0 && levels[9] == 1.0, "{:?}", &levels[..12]);
        // Rising ever slower.
        let rises: Vec<f64> = levels[..10].windows(2).map(|w| w[1] - w[0]).collect();
        assert!(rises.windows(2).all(|w| w[1] < w[0]));
        assert!(
            levels[28] > 0.5 && levels[29] == 0.5,
            "{:?}",
            &levels[9..31]
        );
        assert!(levels[30..100].iter().all(|&l| l == 0.5));
        // Down 40 dB by halfway through the release, and silent at its end.
        let half = levels[100 + 14] / 0.5;
        assert!((0.008..0.012).contains(&half), "{half}");
        assert!(
            levels[128] > 0.0 && levels[129] == 0.0,
            "{:?}",
            &levels[100..]
        );
    }

    #[test]
    fn letting_go_early_releases_from_where_the_envelope_is() {
        let levels = envelope([10.0, 20.0, 0.5, 30.0], 5, 40);
        let at = levels[4];
        assert!(at > 0.3 && at < 1.0);
        assert!(levels[5] < at);
        assert!(levels[33] > 0.0 && levels[34] == 0.0);
    }

    #[test]
    fn spans_without_frames_pass_their_events_to_the_next_span() {
        let mut params = Params::new(&NativeDevice::new(NativeKind::Synth));
        params.prepare(&crate::EngineConfig {
            sample_rate: 48_000,
            max_block: 64,
        });
        let events = [param(0, 0, 1.0), param(4, 1, 1.0), param(4, 2, 50.0)];
        let ctx = context(16, &events);
        let mut cursor = Cursor::default();
        let mut taken = Vec::new();
        params.run(&ctx, |span| {
            if let Some(events) = cursor.events(&ctx, &span) {
                let offsets: Vec<usize> = events.iter().map(|e| e.offset).collect();
                taken.push((span.frames.clone(), offsets));
            }
        });
        assert_eq!(taken, [(0..4, vec![0]), (4..16, vec![4, 4])]);
    }
}

//! The sampler: plays its sample at each note's pitch.
//!
//! A note plays the sample from `start` to `end`, sped up or slowed down
//! by the note's distance from `root`, plus `transpose` and `detune`, and
//! by the note's pitch bend. Looping, it plays on from `end` back to
//! `loop_start` for as long as the note is held, then to `end` and stops
//! once released. In "Classic" mode a note's amp envelope releases when the
//! note is let go; "One-Shot" plays the whole sample however short the
//! note, for drums. Each voice has an attack, decay, sustain and release
//! envelope; velocity sets the level by `velocity` percent of the way from
//! full; and an optional 12 dB state variable filter follows. A note past
//! `voices` takes the oldest voice. Start, end and loop points are in
//! percent of the sample's length.

use super::Scale::{Linear, Log};
use super::Unit::{Cents, Decibels, Hertz, Milliseconds, Note, Number, Percent, Semitones};
use super::{ParamInfo, choice, param, switch, whole};

/// The note that plays the sample at its own pitch.
pub const ROOT: usize = 0;
/// In whole semitones.
pub const TRANSPOSE: usize = 1;
/// In cents.
pub const DETUNE: usize = 2;
/// Where notes start playing, in percent of the sample.
pub const START: usize = 3;
/// Where they stop, or loop from, in percent of the sample.
pub const END: usize = 4;
/// Whether held notes loop.
pub const LOOP: usize = 5;
/// Where a loop goes back to, in percent of the sample.
pub const LOOP_START: usize = 6;
/// One of [`MODES`].
pub const MODE: usize = 7;
/// In ms.
pub const ATTACK: usize = 8;
/// In ms.
pub const DECAY: usize = 9;
/// In percent of the peak.
pub const SUSTAIN: usize = 10;
/// In ms.
pub const RELEASE: usize = 11;
/// How much velocity sets the level, in percent.
pub const VELOCITY: usize = 12;
/// In dB.
pub const VOLUME: usize = 13;
/// One of [`FILTERS`].
pub const FILTER: usize = 14;
/// In Hz.
pub const CUTOFF: usize = 15;
/// In percent.
pub const RESONANCE: usize = 16;
/// How many notes can sound at once.
pub const VOICES: usize = 17;

pub const MODES: [&str; 2] = ["Classic", "One-Shot"];
pub const FILTERS: [&str; 4] = ["Off", "Low Pass", "High Pass", "Band Pass"];

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 18] = [
    whole("root", "Root", "Sample", [0.0, 127.0, 60.0], Note),
    whole("transpose", "Transpose", "Sample", [-48.0, 48.0, 0.0], Semitones),
    param("detune", "Detune", "Sample", [-50.0, 50.0, 0.0], Cents, Linear),
    param("start", "Start", "Sample", [0.0, 100.0, 0.0], Percent, Linear),
    param("end", "End", "Sample", [0.0, 100.0, 100.0], Percent, Linear),
    switch("loop", "Loop", "Sample", false),
    param("loop_start", "Loop Start", "Sample", [0.0, 100.0, 0.0], Percent, Linear),
    choice("mode", "Mode", "Sample", &MODES, 0),
    param("attack", "Attack", "Amp", [0.1, 20_000.0, 1.0], Milliseconds, Log),
    param("decay", "Decay", "Amp", [1.0, 20_000.0, 600.0], Milliseconds, Log),
    param("sustain", "Sustain", "Amp", [0.0, 100.0, 100.0], Percent, Linear),
    param("release", "Release", "Amp", [1.0, 20_000.0, 50.0], Milliseconds, Log),
    param("velocity", "Velocity", "Amp", [0.0, 100.0, 50.0], Percent, Linear),
    param("volume", "Volume", "Amp", [-48.0, 12.0, 0.0], Decibels, Linear),
    choice("filter", "Type", "Filter", &FILTERS, 0),
    param("cutoff", "Cutoff", "Filter", [20.0, 20_000.0, 2000.0], Hertz, Log),
    param("resonance", "Resonance", "Filter", [0.0, 100.0, 0.0], Percent, Linear),
    whole("voices", "Voices", "", [1.0, 32.0, 8.0], Number),
];

//! The synth: two oscillators and noise, through a filter with its own
//! envelope, into an amp envelope.
//!
//! Each voice mixes oscillator 1 (an octave choice), oscillator 2 (in
//! semitones and cents from it) and white noise, each at its level. The
//! saw and the square are band-limited. The filter's cutoff moves by
//! `filter_env` semitones at the filter envelope's peak, and follows the
//! keyboard by `key_track` percent (100% moves it with the note's pitch,
//! from C4). Resonance runs from none to just short of self-oscillation.
//! Velocity sets the level by `velocity` percent of the way from full;
//! notes glide from the last note's pitch over `glide` ms; a note past
//! `voices` takes the oldest voice. Each note's pitch bend moves its
//! oscillators.

use super::Scale::{Fader, Linear, Log};
use super::Unit::{Cents, Decibels, Hertz, Milliseconds, Number, Percent, Semitones};
use super::{ParamInfo, choice, param, whole};

/// One of [`WAVES`].
pub const OSC1_WAVE: usize = 0;
/// In octaves from the note.
pub const OSC1_OCTAVE: usize = 1;
/// In percent.
pub const OSC1_LEVEL: usize = 2;
/// One of [`WAVES`].
pub const OSC2_WAVE: usize = 3;
/// In whole semitones from oscillator 1.
pub const OSC2_PITCH: usize = 4;
/// In cents.
pub const OSC2_DETUNE: usize = 5;
/// In percent.
pub const OSC2_LEVEL: usize = 6;
/// The white noise's level, in percent.
pub const NOISE: usize = 7;
/// One of [`FILTERS`].
pub const FILTER: usize = 8;
/// In Hz.
pub const CUTOFF: usize = 9;
/// In percent.
pub const RESONANCE: usize = 10;
/// How far the filter envelope moves the cutoff at its peak, in semitones.
pub const FILTER_ENV: usize = 11;
/// In percent.
pub const KEY_TRACK: usize = 12;
/// The filter envelope's, in ms.
pub const FILTER_ATTACK: usize = 13;
/// In ms.
pub const FILTER_DECAY: usize = 14;
/// In percent of the peak.
pub const FILTER_SUSTAIN: usize = 15;
/// In ms.
pub const FILTER_RELEASE: usize = 16;
/// The amp envelope's, in ms.
pub const AMP_ATTACK: usize = 17;
/// In ms.
pub const AMP_DECAY: usize = 18;
/// In percent of the peak.
pub const AMP_SUSTAIN: usize = 19;
/// In ms.
pub const AMP_RELEASE: usize = 20;
/// In dB.
pub const VOLUME: usize = 21;
/// How much velocity sets the level, in percent.
pub const VELOCITY: usize = 22;
/// How many notes can sound at once.
pub const VOICES: usize = 23;
/// In ms.
pub const GLIDE: usize = 24;

pub const WAVES: [&str; 4] = ["Sine", "Triangle", "Saw", "Square"];
pub const FILTERS: [&str; 4] = ["Low Pass 12", "Low Pass 24", "High Pass 12", "Band Pass 12"];

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 25] = [
    choice("osc1_wave", "Wave", "Osc 1", &WAVES, 2),
    whole("osc1_octave", "Octave", "Osc 1", [-3.0, 3.0, 0.0], Number),
    param("osc1_level", "Level", "Osc 1", [0.0, 100.0, 100.0], Percent, Linear),
    choice("osc2_wave", "Wave", "Osc 2", &WAVES, 2),
    whole("osc2_pitch", "Pitch", "Osc 2", [-24.0, 24.0, 0.0], Semitones),
    param("osc2_detune", "Detune", "Osc 2", [-100.0, 100.0, 0.0], Cents, Linear),
    param("osc2_level", "Level", "Osc 2", [0.0, 100.0, 0.0], Percent, Linear),
    param("noise", "Level", "Noise", [0.0, 100.0, 0.0], Percent, Linear),
    choice("filter", "Type", "Filter", &FILTERS, 1),
    param("cutoff", "Cutoff", "Filter", [20.0, 20_000.0, 2000.0], Hertz, Log),
    param("resonance", "Resonance", "Filter", [0.0, 100.0, 10.0], Percent, Linear),
    param("filter_env", "Envelope", "Filter", [-96.0, 96.0, 24.0], Semitones, Linear),
    param("key_track", "Key Track", "Filter", [0.0, 100.0, 50.0], Percent, Linear),
    param("filter_attack", "Attack", "Filter Env", [0.1, 20_000.0, 1.0], Milliseconds, Log),
    param("filter_decay", "Decay", "Filter Env", [1.0, 20_000.0, 400.0], Milliseconds, Log),
    param("filter_sustain", "Sustain", "Filter Env", [0.0, 100.0, 30.0], Percent, Linear),
    param("filter_release", "Release", "Filter Env", [1.0, 20_000.0, 200.0], Milliseconds, Log),
    param("amp_attack", "Attack", "Amp Env", [0.1, 20_000.0, 1.0], Milliseconds, Log),
    param("amp_decay", "Decay", "Amp Env", [1.0, 20_000.0, 600.0], Milliseconds, Log),
    param("amp_sustain", "Sustain", "Amp Env", [0.0, 100.0, 80.0], Percent, Linear),
    param("amp_release", "Release", "Amp Env", [1.0, 20_000.0, 150.0], Milliseconds, Log),
    param("volume", "Volume", "", [-48.0, 12.0, -6.0], Decibels, Linear),
    param("velocity", "Velocity", "", [0.0, 100.0, 50.0], Percent, Linear),
    whole("voices", "Voices", "", [1.0, 32.0, 8.0], Number),
    param("glide", "Glide", "", [0.0, 2000.0, 0.0], Milliseconds, Fader),
];

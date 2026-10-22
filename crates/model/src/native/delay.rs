//! The delay: echoes, in time with the song or in milliseconds.
//!
//! Synced, the echoes are a [`NOTES`] value apart at the song's tempo where
//! they're heard; otherwise `time` ms apart. Each echo comes back at
//! `feedback` percent of the last, through a low cut and a high cut, so
//! repeats thin out as they go. Ping pong sends each echo to the other
//! side: the first goes left, the next right, from both channels summed.
//! `mix` blends the echoes with the dry signal, which stays at full level
//! up to half way and then fades, as on most delays.

use super::Scale::{Linear, Log};
use super::Unit::{Hertz, Milliseconds, Percent};
use super::{ParamInfo, choice, param, switch};

/// Whether the time follows the tempo: [`NOTE`] rather than [`TIME`].
pub const SYNC: usize = 0;
/// The time between echoes when not synced, in ms.
pub const TIME: usize = 1;
/// The time between echoes when synced: one of [`NOTES`].
pub const NOTE: usize = 2;
/// How loud each echo is against the last, in percent.
pub const FEEDBACK: usize = 3;
pub const PING_PONG: usize = 4;
/// The echoes' low cut, in Hz.
pub const LOW_CUT: usize = 5;
/// The echoes' high cut, in Hz.
pub const HIGH_CUT: usize = 6;
/// How much of the output is echoes, in percent.
pub const MIX: usize = 7;

/// Note values for the synced time: "T" for triplets, "D" for dotted.
pub const NOTES: [&str; 14] = [
    "1/32", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D", "1/4T", "1/4", "1/4D", "1/2T", "1/2",
    "1/2D", "1/1",
];

/// How many beats each of [`NOTES`] lasts, a beat being a quarter note.
pub const NOTE_BEATS: [f64; 14] = [
    0.125,
    1.0 / 6.0,
    0.25,
    0.375,
    1.0 / 3.0,
    0.5,
    0.75,
    2.0 / 3.0,
    1.0,
    1.5,
    4.0 / 3.0,
    2.0,
    3.0,
    4.0,
];

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 8] = [
    switch("sync", "Sync", "Time", true),
    param("time", "Time", "Time", [1.0, 5000.0, 250.0], Milliseconds, Log),
    choice("note", "Note", "Time", &NOTES, 6),
    param("feedback", "Feedback", "", [0.0, 100.0, 40.0], Percent, Linear),
    switch("ping_pong", "Ping Pong", "", false),
    param("low_cut", "Low Cut", "Filter", [20.0, 20_000.0, 100.0], Hertz, Log),
    param("high_cut", "High Cut", "Filter", [20.0, 20_000.0, 8000.0], Hertz, Log),
    param("mix", "Mix", "", [0.0, 100.0, 30.0], Percent, Linear),
];

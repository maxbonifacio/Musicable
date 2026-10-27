//! The gate: turns the signal down to `floor` while it's quieter than the
//! threshold.
//!
//! It opens on the frame the louder channel's peak reaches `threshold`, and
//! closes once the peak has stayed more than `return` dB below it for
//! `hold`: the gap stops it chattering on a level that hovers round the
//! threshold. It fades open over `attack` and closed over `release`, in a
//! straight line in gain, and a fade that starts part of the way takes
//! that part of the time. Closed, it turns the signal down to `floor`, so
//! a floor of -80 dB is all but silent and a higher one lets some through.
//! A new gate starts open.
//!
//! The peak comes from the key: the device's own input, or with `sidechain`
//! on, its side input, as the compressor's does (see
//! [`compressor`](super::compressor)), through the same key filters, which
//! `listen` plays. So a gate keyed by a kick opens a pad on each beat, and
//! a key low-pass lets a drum's gate open on its thump rather than the
//! hi-hats in its microphone.

use super::Scale::{Fader, Linear, Log};
use super::Unit::{Decibels, Hertz, Milliseconds};
use super::{ParamInfo, param, switch};

/// In dB.
pub const THRESHOLD: usize = 0;
/// How far below the threshold the level falls before the gate closes, in
/// dB.
pub const RETURN: usize = 1;
/// In ms.
pub const ATTACK: usize = 2;
/// How long it stays open once the peak has fallen `return` dB below the
/// threshold, in ms.
pub const HOLD: usize = 3;
/// In ms.
pub const RELEASE: usize = 4;
/// The gain while closed, in dB.
pub const FLOOR: usize = 5;
/// Whether the peak comes from the side input, rather than the device's own
/// input: 0 off, 1 on.
pub const SIDECHAIN: usize = 6;
/// Where the key's high-pass cuts, in Hz. 20 Hz cuts nothing.
pub const KEY_HIGH_PASS: usize = 7;
/// Where the key's low-pass cuts, in Hz. 20 kHz cuts nothing.
pub const KEY_LOW_PASS: usize = 8;
/// Whether the output is the key, as the gate hears it: 0 off, 1 on.
pub const LISTEN: usize = 9;

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 10] = [
    param("threshold", "Threshold", "", [-80.0, 0.0, -40.0], Decibels, Linear),
    param("return", "Return", "", [0.0, 24.0, 3.0], Decibels, Linear),
    param("attack", "Attack", "", [0.02, 100.0, 0.5], Milliseconds, Log),
    param("hold", "Hold", "", [0.0, 1000.0, 10.0], Milliseconds, Fader),
    param("release", "Release", "", [1.0, 3000.0, 100.0], Milliseconds, Log),
    param("floor", "Floor", "", [-80.0, 0.0, -80.0], Decibels, Linear),
    switch("sidechain", "On", "Sidechain", false),
    param("key_high_pass", "High-Pass", "Sidechain", [20.0, 20_000.0, 20.0], Hertz, Log),
    param("key_low_pass", "Low-Pass", "Sidechain", [20.0, 20_000.0, 20_000.0], Hertz, Log),
    switch("listen", "Listen", "Sidechain", false),
];

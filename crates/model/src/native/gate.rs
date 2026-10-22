//! The gate: turns the signal down to `floor` while it's quieter than the
//! threshold.
//!
//! It opens when the louder channel's peak rises above `threshold`, and
//! closes once it falls `return` dB below it and `hold` has passed since it
//! was last above: the gap stops it chattering on a level that hovers
//! round the threshold. It fades open over `attack` and closed over
//! `release`. Closed, it turns the signal down to `floor`, so a floor of
//! -80 dB is all but silent and a higher one lets some through.

use super::Scale::{Fader, Linear, Log};
use super::Unit::{Decibels, Milliseconds};
use super::{ParamInfo, param};

/// In dB.
pub const THRESHOLD: usize = 0;
/// How far below the threshold the level falls before the gate closes, in
/// dB.
pub const RETURN: usize = 1;
/// In ms.
pub const ATTACK: usize = 2;
/// How long it stays open after the level was last above the threshold,
/// in ms.
pub const HOLD: usize = 3;
/// In ms.
pub const RELEASE: usize = 4;
/// The gain while closed, in dB.
pub const FLOOR: usize = 5;

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 6] = [
    param("threshold", "Threshold", "", [-80.0, 0.0, -40.0], Decibels, Linear),
    param("return", "Return", "", [0.0, 24.0, 3.0], Decibels, Linear),
    param("attack", "Attack", "", [0.02, 100.0, 0.5], Milliseconds, Log),
    param("hold", "Hold", "", [0.0, 1000.0, 10.0], Milliseconds, Fader),
    param("release", "Release", "", [1.0, 3000.0, 100.0], Milliseconds, Log),
    param("floor", "Floor", "", [-80.0, 0.0, -80.0], Decibels, Linear),
];

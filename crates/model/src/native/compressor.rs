//! The compressor: turns the signal down by `ratio` above `threshold`.
//!
//! It follows the louder channel's level, so both channels move together,
//! and works in decibels with a soft knee: `knee` dB wide, centred on the
//! threshold, where the ratio comes in gradually. Its level detector rises
//! with `attack` and falls with `release`, each the time it takes to go
//! most of the way (one time constant). Peak detection follows the signal's
//! peaks, RMS its power over about the attack time. The output gain comes
//! after, and `mix` blends the compressed signal with what came in, for
//! parallel compression.

use super::Scale::{Linear, Log};
use super::Unit::{Decibels, Milliseconds, Percent, Ratio};
use super::{ParamInfo, choice, param};

/// In dB.
pub const THRESHOLD: usize = 0;
/// From 1:1, which does nothing, to 20:1.
pub const RATIO: usize = 1;
/// In ms.
pub const ATTACK: usize = 2;
/// In ms.
pub const RELEASE: usize = 3;
/// How wide the soft knee is, in dB. 0 is a hard knee.
pub const KNEE: usize = 4;
/// The gain after compressing, in dB.
pub const OUTPUT: usize = 5;
/// How much of the output is compressed, in percent.
pub const MIX: usize = 6;
/// What the level follows: one of [`DETECTORS`].
pub const DETECTOR: usize = 7;

/// The detectors: peak, or RMS.
pub const DETECTORS: [&str; 2] = ["Peak", "RMS"];

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 8] = [
    param("threshold", "Threshold", "", [-60.0, 0.0, -18.0], Decibels, Linear),
    param("ratio", "Ratio", "", [1.0, 20.0, 4.0], Ratio, Log),
    param("attack", "Attack", "", [0.1, 200.0, 10.0], Milliseconds, Log),
    param("release", "Release", "", [1.0, 3000.0, 100.0], Milliseconds, Log),
    param("knee", "Knee", "", [0.0, 24.0, 6.0], Decibels, Linear),
    param("output", "Output", "", [-24.0, 24.0, 0.0], Decibels, Linear),
    param("mix", "Mix", "", [0.0, 100.0, 100.0], Percent, Linear),
    choice("detector", "Detector", "", &DETECTORS, 0),
];

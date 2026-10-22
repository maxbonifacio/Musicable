//! The utility: gain, balance, stereo width, polarity, mute and a DC
//! filter.
//!
//! In order: the DC filter (a high pass well below hearing, at 5 Hz), the
//! polarity switches, the width, the balance and the gain. Width works in
//! mid and side: 0% is mono, 100% passes the signal as it is and 200%
//! doubles the side. The balance is the mixer's ([`pan_gains`]). Mute fades
//! the output out, and switches fade too, so nothing clicks.
//!
//! [`pan_gains`]: crate::mixer::pan_gains

use super::Scale::{Fader, Linear};
use super::Unit::{Gain, Pan, Percent};
use super::{ParamInfo, param, switch};

/// Linear gain, 1.0 = unity.
pub const GAIN: usize = 0;
/// From -1.0 (left) to 1.0 (right).
pub const PAN: usize = 1;
/// In percent.
pub const WIDTH: usize = 2;
pub const INVERT_LEFT: usize = 3;
pub const INVERT_RIGHT: usize = 4;
pub const MUTE: usize = 5;
/// Whether the DC filter is on.
pub const DC: usize = 6;

/// The most gain: +24 dB.
pub const MAX_GAIN: f64 = 15.848_931_924_611_133;

/// How far the DC filter's high pass is, in Hz.
pub const DC_CUTOFF: f64 = 5.0;

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 7] = [
    param("gain", "Gain", "", [0.0, MAX_GAIN, 1.0], Gain, Fader),
    param("pan", "Pan", "", [-1.0, 1.0, 0.0], Pan, Linear),
    param("width", "Width", "", [0.0, 200.0, 100.0], Percent, Linear),
    switch("invert_left", "Invert L", "", false),
    switch("invert_right", "Invert R", "", false),
    switch("mute", "Mute", "", false),
    switch("dc", "DC Filter", "", false),
];

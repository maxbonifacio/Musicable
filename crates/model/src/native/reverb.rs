//! The reverb: a room, a hall or a cave behind the signal.
//!
//! The dry signal goes through `low_cut` into a pre-delay, then a network
//! of delay lines whose lengths grow with `size`, feeding back so the
//! tail falls by 60 dB over `decay` seconds, and faster above `damping`.
//! `width` spreads the tail from mono to the full stereo of the network,
//! and `mix` blends it with the dry signal as the delay's does.

use super::Scale::{Fader, Linear, Log};
use super::Unit::{Hertz, Milliseconds, Percent, Seconds};
use super::{ParamInfo, param};

/// In ms.
pub const PREDELAY: usize = 0;
/// How big the space is, in percent. Not modulated: it sets the delay
/// lines' lengths.
pub const SIZE: usize = 1;
/// How long the tail takes to fall by 60 dB, in seconds.
pub const DECAY: usize = 2;
/// Above this the tail dies away faster, in Hz.
pub const DAMPING: usize = 3;
/// The low cut before the reverb, in Hz.
pub const LOW_CUT: usize = 4;
/// In percent.
pub const WIDTH: usize = 5;
/// How much of the output is reverb, in percent.
pub const MIX: usize = 6;

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 7] = [
    param("predelay", "Pre-Delay", "", [0.0, 250.0, 10.0], Milliseconds, Fader),
    param("size", "Size", "", [0.0, 100.0, 50.0], Percent, Linear).fixed(),
    param("decay", "Decay", "", [0.1, 30.0, 2.0], Seconds, Log),
    param("damping", "Damping", "", [1000.0, 20_000.0, 6000.0], Hertz, Log),
    param("low_cut", "Low Cut", "", [20.0, 1000.0, 80.0], Hertz, Log),
    param("width", "Width", "", [0.0, 100.0, 100.0], Percent, Linear),
    param("mix", "Mix", "", [0.0, 100.0, 25.0], Percent, Linear),
];

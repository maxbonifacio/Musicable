//! The reverb: a room, a hall or a cave behind the signal.
//!
//! The dry signal goes through `low_cut` (the EQ's Low Cut 12) into a
//! pre-delay, then a network of delay lines whose lengths grow with `size`
//! ([`room_seconds`]), feeding back so the tail falls by 60 dB over `decay`
//! seconds, whatever the size, and faster above `damping`: at the damping
//! frequency it falls twice as fast, and faster still above it. A new size
//! glides there, bending the tail's pitch by about a semitone at most, so
//! it never clicks. `width` spreads the tail from mono to the full stereo
//! of the network, and `mix` blends it with the dry signal as the delay's
//! does ([`mix_levels`](super::delay::mix_levels)).

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

/// The shortest of the network's delay lines at `size` percent, in
/// seconds: 5 ms at 0%, a small room, growing evenly in ratio to 100 ms at
/// 100%, a cave. The longest is nearly twice as long, and the diffusion in
/// front of them spreads the signal over about as long again.
pub fn room_seconds(size: f64) -> f64 {
    0.005 * 20_f64.powf(size.clamp(0.0, 100.0) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_grow_with_size() {
        assert!((room_seconds(0.0) - 0.005).abs() < 1e-12);
        assert!((room_seconds(100.0) - 0.1).abs() < 1e-12);
        let half = room_seconds(50.0);
        assert!((half * half - 0.005 * 0.1).abs() < 1e-12);
        assert_eq!(room_seconds(-5.0), room_seconds(0.0));
    }
}

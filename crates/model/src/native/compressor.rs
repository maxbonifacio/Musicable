//! The compressor: turns the signal down by `ratio` above `threshold`.
//!
//! It follows the louder channel's level, so both channels move together,
//! and works in decibels with a soft knee: `knee` dB wide, centred on the
//! threshold, where the ratio comes in gradually ([`gain_reduction`] is the
//! curve). How far it turns the signal down rises with `attack` and falls
//! with `release`, each the time it takes to go most of the way (one time
//! constant), however far that is. Peak detection follows the signal's
//! peaks, RMS its power averaged over the attack time, so RMS responds a
//! little more slowly. The output gain comes after, and `mix` blends the
//! compressed signal with what came in, for parallel compression.
//!
//! The level comes from the key: the device's own input, or with
//! `sidechain` on, its side input, which wires feed from other points in
//! the mix (see [`routing`](crate::routing)). With nothing wired to the side
//! input, the key is the device's own input whatever `sidechain` says, so it
//! never keys from silence. The key goes through a 12 dB per octave
//! high-pass at `key_high_pass` and a low-pass at `key_low_pass`, so a bass
//! line doesn't pump it, and `listen` plays the key, filtered, in place of
//! the output, for setting them. At their ends, 20 Hz and 20 kHz, the
//! filters cut nothing.

use super::Scale::{Linear, Log};
use super::Unit::{Decibels, Hertz, Milliseconds, Percent, Ratio};
use super::{ParamInfo, choice, param, switch};

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
/// Whether the level comes from the side input, rather than the device's
/// own input: 0 off, 1 on.
pub const SIDECHAIN: usize = 8;
/// Where the key's high-pass cuts, in Hz. 20 Hz cuts nothing.
pub const KEY_HIGH_PASS: usize = 9;
/// Where the key's low-pass cuts, in Hz. 20 kHz cuts nothing.
pub const KEY_LOW_PASS: usize = 10;
/// Whether the output is the key, as the level follows it: 0 off, 1 on.
pub const LISTEN: usize = 11;

/// The detectors: peak, or RMS.
pub const DETECTORS: [&str; 2] = ["Peak", "RMS"];

#[rustfmt::skip]
pub(super) const PARAMS: [ParamInfo; 12] = [
    param("threshold", "Threshold", "", [-60.0, 0.0, -18.0], Decibels, Linear),
    param("ratio", "Ratio", "", [1.0, 20.0, 4.0], Ratio, Log),
    param("attack", "Attack", "", [0.1, 200.0, 10.0], Milliseconds, Log),
    param("release", "Release", "", [1.0, 3000.0, 100.0], Milliseconds, Log),
    param("knee", "Knee", "", [0.0, 24.0, 6.0], Decibels, Linear),
    param("output", "Output", "", [-24.0, 24.0, 0.0], Decibels, Linear),
    param("mix", "Mix", "", [0.0, 100.0, 100.0], Percent, Linear),
    choice("detector", "Detector", "", &DETECTORS, 0),
    switch("sidechain", "On", "Sidechain", false),
    param("key_high_pass", "High-Pass", "Sidechain", [20.0, 20_000.0, 20.0], Hertz, Log),
    param("key_low_pass", "Low-Pass", "Sidechain", [20.0, 20_000.0, 20_000.0], Hertz, Log),
    switch("listen", "Listen", "Sidechain", false),
];

/// How many dB the compressor turns a steady signal `level` dB loud down
/// by, with `threshold`, `ratio` and `knee` as set: its static curve,
/// which the engine's compressor settles on.
///
/// Nothing below the knee. Above it, each dB over the threshold comes out
/// as `1 / ratio` dB. Across the knee, `knee` dB wide and centred on the
/// threshold, the reduction grows along a parabola that meets both lines
/// at their slopes, as in Giannoulis, Massberg and Reiss, "Digital Dynamic
/// Range Compressor Design: A Tutorial and Analysis" (JAES, 2012).
#[inline]
pub fn gain_reduction(level: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    let over = level - threshold;
    let slope = 1.0 - 1.0 / ratio;
    if 2.0 * over <= -knee {
        0.0
    } else if 2.0 * over < knee {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        slope * over
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn above_the_threshold_the_ratio_holds() {
        // 4:1 from -20 dB, with a hard knee: 8 dB over comes out as 2.
        assert_eq!(gain_reduction(-12.0, -20.0, 4.0, 0.0), 6.0);
        assert_eq!(gain_reduction(-20.0, -20.0, 4.0, 0.0), 0.0);
        assert_eq!(gain_reduction(-40.0, -20.0, 4.0, 0.0), 0.0);
        // 20:1 all but stops the level at the threshold.
        assert!((gain_reduction(0.0, -20.0, 20.0, 0.0) - 19.0).abs() < 1e-12);
        // 1:1 does nothing, and silence is left alone.
        assert_eq!(gain_reduction(0.0, -60.0, 1.0, 0.0), 0.0);
        assert_eq!(gain_reduction(f64::NEG_INFINITY, -20.0, 4.0, 6.0), 0.0);
    }

    #[test]
    fn the_knee_eases_the_ratio_in() {
        let curve = |level| gain_reduction(level, -20.0, 4.0, 8.0);
        // Nothing until 4 dB under the threshold, the full ratio from 4 dB
        // over it, and an eighth of the knee's width at the ratio's slope
        // on the threshold itself.
        assert_eq!(curve(-24.0), 0.0);
        assert_eq!(curve(-16.0), 3.0);
        assert_eq!(curve(-20.0), 0.75);
        assert_eq!(curve(-6.0), 10.5);
        // No corners: the slope runs from 0 to 3/4 without a jump.
        let slope = |level: f64| (curve(level + 1e-6) - curve(level - 1e-6)) / 2e-6;
        assert!(slope(-24.0).abs() < 1e-5);
        assert!((slope(-16.0) - 0.75).abs() < 1e-5);
        let mut last = 0.0;
        for step in 0..=100 {
            let level = -30.0 + f64::from(step) * 0.2;
            let s = slope(level);
            assert!(s >= last - 1e-6 && s <= 0.75 + 1e-6, "{level}: {s}");
            last = s;
        }
    }
}

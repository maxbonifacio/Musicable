//! The delay: echoes, in time with the song or in milliseconds.
//!
//! Synced, the echoes are a [`NOTES`] value apart at the song's tempo where
//! they're heard; otherwise `time` ms apart. Times come to whole frames,
//! and a synced time longer than the longest `time`, 5 s, is held there: a
//! whole note below 48 bpm. A new time fades in over [`FADE_SECONDS`] as
//! the echoes at the old one fade out, whether it's set, synced to a new
//! note or tempo, or switched between the two, so the echoes never chirp
//! or click. Modulating `time` moves the echoes as it goes, bending their
//! pitch as a tape delay's would, synced too: by that many ms.
//!
//! Each echo comes back at `feedback` percent of the last, through a low
//! cut and a high cut ([`low_cut`], [`high_cut`]), so repeats thin out as
//! they go: the first echo has been through them once, the next twice. Ping
//! pong sends each echo to the other side: the first goes left, the next
//! right, from both channels mixed to mono, half of each. Echoes going
//! round again saturate softly above full scale, up to twice it, so a
//! feedback near 100% can't run away; below full scale it's exact.
//!
//! `mix` blends the echoes with the dry signal, which stays at full level
//! up to half way and then fades, as on most delays ([`mix_levels`]).

use std::f64::consts::FRAC_1_SQRT_2;

use super::Scale::{Linear, Log};
use super::Unit::{Hertz, Milliseconds, Percent};
use super::eq::{Band, Shape};
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

/// How long a new time takes to fade in: 50 ms.
pub const FADE_SECONDS: f64 = 0.05;

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

/// The echoes' low cut at `freq` Hz: the EQ's Low Cut 12, a second-order
/// Butterworth high pass.
pub fn low_cut(freq: f64) -> Band {
    cut(Shape::LowCut12, freq)
}

/// The echoes' high cut at `freq` Hz: the EQ's High Cut 12.
pub fn high_cut(freq: f64) -> Band {
    cut(Shape::HighCut12, freq)
}

fn cut(shape: Shape, freq: f64) -> Band {
    Band {
        on: true,
        shape,
        freq,
        gain: 0.0,
        q: FRAC_1_SQRT_2,
    }
}

/// How much of the last echo the next keeps at `freq` Hz, as a linear
/// magnitude, with the feedback at `feedback` percent and the cuts at
/// `low` and `high` Hz, running at `sample_rate`: what the design calls
/// for, below full scale.
pub fn repeat_response(feedback: f64, low: f64, high: f64, freq: f64, sample_rate: f64) -> f64 {
    let filters =
        low_cut(low).response(freq, sample_rate) * high_cut(high).response(freq, sample_rate);
    feedback / 100.0 * filters
}

/// The dry signal's gain and the effect's at `mix` percent: the dry at
/// full level up to 50% and then fading to nothing at 100%, the effect
/// rising from nothing at 0% to full level at 50%. The reverb mixes the
/// same way.
pub fn mix_levels(mix: f32) -> [f32; 2] {
    let mix = if mix.is_nan() {
        0.0
    } else {
        mix.clamp(0.0, 100.0)
    };
    [((100.0 - mix) / 50.0).min(1.0), (mix / 50.0).min(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_last_their_beats() {
        assert_eq!(NOTES.len(), NOTE_BEATS.len());
        assert_eq!(
            NOTE_BEATS[NOTES.iter().position(|&n| n == "1/4").unwrap()],
            1.0
        );
        // A triplet is two thirds of the plain note, a dotted one and a half.
        for (i, name) in NOTES.iter().enumerate() {
            let plain = |base: &str| NOTE_BEATS[NOTES.iter().position(|&n| n == base).unwrap()];
            if let Some(base) = name.strip_suffix('T') {
                assert!((NOTE_BEATS[i] - plain(base) * 2.0 / 3.0).abs() < 1e-12);
            } else if let Some(base) = name.strip_suffix('D') {
                assert!((NOTE_BEATS[i] - plain(base) * 1.5).abs() < 1e-12);
            }
        }
        // The plain notes double, from a 32nd to a whole note.
        let plain: Vec<f64> = NOTES
            .iter()
            .zip(NOTE_BEATS)
            .filter(|(name, _)| !name.ends_with(['T', 'D']))
            .map(|(_, beats)| beats)
            .collect();
        assert_eq!(plain, [0.125, 0.25, 0.5, 1.0, 2.0, 4.0]);
    }

    #[test]
    fn the_dry_signal_stays_full_up_to_half_way() {
        assert_eq!(mix_levels(0.0), [1.0, 0.0]);
        assert_eq!(mix_levels(30.0), [1.0, 0.6]);
        assert_eq!(mix_levels(50.0), [1.0, 1.0]);
        assert_eq!(mix_levels(75.0), [0.5, 1.0]);
        assert_eq!(mix_levels(100.0), [0.0, 1.0]);
        assert_eq!(mix_levels(f32::NAN), [1.0, 0.0]);
    }

    #[test]
    fn repeats_lose_their_lows_and_highs() {
        let db = |gain: f64| 20.0 * gain.log10();
        let repeat = |freq| repeat_response(50.0, 100.0, 8000.0, freq, 48_000.0);
        // Half as loud in between, and 3 dB further down at each cut.
        assert!((db(repeat(900.0)) - db(0.5)).abs() < 0.01);
        assert!((db(repeat(100.0)) - db(0.5) + 3.01).abs() < 0.01);
        assert!((db(repeat(8000.0)) - db(0.5) + 3.01).abs() < 0.01);
        assert!(repeat(20.0) < 0.5 * 0.05);
    }
}

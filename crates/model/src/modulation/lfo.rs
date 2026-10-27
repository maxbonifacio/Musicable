//! The LFO: a shape that repeats.
//!
//! Four shapes ([`SHAPES`]): a sine, a triangle, a saw that rises and a
//! square, each starting a cycle at its middle on the way up, as a sine
//! does. Skew bends each cycle so its first half takes `skew` percent of
//! the cycle and its second half the rest: a triangle that rises for longer
//! than it falls, a saw that bends, and on the square, its pulse width. At
//! 50% each shape is even.
//!
//! It runs at `rate` cycles a second, or with Sync on, a cycle each
//! [`NOTES`] value at the song's tempo. Phase moves where in the cycle it
//! starts, in degrees.
//!
//! Its Start Mode says what moves it on ([`STARTS`]):
//!
//! - **Free** runs on its own, from when the LFO starts playing.
//! - **Song** locks it to the song position: its cycles count from the
//!   start of the song, so a synced LFO lines up with the beats and bars,
//!   and it holds still while the transport stands still.
//! - **Note** starts it again on each note, as Note Trigger says
//!   ([`TRIGGERS`](super::TRIGGERS)), and each time Trigger rises past half
//!   way. After each start it waits Delay ms, then fades in over Fade In
//!   ms.
//!
//! The output goes from -1 to 1, or from 0 to 1 with Unipolar on.

use super::TRIGGERS;
use crate::native::Scale::{Fader, Linear, Log};
use crate::native::Unit::{Degrees, Fraction, Hertz, Milliseconds, Percent};
use crate::native::{ParamInfo, choice, param, switch};

/// Which of [`SHAPES`] it is.
pub const SHAPE: usize = 0;
/// In percent: how much of each cycle its first half takes.
pub const SKEW: usize = 1;
/// Whether the rate follows the tempo: [`NOTE`] rather than [`RATE`].
pub const SYNC: usize = 2;
/// Cycles a second, when not synced.
pub const RATE: usize = 3;
/// How long a cycle lasts when synced: one of [`NOTES`].
pub const NOTE: usize = 4;
/// Where in its cycle it starts, in degrees.
pub const PHASE: usize = 5;
/// What moves it on: one of [`STARTS`].
pub const START: usize = 6;
/// How notes start it again, with the Note start: one of
/// [`TRIGGERS`](super::TRIGGERS).
pub const NOTE_TRIGGER: usize = 7;
/// From 0 to 1: rising past half way starts it again, with the Note
/// start.
pub const TRIGGER: usize = 8;
/// How long it waits after each start before fading in, in ms.
pub const DELAY: usize = 9;
/// How long it takes to fade in after the delay, in ms.
pub const FADE_IN: usize = 10;
/// Whether it goes from 0 to 1 rather than from -1 to 1.
pub const UNIPOLAR: usize = 11;

/// The shapes, in order.
pub const SHAPES: [&str; 4] = ["Sine", "Triangle", "Saw", "Square"];

/// What can move it on, in order.
pub const STARTS: [&str; 3] = ["Free", "Song", "Note"];

/// Note values for the synced rate: "T" for triplets, "D" for dotted, and
/// "2/1" for two whole notes.
pub const NOTES: [&str; 18] = [
    "1/32", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D", "1/4T", "1/4", "1/4D", "1/2T", "1/2",
    "1/2D", "1/1", "2/1", "4/1", "8/1", "16/1",
];

/// How many beats each of [`NOTES`] lasts, a beat being a quarter note.
pub const NOTE_BEATS: [f64; 18] = [
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
    8.0,
    16.0,
    32.0,
    64.0,
];

#[rustfmt::skip]
pub(crate) const PARAMS: [ParamInfo; 12] = [
    choice("shape", "Shape", "", &SHAPES, 0),
    param("skew", "Skew", "", [0.0, 100.0, 50.0], Percent, Linear),
    switch("sync", "Sync", "Rate", false),
    param("rate", "Rate", "Rate", [0.01, 100.0, 1.0], Hertz, Log),
    choice("note", "Note", "Rate", &NOTES, 8),
    param("phase", "Phase", "", [0.0, 360.0, 0.0], Degrees, Linear),
    choice("start", "Mode", "Start", &STARTS, 0),
    choice("note_trigger", "Note Trigger", "Start", &TRIGGERS, 0),
    param("trigger", "Trigger", "Start", [0.0, 1.0, 0.0], Fraction, Linear),
    param("delay", "Delay", "Start", [0.0, 10_000.0, 0.0], Milliseconds, Fader),
    param("fade_in", "Fade In", "Start", [0.0, 10_000.0, 0.0], Milliseconds, Fader),
    switch("unipolar", "Unipolar", "", false),
];

/// One of [`SHAPES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    Sine,
    Triangle,
    Saw,
    Square,
}

impl Shape {
    pub const ALL: [Shape; 4] = [Self::Sine, Self::Triangle, Self::Saw, Self::Square];

    /// The shape the [`SHAPE`] parameter picks, rounded and kept in range.
    pub fn from_value(value: f64) -> Shape {
        Self::ALL[super::step(value, Self::ALL.len())]
    }

    /// The shape's value `phase` of the way through a cycle, from 0.0 to
    /// 1.0, bent by `skew`, from 0.0 to 1.0: from -1.0 to 1.0, at 0.0 where
    /// the cycle starts.
    pub fn value(self, phase: f64, skew: f64) -> f64 {
        let phase = bend(phase.rem_euclid(1.0), skew);
        match self {
            Self::Sine => (std::f64::consts::TAU * phase).sin(),
            // Up from the middle to the top at a quarter, down to the
            // bottom at three quarters, and back up to the middle.
            Self::Triangle => {
                if phase < 0.25 {
                    4.0 * phase
                } else if phase < 0.75 {
                    2.0 - 4.0 * phase
                } else {
                    4.0 * phase - 4.0
                }
            }
            // Up from the middle to the top at half way, where it drops to
            // the bottom.
            Self::Saw => {
                if phase < 0.5 {
                    2.0 * phase
                } else {
                    2.0 * phase - 2.0
                }
            }
            Self::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }
}

/// Where `phase` of the way through a cycle falls once it's bent so its
/// first half takes `skew` of the cycle.
fn bend(phase: f64, skew: f64) -> f64 {
    let skew = skew.clamp(0.0, 1.0);
    if phase < skew {
        0.5 * phase / skew
    } else if skew < 1.0 {
        0.5 + 0.5 * (phase - skew) / (1.0 - skew)
    } else {
        1.0
    }
}

/// One of [`STARTS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Start {
    Free,
    Song,
    Note,
}

impl Start {
    pub const ALL: [Start; 3] = [Self::Free, Self::Song, Self::Note];

    /// The start the [`START`] parameter picks, rounded and kept in range.
    pub fn from_value(value: f64) -> Start {
        Self::ALL[super::step(value, Self::ALL.len())]
    }
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
        assert_eq!(
            NOTE_BEATS[NOTES.iter().position(|&n| n == "4/1").unwrap()],
            16.0
        );
        assert_eq!(PARAMS[NOTE].default as usize, 8);
    }

    #[test]
    fn shapes_start_in_the_middle_on_the_way_up() {
        for shape in Shape::ALL {
            let start = shape.value(0.0, 0.5);
            let just_after = shape.value(0.01, 0.5);
            match shape {
                Shape::Square => assert_eq!(start, 1.0),
                _ => {
                    assert!(start.abs() < 1e-12, "{shape:?}");
                    assert!(just_after > 0.0, "{shape:?}");
                }
            }
            for k in 0..=100 {
                let value = shape.value(f64::from(k) / 100.0, 0.5);
                assert!((-1.0..=1.0).contains(&value), "{shape:?} {k}");
            }
        }
        assert!((Shape::Sine.value(0.25, 0.5) - 1.0).abs() < 1e-12);
        assert_eq!(Shape::Triangle.value(0.75, 0.5), -1.0);
        assert_eq!(Shape::Saw.value(0.25, 0.5), 0.5);
        assert_eq!(Shape::Saw.value(0.75, 0.5), -0.5);
    }

    #[test]
    fn skew_is_the_square_s_pulse_width() {
        let high = |skew: f64| {
            let highs =
                (0..1000).filter(|&k| Shape::Square.value(f64::from(k) / 1000.0, skew) > 0.0);
            highs.count()
        };
        assert_eq!(high(0.5), 500);
        assert_eq!(high(0.25), 250);
        assert_eq!(high(0.9), 900);
        // A skewed triangle peaks half way through its first half.
        assert!((Shape::Triangle.value(0.4, 0.8) - 1.0).abs() < 1e-12);
        assert!((Shape::Triangle.value(0.9, 0.8) + 1.0).abs() < 1e-12);
    }

    #[test]
    fn choices_read_from_their_values() {
        assert_eq!(Shape::from_value(2.0), Shape::Saw);
        assert_eq!(Shape::from_value(9.0), Shape::Square);
        assert_eq!(Shape::from_value(f64::NAN), Shape::Sine);
        assert_eq!(Start::from_value(1.4), Start::Song);
    }
}

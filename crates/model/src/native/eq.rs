//! The EQ: eight bands, then an output gain.
//!
//! Each band that's on is a cut, a shelf, a bell or a notch at its
//! frequency. A band is one or more second-order sections, each the analog
//! prototype from Robert Bristow-Johnson's Audio EQ Cookbook brought to the
//! sample rate by the bilinear transform, with the band's frequency
//! prewarped so it lands where it should. [`response`] works out the gain
//! that design gives at any frequency, which the engine's EQ measures up
//! to.
//!
//! A bell's or a shelf's gain is in decibels, and its Q sets how wide it is
//! (or, for a shelf, how steep). The cuts are Butterworth at Q 0.71: 12,
//! 24 or 48 dB per octave from one, two or four sections. A higher Q
//! raises a resonant peak at the cut's frequency, from the steepest
//! section.

use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

use super::{ParamInfo, Scale, Unit, choice, decibels_to_gain, param, switch};

/// How many bands it has.
pub const BANDS: usize = 8;
/// How many parameters each band has. Band `b`, from 0, has parameters
/// `b * PER_BAND + ON` and so on.
pub const PER_BAND: usize = 5;
/// Whether the band is on, or passes everything unchanged.
pub const ON: usize = 0;
/// The band's [`Shape`].
pub const SHAPE: usize = 1;
/// In Hz.
pub const FREQ: usize = 2;
/// A bell's or a shelf's gain, in dB.
pub const GAIN: usize = 3;
pub const Q: usize = 4;
/// The gain after the bands, in dB.
pub const OUTPUT: usize = BANDS * PER_BAND;

/// The names of the [`Shape`]s, in order.
pub const SHAPES: [&str; 10] = [
    "Low Cut 12",
    "Low Cut 24",
    "Low Cut 48",
    "Low Shelf",
    "Bell",
    "Notch",
    "High Shelf",
    "High Cut 12",
    "High Cut 24",
    "High Cut 48",
];

/// The highest a band's frequency goes, as a share of the sample rate:
/// just under half, where the bilinear transform has nowhere left to put
/// it.
pub const HIGHEST: f64 = 0.49;

macro_rules! bands {
    ($(($band:literal, $on:literal, $shape:expr, $freq:literal)),* $(,)?) => {
        [
            $(
                switch(concat!("band", $band, "_on"), "On", concat!("Band ", $band), $on),
                choice(
                    concat!("band", $band, "_shape"),
                    "Shape",
                    concat!("Band ", $band),
                    &SHAPES,
                    $shape as usize,
                ),
                param(
                    concat!("band", $band, "_freq"),
                    "Freq",
                    concat!("Band ", $band),
                    [10.0, 22_000.0, $freq],
                    Unit::Hertz,
                    Scale::Log,
                ),
                param(
                    concat!("band", $band, "_gain"),
                    "Gain",
                    concat!("Band ", $band),
                    [-24.0, 24.0, 0.0],
                    Unit::Decibels,
                    Scale::Linear,
                ),
                param(
                    concat!("band", $band, "_q"),
                    "Q",
                    concat!("Band ", $band),
                    [0.1, 18.0, FRAC_1_SQRT_2],
                    Unit::Number,
                    Scale::Log,
                ),
            )*
            param("output", "Output", "", [-24.0, 24.0, 0.0], Unit::Decibels, Scale::Linear),
        ]
    };
}

/// The cuts start off, and the bands between them on but flat.
pub(super) const PARAMS: [ParamInfo; OUTPUT + 1] = bands![
    (1, false, Shape::LowCut24, 30.0),
    (2, true, Shape::LowShelf, 100.0),
    (3, true, Shape::Bell, 250.0),
    (4, true, Shape::Bell, 800.0),
    (5, true, Shape::Bell, 2500.0),
    (6, true, Shape::Bell, 6000.0),
    (7, true, Shape::HighShelf, 10_000.0),
    (8, false, Shape::HighCut24, 18_000.0),
];

/// What a band does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    LowCut12,
    LowCut24,
    LowCut48,
    LowShelf,
    Bell,
    Notch,
    HighShelf,
    HighCut12,
    HighCut24,
    HighCut48,
}

impl Shape {
    pub const ALL: [Shape; 10] = [
        Self::LowCut12,
        Self::LowCut24,
        Self::LowCut48,
        Self::LowShelf,
        Self::Bell,
        Self::Notch,
        Self::HighShelf,
        Self::HighCut12,
        Self::HighCut24,
        Self::HighCut48,
    ];

    /// The shape a band's [`SHAPE`] parameter picks, rounded and kept in
    /// range.
    pub fn from_value(value: f64) -> Shape {
        let index = if value.is_nan() { 0.0 } else { value.round() };
        Self::ALL[index.clamp(0.0, 9.0) as usize]
    }

    /// Whether it changes the level by its gain: a shelf or a bell.
    pub fn has_gain(self) -> bool {
        matches!(self, Self::LowShelf | Self::Bell | Self::HighShelf)
    }
}

/// One band of an EQ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub on: bool,
    pub shape: Shape,
    /// In Hz.
    pub freq: f64,
    /// In dB.
    pub gain: f64,
    pub q: f64,
}

impl Band {
    /// Band `band`, from 0, of an EQ set as `values`.
    pub fn of(values: &[f64], band: usize) -> Band {
        let value = |param| values[band * PER_BAND + param];
        Band {
            on: value(ON) >= 0.5,
            shape: Shape::from_value(value(SHAPE)),
            freq: value(FREQ),
            gain: value(GAIN),
            q: value(Q),
        }
    }

    /// The frequency the band's sections are tuned to at `sample_rate`: its
    /// own, up to [`HIGHEST`].
    pub fn corner(&self, sample_rate: f64) -> f64 {
        self.freq.min(HIGHEST * sample_rate)
    }

    /// The second-order sections the band is made of, one after another:
    /// one, or two or four for the steeper cuts.
    pub fn sections(&self) -> impl Iterator<Item = Section> + use<> {
        let (kind, count) = match self.shape {
            Shape::LowCut12 => (SectionKind::HighPass, 1),
            Shape::LowCut24 => (SectionKind::HighPass, 2),
            Shape::LowCut48 => (SectionKind::HighPass, 4),
            Shape::HighCut12 => (SectionKind::LowPass, 1),
            Shape::HighCut24 => (SectionKind::LowPass, 2),
            Shape::HighCut48 => (SectionKind::LowPass, 4),
            Shape::LowShelf => (SectionKind::LowShelf, 1),
            Shape::Bell => (SectionKind::Bell, 1),
            Shape::Notch => (SectionKind::Notch, 1),
            Shape::HighShelf => (SectionKind::HighShelf, 1),
        };
        let (q, gain) = (self.q, self.gain);
        (0..count).map(move |i| {
            let q = if matches!(kind, SectionKind::HighPass | SectionKind::LowPass) {
                // The Butterworth Q of the section (an order 2 * count
                // filter's pole pair), with the last and steepest raised
                // by the band's Q against 0.71.
                let order = (2 * count) as f64;
                let angle = (2 * i + 1) as f64 * PI / (2.0 * order);
                let butterworth = 1.0 / (2.0 * angle.cos());
                if i + 1 == count {
                    butterworth * q * SQRT_2
                } else {
                    butterworth
                }
            } else {
                q
            };
            Section {
                kind,
                q,
                a: 10_f64.powf(gain / 40.0),
            }
        })
    }

    /// The band's gain at `freq` Hz, from 0 to just under half the sample
    /// rate, as a linear magnitude: 1.0 where it passes unchanged. On or
    /// not.
    pub fn response(&self, freq: f64, sample_rate: f64) -> f64 {
        let warp = |freq: f64| (PI * freq / sample_rate).tan();
        let omega = warp(freq) / warp(self.corner(sample_rate));
        self.sections()
            .map(|section| section.magnitude(omega))
            .product()
    }
}

/// What a second-order section does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    LowPass,
    HighPass,
    LowShelf,
    Bell,
    Notch,
    HighShelf,
}

/// A second-order section of a band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub q: f64,
    /// The cookbook's A for shelves and bells: the square root of the
    /// linear gain, `10^(dB / 40)`.
    pub a: f64,
}

impl Section {
    /// The section's analog prototype, tuned to 1 rad/s: the numerator's and
    /// the denominator's coefficients of s², s and 1.
    pub fn prototype(&self) -> ([f64; 3], [f64; 3]) {
        let Self { q, a, .. } = *self;
        match self.kind {
            SectionKind::LowPass => ([0.0, 0.0, 1.0], [1.0, 1.0 / q, 1.0]),
            SectionKind::HighPass => ([1.0, 0.0, 0.0], [1.0, 1.0 / q, 1.0]),
            SectionKind::Notch => ([1.0, 0.0, 1.0], [1.0, 1.0 / q, 1.0]),
            SectionKind::Bell => ([1.0, a / q, 1.0], [1.0, 1.0 / (a * q), 1.0]),
            SectionKind::LowShelf => {
                let slope = a.sqrt() / q;
                ([a, a * slope, a * a], [a, slope, 1.0])
            }
            SectionKind::HighShelf => {
                let slope = a.sqrt() / q;
                ([a * a, a * slope, a], [1.0, slope, a])
            }
        }
    }

    /// The prototype's gain at `omega` rad/s, as a linear magnitude.
    pub fn magnitude(&self, omega: f64) -> f64 {
        let (num, den) = self.prototype();
        let at = |[s2, s1, s0]: [f64; 3]| (s0 - s2 * omega * omega).hypot(s1 * omega);
        at(num) / at(den)
    }
}

/// The gain of an EQ set as `values`, running at `sample_rate`, at `freq`
/// Hz, from 0 to just under half the sample rate, as a linear magnitude:
/// what the design calls for.
pub fn response(values: &[f64], freq: f64, sample_rate: f64) -> f64 {
    let bands = (0..BANDS)
        .map(|band| Band::of(values, band))
        .filter(|band| band.on)
        .map(|band| band.response(freq, sample_rate))
        .product::<f64>();
    bands * decibels_to_gain(values[OUTPUT])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{NativeDevice, NativeKind};

    fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    fn eq(bands: &[(usize, Shape, f64, f64, f64)]) -> Vec<f64> {
        let mut eq = NativeDevice::new(NativeKind::Eq);
        for b in 0..BANDS {
            eq.values[b * PER_BAND + ON] = 0.0;
        }
        for &(b, shape, freq, gain, q) in bands {
            let at = b * PER_BAND;
            eq.values[at + ON] = 1.0;
            eq.values[at + SHAPE] = Shape::ALL.iter().position(|&s| s == shape).unwrap() as f64;
            eq.values[at + FREQ] = freq;
            eq.values[at + GAIN] = gain;
            eq.values[at + Q] = q;
        }
        eq.values
    }

    #[test]
    fn a_new_eq_is_flat() {
        let eq = NativeDevice::new(NativeKind::Eq);
        for freq in [10.0, 100.0, 1000.0, 10_000.0, 20_000.0] {
            assert_eq!(response(&eq.values, freq, 48_000.0), 1.0);
        }
    }

    #[test]
    fn bells_and_shelves_reach_their_gain() {
        let fs = 48_000.0;
        let bell = eq(&[(0, Shape::Bell, 1000.0, 6.0, 1.0)]);
        assert!((db(response(&bell, 1000.0, fs)) - 6.0).abs() < 1e-9);
        assert!(db(response(&bell, 20.0, fs)).abs() < 0.01);
        let cut = eq(&[(0, Shape::Bell, 1000.0, -6.0, 1.0)]);
        // A cut mirrors a boost of the same Q.
        for freq in [300.0, 700.0, 1500.0] {
            let sum = db(response(&bell, freq, fs)) + db(response(&cut, freq, fs));
            assert!(sum.abs() < 1e-9);
        }
        let low = eq(&[(0, Shape::LowShelf, 200.0, -9.0, FRAC_1_SQRT_2)]);
        assert!((db(response(&low, 5.0, fs)) + 9.0).abs() < 0.01);
        // A shelf is half its gain at its frequency.
        assert!((db(response(&low, 200.0, fs)) + 4.5).abs() < 1e-9);
        assert!(db(response(&low, 15_000.0, fs)).abs() < 0.01);
        let high = eq(&[(0, Shape::HighShelf, 5000.0, 4.0, FRAC_1_SQRT_2)]);
        assert!((db(response(&high, 5000.0, fs)) - 2.0).abs() < 1e-9);
        assert!((db(response(&high, 23_000.0, fs)) - 4.0).abs() < 0.01);
        let output = {
            let mut values = eq(&[]);
            values[OUTPUT] = -3.0;
            values
        };
        assert!((db(response(&output, 440.0, fs)) + 3.0).abs() < 1e-12);
    }

    #[test]
    fn cuts_fall_at_their_slope() {
        let fs = 96_000.0;
        // Each order is 6.02 dB an octave: "12 dB" is two.
        for (shape, order) in [
            (Shape::LowCut12, 2.0),
            (Shape::LowCut24, 4.0),
            (Shape::LowCut48, 8.0),
        ] {
            let values = eq(&[(0, shape, 1000.0, 0.0, FRAC_1_SQRT_2)]);
            // Butterworth: 3 dB down at the frequency.
            assert!(
                (db(response(&values, 1000.0, fs)) + 3.0103).abs() < 1e-3,
                "{shape:?}"
            );
            // The slope, well below it.
            let fall = db(response(&values, 20.0, fs)) - db(response(&values, 10.0, fs));
            let slope = order * db(2.0);
            assert!((fall - slope).abs() < 0.01, "{shape:?}: {fall}");
            assert!(db(response(&values, 20_000.0, fs)).abs() < 0.01);
        }
        let high = eq(&[(0, Shape::HighCut48, 1000.0, 0.0, FRAC_1_SQRT_2)]);
        assert!((db(response(&high, 1000.0, fs)) + 3.0103).abs() < 1e-3);
        assert!(db(response(&high, 20.0, fs)).abs() < 0.01);
        // A higher Q peaks at the frequency.
        let resonant = eq(&[(0, Shape::LowCut48, 1000.0, 0.0, 4.0)]);
        assert!(db(response(&resonant, 1000.0, fs)) > 6.0);
    }

    #[test]
    fn notches_take_out_their_frequency() {
        let values = eq(&[(0, Shape::Notch, 3000.0, 0.0, 2.0)]);
        assert!(response(&values, 3000.0, 48_000.0) < 1e-6);
        assert!(db(response(&values, 100.0, 48_000.0)).abs() < 0.01);
    }

    #[test]
    fn bands_stay_below_half_the_sample_rate() {
        let values = eq(&[(0, Shape::HighShelf, 22_000.0, 6.0, FRAC_1_SQRT_2)]);
        let band = Band::of(&values, 0);
        assert_eq!(band.corner(96_000.0), 22_000.0);
        assert_eq!(band.corner(44_100.0), 0.49 * 44_100.0);
        assert!(response(&values, 20_000.0, 44_100.0).is_finite());
    }
}

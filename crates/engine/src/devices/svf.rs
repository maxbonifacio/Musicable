//! The state variable filter that the EQ is built from: Andrew Simper's
//! trapezoidal SVF ("Linear Trapezoidal Integrated SVF", Cytomic, 2013).
//!
//! Each mode's response is the analog prototype's from the Audio EQ
//! Cookbook under the bilinear transform, prewarped at the frequency, as
//! [`musicable_model::native::eq`] designs them. Unlike a biquad, it stays
//! well behaved while its frequency and gain move, frame by frame, which
//! modulation needs. State and coefficients are `f64`, so low frequencies
//! at high sample rates stay accurate.

use std::f64::consts::PI;

use musicable_model::native::eq::{Section, SectionKind};

/// A second-order section's coefficients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

impl Coefficients {
    /// `section` tuned to `freq` Hz, below half of `sample_rate`.
    pub fn new(section: &Section, freq: f64, sample_rate: f64) -> Self {
        let Section { kind, q, a } = *section;
        let g = (PI * freq / sample_rate).tan();
        let k = 1.0 / q;
        let (g, k, m0, m1, m2) = match kind {
            SectionKind::LowPass => (g, k, 0.0, 0.0, 1.0),
            SectionKind::HighPass => (g, k, 1.0, -k, -1.0),
            SectionKind::Notch => (g, k, 1.0, -k, 0.0),
            SectionKind::Bell => {
                let k = 1.0 / (q * a);
                (g, k, 1.0, k * (a * a - 1.0), 0.0)
            }
            SectionKind::LowShelf => (g / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0),
            SectionKind::HighShelf => (g * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a),
        };
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            a1,
            a2,
            a3: g * a2,
            m0,
            m1,
            m2,
        }
    }
}

/// One channel's filter memory for a section.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct State {
    ic1eq: f64,
    ic2eq: f64,
}

impl State {
    /// Filters one sample.
    #[inline]
    pub fn tick(&mut self, c: &Coefficients, v0: f64) -> f64 {
        let v3 = v0 - self.ic2eq;
        let v1 = c.a1 * self.ic1eq + c.a2 * v3;
        let v2 = self.ic2eq + c.a2 * self.ic1eq + c.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        c.m0 * v0 + c.m1 * v1 + c.m2 * v2
    }

    /// Filters `samples` in place.
    pub fn run(&mut self, c: &Coefficients, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.tick(c, f64::from(*sample)) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat bell or shelf passes the signal unchanged, bit for bit.
    #[test]
    fn flat_sections_pass_the_signal_unchanged() {
        for kind in [
            SectionKind::Bell,
            SectionKind::LowShelf,
            SectionKind::HighShelf,
        ] {
            let section = Section {
                kind,
                q: 0.7,
                a: 1.0,
            };
            let c = Coefficients::new(&section, 1000.0, 48_000.0);
            let mut state = State::default();
            for i in 0..1000 {
                let x = ((i * 7919) % 1000) as f64 / 1000.0 - 0.5;
                assert_eq!(state.tick(&c, x), x, "{kind:?}");
            }
        }
    }

    /// A low pass's DC gain is 1, and it takes out half the sample rate.
    #[test]
    fn low_passes_pass_dc_and_stop_nyquist() {
        let section = Section {
            kind: SectionKind::LowPass,
            q: 0.7,
            a: 1.0,
        };
        let c = Coefficients::new(&section, 1000.0, 48_000.0);
        let mut dc = State::default();
        let mut nyquist = State::default();
        let (mut last_dc, mut last_nyquist) = (0.0, 0.0);
        for i in 0..48_000 {
            last_dc = dc.tick(&c, 1.0);
            last_nyquist = nyquist.tick(&c, if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        assert!((last_dc - 1.0).abs() < 1e-12);
        assert!(last_nyquist.abs() < 1e-12);
    }
}

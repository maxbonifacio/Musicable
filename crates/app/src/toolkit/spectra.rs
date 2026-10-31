//! Spectra, for audio.spectrum and audio.compare: a sound's mean power in
//! each bin, averaged over windows that overlap, its levels in bands, its
//! strongest peaks and their notes, and how pure and steady the partials
//! of a pitch are.

use std::f64::consts::TAU;
use std::sync::Arc;

use realfft::num_complex::Complex64;
use realfft::{RealFftPlanner, RealToComplex};

/// The windows are about this long: 16384 frames at 44.1 and 48 kHz, so
/// the bins are about 3 Hz apart.
const WINDOW_SECONDS: f64 = 0.3;
/// The fewest frames a spectrum is taken of.
const SHORTEST: usize = 256;

/// The 31 third octaves of IEC 61260, by their nominal centres, from 20 Hz
/// to 20 kHz: the exact centre of the `i`th is 1000 × 10^((i − 17) / 10).
pub(super) const THIRD_OCTAVES: [f64; 31] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
];
/// The 10 octaves, by their nominal centres, from 31.5 Hz to 16 kHz: the
/// exact centre of the `i`th is 1000 × 10^(3(i − 5) / 10).
pub(super) const OCTAVES: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0,
];

/// A band of a spectrum, by its nominal centre and its edges, in hertz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Band {
    pub(crate) centre: f64,
    pub(crate) from: f64,
    pub(crate) to: f64,
}

/// The third octaves, or with `octaves` the octaves, whose centres are
/// below `nyquist`, their top edges no higher.
pub(crate) fn bands(octaves: bool, nyquist: f64) -> Vec<Band> {
    let (nominal, step, first): (&[f64], f64, f64) = match octaves {
        true => (&OCTAVES, 0.3, -5.0),
        false => (&THIRD_OCTAVES, 0.1, -17.0),
    };
    let edge = 10f64.powf(step / 2.0);
    (nominal.iter().enumerate())
        .map(|(i, &centre)| (centre, 1000.0 * 10f64.powf(step * (i as f64 + first))))
        .filter(|&(_, exact)| exact < nyquist)
        .map(|(centre, exact)| Band {
            centre,
            from: exact / edge,
            to: (exact * edge).min(nyquist),
        })
        .collect()
}

/// A mean power spectrum, taken a block of samples at a time: Welch's
/// method, with Hann windows that overlap by half.
pub(super) struct Welch {
    rate: f64,
    size: usize,
    fft: Arc<dyn RealToComplex<f64>>,
    window: Vec<f64>,
    /// The windows' power, which the bins are scaled by, so they add up to
    /// the mean square.
    window_power: f64,
    input: Vec<f64>,
    output: Vec<Complex64>,
    /// Samples not taken yet: the start of the next window.
    held: Vec<f64>,
    bins: Vec<f64>,
    frames: usize,
}

impl Welch {
    pub(super) fn new(rate: u32) -> Self {
        let size = ((f64::from(rate) * WINDOW_SECONDS) as usize).next_power_of_two();
        Self::sized(rate, size)
    }

    fn sized(rate: u32, size: usize) -> Self {
        let window: Vec<f64> = (0..size)
            .map(|i| 0.5 - 0.5 * (TAU * i as f64 / size as f64).cos())
            .collect();
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
        Self {
            rate: f64::from(rate),
            size,
            window_power: window.iter().map(|w| w * w).sum(),
            window,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            bins: vec![0.0; size / 2 + 1],
            fft,
            held: Vec::new(),
            frames: 0,
        }
    }

    /// Takes the next samples of the sound.
    pub(super) fn push(&mut self, samples: &[f64]) {
        self.held.extend_from_slice(samples);
        let hop = self.size / 2;
        let mut start = 0;
        while start + self.size <= self.held.len() {
            self.take(start);
            start += hop;
        }
        self.held.drain(..start);
    }

    fn take(&mut self, start: usize) {
        let samples = &self.held[start..start + self.size];
        for ((slot, sample), w) in self.input.iter_mut().zip(samples).zip(&self.window) {
            *slot = sample * w;
        }
        self.fft
            .process(&mut self.input, &mut self.output)
            .expect("the buffers are the plan's");
        let last = self.output.len() - 1;
        let scale = self.size as f64 * self.window_power;
        for (k, (bin, value)) in self.bins.iter_mut().zip(&self.output).enumerate() {
            let both = if k == 0 || k == last { 1.0 } else { 2.0 };
            *bin += both * value.norm_sqr() / scale;
        }
        self.frames += 1;
    }

    /// The spectrum, or `None` if the sound was too short. A sound shorter
    /// than a window has a spectrum of the one window that fits it.
    pub(super) fn finish(mut self) -> Option<Spectrum> {
        if self.frames == 0 {
            if self.held.len() < SHORTEST {
                return None;
            }
            let size = 1 << self.held.len().ilog2();
            let mut shorter = Self::sized(self.rate as u32, size);
            shorter.push(&self.held[..size]);
            return shorter.finish();
        }
        let frames = self.frames as f64;
        self.bins.iter_mut().for_each(|bin| *bin /= frames);
        Some(Spectrum {
            bins: self.bins,
            size: self.size,
            rate: self.rate,
        })
    }
}

/// A sound's mean power in each bin, the bins adding up to its mean
/// square.
#[derive(Debug, Clone)]
pub(super) struct Spectrum {
    pub(super) bins: Vec<f64>,
    size: usize,
    rate: f64,
}

impl Spectrum {
    /// How far apart the bins are, in hertz.
    pub(super) fn width(&self) -> f64 {
        self.rate / self.size as f64
    }

    /// The bin `k`'s frequency.
    pub(super) fn hz(&self, k: usize) -> f64 {
        k as f64 * self.width()
    }

    /// The mean power from `from` to `to` hertz, each bin counting for the
    /// share of its width in the band.
    pub(super) fn power(&self, from: f64, to: f64) -> f64 {
        let width = self.width();
        let first = ((from / width - 0.5).floor().max(0.0)) as usize;
        let last = ((to / width + 0.5).ceil() as usize).min(self.bins.len() - 1);
        (first..=last)
            .map(|k| {
                let (low, high) = ((k as f64 - 0.5) * width, (k as f64 + 0.5) * width);
                let share = (high.min(to) - low.max(from)).max(0.0) / width;
                share * self.bins[k]
            })
            .sum()
    }

    /// The level from `from` to `to` hertz, as an RMS level in dBFS, or
    /// `None` for silence.
    pub(super) fn level(&self, from: f64, to: f64) -> Option<f64> {
        power_db(self.power(from, to))
    }

    /// The whole spectrum's level, as an RMS level in dBFS.
    pub(super) fn total(&self) -> Option<f64> {
        power_db(self.bins.iter().sum())
    }

    /// The centre of its power, in hertz, without the DC.
    pub(super) fn centroid(&self) -> Option<f64> {
        let power: f64 = self.bins[1..].iter().sum();
        let moment: f64 = (self.bins.iter().enumerate().skip(1))
            .map(|(k, p)| self.hz(k) * p)
            .sum();
        (power > 0.0).then(|| moment / power)
    }

    /// The frequency below which `share` of its power is, without the DC.
    pub(super) fn rolloff(&self, share: f64) -> Option<f64> {
        let power: f64 = self.bins[1..].iter().sum();
        if power <= 0.0 {
            return None;
        }
        let mut sum = 0.0;
        for (k, p) in self.bins.iter().enumerate().skip(1) {
            sum += p;
            if sum >= share * power {
                return Some(self.hz(k));
            }
        }
        Some(self.hz(self.bins.len() - 1))
    }

    /// How flat it is from 20 Hz to 20 kHz: the geometric mean of the
    /// bins' power over their arithmetic mean, 1 for white noise, near 0
    /// for a tone.
    pub(super) fn flatness(&self) -> Option<f64> {
        let first = (20.0 / self.width()).ceil() as usize;
        let last = ((20_000.0 / self.width()) as usize).min(self.bins.len() - 1);
        let bins = self.bins.get(first.max(1)..=last)?;
        let mean = bins.iter().sum::<f64>() / bins.len() as f64;
        if mean <= 0.0 {
            return None;
        }
        // Silent bins count as far below the rest, not as minus infinity.
        let floor = mean * 1e-12;
        let log = bins.iter().map(|p| p.max(floor).ln()).sum::<f64>() / bins.len() as f64;
        Some(log.exp() / mean)
    }

    /// Its strongest peaks, `count` at most, strongest first: bins higher
    /// than the 4 either side, each placed between bins by a parabola
    /// through the decibels of its neighbours, with the power of the 5
    /// bins around it, which hold a tone's.
    pub(super) fn peaks(&self, count: usize) -> Vec<Peak> {
        const REACH: usize = 4;
        let bins = &self.bins;
        let mut peaks: Vec<Peak> = (REACH..bins.len().saturating_sub(REACH))
            .filter(|&k| {
                let around = (k - REACH..=k + REACH).filter(|&j| j != k);
                bins[k] > 0.0 && around.clone().all(|j| bins[j] < bins[k])
            })
            .map(|k| {
                let db = |j: usize| 10.0 * bins[j].max(1e-30).log10();
                let (left, middle, right) = (db(k - 1), db(k), db(k + 1));
                let bend = left - 2.0 * middle + right;
                let shift = match bend < 0.0 {
                    true => 0.5 * (left - right) / bend,
                    false => 0.0,
                };
                Peak {
                    hz: (k as f64 + shift.clamp(-0.5, 0.5)) * self.width(),
                    power: bins[k - 2..=k + 2].iter().sum(),
                }
            })
            .collect();
        peaks.sort_by(|a, b| b.power.total_cmp(&a.power));
        peaks.truncate(count);
        peaks
    }
}

/// A peak of a spectrum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Peak {
    pub(super) hz: f64,
    pub(super) power: f64,
}

/// A mean square in decibels, or `None` for silence.
pub(super) fn power_db(power: f64) -> Option<f64> {
    (power > 0.0).then(|| 10.0 * power.log10())
}

/// The note nearest `hz`, such as A4 for 440 Hz, and how many cents
/// above it `hz` is: A4 is 440 Hz, and middle C is C4.
pub(super) fn note(hz: f64) -> (String, f64) {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let nearest = midi.round();
    let octave = (nearest / 12.0).floor() as i64 - 1;
    let name = NAMES[nearest.rem_euclid(12.0) as usize];
    (format!("{name}{octave}"), (midi - nearest) * 100.0)
}

/// Follows the level of one partial of a sound: the sound turned down by
/// the partial's frequency and low-passed, so what's left is the partial's
/// envelope, with any partner close enough to beat with it.
pub(super) struct Partial {
    /// The oscillator's step, as a turn of the unit circle.
    step: Complex64,
    turn: Complex64,
    /// Four one-pole low-passes, one after the other, which take the
    /// partials either side down by 24 dB an octave.
    coefficient: f64,
    poles: [Complex64; 4],
    /// Samples since the start, and between readings of the envelope.
    at: usize,
    every: usize,
    settle: usize,
    envelope: Vec<f64>,
}

impl Partial {
    /// Follows the partial at `hz` in a sound at `rate`, reading its
    /// envelope every 10 ms through a low-pass at `cutoff` hertz.
    pub(super) fn new(hz: f64, cutoff: f64, rate: u32) -> Self {
        let rate = f64::from(rate);
        let coefficient = (-TAU * cutoff / rate).exp();
        Self {
            step: Complex64::from_polar(1.0, -TAU * hz / rate),
            turn: Complex64::new(1.0, 0.0),
            coefficient,
            poles: [Complex64::new(0.0, 0.0); 4],
            at: 0,
            every: ((rate / 100.0) as usize).max(1),
            // Ten time constants, by when the filters have settled.
            settle: (10.0 * rate / (TAU * cutoff)) as usize,
            envelope: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, samples: &[f64]) {
        let a = self.coefficient;
        for &sample in samples {
            let mut value = self.turn * sample;
            for pole in &mut self.poles {
                *pole = value * (1.0 - a) + *pole * a;
                value = *pole;
            }
            self.turn *= self.step;
            self.at += 1;
            if self.at.is_multiple_of(4096) {
                // Keeps the oscillator on the unit circle.
                self.turn /= self.turn.norm();
            }
            if self.at > self.settle && self.at.is_multiple_of(self.every) {
                // Twice the low-passed magnitude is the partial's peak.
                self.envelope.push(2.0 * self.poles[3].norm());
            }
        }
    }

    /// How much its envelope ripples, in decibels: from its 10th
    /// percentile to its 90th. `None` without a reading, or if it's
    /// silent.
    pub(super) fn ripple(mut self) -> Option<f64> {
        if self.envelope.is_empty() {
            return None;
        }
        self.envelope.sort_by(f64::total_cmp);
        let at = |share: f64| self.envelope[((self.envelope.len() - 1) as f64 * share) as usize];
        let (low, high) = (at(0.1), at(0.9));
        (low > 0.0).then(|| 20.0 * (high / low).log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_s_power_is_in_its_peak_and_its_band() {
        let rate = 48_000;
        let sine: Vec<f64> = (0..96_000)
            .map(|n| 0.5 * (TAU * 1000.0 * n as f64 / 48_000.0).sin())
            .collect();
        let mut welch = Welch::new(rate);
        // In pieces, as files are read.
        for piece in sine.chunks(10_000) {
            welch.push(piece);
        }
        let spectrum = welch.finish().unwrap();
        // 0.5 is -6.02 dBFS, and as an RMS level, -9.03.
        assert!((spectrum.total().unwrap() + 9.03).abs() < 0.01);
        let band = bands(false, 24_000.0)[17];
        assert_eq!(band.centre, 1000.0);
        assert!((spectrum.level(band.from, band.to).unwrap() + 9.03).abs() < 0.01);
        let peaks = spectrum.peaks(3);
        assert!((peaks[0].hz - 1000.0).abs() < 0.5, "{peaks:?}");
        assert!((power_db(peaks[0].power).unwrap() + 9.03).abs() < 0.05);
        assert!((spectrum.centroid().unwrap() - 1000.0).abs() < 2.0);
        assert!(spectrum.flatness().unwrap() < 0.01);
    }

    #[test]
    fn notes_are_named_with_their_octave_and_cents() {
        assert_eq!(note(440.0), ("A4".to_owned(), 0.0));
        let (name, cents) = note(261.63);
        assert_eq!(name, "C4");
        assert!(cents.abs() < 0.1);
        let (name, cents) = note(450.0);
        assert_eq!(name, "A4");
        assert!((cents - 38.9).abs() < 0.1);
    }

    #[test]
    fn two_partials_close_together_beat_and_one_alone_does_not() {
        let rate = 48_000;
        let steady: Vec<f64> = (0..96_000)
            .map(|n| 0.5 * (TAU * 220.0 * n as f64 / 48_000.0).sin())
            .collect();
        // 2 Hz apart, at equal levels: the envelope dips to nothing twice
        // a second, and at half the level, by 9.5 dB from 1.5 to 0.5.
        let beating: Vec<f64> = (0..96_000)
            .map(|n| {
                let t = n as f64 / 48_000.0;
                0.5 * (TAU * 220.0 * t).sin() + 0.25 * (TAU * 222.0 * t).sin()
            })
            .collect();
        let ripple = |samples: &[f64]| {
            let mut partial = Partial::new(220.0, 25.0, rate);
            partial.push(samples);
            partial.ripple().unwrap()
        };
        assert!(ripple(&steady) < 0.1);
        let beats = ripple(&beating);
        assert!((7.0..9.6).contains(&beats), "{beats}");
    }
}

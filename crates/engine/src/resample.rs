//! Sample-rate conversion for audio clips whose files aren't at the
//! device's rate: windowed-sinc interpolation.
//!
//! Each output frame falls at a fractional position in the file, and is the
//! sum of the file's frames around it, weighted by a Kaiser-windowed sinc
//! centred on it. The sinc cuts off a little below the lower of the two
//! Nyquist frequencies, so converting up adds no images and converting down
//! folds nothing back: 32 zero crossings each side of a Kaiser window with
//! β = 9 put the stopband about 90 dB down by Nyquist, and pass everything
//! up to about 41% of the lower sample rate (18 kHz from 44.1 kHz). The
//! weights are normalised to sum to one, so silence and DC come through
//! exactly.
//!
//! Every frame's weights come from its position alone, so a frame renders
//! the same samples however the blocks fall.

use std::f64::consts::PI;
use std::sync::OnceLock;

/// The sinc's zero crossings each side of the centre.
const HALF_WIDTH: usize = 32;
/// Points in the kernel's table for each frame of the file.
const RESOLUTION: usize = 256;
/// The sinc's cutoff as a fraction of the lower Nyquist frequency, leaving
/// room for the window's transition band below it.
const CUTOFF: f64 = 0.91;
/// The Kaiser window's shape: higher trades a wider transition band for
/// more attenuation.
const BETA: f64 = 9.0;

/// The windowed sinc, tabulated from −[`HALF_WIDTH`] to [`HALF_WIDTH`].
struct Kernel {
    table: Box<[f32]>,
}

impl Kernel {
    /// The one table every resampler shares, made the first time one is.
    fn get() -> &'static Kernel {
        static KERNEL: OnceLock<Kernel> = OnceLock::new();
        KERNEL.get_or_init(|| {
            let points = 2 * HALF_WIDTH * RESOLUTION + 1;
            let table = (0..points)
                .map(|i| {
                    let t = i as f64 / RESOLUTION as f64 - HALF_WIDTH as f64;
                    (CUTOFF * sinc(CUTOFF * t) * kaiser(t / HALF_WIDTH as f64)) as f32
                })
                .collect();
            Kernel { table }
        })
    }

    /// The kernel at `t` frames from the centre, between table points in a
    /// straight line, and zero beyond its ends.
    #[inline]
    fn at(&self, t: f64) -> f32 {
        let u = (t + HALF_WIDTH as f64) * RESOLUTION as f64;
        if !(0.0..(self.table.len() - 1) as f64).contains(&u) {
            return 0.0;
        }
        let i = u as usize;
        let (a, b) = (self.table[i], self.table[i + 1]);
        a + (b - a) * (u - i as f64) as f32
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// The Kaiser window at `u`, from −1 to 1.
fn kaiser(u: f64) -> f64 {
    bessel_i0(BETA * (1.0 - u * u).max(0.0).sqrt()) / bessel_i0(BETA)
}

/// The zeroth-order modified Bessel function of the first kind, from its
/// power series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..100 {
        let half = x / (2.0 * k as f64);
        term *= half * half;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Converts from one sample rate to another.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resampler {
    kernel: &'static Kernel,
    /// File frames for each device frame: the file's rate over the device's.
    step: f64,
    /// What distances in file frames are scaled by to reach the kernel:
    /// below one when converting down, so the cutoff falls with the rate.
    scale: f64,
    /// File frames each side of a position that weigh in.
    reach: usize,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel").finish_non_exhaustive()
    }
}

impl Resampler {
    /// Reads a file at `from` Hz for a device at `to` Hz.
    pub fn new(from: u32, to: u32) -> Self {
        let step = f64::from(from) / f64::from(to);
        let scale = step.recip().min(1.0);
        Self {
            kernel: Kernel::get(),
            step,
            scale,
            reach: (HALF_WIDTH as f64 / scale).ceil() as usize,
        }
    }

    pub fn step(&self) -> f64 {
        self.step
    }

    /// File frames each side of a position that weigh in: the weights for
    /// a position cover `2 * reach` frames.
    pub fn reach(&self) -> usize {
        self.reach
    }

    /// The weights of the file frames around position `x` into `weights`,
    /// which holds `2 * reach` of them, and the first of those frames.
    pub fn weights(&self, x: f64, weights: &mut [f32]) -> i64 {
        debug_assert_eq!(weights.len(), 2 * self.reach);
        let first = x.floor() as i64 - self.reach as i64 + 1;
        let mut sum = 0.0;
        for (j, weight) in weights.iter_mut().enumerate() {
            let t = (x - (first + j as i64) as f64) * self.scale;
            *weight = self.kernel.at(t);
            sum += *weight;
        }
        if sum != 0.0 {
            let normalise = sum.recip();
            for weight in weights.iter_mut() {
                *weight *= normalise;
            }
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    /// `input` at `from` Hz, resampled to `to` Hz: `frames` frames.
    fn resample(input: &[f32], from: u32, to: u32, frames: usize) -> Vec<f32> {
        let r = Resampler::new(from, to);
        let mut weights = vec![0.0; 2 * r.reach()];
        (0..frames)
            .map(|i| {
                let first = r.weights(i as f64 * r.step(), &mut weights);
                weights
                    .iter()
                    .enumerate()
                    .map(|(j, w)| {
                        let n = first + j as i64;
                        let x = usize::try_from(n).ok().and_then(|n| input.get(n));
                        x.copied().unwrap_or(0.0) * w
                    })
                    .sum()
            })
            .collect()
    }

    fn sine(frequency: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (TAU * frequency * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    /// The worst difference between `a` and `b`, away from the ends.
    fn error(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        (100..n - 100)
            .map(|i| (a[i] - b[i]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn tones_in_the_passband_come_through() {
        for (from, to) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (96_000, 48_000),
            (22_050, 48_000),
        ] {
            for frequency in [100.0, 1_000.0, 0.4 * f64::from(from.min(to))] {
                let out = resample(&sine(frequency, from, 20_000), from, to, 8_000);
                let expected = sine(frequency, to, 8_000);
                let e = error(&out, &expected);
                assert!(e < 2e-3, "{frequency} Hz, {from} to {to} Hz: off by {e}");
            }
        }
    }

    #[test]
    fn what_wont_fit_the_new_rate_is_filtered_out() {
        // 30 kHz can't be played at 48 kHz.
        let out = resample(&sine(30_000.0, 96_000, 20_000), 96_000, 48_000, 8_000);
        let loudest = out[100..7_900].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(loudest < 1e-3, "{loudest}");
    }

    #[test]
    fn dc_and_silence_come_through_exactly() {
        for (from, to) in [(44_100, 48_000), (96_000, 44_100)] {
            assert!(
                resample(&[0.0; 1000], from, to, 500)
                    .iter()
                    .all(|&s| s == 0.0)
            );
            let dc = resample(&[0.5; 4000], from, to, 1000);
            assert!(dc[100..900].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        }
    }
}

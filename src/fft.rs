//! Thin wrapper around `realfft` with cached plans and scratch buffers.

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct Fft {
    pub n: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    scratch_f: Vec<Complex32>,
    scratch_i: Vec<Complex32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let scratch_f = fwd.make_scratch_vec();
        let scratch_i = inv.make_scratch_vec();
        Fft { n, fwd, inv, scratch_f, scratch_i }
    }
    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }
    /// Forward real FFT, X[k] = sum_m x[m] e^{-2 pi i k m / n}. `input` is used as scratch.
    pub fn forward(&mut self, input: &mut [f32], out: &mut [Complex32]) {
        self.fwd
            .process_with_scratch(input, out, &mut self.scratch_f)
            .expect("fft forward");
    }
    /// Unnormalised inverse real FFT (result is n times the true inverse). `spec` is used as scratch.
    pub fn inverse(&mut self, spec: &mut [Complex32], out: &mut [f32]) {
        let k = spec.len();
        spec[0].im = 0.0;
        if self.n % 2 == 0 {
            spec[k - 1].im = 0.0;
        }
        self.inv
            .process_with_scratch(spec, out, &mut self.scratch_i)
            .expect("fft inverse");
    }
}

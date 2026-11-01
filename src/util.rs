//! Small helpers: deterministic RNG, phase wrapping, window-size scaling.

use std::f32::consts::PI;

/// SplitMix64: tiny, seeded, deterministic PRNG (Steele, Lea, Flood 2014).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    #[inline]
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    /// Approximately standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Wrap a phase to (-pi, pi].
#[inline]
pub fn wrap(x: f32) -> f32 {
    x - 2.0 * PI * (x / (2.0 * PI)).round()
}

/// Scale a window length given for 44.1 kHz to another sample rate, keeping a power of two.
pub fn scaled_pow2(base_at_44k: usize, sr: u32) -> usize {
    let want = base_at_44k as f64 * sr as f64 / 44100.0;
    let p = want.log2().round().max(4.0) as u32;
    1usize << p
}

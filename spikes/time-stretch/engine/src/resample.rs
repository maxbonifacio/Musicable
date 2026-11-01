//! Kaiser-windowed sinc resampler (offline, arbitrary ratio).

use std::f64::consts::PI;

fn bessel_i0(x: f64) -> f64 {
    let mut s = 1.0;
    let mut t = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        t *= q / (k as f64 * k as f64);
        s += t;
        if t < 1e-17 * s {
            break;
        }
    }
    s
}

/// out[j] = x(j * step), band-limited to min(1, 1/step) of the input Nyquist.
pub fn resample(x: &[f32], n_out: usize, step: f64) -> Vec<f32> {
    const Z: f64 = 32.0; // zero crossings per side
    const TAB: usize = 512; // table points per zero crossing
    const BETA: f64 = 9.0;
    let fc = (1.0 / step).min(1.0) * 0.96;
    let ntab = (Z as usize) * TAB + 2;
    let i0b = bessel_i0(BETA);
    let table: Vec<f64> = (0..ntab)
        .map(|i| {
            let d = i as f64 / TAB as f64;
            if d >= Z {
                return 0.0;
            }
            let s = if d == 0.0 { 1.0 } else { (PI * d).sin() / (PI * d) };
            let r = d / Z;
            s * bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / i0b
        })
        .collect();
    let half = Z / fc;
    let n = x.len() as i64;
    let mut out = vec![0f32; n_out];
    for (j, o) in out.iter_mut().enumerate() {
        let t = j as f64 * step;
        let lo = ((t - half).ceil() as i64).max(0);
        let hi = ((t + half).floor() as i64).min(n - 1);
        let mut acc = 0.0f64;
        let mut i = lo;
        while i <= hi {
            let d = ((t - i as f64) * fc).abs() * TAB as f64;
            let di = d as usize;
            if di + 1 < ntab {
                let fr = d - di as f64;
                let h = table[di] + (table[di + 1] - table[di]) * fr;
                acc += h * x[i as usize] as f64;
            }
            i += 1;
        }
        *o = (acc * fc) as f32;
    }
    out
}

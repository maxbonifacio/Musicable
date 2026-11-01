//! Residual ("noise") path: resynthesis from the time-smoothed, stretched cross-spectral
//! matrix with fresh random phases.
//!
//! For each synthesis frame at output y we take the input position c = y/R, average the
//! per-bin channel covariance C_ij = X_i X_j^* over analysis frames within +-hw input
//! samples of c (triangular weights, never across an onset), and draw Y = chol(C) z with
//! z_i = exp(i theta_i), theta uniform (seeded). This keeps the expected power spectrum,
//! inter-channel level difference, phase difference and coherence of every bin, while the
//! random phases give the texture a natural, unfrozen fine structure. Frames are overlap-
//! added with a Hann window and normalised by sqrt(sum w^2) (incoherent sum).

use crate::fft::Fft;
use crate::util::{Rng, scaled_pow2};
use crate::window::{Win, add_frame, add_weight, load_frame};
use realfft::num_complex::Complex32;
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct NoiseParams {
    pub win: usize,
    pub syn_hop: usize,
    pub grid_hop: usize,
    pub seed: u64,
}

impl NoiseParams {
    pub fn new(sr: u32) -> Self {
        let win = scaled_pow2(2048, sr);
        NoiseParams { win, syn_hop: win / 4, grid_hop: win / 8, seed: 0x5eed }
    }
}

/// In-place Cholesky of a small Hermitian matrix (row-major n x n), lower triangle result.
fn cholesky(a: &mut [Complex32], n: usize) {
    for j in 0..n {
        let mut d = a[j * n + j].re;
        for k in 0..j {
            d -= a[j * n + k].norm_sqr();
        }
        let ljj = if d > 0.0 { d.sqrt() } else { 0.0 };
        a[j * n + j] = Complex32::new(ljj, 0.0);
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k].conj();
            }
            a[i * n + j] = if ljj > 0.0 { s / ljj } else { Complex32::new(0.0, 0.0) };
        }
        for i in 0..j {
            a[i * n + j] = Complex32::new(0.0, 0.0);
        }
    }
}

pub fn stretch_noise(x: &[Vec<f32>], r: f64, n_out: usize, onsets: &[i64], p: &NoiseParams) -> Vec<Vec<f32>> {
    let nch = x.len();
    if (r - 1.0).abs() < 1e-9 {
        // identity at R = 1: random phases only make sense when the time base changes
        return x.iter().map(|c| {
            let mut v = c.clone();
            v.resize(n_out, 0.0);
            v
        }).collect();
    }
    let l = p.win;
    let kb = l / 2 + 1;
    let win = Win::hann(l);
    let mut fft = Fft::new(l);
    let g = p.grid_hop as i64;
    let hop = p.syn_hop as i64;
    let hw = (l as f64 / 2.0).max(p.syn_hop as f64 / r);
    let scale = (l as f64 / win.sum_sq()).sqrt() as f32 / l as f32;
    let w2: Vec<f32> = win.w.iter().map(|v| v * v).collect();

    let mut out = vec![vec![0f32; n_out]; nch];
    let mut norm = vec![0f32; n_out];
    // cache of grid analysis frames: front index j0
    let mut cache: VecDeque<Vec<Vec<Complex32>>> = VecDeque::new();
    let mut j0: i64 = i64::MIN;
    let mut buf = vec![0f32; l];
    let mut cov = vec![Complex32::new(0.0, 0.0); kb * nch * nch];
    let mut a = vec![Complex32::new(0.0, 0.0); nch * nch];
    let mut ys = vec![vec![Complex32::new(0.0, 0.0); kb]; nch];
    let mut obuf = vec![0f32; l];
    let side = |t: f64| onsets.partition_point(|&o| (o as f64) <= t);

    let n_lo = -(l as i64 / hop) - 1;
    let n_hi = (n_out as i64 + l as i64) / hop + 1;
    for n in n_lo..=n_hi {
        let y = n * hop;
        let c = y as f64 / r;
        let jlo = ((c - hw) / g as f64).ceil() as i64;
        let jhi = ((c + hw) / g as f64).floor() as i64;
        // maintain cache [jlo, jhi]
        if j0 == i64::MIN || jlo < j0 {
            cache.clear();
            j0 = jlo;
        }
        while j0 < jlo && !cache.is_empty() {
            cache.pop_front();
            j0 += 1;
        }
        if cache.is_empty() {
            j0 = jlo;
        }
        while j0 + (cache.len() as i64) <= jhi {
            let j = j0 + cache.len() as i64;
            let mut fr = vec![vec![Complex32::new(0.0, 0.0); kb]; nch];
            for ch in 0..nch {
                load_frame(&x[ch], j * g, &win.w, &mut buf);
                fft.forward(&mut buf, &mut fr[ch]);
            }
            cache.push_back(fr);
        }
        // weights
        let sc = side(c);
        let mut wts: Vec<(usize, f32)> = Vec::new();
        let mut wsum = 0f32;
        for j in jlo..=jhi {
            let gc = (j * g) as f64;
            if side(gc) != sc {
                continue;
            }
            let wv = (1.0 - (gc - c).abs() / hw).max(0.0) as f32;
            if wv > 0.0 {
                wts.push(((j - j0) as usize, wv));
                wsum += wv;
            }
        }
        if wsum <= 0.0 {
            let j = ((c / g as f64).round() as i64).clamp(jlo, jhi);
            wts = vec![((j - j0) as usize, 1.0)];
            wsum = 1.0;
        }
        cov.iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0));
        for &(ji, wv) in &wts {
            let fr = &cache[ji];
            let wv = wv / wsum;
            for k in 0..kb {
                let base = k * nch * nch;
                for i in 0..nch {
                    for jj in 0..=i {
                        cov[base + i * nch + jj] += fr[i][k] * fr[jj][k].conj() * wv;
                    }
                }
            }
        }
        let mut rng = Rng::new(p.seed ^ (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for k in 0..kb {
            let base = k * nch * nch;
            for i in 0..nch {
                for jj in 0..nch {
                    a[i * nch + jj] = if jj <= i { cov[base + i * nch + jj] } else { cov[base + jj * nch + i].conj() };
                }
            }
            cholesky(&mut a, nch);
            let edge = k == 0 || k == kb - 1;
            let mut z = [Complex32::new(0.0, 0.0); 8];
            for zi in z.iter_mut().take(nch.min(8)) {
                let th = (rng.uniform() * std::f64::consts::TAU) as f32;
                *zi = if edge { Complex32::new(if th.cos() >= 0.0 { 1.0 } else { -1.0 }, 0.0) } else { Complex32::new(th.cos(), th.sin()) };
            }
            for i in 0..nch {
                let mut s = Complex32::new(0.0, 0.0);
                for jj in 0..=i.min(7) {
                    s += a[i * nch + jj] * z[jj];
                }
                ys[i][k] = s * scale;
            }
        }
        for ch in 0..nch {
            fft.inverse(&mut ys[ch], &mut obuf);
            add_frame(&mut out[ch], y, &obuf, &win.w, 1.0);
        }
        add_weight(&mut norm, y, &w2);
    }
    for ch in out.iter_mut() {
        for (v, &nv) in ch.iter_mut().zip(norm.iter()) {
            *v = if nv > 1e-6 { *v / nv.sqrt() } else { 0.0 };
        }
    }
    out
}

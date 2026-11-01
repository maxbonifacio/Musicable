//! Classic phase vocoder with identity phase locking (Laroche & Dolson 1999), per channel,
//! used only by the `hptsm` baseline (Driedger, Mueller, Ewert 2014 use a phase-locked PV
//! for the harmonic part; libtsm's hps_tsm uses Hann 2048 / synthesis hop 512 at 22.05 kHz,
//! i.e. 4096 / 1024 at 44.1 kHz).

use crate::fft::Fft;
use crate::util::wrap;
use crate::window::{Win, add_frame, add_weight, load_frame};
use realfft::num_complex::Complex32;
use std::f32::consts::PI;

pub fn stretch_classic_pv(x: &[Vec<f32>], r: f64, n_out: usize, win: usize, syn_hop: usize) -> Vec<Vec<f32>> {
    let nch = x.len();
    let l = win;
    let kb = l / 2 + 1;
    let w = Win::hann(l);
    let w2: Vec<f32> = w.w.iter().map(|v| v * v).collect();
    let mut fft = Fft::new(l);
    let hop = syn_hop as i64;
    let mut out = vec![vec![0f32; n_out]; nch];
    let mut norm = vec![0f32; n_out];
    let n_lo = -(l as i64 / hop) - 1;
    let n_hi = (n_out as i64 + l as i64) / hop + 1;
    let mut buf = vec![0f32; l];
    let mut spec = vec![Complex32::new(0.0, 0.0); kb];
    let mut obuf = vec![0f32; l];
    let mut mag = vec![0f32; kb];
    let mut pha = vec![0f32; kb];
    let mut prev_pha = vec![vec![0f32; kb]; nch];
    let mut syn = vec![vec![0f32; kb]; nch];
    let mut first = true;
    let mut prev_c = 0i64;
    let mut peaks: Vec<usize> = Vec::new();
    for n in n_lo..=n_hi {
        let y = n * hop;
        let c = (y as f64 / r).round() as i64;
        let dc = (c - prev_c) as f32;
        for ch in 0..nch {
            load_frame(&x[ch], c, &w.w, &mut buf);
            fft.forward(&mut buf, &mut spec);
            for k in 0..kb {
                mag[k] = spec[k].norm();
                pha[k] = spec[k].arg();
            }
            let s = &mut syn[ch];
            if first {
                s.copy_from_slice(&pha);
            } else {
                // peaks: larger than two neighbours on each side
                peaks.clear();
                for k in 0..kb {
                    let mut ok = true;
                    for d in 1..=2usize {
                        if (k >= d && mag[k - d] >= mag[k]) || (k + d < kb && mag[k + d] > mag[k]) {
                            ok = false;
                            break;
                        }
                    }
                    if ok && mag[k] > 0.0 {
                        peaks.push(k);
                    }
                }
                let pp = &prev_pha[ch];
                let adv = |k: usize| -> f32 {
                    let omega = 2.0 * PI * k as f32 / l as f32;
                    let f = if dc != 0.0 { omega + wrap(pha[k] - pp[k] - omega * dc) / dc } else { omega };
                    f * hop as f32
                };
                if peaks.is_empty() {
                    for k in 0..kb {
                        s[k] = wrap(s[k] + adv(k));
                    }
                } else {
                    let mut new = vec![0f32; kb];
                    for (pi, &pk) in peaks.iter().enumerate() {
                        let lo = if pi == 0 { 0 } else { (peaks[pi - 1] + pk) / 2 + 1 };
                        let hi = if pi + 1 == peaks.len() { kb - 1 } else { (pk + peaks[pi + 1]) / 2 };
                        let sp = wrap(s[pk] + adv(pk));
                        let rot = sp - pha[pk];
                        for k in lo..=hi {
                            new[k] = wrap(pha[k] + rot);
                        }
                    }
                    s.copy_from_slice(&new);
                }
            }
            prev_pha[ch].copy_from_slice(&pha);
            for k in 0..kb {
                spec[k] = Complex32::from_polar(mag[k], s[k]);
            }
            fft.inverse(&mut spec, &mut obuf);
            add_frame(&mut out[ch], y, &obuf, &w.w, 1.0 / l as f32);
        }
        add_weight(&mut norm, y, &w2);
        prev_c = c;
        first = false;
    }
    for ch in out.iter_mut() {
        for (v, &nv) in ch.iter_mut().zip(norm.iter()) {
            *v = if nv > 1e-6 { *v / nv } else { 0.0 };
        }
    }
    out
}

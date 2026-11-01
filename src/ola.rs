//! Overlap-add TSM for the percussive part (as in Driedger, Mueller, Ewert 2014), with grain
//! placement driven by a [`TimeMap`] so that attacks land exactly on round(R*t0).

use crate::timemap::TimeMap;
use crate::window::{Win, add_weight};

pub fn stretch_ola(x: &[Vec<f32>], map: &TimeMap, n_out: usize, win: usize, hop: usize) -> Vec<Vec<f32>> {
    let nch = x.len();
    let n_in = x[0].len() as i64;
    let w = Win::hann(win);
    let h = (win / 2) as i64;
    let hop = hop as i64;
    let mut out = vec![vec![0f32; n_out]; nch];
    let mut norm = vec![0f32; n_out];
    let n_lo = -(win as i64 / hop) - 1;
    let n_hi = (n_out as i64 + win as i64) / hop + 1;
    for n in n_lo..=n_hi {
        let y = n * hop;
        let c = map.eval(y as f64).round() as i64;
        let u_lo = (-h).max(-y);
        let u_hi = h.min(n_out as i64 - y);
        if u_lo >= u_hi {
            continue;
        }
        for (ch, xc) in x.iter().enumerate() {
            let o = &mut out[ch];
            for u in u_lo..u_hi {
                let t = c + u;
                if t >= 0 && t < n_in {
                    o[(y + u) as usize] += xc[t as usize] * w.w[(u + h) as usize];
                }
            }
        }
        add_weight(&mut norm, y, &w.w);
    }
    for ch in out.iter_mut() {
        for (v, &nv) in ch.iter_mut().zip(norm.iter()) {
            *v = if nv > 1e-6 { *v / nv } else { 0.0 };
        }
    }
    out
}

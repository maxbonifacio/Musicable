//! Overlap-add / WSOLA time-scale modification for the percussive part.
//!
//! Plain OLA is what Driedger, Mueller and Ewert (2014) use for the percussive part. With
//! `tol > 0` this becomes WSOLA (Verhelst & Roelands 1993): every grain's analysis position
//! may move by up to +-tol samples from the time map so that it continues the previous grain's
//! waveform (maximum normalised cross-correlation with the "natural continuation" of the
//! previous grain). Low-frequency drum bodies (kick, toms) otherwise get comb-filtered by grain
//! repetition (measured -8..-14 dB around 100-160 Hz at R = 2.5 with plain OLA).
//!
//! Attack placement: the [`TimeMap`] has slope 1 around every onset, so all grains overlapping
//! an attack read it from the same relative position and it lands exactly on round(R*t0). The
//! WSOLA search is disabled inside those cores and is never allowed to pull an onset into (or
//! out of) a grain window, so it cannot create doubled attacks. Grain positions are shared by
//! all channels (search on the channel sum) to keep the stereo image.

use crate::fft::Fft;
use crate::timemap::TimeMap;
use crate::window::{Win, add_weight};
use realfft::num_complex::Complex32;

/// Minimum normalised cross-correlation for a WSOLA shift to be taken.
const WSOLA_MIN_CORR: f64 = 0.6;

pub struct OlaParams {
    pub win: usize,
    pub hop: usize,
    /// WSOLA search tolerance in samples (0 = plain OLA).
    pub tol: usize,
}

pub fn stretch_ola(x: &[Vec<f32>], map: &TimeMap, n_out: usize, p: &OlaParams, onsets: &[i64], core: i64) -> Vec<Vec<f32>> {
    let nch = x.len();
    let n_in = x[0].len() as i64;
    let win = p.win;
    let w = Win::hann(win);
    let h = (win / 2) as i64;
    let hop = p.hop as i64;
    let tol = if (map.eval(1e6) - map.eval(0.0) - 1e6).abs() < 1e-3 { 0 } else { p.tol as i64 };
    // energy prefix sums over all channels (the search correlates the stacked channels, so
    // side content counts as much as centre content and anti-phase parts can't cancel)
    let mut prefix = vec![0f64; n_in as usize + 1];
    for t in 0..n_in as usize {
        prefix[t + 1] = prefix[t] + x.iter().map(|c| (c[t] as f64) * (c[t] as f64)).sum::<f64>();
    }
    let seg_energy = |a: i64, b: i64| -> f64 {
        let a = a.clamp(0, n_in) as usize;
        let b = b.clamp(0, n_in) as usize;
        prefix[b] - prefix[a]
    };
    let at = |ch: usize, t: i64| -> f32 { if t >= 0 && t < n_in { x[ch][t as usize] } else { 0.0 } };
    // FFT cross-correlation buffers
    let nfft = (2 * win + 2 * p.tol).next_power_of_two();
    let mut fft = Fft::new(nfft);
    let kb = nfft / 2 + 1;
    let mut ta = vec![0f32; nfft];
    let mut sa = vec![0f32; nfft];
    let mut tf = vec![Complex32::new(0.0, 0.0); kb];
    let mut sf = vec![Complex32::new(0.0, 0.0); kb];
    let mut cc = vec![0f32; nfft];
    let mut cc_sum = vec![0f32; nfft];
    // onset y positions for core detection
    let on_y: Vec<i64> = onsets.iter().map(|&t| map_y(map, t)).collect();

    let mut out = vec![vec![0f32; n_out]; nch];
    let mut norm = vec![0f32; n_out];
    let n_lo = -(win as i64 / hop) - 1;
    let n_hi = (n_out as i64 + win as i64) / hop + 1;
    let mut prev_c: Option<i64> = None;
    for n in n_lo..=n_hi {
        let y = n * hop;
        let c_nom = map.eval(y as f64).round() as i64;
        let mut c = c_nom;
        let in_core = {
            let i = on_y.partition_point(|&oy| oy < y - core);
            i < on_y.len() && on_y[i] <= y + core
        };
        if tol > 0 && !in_core
            && let Some(pc) = prev_c {
                // allowed shift range: never move an onset across the grain boundary
                let mut lo = -tol;
                let mut hi = tol;
                let i0 = onsets.partition_point(|&t| t < c_nom - h - tol);
                let mut i = i0;
                while i < onsets.len() && onsets[i] <= c_nom + h + tol {
                    let t0 = onsets[i];
                    if c_nom + h <= t0 {
                        hi = hi.min(t0 - h - c_nom);
                    } else if c_nom - h >= t0 {
                        lo = lo.max(t0 + h - c_nom);
                    } else {
                        lo = 0;
                        hi = 0;
                    }
                    i += 1;
                }
                if hi > lo {
                    // template: natural continuation of the previous grain
                    let nat = pc + hop;
                    let base = c_nom + lo - h;
                    let span = (hi - lo) as usize + win;
                    let mut te = 0f64;
                    cc_sum.iter_mut().for_each(|v| *v = 0.0);
                    for ch in 0..nch {
                        ta.iter_mut().for_each(|v| *v = 0.0);
                        sa.iter_mut().for_each(|v| *v = 0.0);
                        // rectangular template, so a shift of 0 scores exactly 1 whenever the
                        // time map already continues the previous grain (R = 1, onset cores)
                        for u in 0..win {
                            ta[u] = at(ch, nat - h + u as i64);
                        }
                        for v in 0..span {
                            sa[v] = at(ch, base + v as i64);
                        }
                        te += ta.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>();
                        fft.forward(&mut ta, &mut tf);
                        fft.forward(&mut sa, &mut sf);
                        for k in 0..kb {
                            sf[k] *= tf[k].conj();
                        }
                        fft.inverse(&mut sf, &mut cc);
                        for (a, b) in cc_sum.iter_mut().zip(cc.iter()) {
                            *a += *b;
                        }
                    }
                    if te > 1e-12 {
                        let mut best = f64::NEG_INFINITY;
                        let mut best_d = 0i64;
                        let e_nom = seg_energy(c_nom - h, c_nom + h);
                        for d in lo..=hi {
                            let lag = (d - lo) as usize;
                            let e = seg_energy(c_nom + d - h, c_nom + d + h);
                            // stay within +-1 dB of the energy the map asks for: in a decay a
                            // big shift would re-read louder material (envelope bump) or keep
                            // skipping ahead to quieter material (decay too fast)
                            if e > 1.26 * e_nom + 1e-12 || e < 0.79 * e_nom {
                                continue;
                            }
                            let v = cc_sum[lag] as f64 / ((e * te).sqrt() + 1e-9);
                            // prefer the smallest shift among (near) equal candidates: on a
                            // periodic decay all period-spaced lags score alike, and a big
                            // backward jump would re-read a louder part (envelope bump)
                            let v = v - 0.1 * (d.abs() as f64 / tol as f64);
                            if v > best {
                                best = v;
                                best_d = d;
                            }
                        }
                        // only follow the waveform when there is a waveform to follow: on
                        // noise the best lag is arbitrary and would just jitter the envelope
                        if best > WSOLA_MIN_CORR {
                            c = c_nom + best_d;
                        }
                    }
                }
            }
        prev_c = Some(c);
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

/// Output position of input sample t under `map` (inverse by bisection; maps are monotone).
fn map_y(map: &TimeMap, t: i64) -> i64 {
    let (mut lo, mut hi) = (-1e12f64, 1e12f64);
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if map.eval(mid) < t as f64 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi.round() as i64
}

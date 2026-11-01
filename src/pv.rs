//! Phase vocoder with phase-gradient heap integration (PGHI).
//!
//! References: Prusa, Balazs, Sondergaard 2017 (PGHI); Prusa & Holighaus 2017/2022
//! ("Phase vocoder done right", PVDR); Auger & Flandrin 1995 (reassignment operators used
//! to obtain the phase gradient from the analysis STFT); Roebel 2003 / Duxbury et al. 2002
//! (phase reset at onsets).
//!
//! Formulation used here. Let X be the analysis STFT (frame-centred phase convention, see
//! `window.rs`) taken at input positions c_n, and let the synthesis frames sit at output
//! positions y_n. We write the synthesis phase as
//!
//!     phi_out(n,k) = arg X(n,k) + psi(n,k)
//!
//! and integrate the *correction field* psi with the PGHI heap instead of the absolute phase.
//! The target gradient of phi_out is the PVDR one: time direction 2*pi*f*dy (instantaneous
//! frequency f kept), frequency direction -2*pi*R*t_off/M (the time offset t_off of the
//! energy inside the frame is scaled by the stretch, which is PVDR's b_s = alpha*b_a). The
//! analysis phase already carries 2*pi*f*dc and -2*pi*t_off/M exactly (mod 2pi), so
//!
//!     d psi / dn = 2*pi * f * (dy - dc)            (trapezoid between frames)
//!     d psi / dk = -2*pi * (R - 1) * t_off / M     (trapezoid between bins)
//!
//! For an impulse (f = k/M) and for a linear chirp this field is curl-free, so the heap order
//! does not matter for them; at R = 1 psi is constant and the input is reconstructed exactly.
//! Because every channel uses the same psi, inter-channel phase and level differences of the
//! analysis frame are kept exactly: Y_ch = X_ch * exp(i psi). f and t_off are taken from the
//! energy-weighted channel sum, computed by reassignment (derivative and time-weighted windows)
//! rather than from finite phase differences.

use crate::fft::Fft;
use crate::onsets::Onset;
use crate::util::{scaled_pow2, wrap};
use crate::window::{Win, add_frame, add_weight, load_frame};
use realfft::num_complex::Complex32;
use std::f32::consts::PI;

#[derive(Clone, Debug)]
pub struct PvParams {
    /// Window length L (Hann).
    pub win: usize,
    /// FFT length M (>= 2L recommended so scaled time offsets don't wrap).
    pub fft: usize,
    /// Synthesis hop (<= L/8).
    pub syn_hop: usize,
    /// Relative magnitude tolerance below which bins are not integrated (PVDR uses 1e-6).
    pub tol: f32,
    /// Frames per heap block (whole-plane heap within a block, seeded by the previous block).
    pub block: usize,
    /// Scale the frequency-direction phase derivative by R (PVDR) for impulse-like bins.
    /// Off = plain PGHI gradients (time offsets kept as analysed).
    pub scale_toff: bool,
    /// Onset phase resets.
    pub reset: bool,
    /// Energy rise (post/pre) a bin needs at an onset to be reset.
    pub reset_ratio: f32,
    pub min_strength: f32,
}

impl PvParams {
    pub fn new(sr: u32) -> Self {
        let win = scaled_pow2(4096, sr);
        PvParams {
            win,
            fft: 2 * win,
            syn_hop: win / 8,
            tol: 1e-6,
            block: 64,
            scale_toff: true,
            reset: true,
            reset_ratio: 2.0,
            min_strength: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    y: i64,
    c: i64,
    onset: bool,
}

/// Synthesis frame grid: uniform hop, plus one frame exactly at round(R*t0) for every onset
/// (analysed exactly at t0), with grid frames closer than hop/2 to an onset frame removed.
fn schedule(n_out: usize, r: f64, onsets: &[i64], hop: i64, l: i64) -> Vec<Frame> {
    let mut on: Vec<(i64, i64)> = Vec::new();
    for &t in onsets {
        let y = (r * t as f64).round() as i64;
        if let Some(&(py, _)) = on.last() {
            if y - py < hop / 2 {
                continue;
            }
        }
        on.push((y, t));
    }
    let mut frames = Vec::new();
    let n_lo = -(l / hop) - 1;
    let n_hi = (n_out as i64 + l) / hop + 1;
    let mut oi = 0;
    for n in n_lo..=n_hi {
        let y = n * hop;
        while oi < on.len() && on[oi].0 <= y {
            frames.push(Frame { y: on[oi].0, c: on[oi].1, onset: true });
            oi += 1;
        }
        let near = |o: Option<&(i64, i64)>| o.map(|&(oy, _)| (oy - y).abs() * 2 < hop).unwrap_or(false);
        if near(on.get(oi)) || (oi > 0 && near(on.get(oi - 1))) {
            continue;
        }
        frames.push(Frame { y, c: (y as f64 / r).round() as i64, onset: false });
    }
    while oi < on.len() {
        frames.push(Frame { y: on[oi].0, c: on[oi].1, onset: true });
        oi += 1;
    }
    frames
}

struct Analyzer {
    fft: Fft,
    win: Win,
    buf: Vec<f32>,
    sd: Vec<Complex32>,
    st: Vec<Complex32>,
}

impl Analyzer {
    fn new(l: usize, m: usize) -> Self {
        let fft = Fft::new(m);
        let k = fft.bins();
        Analyzer {
            fft,
            win: Win::hann(l),
            buf: vec![0.0; m],
            sd: vec![Complex32::new(0.0, 0.0); k],
            st: vec![Complex32::new(0.0, 0.0); k],
        }
    }
    fn spectrum(&mut self, x: &[f32], c: i64, out: &mut [Complex32]) {
        load_frame(x, c, &self.win.w, &mut self.buf);
        self.fft.forward(&mut self.buf, out);
    }
    /// Spectrum into `sw`; accumulate |X|^2, Im(Xd X*), Re(Xt X*).
    fn reassign(&mut self, x: &[f32], c: i64, sw: &mut [Complex32], e: &mut [f32], nf: &mut [f32], nt: &mut [f32]) {
        load_frame(x, c, &self.win.w, &mut self.buf);
        self.fft.forward(&mut self.buf, sw);
        load_frame(x, c, &self.win.dw, &mut self.buf);
        self.fft.forward(&mut self.buf, &mut self.sd);
        load_frame(x, c, &self.win.tw, &mut self.buf);
        self.fft.forward(&mut self.buf, &mut self.st);
        for k in 0..sw.len() {
            let a = sw[k];
            let d = self.sd[k] * a.conj();
            let t = self.st[k] * a.conj();
            e[k] += a.norm_sqr();
            nf[k] += d.im;
            nt[k] += t.re;
        }
    }
}

/// Max-priority bucket queue on quantised log magnitude (0.25 dB buckets, 140 dB range).
/// PGHI only needs "roughly the largest first"; this makes push/pop O(1).
struct BucketQueue {
    buckets: Vec<Vec<u32>>,
    top: usize,
}

const NB: usize = 561;

impl BucketQueue {
    fn new() -> Self {
        BucketQueue { buckets: vec![Vec::new(); NB], top: 0 }
    }
    #[inline]
    fn push(&mut self, idx: u32, mag: f32, inv_max: f32) {
        let r = mag * inv_max;
        let db = if r > 0.0 { 8.685_889_6 * r.ln() } else { -140.0 };
        let b = ((db + 140.0) * 4.0).clamp(0.0, (NB - 1) as f32) as usize;
        self.buckets[b].push(idx);
        if b > self.top {
            self.top = b;
        }
    }
    #[inline]
    fn pop(&mut self) -> Option<u32> {
        loop {
            if let Some(v) = self.buckets[self.top].pop() {
                return Some(v);
            }
            if self.top == 0 {
                return None;
            }
            self.top -= 1;
        }
    }
}

/// Per-bin time-offset scale. Bins whose reassigned time offsets agree with their neighbours
/// (k-8..k+8 of the 2x zero-padded FFT, energy weighted) belong to an impulse-like event and get s = R, so the event is
/// placed at R*t_off in every frame (PVDR's b_s = alpha*b_a, which makes the gradient field
/// curl-free for impulses and chirps). Bins with scattered offsets are noise; scaling them
/// would push their energy outside the synthesis window (about -2 dB on noise at R = 4), so
/// they keep s = 1. In between s blends with c = exp(-var/(2 sigma^2)), sigma = L/32.
const IMP_RADIUS: usize = 8;

fn impulse_scale(mag: &[f32], toff: &[f32], sc: &mut [f32], r: f32, l: f32) {
    let kb = mag.len();
    let sig2 = 2.0 * (l / 32.0) * (l / 32.0);
    for k in 0..kb {
        let lo = k.saturating_sub(IMP_RADIUS);
        let hi = (k + IMP_RADIUS).min(kb - 1);
        let (mut w, mut m1, mut m2) = (0f32, 0f32, 0f32);
        for j in lo..=hi {
            let e = mag[j] * mag[j];
            w += e;
            m1 += e * toff[j];
            m2 += e * toff[j] * toff[j];
        }
        let c = if w > 0.0 {
            let mean = m1 / w;
            let var = (m2 / w - mean * mean).max(0.0);
            (-var / sig2).exp()
        } else {
            0.0
        };
        sc[k] = 1.0 + (r - 1.0) * c;
    }
}

/// Amplitude correction for a component analysed at offset t_off (window weight w(t_off)) and
/// placed at s*t_off: it should carry w(s*t_off) so the coherent overlap-add normalisation
/// (sum w_a w_s) stays exact. Zero outside the window, which also prevents circular wrap.
#[inline]
fn place_gain(toff: f32, s: f32, l: f32) -> f32 {
    let hann = |u: f32| if u.abs() >= 0.5 * l { 0.0 } else { 0.5 + 0.5 * (2.0 * PI * u / l).cos() };
    let wa = hann(toff);
    if wa < 1e-3 {
        return if (s * toff).abs() < 0.5 * l { 1.0 } else { 0.0 };
    }
    (hann(s * toff) / wa).min(4.0)
}

pub fn stretch_pv(x: &[Vec<f32>], r: f64, n_out: usize, onsets: &[Onset], p: &PvParams) -> Vec<Vec<f32>> {
    let nch = x.len();
    let n_in = x[0].len() as i64;
    let l = p.win;
    let m = p.fft;
    let kb = m / 2 + 1;
    let hop = p.syn_hop as i64;
    let on: Vec<i64> = if p.reset {
        onsets
            .iter()
            .filter(|o| o.strength >= p.min_strength && (o.frame as i64) < n_in)
            .map(|o| o.frame as i64)
            .collect()
    } else {
        Vec::new()
    };
    let frames = schedule(n_out, r, &on, hop, l as i64);
    let nf = frames.len();

    let mut an = Analyzer::new(l, m);
    let w2: Vec<f32> = an.win.w.iter().map(|v| v * v).collect();
    let mut out = vec![vec![0f32; n_out]; nch];
    let mut norm = vec![0f32; n_out];

    let cap = p.block + 1;
    let mut spec = vec![vec![Complex32::new(0.0, 0.0); cap * kb]; nch];
    let mut mag = vec![0f32; cap * kb];
    let mut fi = vec![0f32; cap * kb];
    let mut toff = vec![0f32; cap * kb];
    let mut psi = vec![0f32; cap * kb];
    let mut sc = vec![1f32; cap * kb]; // per-bin time-offset scale s (1 = unscaled, R = PVDR)
    let mut state = vec![0u8; cap * kb]; // 0 = free, 1 = assigned, 2 = below tolerance
    let mut ys = vec![0i64; cap];
    let mut cs = vec![0i64; cap];
    let mut dyc = vec![0f32; cap];
    let mut have_carry = false;

    let mut nfacc = vec![0f32; kb];
    let mut ntacc = vec![0f32; kb];
    let mut tmp = vec![Complex32::new(0.0, 0.0); kb];
    let mut e_pre = vec![0f32; kb];
    let mut e_post = vec![0f32; kb];
    let mut ybuf = vec![Complex32::new(0.0, 0.0); kb];
    let mut obuf = vec![0f32; m];
    let mut queue = BucketQueue::new();

    let rv = r as f32;
    let vfac = -PI / m as f32;
    let inv_m = 1.0 / m as f32;

    let mut b0 = 0;
    while b0 < nf {
        let b1 = (b0 + p.block).min(nf);
        let nb = 1 + b1 - b0;
        if !have_carry {
            ys[0] = frames[b0].y - hop;
            cs[0] = frames[b0].c - hop;
            mag[..kb].iter_mut().for_each(|v| *v = 0.0);
        }
        // --- analysis
        for n in 1..nb {
            let fr = frames[b0 + n - 1];
            ys[n] = fr.y;
            cs[n] = fr.c;
            let row = n * kb..(n + 1) * kb;
            let e = &mut mag[row.clone()];
            e.iter_mut().for_each(|v| *v = 0.0);
            nfacc.iter_mut().for_each(|v| *v = 0.0);
            ntacc.iter_mut().for_each(|v| *v = 0.0);
            for ch in 0..nch {
                an.reassign(&x[ch], fr.c, &mut spec[ch][row.clone()], e, &mut nfacc, &mut ntacc);
            }
            for k in 0..kb {
                let i = n * kb + k;
                let en = mag[i];
                let fb = k as f32 * inv_m;
                if en > 1e-30 {
                    fi[i] = (fb - nfacc[k] / (2.0 * PI * en)).clamp(0.0, 0.5);
                    toff[i] = (ntacc[k] / en).clamp(-(l as f32) / 2.0, l as f32 / 2.0);
                } else {
                    fi[i] = fb;
                    toff[i] = 0.0;
                }
                mag[i] = en.sqrt();
                psi[i] = 0.0;
            }
            if p.scale_toff && rv != 1.0 {
                impulse_scale(&mag[row.clone()], &toff[row.clone()], &mut sc[row.clone()], rv, l as f32);
            } else {
                sc[row.clone()].iter_mut().for_each(|v| *v = 1.0);
            }
            dyc[n] = ((ys[n] - ys[n - 1]) - (cs[n] - cs[n - 1])) as f32;
        }
        let maxmag = mag[..nb * kb].iter().fold(0f32, |a, &b| a.max(b));
        let abstol = p.tol * maxmag;
        let inv_max = if maxmag > 0.0 { 1.0 / maxmag } else { 0.0 };
        // --- states and seeds
        for k in 0..kb {
            if have_carry && mag[k] > abstol {
                state[k] = 1;
                queue.push(k as u32, mag[k], inv_max);
            } else {
                state[k] = 2;
            }
        }
        for i in kb..nb * kb {
            state[i] = if mag[i] > abstol { 0 } else { 2 };
        }
        for n in 1..nb {
            let fr = frames[b0 + n - 1];
            if !fr.onset {
                continue;
            }
            e_pre.iter_mut().for_each(|v| *v = 0.0);
            e_post.iter_mut().for_each(|v| *v = 0.0);
            for xc in x.iter() {
                an.spectrum(xc, fr.c - (l as i64) / 2, &mut tmp);
                for k in 0..kb {
                    e_pre[k] += tmp[k].norm_sqr();
                }
                an.spectrum(xc, fr.c + (l as i64) / 4, &mut tmp);
                for k in 0..kb {
                    e_post[k] += tmp[k].norm_sqr();
                }
            }
            for k in 0..kb {
                let lo = k.saturating_sub(1);
                let hi = (k + 1).min(kb - 1);
                let (mut a, mut b) = (0f32, 0f32);
                for kk in lo..=hi {
                    a += e_pre[kk];
                    b += e_post[kk];
                }
                let i = n * kb + k;
                if state[i] == 0 && b > p.reset_ratio * a {
                    state[i] = 1;
                    psi[i] = 0.0;
                    queue.push(i as u32, mag[i], inv_max);
                }
            }
        }
        // --- heap integration
        let mut order: Option<Vec<u32>> = None;
        let mut opos = 0usize;
        loop {
            let idx = match queue.pop() {
                Some(v) => v as usize,
                None => {
                    // start a new island at the largest free coefficient, with the analysis phase
                    let ord = order.get_or_insert_with(|| {
                        let mut v: Vec<u32> = (kb..nb * kb).filter(|&i| state[i] == 0).map(|i| i as u32).collect();
                        v.sort_unstable_by(|&a, &b| mag[b as usize].total_cmp(&mag[a as usize]));
                        v
                    });
                    while opos < ord.len() && state[ord[opos] as usize] != 0 {
                        opos += 1;
                    }
                    if opos >= ord.len() {
                        break;
                    }
                    let i = ord[opos] as usize;
                    state[i] = 1;
                    psi[i] = 0.0;
                    queue.push(i as u32, mag[i], inv_max);
                    continue;
                }
            };
            let n = idx / kb;
            let k = idx % kb;
            let ph = psi[idx];
            if n + 1 < nb {
                let j = idx + kb;
                if state[j] == 0 {
                    psi[j] = wrap(ph + PI * (fi[idx] + fi[j]) * dyc[n + 1]);
                    state[j] = 1;
                    queue.push(j as u32, mag[j], inv_max);
                }
            }
            if n >= 2 {
                let j = idx - kb;
                if state[j] == 0 {
                    psi[j] = wrap(ph - PI * (fi[idx] + fi[j]) * dyc[n]);
                    state[j] = 1;
                    queue.push(j as u32, mag[j], inv_max);
                }
            }
            if n >= 1 {
                if k + 1 < kb {
                    let j = idx + 1;
                    if state[j] == 0 {
                        psi[j] = wrap(ph + vfac * ((sc[idx] - 1.0) * toff[idx] + (sc[j] - 1.0) * toff[j]));
                        state[j] = 1;
                        queue.push(j as u32, mag[j], inv_max);
                    }
                }
                if k >= 1 {
                    let j = idx - 1;
                    if state[j] == 0 {
                        psi[j] = wrap(ph - vfac * ((sc[idx] - 1.0) * toff[idx] + (sc[j] - 1.0) * toff[j]));
                        state[j] = 1;
                        queue.push(j as u32, mag[j], inv_max);
                    }
                }
            }
        }
        // --- synthesis
        for n in 1..nb {
            for ch in 0..nch {
                let s = &spec[ch][n * kb..(n + 1) * kb];
                for k in 0..kb {
                    let i = n * kb + k;
                    let g = if sc[i] != 1.0 { place_gain(toff[i], sc[i], l as f32) } else { 1.0 };
                    let (sn, cn) = psi[i].sin_cos();
                    ybuf[k] = s[k] * Complex32::new(cn * g, sn * g);
                }
                an.fft.inverse(&mut ybuf, &mut obuf);
                add_frame(&mut out[ch], ys[n], &obuf, &an.win.w, inv_m);
            }
            add_weight(&mut norm, ys[n], &w2);
        }
        // --- carry the last frame
        let last = nb - 1;
        if last > 0 {
            let src = last * kb..(last + 1) * kb;
            mag.copy_within(src.clone(), 0);
            fi.copy_within(src.clone(), 0);
            toff.copy_within(src.clone(), 0);
            psi.copy_within(src.clone(), 0);
            sc.copy_within(src, 0);
            ys[0] = ys[last];
            cs[0] = cs[last];
            have_carry = true;
        }
        b0 = b1;
    }
    for ch in out.iter_mut() {
        for (v, &nv) in ch.iter_mut().zip(norm.iter()) {
            *v = if nv > 1e-6 { *v / nv } else { 0.0 };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reassignment_gives_frequency_and_offset() {
        let l = 1024;
        let m = 2048;
        let mut an = Analyzer::new(l, m);
        let kb = m / 2 + 1;
        // sinusoid
        let f0 = 0.0371f32;
        let x: Vec<f32> = (0..8000).map(|t| (2.0 * PI * f0 * t as f32 + 0.3).cos()).collect();
        let mut sw = vec![Complex32::new(0.0, 0.0); kb];
        let (mut e, mut nf, mut nt) = (vec![0f32; kb], vec![0f32; kb], vec![0f32; kb]);
        an.reassign(&x, 4000, &mut sw, &mut e, &mut nf, &mut nt);
        let kp = (0..kb).max_by(|&a, &b| e[a].total_cmp(&e[b])).unwrap();
        for k in kp - 2..=kp + 2 {
            let f = k as f32 / m as f32 - nf[k] / (2.0 * PI * e[k]);
            assert!((f - f0).abs() < 2e-4, "k={k} f={f}");
        }
        // impulse at offset +100
        let mut x = vec![0f32; 8000];
        x[4100] = 1.0;
        let (mut e, mut nf, mut nt) = (vec![0f32; kb], vec![0f32; kb], vec![0f32; kb]);
        an.reassign(&x, 4000, &mut sw, &mut e, &mut nf, &mut nt);
        for k in [3usize, 100, 500] {
            assert!((nt[k] / e[k] - 100.0).abs() < 1e-2);
            let f = k as f32 / m as f32 - nf[k] / (2.0 * PI * e[k]);
            assert!((f - k as f32 / m as f32).abs() < 1e-5);
        }
    }
}

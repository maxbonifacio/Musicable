//! Harmonic / percussive / residual separation by median filtering of STFT magnitudes
//! (Fitzgerald 2010; Driedger, Mueller, Disch 2014 for the separation factor and residual).
//!
//! The separator is behind the [`Separator`] trait so a learned separator can replace it: it
//! only has to return three time-domain signals that sum to the input.

use crate::fft::Fft;
use crate::util::scaled_pow2;
use crate::window::{Win, add_frame, add_weight, load_frame};
use realfft::num_complex::Complex32;

/// Three time-domain parts; harmonic + percussive + residual == input (to float precision).
#[derive(Clone, Debug)]
pub struct Parts {
    pub harmonic: Vec<Vec<f32>>,
    pub percussive: Vec<Vec<f32>>,
    pub residual: Vec<Vec<f32>>,
    /// Optional versions of the three parts to use when stretching (R != 1). With soft masks
    /// the parts are coherent fractions of the same STFT coefficient; once they go through
    /// different stretchers (PV, OLA, random-phase noise) they add up incoherently and the
    /// cross terms are lost (white noise came out ~4 dB too quiet). Here the masks are
    /// normalised to sum(M_i^2) = 1 instead of sum(M_i) = 1, so the incoherent sum carries
    /// the input power while keeping each bin's proportions (a partial with M_H = 0.99 stays
    /// a partial, it doesn't spawn a -17 dB noise halo as an additive fix would). `None`:
    /// stretch the plain parts.
    pub stretch: Option<Box<Parts>>,
}

pub trait Separator {
    fn split(&self, x: &[Vec<f32>], sr: u32) -> Parts;
}

#[derive(Clone, Debug)]
pub struct MedianSplit {
    /// STFT window (Hann) length.
    pub win: usize,
    pub hop: usize,
    /// Length of the harmonic (time-direction) median filter in frames (odd).
    pub harm_frames: usize,
    /// Length of the percussive (frequency-direction) median filter in bins (odd).
    pub perc_bins: usize,
    /// Separation factor beta >= 1 (Driedger et al. 2014). beta = 1 gives a 2-way split.
    pub beta: f32,
    /// Exponent of the soft (Wiener-like) masks; ignored when `binary`.
    pub power: f32,
    pub binary: bool,
    /// One mask shared by all channels (computed on the channel energy sum) so a source can't
    /// be routed differently in L and R. `false` reproduces per-channel processing.
    pub shared_masks: bool,
    /// Give the main-lobe skirts of strong harmonic peaks the peak's harmonic share, so a
    /// partial is not torn between the PV and the random-phase path (see `protect_main_lobes`).
    pub lobe_protect: bool,
    /// Also return energy-normalised parts for stretching (see [`Parts::stretch`]).
    pub energy_parts: bool,
}

/// Main-lobe protection. The median filters judge every bin on its own, so the skirts of a
/// strong partial (which sit next to a quieter, noisier neighbourhood) often get a low
/// harmonic mask and leak into the residual, where random phases would turn them into a
/// noise halo around the partial. For each local maximum kp with M_H >= 0.7 we bound how
/// much of a neighbour's magnitude the peak's own Hann main lobe can explain (worst-case
/// off-bin envelope 1, 0.2, 0.03 at 1, 2, 3 bins) and raise that bin's harmonic mask to the
/// explained fraction times M_H(kp). The other two masks are scaled down so the three still
/// sum to one.
pub fn protect_main_lobes(mag: &[f32], mh: &mut [f32], mp: &mut [f32], tmp: &mut [f32]) {
    const ENV: [f32; 4] = [1.0, 1.0, 0.2, 0.03];
    let kb = mag.len();
    tmp.copy_from_slice(mh);
    for kp in 1..kb - 1 {
        if !(mag[kp] >= mag[kp - 1] && mag[kp] > mag[kp + 1]) || mh[kp] < 0.7 {
            continue;
        }
        for d in 1..=3usize {
            for k in [kp.wrapping_sub(d), kp + d] {
                if k >= kb || mag[k] <= 0.0 {
                    continue;
                }
                let share = (mag[kp] * ENV[d] / mag[k]).min(1.0) * mh[kp];
                if share > tmp[k] {
                    tmp[k] = share;
                }
            }
        }
    }
    for k in 0..kb {
        if tmp[k] > mh[k] {
            let old = 1.0 - mh[k];
            let new = 1.0 - tmp[k];
            mp[k] = if old > 1e-9 { mp[k] * new / old } else { 0.0 };
            mh[k] = tmp[k];
        }
    }
}

impl MedianSplit {
    /// Settings for the `full` engine: soft 3-way masks shared by all channels.
    pub fn full(sr: u32) -> Self {
        let win = scaled_pow2(2048, sr);
        let hop = win / 4;
        // ~200 ms of frames, ~500 Hz of bins
        let frame_s = hop as f64 / sr as f64;
        let mut hf = (0.2 / frame_s).round() as usize | 1;
        hf = hf.max(3);
        let bin_hz = sr as f64 / win as f64;
        let pb = ((500.0 / bin_hz).round() as usize | 1).max(3);
        MedianSplit {
            win,
            hop,
            harm_frames: hf,
            perc_bins: pb,
            beta: 2.0,
            power: 2.0,
            binary: false,
            shared_masks: true,
            lobe_protect: true,
            energy_parts: true,
        }
    }
    /// Classic HPTSM settings (Driedger, Mueller, Ewert 2014 / libtsm defaults scaled from
    /// 22.05 kHz): binary 2-way masks, 10-frame and 10-bin median filters, per channel.
    pub fn hptsm(sr: u32) -> Self {
        let win = scaled_pow2(2048, sr);
        MedianSplit {
            win,
            hop: win / 4,
            harm_frames: 11,
            perc_bins: 11,
            beta: 1.0,
            power: 2.0,
            binary: true,
            shared_masks: false,
            lobe_protect: false,
            energy_parts: false,
        }
    }

    /// Masks for one bin given harmonic- and percussive-enhanced magnitudes.
    #[inline]
    pub fn masks(&self, h: f32, p: f32) -> (f32, f32) {
        let b = self.beta;
        if self.binary {
            if b <= 1.0 {
                let mh = if h >= p { 1.0 } else { 0.0 };
                return (mh, 1.0 - mh);
            }
            let mh = if h > b * p { 1.0 } else { 0.0 };
            let mp = if p >= b * h && mh == 0.0 { 1.0 } else { 0.0 };
            return (mh, mp);
        }
        let hp = h.powf(self.power);
        let pp = p.powf(self.power);
        let bp = b.powf(self.power);
        let dh = hp + bp * pp;
        let dp = pp + bp * hp;
        if dh <= 0.0 || dp <= 0.0 {
            return (1.0, 0.0);
        }
        let mh = hp / dh;
        let mp = if b <= 1.0 { 1.0 - mh } else { pp / dp };
        (mh, mp)
    }
}

fn median(buf: &mut [f32]) -> f32 {
    let m = buf.len() / 2;
    let (_, v, _) = buf.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    *v
}

impl Separator for MedianSplit {
    fn split(&self, x: &[Vec<f32>], _sr: u32) -> Parts {
        let nch = x.len();
        let n = x[0].len();
        let l = self.win;
        let hop = self.hop as i64;
        let k_bins = l / 2 + 1;
        let win = Win::hann(l);
        let mut fft = Fft::new(l);
        let j_lo = -((l as i64) / hop) - 1;
        let j_hi = (n as i64 + l as i64) / hop + 1;
        let nfr = (j_hi - j_lo + 1) as usize;
        let nmask = if self.shared_masks { 1 } else { nch };
        // pass 1: magnitudes
        let mut mags = vec![vec![0f32; nfr * k_bins]; nmask];
        let mut buf = vec![0f32; l];
        let mut spec = vec![Complex32::new(0.0, 0.0); k_bins];
        for f in 0..nfr {
            let c = (j_lo + f as i64) * hop;
            for (ch, xc) in x.iter().enumerate() {
                load_frame(xc, c, &win.w, &mut buf);
                fft.forward(&mut buf, &mut spec);
                let mi = if self.shared_masks { 0 } else { ch };
                let row = &mut mags[mi][f * k_bins..(f + 1) * k_bins];
                for k in 0..k_bins {
                    row[k] += spec[k].norm_sqr();
                }
            }
        }
        for m in mags.iter_mut() {
            m.iter_mut().for_each(|v| *v = v.sqrt());
        }
        // pass 2: masks + resynthesis
        let mut harm = vec![vec![0f32; n]; nch];
        let mut perc = vec![vec![0f32; n]; nch];
        let mut norm = vec![0f32; n];
        let w2: Vec<f32> = win.w.iter().map(|v| v * v).collect();
        let hh = (self.harm_frames / 2) as i64;
        let ph = self.perc_bins / 2;
        let mut tbuf = vec![0f32; self.harm_frames];
        let mut fbuf = vec![0f32; self.perc_bins];
        let mut mh = vec![vec![0f32; k_bins]; nmask];
        let mut mp = vec![vec![0f32; k_bins]; nmask];
        let mut sh = vec![Complex32::new(0.0, 0.0); k_bins];
        let mut sp = vec![Complex32::new(0.0, 0.0); k_bins];
        let mut lobe_tmp = vec![0f32; k_bins];
        let three = self.beta > 1.0 && self.energy_parts;
        let ne = if three { n } else { 0 };
        let mut eh = vec![vec![0f32; ne]; nch];
        let mut ep = vec![vec![0f32; ne]; nch];
        let mut er = vec![vec![0f32; ne]; nch];
        let mut obuf = vec![0f32; l];
        for f in 0..nfr {
            let c = (j_lo + f as i64) * hop;
            // skip frames that cannot touch the signal
            if c + (l as i64) / 2 <= 0 || c - (l as i64) / 2 >= n as i64 {
                continue;
            }
            for mi in 0..nmask {
                let m = &mags[mi];
                let row = &m[f * k_bins..(f + 1) * k_bins];
                for k in 0..k_bins {
                    // time median (clamped at the edges by repeating the border frame)
                    for (i, d) in (-hh..=hh).enumerate() {
                        let ff = (f as i64 + d).clamp(0, nfr as i64 - 1) as usize;
                        tbuf[i] = m[ff * k_bins + k];
                    }
                    let hmed = median(&mut tbuf);
                    for (i, kk) in (k as i64 - ph as i64..=k as i64 + ph as i64).enumerate() {
                        // mirror at the spectrum edges
                        let kk = if kk < 0 {
                            -kk
                        } else if kk >= k_bins as i64 {
                            2 * (k_bins as i64 - 1) - kk
                        } else {
                            kk
                        };
                        fbuf[i] = row[kk as usize];
                    }
                    let pmed = median(&mut fbuf);
                    let (a, b) = self.masks(hmed, pmed);
                    mh[mi][k] = a;
                    mp[mi][k] = b;
                }
                if self.lobe_protect && !self.binary {
                    protect_main_lobes(row, &mut mh[mi], &mut mp[mi], &mut lobe_tmp);
                }
            }
            for ch in 0..nch {
                let mi = if self.shared_masks { 0 } else { ch };
                load_frame(&x[ch], c, &win.w, &mut buf);
                fft.forward(&mut buf, &mut spec);
                for k in 0..k_bins {
                    sh[k] = spec[k] * mh[mi][k];
                    sp[k] = spec[k] * mp[mi][k];
                }
                fft.inverse(&mut sh, &mut obuf);
                add_frame(&mut harm[ch], c, &obuf, &win.w, 1.0 / l as f32);
                fft.inverse(&mut sp, &mut obuf);
                add_frame(&mut perc[ch], c, &obuf, &win.w, 1.0 / l as f32);
                if three {
                    for (dst, which) in [(&mut eh[ch], 0), (&mut ep[ch], 1), (&mut er[ch], 2)] {
                        for k in 0..k_bins {
                            let (a, b) = (mh[mi][k], mp[mi][k]);
                            let rr = (1.0 - a - b).max(0.0);
                            let g = 1.0 / (a * a + b * b + rr * rr).max(1e-12).sqrt();
                            let mk = [a, b, rr][which] * g;
                            sh[k] = spec[k] * mk;
                        }
                        fft.inverse(&mut sh, &mut obuf);
                        add_frame(dst, c, &obuf, &win.w, 1.0 / l as f32);
                    }
                }
            }
            add_weight(&mut norm, c, &w2);
        }
        let mut resid = vec![vec![0f32; n]; nch];
        for ch in 0..nch {
            for t in 0..n {
                let g = if norm[t] > 1e-6 { 1.0 / norm[t] } else { 0.0 };
                harm[ch][t] *= g;
                perc[ch][t] *= g;
                if three {
                    eh[ch][t] *= g;
                    ep[ch][t] *= g;
                    er[ch][t] *= g;
                }
                if self.beta <= 1.0 {
                    // 2-way: percussive takes the exact remainder
                    perc[ch][t] = x[ch][t] - harm[ch][t];
                } else {
                    resid[ch][t] = x[ch][t] - harm[ch][t] - perc[ch][t];
                }
            }
        }
        let stretch = three.then(|| Box::new(Parts { harmonic: eh, percussive: ep, residual: er, stretch: None }));
        Parts { harmonic: harm, percussive: perc, residual: resid, stretch }
    }
}

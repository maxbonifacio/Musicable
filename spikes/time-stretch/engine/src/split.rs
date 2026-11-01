//! Harmonic / percussive / residual separation by median filtering of STFT magnitudes
//! (Fitzgerald 2010; Driedger, Mueller, Disch 2014 for the separation factor and residual).
//!
//! The separator is behind the [`Separator`] trait so a learned separator can replace it: it
//! only has to return three time-domain signals that sum to the input.

use crate::fft::Fft;
use crate::util::scaled_pow2;
use crate::window::{Win, add_frame, add_weight, load_frame};
use realfft::num_complex::Complex32;

/// Three time-domain parts.
///
/// Amplitude parts (the default) sum to the input: harmonic + percussive + residual == x.
///
/// Energy parts (`MedianSplit::energy_parts`) are meant for stretching (R != 1). With soft
/// masks the parts are coherent fractions of the same STFT coefficient; once they go through
/// different stretchers (PV, OLA, random-phase noise) they add up incoherently and the cross
/// terms are lost (white noise came out ~4 dB too quiet). Energy parts normalise each bin's
/// masks over the groups that will be stretched *incoherently*, sum over groups of
/// (sum of the group's masks)^2 = 1, so the incoherent sum carries the input power while each
/// bin keeps its proportions (a partial with M_H = 0.99 stays a partial; an additive fix
/// would give it a -17 dB noise halo).
#[derive(Clone, Debug)]
pub struct Parts {
    pub harmonic: Vec<Vec<f32>>,
    pub percussive: Vec<Vec<f32>>,
    pub residual: Vec<Vec<f32>>,
    /// Energy parts only: the harmonic and residual signals for the grouping in which the
    /// percussive part is merged into the harmonic one ({H+P}, {R}; for the 2-way engine
    /// {H+P+R}). [`percussive_attacks_only`] switches to them outside the attack windows.
    /// Merging energy parts by adding them would overshoot: H and P are coherent copies of the
    /// same bins, so |H + P|^2 > |H|^2 + |P|^2 (measured +0.5..+2 dB on a dense mix).
    pub merged_hp: Option<[Vec<Vec<f32>>; 2]>,
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
    /// Return energy-normalised parts for stretching instead of amplitude parts (see
    /// [`Parts`]).
    pub energy_parts: bool,
    /// Below this frequency, tonal peaks hand the residual share of their main lobe to the
    /// harmonic part (see `protect_low_peaks`; 0 = off).
    pub low_peak_hz: f32,
    /// The caller will stretch harmonic + residual together (2-way engine): energy parts are
    /// normalised for the grouping {H+R}, {P} and the residual is returned inside the
    /// harmonic part (the returned residual is silent).
    pub merge_residual: bool,
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

/// Low-frequency tonal peaks. The time median judges a bass note by its stability over
/// ~200 ms, so walking bass lines and short notes often end up in the residual. At 21.5 Hz
/// bin spacing the random-phase noise path then smears them over the neighbouring bands
/// (+3 dB at 126 Hz, -2 dB at 63 Hz on a mix). Below `kmax`, a local maximum that stands
/// 12 dB above the mean of the bins 3 and 4 away on both sides is treated as a sinusoid:
/// for the bins of its main lobe (+-3) the residual share that the peak's Hann lobe can explain
/// moves to the harmonic part (P is untouched, so kick drums stay percussive). For white noise
/// such peaks are rare (Rayleigh tail), so noise is left to the noise path.
pub fn protect_low_peaks(mag: &[f32], mh: &mut [f32], mp: &[f32], kmax: usize) {
    const ENV: [f32; 4] = [1.0, 1.0, 0.2, 0.03];
    for kp in 4..kmax {
        if !(mag[kp] >= mag[kp - 1] && mag[kp] > mag[kp + 1]) {
            continue;
        }
        let side = 0.25 * (mag[kp - 3] + mag[kp - 4] + mag[kp + 3] + mag[kp + 4]);
        if mag[kp] < 4.0 * side {
            continue;
        }
        for k in kp - 3..=kp + 3 {
            let d = k.abs_diff(kp);
            if mag[k] <= 0.0 {
                continue;
            }
            let explained = (mag[kp] * ENV[d] / mag[k]).min(1.0);
            let rr = (1.0 - mh[k] - mp[k]).max(0.0);
            mh[k] += explained * rr;
        }
    }
}

impl MedianSplit {
    /// Settings for the `full` engine: soft 3-way masks shared by all channels (amplitude
    /// parts; the engine switches on `energy_parts` for R != 1).
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
            energy_parts: false,
            low_peak_hz: 250.0,
            merge_residual: false,
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
            low_peak_hz: 0.0,
            merge_residual: false,
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
    fn split(&self, x: &[Vec<f32>], sr: u32) -> Parts {
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
        let mut resid = vec![vec![0f32; n]; nch];
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
        let energy = self.beta > 1.0 && self.energy_parts;
        let merge_r = energy && self.merge_residual;
        // merged grouping outputs (energy mode): H2 = harmonic with P merged, R2 = residual
        let mut h2 = vec![vec![0f32; if energy { n } else { 0 }]; nch];
        let mut r2 = vec![vec![0f32; if energy { n } else { 0 }]; nch];
        let mut gains = vec![[0f32; 5]; k_bins];
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
                if self.low_peak_hz > 0.0 && self.beta > 1.0 {
                    let kmax = ((self.low_peak_hz * l as f32 / sr as f32) as usize).min(k_bins - 5);
                    protect_low_peaks(row, &mut mh[mi], &mp[mi], kmax);
                }
            }
            for ch in 0..nch {
                let mi = if self.shared_masks { 0 } else { ch };
                load_frame(&x[ch], c, &win.w, &mut buf);
                fft.forward(&mut buf, &mut spec);
                if !energy {
                    for k in 0..k_bins {
                        sh[k] = spec[k] * mh[mi][k];
                        sp[k] = spec[k] * mp[mi][k];
                    }
                    fft.inverse(&mut sh, &mut obuf);
                    add_frame(&mut harm[ch], c, &obuf, &win.w, 1.0 / l as f32);
                    fft.inverse(&mut sp, &mut obuf);
                    add_frame(&mut perc[ch], c, &obuf, &win.w, 1.0 / l as f32);
                    continue;
                }
                // energy-normalised masks: [H, P, R] for the 3-group split and [H2, R2] for the
                // merged grouping (with merge_r the residual joins the harmonic group)
                for k in 0..k_bins {
                    let (a, b) = (mh[mi][k], mp[mi][k]);
                    let rr = (1.0 - a - b).max(0.0);
                    let (ga, gr) = if merge_r { (a + rr, 0.0) } else { (a, rr) };
                    let g3 = 1.0 / (ga * ga + b * b + gr * gr).max(1e-12).sqrt();
                    let gm = ga + b;
                    let g2 = 1.0 / (gm * gm + gr * gr).max(1e-12).sqrt();
                    gains[k] = [ga * g3, b * g3, gr * g3, gm * g2, gr * g2];
                }
                let outs: [(&mut Vec<f32>, usize); 5] =
                    [(&mut harm[ch], 0), (&mut perc[ch], 1), (&mut resid[ch], 2), (&mut h2[ch], 3), (&mut r2[ch], 4)];
                for (dst, which) in outs {
                    if merge_r && (which == 2 || which == 4) {
                        continue; // silent
                    }
                    for k in 0..k_bins {
                        sh[k] = spec[k] * gains[k][which];
                    }
                    fft.inverse(&mut sh, &mut obuf);
                    add_frame(dst, c, &obuf, &win.w, 1.0 / l as f32);
                }
            }
            add_weight(&mut norm, c, &w2);
        }
        for ch in 0..nch {
            for t in 0..n {
                let g = if norm[t] > 1e-6 { 1.0 / norm[t] } else { 0.0 };
                harm[ch][t] *= g;
                perc[ch][t] *= g;
                if energy {
                    resid[ch][t] *= g;
                    h2[ch][t] *= g;
                    r2[ch][t] *= g;
                } else if self.beta <= 1.0 {
                    // 2-way: percussive takes the exact remainder
                    perc[ch][t] = x[ch][t] - harm[ch][t];
                } else {
                    resid[ch][t] = x[ch][t] - harm[ch][t] - perc[ch][t];
                }
            }
        }
        let merged_hp = energy.then_some([h2, r2]);
        Parts { harmonic: harm, percussive: perc, residual: resid, merged_hp }
    }
}

/// Keep separation smear out of the time before each onset.
///
/// A time-varying STFT mask spreads energy over the whole analysis/synthesis window, so the
/// percussive and residual parts of an attack start up to one split window (L) *before* the
/// attack. At R = 1 these smears cancel between the parts; once the parts are stretched by
/// different methods they don't, and become audible pre-echo. For every onset t0 we
/// therefore hand the region [t0 - D, t0) entirely to the harmonic part (whose phase
/// vocoder gates new components at the onset): P <- g P, R <- g R, H <- g H + (1 - g) x with
/// g = 0 there (raised-cosine fades). Parts still sum to x when they did before.
/// D = min(L, 0.75 * gap to the previous onset).
pub fn gate_before_onsets(parts: &mut Parts, x: &[Vec<f32>], onsets: &[i64], l: usize) {
    let n = x[0].len() as i64;
    let mut g = vec![1f32; n as usize];
    let fade_in = 32i64; // just before the attack
    let mut prev = i64::MIN / 4;
    for &t0 in onsets {
        if t0 <= 0 || t0 >= n {
            prev = t0;
            continue;
        }
        let d = (l as i64).min(((t0 - prev) as f64 * 0.75) as i64);
        if d < 2 * fade_in {
            prev = t0;
            continue;
        }
        let fade_out = (d / 4).min(256);
        let a = t0 - d;
        for t in a.max(0)..t0 {
            let v = if t < a + fade_out {
                0.5 + 0.5 * (std::f32::consts::PI * (t - a) as f32 / fade_out as f32).cos()
            } else if t >= t0 - fade_in {
                0.5 - 0.5 * (std::f32::consts::PI * (t - (t0 - fade_in)) as f32 / fade_in as f32).cos()
            } else {
                0.0
            };
            let gi = &mut g[t as usize];
            *gi = gi.min(v);
        }
        prev = t0;
    }
    for ch in 0..x.len() {
        for t in 0..n as usize {
            let gv = g[t];
            if gv < 1.0 {
                parts.harmonic[ch][t] = gv * parts.harmonic[ch][t] + (1.0 - gv) * x[ch][t];
                parts.percussive[ch][t] *= gv;
                parts.residual[ch][t] *= gv;
            }
        }
    }
}

/// Restrict the percussive part to the attacks.
///
/// With onsets known, the percussive (OLA/WSOLA) path only needs to carry the first few
/// milliseconds of each attack, which the onset-anchored time map reproduces 1:1. Left in the
/// grain path, the rest of a drum's decay is re-read by overlapping grains, and at large
/// ratios grain repetition turns noisy decays into a buzz at the grain rate (measured as
/// +-3..6 dB envelope ripple every 5.8 ms at R = 4). So for t outside
/// [t0 - fade, t0 + hold + fade] the percussive signal is handed to the harmonic part
/// (phase vocoder, which has the onset reset/gating/level model). Amplitude parts: H += P,
/// sums unchanged. Energy parts: crossfade to the merged grouping `merged_hp` (consumed).
/// Without onsets the whole percussive part goes to the phase vocoder: grains without
/// placement only re-read attacks (17 of 17 clicks doubled at R = 2, copies up to 10 ms early).
pub fn percussive_attacks_only(parts: &mut Parts, onsets: &[i64], hold: usize, fade: usize) {
    let n = parts.percussive[0].len();
    let mut a = vec![0f32; n];
    let (hold, fade) = (hold as i64, fade as i64);
    for &t0 in onsets {
        let lo = (t0 - fade).max(0);
        let hi = (t0 + hold + fade).min(n as i64);
        for t in lo..hi {
            let v = if t < t0 {
                0.5 - 0.5 * (std::f32::consts::PI * (t - (t0 - fade)) as f32 / fade as f32).cos()
            } else if t < t0 + hold {
                1.0
            } else {
                0.5 + 0.5 * (std::f32::consts::PI * (t - (t0 + hold)) as f32 / fade as f32).cos()
            };
            let av = &mut a[t as usize];
            *av = av.max(v);
        }
    }
    let merged = parts.merged_hp.take();
    for ch in 0..parts.percussive.len() {
        for t in 0..n {
            let (p, av) = (parts.percussive[ch][t], a[t]);
            parts.percussive[ch][t] = p * av;
            match &merged {
                None => parts.harmonic[ch][t] += p * (1.0 - av),
                Some([h2, r2]) => {
                    parts.harmonic[ch][t] = av * parts.harmonic[ch][t] + (1.0 - av) * h2[ch][t];
                    parts.residual[ch][t] = av * parts.residual[ch][t] + (1.0 - av) * r2[ch][t];
                }
            }
        }
    }
}

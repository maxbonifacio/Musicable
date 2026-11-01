//! Objective self-checks (we can't listen, so we measure).
//!
//! Every check builds a test signal, runs the engine and reports a number together with its
//! pass limit. `run_and_print` prints the table used in README.md; the integration tests in
//! `tests/` call the same functions and assert the limits.

use crate::audio::Audio;
use crate::fft::Fft;
use crate::onsets::Onset;
use crate::util::Rng;
use crate::{Options, Variant, out_len, process};
use realfft::num_complex::Complex32;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

pub const SR: u32 = 44100;

#[derive(Clone, Debug)]
pub struct Row {
    pub check: String,
    pub variant: String,
    pub param: String,
    pub value: f64,
    pub unit: &'static str,
    /// Human readable limit, empty for informational rows.
    pub limit: String,
    pub pass: Option<bool>,
}

fn row(check: &str, v: Variant, param: String, value: f64, unit: &'static str, limit: &str, pass: Option<bool>) -> Row {
    Row { check: check.into(), variant: v.name().into(), param, value, unit, limit: limit.into(), pass }
}

// ------------------------------------------------------------------ signals

pub mod sig {
    use super::*;

    pub fn sine(n: usize, f: f64, amp: f64) -> Vec<f32> {
        (0..n).map(|t| (amp * (2.0 * PI * f * t as f64 / SR as f64).sin()) as f32).collect()
    }

    pub fn white(n: usize, sigma: f64, seed: u64) -> Vec<f32> {
        let mut r = Rng::new(seed);
        (0..n).map(|_| (sigma * r.gauss()) as f32).collect()
    }

    /// Single-sample clicks at irregular positions; returns (signal, onset frames).
    pub fn click_train(n: usize) -> (Vec<f32>, Vec<u64>) {
        let mut x = vec![0f32; n];
        let mut on = Vec::new();
        let mut t = (0.3 * SR as f64) as usize;
        let mut k = 0;
        while t + SR as usize / 4 < n {
            x[t] = 0.8;
            on.push(t as u64);
            t += (SR as f64 * (0.29 + 0.07 * ((k * 7) % 5) as f64)) as usize + 13 * k;
            k += 1;
        }
        (x, on)
    }

    /// Synthetic drum hits (noise burst + damped partials above 500 Hz) over a quiet pad.
    pub fn drum_hits(n: usize, seed: u64) -> (Vec<f32>, Vec<u64>) {
        let mut r = Rng::new(seed);
        let mut x: Vec<f32> = (0..n)
            .map(|t| {
                let tt = t as f64 / SR as f64;
                let pad = [220.0, 277.18, 329.63, 440.0]
                    .iter()
                    .map(|f| (2.0 * PI * f * tt).sin())
                    .sum::<f64>();
                (0.012 * pad) as f32
            })
            .collect();
        let mut on = Vec::new();
        let mut t = (0.4 * SR as f64) as usize;
        let mut k = 0usize;
        while t + SR as usize / 3 < n {
            let amp = 0.5 + 0.1 * (k % 3) as f64;
            let tau_n = 0.012 + 0.004 * (k % 2) as f64;
            let fp = [700.0 + 90.0 * (k % 4) as f64, 1830.0, 3170.0];
            for u in 0..(SR as usize / 4) {
                let tt = u as f64 / SR as f64;
                let noise = r.gauss() * (-tt / tau_n).exp();
                let tone: f64 = fp.iter().enumerate().map(|(i, f)| (2.0 * PI * f * tt).sin() * (-tt / (0.03 + 0.02 * i as f64)).exp()).sum();
                x[t + u] += (amp * (0.6 * noise + 0.25 * tone)) as f32;
            }
            on.push(t as u64);
            t += (SR as f64 * (0.23 + 0.05 * ((k * 3) % 4) as f64)) as usize;
            k += 1;
        }
        (x, on)
    }

    /// Wide stereo test signal: common + independent noise, a 0.4 ms inter-channel delay on the
    /// common part (frequency-dependent correlation), and panned tones.
    pub fn wide_stereo(n: usize, seed: u64) -> Vec<Vec<f32>> {
        let c = white(n + 64, 0.06, seed);
        let a = white(n, 0.05, seed + 1);
        let b = white(n, 0.05, seed + 2);
        let d = 17;
        let mut l = vec![0f32; n];
        let mut rr = vec![0f32; n];
        for t in 0..n {
            let tt = t as f64 / SR as f64;
            let t1 = (0.12 * (2.0 * PI * 330.0 * tt).sin()) as f32;
            let t2 = (0.08 * (2.0 * PI * 1250.0 * tt + 0.3 * (2.0 * PI * 5.0 * tt).sin()).sin()) as f32;
            l[t] = c[t + d] + a[t] + 0.9 * t1 + 0.2 * t2;
            rr[t] = c[t] + 0.5 * b[t] + 0.25 * t1 + 0.95 * t2;
        }
        vec![l, rr]
    }

    /// Plucked harmonic notes (8 harmonics, 2 ms attack ramp, exponential decays), each note
    /// ringing on under the next one; returns (signal, onset frames).
    pub fn plucked_notes(n: usize, seed: u64) -> (Vec<f32>, Vec<u64>) {
        let mut r = Rng::new(seed);
        let mut x = vec![0f32; n];
        let mut on = Vec::new();
        let mut t = (0.35 * SR as f64) as usize;
        let notes = [196.0, 261.63, 329.63, 233.08, 392.0, 293.66, 349.23, 220.0];
        let mut k = 0;
        while t + SR as usize / 3 < n {
            let f0 = notes[k % notes.len()] * (1.0 + 0.002 * r.gauss());
            let ramp = 0.002 * SR as f64;
            for u in 0..(n - t) {
                let tt = u as f64 / SR as f64;
                let env = (u as f64 / ramp).min(1.0);
                let mut v = 0.0;
                for h in 1..=8 {
                    let fh = f0 * h as f64;
                    if fh > 15000.0 {
                        break;
                    }
                    v += (2.0 * PI * fh * tt + 0.7 * h as f64).sin() / h as f64 * (-tt / (0.6 / (h as f64).sqrt())).exp();
                }
                x[t + u] += (0.22 * env * v) as f32;
            }
            on.push(t as u64);
            t += (SR as f64 * (0.42 + 0.11 * ((k * 5) % 3) as f64)) as usize;
            k += 1;
        }
        (x, on)
    }

    pub fn onsets(frames: &[u64]) -> Vec<Onset> {
        frames.iter().map(|&f| Onset::at(f)).collect()
    }
}

// ------------------------------------------------------------------ metrics

pub mod metric {
    use super::*;

    pub fn snr_db(reference: &[f32], test: &[f32]) -> f64 {
        let mut s = 0.0;
        let mut e = 0.0;
        for (a, b) in reference.iter().zip(test) {
            s += (*a as f64).powi(2);
            e += (*a as f64 - *b as f64).powi(2);
        }
        10.0 * (s / e.max(1e-30)).log10()
    }

    fn bh4(n: usize) -> Vec<f64> {
        let (a0, a1, a2, a3) = (0.35875, 0.48829, 0.14128, 0.01168);
        (0..n)
            .map(|i| {
                let x = 2.0 * PI * i as f64 / (n - 1) as f64;
                a0 - a1 * x.cos() + a2 * (2.0 * x).cos() - a3 * (3.0 * x).cos()
            })
            .collect()
    }

    fn power_spectrum(seg: &[f32], pad: usize) -> Vec<f64> {
        let n = seg.len();
        let m = (n * pad).next_power_of_two();
        let w = bh4(n);
        let mut buf = vec![0f32; m];
        for i in 0..n {
            buf[i] = (seg[i] as f64 * w[i]) as f32;
        }
        let mut f = Fft::new(m);
        let mut sp = vec![Complex32::new(0.0, 0.0); m / 2 + 1];
        f.forward(&mut buf, &mut sp);
        sp.iter().map(|c| c.norm_sqr() as f64).collect()
    }

    #[derive(Debug, Clone, Copy)]
    pub struct SineStats {
        pub freq_err_cents: f64,
        pub ripple_db: f64,
        pub spur_db: f64,
        pub resid_db: f64,
    }

    /// Steady-state analysis of a stretched sine (1 s trimmed at both ends).
    pub fn sine_stats(y: &[f32], f0: f64) -> SineStats {
        let sr = SR as f64;
        let seg = &y[SR as usize..y.len() - SR as usize];
        let ps = power_spectrum(seg, 2);
        let m = (ps.len() - 1) * 2;
        let hz = sr / m as f64;
        let kp = (1..ps.len() - 1).max_by(|&a, &b| ps[a].total_cmp(&ps[b])).unwrap();
        let (l0, l1, l2) = (ps[kp - 1].ln(), ps[kp].ln(), ps[kp + 1].ln());
        let d = 0.5 * (l0 - l2) / (l0 - 2.0 * l1 + l2);
        let f = (kp as f64 + d) * hz;
        let excl = (20.0 / hz).ceil() as usize;
        let lo_k = (20.0 / hz) as usize;
        let mut spur = 0f64;
        let mut res = 0f64;
        let mut tot = 0f64;
        for (k, &p) in ps.iter().enumerate() {
            tot += p;
            if k < lo_k || (k as i64 - kp as i64).unsigned_abs() as usize <= excl {
                continue;
            }
            spur = spur.max(p);
            res += p;
        }
        // amplitude envelope by complex demodulation, 20 ms Hann low-pass, every 5 ms
        let hl = 882usize;
        let h: Vec<f64> = (0..hl).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / hl as f64).cos()).collect();
        let hs: f64 = h.iter().sum();
        let mut amin = f64::INFINITY;
        let mut amax = 0f64;
        let mut t = 0;
        while t + hl < seg.len() {
            let (mut re, mut im) = (0.0, 0.0);
            for i in 0..hl {
                let ph = 2.0 * PI * f0 * (t + i) as f64 / sr;
                let v = seg[t + i] as f64 * h[i];
                re += v * ph.cos();
                im -= v * ph.sin();
            }
            let a = 2.0 * (re * re + im * im).sqrt() / hs;
            amin = amin.min(a);
            amax = amax.max(a);
            t += 220;
        }
        SineStats {
            freq_err_cents: 1200.0 * (f / f0).log2(),
            ripple_db: 20.0 * (amax / amin.max(1e-12)).log10(),
            spur_db: 10.0 * (spur / ps[kp]).log10(),
            resid_db: 10.0 * (res.max(1e-30) / tot).log10(),
        }
    }

    /// Centred moving RMS with window `w` samples.
    pub fn env_rms(y: &[f32], w: usize) -> Vec<f32> {
        let n = y.len();
        let mut cs = vec![0f64; n + 1];
        for i in 0..n {
            cs[i + 1] = cs[i] + (y[i] as f64).powi(2);
        }
        let h = w / 2;
        (0..n)
            .map(|i| {
                let a = i.saturating_sub(h);
                let b = (i + w - h).min(n);
                ((cs[b] - cs[a]) / (b - a) as f64).sqrt() as f32
            })
            .collect()
    }

    /// Attack time: in [c-s, c+s] take the peak, the minimum before it (base), and return the
    /// first index after the minimum where env >= base + thr * (peak - base). Works on top of
    /// a decaying previous note.
    pub fn attack_time(env: &[f32], c: usize, s: usize, thr: f32) -> Option<usize> {
        let a = c.saturating_sub(s);
        let b = (c + s).min(env.len());
        let (pi, peak) = (a..b).map(|i| (i, env[i])).fold((a, 0f32), |m, v| if v.1 > m.1 { v } else { m });
        if peak <= 0.0 {
            return None;
        }
        let (bi, base) = (a..=pi).map(|i| (i, env[i])).fold((a, f32::INFINITY), |m, v| if v.1 < m.1 { v } else { m });
        let th = base + thr * (peak - base);
        (bi..=pi).find(|&i| env[i] >= th)
    }

    /// Number of secondary attacks within 50 ms after the main one: local maxima more than 3 ms
    /// after the main peak, above -20 dB of it, rising >= 6 dB above the preceding minimum.
    /// Also returns the pre-echo level (max env 50..3 ms before the attack, dB re peak).
    pub fn doubling(env: &[f32], c: usize) -> (usize, f64) {
        let ms = SR as usize / 1000;
        let a = c.saturating_sub(5 * ms);
        let b = (c + 10 * ms).min(env.len());
        let (pk_i, pk) = (a..b).map(|i| (i, env[i])).fold((a, 0f32), |acc, v| if v.1 > acc.1 { v } else { acc });
        if pk <= 0.0 {
            return (0, -200.0);
        }
        let end = (c + 50 * ms).min(env.len() - 1);
        let mut count = 0;
        let mut vmin = pk;
        let mut i = pk_i + 1;
        while i < end {
            vmin = vmin.min(env[i]);
            if i > pk_i + 3 * ms && env[i] >= env[i - 1] && env[i] >= env[i + 1] && env[i] > 0.1 * pk && env[i] >= 2.0 * vmin {
                count += 1;
                vmin = env[i];
            }
            i += 1;
        }
        let pa = c.saturating_sub(50 * ms);
        let pb = c.saturating_sub(3 * ms);
        let pre = env[pa..pb].iter().cloned().fold(0f32, f32::max);
        (count, 20.0 * ((pre as f64).max(1e-10) / pk as f64).log10())
    }

    /// Welch PSD (Hann 4096, hop 2048) summed into 1/3-octave bands 50 Hz..16 kHz, in dB.
    pub fn third_octave_db(y: &[f32]) -> Vec<f64> {
        let m = 4096;
        let mut f = Fft::new(m);
        let w: Vec<f32> = (0..m).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / m as f64).cos()) as f32).collect();
        let mut acc = vec![0f64; m / 2 + 1];
        let mut buf = vec![0f32; m];
        let mut sp = vec![Complex32::new(0.0, 0.0); m / 2 + 1];
        let mut t = 0;
        let mut frames = 0usize;
        while t + m <= y.len() {
            for i in 0..m {
                buf[i] = y[t + i] * w[i];
            }
            f.forward(&mut buf, &mut sp);
            for k in 0..sp.len() {
                acc[k] += sp[k].norm_sqr() as f64;
            }
            t += m / 2;
            frames += 1;
        }
        acc.iter_mut().for_each(|v| *v /= frames.max(1) as f64);
        let hz = SR as f64 / m as f64;
        let mut out = Vec::new();
        let mut fc = 50.0f64;
        while fc <= 16000.0 {
            let (lo, hi) = (fc / 2f64.powf(1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
            let s: f64 = (0..acc.len()).filter(|&k| (k as f64 * hz) >= lo && (k as f64 * hz) < hi).map(|k| acc[k]).sum();
            out.push(10.0 * s.max(1e-30).log10());
            fc *= 2f64.powf(1.0 / 3.0);
        }
        out
    }

    /// Per octave band (63 Hz..16 kHz): (inter-channel correlation, L/R level in dB, band energy).
    pub fn stereo_bands(l: &[f32], r: &[f32]) -> Vec<(f64, f64, f64)> {
        let m = 4096;
        let mut f = Fft::new(m);
        let w: Vec<f32> = (0..m).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / m as f64).cos()) as f32).collect();
        let kb = m / 2 + 1;
        let (mut pl, mut pr, mut cr) = (vec![0f64; kb], vec![0f64; kb], vec![0f64; kb]);
        let mut buf = vec![0f32; m];
        let mut sl = vec![Complex32::new(0.0, 0.0); kb];
        let mut sr_ = vec![Complex32::new(0.0, 0.0); kb];
        let mut t = 0;
        while t + m <= l.len().min(r.len()) {
            for i in 0..m {
                buf[i] = l[t + i] * w[i];
            }
            f.forward(&mut buf, &mut sl);
            for i in 0..m {
                buf[i] = r[t + i] * w[i];
            }
            f.forward(&mut buf, &mut sr_);
            for k in 0..kb {
                pl[k] += sl[k].norm_sqr() as f64;
                pr[k] += sr_[k].norm_sqr() as f64;
                cr[k] += (sl[k] * sr_[k].conj()).re as f64;
            }
            t += m / 4;
        }
        let hz = SR as f64 / m as f64;
        let mut out = Vec::new();
        let mut fc = 62.5f64;
        while fc <= 16000.0 {
            let (lo, hi) = (fc / 2f64.sqrt(), fc * 2f64.sqrt());
            let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
            for k in 0..kb {
                let fk = k as f64 * hz;
                if fk >= lo && fk < hi {
                    a += pl[k];
                    b += pr[k];
                    c += cr[k];
                }
            }
            out.push((c / (a * b).sqrt().max(1e-30), 10.0 * (a.max(1e-30) / b.max(1e-30)).log10(), a + b));
            fc *= 2.0;
        }
        out
    }
}

// ------------------------------------------------------------------ helpers

fn run(x: Vec<Vec<f32>>, r: f64, v: Variant, onsets: Vec<Onset>) -> Vec<Vec<f32>> {
    let a = Audio::new(SR, x);
    process(&a, &Options::new(r).variant(v).onsets(onsets)).ch
}

fn trim(y: &[f32], sec: f64) -> &[f32] {
    let t = (sec * SR as f64) as usize;
    &y[t..y.len() - t]
}

/// CPU time of this process in seconds (Linux /proc; falls back to None).
pub fn cpu_time() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ut: f64 = f.get(11)?.parse().ok()?;
    let st: f64 = f.get(12)?.parse().ok()?;
    Some((ut + st) / 100.0)
}

/// Find a real stereo mix in the material folder (prefers a full mix) with its onset list.
pub fn find_mix(dir: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let prefs = ["Master", "Mix", "Moon"];
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|e| e == "wav").unwrap_or(false)).collect();
    files.sort();
    let pick = prefs.iter().find_map(|k| files.iter().find(|p| p.file_name().unwrap().to_string_lossy().contains(k)).cloned()).or_else(|| files.first().cloned())?;
    let stem = pick.file_stem()?.to_string_lossy().to_string();
    let on = dir.join("onsets").join(format!("{stem}.json"));
    Some((pick.clone(), on.exists().then_some(on)))
}

// ------------------------------------------------------------------ checks

pub fn check_length(variants: &[Variant]) -> Vec<Row> {
    let mut rows = Vec::new();
    let n = SR as usize + 37;
    let x = vec![sig::white(n, 0.1, 1), sig::white(n, 0.1, 2)];
    for &v in variants {
        let mut worst = 0i64;
        for &r in &[0.5, 0.8, 1.0, 1.25, 2.0, 3.7] {
            let y = run(x.clone(), r, v, vec![Onset::at(10000)]);
            for c in &y {
                worst = worst.max((c.len() as i64 - out_len(n, r) as i64).abs());
            }
        }
        let a = Audio::new(SR, x.clone());
        let y = process(&a, &Options::new(1.5).variant(v).pitch(3.0)).ch;
        worst = worst.max((y[0].len() as i64 - out_len(n, 1.5) as i64).abs());
        rows.push(row("exact length (R 0.5..3.7, +pitch)", v, "max |len err|".into(), worst as f64, "frames", "= 0", Some(worst == 0)));
    }
    rows
}

/// R = 1 reconstruction SNR (and the split's own sum check).
pub fn check_identity(material: Option<&Path>) -> Vec<Row> {
    let mut rows = Vec::new();
    let (x, what) = match material.and_then(find_mix) {
        Some((p, _)) => {
            let a = Audio::read_wav(&p).expect("read mix");
            let n = a.len().min(10 * SR as usize);
            (a.ch.iter().map(|c| c[..n].to_vec()).collect::<Vec<_>>(), "real mix 10 s")
        }
        None => {
            let (d, _) = sig::drum_hits(8 * SR as usize, 3);
            (vec![d.clone(), d], "synthetic drums")
        }
    };
    use crate::split::Separator;
    let parts = crate::split::MedianSplit::full(SR).split(&x, SR);
    let mut worst = f64::INFINITY;
    for ch in 0..x.len() {
        let s: Vec<f32> = (0..x[ch].len()).map(|t| parts.harmonic[ch][t] + parts.percussive[ch][t] + parts.residual[ch][t]).collect();
        worst = worst.min(metric::snr_db(&x[ch], &s));
    }
    rows.push(row("split H+P+R == input", Variant::Full, what.into(), worst, "dB SNR", "> 100", Some(worst > 100.0)));
    let y = run(x.clone(), 1.0, Variant::Full, vec![]);
    let snr = (0..x.len()).map(|c| metric::snr_db(&x[c], &y[c])).fold(f64::INFINITY, f64::min);
    rows.push(row("R=1 reconstruction", Variant::Full, what.into(), snr, "dB SNR", "> 40", Some(snr > 40.0)));
    let y = run(x.clone(), 1.0, Variant::Pv, vec![]);
    let snr = (0..x.len()).map(|c| metric::snr_db(&x[c], &y[c])).fold(f64::INFINITY, f64::min);
    rows.push(row("R=1 reconstruction", Variant::Pv, what.into(), snr, "dB SNR", "", None));
    rows
}

pub fn check_sine(v: Variant, ratios: &[f64]) -> Vec<Row> {
    let mut rows = Vec::new();
    for &f0 in &[440.0, 2637.0] {
        let x = sig::sine(5 * SR as usize, f0, 0.5);
        for &r in ratios {
            let y = run(vec![x.clone()], r, v, vec![]);
            let s = metric::sine_stats(&y[0], f0);
            let p = format!("{f0} Hz R={r}");
            rows.push(row("sine freq error", v, p.clone(), s.freq_err_cents, "cents", "|x| < 1", Some(s.freq_err_cents.abs() < 1.0)));
            rows.push(row("sine AM ripple", v, p.clone(), s.ripple_db, "dB", "< 0.5", Some(s.ripple_db < 0.5)));
            rows.push(row("sine max spurious", v, p.clone(), s.spur_db, "dB", "< -50", Some(s.spur_db < -50.0)));
            rows.push(row("sine total non-fund.", v, p, s.resid_db, "dB", "", None));
        }
    }
    rows
}

/// Pitch shift: a 440 Hz sine shifted by +12 / -7 semitones (with and without time stretch)
/// must land on the target frequency and stay clean (stretch + windowed-sinc resampling).
pub fn check_pitch(v: Variant) -> Vec<Row> {
    let mut rows = Vec::new();
    let x = sig::sine(4 * SR as usize, 440.0, 0.5);
    for (semis, r) in [(12.0, 1.0), (-7.0, 1.0), (5.0, 1.5)] {
        let a = Audio::new(SR, vec![x.clone()]);
        let y = process(&a, &Options::new(r).variant(v).pitch(semis)).ch;
        let target = 440.0 * 2f64.powf(semis / 12.0);
        let s = metric::sine_stats(&y[0], target);
        let p = format!("440 Hz {semis:+} st R={r}");
        rows.push(row("pitch shift freq error", v, p.clone(), s.freq_err_cents, "cents", "|x| < 1", Some(s.freq_err_cents.abs() < 1.0)));
        rows.push(row("pitch shift max spurious", v, p, s.spur_db, "dB", "< -50", Some(s.spur_db < -50.0)));
    }
    rows
}

pub struct AttackResult {
    pub max_err_ms: f64,
    pub mean_err_ms: f64,
    pub doubled: usize,
    pub worst_pre_db: f64,
    pub n: usize,
}

pub fn attack_eval(x: &[f32], y: &[f32], on: &[u64], r: f64) -> AttackResult {
    let ms = SR as usize / 1000;
    let ex = metric::env_rms(x, ms / 2);
    let ey = metric::env_rms(y, ms / 2);
    let ex2 = metric::env_rms(x, 2 * ms);
    let ey2 = metric::env_rms(y, 2 * ms);
    let mut max_err = 0f64;
    let mut sum = 0f64;
    let mut doubled = 0;
    let mut pre = -200f64;
    let mut n = 0;
    for &t in on {
        let t = t as usize;
        let ys = (r * t as f64).round() as usize;
        if ys + 60 * ms >= y.len() || ys < 60 * ms {
            continue;
        }
        let s = (25.0 * r.min(1.0)) as usize * ms;
        let ti = metric::attack_time(&ex, t, s, 0.5).unwrap() as f64 - t as f64;
        let to = match metric::attack_time(&ey, ys, s, 0.5) {
            Some(v) => v as f64 - ys as f64,
            None => 1e9,
        };
        let err = (to - ti).abs() / ms as f64;
        max_err = max_err.max(err);
        sum += err;
        n += 1;
        let (dy, py) = metric::doubling(&ey2, ys);
        let (dx, px) = metric::doubling(&ex2, t);
        doubled += dy.saturating_sub(dx);
        let _ = px;
        pre = pre.max(py);
    }
    AttackResult { max_err_ms: max_err, mean_err_ms: sum / n.max(1) as f64, doubled, worst_pre_db: pre, n }
}

pub fn check_attacks(v: Variant, ratios: &[f64], with_onsets: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let (clk, clk_on) = sig::click_train(6 * SR as usize);
    let (drm, drm_on) = sig::drum_hits(6 * SR as usize, 7);
    for (name, x, on) in [("clicks", &clk, &clk_on), ("drums", &drm, &drm_on)] {
        for &r in ratios {
            let ons = if with_onsets { sig::onsets(on) } else { vec![] };
            let y = run(vec![x.clone(), x.clone()], r, v, ons);
            let a = attack_eval(x, &y[0], on, r);
            let p = format!("{name} R={r}{}", if with_onsets { "" } else { " no onsets" });
            let lim = with_onsets && v != Variant::Pv && v != Variant::Hptsm;
            rows.push(row("attack timing max |err|", v, p.clone(), a.max_err_ms, "ms", if lim { "< 1" } else { "" }, lim.then_some(a.max_err_ms < 1.0)));
            rows.push(row("doubled attacks", v, p.clone(), a.doubled as f64, "count", if lim { "= 0" } else { "" }, lim.then_some(a.doubled == 0)));
            if name == "clicks" {
                rows.push(row("pre-echo (clicks, worst)", v, p, a.worst_pre_db, "dB re peak", "", None));
            }
        }
    }
    rows
}

/// Harmonic attacks: timing error and pre-echo excess (output envelope over the stretched input
/// envelope, 5 ms RMS, in the 50..4 ms before each attack).
pub fn tonal_attack_eval(x: &[f32], y: &[f32], on: &[u64], r: f64) -> (f64, f64) {
    let ms = SR as usize / 1000;
    // 6 ms RMS: at least one period of the lowest note, so the envelope doesn't ripple
    let ex = metric::env_rms(x, 6 * ms);
    let ey = metric::env_rms(y, 6 * ms);
    let ex5 = metric::env_rms(x, 5 * ms);
    let ey5 = metric::env_rms(y, 5 * ms);
    let mut max_err = 0f64;
    let mut pre = -200f64;
    let floor = 1e-3 * ex5.iter().cloned().fold(0f32, f32::max) as f64;
    for &t in on {
        let t = t as usize;
        let ys = (r * t as f64).round() as usize;
        if ys + 60 * ms >= y.len() || ys < 60 * ms {
            continue;
        }
        let s = (12.0 * r.min(1.0)) as usize * ms;
        let ti = metric::attack_time(&ex, t, s, 0.5).unwrap() as f64 - t as f64;
        let to = metric::attack_time(&ey, ys, s, 0.5).map(|v| v as f64 - ys as f64).unwrap_or(1e9);
        max_err = max_err.max((to - ti).abs() / ms as f64);
        for d in (4 * ms..50 * ms).step_by(ms / 4) {
            let yo = ys - d;
            let xi = ((t as f64) - d as f64 / r).round() as usize;
            let v = 20.0 * ((ey5[yo] as f64 + floor) / (ex5[xi] as f64 + floor)).log10();
            pre = pre.max(v);
        }
    }
    (max_err, pre)
}

pub fn check_tonal_attacks(v: Variant, ratios: &[f64], with_onsets: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let (x, on) = sig::plucked_notes(7 * SR as usize, 4);
    for &r in ratios {
        let ons = if with_onsets { sig::onsets(&on) } else { vec![] };
        let y = run(vec![x.clone(), x.clone()], r, v, ons);
        let (err, pre) = tonal_attack_eval(&x, &y[0], &on, r);
        let p = format!("plucked R={r}{}", if with_onsets { "" } else { " no onsets" });
        // informational: a 6 ms RMS envelope on beating harmonic tones resolves ~+-3 ms only
        rows.push(row("tonal attack timing max |err|", v, p.clone(), err, "ms", "", None));
        rows.push(row("tonal pre-echo excess (worst)", v, p, pre, "dB", "", None));
    }
    rows
}

pub fn check_noise(v: Variant, ratios: &[f64]) -> Vec<Row> {
    let mut rows = Vec::new();
    let x = sig::white(12 * SR as usize, 0.1, 11);
    let bx = metric::third_octave_db(trim(&x, 0.5));
    for &r in ratios {
        let y = run(vec![x.clone()], r, v, vec![]);
        let by = metric::third_octave_db(trim(&y[0], 0.5));
        let dev = bx.iter().zip(&by).map(|(a, b)| (b - a).abs()).fold(0.0, f64::max);
        let lim = v == Variant::Full;
        rows.push(row("white noise 1/3-oct spectrum", v, format!("R={r}"), dev, "dB max dev", if lim { "< 1" } else { "" }, lim.then_some(dev < 1.0)));
    }
    rows
}

fn stereo_compare(x: &[Vec<f32>], y: &[Vec<f32>], trim_s: f64) -> (f64, f64) {
    let bx = metric::stereo_bands(trim(&x[0], trim_s), trim(&x[1], trim_s));
    let by = metric::stereo_bands(trim(&y[0], trim_s), trim(&y[1], trim_s));
    let etot: f64 = bx.iter().map(|b| b.2).sum();
    let mut dc = 0f64;
    let mut db = 0f64;
    for (a, b) in bx.iter().zip(&by) {
        if a.2 < 1e-5 * etot {
            continue; // skip empty bands
        }
        dc = dc.max((a.0 - b.0).abs());
        db = db.max((a.1 - b.1).abs());
    }
    (dc, db)
}

pub fn check_stereo(v: Variant, ratios: &[f64], material: Option<&Path>) -> Vec<Row> {
    let mut rows = Vec::new();
    // 30 s: the per-band correlation of a noise-like signal is itself a random estimate; at
    // 10 s its standard error in the 63 Hz octave (~400 degrees of freedom) was ~0.035 for the
    // difference of two realisations, too close to the 0.05 limit to tell bias from chance
    let x = sig::wide_stereo(30 * SR as usize, 21);
    let lim = v == Variant::Full;
    for &r in ratios {
        let y = run(x.clone(), r, v, vec![]);
        let (dc, db) = stereo_compare(&x, &y, 0.5);
        rows.push(row("stereo corr (synthetic wide)", v, format!("R={r}"), dc, "max |dcorr|", if lim { "< 0.05" } else { "" }, lim.then_some(dc < 0.05)));
        rows.push(row("stereo L/R balance (synth.)", v, format!("R={r}"), db, "dB max dev", if lim { "< 0.5" } else { "" }, lim.then_some(db < 0.5)));
    }
    if let Some((p, on)) = material.and_then(find_mix) {
        let a = Audio::read_wav(&p).expect("read mix");
        let ons = on.and_then(|o| crate::onsets::load(&o).ok()).unwrap_or_default();
        let name = p.file_stem().unwrap().to_string_lossy().chars().take(24).collect::<String>();
        for &r in ratios {
            let y = process(&a, &Options::new(r).variant(v).onsets(ons.clone())).ch;
            let (dc, db) = stereo_compare(&a.ch, &y, 0.5);
            rows.push(row("stereo corr (real mix)", v, format!("{name} R={r}"), dc, "max |dcorr|", if lim { "< 0.05" } else { "" }, lim.then_some(dc < 0.05)));
            rows.push(row("stereo L/R balance (real mix)", v, format!("{name} R={r}"), db, "dB max dev", if lim { "< 0.5" } else { "" }, lim.then_some(db < 0.5)));
        }
    }
    rows
}

pub fn check_speed(material: Option<&Path>) -> Vec<Row> {
    let mut rows = Vec::new();
    let (a, ons) = match material.and_then(find_mix) {
        Some((p, on)) => (Audio::read_wav(&p).expect("read"), on.and_then(|o| crate::onsets::load(&o).ok()).unwrap_or_default()),
        None => (Audio::new(SR, sig::wide_stereo(20 * SR as usize, 5)), vec![]),
    };
    for v in [Variant::Full, Variant::Pv, Variant::Hptsm] {
        for &r in &[0.5, 2.0] {
            let c0 = cpu_time();
            let t0 = std::time::Instant::now();
            let y = process(&a, &Options::new(r).variant(v).onsets(ons.clone()));
            let wall = t0.elapsed().as_secs_f64();
            let cpu = match (c0, cpu_time()) {
                (Some(a), Some(b)) => b - a,
                _ => wall,
            };
            let mins = y.len() as f64 / SR as f64 / 60.0;
            rows.push(row("CPU s per output minute", v, format!("stereo R={r}"), cpu / mins, "s", "", None));
        }
    }
    rows
}

pub fn run_all(material: Option<&Path>, quick: bool) -> Vec<Row> {
    let ratios: &[f64] = if quick { &[0.5, 2.0] } else { &[0.5, 0.8, 1.25, 2.0] };
    let mut rows = Vec::new();
    rows.extend(check_length(&Variant::ALL));
    rows.extend(check_identity(material));
    for v in [Variant::Full, Variant::Pv] {
        rows.extend(check_sine(v, ratios));
    }
    rows.extend(check_pitch(Variant::Full));
    rows.extend(check_attacks(Variant::Full, &[0.5, 0.8, 1.25, 2.0, 4.0], true));
    if !quick {
        rows.extend(check_attacks(Variant::Full, &[2.0], false));
        rows.extend(check_attacks(Variant::Pv, &[2.0], false));
        rows.extend(check_attacks(Variant::Hptsm, &[2.0], false));
    }
    rows.extend(check_tonal_attacks(Variant::Full, &[0.5, 2.0, 4.0], true));
    if !quick {
        rows.extend(check_tonal_attacks(Variant::Full, &[2.0, 4.0], false));
        rows.extend(check_tonal_attacks(Variant::Pv, &[2.0, 4.0], false));
        rows.extend(check_tonal_attacks(Variant::Hptsm, &[2.0, 4.0], false));
    }
    rows.extend(check_noise(Variant::Full, &[0.5, 2.0, 4.0]));
    if !quick {
        rows.extend(check_noise(Variant::Pv, &[0.5, 2.0, 4.0]));
        rows.extend(check_noise(Variant::Hptsm, &[2.0]));
    }
    rows.extend(check_stereo(Variant::Full, &[0.8, 2.0], material));
    if !quick {
        rows.extend(check_stereo(Variant::Pv, &[2.0], None));
        rows.extend(check_stereo(Variant::Hptsm, &[2.0], material));
    }
    rows.extend(check_speed(material));
    rows
}

pub fn print_table(rows: &[Row]) {
    println!("| check | variant | case | value | unit | limit | pass |");
    println!("|---|---|---|---:|---|---|---|");
    for r in rows {
        let p = match r.pass {
            Some(true) => "ok",
            Some(false) => "FAIL",
            None => "info",
        };
        println!("| {} | {} | {} | {:.3} | {} | {} | {} |", r.check, r.variant, r.param, r.value, r.unit, r.limit, p);
    }
}

pub fn run_and_print(material: Option<&Path>, quick: bool) -> bool {
    let rows = run_all(material, quick);
    print_table(&rows);
    let fails = rows.iter().filter(|r| r.pass == Some(false)).count();
    println!("\n{} checks, {} with limits, {} failed", rows.len(), rows.iter().filter(|r| r.pass.is_some()).count(), fails);
    fails == 0
}

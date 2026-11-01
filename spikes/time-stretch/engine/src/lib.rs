//! tsm-lab: offline time-scale modification research spike.
//!
//! See README.md for the design and the papers it is built from.

// DSP loops index several parallel arrays (bins, channels, frames) by the same index.
#![allow(clippy::needless_range_loop)]

pub mod audio;
pub mod checks;
pub mod classic_pv;
pub mod fft;
pub mod noise;
pub mod ola;
pub mod onsets;
pub mod pv;
pub mod resample;
pub mod split;
pub mod timemap;
pub mod util;
pub mod window;

use audio::Audio;
use onsets::Onset;
use split::{MedianSplit, Separator};
use timemap::TimeMap;
use util::scaled_pow2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// 3-way split; PGHI harmonic with onset resets; placed OLA percussive; noise resynthesis.
    Full,
    /// Like `Full` but the residual is phase-vocoded together with the harmonic part.
    Full2Way,
    /// PGHI phase vocoder alone on the whole signal.
    Pv,
    /// Classic HPTSM: binary 2-way split, phase-locked PV + OLA, per channel, no onsets.
    Hptsm,
}

impl Variant {
    pub fn parse(s: &str) -> Option<Variant> {
        Some(match s {
            "full" => Variant::Full,
            "full-2way" => Variant::Full2Way,
            "pv" => Variant::Pv,
            "hptsm" => Variant::Hptsm,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Variant::Full => "full",
            Variant::Full2Way => "full-2way",
            Variant::Pv => "pv",
            Variant::Hptsm => "hptsm",
        }
    }
    pub const ALL: [Variant; 4] = [Variant::Full, Variant::Full2Way, Variant::Pv, Variant::Hptsm];
}

#[derive(Clone, Debug)]
pub struct Options {
    pub ratio: f64,
    pub pitch_semitones: f64,
    pub variant: Variant,
    pub onsets: Vec<Onset>,
    /// Onsets weaker than this are ignored for percussive grain placement.
    pub min_place_strength: f32,
    /// Onsets weaker than this don't get the pre-onset hand-over of P and R to the PV.
    pub min_gate_strength: f32,
}

impl Options {
    pub fn new(ratio: f64) -> Self {
        Options { ratio, pitch_semitones: 0.0, variant: Variant::Full, onsets: Vec::new(), min_place_strength: 0.0, min_gate_strength: 0.25 }
    }
    pub fn variant(mut self, v: Variant) -> Self {
        self.variant = v;
        self
    }
    pub fn onsets(mut self, o: Vec<Onset>) -> Self {
        self.onsets = o;
        self
    }
    pub fn pitch(mut self, s: f64) -> Self {
        self.pitch_semitones = s;
        self
    }
}

/// Output length for a given input length and ratio.
pub fn out_len(n_in: usize, ratio: f64) -> usize {
    (n_in as f64 * ratio).round() as usize
}

fn add_into(acc: &mut [Vec<f32>], part: &[Vec<f32>]) {
    for (a, p) in acc.iter_mut().zip(part) {
        for (x, y) in a.iter_mut().zip(p) {
            *x += *y;
        }
    }
}

/// Pure time stretch of `x` (channels) by `r` to exactly `n_out` samples.
pub fn stretch_channels(x: &[Vec<f32>], sr: u32, r: f64, n_out: usize, opt: &Options) -> Vec<Vec<f32>> {
    let n_in = x[0].len();
    let ons: Vec<Onset> = opt.onsets.iter().filter(|o| (o.frame as usize) < n_in).cloned().collect();
    let place: Vec<i64> =
        ons.iter().filter(|o| o.strength >= opt.min_place_strength).map(|o| o.frame as i64).collect();
    let all_on: Vec<i64> = ons.iter().map(|o| o.frame as i64).collect();
    let lp = scaled_pow2(512, sr);
    match opt.variant {
        Variant::Pv => {
            let mut p = pv::PvParams::new(sr);
            p.reset = false;
            pv::stretch_pv(x, r, n_out, &[], &p)
        }
        Variant::Hptsm => {
            let parts = MedianSplit::hptsm(sr).split(x, sr);
            let pvw = scaled_pow2(4096, sr);
            let mut h = classic_pv::stretch_classic_pv(&parts.harmonic, r, n_out, pvw, pvw / 4);
            let op = ola::OlaParams { win: lp, hop: lp / 2, tol: 0 };
            let pp = ola::stretch_ola(&parts.percussive, &TimeMap::uniform(r), n_out, &op, &[], 0);
            add_into(&mut h, &pp);
            h
        }
        Variant::Full | Variant::Full2Way => {
            let mut sep = MedianSplit::full(sr);
            // at R = 1 every path is an identity: use the amplitude parts, which sum to the
            // input; otherwise the energy parts (incoherent recombination, see split::Parts)
            sep.energy_parts = (r - 1.0).abs() > 1e-9;
            sep.merge_residual = opt.variant == Variant::Full2Way;
            let mut parts = sep.split(x, sr);
            split::percussive_attacks_only(&mut parts, &place, lp, lp / 2);
            parts.merged_hp = None;
            // only clear attacks need the pre-onset hand-over; doing it before every weak
            // (e.g. sung) onset would keep switching the residual between two textures
            let strong: Vec<i64> =
                ons.iter().filter(|o| o.strength >= opt.min_gate_strength).map(|o| o.frame as i64).collect();
            split::gate_before_onsets(&mut parts, x, &strong, sep.win);
            let p = pv::PvParams::new(sr);
            if opt.variant == Variant::Full2Way {
                let res = std::mem::take(&mut parts.residual);
                add_into(&mut parts.harmonic, &res);
            }
            let mut h = pv::stretch_pv(&parts.harmonic, r, n_out, &ons, &p);
            let map = TimeMap::anchored(r, &place, (lp / 2) as f64);
            let op = ola::OlaParams { win: lp, hop: lp / 2, tol: lp * 7 / 8 };
            let pp = ola::stretch_ola(&parts.percussive, &map, n_out, &op, &place, (lp / 2) as i64);
            add_into(&mut h, &pp);
            if opt.variant == Variant::Full {
                let nz = noise::stretch_noise(&parts.residual, r, n_out, &all_on, &noise::NoiseParams::new(sr));
                add_into(&mut h, &nz);
            }
            h
        }
    }
}

/// Time-stretch (and optionally pitch-shift) an audio buffer. Output length is exactly
/// round(len * ratio).
pub fn process(input: &Audio, opt: &Options) -> Audio {
    let n_in = input.len();
    let n_out = out_len(n_in, opt.ratio);
    if n_in == 0 || n_out == 0 {
        return Audio::new(input.sr, vec![vec![0.0; n_out]; input.channels()]);
    }
    if opt.pitch_semitones == 0.0 {
        let ch = stretch_channels(&input.ch, input.sr, opt.ratio, n_out, opt);
        return Audio::new(input.sr, ch);
    }
    // pitch: stretch by R*p, then resample by p (read stretched signal at j*p)
    let p = 2f64.powf(opt.pitch_semitones / 12.0);
    let r_eff = opt.ratio * p;
    let n_mid = out_len(n_in, r_eff);
    let mid = stretch_channels(&input.ch, input.sr, r_eff, n_mid, opt);
    let ch = mid.iter().map(|c| resample::resample(c, n_out, p)).collect();
    Audio::new(input.sr, ch)
}

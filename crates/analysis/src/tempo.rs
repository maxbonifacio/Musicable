//! The tempo, from how the flux repeats: its autocorrelation, weighted
//! towards 120 BPM as librosa's is, with every peak kept as a candidate so
//! ×2 and ÷2 can put a wrong octave right.

use serde::{Deserialize, Serialize};

use crate::flux::Flux;

/// The tempos looked for.
pub const SLOWEST: f64 = 40.0;
pub const FASTEST: f64 = 240.0;
/// The weighting's centre, and its width in octaves.
const LIKELIEST: f64 = 120.0;
const OCTAVES: f64 = 1.0;
/// The flux's moving mean, taken away first, spans this long either side.
const MEAN_SECONDS: f64 = 0.5;

/// A tempo the sound could be at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// In beats a minute.
    pub bpm: f64,
    /// How likely, as a fraction of the likeliest's.
    pub score: f64,
    /// How much the flux repeats at this tempo: its autocorrelation there,
    /// as a fraction of the flux's power, unweighted. Near 0 for a sound
    /// with no pulse.
    pub periodicity: f64,
}

/// The tempos the flux repeats at, likeliest first.
pub fn candidates(flux: &Flux) -> Vec<Candidate> {
    let rate = flux.rate();
    let values: Vec<f64> = flux.values.iter().map(|&v| f64::from(v)).collect();
    let mean = moving_mean(&values, (MEAN_SECONDS * rate) as usize);
    let o: Vec<f64> = values.iter().zip(&mean).map(|(v, m)| v - m).collect();
    let shortest = ((60.0 / FASTEST * rate) as usize).max(1);
    let longest = ((60.0 / SLOWEST * rate) as usize).min(o.len() / 2);
    if longest < shortest + 2 {
        return Vec::new();
    }
    let power = o.iter().map(|v| v * v).sum::<f64>() / o.len() as f64;
    if power <= 0.0 {
        return Vec::new();
    }
    let correlation: Vec<f64> = (0..=longest + 1)
        .map(|lag| {
            if lag < shortest || lag >= o.len() {
                return 0.0;
            }
            let sum: f64 = o[lag..].iter().zip(&o).map(|(a, b)| a * b).sum();
            sum / (o.len() - lag) as f64 / power
        })
        .collect();
    let score: Vec<f64> = (correlation.iter().enumerate())
        .map(|(lag, correlation)| {
            let octaves = (lag as f64 / (60.0 / LIKELIEST * rate)).log2() / OCTAVES;
            correlation * (-0.5 * octaves * octaves).exp()
        })
        .collect();
    let mut peaks: Vec<Candidate> = (shortest + 1..=longest)
        .filter(|&lag| {
            score[lag] > 0.0 && score[lag] >= score[lag - 1] && score[lag] > score[lag + 1]
        })
        .map(|lag| {
            let (a, b, c) = (score[lag - 1], score[lag], score[lag + 1]);
            let curve = a - 2.0 * b + c;
            let shift = if curve < 0.0 {
                0.5 * (a - c) / curve
            } else {
                0.0
            };
            Candidate {
                bpm: 60.0 * rate / (lag as f64 + shift),
                score: b,
                periodicity: correlation[lag],
            }
        })
        .collect();
    peaks.sort_by(|a, b| b.score.total_cmp(&a.score));
    if let Some(best) = peaks.first().map(|c| c.score) {
        for peak in &mut peaks {
            peak.score /= best;
        }
    }
    peaks
}

/// The mean of `values` over `half` either side of each, as far as there
/// are values.
pub(crate) fn moving_mean(values: &[f64], half: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(values.len());
    let mut sum = 0.0;
    let (mut lo, mut hi) = (0, 0);
    for i in 0..values.len() {
        while hi < (i + half + 1).min(values.len()) {
            sum += values[hi];
            hi += 1;
        }
        while lo < i.saturating_sub(half) {
            sum -= values[lo];
            lo += 1;
        }
        out.push(sum / (hi - lo) as f64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flux::flux_of;
    use crate::groove::{Groove, SAMPLE_RATE, synth};

    #[test]
    fn a_steady_groove_has_its_tempo_first_with_its_octaves_behind() {
        for bpm in [120.0, 127.3] {
            let song = synth(30.0, &Groove::steady(bpm));
            let tempos = candidates(&flux_of(&song.audio, SAMPLE_RATE));
            assert!((tempos[0].bpm - bpm).abs() < 0.3, "{bpm}: {tempos:?}");
            assert_eq!(tempos[0].score, 1.0);
            let half = tempos.iter().find(|c| (c.bpm - bpm / 2.0).abs() < 0.3);
            assert!(half.is_some_and(|c| c.score > 0.3), "{bpm}: {tempos:?}");
        }
    }

    #[test]
    fn too_little_to_go_on_gives_no_tempo() {
        let flux = flux_of(&[0.0; 4410], SAMPLE_RATE);
        assert!(candidates(&flux).is_empty());
    }
}

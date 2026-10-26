//! Onsets, the transients: the flux's peaks, each then found again to the
//! sample where its attack starts, in the band it rose most in.

use serde::{Deserialize, Serialize};

use crate::audio::{Audio, read_padded};
use crate::flux::{CROSSOVERS, Flux};
use musicable_io::audio::AudioError;

/// A peak counts if it's at least this much of the strongest above the
/// flux's local mean (the onset research's best on its groove).
const DELTA: f32 = 0.02;
/// A peak is the highest of the frames this far either side, and at least
/// this far from the last.
const PEAK_FRAMES: usize = 3;
/// The local mean takes this many frames before a peak, and after.
const MEAN_BEFORE: usize = 10;
const MEAN_AFTER: usize = 7;
/// The last frames' windows reach past the end, where a sound that stops
/// short reads as a rise across the spectrum.
const LAST_FRAMES: usize = 3;
/// How much of the first frame's flux counts towards the strongest.
const FIRST_SCALE: f32 = 0.1;

/// Refinement looks for the attack's start from this long before a peak's
/// frame to this long after, comparing each sample's level with the
/// loudest of the stretch before it, from 15 ms to 2 ms back.
const BEFORE: f64 = 0.020;
const AFTER: f64 = 0.030;
const PAST: f64 = 0.015;
const GAP: f64 = 0.002;
/// The attack starts where its rise first reached this much of its size:
/// at half, a hit over other sound came out up to 3 ms late, and a slice
/// cut there left the start of the hit to the slice before.
const START: f64 = 0.1;
/// The envelope's release, and how long it settles before it's read.
const RELEASE: f64 = 0.010;
const SETTLE: f64 = 0.050;
/// How long after its start an attack may peak, which says how sharp it
/// is: within 2 ms is a hit, 40 ms or more a soft start.
const ATTACK: f64 = 0.050;
const SHARP: f64 = 0.002;
const SOFT: f64 = 0.040;
/// A rise this large, or larger, counts in full towards a sharp hit.
const FULL_RISE_DB: f64 = 9.0;
/// Onsets this close come back to one attack.
const MERGE: f64 = 0.001;

/// A transient: where a sound starts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Onset {
    /// The frame of the file its attack starts at.
    pub frame: u64,
    /// How much the spectrum rose, as a fraction of the file's largest
    /// rise.
    pub strength: f32,
    /// How sharp its attack is: 0 for a soft start, 1 for a hit.
    pub percussiveness: f32,
}

/// The flux's peaks: frames that are the highest of those either side,
/// above the local mean by a fraction of the strongest, each with its
/// strength.
pub(crate) fn peaks(flux: &Flux) -> Vec<(usize, f32)> {
    let values = &flux.values;
    let end = values.len().saturating_sub(LAST_FRAMES);
    if end == 0 {
        return Vec::new();
    }
    // The first frame rises from the silence before the sound by all of the
    // sound there from the start, so it counts for less in the scale.
    let rest = values[1..end].iter().fold(0.0f32, |a, &b| a.max(b));
    let strongest = rest.max(values[0] * FIRST_SCALE);
    if strongest <= 0.0 {
        return Vec::new();
    }
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    for n in 0..end {
        let value = values[n];
        let around =
            &values[n.saturating_sub(PEAK_FRAMES)..=(n + PEAK_FRAMES).min(values.len() - 1)];
        if around.iter().any(|&v| v > value) {
            continue;
        }
        let near = &values[n.saturating_sub(MEAN_BEFORE)..=(n + MEAN_AFTER).min(values.len() - 1)];
        let mean = near.iter().sum::<f32>() / near.len() as f32;
        if value < mean + DELTA * strongest || value <= 0.0 {
            continue;
        }
        if peaks
            .last()
            .is_some_and(|&(last, _)| n - last < PEAK_FRAMES)
        {
            continue;
        }
        peaks.push((n, (value / strongest).min(1.0)));
    }
    peaks
}

/// Finds each peak's onset again to the sample its attack starts at, in
/// the band its flux rose most in, reading `audio` around it. The onsets
/// come out in order of frame, one to a sample.
pub(crate) fn refine(
    audio: &mut dyn Audio,
    flux: &Flux,
    peaks: &[(usize, f32)],
) -> Result<Vec<Onset>, AudioError> {
    let rate = f64::from(audio.sample_rate());
    let samples = |seconds: f64| (seconds * rate).round() as i64;
    let (before, after) = (samples(BEFORE), samples(AFTER));
    let (past, gap) = (samples(PAST).max(1), samples(GAP).max(1));
    let settle = samples(SETTLE);
    let attack = samples(ATTACK);
    let release = (-1.0 / (RELEASE * rate)).exp();
    let filters = [
        Lowpass::new(CROSSOVERS[0], rate),
        Lowpass::new(CROSSOVERS[1], rate),
    ];
    let mut read = Vec::new();
    let mut envelope = Vec::new();
    let mut onsets: Vec<Onset> = Vec::new();
    for &(n, strength) in peaks {
        let centre = flux.sample_of(n) as i64;
        let (lo, hi) = (centre - before, centre + after);
        // From where the envelope settles to where the attack may peak.
        let base = lo - past - settle;
        read.resize((hi + attack - base + 1) as usize, 0.0);
        read_padded(audio, base, &mut read)?;
        band(&mut read, flux.band_of(n), &filters);
        envelope.clear();
        let mut level = 0.0f64;
        envelope.extend(read.iter().map(|&x| {
            level = f64::from(x.abs()).max(level * release);
            20.0 * (level + 1e-9).log10()
        }));
        // Each sample's rise above the loudest of the stretch before it.
        let at = |k: i64| envelope[(k - base) as usize];
        let mut window = Window::default();
        let mut next = lo - past;
        let rises: Vec<f64> = (lo..=hi)
            .map(|k| {
                while next <= k - gap {
                    window.push(next, at(next));
                    next += 1;
                }
                window.forget_before(k - past);
                at(k) - window.loudest()
            })
            .collect();
        let (mut best, mut top) = (0, f64::MIN);
        for (i, &rise) in rises.iter().enumerate() {
            if rise > top {
                (best, top) = (i, rise);
            }
        }
        // Back to where the rise first reached a tenth of its size.
        let mut start = best;
        while start > 0 && rises[start - 1] >= START * top {
            start -= 1;
        }
        let frame = (lo + start as i64).max(0);
        let peak = (frame..=hi + attack)
            .max_by(|&a, &b| at(a).total_cmp(&at(b)))
            .unwrap_or(frame);
        let took = (peak - frame) as f64 / rate;
        let sharpness = ((SOFT - took) / (SOFT - SHARP)).clamp(0.0, 1.0);
        let percussiveness = sharpness * (top / FULL_RISE_DB).clamp(0.0, 1.0);
        onsets.push(Onset {
            frame: frame as u64,
            strength,
            percussiveness: percussiveness as f32,
        });
    }
    // Two peaks can come back to one attack, or cross: keep the earlier
    // start, and the stronger's strength and sharpness.
    onsets.sort_by_key(|onset| onset.frame);
    let merge = samples(MERGE) as u64;
    let mut merged: Vec<Onset> = Vec::with_capacity(onsets.len());
    for onset in onsets {
        match merged.last_mut() {
            Some(last) if onset.frame - last.frame <= merge => {
                if onset.strength > last.strength {
                    *last = Onset {
                        frame: last.frame,
                        ..onset
                    };
                }
            }
            _ => merged.push(onset),
        }
    }
    Ok(merged)
}

/// The maximum of a sliding stretch of the envelope, kept as a queue of
/// the samples that could still be it.
#[derive(Default)]
struct Window {
    queue: std::collections::VecDeque<(i64, f64)>,
}

impl Window {
    fn push(&mut self, at: i64, level: f64) {
        while self.queue.back().is_some_and(|&(_, l)| l <= level) {
            self.queue.pop_back();
        }
        self.queue.push_back((at, level));
    }

    fn forget_before(&mut self, at: i64) {
        while self.queue.front().is_some_and(|&(k, _)| k < at) {
            self.queue.pop_front();
        }
    }

    fn loudest(&self) -> f64 {
        self.queue.front().map_or(-180.0, |&(_, level)| level)
    }
}

/// A one-pole low-pass filter's coefficient.
#[derive(Clone, Copy)]
struct Lowpass(f32);

impl Lowpass {
    fn new(hz: f64, rate: f64) -> Self {
        Self((-std::f64::consts::TAU * hz / rate).exp() as f32)
    }

    /// Filters `samples` twice over, from silence.
    fn twice(self, samples: &[f32], out: &mut Vec<f32>) {
        let a = self.0;
        out.clear();
        let (mut one, mut two) = (0.0, 0.0);
        out.extend(samples.iter().map(|&x| {
            one = (1.0 - a) * x + a * one;
            two = (1.0 - a) * one + a * two;
            two
        }));
    }
}

/// Keeps band `band` of `samples`: what's below the first crossover, what's
/// between the two, or what's above the second.
fn band(samples: &mut [f32], band: usize, filters: &[Lowpass; 2]) {
    let mut low = Vec::new();
    match band {
        0 => {
            filters[0].twice(samples, &mut low);
            samples.copy_from_slice(&low);
        }
        1 => {
            let mut below_high = Vec::new();
            filters[0].twice(samples, &mut low);
            filters[1].twice(samples, &mut below_high);
            // What's under the upper crossover, less what's under the lower.
            for ((x, low), below) in samples.iter_mut().zip(&low).zip(&below_high) {
                *x = below - low;
            }
        }
        _ => {
            filters[1].twice(samples, &mut low);
            for (x, low) in samples.iter_mut().zip(&low) {
                *x -= low;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Samples;
    use crate::flux::flux_of;
    use crate::groove::{Groove, SAMPLE_RATE, synth};

    fn onsets_of(samples: &[f32]) -> Vec<Onset> {
        let flux = flux_of(samples, SAMPLE_RATE);
        let mut audio = Samples {
            samples,
            sample_rate: SAMPLE_RATE,
        };
        refine(&mut audio, &flux, &peaks(&flux)).unwrap()
    }

    #[test]
    fn every_hit_of_a_groove_is_found_to_within_a_millisecond_or_so() {
        let song = synth(20.0, &Groove::steady(120.0));
        let onsets = onsets_of(&song.audio);
        let ms = |frames: f64| frames / f64::from(SAMPLE_RATE) * 1000.0;
        let mut errors = Vec::new();
        for &hit in &song.onsets {
            let nearest = (onsets.iter())
                .map(|o| o.frame as f64 - hit as f64)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                .unwrap();
            assert!(
                ms(nearest.abs()) <= 25.0,
                "a hit at {hit} found {nearest} off"
            );
            errors.push(ms(nearest.abs()));
        }
        // Nothing found where nothing was played.
        let extra = onsets.len() - song.onsets.len();
        assert!(extra <= 1, "{extra} onsets too many of {}", onsets.len());
        let within = errors.iter().filter(|&&e| e <= 1.0).count();
        assert!(
            within as f64 >= 0.95 * errors.len() as f64,
            "{within} of {} within 1 ms",
            errors.len()
        );
        assert!(onsets.windows(2).all(|w| w[0].frame < w[1].frame));
    }

    #[test]
    fn a_hit_is_percussive_and_a_swell_is_not() {
        let rate = f64::from(SAMPLE_RATE);
        let mut samples = vec![0.0f32; SAMPLE_RATE as usize * 2];
        // A click with a short decay at 0.5 s.
        for k in 0..2000 {
            let t = k as f64 / rate;
            samples[22_050 + k] = (0.8 * (-t / 0.01).exp() * (k as f64 * 0.3).sin()) as f32;
        }
        // A tone fading in over 200 ms from 1.2 s.
        for k in 0..30_000 {
            let t = k as f64 / rate;
            let gain = (t / 0.2).min(1.0) * (1.0 - (k as f64 / 30_000.0)).max(0.0);
            samples[52_920 + k] = (0.5 * gain * (k as f64 * 0.05).sin()) as f32;
        }
        let onsets = onsets_of(&samples);
        let click = onsets
            .iter()
            .find(|o| o.frame.abs_diff(22_050) < 200)
            .unwrap();
        assert!(click.percussiveness > 0.8, "{click:?}");
        let swell = onsets.iter().find(|o| o.frame.abs_diff(52_920) < 4_000);
        if let Some(swell) = swell {
            assert!(swell.percussiveness < 0.3, "{swell:?}");
        }
    }

    #[test]
    fn an_attack_over_other_sound_is_found_where_it_starts() {
        // A tone rising over 3 ms, over noise about 25 dB down: the onset is
        // where its rise starts, not partway up, where a slice cut there
        // would leave the start of the attack to the slice before.
        let rate = f64::from(SAMPLE_RATE);
        let mut noise = 12_345u32;
        let mut samples: Vec<f32> = (0..2 * SAMPLE_RATE as usize)
            .map(|_| {
                noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (f64::from(noise) / f64::from(u32::MAX) - 0.5) as f32 * 0.1
            })
            .collect();
        let start = 44_223;
        for (k, sample) in samples[start..start + 20_000].iter_mut().enumerate() {
            let t = k as f64 / rate;
            let gain = (t / 0.003).min(1.0) * (-t / 0.4).exp();
            let tone: f64 = (1..8)
                .map(|h| (std::f64::consts::TAU * 220.0 * f64::from(h) * t).sin() / f64::from(h))
                .sum();
            *sample += (0.4 * gain * tone) as f32;
        }
        let onsets = onsets_of(&samples);
        let onset = (onsets.iter())
            .min_by_key(|o| o.frame.abs_diff(start as u64))
            .unwrap();
        let ms = (onset.frame as f64 - start as f64) / rate * 1000.0;
        assert!(
            (-2.0..=1.0).contains(&ms),
            "found {ms:+.2} ms from its start"
        );
    }

    #[test]
    fn a_file_that_starts_with_a_hit_has_an_onset_at_its_first_frame() {
        let song = synth(6.0, &Groove::steady(120.0));
        let start = song.onsets[0];
        let onsets = onsets_of(&song.audio[start..]);
        assert!(onsets[0].frame <= 2, "{:?}", onsets[0]);
    }

    #[test]
    fn silence_has_no_onsets() {
        assert!(onsets_of(&[0.0; 44_100]).is_empty());
        assert!(onsets_of(&[]).is_empty());
    }
}

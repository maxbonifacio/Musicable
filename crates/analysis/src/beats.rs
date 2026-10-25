//! Beats: the classic tracker's, by dynamic programming over the flux
//! (Ellis, 2007, as librosa has it), the network's kept in step, both moved
//! onto their onsets, and the fits that read a tempo from them.

use serde::{Deserialize, Serialize};

use crate::flux::Flux;
use crate::onsets::Onset;

/// How strongly the classic tracker holds to its tempo (librosa's).
const TIGHTNESS: f64 = 100.0;
/// A beat that comes sooner than this many periods after the one before
/// is a stray.
const TOO_SOON: f64 = 0.6;
/// The network's beats keep in step by a chain whose steps lose this much
/// for straying from whole periods (squared, as a fraction of a period: a
/// tenth costs 0.3), and this much for each beat they skip, up to two,
/// against the 1 each beat adds: a long stretch the network heard no beat
/// in, such as a break, costs no more. A step spans at most this many
/// periods, and a beat's period comes from gaps this many either side of
/// it, within this factor of the overall period.
const STEADINESS: f64 = 30.0;
const SKIPPED: f64 = 0.5;
const MOST_SKIPPED: f64 = 2.0;
const LONGEST_STEP: f64 = 64.0;
const PERIOD_GAPS: usize = 16;
const NEAR_PERIOD: f64 = 1.43;
/// The smooth fit is a parabola through this many beats either side.
const SMOOTH_BEATS: usize = 8;
/// A gap this far from a whole number of periods is a break in the beat.
const BREAK: f64 = 0.25;

/// The classic tracker's beats at `bpm`, as frames of the flux: the path
/// through its peaks that best keeps to the tempo.
pub(crate) fn track(flux: &Flux, bpm: f64) -> Vec<usize> {
    let values = &flux.values;
    let n = values.len();
    let period = 60.0 * flux.rate() / bpm;
    if n < 3 || !period.is_finite() || period < 2.0 {
        return Vec::new();
    }
    let mean = values.iter().map(|&v| f64::from(v)).sum::<f64>() / n as f64;
    let spread = values
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>();
    let deviation = (spread / (n - 1) as f64).sqrt();
    if deviation <= 0.0 {
        return Vec::new();
    }
    let o: Vec<f64> = values.iter().map(|&v| f64::from(v) / deviation).collect();
    // Each frame's own score: the flux around it, weighted by a Gaussian a
    // 32nd of the period wide.
    let half = period.round() as isize;
    let kernel: Vec<f64> = (-half..=half)
        .map(|k| (-0.5 * (32.0 * k as f64 / period).powi(2)).exp())
        .collect();
    let local: Vec<f64> = (0..n as isize)
        .map(|t| {
            (kernel.iter().enumerate())
                .filter_map(|(i, w)| {
                    let j = t + i as isize - half;
                    (0..n as isize).contains(&j).then(|| o[j as usize] * w)
                })
                .sum()
        })
        .collect();
    // The best path to each frame, from a beat half a period to two periods
    // before it, less how far that strays from the period.
    let (nearest, furthest) = (
        (period / 2.0).round() as usize,
        (2.0 * period).round() as usize,
    );
    let mut total = vec![0.0; n];
    let mut back: Vec<Option<usize>> = vec![None; n];
    for t in 0..n {
        let mut best: Option<(usize, f64)> = None;
        if t >= nearest {
            let from = t.saturating_sub(furthest);
            for (before, &earlier) in total.iter().enumerate().take(t - nearest + 1).skip(from) {
                let stray = ((t - before) as f64 / period).ln();
                let score = earlier - TIGHTNESS * stray * stray;
                if best.is_none_or(|(_, s)| score > s) {
                    best = Some((before, score));
                }
            }
        }
        total[t] = local[t] + best.map_or(0.0, |(_, s)| s);
        back[t] = best.map(|(before, _)| before);
    }
    // The last beat: the last peak of the total within half the median
    // peak's (librosa's rule).
    let peaks: Vec<usize> = (1..n - 1)
        .filter(|&t| total[t] > total[t - 1] && total[t] >= total[t + 1])
        .collect();
    let mut heights: Vec<f64> = peaks.iter().map(|&t| total[t]).collect();
    heights.sort_by(f64::total_cmp);
    let Some(&median) = heights.get(heights.len() / 2) else {
        return Vec::new();
    };
    let Some(&last) = peaks.iter().rev().find(|&&t| total[t] >= 0.5 * median) else {
        return Vec::new();
    };
    let mut beats = vec![last];
    let mut at = last;
    while let Some(before) = back[at] {
        beats.push(before);
        at = before;
    }
    beats.reverse();
    // Weak beats at either end go: those under half the beats' RMS score.
    let rms = (beats.iter().map(|&b| local[b].powi(2)).sum::<f64>() / beats.len() as f64).sqrt();
    let strong = |b: &usize| local[*b] > 0.5 * rms;
    let (Some(first), Some(last)) = (
        beats.iter().position(strong),
        beats.iter().rposition(strong),
    ) else {
        return Vec::new();
    };
    beats[first..=last].to_vec()
}

/// The network's beats kept in step: of those it found, the chain that best
/// keeps to the beats' period from each to the next. A stray beat, or a
/// run at twice the tempo, is left out, and a missed beat costs a little,
/// so the chain bridges it. The periodic fit the beat research asked for.
pub(crate) fn in_step(beats: &[f64]) -> Vec<f64> {
    if beats.len() < 3 {
        return beats.to_vec();
    }
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let overall = median(gaps.clone());
    // Each beat's period: the median of the gaps near it that are near the
    // overall one, so a run of strays doesn't move it.
    let periods: Vec<f64> = (0..beats.len())
        .map(|i| {
            let near = &gaps[i.saturating_sub(PERIOD_GAPS)..(i + PERIOD_GAPS).min(gaps.len())];
            let near: Vec<f64> = (near.iter().copied())
                .filter(|&g| (1.0 / NEAR_PERIOD..=NEAR_PERIOD).contains(&(g / overall)))
                .collect();
            if near.is_empty() {
                overall
            } else {
                median(near)
            }
        })
        .collect();
    // The best chain ending at each beat: a beat each, less how far each
    // step strays from a whole number of periods, and the beats it skips.
    let mut score = vec![1.0; beats.len()];
    let mut back: Vec<Option<usize>> = vec![None; beats.len()];
    for j in 1..beats.len() {
        for i in (0..j).rev() {
            let periods = (beats[j] - beats[i]) / periods[j];
            if periods > LONGEST_STEP {
                break;
            }
            if periods < TOO_SOON {
                continue;
            }
            let count = periods.round().max(1.0);
            let off = periods / count - 1.0;
            let skipped = (count - 1.0).min(MOST_SKIPPED);
            let step = score[i] + 1.0 - STEADINESS * off * off - SKIPPED * skipped;
            if step > score[j] {
                (score[j], back[j]) = (step, Some(i));
            }
        }
    }
    let Some(mut at) = (0..beats.len()).max_by(|&a, &b| score[a].total_cmp(&score[b])) else {
        return Vec::new();
    };
    let mut chain = vec![beats[at]];
    while let Some(before) = back[at] {
        chain.push(beats[before]);
        at = before;
    }
    chain.reverse();
    chain
}

/// The median of `values`.
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values
        .get(values.len() / 2)
        .copied()
        .unwrap_or(f64::INFINITY)
}

/// The median of `gaps` within `SMOOTH_BEATS` of gap `i`.
fn local_median(gaps: &[f64], i: usize) -> f64 {
    if gaps.is_empty() {
        return f64::INFINITY;
    }
    let i = i.min(gaps.len() - 1);
    let mut near =
        gaps[i.saturating_sub(SMOOTH_BEATS)..=(i + SMOOTH_BEATS).min(gaps.len() - 1)].to_vec();
    near.sort_by(f64::total_cmp);
    near[near.len() / 2]
}

/// Moves each beat, a frame of the file, to the onset nearest it within
/// `window` frames. Returns the beats, and how many moved.
pub(crate) fn snap(beats: &[f64], onsets: &[Onset], window: f64) -> (Vec<u64>, usize) {
    let mut moved = 0;
    let snapped = beats
        .iter()
        .map(|&beat| {
            let at = onsets.partition_point(|o| (o.frame as f64) < beat);
            let nearest = [at.checked_sub(1), Some(at)]
                .into_iter()
                .flatten()
                .filter_map(|i| onsets.get(i))
                .map(|o| o.frame)
                .min_by(|&a, &b| (a as f64 - beat).abs().total_cmp(&(b as f64 - beat).abs()));
            match nearest {
                Some(frame) if (frame as f64 - beat).abs() <= window => {
                    moved += 1;
                    frame
                }
                _ => beat.round().max(0.0) as u64,
            }
        })
        .collect::<Vec<u64>>();
    // Two beats moved onto one onset leave one.
    let mut unique = snapped;
    unique.dedup();
    (unique, moved)
}

/// Each beat's number, from 0 at the first: each gap counts as one beat,
/// or as several where it spans several of the local periods, where a beat
/// was missed.
pub(crate) fn numbers(beats: &[u64]) -> Vec<f64> {
    let beats: Vec<f64> = beats.iter().map(|&b| b as f64).collect();
    numbers_of(&beats)
}

/// [`numbers`], for beats at any frame.
fn numbers_of(beats: &[f64]) -> Vec<f64> {
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let mut numbers = Vec::with_capacity(beats.len());
    if beats.is_empty() {
        return numbers;
    }
    numbers.push(0.0);
    for (i, &gap) in gaps.iter().enumerate() {
        let period = local_median(&gaps, i);
        let count = (gap / period).round().max(1.0);
        numbers.push(numbers[i] + count);
    }
    numbers
}

/// The beats where their smooth curve puts them: each where a parabola
/// through the beats either side of it says, which evens out a tracker's
/// jitter from beat to beat but follows the tempo as it moves.
pub(crate) fn smooth(beats: &[f64]) -> Vec<f64> {
    let k = numbers_of(beats);
    (0..beats.len())
        .map(|i| {
            let lo = i.saturating_sub(SMOOTH_BEATS);
            let hi = (i + SMOOTH_BEATS).min(beats.len() - 1);
            parabola_at(&k[lo..=hi], &beats[lo..=hi], k[i]).unwrap_or(beats[i])
        })
        .collect()
}

/// How many of the gaps between `beats` are no whole number of their local
/// period, by more than a quarter: where a tracker lost the beat and found
/// it again off by part of one.
pub(crate) fn breaks(beats: &[f64]) -> usize {
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    (0..gaps.len())
        .filter(|&i| {
            let periods = gaps[i] / local_median(&gaps, i);
            (periods - periods.round()).abs() > BREAK
        })
        .count()
}

/// The steady tempo that best fits a sound's beats: a straight line
/// through them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Steady {
    pub bpm: f64,
    /// The frame where the line puts the first beat.
    pub origin: f64,
    /// How far the beats stray from the line, as their RMS, in
    /// milliseconds: how loosely they're played.
    pub error_ms: f64,
    /// How far the beats' smooth curve strays from the line at most, in
    /// milliseconds: how far the tempo drifts.
    pub drift_ms: f64,
}

/// The straight fit through `beats`, frames of a file at `sample_rate`,
/// and how far their smooth curve drifts from it. `None` for fewer than
/// three beats.
pub(crate) fn fit(beats: &[u64], sample_rate: u32) -> Option<Steady> {
    if beats.len() < 3 {
        return None;
    }
    let rate = f64::from(sample_rate);
    let k = numbers(beats);
    let t: Vec<f64> = beats.iter().map(|&b| b as f64).collect();
    let (origin, slope) = line(&k, &t)?;
    let ms = |frames: f64| frames / rate * 1000.0;
    let squares: f64 = (k.iter().zip(&t))
        .map(|(k, t)| (t - origin - slope * k).powi(2))
        .sum();
    let error_ms = ms((squares / t.len() as f64).sqrt());
    let drift_ms = (0..k.len())
        .map(|i| {
            let lo = i.saturating_sub(SMOOTH_BEATS);
            let hi = (i + SMOOTH_BEATS).min(k.len() - 1);
            let smooth = parabola_at(&k[lo..=hi], &t[lo..=hi], k[i]).unwrap_or(t[i]);
            ms((smooth - origin - slope * k[i]).abs())
        })
        .fold(0.0, f64::max);
    Some(Steady {
        bpm: 60.0 * rate / slope,
        origin,
        error_ms,
        drift_ms,
    })
}

/// The least-squares line through the points: where it crosses 0, and its
/// slope.
fn line(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx).powi(2)).sum();
    if sxx <= 0.0 || sxy <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    Some((my - slope * mx, slope))
}

/// The least-squares parabola through the points, at `at`.
fn parabola_at(x: &[f64], y: &[f64], at: f64) -> Option<f64> {
    if x.len() < 3 {
        return line(x, y).map(|(origin, slope)| origin + slope * at);
    }
    let mut m = [[0.0f64; 4]; 3];
    for (xi, yi) in x.iter().zip(y) {
        let u = xi - at;
        let p = [1.0, u, u * u];
        for r in 0..3 {
            for c in 0..3 {
                m[r][c] += p[r] * p[c];
            }
            m[r][3] += p[r] * yi;
        }
    }
    // Gaussian elimination: the value at `at` is the constant term.
    for col in 0..3 {
        let pivot = (col..3).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        m.swap(col, pivot);
        if m[col][col].abs() < 1e-12 {
            return None;
        }
        for r in 0..3 {
            if r != col {
                let f = m[r][col] / m[col][col];
                let pivot_row = m[col];
                for (value, p) in m[r].iter_mut().zip(pivot_row).skip(col) {
                    *value -= f * p;
                }
            }
        }
    }
    Some(m[0][3] / m[0][0])
}

/// The F-measure of `found` against `truth`, frames of a file, each
/// matched at most once within `tolerance` frames: 1 when every beat is
/// found and nothing else.
pub fn f_measure(found: &[f64], truth: &[f64], tolerance: f64) -> f64 {
    if found.is_empty() || truth.is_empty() {
        return if found.is_empty() && truth.is_empty() {
            1.0
        } else {
            0.0
        };
    }
    let mut used = vec![false; found.len()];
    let mut hits = 0;
    for &beat in truth {
        let at = found.partition_point(|&f| f < beat - tolerance);
        let best = (at..found.len())
            .take_while(|&i| found[i] <= beat + tolerance)
            .filter(|&i| !used[i])
            .min_by(|&a, &b| (found[a] - beat).abs().total_cmp(&(found[b] - beat).abs()));
        if let Some(i) = best {
            used[i] = true;
            hits += 1;
        }
    }
    let precision = hits as f64 / found.len() as f64;
    let recall = hits as f64 / truth.len() as f64;
    if hits == 0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

/// How well two trackers' beats agree: the F-measure of `other` against
/// `beats`, at the same tempo, at twice it, or at half (either phase),
/// whichever is best.
pub(crate) fn agreement(beats: &[u64], other: &[u64], tolerance: f64) -> f64 {
    let beats: Vec<f64> = beats.iter().map(|&b| b as f64).collect();
    let other: Vec<f64> = other.iter().map(|&b| b as f64).collect();
    let doubled: Vec<f64> = (other.windows(2))
        .flat_map(|w| [w[0], 0.5 * (w[0] + w[1])])
        .chain(other.last().copied())
        .collect();
    let even: Vec<f64> = other.iter().copied().step_by(2).collect();
    let odd: Vec<f64> = other.iter().copied().skip(1).step_by(2).collect();
    [other, doubled, even, odd]
        .iter()
        .map(|o| f_measure(o, &beats, tolerance))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flux::flux_of;
    use crate::groove::{Groove, SAMPLE_RATE, synth};
    use crate::tempo::candidates;

    #[test]
    fn the_classic_tracker_finds_a_steady_grooves_beats() {
        let song = synth(30.0, &Groove::steady(120.0));
        let flux = flux_of(&song.audio, SAMPLE_RATE);
        let bpm = candidates(&flux)[0].bpm;
        let beats: Vec<f64> = (track(&flux, bpm).iter())
            .map(|&f| flux.sample_of(f) as f64)
            .collect();
        let tolerance = 0.07 * f64::from(SAMPLE_RATE);
        let f = f_measure(&beats, &song.beats, tolerance);
        assert!(f > 0.95, "F {f}");
    }

    #[test]
    fn strays_go_and_beats_stay() {
        let beats = [0.0, 100.0, 200.0, 230.0, 300.0, 400.0, 480.0, 500.0, 600.0];
        assert_eq!(
            in_step(&beats),
            [0.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0]
        );
        // A run at twice the tempo keeps every other beat, a missed beat is
        // bridged, and a long gap too.
        let mut beats: Vec<f64> = (0..20).map(|i| f64::from(i) * 100.0).collect();
        beats.extend((0..8).map(|i| 2000.0 + f64::from(i) * 50.0));
        beats.extend([2400.0, 2500.0, 2700.0, 2800.0, 3900.0, 4000.0, 4100.0]);
        let kept = in_step(&beats);
        let expected: Vec<f64> = (0..20)
            .map(|i| f64::from(i) * 100.0)
            .chain([
                2000.0, 2100.0, 2200.0, 2300.0, 2400.0, 2500.0, 2700.0, 2800.0,
            ])
            .chain([3900.0, 4000.0, 4100.0])
            .collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn beats_move_to_onsets_near_them() {
        let onset = |frame| Onset {
            frame,
            strength: 1.0,
            percussiveness: 1.0,
        };
        let onsets = [onset(95), onset(210), onset(1000)];
        let (beats, moved) = snap(&[100.0, 200.0, 300.4, 990.0, 1004.0], &onsets, 12.0);
        assert_eq!(beats, [95, 210, 300, 1000]);
        assert_eq!(moved, 4);
    }

    #[test]
    fn a_missed_beat_counts_in_the_numbers() {
        assert_eq!(numbers(&[0, 100, 200, 400, 500]), [0.0, 1.0, 2.0, 4.0, 5.0]);
    }

    #[test]
    fn smoothing_evens_out_jitter() {
        let jittery: Vec<f64> = (0..32)
            .map(|i| f64::from(i) * 100.0 + [0.0, 6.0, -6.0][i as usize % 3])
            .collect();
        let smooth = smooth(&jittery);
        for (i, beat) in smooth.iter().enumerate().skip(8).take(16) {
            assert!((beat - i as f64 * 100.0).abs() < 2.0, "{smooth:?}");
        }
    }

    #[test]
    fn a_beat_lost_by_half_a_beat_is_a_break() {
        assert_eq!(breaks(&[0.0, 100.0, 200.0, 300.0, 400.0]), 0);
        assert_eq!(
            breaks(&[0.0, 100.0, 200.0, 300.0, 450.0, 550.0, 650.0, 750.0]),
            1
        );
        assert_eq!(breaks(&[0.0, 100.0, 200.0, 400.0, 500.0, 600.0]), 0);
    }

    #[test]
    fn the_fit_reads_a_steady_tempo_and_a_drift() {
        let steady: Vec<u64> = (0..32).map(|i| 1000 + i * 22_050).collect();
        let fit = fit(&steady, 44_100).unwrap();
        assert!((fit.bpm - 120.0).abs() < 1e-9, "{fit:?}");
        assert!((fit.origin - 1000.0).abs() < 1e-6);
        assert!(fit.error_ms < 1e-6 && fit.drift_ms < 1e-6);
        // Slowing by a tenth over the second half.
        let mut at = 0.0;
        let drifting: Vec<u64> = (0..64)
            .map(|i| {
                let beat = at as u64;
                at += if i < 32 { 22_050.0 } else { 24_255.0 };
                beat
            })
            .collect();
        let fit = super::fit(&drifting, 44_100).unwrap();
        assert!(fit.drift_ms > 50.0, "{fit:?}");
        assert!(super::fit(&steady[..2], 44_100).is_none());
    }

    #[test]
    fn agreement_allows_another_octave() {
        let beats: Vec<u64> = (0..16).map(|i| i * 100).collect();
        let halves: Vec<u64> = (0..8).map(|i| 100 + i * 200).collect();
        let doubles: Vec<u64> = (0..32).map(|i| i * 50).collect();
        assert!(agreement(&beats, &beats, 10.0) > 0.99);
        assert!(agreement(&beats, &halves, 10.0) > 0.6);
        assert!(agreement(&beats, &doubles, 10.0) > 0.95);
        let elsewhere: Vec<u64> = (0..16).map(|i| 37 + i * 93).collect();
        assert!(agreement(&beats, &elsewhere, 10.0) < 0.5);
    }

    #[test]
    fn the_f_measure_counts_hits_once() {
        assert_eq!(f_measure(&[0.0, 1.0], &[0.0, 1.0], 0.1), 1.0);
        assert_eq!(f_measure(&[0.0, 0.05], &[0.0], 0.1), 2.0 / 3.0);
        assert_eq!(f_measure(&[], &[0.0], 0.1), 0.0);
        assert_eq!(f_measure(&[], &[], 0.1), 1.0);
    }
}

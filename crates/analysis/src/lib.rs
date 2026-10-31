//! Analysis of audio for warping: where its sounds start, its tempo, and
//! its beats and downbeats, from the Beat This! network beside a classic
//! tracker that checks it, or from the classic tracker alone in Fast
//! analysis. Plain functions on audio, with no UI: the app runs them on
//! the library's background queue and keeps their results in a cache.
//!
//! [`analyse`] reads a file start to end once, so a long one never has to
//! be in memory, for the spectral flux, the network and the pitch, and
//! then around what that found: each onset, to find it to the sample, and
//! each stretch with a pitch, to mark its periods. From what they find it
//! works out what auto-warp asks of a sound: whether it has a pulse,
//! whether its tempo is steady, whether it's a loop of whole bars, and
//! where a sound without a pulse syncs. Granular plays the pitch marks.

mod audio;
mod beats;
pub mod cache;
pub mod evaluate;
mod flux;
#[cfg(test)]
mod groove;
pub mod loudness;
mod network;
mod onsets;
mod pitch;
mod tempo;

use std::fmt;

use musicable_io::audio::AudioError;
use serde::{Deserialize, Serialize};

pub use audio::{Audio, File, Samples};
pub use beats::{Steady, f_measure};
pub use cache::{Cache, Found};
pub use network::{BEAT_FILE, MEL_FILE, MODELS_VARIABLE, Network, NetworkError, models_folder};
pub use onsets::Onset;
pub use pitch::{PitchMarks, PitchRun, SUBFRAMES, pitch_marks};
pub use tempo::{Candidate, FASTEST, SLOWEST};

use audio::CHUNK;

/// The analysis's version. A result kept from an older one is analysed
/// again.
pub const VERSION: u32 = 3;

/// The network's beats move to an onset this near them, and the classic
/// tracker's, on 10 ms frames, to one within three frames.
const NETWORK_SNAP: f64 = 0.040;
const CLASSIC_SNAP: f64 = 0.030;
/// Two trackers' beats match within this, as beat tracking is scored.
pub const TOLERANCE: f64 = 0.070;
/// An onset this strong, as a fraction of the strongest, is an event of
/// its own, unless it comes this soon after another.
const EVENT_STRENGTH: f32 = 0.3;
const EVENT_GAP: f64 = 0.100;
/// A pulse needs this many beats, nearly all of them a whole number of
/// local periods apart, spanning at least this much of the sound.
const PULSE_BEATS: usize = 4;
const PULSE_REGULAR: f64 = 0.8;
const PULSE_SPAN: f64 = 0.5;
/// Gaps this near a whole number of periods count as regular.
const REGULAR_SLACK: f64 = 0.15;
/// Most of the classic tracker's beats must land on onsets, and the flux
/// must repeat at its tempo: the tracker finds beats in anything.
const CLASSIC_ON_ONSETS: f64 = 0.5;
const CLASSIC_PERIODICITY: f64 = 0.15;
/// A steady tempo's beats drift from a straight line by no more than this.
pub const STEADY_DRIFT_MS: f64 = 10.0;
/// A loop's first beat is this near its start, and its length this near a
/// whole number of bars, in beats.
const LOOP_START: f64 = 0.030;
const LOOP_SLACK_BEATS: f64 = 0.1;
const MOST_BARS: u32 = 32;
/// A riser builds over at least this long to a peak at least this much
/// louder than where it started, getting louder nearly all the way.
const RISER_SECONDS: f64 = 0.5;
const RISER_DB: f64 = 10.0;
const RISER_RISING: f64 = 0.7;

/// What analysis found in a sound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// The [`VERSION`] that found it.
    pub version: u32,
    pub sample_rate: u32,
    /// The sound's length.
    pub frames: u64,
    /// Its transients, in order.
    pub onsets: Vec<Onset>,
    /// The tempos its flux repeats at, likeliest first.
    pub tempi: Vec<Candidate>,
    /// Its beats, as frames, in order.
    pub beats: Vec<u64>,
    /// Which of the beats start a bar.
    pub downbeats: Vec<usize>,
    /// Beats to a bar.
    pub meter: u32,
    /// Which tracker found the beats.
    pub tracker: Tracker,
    /// How well the classic tracker's beats agreed with the network's, as
    /// an F-measure from 0 to 1, at any octave. `None` without the network.
    pub agreement: Option<f64>,
    /// The steady tempo through the beats, if there are three or more.
    pub steady: Option<Steady>,
    /// How many times the beat slips by part of one, where the tracker lost
    /// it and found it again off the beat: a sign it's unsure.
    pub breaks: usize,
    /// How many separate strong events it has: 1 for a one-shot.
    pub events: usize,
    /// Whether it has a pulse, a beat that goes on.
    pub pulse: bool,
    /// For a sound without a pulse, the frame that should land on the
    /// grid: the attack of its first strong hit, or the peak a riser
    /// builds to.
    pub sync: Option<u64>,
    /// Whether it builds to a peak, as a riser does.
    pub riser: bool,
    /// If it's a loop at a steady tempo, starting on a beat and a whole
    /// number of bars long: how many, and the tempo they give exactly.
    #[serde(rename = "loop")]
    pub looped: Option<Loop>,
    /// Where each period of its pitch starts, where it has one.
    pub pitch: PitchMarks,
}

/// The tracker the beats come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tracker {
    Network,
    Classic,
}

/// A loop of whole bars.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    pub bars: u32,
    /// The tempo its length gives for that many bars.
    pub bpm: f64,
}

impl Analysis {
    /// What analysis knows of a sound `frames` long at `sample_rate` once
    /// it has found its transients, `onsets`: no tempo, beats or pulse yet.
    pub fn with_onsets(sample_rate: u32, frames: u64, onsets: Vec<Onset>) -> Self {
        Self {
            version: VERSION,
            sample_rate,
            frames,
            onsets,
            tempi: Vec::new(),
            beats: Vec::new(),
            downbeats: Vec::new(),
            meter: 4,
            tracker: Tracker::Classic,
            agreement: None,
            steady: None,
            breaks: 0,
            events: 0,
            pulse: false,
            sync: None,
            riser: false,
            looped: None,
            pitch: PitchMarks::default(),
        }
    }

    /// Each beat's number, from 0 at the first: a gap that spans several
    /// beats, where the tracker missed some, counts them all, so the bars
    /// stay in step.
    pub fn beat_numbers(&self) -> Vec<f64> {
        beats::numbers(&self.beats)
    }

    /// Seconds, as frames of the sound.
    fn frames_of(&self, seconds: f64) -> f64 {
        seconds * f64::from(self.sample_rate)
    }
}

/// What went wrong analysing a sound.
#[derive(Debug)]
pub enum AnalysisError {
    Audio(AudioError),
    /// The step said to stop, as when the app closes.
    Stopped,
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Audio(error) => write!(f, "{error}"),
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

impl std::error::Error for AnalysisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Audio(error) => Some(error),
            Self::Stopped => None,
        }
    }
}

impl From<AudioError> for AnalysisError {
    fn from(error: AudioError) -> Self {
        Self::Audio(error)
    }
}

/// The onsets of `audio` alone, found as [`analyse`] finds them, to the
/// sample, without its tempo, beats or pitch.
pub fn onsets(audio: &mut dyn Audio) -> Result<Vec<Onset>, AudioError> {
    let mut spectrum = flux::Spectrum::new(audio.sample_rate());
    let mut chunk = vec![0.0; CHUNK];
    let mut at = 0;
    while at < audio.frames() {
        let read = audio.read(at, &mut chunk)?;
        if read == 0 {
            break;
        }
        spectrum.push(&chunk[..read]);
        at += read as u64;
    }
    let flux = spectrum.finish();
    onsets::refine(audio, &flux, &onsets::peaks(&flux))
}

/// Analyses `audio`, with `network` if there's one, calling `step` between
/// the pieces of the work, as a job that keeps pace with the engine does:
/// it says whether to go on. If the network fails, the classic tracker's
/// beats stand alone, and `failed` says why.
pub fn analyse(
    audio: &mut dyn Audio,
    network: Option<&mut Network>,
    step: &mut dyn FnMut() -> bool,
) -> Result<(Analysis, Option<NetworkError>), AnalysisError> {
    let mut step = || {
        if step() {
            Ok(())
        } else {
            Err(AnalysisError::Stopped)
        }
    };
    let sample_rate = audio.sample_rate();
    let frames = audio.frames();
    // The first pass: the flux, the network and the pitch, chunk by chunk.
    let mut spectrum = flux::Spectrum::new(sample_rate);
    let mut tracker = pitch::Tracker::new(sample_rate);
    let mut failed = None;
    let mut listening = match network.map(|n| n.listen(sample_rate)) {
        Some(Ok(stream)) => Some(stream),
        Some(Err(error)) => {
            failed = Some(error);
            None
        }
        None => None,
    };
    let mut chunk = vec![0.0; CHUNK];
    let mut at = 0;
    while at < frames {
        let read = audio.read(at, &mut chunk)?;
        if read == 0 {
            break;
        }
        spectrum.push(&chunk[..read]);
        tracker.push(&chunk[..read]);
        if let Some(stream) = &mut listening
            && let Err(error) = stream.push(&chunk[..read])
        {
            failed = Some(error.into());
            listening = None;
        }
        at += read as u64;
        step()?;
    }
    let flux = spectrum.finish();
    let heard = match listening.map(|stream| stream.finish()) {
        Some(Ok(heard)) if at > 0 => Some(heard),
        Some(Err(error)) if at > 0 => {
            failed = Some(error.into());
            None
        }
        _ => None,
    };
    // A network stopped by its stop flag, as the app closing stops it, has
    // failed: the step, which says the same, stops here.
    step()?;
    // The second pass: each onset to the sample, and the marks of each
    // stretch with a pitch.
    let onsets = onsets::refine(audio, &flux, &onsets::peaks(&flux))?;
    step()?;
    let pitch = pitch::mark(audio, &tracker.finish(), &mut step)?;
    let tempi = tempo::candidates(&flux);
    let rate = f64::from(sample_rate);
    let mut analysis = Analysis {
        tempi,
        pitch,
        ..Analysis::with_onsets(sample_rate, frames, onsets)
    };
    let mut classic_moved = 0;
    match heard {
        Some(heard) => {
            let beats: Vec<f64> = heard.beats.iter().map(|&s| f64::from(s) * rate).collect();
            let chain = beats::in_step(&beats);
            // Before smoothing, which would spread a slip over its beats.
            analysis.breaks = beats::breaks(&chain);
            let (beats, _) = beats::snap(
                &beats::smooth(&chain),
                &analysis.onsets,
                NETWORK_SNAP * rate,
            );
            let downbeats: Vec<f64> = heard
                .downbeats
                .iter()
                .map(|&s| f64::from(s) * rate)
                .collect();
            let downbeats = downbeats_among(&beats, &downbeats, NETWORK_SNAP * rate);
            analysis.meter = meter(&downbeats).unwrap_or(4);
            analysis.downbeats = fill_downbeats(&beats, &downbeats, analysis.meter);
            // The classic tracker checks the network's beats at the
            // network's tempo, as a tracker's tempo is often another
            // metrical level's, such as three beats for four.
            let bpm = beats::fit(&beats, sample_rate).map(|s| s.bpm);
            if let Some(bpm) = bpm.or(analysis.tempi.first().map(|c| c.bpm)) {
                let (classic, _) = classic_beats(&flux, &analysis.onsets, bpm);
                analysis.agreement = Some(beats::agreement(&beats, &classic, TOLERANCE * rate));
            }
            analysis.beats = beats;
            analysis.tracker = Tracker::Network;
        }
        None => {
            if let Some(best) = analysis.tempi.first() {
                let (beats, moved) = classic_beats(&flux, &analysis.onsets, best.bpm);
                analysis.beats = beats;
                classic_moved = moved;
            }
            // Without the network, the first beat starts the first bar.
            analysis.downbeats = (0..analysis.beats.len()).step_by(4).collect();
        }
    }
    analysis.steady = beats::fit(&analysis.beats, sample_rate);
    analysis.events = events(&analysis);
    let clear = match analysis.tracker {
        Tracker::Network => true,
        Tracker::Classic => {
            let periodic =
                (analysis.tempi.first()).is_some_and(|c| c.periodicity >= CLASSIC_PERIODICITY);
            periodic
                && classic_moved as f64 >= CLASSIC_ON_ONSETS * analysis.beats.len().max(1) as f64
        }
    };
    analysis.pulse = clear && analysis.events >= 3 && regular(&analysis);
    if !analysis.pulse {
        let peak = riser(&flux, frames);
        analysis.riser = peak.is_some();
        let first = (analysis.onsets.iter()).find(|o| o.strength >= EVENT_STRENGTH);
        let first = first.or(analysis.onsets.first()).map(|o| o.frame);
        analysis.sync = peak.or(first);
    }
    analysis.looped = looped(&analysis);
    Ok((analysis, failed))
}

/// The classic tracker's beats at `bpm`, moved to their onsets, and how
/// many moved. Beats past the first and last onsets go: they're the
/// tracker counting on into silence.
fn classic_beats(flux: &flux::Flux, onsets: &[Onset], bpm: f64) -> (Vec<u64>, usize) {
    let rate = f64::from(flux.sample_rate);
    let (Some(first), Some(last)) = (onsets.first(), onsets.last()) else {
        return (Vec::new(), 0);
    };
    let half = 30.0 * rate / bpm;
    let span = first.frame as f64 - half..=last.frame as f64 + half;
    let tracked: Vec<f64> = (beats::track(flux, bpm).iter())
        .map(|&n| flux.sample_of(n) as f64)
        .filter(|b| span.contains(b))
        .collect();
    beats::snap(&tracked, onsets, CLASSIC_SNAP * rate)
}

/// The downbeats among `beats`, from those the network found, `downbeats`:
/// a bar it missed, `meter` beats after one it found and as many before
/// the next, gets its downbeat, as do the bars before the first it found
/// and after the last.
fn fill_downbeats(beats: &[u64], downbeats: &[usize], meter: u32) -> Vec<usize> {
    let (Some(&first), Some(&last)) = (downbeats.first(), downbeats.last()) else {
        return Vec::new();
    };
    let numbers = beats::numbers(beats);
    let meter = f64::from(meter);
    // The beat numbered `number`, if there's one.
    let beat_numbered = |number: f64| {
        let at = numbers.partition_point(|&n| n < number);
        (numbers.get(at) == Some(&number)).then_some(at)
    };
    let mut filled = Vec::with_capacity(downbeats.len());
    let mut number = numbers[first] - meter;
    let mut before = Vec::new();
    while number >= 0.0 {
        before.extend(beat_numbered(number));
        number -= meter;
    }
    filled.extend(before.into_iter().rev());
    for pair in downbeats.windows(2) {
        filled.push(pair[0]);
        let (from, to) = (numbers[pair[0]], numbers[pair[1]]);
        let bars = (to - from) / meter;
        if bars.fract() == 0.0 && bars > 1.0 {
            for bar in 1..bars as usize {
                filled.extend(beat_numbered(from + bar as f64 * meter));
            }
        }
    }
    filled.push(last);
    let end = numbers.last().copied().unwrap_or(0.0);
    let mut number = numbers[last] + meter;
    while number <= end {
        filled.extend(beat_numbered(number));
        number += meter;
    }
    filled
}

/// Which of `beats` the network's `downbeats` fall on, each within
/// `window` frames of one.
fn downbeats_among(beats: &[u64], downbeats: &[f64], window: f64) -> Vec<usize> {
    let mut found: Vec<usize> = downbeats
        .iter()
        .filter_map(|&d| {
            let at = beats.partition_point(|&b| (b as f64) < d);
            [at.checked_sub(1), Some(at)]
                .into_iter()
                .flatten()
                .filter(|&i| i < beats.len())
                .min_by(|&a, &b| {
                    (beats[a] as f64 - d)
                        .abs()
                        .total_cmp(&(beats[b] as f64 - d).abs())
                })
                .filter(|&i| (beats[i] as f64 - d).abs() <= window)
        })
        .collect();
    found.dedup();
    found
}

/// The most common number of beats from one downbeat to the next.
fn meter(downbeats: &[usize]) -> Option<u32> {
    let mut counts = [0usize; 9];
    for w in downbeats.windows(2) {
        if let Some(count) = counts.get_mut(w[1] - w[0]) {
            *count += 1;
        }
    }
    let (beats, &count) =
        (counts.iter().enumerate().skip(2)).max_by_key(|&(beats, &count)| (count, beats == 4))?;
    (count > 0).then_some(beats as u32)
}

/// How many strong events the sound has: its strong onsets, counting
/// those that come close together as one.
fn events(analysis: &Analysis) -> usize {
    let gap = analysis.frames_of(EVENT_GAP);
    let mut last: Option<u64> = None;
    let mut events = 0;
    for onset in analysis
        .onsets
        .iter()
        .filter(|o| o.strength >= EVENT_STRENGTH)
    {
        if last.is_none_or(|last| (onset.frame - last) as f64 > gap) {
            events += 1;
        }
        last = Some(onset.frame);
    }
    events
}

/// Whether the beats go on as a pulse does: enough of them, nearly all a
/// whole number of local periods apart, over enough of the sound.
fn regular(analysis: &Analysis) -> bool {
    let beats = &analysis.beats;
    if beats.len() < PULSE_BEATS {
        return false;
    }
    let numbers = beats::numbers(beats);
    let gaps: Vec<f64> = beats.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let steps: Vec<f64> = numbers.windows(2).map(|w| w[1] - w[0]).collect();
    let periods: Vec<f64> = gaps.iter().zip(&steps).map(|(g, s)| g / s).collect();
    let regular = (0..gaps.len())
        .filter(|&i| {
            let lo = i.saturating_sub(8);
            let mut near = periods[lo..(i + 9).min(periods.len())].to_vec();
            near.sort_by(f64::total_cmp);
            let period = near[near.len() / 2];
            let count = gaps[i] / period;
            (count - count.round()).abs() <= REGULAR_SLACK && count.round() >= 1.0
        })
        .count();
    let span = (beats[beats.len() - 1] - beats[0]) as f64;
    regular as f64 >= PULSE_REGULAR * gaps.len() as f64
        && span >= PULSE_SPAN * analysis.frames as f64
}

/// If the sound builds to a peak, as a riser does, the frame of the peak.
fn riser(flux: &flux::Flux, frames: u64) -> Option<u64> {
    let levels = &flux.levels;
    if levels.is_empty() {
        return None;
    }
    // The level in dB, smoothed over about 50 ms.
    let db: Vec<f64> = levels
        .iter()
        .map(|&l| 20.0 * (f64::from(l) + 1e-9).log10())
        .collect();
    let smooth = tempo::moving_mean(&db, (0.025 * flux.rate()) as usize);
    let peak = (0..smooth.len()).max_by(|&a, &b| smooth[a].total_cmp(&smooth[b]))?;
    let from = peak / 4;
    let seconds = (peak - from) as f64 / flux.rate();
    if seconds < RISER_SECONDS || smooth[peak] - smooth[from] < RISER_DB {
        return None;
    }
    // Louder, or within a dB, from one 50 ms step to the next.
    let step = ((0.05 * flux.rate()) as usize).max(1);
    let points: Vec<f64> = (from..=peak).step_by(step).map(|n| smooth[n]).collect();
    let rising = points.windows(2).filter(|w| w[1] >= w[0] - 1.0).count();
    if (rising as f64) < RISER_RISING * (points.len() - 1).max(1) as f64 {
        return None;
    }
    Some(flux.sample_of(peak).min(frames.saturating_sub(1)))
}

/// If the sound loops: a pulse at a steady tempo, its first beat on its
/// start, and its length within a tenth of a beat of whole bars.
fn looped(analysis: &Analysis) -> Option<Loop> {
    let steady = analysis.steady?;
    if !analysis.pulse || steady.drift_ms > STEADY_DRIFT_MS {
        return None;
    }
    let first = *analysis.beats.first()?;
    if first as f64 > analysis.frames_of(LOOP_START) {
        return None;
    }
    let beat = 60.0 * f64::from(analysis.sample_rate) / steady.bpm;
    let beats = analysis.frames as f64 / beat;
    let meter = f64::from(analysis.meter);
    let bars = (beats / meter).round();
    if bars < 1.0 || bars > f64::from(MOST_BARS) || (beats - bars * meter).abs() > LOOP_SLACK_BEATS
    {
        return None;
    }
    Some(Loop {
        bars: bars as u32,
        bpm: 60.0 * f64::from(analysis.sample_rate) * bars * meter / analysis.frames as f64,
    })
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;
    use crate::groove::{Groove, SAMPLE_RATE, kick, synth};

    fn analysed(samples: &[f32]) -> Analysis {
        let mut audio = Samples {
            samples,
            sample_rate: SAMPLE_RATE,
        };
        analyse(&mut audio, None, &mut || true).unwrap().0
    }

    #[test]
    fn a_groove_has_a_steady_pulse_at_its_tempo() {
        let song = synth(30.0, &Groove::steady(120.0));
        let analysis = analysed(&song.audio);
        assert_eq!(analysis.tracker, Tracker::Classic);
        assert!(analysis.pulse);
        let steady = analysis.steady.unwrap();
        assert!((steady.bpm - 120.0).abs() < 0.01, "{steady:?}");
        assert!(steady.drift_ms < 2.0, "{steady:?}");
        let rate = f64::from(SAMPLE_RATE);
        let beats: Vec<f64> = analysis.beats.iter().map(|&b| b as f64).collect();
        assert!(f_measure(&beats, &song.beats, TOLERANCE * rate) > 0.95);
        // Not a loop: it starts half a second in.
        assert_eq!(analysis.looped, None);
        assert_eq!(analysis.sync, None);
    }

    #[test]
    fn a_loop_of_whole_bars_gives_its_tempo_exactly() {
        let bpm = 127.3;
        let song = synth(40.0, &Groove::steady(bpm));
        let (start, end) = (
            song.downbeats[4].round() as usize,
            song.downbeats[8].round() as usize,
        );
        let analysis = analysed(&song.audio[start..end]);
        assert!(analysis.pulse);
        // At its tempo, or half it, which least stretch puts right.
        let looped = analysis.looped.unwrap();
        let exact = |bars: u32, bpm: f64| looped.bars == bars && (looped.bpm - bpm).abs() < 0.01;
        assert!(exact(4, bpm) || exact(2, bpm / 2.0), "{looped:?}");
    }

    #[test]
    fn a_single_hit_is_one_event_that_syncs_on_its_attack() {
        let mut x = vec![0.0f64; SAMPLE_RATE as usize];
        kick(&mut x, 4410, 0.9);
        let samples: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        let analysis = analysed(&samples);
        assert_eq!(analysis.events, 1);
        assert!(!analysis.pulse);
        assert!(!analysis.riser);
        let sync = analysis.sync.unwrap();
        assert!(sync.abs_diff(4410) <= 44, "{sync}");
        assert_eq!(analysis.looped, None);
    }

    #[test]
    fn a_riser_syncs_on_its_peak() {
        let rate = f64::from(SAMPLE_RATE);
        let mut noise = crate::groove::Lcg(7);
        let peak = (3.0 * rate) as usize;
        let samples: Vec<f32> = (0..(4.0 * rate) as usize)
            .map(|i| {
                let gain = if i <= peak {
                    // 40 dB up over three seconds.
                    10f64.powf(2.0 * (i as f64 / peak as f64 - 1.0))
                } else {
                    (-((i - peak) as f64) / (0.05 * rate)).exp()
                };
                (0.5 * gain * noise.next()) as f32
            })
            .collect();
        let analysis = analysed(&samples);
        assert!(!analysis.pulse);
        assert!(analysis.riser);
        let sync = analysis.sync.unwrap() as f64;
        assert!((sync - peak as f64).abs() < 0.05 * rate, "{sync}");
    }

    #[test]
    fn silence_and_nothing_analyse_to_nothing() {
        for samples in [&[][..], &[0.0; 44_100][..]] {
            let analysis = analysed(samples);
            assert!(analysis.onsets.is_empty() && analysis.beats.is_empty());
            assert!(!analysis.pulse && analysis.sync.is_none() && analysis.looped.is_none());
            assert!(analysis.pitch.runs.is_empty());
        }
    }

    #[test]
    fn a_pitched_sound_has_the_marks_pitch_marks_finds_alone() {
        let rate = f64::from(SAMPLE_RATE);
        let samples: Vec<f32> = (0..SAMPLE_RATE as usize)
            .map(|i| {
                let phase = 196.0 * i as f64 / rate;
                let harmonics = (1..=12).map(|h| (TAU * h as f64 * phase).sin() / h as f64);
                (0.3 * harmonics.sum::<f64>()) as f32
            })
            .collect();
        let analysis = analysed(&samples);
        let mut audio = Samples {
            samples: &samples,
            sample_rate: SAMPLE_RATE,
        };
        let alone = pitch_marks(&mut audio, &mut || true).unwrap();
        assert_eq!(alone.runs.len(), 1);
        assert_eq!(analysis.pitch, alone);
    }

    #[test]
    fn an_analysis_stops_when_its_step_says_to() {
        let song = synth(10.0, &Groove::steady(120.0));
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let mut steps = 0;
        let result = analyse(&mut audio, None, &mut || {
            steps += 1;
            steps < 3
        });
        assert!(matches!(result, Err(AnalysisError::Stopped)), "{result:?}");
        assert_eq!(steps, 3);
    }

    #[test]
    fn a_stopped_network_fails_and_the_classic_beats_stand() {
        let Some(folder) = network::tests::models() else {
            return;
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut network = Network::load(&folder, 1, || {}, stop).unwrap();
        let song = synth(10.0, &Groove::steady(120.0));
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let (analysis, failed) = analyse(&mut audio, Some(&mut network), &mut || true).unwrap();
        assert!(failed.is_some_and(|error| error.0.contains("stopped")));
        assert_eq!(analysis.tracker, Tracker::Classic);
        assert!(analysis.pulse);
    }

    #[test]
    fn downbeats_the_network_missed_are_filled_in() {
        let beats: Vec<u64> = (0..20).map(|i| i * 100).collect();
        // Found at beats 5 and 13, with a bar missed between.
        assert_eq!(fill_downbeats(&beats, &[5, 13], 4), [1, 5, 9, 13, 17]);
        // A bar of 3 beats leaves the gap as it is.
        assert_eq!(fill_downbeats(&beats, &[4, 11], 4), [0, 4, 11, 15, 19]);
        assert!(fill_downbeats(&beats, &[], 4).is_empty());
    }

    #[test]
    fn the_meter_is_the_commonest_bar() {
        assert_eq!(meter(&[0, 4, 8, 12, 15, 19]), Some(4));
        assert_eq!(meter(&[0, 3, 6, 9]), Some(3));
        assert_eq!(meter(&[0]), None);
    }

    #[test]
    fn the_network_finds_a_grooves_beats_and_downbeats() {
        let Some(mut network) = network::tests::network() else {
            return;
        };
        let song = synth(30.0, &Groove::steady(120.0));
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let (analysis, failed) = analyse(&mut audio, Some(&mut network), &mut || true).unwrap();
        assert_eq!(failed, None);
        assert_eq!(analysis.tracker, Tracker::Network);
        let rate = f64::from(SAMPLE_RATE);
        let beats: Vec<f64> = analysis.beats.iter().map(|&b| b as f64).collect();
        let f = f_measure(&beats, &song.beats, TOLERANCE * rate);
        assert!(f > 0.95, "F {f}");
        assert!(
            analysis.agreement.unwrap() > 0.9,
            "{:?}",
            analysis.agreement
        );
        assert!(analysis.pulse);
        assert_eq!(analysis.meter, 4);
        let downbeats: Vec<f64> = (analysis.downbeats.iter())
            .map(|&i| analysis.beats[i] as f64)
            .collect();
        let f = f_measure(&downbeats, &song.downbeats, TOLERANCE * rate);
        assert!(f > 0.9, "downbeat F {f}");
    }
}

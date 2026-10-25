//! Auto-warp: how audio files are warped as they come into a song, by rules
//! for each kind of file, from what analysis found in them.
//!
//! The first rule that fits a file wins, each with its own switch in the
//! [`Behaviour`] settings:
//!
//! - A recording, made in this song, follows the tempo as it was when it
//!   was played (see [`take`](crate::take)).
//! - A sound without a pulse, such as a one-shot, an effect or speech,
//!   stays unwarped, and plays its sync point where it was dropped: the
//!   attack of its first strong hit, or the peak a riser builds to.
//! - A loop of whole bars at a steady tempo gets one marker at its start,
//!   at the tempo its length gives for them exactly.
//! - A file at a steady tempo gets one marker on its first downbeat, at
//!   its tempo, and that downbeat goes on the bar line nearest where it
//!   was dropped, with any pickup before it.
//! - A file whose tempo drifts gets a marker on every downbeat, and on the
//!   beats between that stray from where the downbeats put them, so it
//!   locks to the grid but keeps its feel.
//!
//! Analysis is unsure of a file whose beats the classic tracker doesn't
//! agree with, or where the beat slips, and auto-warp leaves such a file
//! as it comes in: Warp warps it at the best guess at its tempo. Across the
//! rules, a file's tempo is read at the octave nearest the song's, half, as
//! it is or double, so it stretches least ([`least_stretch`]), and the
//! first file with a pulse dropped into an empty song gives the song its
//! tempo and meter instead of being stretched.
//!
//! A file dropped before it's been analysed comes in as it is, and waits
//! ([`Pending`]): the app warps it as its analysis lands, as part of the
//! drop's step in the undo history while that's the last, or else as a
//! step of its own, unless the clip has been changed since.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use musicable_analysis::{Analysis, STEADY_DRIFT_MS, Tracker};
use musicable_model::clip::{AudioClip, Clip, ClipContent, ClipId, Offset, Timing, Warping};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::warp::{Warp, WarpMarker, tempo_in_name};
use musicable_model::{Project, TrackId};

use crate::arrange::{after_frames, clear};
use crate::editor::{Behaviour, Editor};
use crate::warp::{at_least_a_tick, ceil_beats, found_onsets};
use crate::{Document, Transaction};

/// Analysis is sure of a file's beats when the classic tracker agrees with
/// the network's this well, as an F-measure.
const AGREEMENT: f64 = 0.8;
/// A beat of a drifting file gets a marker of its own where the markers
/// either side would play it this far from where it is.
pub(crate) const STRAY_MS: f64 = 10.0;
/// A file's tempo from its loop data or name is its tempo if the beats it
/// gives keep within this of analysis's over the whole file.
const CLUE_MS: f64 = 10.0;
/// Where a file's first downbeat goes on the song's first bar, audio this
/// short before it is cut, rather than the downbeat going a bar later.
const CUT_MS: f64 = 50.0;
/// A tempo from a file's loop data or name is near analysis's within this
/// fraction, at any octave, for the tempo Warp starts at.
const NEAR: f64 = 0.02;
/// A number on its own in a file's name is its tempo if the file is this
/// close to a whole number of beats at it, in seconds.
const WHOLE_BEATS: f64 = 0.005;

/// What kind of sound a file is, as auto-warp's rules ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No pulse: a one-shot, an effect, ambience, speech, a field
    /// recording.
    OneShot,
    /// A loop of whole bars at a steady tempo.
    Loop,
    /// A steady tempo, but no loop: a bounced song or stem, a programmed
    /// phrase.
    Steady,
    /// A pulse whose tempo drifts: a live band, a recording without a
    /// click.
    Drifting,
}

/// The kind of sound `analysis` found, or `None` if analysis is unsure of
/// its beats.
pub fn kind(analysis: &Analysis) -> Option<Kind> {
    if !analysis.pulse {
        return Some(Kind::OneShot);
    }
    let agrees = match analysis.tracker {
        Tracker::Network => analysis.agreement.is_some_and(|a| a >= AGREEMENT),
        // Fast analysis has nothing to check its beats against.
        Tracker::Classic => true,
    };
    if !agrees || analysis.breaks > 0 {
        return None;
    }
    if analysis.looped.is_some() {
        return Some(Kind::Loop);
    }
    let steady = analysis.steady?;
    Some(match steady.drift_ms <= STEADY_DRIFT_MS {
        true => Kind::Steady,
        false => Kind::Drifting,
    })
}

/// Whether the rule for `kind` is switched on.
fn switched_on(behaviour: &Behaviour, kind: Kind) -> bool {
    match kind {
        Kind::OneShot => behaviour.sync_one_shots,
        Kind::Loop => behaviour.warp_loops,
        Kind::Steady => behaviour.warp_steady,
        Kind::Drifting => behaviour.warp_drifting,
    }
}

/// How many beats of a song at `song` BPM to count for each beat of a
/// file at `bpm`: half, one or two, whichever brings its tempo nearest the
/// song's, so it stretches least. An 85 BPM loop in a 170 BPM song plays
/// at its own speed, as half-time.
pub fn least_stretch(bpm: f64, song: f64) -> f64 {
    let off = |factor: f64| (bpm * factor / song).ln().abs();
    [1.0, 0.5, 2.0]
        .into_iter()
        .min_by(|&a, &b| off(a).total_cmp(&off(b)))
        .unwrap_or(1.0)
}

/// The tempo a file's loop data gives, or else one its name gives next to
/// "bpm", as in "Drums 120 BPM.wav", or a number on its own in its name
/// that the file is a whole number of beats long at, as in
/// "Bass_Am_128.wav". Analysis checks it against the audio.
pub(crate) fn clue(editor: &Editor, audio: &AudioClip) -> Option<f64> {
    let file = editor.files.get(&audio.file);
    let looped = file.and_then(|f| f.tempo);
    let tempo = looped.filter(|&bpm| (Warp::MIN_TEMPO..=Warp::MAX_TEMPO).contains(&bpm));
    if tempo.is_some() {
        return tempo;
    }
    let named = tempo_in_name(&audio.file)?;
    let whole = |seconds: f64| {
        let beats = seconds * named.bpm / 60.0;
        beats.round() >= 1.0 && (beats - beats.round()).abs() * 60.0 / named.bpm <= WHOLE_BEATS
    };
    let fits = file.is_some_and(|f| whole(f.frames as f64 / f64::from(f.sample_rate)));
    (named.marked || fits).then_some(named.bpm)
}

/// The tempo of a file of `kind`, as its rule reads it: a loop's from its
/// length, and else the steady tempo through its beats, or its loop data's
/// or name's, `clue`, where that keeps to its beats. `None` for a one-shot.
fn file_tempo(analysis: &Analysis, kind: Kind, clue: Option<f64>) -> Option<f64> {
    match kind {
        Kind::OneShot => None,
        Kind::Loop => analysis.looped.map(|looped| looped.bpm),
        Kind::Steady => {
            let bpm = analysis.steady?.bpm;
            Some(
                clue.and_then(|clue| keeps_to(analysis, bpm, clue))
                    .unwrap_or(bpm),
            )
        }
        Kind::Drifting => analysis.steady.map(|steady| steady.bpm),
    }
}

/// `clue`, at the octave nearest `bpm`, if the beats it gives keep to
/// those at `bpm` over all of `analysis`'s, as a file's own tempo does.
fn keeps_to(analysis: &Analysis, bpm: f64, clue: f64) -> Option<f64> {
    let clue = clue * least_stretch(clue, bpm);
    let numbers = analysis.beat_numbers();
    let span = numbers.last()? - numbers.first()?;
    let apart_ms = span * 60_000.0 * (1.0 / clue - 1.0 / bpm).abs();
    (apart_ms <= CLUE_MS).then_some(clue)
}

/// The tempo analysis found in a file, if it has a pulse.
pub(crate) fn found_tempo(analysis: &Analysis) -> Option<f64> {
    let looped = analysis.looped.map(|looped| looped.bpm);
    looped.or_else(|| analysis.steady.filter(|_| analysis.pulse).map(|s| s.bpm))
}

/// The warp auto-warp gives a file of `kind`, counting `factor` beats of
/// the clip's own time to each of the file's (see [`least_stretch`]):
/// beat 0 of that time is a loop's start, or the file's first downbeat.
/// `None` for a one-shot, or a file with too few beats.
pub fn warp_for(analysis: &Analysis, kind: Kind, clue: Option<f64>, factor: f64) -> Option<Warp> {
    let bpm = file_tempo(analysis, kind, clue)? * factor;
    match kind {
        Kind::OneShot => None,
        Kind::Loop => Some(Warp::new(0, Beats::ZERO, bpm)),
        Kind::Steady => {
            // Where the steady line through the beats puts the first
            // downbeat, which evens out the tracker's jitter.
            let steady = analysis.steady?;
            let numbers = analysis.beat_numbers();
            let downbeat = analysis.downbeats.first().copied().unwrap_or(0);
            let number = *numbers.get(downbeat)?;
            let frames_per_beat = 60.0 * f64::from(analysis.sample_rate) / steady.bpm;
            let frame = (steady.origin + number * frames_per_beat).round().max(0.0);
            Some(Warp::new(frame as u64, Beats::ZERO, bpm))
        }
        Kind::Drifting => Some(Warp {
            markers: drifting(analysis, factor)?,
            ..Warp::new(0, Beats::ZERO, bpm)
        }),
    }
}

/// A drifting file's markers: one on each downbeat, and one on each beat
/// that the markers either side would play more than [`STRAY_MS`] from
/// where it is, until none would. Before the first downbeat and after the
/// last, the beats play at the tempo of the bar next to them.
fn drifting(analysis: &Analysis, factor: f64) -> Option<Vec<WarpMarker>> {
    let beats = &analysis.beats;
    let numbers = analysis.beat_numbers();
    let meter = (analysis.meter as usize).max(1);
    let pinned: Vec<usize> = match analysis.downbeats.is_empty() {
        true => (0..beats.len()).step_by(meter).collect(),
        false => analysis.downbeats.clone(),
    };
    let zero = numbers[*pinned.first()?];
    let pinned = pin_strays(
        beats,
        &numbers,
        pinned,
        steady_frames(analysis),
        analysis.sample_rate,
    );
    let mut markers: Vec<WarpMarker> = Vec::with_capacity(pinned.len());
    for i in pinned {
        let marker = WarpMarker {
            frame: beats[i],
            beat: Beats::from_f64((numbers[i] - zero) * factor),
        };
        if markers
            .last()
            .is_none_or(|m| m.frame < marker.frame && m.beat < marker.beat)
        {
            markers.push(marker);
        }
    }
    Some(markers)
}

/// Frames to the beat at the steady tempo through `analysis`'s beats, if
/// it found one.
pub(crate) fn steady_frames(analysis: &Analysis) -> Option<f64> {
    let rate = f64::from(analysis.sample_rate);
    analysis.steady.map(|steady| 60.0 * rate / steady.bpm)
}

/// Which of `beats`, frames of a file at `rate` with the beat numbers
/// `numbers`, get markers: those `pinned`, and each that the markers
/// either side would play more than [`STRAY_MS`] from where it is, until
/// none would, in order. Before the first marker and after the last, the
/// beats play at the tempo of the stretch next to them, or `steady` frames
/// to the beat by a marker alone.
pub(crate) fn pin_strays(
    beats: &[u64],
    numbers: &[f64],
    mut pinned: Vec<usize>,
    steady: Option<f64>,
    rate: u32,
) -> Vec<usize> {
    let stray = STRAY_MS * f64::from(rate) / 1000.0;
    let frames_per_beat =
        |a: usize, b: usize| (beats[b] - beats[a]) as f64 / (numbers[b] - numbers[a]);
    // Where the markers on the beats `pinned` would play beat `i`.
    let played = |pinned: &[usize], i: usize| -> Option<f64> {
        let after = pinned.partition_point(|&p| p < i);
        let before = after.checked_sub(1);
        let (from, per) = match (before.map(|b| pinned[b]), pinned.get(after).copied()) {
            (Some(a), Some(b)) => (a, frames_per_beat(a, b)),
            (Some(a), None) => match before.and_then(|b| b.checked_sub(1)) {
                Some(b) => (a, frames_per_beat(pinned[b], a)),
                None => (a, steady?),
            },
            (None, Some(b)) => match pinned.get(after + 1) {
                Some(&c) => (b, frames_per_beat(b, c)),
                None => (b, steady?),
            },
            (None, None) => return None,
        };
        Some(beats[from] as f64 + (numbers[i] - numbers[from]) * per)
    };
    pinned.sort_unstable();
    pinned.dedup();
    loop {
        let strays: Vec<usize> = (0..beats.len())
            .filter(|i| pinned.binary_search(i).is_err())
            .filter(|&i| played(&pinned, i).is_some_and(|at| (beats[i] as f64 - at).abs() > stray))
            .collect();
        if strays.is_empty() {
            return pinned;
        }
        pinned.extend(strays);
        pinned.sort_unstable();
    }
}

/// The tempo Warp warps the unwarped clip `clip`, playing `audio`, at,
/// where its file's rule doesn't say more, read at the octave nearest the
/// song's where it starts: the tempo analysis found in its file, or its
/// loop data's or name's where that's near it, at any octave, or else
/// either alone, or else the song's. The Sample Editor shows it as the
/// clip's tempo while it's unwarped.
pub fn tempo_guess(project: &Project, editor: &Editor, clip: &Clip, audio: &AudioClip) -> f64 {
    let song = project.tempo.bpm_at(clip.start);
    let found = (editor.analyses.get(&audio.file)).and_then(|analysis| found_tempo(analysis));
    let near = |a: f64, b: f64| (a * least_stretch(a, b) / b).ln().abs() <= NEAR.ln_1p();
    let bpm = match (found, clue(editor, audio)) {
        (Some(found), Some(clue)) if near(clue, found) => clue,
        (Some(found), _) => found,
        (None, Some(clue)) => clue,
        (None, None) => return song,
    };
    (bpm * least_stretch(bpm, song)).clamp(Warp::MIN_TEMPO, Warp::MAX_TEMPO)
}

/// The warp Warp gives the unwarped clip `clip`, playing `audio`: the one
/// its file's rule gives, where analysis is sure of the file and the rule
/// is switched on, and else one marker on the frame it starts at, at
/// [`tempo_guess`]'s tempo. It has the transients analysis found.
pub(crate) fn warp_on(project: &Project, editor: &Editor, clip: &Clip, audio: &AudioClip) -> Warp {
    let onsets = found_onsets(editor, &audio.file);
    let song = project.tempo.bpm_at(clip.start);
    let clue = clue(editor, audio);
    let by_rule = editor.analyses.get(&audio.file).and_then(|analysis| {
        let kind = kind(analysis).filter(|&k| switched_on(&editor.behaviour, k))?;
        let bpm = file_tempo(analysis, kind, clue)?;
        warp_for(analysis, kind, clue, least_stretch(bpm, song))
    });
    let warp = by_rule.unwrap_or_else(|| {
        let first = audio.offset.frames().unwrap_or(0);
        Warp::new(
            first,
            Beats::ZERO,
            tempo_guess(project, editor, clip, audio),
        )
    });
    Warp { onsets, ..warp }
}

/// A drop of audio files into the song whose analyses auto-warp waits for.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// The step in the undo history that dropped them: auto-warp joins it
    /// while it's the last.
    pub step: Option<u64>,
    /// The clips as they were dropped, each playing all of its file from
    /// where it was dropped. One changed since stays as it is.
    pub clips: Vec<Clip>,
    /// The song's tempo when they were dropped, if it had no other clips
    /// then: the first file with a pulse can set it, while nothing else
    /// has changed it, and the song has no other clips yet.
    pub empty: Option<TempoMap>,
}

/// Whether `project` has no clips, on its tracks or in their takes.
pub(crate) fn is_empty(project: &Project) -> bool {
    (project.tracks.iter())
        .all(|track| track.clips.is_empty() && track.takes.iter().all(|t| t.clips.is_empty()))
}

/// A clip auto-warp places, with what it knows of its file.
struct Dropped {
    track: TrackId,
    clip: Clip,
    file: String,
    frames: u64,
    rate: u32,
    analysis: Arc<Analysis>,
    kind: Option<Kind>,
    clue: Option<f64>,
}

/// Auto-warps the clips with ids `clips`, as they were just dropped into
/// the song, in order, each playing all of its file from where it was
/// dropped, and returns whether it changed any. A clip whose file hasn't
/// been analysed stays as it is. With `empty`, the song had no other clips,
/// so the first file with a pulse gives it its tempo and meter. A clip
/// dropped right after another on its track stays right after it as that
/// one's length changes. Each overwrites what it comes to cover but the
/// clips dropped after it, unless `careful`, which leaves a clip as it is
/// if it would cover any other clip than those dropped with it.
pub(crate) fn warp_dropped(
    tx: &mut Transaction<'_>,
    clips: &[ClipId],
    empty: bool,
    careful: bool,
) -> bool {
    let behaviour = tx.editor().behaviour;
    let mut dropped = Vec::new();
    for &id in clips {
        let Some((track, clip)) = tx.project().find_clip(id) else {
            continue;
        };
        let ClipContent::Audio(audio) = &clip.content else {
            continue;
        };
        let editor = tx.editor();
        let (Some(analysis), Some(file)) = (
            editor.analyses.get(&audio.file),
            editor.files.get(&audio.file),
        ) else {
            continue;
        };
        if audio.is_warped() || file.frames == 0 {
            continue;
        }
        dropped.push(Dropped {
            track: track.id,
            clip: clip.clone(),
            file: audio.file.clone(),
            frames: file.frames,
            rate: file.sample_rate,
            analysis: analysis.clone(),
            kind: kind(analysis),
            clue: clue(editor, audio),
        });
    }
    let tempo = tx.project().tempo.clone();
    let setter = match empty && behaviour.tempo_from_first_file {
        true => set_song_tempo(tx, &dropped),
        false => None,
    };
    let mut changed = tx.project().tempo != tempo;
    // Where each goes, in order.
    let ids: BTreeSet<ClipId> = dropped.iter().map(|d| d.clip.id).collect();
    let mut ends: BTreeMap<TrackId, (Beats, Beats)> = BTreeMap::new();
    let mut placed = Vec::with_capacity(dropped.len());
    for (i, d) in dropped.iter().enumerate() {
        let at = match ends.get(&d.track) {
            Some(&(was, now)) if was == d.clip.start => now,
            _ => d.clip.start,
        };
        let (mut timing, mut warp) = place(tx, d, at, &behaviour, setter == Some(i));
        let range = BeatRange::new(timing.start, timing.end());
        let covers = (tx.project().track(d.track)).is_some_and(|t| {
            (t.clips.iter())
                .any(|c| !ids.contains(&c.id) && c.start < range.end && c.end() > range.start)
        });
        if careful && covers {
            (timing, warp) = (d.clip.timing(), None);
        }
        ends.insert(d.track, (d.clip.end(), timing.end()));
        placed.push((d, timing, warp));
    }
    for (i, (d, timing, warp)) in placed.iter().enumerate() {
        if *timing == d.clip.timing() && warp.is_none() {
            continue;
        }
        let keep: BTreeSet<ClipId> = placed[i..].iter().map(|(d, ..)| d.clip.id).collect();
        clear(
            tx,
            d.track,
            BeatRange::new(timing.start, timing.end()),
            &keep,
        );
        let warping = Warping {
            offset: timing.offset,
            warp: warp.clone(),
        };
        tx.set_clip_warping(d.clip.id, warping);
        tx.set_clip_timing(d.clip.id, *timing);
        changed = true;
    }
    changed
}

/// Gives the song the tempo and meter of the first of `dropped` with a
/// pulse that analysis is sure of, if its tempo map has no changes, and
/// returns which that was.
fn set_song_tempo(tx: &mut Transaction<'_>, dropped: &[Dropped]) -> Option<usize> {
    let tempo = &tx.project().tempo;
    if tempo.tempos().len() > 1 || tempo.signatures().len() > 1 {
        return None;
    }
    let (i, bpm, meter) = dropped.iter().enumerate().find_map(|(i, d)| {
        let kind = d.kind.filter(|&kind| kind != Kind::OneShot)?;
        Some((i, file_tempo(&d.analysis, kind, d.clue)?, d.analysis.meter))
    })?;
    let mut map = tempo.clone();
    map.set_tempo(bpm);
    if let Some(signature) = u8::try_from(meter)
        .ok()
        .and_then(|n| TimeSignature::new(n, 4))
    {
        map.set_signature(signature);
    }
    if map != *tempo {
        tx.set_tempo(map);
    }
    Some(i)
}

/// Where auto-warp puts the dropped clip `d`, dropped at `at`, and its
/// warp, if it warps it: by its kind's rule, where analysis is sure of it
/// and the rule is switched on, at the song's tempo where it `sets_tempo`,
/// and else unwarped, playing all of its file from `at`.
fn place(
    tx: &Transaction<'_>,
    d: &Dropped,
    at: Beats,
    behaviour: &Behaviour,
    sets_tempo: bool,
) -> (Timing, Option<Warp>) {
    let tempo = &tx.project().tempo;
    let whole = || unwarped(tempo, d.rate, at, d.frames);
    let Some(kind) = d.kind.filter(|&kind| switched_on(behaviour, kind)) else {
        return (whole(), None);
    };
    if kind == Kind::OneShot {
        let timing = match d.analysis.sync {
            Some(sync) => synced(tempo, d.rate, at, sync, d.frames),
            None => whole(),
        };
        return (timing, None);
    }
    let Some(bpm) = file_tempo(&d.analysis, kind, d.clue) else {
        return (whole(), None);
    };
    let factor = match sets_tempo {
        true => 1.0,
        false => least_stretch(bpm, tempo.bpm_at(at)),
    };
    let Some(warp) = warp_for(&d.analysis, kind, d.clue, factor) else {
        return (whole(), None);
    };
    let warp = Warp {
        onsets: found_onsets(tx.editor(), &d.file),
        ..warp
    };
    let first = ceil_beats(warp.beat_at(0.0, d.rate));
    let zero = match kind {
        Kind::Loop => at,
        _ => downbeat_at(tempo, at, first),
    };
    let last = Beats::from_f64(warp.beat_at(d.frames as f64, d.rate));
    // Audio before the song's start is cut.
    let offset = first.max(-zero);
    let timing = Timing {
        start: zero + offset,
        length: at_least_a_tick(last - offset),
        offset: Offset::Beats(offset),
    };
    (timing, Some(warp))
}

/// Where beat 0 of a warped clip's own time, its file's first downbeat,
/// goes in the song for a clip dropped at `at` whose file starts at beat
/// `first` of that time: on the bar line nearest where it plays if the
/// clip starts at `at`, the earlier of two as near, so the clip moves by
/// half a bar at most, any pickup staying before it. The clip can't start
/// before the song: the audio before the song's start is cut if it's
/// short, and else the downbeat goes a bar later.
fn downbeat_at(tempo: &TempoMap, at: Beats, first: Beats) -> Beats {
    let plays = at - first;
    let mut bar = tempo.bar_at(plays).bar;
    if tempo.bar_start(bar + 1) - plays < plays - tempo.bar_start(bar) {
        bar += 1;
    }
    let ms = |beats: Beats| beats.to_f64() * 60_000.0 / tempo.bpm_at(Beats::ZERO);
    while tempo.bar_start(bar) + first < Beats::ZERO && ms(-(tempo.bar_start(bar) + first)) > CUT_MS
    {
        bar += 1;
    }
    tempo.bar_start(bar)
}

/// The timing of an unwarped clip playing all of a file `frames` long at
/// `rate`, from `at`.
fn unwarped(tempo: &TempoMap, rate: u32, at: Beats, frames: u64) -> Timing {
    let first = tempo.frame_at(at, rate);
    let end = after_frames(tempo, rate, first, frames).max(at + Beats::from_ticks(1));
    Timing {
        start: at,
        length: end - at,
        offset: Offset::Frames(0),
    }
}

/// The timing of an unwarped clip playing all of a file `frames` long at
/// `rate`, whose frame `sync` plays at `at`: it starts earlier, and if
/// that's before the song, from that much further into its file.
fn synced(tempo: &TempoMap, rate: u32, at: Beats, sync: u64, frames: u64) -> Timing {
    // The song frame its first frame plays on.
    let first = tempo.frame_at(at, rate) - sync.min(frames) as i64;
    let start = match first > 0 {
        true => after_frames(tempo, rate, first, 0),
        false => Beats::ZERO,
    };
    let skip = (tempo.frame_at(start, rate) - first).max(0) as u64;
    let end = after_frames(tempo, rate, first, frames).max(start + Beats::from_ticks(1));
    Timing {
        start,
        length: end - start,
        offset: Offset::Frames(skip),
    }
}

/// Auto-warps the drops waiting for analyses with none of their files left
/// to analyse, which `analysing` says, and returns whether that changed
/// the project: as part of each drop's step in the undo history while
/// that's the last, and else as a step of its own, called Auto-Warp, that
/// leaves other clips alone. A clip changed since it was dropped, or whose
/// file analysis couldn't read, stays as it is.
pub fn apply_pending(document: &mut Document, analysing: impl Fn(&str) -> bool) -> bool {
    let waiting = std::mem::take(&mut document.editor_mut().auto_warp);
    let mut changed = false;
    for drop in waiting {
        let waits = (drop.clips.iter()).any(|clip| match &clip.content {
            ClipContent::Audio(audio) => analysing(&audio.file),
            ClipContent::Midi(_) => false,
        });
        if waits {
            document.editor_mut().auto_warp.push(drop);
            continue;
        }
        let project = document.project();
        let ids: Vec<ClipId> = (drop.clips.iter())
            .filter(|&c| project.find_clip(c.id).is_some_and(|(_, now)| now == c))
            .map(|c| c.id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        let dropped: BTreeSet<ClipId> = drop.clips.iter().map(|c| c.id).collect();
        let alone = (project.tracks.iter())
            .flat_map(|t| {
                t.clips
                    .iter()
                    .chain(t.takes.iter().flat_map(|take| &take.clips))
            })
            .all(|c| dropped.contains(&c.id));
        let empty = alone && drop.empty.as_ref() == Some(&project.tempo);
        let amended = (drop.step)
            .and_then(|step| document.amend(step, |tx| warp_dropped(tx, &ids, empty, false)));
        changed |= match amended {
            Some(warped) => warped,
            None => document.transact("Auto-Warp", |tx| warp_dropped(tx, &ids, empty, true)),
        };
    }
    changed
}

#[cfg(test)]
mod tests;

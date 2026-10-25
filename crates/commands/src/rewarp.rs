//! Warping a clip again from a point in it: the warp commands of the
//! Sample Editor's context menu, as Live's. Set 1.1.1 Here makes a point
//! the clip's first downbeat, Warp From Here and its variants warp the rest
//! of the clip again from a point, by analysis or at one tempo, and Warp
//! Selection as a Loop fits a stretch of it into whole bars.
//!
//! They name the point by a frame of the clip's file, as the menu finds it
//! where it was opened: a marker's, a transient's, or the one that plays
//! under the pointer. Each keeps the clip's curves with its audio, and
//! overwrites what the clip comes to cover, as trimming does.

use musicable_analysis::Analysis;
use musicable_model::Project;
use musicable_model::automation::Point;
use musicable_model::clip::{AudioClip, Clip, ClipId, Offset, Timing, Warping};
use musicable_model::param::{ClipParam, ParamAddress};
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::warp::{Warp, WarpMarker};

use crate::auto_warp::{self, Kind, least_stretch};
use crate::editor::Editor;
use crate::warp::{CLIP, at_least_a_tick, clip_arg, retime, warped};
use crate::{
    Args, Command, CommandError, CommandId, Param, Registry, Transaction, Type, Value, When,
};

/// Makes a point of a warped clip its first downbeat: Set 1.1.1 Here.
pub const SET_DOWNBEAT: CommandId = CommandId::new("warp.set_downbeat");
/// Warps the rest of a warped clip again from a point, by analysis.
pub const FROM_HERE: CommandId = CommandId::new("warp.from_here");
/// The same, reading its file's tempo at the octave nearest the song's.
pub const FROM_HERE_AT_SONG_TEMPO: CommandId = CommandId::new("warp.from_here_at_song_tempo");
/// Warps the rest of a warped clip from a point at one tempo, its file's.
pub const FROM_HERE_STRAIGHT: CommandId = CommandId::new("warp.from_here_straight");
/// Warps the rest of a warped clip from a point at the song's tempo there.
pub const SONG_TEMPO_FROM_HERE: CommandId = CommandId::new("warp.song_tempo_from_here");
/// Fits a stretch of a warped clip into whole bars, as a loop.
pub const SELECTION_AS_LOOP: CommandId = CommandId::new("warp.selection_as_loop");

const FRAME: Param = Param::required(
    "frame",
    Type::Int,
    "The point, as a frame of the clip's file: a marker's, or the audio's there.",
);

/// How Warp From Here warps the rest of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    /// By analysis: a marker on each downbeat, and on each beat that
    /// strays, where its file's tempo drifts, and else at its file's tempo.
    /// That's read at the octave nearest the clip's tempo there, or the
    /// song's.
    Found { at_song_tempo: bool },
    /// At its file's tempo, read at the octave nearest the clip's there,
    /// or else at the clip's tempo there.
    Straight,
    /// At the song's tempo there.
    SongTempo,
}

/// The Warp From Here commands, with their titles and how each warps.
const FROM: [(CommandId, &str, Rest); 4] = [
    (
        FROM_HERE,
        "Warp From Here",
        Rest::Found {
            at_song_tempo: false,
        },
    ),
    (
        FROM_HERE_AT_SONG_TEMPO,
        "Warp From Here (Start at Song Tempo)",
        Rest::Found {
            at_song_tempo: true,
        },
    ),
    (
        FROM_HERE_STRAIGHT,
        "Warp From Here (Straight)",
        Rest::Straight,
    ),
    (
        SONG_TEMPO_FROM_HERE,
        "Warp at Song Tempo From Here",
        Rest::SongTempo,
    ),
];

/// What a command changes in a clip: its warping and timing, and its
/// curves, moved to stay with its audio.
struct Change {
    clip: Clip,
    warping: Warping,
    timing: Timing,
    curves: Vec<(ClipParam, Vec<Point>)>,
}

/// Registers the commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(SET_DOWNBEAT, "Set 1.1.1 Here").with_params(&[FRAME, CLIP]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let frame = frame_arg(args)?;
            let (project, editor) = (ctx.document.project(), ctx.document.editor());
            let (change, shift) = set_downbeat(project, editor, clip, frame)?;
            let shown = editor.sample_editor.clone();
            ctx.document
                .transact("Set 1.1.1 Here", |tx| apply(tx, change));
            // What was selected stays, in the clip's new time.
            if shown.clip == Some(clip) {
                let selected = &mut ctx.document.editor_mut().sample_editor;
                selected.markers = shown.markers.iter().map(|&beat| beat - shift).collect();
                selected.time =
                    (shown.time).map(|t| BeatRange::new(t.start - shift, t.end - shift));
            }
            Ok(Value::None)
        },
    );
    for (id, title, rest) in FROM {
        registry.add(
            Command::new(id, title).with_params(&[FRAME, CLIP]),
            move |ctx, args| {
                let clip = clip_arg(ctx, args)?;
                let frame = frame_arg(args)?;
                let (project, editor) = (ctx.document.project(), ctx.document.editor());
                let change = from_here(project, editor, clip, frame, rest)?;
                ctx.document.transact(title, |tx| apply(tx, change));
                Ok(Value::None)
            },
        );
    }
    registry.add(
        Command::new(SELECTION_AS_LOOP, "Warp Selection as a Loop")
            .when(When::TimeInSampleEditor)
            .with_params(&[
                Param::optional(
                    "start",
                    Type::Beats,
                    "Where the stretch starts, in the clip's own time: the Sample Editor's time \
                     selection, without it and the end.",
                ),
                Param::optional("end", Type::Beats, "Where it ends."),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let editor = ctx.document.editor();
            let shown = &editor.sample_editor;
            let range = match (args.opt::<Beats>("start")?, args.opt::<Beats>("end")?) {
                (Some(start), Some(end)) if start < end => BeatRange::new(start, end),
                (Some(_), Some(_)) => {
                    return Err(CommandError::invalid(
                        "the stretch has to end after it starts",
                    ));
                }
                (None, None) => (shown.time)
                    .filter(|_| shown.clip == Some(clip))
                    .ok_or_else(|| CommandError::invalid("no time of the clip is selected"))?,
                _ => {
                    return Err(CommandError::invalid(
                        "the stretch needs its start and its end, or neither",
                    ));
                }
            };
            let change = selection_as_loop(ctx.document.project(), editor, clip, range)?;
            let length = change.timing.length;
            ctx.document
                .transact("Warp Selection as a Loop", |tx| apply(tx, change));
            // The loop is selected.
            let selected = &mut ctx.document.editor_mut().sample_editor;
            if selected.clip == Some(clip) {
                selected.markers.clear();
                selected.time = Some(BeatRange::new(Beats::ZERO, length));
            }
            Ok(Value::None)
        },
    );
}

fn frame_arg(args: &Args) -> Result<u64, CommandError> {
    let frame: i64 = args.get("frame")?;
    u64::try_from(frame).map_err(|_| CommandError::invalid("frames start at 0"))
}

/// A warped clip's offset, in beats of its own time.
fn own_offset(audio: &AudioClip) -> Result<Beats, CommandError> {
    match audio.offset {
        Offset::Beats(offset) => Ok(offset),
        Offset::Frames(_) => Err(CommandError::invalid("the clip isn't warped")),
    }
}

/// Set 1.1.1 Here: makes the file's `frame` beat 0 of the warped clip
/// `id`'s own time, its first downbeat, and moves the clip with its audio
/// so that plays on the song's bar line nearest where it plays now. The
/// clip can't start before the song: the audio before the song's start is
/// cut. Returns the change, and how far earlier the clip's own time puts
/// each beat now.
fn set_downbeat(
    project: &Project,
    editor: &Editor,
    id: ClipId,
    frame: u64,
) -> Result<(Change, Beats), CommandError> {
    let (clip, audio, warp) = warped(project, id)?;
    let offset = own_offset(audio)?;
    let rate = editor.sample_rate_of(&audio.file);
    let shift = Beats::from_f64(warp.beat_at(frame as f64, rate));
    let plays = clip.start + shift - offset;
    let bar = nearest_bar_line(&project.tempo, plays.max(Beats::ZERO));
    let mut new = warp.clone();
    for marker in &mut new.markers {
        marker.beat -= shift;
    }
    let moved = clip.start + (bar - plays);
    let cut = (-moved).max(Beats::ZERO);
    if cut >= clip.length {
        return Err(CommandError::invalid(
            "the clip would move to before the song's start",
        ));
    }
    let timing = Timing {
        start: moved + cut,
        length: clip.length - cut,
        offset: Offset::Beats(offset - shift + cut),
    };
    Ok((change(clip, audio, warp, new, timing, rate), shift))
}

/// Warp From Here: warps the rest of the warped clip `id` again from the
/// file's `frame`, as `rest` says, keeping the markers before it, with a
/// marker at it, pinning it where it plays now, if there isn't one. The
/// clip plays the same stretch of its file as before: its end moves to
/// where that plays now.
fn from_here(
    project: &Project,
    editor: &Editor,
    id: ClipId,
    frame: u64,
    rest: Rest,
) -> Result<Change, CommandError> {
    let (clip, audio, warp) = warped(project, id)?;
    let offset = own_offset(audio)?;
    let rate = editor.sample_rate_of(&audio.file);
    let mut new = warp.clone();
    let index = match new.markers.iter().position(|m| m.frame == frame) {
        Some(index) => index,
        None => new
            .insert_at_frame(frame, rate)
            .ok_or_else(|| CommandError::invalid("a warp marker is on that tick already"))?,
    };
    let here = new.markers[index];
    // The clip's tempo there, from the marker before, and the song's.
    let clip_bpm = (new.segments(rate).nth(index.saturating_sub(1)))
        .map_or(new.tempo, |segment| segment.tempo(rate));
    let song_bpm = (project.tempo).bpm_at((clip.start + here.beat - offset).max(Beats::ZERO));
    new.markers.truncate(index + 1);
    let analysis = editor.analyses.get(&audio.file);
    let found = analysis.and_then(|analysis| auto_warp::found_tempo(analysis));
    let bpm = match rest {
        Rest::SongTempo => Some(song_bpm),
        Rest::Straight => Some(found.map_or(clip_bpm, |bpm| bpm * least_stretch(bpm, clip_bpm))),
        Rest::Found { at_song_tempo } => {
            let analysis = analysis
                .ok_or_else(|| CommandError::invalid("the clip's file hasn't been analysed yet"))?;
            let bpm = found.ok_or_else(|| {
                CommandError::invalid("analysis found no beat in the clip's file")
            })?;
            let near = if at_song_tempo { song_bpm } else { clip_bpm };
            let factor = least_stretch(bpm, near);
            let steady = matches!(auto_warp::kind(analysis), Some(Kind::Loop | Kind::Steady));
            let after = match steady {
                true => Vec::new(),
                false => drifting_from(analysis, factor, here),
            };
            new.markers.extend_from_slice(&after);
            after.is_empty().then_some(bpm * factor)
        }
    };
    if let Some(bpm) = bpm {
        let frames = editor.files.get(&audio.file).map(|file| file.frames);
        tempo_after(&mut new, bpm, frames, rate);
    }
    if !new.is_valid() {
        return Err(CommandError::invalid("the clip can't be warped so"));
    }
    // The same stretch of the file as before.
    let first = warp.frame_at(offset.to_f64(), rate);
    let last = warp.frame_at((offset + clip.length).to_f64(), rate);
    let start = Beats::from_f64(new.beat_at(first, rate));
    let end = Beats::from_f64(new.beat_at(last, rate));
    let timing = Timing {
        start: clip.start,
        length: at_least_a_tick(end - start),
        offset: Offset::Beats(start),
    };
    Ok(change(clip, audio, warp, new, timing, rate))
}

/// Warp Selection as a Loop: fits the stretch `range` of the warped clip
/// `id`'s own time into the nearest whole number of the song's bars, at
/// least one, and trims the clip to it, from the bar line nearest where it
/// starts: one marker at its start, as for a loop dropped in.
fn selection_as_loop(
    project: &Project,
    editor: &Editor,
    id: ClipId,
    range: BeatRange,
) -> Result<Change, CommandError> {
    let (clip, audio, warp) = warped(project, id)?;
    let offset = own_offset(audio)?;
    let rate = editor.sample_rate_of(&audio.file);
    let first = warp.frame_at(range.start.to_f64(), rate).round();
    let last = warp.frame_at(range.end.to_f64(), rate).round();
    if first < 0.0 || last <= first {
        return Err(CommandError::invalid(
            "the stretch has none of the file in it",
        ));
    }
    let tempo = &project.tempo;
    let start = nearest_bar_line(tempo, (clip.start + range.start - offset).max(Beats::ZERO));
    let bar = tempo.bar_at(start).bar;
    let bar_length = tempo.bar_start(bar + 1) - start;
    let bars = (range.len().to_f64() / bar_length.to_f64())
        .round()
        .max(1.0) as i64;
    let length = tempo.bar_start(bar + bars) - start;
    let bpm = 60.0 * f64::from(rate) * length.to_f64() / (last - first);
    if !(Warp::MIN_TEMPO..=Warp::MAX_TEMPO).contains(&bpm) {
        return Err(CommandError::invalid(format!(
            "that takes the clip's tempo to {bpm:.2} BPM, beyond what a clip can have"
        )));
    }
    let new = Warp {
        markers: vec![WarpMarker {
            frame: first as u64,
            beat: Beats::ZERO,
        }],
        tempo: bpm,
        ..warp.clone()
    };
    let timing = Timing {
        start,
        length,
        offset: Offset::Beats(Beats::ZERO),
    };
    Ok(change(clip, audio, warp, new, timing, rate))
}

/// The song's bar line nearest `at`, the earlier of two as near.
fn nearest_bar_line(tempo: &TempoMap, at: Beats) -> Beats {
    let bar = tempo.bar_at(at).bar;
    let (before, after) = (tempo.bar_start(bar), tempo.bar_start(bar + 1));
    match after - at < at - before {
        true => after,
        false => before,
    }
}

/// Makes the audio after the warp's last marker play at `bpm`: the warp's
/// tempo, with one marker, and else by a marker where the file ends, which
/// is `frames` long, or a beat later.
fn tempo_after(warp: &mut Warp, bpm: f64, frames: Option<u64>, rate: u32) {
    let bpm = bpm.clamp(Warp::MIN_TEMPO, Warp::MAX_TEMPO);
    let Some(&last) = warp.markers.last() else {
        return;
    };
    if warp.markers.len() == 1 {
        warp.tempo = bpm;
        return;
    }
    let frames_per_beat = 60.0 * f64::from(rate) / bpm;
    let (frame, beats) = match frames.filter(|&end| end > last.frame) {
        Some(end) => (end, (end - last.frame) as f64 / frames_per_beat),
        None => (last.frame + frames_per_beat.round().max(1.0) as u64, 1.0),
    };
    let beat = last.beat + Beats::from_f64(beats);
    if beat > last.beat {
        warp.markers.push(WarpMarker { frame, beat });
    }
}

/// The markers Warp From Here gives a drifting file after the marker
/// `here`: on the downbeats analysis found after it, and on each beat that
/// those would play more than auto-warp's 10 ms from where it is, counting
/// `factor` beats of the clip's own time to each of the file's.
fn drifting_from(analysis: &Analysis, factor: f64, here: WarpMarker) -> Vec<WarpMarker> {
    let rate = analysis.sample_rate;
    let numbers = analysis.beat_numbers();
    let steady = auto_warp::steady_frames(analysis);
    // The beats after it, by more than a stray's worth, after the marker
    // itself, at its place among the beats.
    let gap = (auto_warp::STRAY_MS * f64::from(rate) / 1000.0) as u64;
    let from = (analysis.beats).partition_point(|&beat| beat <= here.frame + gap);
    let Some(number) = number_at(&analysis.beats, &numbers, here.frame, steady) else {
        return Vec::new();
    };
    let beats: Vec<u64> = std::iter::once(here.frame)
        .chain(analysis.beats[from..].iter().copied())
        .collect();
    let numbers: Vec<f64> = std::iter::once(number)
        .chain(numbers[from..].iter().copied())
        .collect();
    let downbeats = (analysis.downbeats.iter())
        .filter(|&&downbeat| downbeat >= from)
        .map(|&downbeat| downbeat - from + 1);
    let pinned = std::iter::once(0).chain(downbeats).collect();
    let pinned = auto_warp::pin_strays(&beats, &numbers, pinned, steady, rate);
    let mut markers: Vec<WarpMarker> = Vec::with_capacity(pinned.len());
    for i in pinned.into_iter().skip(1) {
        let marker = WarpMarker {
            frame: beats[i],
            beat: here.beat + Beats::from_f64((numbers[i] - number) * factor),
        };
        let before = markers.last().copied().unwrap_or(here);
        if before.frame < marker.frame && before.beat < marker.beat {
            markers.push(marker);
        }
    }
    markers
}

/// The beat number, fractional, of the file's `frame`, among `beats` with
/// the numbers `numbers`: between the beats either side, or before the
/// first and after the last at `steady` frames to the beat.
fn number_at(beats: &[u64], numbers: &[f64], frame: u64, steady: Option<f64>) -> Option<f64> {
    let next = beats.partition_point(|&beat| beat <= frame);
    let frame = frame as f64;
    match (next.checked_sub(1), beats.get(next)) {
        (Some(before), Some(&after)) => {
            let along = (frame - beats[before] as f64) / (after - beats[before]) as f64;
            Some(numbers[before] + along * (numbers[next] - numbers[before]))
        }
        (Some(before), None) => Some(numbers[before] + (frame - beats[before] as f64) / steady?),
        (None, Some(&after)) => Some(numbers[0] - (after as f64 - frame) / steady?),
        (None, None) => None,
    }
}

/// The change giving `clip`, playing `audio` warped by `old`, the warp
/// `new` and the timing `timing`, with its curves moved to stay with its
/// audio, for a file at `rate`.
fn change(
    clip: &Clip,
    audio: &AudioClip,
    old: &Warp,
    new: Warp,
    timing: Timing,
    rate: u32,
) -> Change {
    let curves = (audio.automation.iter())
        .map(|lane| {
            let points = (lane.points.iter())
                .map(|point| {
                    let frame = old.frame_at(point.at.to_f64(), rate);
                    Point::new(Beats::from_f64(new.beat_at(frame, rate)), point.value)
                })
                .collect();
            (lane.param, points)
        })
        .collect();
    let warping = Warping {
        offset: timing.offset,
        warp: Some(new),
    };
    Change {
        clip: clip.clone(),
        warping,
        timing,
        curves,
    }
}

fn apply(tx: &mut Transaction<'_>, change: Change) {
    retime(tx, &change.clip, change.warping, change.timing);
    for (param, points) in change.curves {
        tx.set_lane_points(ParamAddress::Clip(change.clip.id, param), points);
    }
}

#[cfg(test)]
mod tests;

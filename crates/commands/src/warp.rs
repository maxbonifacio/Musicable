//! Warping audio clips: the Sample Editor's commands.
//!
//! The commands here warp an audio clip, so it follows the tempo, edit its
//! warp markers, and set its own settings, such as its gain: the clip their
//! `clip` argument names, or else the one the Sample Editor shows. They
//! place markers in the clip's own time, as the project file does: beat 0
//! is the clip's 1.1.1, and a marker at the clip's offset plays at its
//! start (see [`musicable_model::warp`]). No two markers share a beat, so
//! the commands name markers by their beats. The Sample Editor's mouse
//! gestures run them, and so can scripts and agents. Those without
//! arguments act on the Sample Editor's clip, and its markers selected
//! while it has the focus, with Live's keys.
//!
//! While the Sample Editor has the focus, Delete and Select All act on its
//! markers too (see [`edit`](crate::edit)): the functions for that are here.
//!
//! Turning Warp on warps a clip as [`auto_warp`](crate::auto_warp)'s rule
//! for its file says, where analysis is sure of the file, and else puts
//! one marker at the clip's start, at the tempo
//! [`tempo_guess`](crate::auto_warp::tempo_guess) gives. Warp keeps the
//! stretch of the file a clip plays, as Live does, so turning it on or off,
//! or changing the clip's tempo, changes the clip's length, overwriting
//! what it comes to cover, as trimming does.
//!
//! A warped clip takes its file's transients from analysis once: as it's
//! warped, or, if the file hasn't been analysed yet, when
//! [`take_transients`] finds its analysis. The [`transient`](crate::transient)
//! commands change them, and Insert Warp Marker puts markers at those in
//! the Sample Editor's time selection.

use std::collections::BTreeSet;

use musicable_model::Project;
use musicable_model::automation::Point;
use musicable_model::clip::{AudioClip, Clip, ClipContent, ClipId, Offset, Timing, Warping};
use musicable_model::param::{ClipParam, ParamAddress};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{Onset, Onsets, Origin, Warp};

use crate::arrange::{after_frames, clear};
use crate::auto_warp;
use crate::editor::{Editor, Focus};
use crate::{
    Args, Command, CommandError, CommandId, Context, Document, Param, Registry, Request,
    Transaction, Type, Value, When, panel,
};

/// Shows an audio clip in the Sample Editor.
pub const OPEN: CommandId = CommandId::new("clip.open");
/// Turns an audio clip's Warp on or off: whether it follows the tempo.
pub const SET_WARP: CommandId = CommandId::new("clip.set_warp");
/// Sets an audio clip's gain.
pub const SET_GAIN: CommandId = CommandId::new("clip.set_gain");
/// Selects warp markers in the Sample Editor.
pub const SELECT_MARKERS: CommandId = CommandId::new("warp.select_markers");
/// Selects a stretch of the Sample Editor's clip, with the warp markers and
/// the transients in it.
pub const SELECT_TIME: CommandId = CommandId::new("warp.select_time");
/// Adds warp markers, each pinning the audio that plays where it goes.
pub const INSERT_MARKERS: CommandId = CommandId::new("warp.insert_markers");
/// Moves warp markers, stretching the audio either side.
pub const MOVE_MARKERS: CommandId = CommandId::new("warp.move_markers");
/// Moves the audio under a warp marker, which stays on its beat.
pub const MOVE_AUDIO: CommandId = CommandId::new("warp.move_audio");
/// Deletes warp markers.
pub const DELETE_MARKERS: CommandId = CommandId::new("warp.delete_markers");
/// Sets a warped clip's tempo.
pub const SET_TEMPO: CommandId = CommandId::new("warp.set_tempo");
/// Doubles a warped clip's tempo, so it plays half as fast: Live's ×2.
pub const DOUBLE_TEMPO: CommandId = CommandId::new("warp.double_tempo");
/// Halves a warped clip's tempo, so it plays twice as fast: Live's ÷2.
pub const HALVE_TEMPO: CommandId = CommandId::new("warp.halve_tempo");
/// Moves the selected warp markers a step of the grid earlier.
pub const NUDGE_LEFT: CommandId = CommandId::new("warp.nudge_left");
/// Moves the selected warp markers a step of the grid later.
pub const NUDGE_RIGHT: CommandId = CommandId::new("warp.nudge_right");

pub(crate) const CLIP: Param = Param::optional(
    "clip",
    Type::Clip,
    "The audio clip: the one the Sample Editor shows, without it.",
);
const BEATS: Type = Type::List(&Type::Beats);
/// Frames of a file.
pub(crate) const FRAMES: Type = Type::List(&Type::Int);

type Handler = fn(&mut Context<'_>);

/// The commands that take no arguments, for keys, with their titles, when
/// they apply and what they run.
const COMMANDS: [(CommandId, &str, When, Handler); 4] = [
    (DOUBLE_TEMPO, "Double Clip Tempo", When::WarpedClip, |ctx| {
        scale_shown(ctx, "Double Clip Tempo", 2.0)
    }),
    (HALVE_TEMPO, "Halve Clip Tempo", When::WarpedClip, |ctx| {
        scale_shown(ctx, "Halve Clip Tempo", 0.5)
    }),
    (
        NUDGE_LEFT,
        "Nudge Warp Markers Left",
        When::MarkersSelected,
        |ctx| nudge(ctx, "Nudge Warp Markers Left", -1),
    ),
    (
        NUDGE_RIGHT,
        "Nudge Warp Markers Right",
        When::MarkersSelected,
        |ctx| nudge(ctx, "Nudge Warp Markers Right", 1),
    ),
];

/// Registers the warping commands.
pub fn register(registry: &mut Registry) {
    for (id, title, when, run) in COMMANDS {
        registry.add(Command::new(id, title).when(when), move |ctx, _| {
            run(ctx);
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(OPEN, "Open in Sample Editor").with_params(&[Param::required(
            "clip",
            Type::Clip,
            "An audio clip.",
        )]),
        |ctx, args| {
            let clip = args.get("clip")?;
            let (project, editor) = ctx.document.parts();
            let (c, _) = audio(project, clip)?;
            editor.open_in_sample_editor(c);
            ctx.requests.push(Request::ShowPanel(panel::SAMPLE_EDITOR));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_WARP, "Warp")
            .when(When::SampleEditorClip)
            .with_params(&[
                Param::optional(
                    "on",
                    Type::Bool,
                    "Whether the clip follows the tempo: the other way from now, without it.",
                ),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let warped = audio(ctx.document.project(), clip)?.1.is_warped();
            let on = args.opt("on")?.unwrap_or(!warped);
            if on != warped {
                let name = if on { "Warp" } else { "Unwarp" };
                ctx.document.transact(name, |tx| set_warp(tx, clip, on));
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_GAIN, "Set Clip Gain").with_params(&[
            Param::required(
                "gain",
                Type::Number,
                "The gain, linear: 1 is unity, and it goes up to about 15.85, +24 dB.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let gain: f64 = args.get("gain")?;
            if gain < 0.0 {
                return Err(CommandError::invalid("a gain is 0 or more"));
            }
            ctx.document
                .transact("Set Clip Gain", |tx| tx.set_clip_gain(clip, gain as f32));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT_MARKERS, "Select Warp Markers").with_params(&[
            Param::required(
                "markers",
                BEATS,
                "The markers to select, by their beats in the clip's own time.",
            ),
            CLIP,
            Param::optional(
                "toggle",
                Type::Bool,
                "Adds the markers to the selection, or takes away those already in it, \
                 instead of selecting just them, which takes away the time selected.",
            ),
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let markers: Vec<Beats> = args.get("markers")?;
            let toggle = args.opt("toggle")?.unwrap_or(false);
            let (project, editor) = ctx.document.parts();
            let (c, _, warp) = warped(project, clip)?;
            find_markers(warp, &markers)?;
            editor.open_in_sample_editor(c);
            let shown = &mut editor.sample_editor;
            // Selecting just them takes away the time selected too.
            if !toggle {
                shown.markers.clear();
                shown.time = None;
            }
            for beat in markers {
                if !toggle || !shown.markers.remove(&beat) {
                    shown.markers.insert(beat);
                }
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT_TIME, "Select Time in Sample Editor").with_params(&[
            Param::required(
                "start",
                Type::Beats,
                "Where the stretch starts, in the clip's own time.",
            ),
            Param::required(
                "end",
                Type::Beats,
                "Where it ends. At the start, no time is selected, nor are any markers.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let (start, end): (Beats, Beats) = (args.get("start")?, args.get("end")?);
            let range = BeatRange::new(start.min(end), start.max(end));
            let (project, editor) = ctx.document.parts();
            let (c, _, warp) = warped(project, clip)?;
            editor.open_in_sample_editor(c);
            let shown = &mut editor.sample_editor;
            shown.time = (!range.is_empty()).then_some(range);
            shown.markers = (warp.markers.iter())
                .map(|m| m.beat)
                .filter(|&beat| !range.is_empty() && range.start <= beat && beat <= range.end)
                .collect();
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(INSERT_MARKERS, "Insert Warp Marker")
            .when(When::WarpedClip)
            .with_params(&[
                Param::optional(
                    "at",
                    BEATS,
                    "Where each goes, in the clip's own time, pinning the audio that plays \
                     there.",
                ),
                Param::optional(
                    "frames",
                    FRAMES,
                    "Frames of the file to pin where they play, as on transients. Without \
                     these or `at`, at the transients in the Sample Editor's time selection, \
                     or else at the start marker, where the waveform was clicked last.",
                ),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let at: Option<Vec<Beats>> = args.opt("at")?;
            let frames: Option<Vec<i64>> = args.opt("frames")?;
            let project = ctx.document.project();
            let editor = ctx.document.editor();
            let (c, a, warp) = warped(project, clip)?;
            let rate = editor.sample_rate_of(&a.file);
            let (at, frames) = match (at, frames) {
                (None, None) => match selected_transients(project, editor) {
                    Some((shown, frames)) if shown == clip && !frames.is_empty() => {
                        (Vec::new(), frames)
                    }
                    _ => (vec![clip_position(c, a, editor.start_marker)], Vec::new()),
                },
                (at, frames) => (at.unwrap_or_default(), file_frames(frames)?),
            };
            let mut new = warp.clone();
            let mut added: Vec<Beats> = Vec::new();
            for frame in frames {
                if let Some(i) = new.insert_at_frame(frame, rate) {
                    added.push(new.markers[i].beat);
                }
            }
            added.extend(
                at.into_iter()
                    .filter(|&beat| new.insert(beat, rate).is_some()),
            );
            if !added.is_empty() {
                ctx.document
                    .transact("Insert Warp Marker", |tx| change_warp(tx, clip, new));
                select(ctx, clip, added.iter().copied());
            }
            Ok(Value::List(added.into_iter().map(Value::Beats).collect()))
        },
    );
    registry.add(
        Command::new(MOVE_MARKERS, "Move Warp Markers").with_params(&[
            Param::required(
                "markers",
                BEATS,
                "The markers, by their beats in the clip's own time.",
            ),
            Param::required(
                "by",
                Type::Beats,
                "How far later, or earlier if negative: as far as that goes, short of the \
                 markers either side that stay.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let markers: Vec<Beats> = args.get("markers")?;
            let by: Beats = args.get("by")?;
            let (_, _, warp) = warped(ctx.document.project(), clip)?;
            let indices = find_markers(warp, &markers)?;
            let by = warp.markers_can_move(&indices, by);
            move_markers(ctx, "Move Warp Markers", clip, &indices, by);
            Ok(Value::Beats(by))
        },
    );
    registry.add(
        Command::new(MOVE_AUDIO, "Move Audio Under Warp Marker").with_params(&[
            Param::required(
                "marker",
                Type::Beats,
                "The marker, by its beat in the clip's own time, where it stays.",
            ),
            Param::required(
                "frame",
                Type::Int,
                "The frame of the file that plays there from now on: after the frame of \
                 the marker before it, and before the next's.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let marker: Beats = args.get("marker")?;
            let frame: i64 = args.get("frame")?;
            let (_, _, warp) = warped(ctx.document.project(), clip)?;
            let index = find_markers(warp, &[marker])?[0];
            let mut new = warp.clone();
            let moved = u64::try_from(frame).is_ok_and(|frame| new.move_audio(index, frame));
            if !moved {
                return Err(CommandError::invalid(format!(
                    "frame {frame} isn't between the frames of the markers either side"
                )));
            }
            ctx.document.transact("Move Audio Under Warp Marker", |tx| {
                change_warp(tx, clip, new)
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(DELETE_MARKERS, "Delete Warp Markers")
            .when(When::MarkersSelected)
            .with_params(&[
                Param::optional(
                    "markers",
                    BEATS,
                    "The markers, by their beats in the clip's own time: those selected in \
                     the Sample Editor, without them. The first stays if they're all of them, \
                     as a warped clip has one.",
                ),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let markers = match args.opt::<Vec<Beats>>("markers")? {
                Some(markers) => markers,
                None => selected(ctx).map(|(_, beats)| beats).unwrap_or_default(),
            };
            delete_markers(ctx, "Delete Warp Markers", clip, &markers)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_TEMPO, "Set Clip Tempo").with_params(&[
            Param::required(
                "bpm",
                Type::Number,
                "The tempo of the file, in quarter notes a minute, from 5 to 999: from the \
                 first marker to the last, with more than one.",
            ),
            Param::optional(
                "keep_end",
                Type::Bool,
                "Keeps the clip's end where it is, so its start moves, as stretching it from \
                 its start does: its start stays, without it.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let bpm: f64 = args.get("bpm")?;
            let keep_end = args.opt("keep_end")?.unwrap_or(false);
            if !(Warp::MIN_TEMPO..=Warp::MAX_TEMPO).contains(&bpm) {
                return Err(CommandError::invalid("a clip's tempo goes from 5 to 999"));
            }
            let (_, a, warp) = warped(ctx.document.project(), clip)?;
            let rate = ctx.document.editor().sample_rate_of(&a.file);
            let factor = bpm / warp.clip_tempo(rate);
            scale(ctx, "Set Clip Tempo", clip, factor, keep_end)
        },
    );
}

/// Changes the clip's tempo by `factor`, as a transaction called `name`,
/// keeping its end where it is if `keep_end`, and else its start. The
/// selected markers stay selected as they spread out.
fn scale(
    ctx: &mut Context<'_>,
    name: &str,
    clip: ClipId,
    factor: f64,
    keep_end: bool,
) -> Result<Value, CommandError> {
    let (_, a, _) = warped(ctx.document.project(), clip)?;
    let about = a.offset.beats().unwrap_or(Beats::ZERO);
    let selection = shown_selection(ctx, clip, |beat| spread(beat, about, factor));
    if !(ctx.document).transact(name, |tx| scale_tempo(tx, clip, factor, keep_end)) {
        return Err(CommandError::invalid(
            "that would take the clip's tempo out of range, two markers onto one tick, or \
             its start before the song's",
        ));
    }
    if let Some(selection) = selection {
        ctx.document.editor_mut().sample_editor.markers = selection;
    }
    Ok(Value::None)
}

/// Changes the tempo of the clip the Sample Editor shows by `factor`, as a
/// transaction called `name`.
fn scale_shown(ctx: &mut Context<'_>, name: &str, factor: f64) {
    if let Some(clip) = ctx.document.editor().sample_editor.clip {
        // Out of range, it stays as it is.
        let _ = scale(ctx, name, clip, factor, false);
    }
}

/// Moves the selected markers `steps` steps of the grid, as a transaction
/// called `name`. The grid step is the one where the first of them plays,
/// at the Sample Editor's zoom.
fn nudge(ctx: &mut Context<'_>, name: &str, steps: i64) {
    let Some((clip, beats)) = selected(ctx) else {
        return;
    };
    let project = ctx.document.project();
    let Ok((c, a, warp)) = warped(project, clip) else {
        return;
    };
    let Ok(indices) = find_markers(warp, &beats) else {
        return;
    };
    let Some(&first) = beats.first() else {
        return;
    };
    let editor = ctx.document.editor();
    let at = song_position(c, a, first).max(Beats::ZERO);
    let zoom = editor.sample_editor.viewport.zoom;
    let step = editor.grid.step_at(&project.tempo, at, zoom);
    let by = warp.markers_can_move(&indices, step * steps);
    move_markers(ctx, name, clip, &indices, by);
}

/// Moves the markers at `indices` in the clip `clip`'s warp by `by`, as a
/// transaction called `name`, and moves the selected ones' places in the
/// selection with them. `by` must keep them in order.
fn move_markers(ctx: &mut Context<'_>, name: &str, clip: ClipId, indices: &[usize], by: Beats) {
    if by == Beats::ZERO {
        return;
    }
    let Ok((_, _, warp)) = warped(ctx.document.project(), clip) else {
        return;
    };
    let mut new = warp.clone();
    if !new.move_markers(indices, by) {
        return;
    }
    // Worked out first, as the transaction forgets the beats they leave.
    let moved: BTreeSet<Beats> = indices.iter().map(|&i| warp.markers[i].beat).collect();
    let selection = shown_selection(ctx, clip, |beat| match moved.contains(&beat) {
        true => beat + by,
        false => beat,
    });
    ctx.document.transact(name, |tx| change_warp(tx, clip, new));
    if let Some(selection) = selection {
        ctx.document.editor_mut().sample_editor.markers = selection;
    }
}

/// The Sample Editor's selected markers, if it shows the clip `clip`, each
/// moved to where `to` puts it: for setting once their markers move.
fn shown_selection(
    ctx: &Context<'_>,
    clip: ClipId,
    to: impl Fn(Beats) -> Beats,
) -> Option<BTreeSet<Beats>> {
    let shown = &ctx.document.editor().sample_editor;
    (shown.clip == Some(clip)).then(|| shown.markers.iter().map(|&beat| to(beat)).collect())
}

/// Deletes the markers at `beats` from the clip `clip`'s warp, as a
/// transaction called `name`.
fn delete_markers(
    ctx: &mut Context<'_>,
    name: &str,
    clip: ClipId,
    beats: &[Beats],
) -> Result<(), CommandError> {
    let (_, a, warp) = warped(ctx.document.project(), clip)?;
    let indices = find_markers(warp, beats)?;
    if indices.is_empty() {
        return Ok(());
    }
    let rate = ctx.document.editor().sample_rate_of(&a.file);
    let mut new = warp.clone();
    new.delete(&indices, rate);
    ctx.document.transact(name, |tx| change_warp(tx, clip, new));
    Ok(())
}

/// Deletes the warp markers selected in the Sample Editor: what Delete does
/// while it has the focus.
pub(crate) fn delete_selected(ctx: &mut Context<'_>) {
    if let Some((clip, beats)) = selected(ctx) {
        // The markers are there: pruning keeps the selection to them.
        let _ = delete_markers(ctx, "Delete", clip, &beats);
    }
}

/// Selects all the Sample Editor's warp markers, and the time the clip
/// plays, with its transients: what Select All does while it has the
/// focus.
pub(crate) fn select_all(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    let Some(clip) = editor.sample_editor.clip else {
        return;
    };
    if let Ok((c, a, warp)) = warped(project, clip) {
        let shown = &mut editor.sample_editor;
        shown.markers = warp.markers.iter().map(|m| m.beat).collect();
        let start = clip_position(c, a, c.start);
        shown.time = Some(BeatRange::new(start, start + c.length));
    }
}

/// The Sample Editor's clip and the frames of its transients in its time
/// selection, in order, while it shows a warped clip with one.
pub fn selected_transients(project: &Project, editor: &Editor) -> Option<(ClipId, Vec<u64>)> {
    let shown = &editor.sample_editor;
    let (clip, range) = (shown.clip?, shown.time?);
    let (_, a, warp) = warped(project, clip).ok()?;
    let rate = editor.sample_rate_of(&a.file);
    let frame = |beat: Beats| warp.frame_at(beat.to_f64(), rate);
    let (first, last) = (frame(range.start), frame(range.end));
    let frames = (warp.transients())
        .map(|o| o.frame)
        .filter(|&f| first <= f as f64 && (f as f64) < last)
        .collect();
    Some((clip, frames))
}

/// The transients analysis found in `file`, for a clip to take, if the
/// editor has its analysis.
pub fn found_onsets(editor: &Editor, file: &str) -> Option<Onsets> {
    let analysis = editor.analyses.get(file)?;
    Some(Onsets::new(analysis.onsets.iter().map(|o| Onset {
        frame: o.frame,
        strength: o.strength.clamp(0.0, 1.0),
        percussiveness: o.percussiveness.clamp(0.0, 1.0),
        origin: Origin::Found,
    })))
}

/// Gives each warped clip on the tracks that hasn't taken its transients
/// yet those analysis found in its file, where the editor has its analysis.
/// It's a change the project follows, not a step in the undo history: the
/// app calls this as analyses come in and the project changes, so a clip
/// warped before its file was analysed, or brought back by undo as it was
/// then, takes them. Returns whether any did.
pub fn take_transients(document: &mut Document) -> bool {
    let editor = document.editor();
    let taking: Vec<(ClipId, Warping)> = (document.project().tracks.iter())
        .flat_map(|track| &track.clips)
        .filter_map(|clip| {
            let ClipContent::Audio(audio) = &clip.content else {
                return None;
            };
            let warp = audio.warp.as_ref().filter(|w| w.onsets.is_none())?;
            let warp = Warp {
                onsets: Some(found_onsets(editor, &audio.file)?),
                ..warp.clone()
            };
            let warping = Warping {
                offset: audio.offset,
                warp: Some(warp),
            };
            Some((clip.id, warping))
        })
        .collect();
    if taking.is_empty() {
        return false;
    }
    document.follow("Find Transients", |tx| {
        for (clip, warping) in taking {
            tx.set_clip_warping(clip, warping);
        }
    });
    true
}

/// `frames`, as frames of a file: none without them.
pub(crate) fn file_frames(frames: Option<Vec<i64>>) -> Result<Vec<u64>, CommandError> {
    (frames.unwrap_or_default().into_iter())
        .map(|frame| {
            u64::try_from(frame).map_err(|_| {
                CommandError::invalid(format!("frame {frame} is before the file's start"))
            })
        })
        .collect()
}

/// The song positions the Sample Editor's selected markers play at, from
/// the first to the last, or a beat either side of one on its own, for
/// zooming to them.
pub(crate) fn selected_range(project: &Project, editor: &Editor) -> Option<BeatRange> {
    let shown = &editor.sample_editor;
    let (c, a, _) = warped(project, shown.clip?).ok()?;
    let first = song_position(c, a, *shown.markers.first()?);
    let last = song_position(c, a, *shown.markers.last()?);
    let beat = Beats::new(1);
    Some(match first == last {
        true => BeatRange::new((first - beat).max(Beats::ZERO), last + beat),
        false => BeatRange::new(first, last),
    })
}

/// The clip the Sample Editor shows and its selected markers' beats, in
/// order, while it has the focus and some are selected.
fn selected(ctx: &Context<'_>) -> Option<(ClipId, Vec<Beats>)> {
    let editor = ctx.document.editor();
    let shown = &editor.sample_editor;
    let clip = shown
        .clip
        .filter(|_| editor.focus == Focus::SAMPLE_EDITOR)?;
    (!shown.markers.is_empty()).then(|| (clip, shown.markers.iter().copied().collect()))
}

/// Selects just the markers at `beats` of `clip` in the Sample Editor, and
/// gives it the focus.
fn select(ctx: &mut Context<'_>, clip: ClipId, beats: impl IntoIterator<Item = Beats>) {
    let (project, editor) = ctx.document.parts();
    if let Ok((c, _)) = audio(project, clip) {
        editor.open_in_sample_editor(c);
        editor.sample_editor.markers = beats.into_iter().collect::<BTreeSet<_>>();
    }
}

/// The indices of the markers at `beats` in `warp`, in order, or an error
/// if one of them isn't a marker's.
fn find_markers(warp: &Warp, beats: &[Beats]) -> Result<Vec<usize>, CommandError> {
    let mut indices = Vec::with_capacity(beats.len());
    for &beat in beats {
        let index = warp.marker_at(beat).ok_or_else(|| {
            CommandError::invalid(format!("there's no warp marker at beat {}", beat.to_f64()))
        })?;
        indices.push(index);
    }
    indices.sort_unstable();
    indices.dedup();
    Ok(indices)
}

/// Gives the warped clip `clip` the warp `warp`, keeping its timing.
pub(crate) fn change_warp(tx: &mut Transaction<'_>, clip: ClipId, warp: Warp) {
    let Ok((_, a)) = audio(tx.project(), clip) else {
        return;
    };
    let warping = Warping {
        offset: a.offset,
        warp: Some(warp),
    };
    tx.set_clip_warping(clip, warping);
}

/// Warps or unwarps the audio clip `clip`, keeping the frame of its file
/// that plays at its start, and the stretch of the file it plays: its
/// length changes with that stretch's, overwriting what it comes to cover.
/// Its curves stay with its audio. A warped clip that starts before its
/// file unwarps from where the file starts.
fn set_warp(tx: &mut Transaction<'_>, clip: ClipId, on: bool) {
    let Ok((c, a)) = audio(tx.project(), clip) else {
        return;
    };
    let (c, a) = (c.clone(), a.clone());
    let rate = tx.editor().sample_rate_of(&a.file);
    let tempo = &tx.project().tempo;
    let (warping, timing) = match (a.offset, &a.warp, on) {
        (Offset::Frames(first), None, true) => {
            // The file's frames the clip plays, at the file's own speed.
            let played = tempo.sample_at(c.end(), rate) - tempo.sample_at(c.start, rate);
            let warp = auto_warp::warp_on(tx.project(), tx.editor(), &c, &a);
            let offset = Beats::from_f64(warp.beat_at(first as f64, rate));
            let end = Beats::from_f64(warp.beat_at(first as f64 + played, rate));
            let timing = Timing {
                start: c.start,
                length: at_least_a_tick(end - offset),
                offset: Offset::Beats(offset),
            };
            let warping = Warping {
                offset: timing.offset,
                warp: Some(warp),
            };
            (warping, timing)
        }
        (Offset::Beats(offset), Some(warp), false) => {
            // Where the file's first frame plays, in the song.
            let file_start = c.start - offset + ceil_beats(warp.beat_at(0.0, rate));
            let start = c.start.max(file_start);
            if start >= c.end() {
                return;
            }
            let own = |at: Beats| (offset + (at - c.start)).to_f64();
            let first = warp.frame_at(own(start), rate).round().max(0.0) as u64;
            let last = warp.frame_at(own(c.end()), rate).round().max(0.0) as u64;
            let file = tempo.frame_at(start, rate) - first as i64;
            let end = after_frames(tempo, rate, file, last).max(start + Beats::from_ticks(1));
            let timing = Timing {
                start,
                length: end - start,
                offset: Offset::Frames(first),
            };
            let warping = Warping {
                offset: timing.offset,
                warp: None,
            };
            (warping, timing)
        }
        _ => return,
    };
    // Curves in the clip's own time, which turns from seconds of the file
    // into beats, or back.
    let curves: Vec<(ClipParam, Vec<Point>)> = (a.automation.iter())
        .map(|lane| {
            let points = (lane.points.iter())
                .map(|p| {
                    let own = p.at.to_f64();
                    let at = match &warping.warp {
                        Some(warp) => warp.beat_at(own * f64::from(rate), rate),
                        None => a.frame_at(own, rate) / f64::from(rate),
                    };
                    Point::new(Beats::from_f64(at), p.value)
                })
                .collect();
            (lane.param, points)
        })
        .collect();
    retime(tx, &c, warping, timing);
    for (param, points) in curves {
        tx.set_lane_points(ParamAddress::Clip(clip, param), points);
    }
}

/// Makes the warped clip `clip`'s tempo `factor` times what it is, keeping
/// the frame of its file that plays at its start, and the stretch of the
/// file it plays: its markers spread out from its start by `factor`, and so
/// do its curves, so they stay with its audio. Its start stays where it is,
/// or its end if `keep_end`. Returns `false`, changing nothing, if that
/// takes its tempo out of range, brings two markers onto one tick (see
/// [`Warp::scale`]) or puts its start before the song's.
fn scale_tempo(tx: &mut Transaction<'_>, clip: ClipId, factor: f64, keep_end: bool) -> bool {
    let Ok((c, a, warp)) = warped(tx.project(), clip) else {
        return false;
    };
    let (c, a, warp) = (c.clone(), a.clone(), warp.clone());
    let Offset::Beats(offset) = a.offset else {
        return false;
    };
    let rate = tx.editor().sample_rate_of(&a.file);
    let mut new = warp.clone();
    if !new.scale(factor, offset, rate) {
        return false;
    }
    let last = warp.frame_at((offset + c.length).to_f64(), rate);
    let end = Beats::from_f64(new.beat_at(last, rate));
    let length = at_least_a_tick(end - offset);
    let start = match keep_end {
        true => c.end() - length,
        false => c.start,
    };
    if start < Beats::ZERO {
        return false;
    }
    let timing = Timing {
        start,
        length,
        ..c.timing()
    };
    let curves: Vec<(ClipParam, Vec<Point>)> = (a.automation.iter())
        .map(|lane| {
            let points = (lane.points.iter())
                .map(|p| Point::new(spread(p.at, offset, factor), p.value))
                .collect();
            (lane.param, points)
        })
        .collect();
    let warping = Warping {
        offset: a.offset,
        warp: Some(new),
    };
    retime(tx, &c, warping, timing);
    for (param, points) in curves {
        tx.set_lane_points(ParamAddress::Clip(clip, param), points);
    }
    true
}

/// Gives `clip` the warping `warping` and the timing `timing`, its offset
/// the same, overwriting what it comes to cover on its track.
pub(crate) fn retime(tx: &mut Transaction<'_>, clip: &Clip, warping: Warping, timing: Timing) {
    let Some((track, _)) = tx.project().find_clip(clip.id) else {
        return;
    };
    let track = track.id;
    let keep = BTreeSet::from([clip.id]);
    clear(tx, track, BeatRange::new(timing.start, clip.start), &keep);
    clear(tx, track, BeatRange::new(clip.end(), timing.end()), &keep);
    tx.set_clip_warping(clip.id, warping);
    tx.set_clip_timing(clip.id, timing);
}

/// The song position where `at`, a moment of the warped clip `clip`'s own
/// time, plays.
pub fn song_position(clip: &Clip, audio: &AudioClip, at: Beats) -> Beats {
    clip.start + (at - audio.offset.beats().unwrap_or(Beats::ZERO))
}

/// The moment of the warped clip `clip`'s own time that plays at the song
/// position `at`.
pub fn clip_position(clip: &Clip, audio: &AudioClip, at: Beats) -> Beats {
    at - clip.start + audio.offset.beats().unwrap_or(Beats::ZERO)
}

/// Where `beat` goes as the beats of a warped clip's own time spread out
/// from `about` by `factor`, to the nearest tick, as its markers do (see
/// [`Warp::scale`]).
fn spread(beat: Beats, about: Beats, factor: f64) -> Beats {
    let from = (beat - about).ticks() as f64 * factor;
    about + Beats::from_ticks(from.round() as i64)
}

/// `beats`, rounded up to a tick.
pub(crate) fn ceil_beats(beats: f64) -> Beats {
    Beats::from_ticks((beats * Beats::TICKS_PER_BEAT as f64).ceil() as i64)
}

pub(crate) fn at_least_a_tick(length: Beats) -> Beats {
    length.max(Beats::from_ticks(1))
}

/// The clip the `clip` argument names, or else the one the Sample Editor
/// shows, if it's an audio clip on a track.
pub(crate) fn clip_arg(ctx: &Context<'_>, args: &Args) -> Result<ClipId, CommandError> {
    let clip = match args.opt("clip")? {
        Some(clip) => clip,
        None => ctx
            .document
            .editor()
            .sample_editor
            .clip
            .ok_or_else(|| CommandError::invalid("the Sample Editor shows no clip"))?,
    };
    audio(ctx.document.project(), clip)?;
    Ok(clip)
}

/// The clip with id `id` on one of the project's tracks, and what it
/// plays, if it's an audio clip.
pub(crate) fn audio(project: &Project, id: ClipId) -> Result<(&Clip, &AudioClip), CommandError> {
    let (_, clip) = project
        .find_clip(id)
        .ok_or_else(|| CommandError::invalid(format!("there's no clip {}", id.0)))?;
    match &clip.content {
        ClipContent::Audio(audio) => Ok((clip, audio)),
        ClipContent::Midi(_) => Err(CommandError::invalid(format!(
            "clip {} isn't an audio clip",
            id.0
        ))),
    }
}

/// The clip with id `id`, what it plays and its warp, if it's a warped
/// audio clip on a track.
pub(crate) fn warped(
    project: &Project,
    id: ClipId,
) -> Result<(&Clip, &AudioClip, &Warp), CommandError> {
    let (clip, audio) = audio(project, id)?;
    match &audio.warp {
        Some(warp) => Ok((clip, audio, warp)),
        None => Err(CommandError::invalid(format!("clip {} isn't warped", id.0))),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use musicable_model::TrackKind;
    use musicable_model::warp::WarpMarker;

    use super::*;
    use crate::editor::AudioFile;
    use crate::keymap::{Keymap, Scope};
    use crate::testing::Setup;
    use crate::{TrackId, edit};

    /// 48 kHz: at the song's 120 BPM, 24,000 frames to the beat.
    const RATE: u32 = 48_000;
    /// Four beats at 100 BPM: 2.4 seconds, or 4.8 beats at 120.
    const LOOP: &str = "Drums 100 BPM.wav";
    const LOOP_FRAMES: u64 = 115_200;
    /// Frames to the beat at 100 BPM.
    pub(crate) const AT_100: u64 = 28_800;

    fn beats(beats: f64) -> Beats {
        Beats::from_f64(beats)
    }

    /// A 120 BPM song with Drums, an audio track, with a clip, Loop, that
    /// plays all of `LOOP` from beat 4, unwarped, to 8.8, and a clip,
    /// After, from beat 9 to 13.
    fn song() -> (Setup, TrackId, ClipId) {
        let mut project = Project::new();
        let drums = project.add_track("Drums", TrackKind::Audio);
        let mut add = |name: &str, file: &str, start, end| {
            let range = BeatRange::new(beats(start), beats(end));
            let audio = ClipContent::Audio(AudioClip::new(file));
            let clip = project.new_clip(name, range, audio);
            let id = clip.id;
            project.track_mut(drums).unwrap().clips.push(clip);
            id
        };
        let clip = add("Loop", LOOP, 4.0, 8.8);
        add("After", "after.wav", 9.0, 13.0);
        let mut setup = Setup::new(project);
        let file = AudioFile {
            sample_rate: RATE,
            frames: LOOP_FRAMES,
            channels: 2,
            tempo: None,
        };
        setup.document.editor_mut().files.insert(LOOP.into(), file);
        (setup, drums, clip)
    }

    /// The song with Loop warped, in the Sample Editor.
    fn warped_song() -> (Setup, TrackId, ClipId) {
        let (mut setup, drums, clip) = song();
        open(&mut setup, clip);
        setup.run(&SET_WARP);
        setup.committed_names();
        (setup, drums, clip)
    }

    fn open(setup: &mut Setup, clip: ClipId) {
        setup
            .run_with(&OPEN, Args::new().with("clip", clip))
            .unwrap();
    }

    /// Each clip on `track`, as (name, start, end) in beats, in order.
    fn clips(setup: &Setup, track: TrackId) -> Vec<(String, f64, f64)> {
        let track = setup.document.project().track(track).unwrap();
        let mut clips: Vec<_> = (track.clips.iter())
            .map(|c| (c.name.clone(), c.start.to_f64(), c.end().to_f64()))
            .collect();
        clips.sort_by(|a, b| a.1.total_cmp(&b.1));
        clips
    }

    fn row(name: &str, start: f64, end: f64) -> (String, f64, f64) {
        (name.to_owned(), start, end)
    }

    fn audio_of(setup: &Setup, clip: ClipId) -> AudioClip {
        audio(setup.document.project(), clip).unwrap().1.clone()
    }

    fn warp_of(setup: &Setup, clip: ClipId) -> Warp {
        audio_of(setup, clip).warp.unwrap()
    }

    /// The clip's markers, as (frame, beat).
    fn markers(setup: &Setup, clip: ClipId) -> Vec<(u64, f64)> {
        let warp = warp_of(setup, clip);
        (warp.markers.iter())
            .map(|m| (m.frame, m.beat.to_f64()))
            .collect()
    }

    /// The beats of the Sample Editor's selected markers.
    fn selected(setup: &Setup) -> Vec<f64> {
        let shown = &setup.document.editor().sample_editor;
        shown.markers.iter().map(|beat| beat.to_f64()).collect()
    }

    /// The clip's gain curve, as (time, value) in its own time.
    fn curve(setup: &Setup, clip: ClipId) -> Vec<(f64, f64)> {
        let audio = audio_of(setup, clip);
        let lane = audio.lane(ClipParam::Gain).unwrap();
        (lane.points.iter())
            .map(|p| (p.at.to_f64(), p.value))
            .collect()
    }

    fn list(beats: &[f64]) -> Vec<Beats> {
        beats.iter().map(|&b| Beats::from_f64(b)).collect()
    }

    /// Where analysis finds the loop's transients: on each of its beats at
    /// 100 BPM, and halfway through the first.
    pub(crate) const FOUND: [u64; 5] = [0, AT_100 / 2, AT_100, 2 * AT_100, 3 * AT_100];

    /// Gives the editor the loop's analysis, with its transients at
    /// [`FOUND`].
    pub(crate) fn analysed(setup: &mut Setup) {
        let onsets = FOUND.map(|frame| musicable_analysis::Onset {
            frame,
            strength: 0.5,
            percussiveness: 1.0,
        });
        let analysis =
            musicable_analysis::Analysis::with_onsets(RATE, LOOP_FRAMES, onsets.to_vec());
        let analyses = &mut setup.document.editor_mut().analyses;
        analyses.insert(LOOP.into(), std::sync::Arc::new(analysis));
    }

    /// The frames of the clip's transients in sight.
    pub(crate) fn transients(setup: &Setup, clip: ClipId) -> Vec<u64> {
        warp_of(setup, clip).transients().map(|o| o.frame).collect()
    }

    /// The song, its loop analysed, warped in the Sample Editor, with the
    /// transients analysis found.
    pub(crate) fn analysed_song() -> (Setup, ClipId) {
        let (mut setup, _, clip) = song();
        analysed(&mut setup);
        open(&mut setup, clip);
        setup.run(&SET_WARP);
        setup.committed_names();
        (setup, clip)
    }

    /// Arguments that select a stretch of the clip's own time.
    pub(crate) fn time(start: f64, end: f64) -> Args {
        Args::new()
            .with("start", beats(start))
            .with("end", beats(end))
    }

    #[test]
    fn warped_clips_take_their_files_transients_from_analysis_once() {
        let (setup, clip) = analysed_song();
        assert_eq!(transients(&setup, clip), FOUND);
        // Warped before its file's analysed, it takes them when the
        // analysis comes, as a change the project follows.
        let (mut setup, _, clip) = warped_song();
        assert_eq!(warp_of(&setup, clip).onsets, None);
        assert!(!take_transients(&mut setup.document), "no analysis yet");
        analysed(&mut setup);
        assert!(take_transients(&mut setup.document));
        assert_eq!(transients(&setup, clip), FOUND);
        assert_eq!(setup.committed_names(), ["Find Transients"]);
        assert_eq!(setup.document.undo_name(), Some("Warp"));
        assert!(!take_transients(&mut setup.document), "once");
        // Undo and redo bring the clip back as Warp left it, and it takes
        // them again.
        setup.run(&edit::UNDO);
        setup.run(&edit::REDO);
        assert_eq!(warp_of(&setup, clip).onsets, None);
        assert!(take_transients(&mut setup.document));
        assert_eq!(transients(&setup, clip), FOUND);
    }

    #[test]
    fn the_time_selected_picks_the_markers_and_transients_in_it() {
        let (mut setup, clip) = analysed_song();
        // From just before the clip's beat 1 to just after its beat 2.
        setup.run_with(&SELECT_TIME, time(0.9, 2.1)).unwrap();
        let picked = |setup: &Setup| {
            let document = &setup.document;
            selected_transients(document.project(), document.editor())
        };
        assert_eq!(picked(&setup), Some((clip, vec![AT_100, 2 * AT_100])));
        // Insert Warp Marker puts markers on them, and selects them.
        let inserted = setup.run_with(&INSERT_MARKERS, Args::new()).unwrap();
        let on = |beats: &[i64]| beats.iter().map(|&b| Value::Beats(Beats::new(b))).collect();
        assert_eq!(inserted, Value::List(on(&[1, 2])));
        let at = |beat| (beat * AT_100, beat as f64);
        assert_eq!(markers(&setup, clip), [at(0), at(1), at(2)]);
        assert_eq!(selected(&setup), [1.0, 2.0]);
        // Selecting time selects the markers in it, and an empty stretch
        // selects nothing. A transient at its start is in it.
        setup.run_with(&SELECT_TIME, time(0.5, 1.5)).unwrap();
        assert_eq!(selected(&setup), [1.0]);
        assert_eq!(picked(&setup), Some((clip, vec![AT_100 / 2, AT_100])));
        setup.run_with(&SELECT_TIME, time(1.0, 1.0)).unwrap();
        assert_eq!(setup.document.editor().sample_editor.time, None);
        assert_eq!(selected(&setup), [] as [f64; 0]);
        // Without a time selection, at the start marker. Song beat 7.5 is
        // the clip's 3.5.
        setup.document.editor_mut().start_marker = beats(7.5);
        let inserted = setup.run_with(&INSERT_MARKERS, Args::new()).unwrap();
        assert_eq!(inserted, Value::List(vec![Value::Beats(beats(3.5))]));
        // On frames, wherever they play.
        let frames = Args::new().with("frames", vec![(AT_100 / 2) as i64, 0]);
        let inserted = setup.run_with(&INSERT_MARKERS, frames).unwrap();
        assert_eq!(inserted, Value::List(vec![Value::Beats(beats(0.5))]));
        // Select All selects the time the clip plays, so every transient,
        // and every marker.
        setup.run(&edit::SELECT_ALL);
        assert_eq!(selected(&setup), [0.0, 0.5, 1.0, 2.0, 3.5]);
        assert_eq!(picked(&setup), Some((clip, FOUND.to_vec())));
        // Selecting markers takes away the time selected, unless it adds
        // them to the selection.
        setup.run_with(&SELECT_TIME, time(0.9, 2.1)).unwrap();
        let first = || Args::new().with("markers", vec![beats(0.0)]);
        setup
            .run_with(&SELECT_MARKERS, first().with("toggle", true))
            .unwrap();
        assert_eq!(selected(&setup), [0.0, 1.0, 2.0]);
        assert!(setup.document.editor().sample_editor.time.is_some());
        setup.run_with(&SELECT_MARKERS, first()).unwrap();
        assert_eq!(setup.document.editor().sample_editor.time, None);
    }

    #[test]
    fn warp_keeps_the_stretch_of_the_file_the_clip_plays() {
        let (mut setup, drums, clip) = song();
        // Warp is for the clip the Sample Editor shows.
        assert!(!setup.applies(&SET_WARP));
        open(&mut setup, clip);
        assert_eq!(setup.requests, [Request::ShowPanel(panel::SAMPLE_EDITOR)]);
        assert_eq!(setup.document.editor().focus, Focus::SAMPLE_EDITOR);
        assert!(setup.applies(&SET_WARP) && !setup.applies(&INSERT_MARKERS));
        setup.run(&SET_WARP);
        // At the 100 BPM its name gives, the loop is four beats long, from
        // the marker pinning its first frame to the clip's start.
        let audio = audio_of(&setup, clip);
        assert_eq!(audio.offset, Offset::Beats(Beats::ZERO));
        let warp = audio.warp.unwrap();
        let first = WarpMarker {
            frame: 0,
            beat: Beats::ZERO,
        };
        assert_eq!((warp.markers, warp.tempo), (vec![first], 100.0));
        let after = row("After", 9.0, 13.0);
        assert_eq!(clips(&setup, drums), [row("Loop", 4.0, 8.0), after.clone()]);
        assert!(setup.applies(&INSERT_MARKERS));
        // Unwarped, it plays at the file's own speed again.
        setup.run(&SET_WARP);
        let audio = audio_of(&setup, clip);
        assert_eq!((audio.offset, audio.warp), (Offset::Frames(0), None));
        assert_eq!(clips(&setup, drums), [row("Loop", 4.0, 8.8), after]);
        assert_eq!(setup.committed_names(), ["Warp", "Unwarp"]);
        // Asked to be as it is, it does nothing.
        let off = Args::new().with("on", false).with("clip", clip);
        setup.run_with(&SET_WARP, off).unwrap();
        assert_eq!(setup.committed_names(), Vec::<String>::new());
    }

    #[test]
    fn a_clip_played_from_further_in_keeps_its_first_frame() {
        let (mut setup, drums, clip) = song();
        // Loop from its second beat at 100 BPM, for 1.2 seconds: 2.4 beats
        // at 120.
        let timing = Timing {
            start: Beats::new(4),
            length: beats(2.4),
            offset: Offset::Frames(AT_100),
        };
        setup
            .document
            .transact("Trim", |tx| tx.set_clip_timing(clip, timing));
        open(&mut setup, clip);
        setup.run(&SET_WARP);
        assert_eq!(markers(&setup, clip), [(AT_100, 0.0)]);
        assert_eq!(clips(&setup, drums)[0], row("Loop", 4.0, 6.0));
        setup.run(&SET_WARP);
        assert_eq!(audio_of(&setup, clip).offset, Offset::Frames(AT_100));
        assert_eq!(clips(&setup, drums)[0], row("Loop", 4.0, 6.4));
    }

    #[test]
    fn a_clip_that_starts_before_its_file_unwarps_from_where_the_file_starts() {
        let (mut setup, drums, clip) = warped_song();
        // The file's first frame on the clip's second beat, so it starts
        // with a beat of silence.
        let mut warp = warp_of(&setup, clip);
        warp.markers[0].beat = Beats::new(1);
        let warping = Warping {
            offset: Offset::Beats(Beats::ZERO),
            warp: Some(warp),
        };
        setup
            .document
            .transact("Later", |tx| tx.set_clip_warping(clip, warping));
        setup.run(&SET_WARP);
        // The three beats of the file it played at 100 BPM are 3.6 at 120.
        assert_eq!(audio_of(&setup, clip).offset, Offset::Frames(0));
        assert_eq!(clips(&setup, drums)[0], row("Loop", 5.0, 8.6));
    }

    #[test]
    fn warp_starts_at_the_loop_data_s_tempo_then_the_name_s_then_the_song_s() {
        let project = Project::new();
        let tempo = |file: &str, frames: u64, looped: Option<f64>| {
            let mut editor = Editor::default();
            let header = AudioFile {
                sample_rate: RATE,
                frames,
                channels: 2,
                tempo: looped,
            };
            editor.files.insert(file.into(), header);
            let audio = AudioClip::new(file);
            let clip = Clip {
                id: ClipId(1),
                name: "Loop".into(),
                start: Beats::ZERO,
                length: Beats::new(4),
                content: ClipContent::Audio(audio.clone()),
            };
            auto_warp::tempo_guess(&project, &editor, &clip, &audio)
        };
        // The loop data's, if it's in range.
        assert_eq!(tempo(LOOP, LOOP_FRAMES, Some(97.5)), 97.5);
        assert_eq!(tempo(LOOP, LOOP_FRAMES, Some(2_000.0)), 100.0);
        // Next to "bpm", the name's, however long the file is.
        assert_eq!(tempo(LOOP, 100_000, None), 100.0);
        // A number on its own, if the file is a whole number of beats at
        // it, give or take 5 ms: four beats at 128 BPM are 1.875 seconds,
        // 90,000 frames.
        let bass = "Bass_Am_128.wav";
        assert_eq!(tempo(bass, 90_000, None), 128.0);
        assert_eq!(tempo(bass, 90_200, None), 128.0);
        assert_eq!(tempo(bass, 90_300, None), 120.0);
        assert_eq!(tempo(bass, 100_000, None), 120.0);
        // Otherwise the song's.
        assert_eq!(tempo("Vocal take.wav", 90_000, None), 120.0);
    }

    #[test]
    fn curves_stay_with_the_audio() {
        let (mut setup, _, clip) = song();
        // Unwarped, a curve is in seconds of the file: 1.2 seconds in is
        // halfway through the loop.
        let gain = ParamAddress::Clip(clip, ClipParam::Gain);
        let points = vec![Point::new(Beats::ZERO, 0.5), Point::new(beats(1.2), 1.0)];
        setup
            .document
            .transact("Curve", |tx| tx.set_lane_points(gain, points));
        open(&mut setup, clip);
        setup.run(&SET_WARP);
        // Warped, it's in beats: halfway is beat 2 at 100 BPM.
        assert_eq!(curve(&setup, clip), [(0.0, 0.5), (2.0, 1.0)]);
        // Changing the tempo spreads it out with the audio.
        setup.run(&DOUBLE_TEMPO);
        assert_eq!(curve(&setup, clip), [(0.0, 0.5), (4.0, 1.0)]);
        setup.run(&HALVE_TEMPO);
        setup.run(&SET_WARP);
        assert_eq!(curve(&setup, clip), [(0.0, 0.5), (1.2, 1.0)]);
    }

    #[test]
    fn changing_the_tempo_keeps_the_stretch_of_the_file_the_clip_plays() {
        let (mut setup, drums, clip) = warped_song();
        // ×2: the loop plays half as fast, over eight beats, and the start
        // of After goes under it.
        setup.run(&DOUBLE_TEMPO);
        assert_eq!(warp_of(&setup, clip).tempo, 200.0);
        let after = row("After", 12.0, 13.0);
        assert_eq!(
            clips(&setup, drums),
            [row("Loop", 4.0, 12.0), after.clone()]
        );
        setup.run(&HALVE_TEMPO);
        assert_eq!(warp_of(&setup, clip).tempo, 100.0);
        assert_eq!(clips(&setup, drums), [row("Loop", 4.0, 8.0), after]);
        assert_eq!(
            setup.committed_names(),
            ["Double Clip Tempo", "Halve Clip Tempo"]
        );
        // With more markers, they spread out from the clip's start, as do
        // the selected ones.
        let at = Args::new().with("at", list(&[1.0, 2.0]));
        setup.run_with(&INSERT_MARKERS, at).unwrap();
        let bpm = |bpm: f64| Args::new().with("bpm", bpm);
        setup.run_with(&SET_TEMPO, bpm(50.0)).unwrap();
        let expected = [(0, 0.0), (AT_100, 0.5), (2 * AT_100, 1.0)];
        assert_eq!(markers(&setup, clip), expected);
        assert_eq!(selected(&setup), [0.5, 1.0]);
        assert_eq!(clips(&setup, drums)[0], row("Loop", 4.0, 6.0));
        // Not past 999 BPM, nor under 5.
        assert!(setup.run_with(&SET_TEMPO, bpm(1_000.0)).is_err());
        assert!(setup.run_with(&SET_TEMPO, bpm(4.0)).is_err());
        setup.committed_names();
        setup.run_with(&SET_TEMPO, bpm(999.0)).unwrap();
        setup.run(&DOUBLE_TEMPO);
        // Off by what the markers' ticks round off.
        let tempo = warp_of(&setup, clip).clip_tempo(RATE);
        assert!((tempo - 999.0).abs() < 1e-3, "{tempo}");
        assert_eq!(setup.committed_names(), ["Set Clip Tempo"]);
    }

    #[test]
    fn stretching_from_the_start_keeps_the_end() {
        let (mut setup, drums, clip) = warped_song();
        let keeping_the_end = |bpm: f64| Args::new().with("bpm", bpm).with("keep_end", true);
        // At 50 BPM, the loop's four beats play over two, ending where they
        // did, from the same frame of the file.
        setup.run_with(&SET_TEMPO, keeping_the_end(50.0)).unwrap();
        let after = row("After", 9.0, 13.0);
        assert_eq!(clips(&setup, drums), [row("Loop", 6.0, 8.0), after.clone()]);
        assert_eq!(audio_of(&setup, clip).offset, Offset::Beats(Beats::ZERO));
        // Not from before the song's start.
        assert!(setup.run_with(&SET_TEMPO, keeping_the_end(250.0)).is_err());
        setup.run_with(&SET_TEMPO, keeping_the_end(150.0)).unwrap();
        assert_eq!(clips(&setup, drums), [row("Loop", 2.0, 8.0), after]);
        assert_eq!(
            setup.committed_names(),
            ["Set Clip Tempo", "Set Clip Tempo"]
        );
    }

    #[test]
    fn markers_go_in_at_the_start_marker_and_stretch_the_audio_as_they_move() {
        let (mut setup, _, clip) = warped_song();
        // Song beat 5 is the clip's beat 1, where frame 28,800 plays.
        setup.document.editor_mut().start_marker = Beats::new(5);
        let inserted = setup.run_with(&INSERT_MARKERS, Args::new()).unwrap();
        assert_eq!(inserted, Value::List(vec![Value::Beats(Beats::new(1))]));
        assert_eq!(markers(&setup, clip), [(0, 0.0), (AT_100, 1.0)]);
        assert_eq!(selected(&setup), [1.0]);
        // Not where there's one already.
        let at = Args::new().with("at", list(&[1.0, 3.0]));
        let inserted = setup.run_with(&INSERT_MARKERS, at).unwrap();
        assert_eq!(inserted, Value::List(vec![Value::Beats(Beats::new(3))]));
        let three = (3 * AT_100, 3.0);
        assert_eq!(markers(&setup, clip), [(0, 0.0), (AT_100, 1.0), three]);
        assert_eq!(selected(&setup), [3.0]);

        // The one at beat 1 moves half a beat later, its audio with it.
        let select = |beats: &[f64]| Args::new().with("markers", list(beats));
        setup.run_with(&SELECT_MARKERS, select(&[1.0])).unwrap();
        let by = |markers: &[f64], by: f64| select(markers).with("by", beats(by));
        let moved = setup.run_with(&MOVE_MARKERS, by(&[1.0], 0.5)).unwrap();
        assert_eq!(moved, Value::Beats(beats(0.5)));
        assert_eq!(markers(&setup, clip), [(0, 0.0), (AT_100, 1.5), three]);
        assert_eq!(selected(&setup), [1.5]);
        // As far as goes, short of the next marker.
        let moved = setup.run_with(&MOVE_MARKERS, by(&[1.5], 10.0)).unwrap();
        let tick = Beats::from_ticks(1);
        let short = Beats::new(3) - tick;
        assert_eq!(moved, Value::Beats(short - beats(1.5)));
        assert_eq!(markers(&setup, clip)[1], (AT_100, short.to_f64()));
        assert_eq!(selected(&setup), [short.to_f64()]);
        // A marker that isn't there.
        assert!(setup.run_with(&MOVE_MARKERS, by(&[1.5], 1.0)).is_err());

        // The audio under a marker moves between its neighbours' frames.
        let under = |frame: i64| {
            let marker = Args::new().with("marker", Beats::new(3));
            marker.with("frame", frame)
        };
        setup.run_with(&MOVE_AUDIO, under(100_000)).unwrap();
        assert_eq!(markers(&setup, clip)[2], (100_000, 3.0));
        assert!(setup.run_with(&MOVE_AUDIO, under(AT_100 as i64)).is_err());
        assert!(setup.run_with(&MOVE_AUDIO, under(-1)).is_err());
        assert_eq!(
            setup.committed_names(),
            [
                "Insert Warp Marker",
                "Insert Warp Marker",
                "Move Warp Markers",
                "Move Warp Markers",
                "Move Audio Under Warp Marker"
            ]
        );
    }

    #[test]
    fn delete_and_select_all_act_on_the_markers_while_the_sample_editor_has_the_focus() {
        let (mut setup, drums, clip) = warped_song();
        let at = Args::new().with("at", list(&[1.0, 2.0, 3.0]));
        setup.run_with(&INSERT_MARKERS, at).unwrap();
        let toggle = |beats: &[f64]| {
            let markers = Args::new().with("markers", list(beats));
            markers.with("toggle", true)
        };
        setup
            .run_with(&SELECT_MARKERS, toggle(&[2.0, 0.0]))
            .unwrap();
        assert_eq!(selected(&setup), [0.0, 1.0, 3.0]);
        assert!(setup.applies(&DELETE_MARKERS));
        setup.run(&edit::DELETE);
        // The marker left plays on at the tempo of the stretch it started.
        assert_eq!(markers(&setup, clip), [(2 * AT_100, 2.0)]);
        assert_eq!(warp_of(&setup, clip).tempo, 100.0);
        assert!(selected(&setup).is_empty() && !setup.applies(&DELETE_MARKERS));
        // Deleting them all keeps one, as a warped clip has one.
        setup.run(&edit::SELECT_ALL);
        assert_eq!(selected(&setup), [2.0]);
        setup.run(&edit::DELETE);
        assert_eq!(markers(&setup, clip), [(2 * AT_100, 2.0)]);
        // The arrangement's clips stay.
        assert!(setup.document.editor().selection.clips.is_empty());
        assert_eq!(clips(&setup, drums).len(), 2);
        // Without the focus, Select All is the arrangement's again.
        setup.document.editor_mut().focus = Focus::ARRANGEMENT;
        setup.run(&edit::SELECT_ALL);
        assert_eq!(setup.document.editor().selection.clips.len(), 2);
        let names = setup.committed_names();
        assert_eq!(names, ["Insert Warp Marker", "Delete"]);
        // Picking a beat with no marker is a mistake.
        let missing = Args::new().with("markers", list(&[0.5]));
        let error = setup.run_with(&SELECT_MARKERS, missing).unwrap_err();
        assert!(
            error.to_string().contains("no warp marker at beat 0.5"),
            "{error}"
        );
    }

    #[test]
    fn the_arrow_keys_nudge_the_selected_markers_a_step_of_the_grid() {
        let (mut setup, _, clip) = warped_song();
        let at = Args::new().with("at", list(&[1.0, 2.0]));
        setup.run_with(&INSERT_MARKERS, at).unwrap();
        let editor = setup.document.editor();
        let zoom = editor.sample_editor.viewport.zoom;
        let tempo = &setup.document.project().tempo;
        let step = editor.grid.step_at(tempo, Beats::new(5), zoom).to_f64();
        assert!(step > 0.0 && step < 1.0, "{step}");
        setup.run(&NUDGE_RIGHT);
        let moved = [(0, 0.0), (AT_100, 1.0 + step), (2 * AT_100, 2.0 + step)];
        assert_eq!(markers(&setup, clip), moved);
        assert_eq!(selected(&setup), [1.0 + step, 2.0 + step]);
        // As far as goes, short of the first marker.
        for _ in 0..(2.0 / step) as usize + 2 {
            setup.run(&NUDGE_LEFT);
        }
        let tick = Beats::from_ticks(1).to_f64();
        let pressed = markers(&setup, clip);
        assert_eq!(pressed[1], (AT_100, tick));
        assert_eq!(pressed[2].1, tick + 1.0);
        // Without markers selected, the arrows are the arrangement's again.
        setup.document.editor_mut().sample_editor.markers.clear();
        assert!(!setup.applies(&NUDGE_LEFT) && !setup.applies(&NUDGE_RIGHT));
    }

    #[test]
    fn gain_goes_from_silence_to_plus_24_db() {
        let (mut setup, _, clip) = song();
        open(&mut setup, clip);
        let gain = |gain: f64| Args::new().with("gain", gain);
        setup.run_with(&SET_GAIN, gain(0.5)).unwrap();
        assert_eq!(audio_of(&setup, clip).gain, 0.5);
        setup.run_with(&SET_GAIN, gain(100.0)).unwrap();
        let loudest = audio_of(&setup, clip).gain;
        assert!((f64::from(loudest) - 10f64.powf(24.0 / 20.0)).abs() < 1e-3);
        assert!(setup.run_with(&SET_GAIN, gain(-1.0)).is_err());
        assert_eq!(setup.committed_names(), ["Set Clip Gain", "Set Clip Gain"]);
    }

    #[test]
    fn only_audio_clips_open_in_the_sample_editor() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let midi = ClipContent::Midi(Default::default());
        let clip = project.new_clip("Keys", range, midi);
        let id = clip.id;
        project.track_mut(keys).unwrap().clips.push(clip);
        let mut setup = Setup::new(project);
        let error = (setup.run_with(&OPEN, Args::new().with("clip", id))).unwrap_err();
        assert!(error.to_string().contains("isn't an audio clip"), "{error}");
        let error = setup.run_with(&SET_GAIN, Args::new().with("gain", 1.0));
        assert!(error.unwrap_err().to_string().contains("shows no clip"));
    }

    #[test]
    fn the_sample_editor_has_live_s_keys() {
        let keymap = Keymap::default_keymap();
        let sample_editor = Scope::Panel(panel::SAMPLE_EDITOR);
        let keys = [
            (INSERT_MARKERS, "Ctrl+I"),
            (NUDGE_LEFT, "Left"),
            (NUDGE_RIGHT, "Right"),
            (crate::transient::INSERT, "Ctrl+Shift+I"),
            (crate::transient::DELETE, "Ctrl+Shift+Backspace"),
        ];
        for (id, chord) in keys {
            let chords: Vec<String> = (keymap.chords_in(&id, sample_editor))
                .map(|c| c.to_string())
                .collect();
            assert_eq!(chords, [chord], "{id}");
            let found = keymap.lookup_focused(chord.parse().unwrap(), Focus::SAMPLE_EDITOR);
            assert_eq!(found, Some(&id));
        }
        // Delete and Select All are the edit commands', which act on the
        // markers while it has the focus.
        let delete = keymap.chords_focused(&edit::DELETE, Focus::SAMPLE_EDITOR);
        assert_eq!(delete.len(), 2);
        let all = keymap.chords_focused(&edit::SELECT_ALL, Focus::SAMPLE_EDITOR);
        assert_eq!(all, ["Ctrl+A".parse().unwrap()]);
        let mut registry = Registry::new();
        register(&mut registry);
        for (id, title, when, _) in COMMANDS {
            let command = registry.get(&id).unwrap();
            assert_eq!((command.title.as_str(), command.when), (title, when));
        }
    }
}

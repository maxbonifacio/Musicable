//! A warped clip's transients: the Sample Editor's commands for them.
//!
//! A warped clip keeps its file's transients, where the sounds in it start,
//! as analysis found them (see [`musicable_model::warp::Onsets`]). The
//! commands here add, move and delete them, and go back to analysis's: the
//! clip their `clip` argument names, or else the one the Sample Editor
//! shows. They name transients by their frames in the file, as the project
//! file does. Deleting a transient analysis found hides it, and so does
//! moving it away, so Reset Transients can bring it back. Without
//! arguments, Insert Transient adds one at the start marker and Delete
//! Transients deletes those in the Sample Editor's time selection, with
//! Live's keys.
//!
//! Quantize moves transients towards the grid, as it moves notes (see
//! [`edit::QUANTIZE`](crate::edit::QUANTIZE)): the functions for that are
//! here.

use std::collections::BTreeSet;

use musicable_model::Project;
use musicable_model::clip::{AudioClip, Clip, ClipId};
use musicable_model::time::Beats;
use musicable_model::warp::{Onsets, Warp};

use crate::editor::Editor;
use crate::warp::{
    FRAMES, change_warp, clip_arg, clip_position, file_frames, found_onsets, selected_transients,
    song_position, warped,
};
use crate::{Command, CommandError, CommandId, Context, Param, Registry, Type, Value, When};

/// Adds transients to a warped clip.
pub const INSERT: CommandId = CommandId::new("transient.insert");
/// Moves a transient to another frame of its file.
pub const MOVE: CommandId = CommandId::new("transient.move");
/// Deletes transients.
pub const DELETE: CommandId = CommandId::new("transient.delete");
/// Goes back to the transients analysis found.
pub const RESET: CommandId = CommandId::new("transient.reset");

const CLIP: Param = Param::optional(
    "clip",
    Type::Clip,
    "The warped clip: the one the Sample Editor shows, without it.",
);

/// Registers the transient commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(INSERT, "Insert Transient")
            .when(When::Transients)
            .with_params(&[
                Param::optional(
                    "frames",
                    FRAMES,
                    "The frames of the file to add them at: where the start marker is, \
                     without them.",
                ),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let frames: Option<Vec<i64>> = args.opt("frames")?;
            let project = ctx.document.project();
            let editor = ctx.document.editor();
            let (c, a, warp) = warped(project, clip)?;
            let frames = match frames {
                Some(frames) => file_frames(Some(frames))?,
                None => {
                    let rate = editor.sample_rate_of(&a.file);
                    let own = clip_position(c, a, editor.start_marker);
                    let frame = warp.frame_at(own.to_f64(), rate).round();
                    if frame < 0.0 {
                        return Err(CommandError::invalid(
                            "the start marker is before the clip's file starts",
                        ));
                    }
                    vec![frame as u64]
                }
            };
            let mut onsets = own_onsets(warp, clip)?;
            let added: Vec<u64> = (frames.into_iter())
                .filter(|&frame| onsets.insert(frame))
                .collect();
            if !added.is_empty() {
                change(ctx, "Insert Transient", clip, warp.clone(), onsets);
            }
            Ok(Value::List(
                added.into_iter().map(|f| Value::Int(f as i64)).collect(),
            ))
        },
    );
    registry.add(
        Command::new(MOVE, "Move Transient").with_params(&[
            Param::required(
                "frame",
                Type::Int,
                "The transient, by the frame of the file it's at.",
            ),
            Param::required(
                "to",
                Type::Int,
                "The frame it moves to: between those of the transients either side.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let (frame, to): (i64, i64) = (args.get("frame")?, args.get("to")?);
            let (_, _, warp) = warped(ctx.document.project(), clip)?;
            let mut onsets = own_onsets(warp, clip)?;
            let frames = file_frames(Some(vec![frame, to]))?;
            if !onsets.move_to(frames[0], frames[1]) {
                return Err(CommandError::invalid(format!(
                    "there's no transient at frame {frame}, or frame {to} isn't between the \
                     transients either side of it"
                )));
            }
            change(ctx, "Move Transient", clip, warp.clone(), onsets);
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Transients")
            .when(When::TransientsSelected)
            .with_params(&[
                Param::optional(
                    "frames",
                    FRAMES,
                    "The transients, by their frames in the file: those in the Sample \
                     Editor's time selection, without them.",
                ),
                CLIP,
            ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let frames = match args.opt::<Vec<i64>>("frames")? {
                Some(frames) => file_frames(Some(frames))?,
                None => selected_transients(ctx.document.project(), ctx.document.editor())
                    .filter(|(shown, _)| *shown == clip)
                    .map(|(_, frames)| frames)
                    .unwrap_or_default(),
            };
            let (_, _, warp) = warped(ctx.document.project(), clip)?;
            let mut onsets = own_onsets(warp, clip)?;
            let deleted = onsets.delete(&frames);
            if deleted > 0 {
                change(ctx, "Delete Transients", clip, warp.clone(), onsets);
            }
            Ok(Value::Int(deleted as i64))
        },
    );
    registry.add(
        Command::new(RESET, "Reset Transients")
            .when(When::Transients)
            .with_params(&[CLIP]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let (_, a, warp) = warped(ctx.document.project(), clip)?;
            // Analysis's as it is now, or else the clip's own as found.
            let onsets = match (found_onsets(ctx.document.editor(), &a.file), &warp.onsets) {
                (Some(found), _) => found,
                (None, Some(own)) => {
                    let mut own = own.clone();
                    own.reset();
                    own
                }
                (None, None) => return Err(not_found(clip)),
            };
            change(ctx, "Reset Transients", clip, warp.clone(), onsets);
            Ok(Value::None)
        },
    );
}

/// Whether the clip with id `id` is a warped audio clip with transients in
/// sight, for Quantize to move.
pub(crate) fn has_transients(project: &Project, id: ClipId) -> bool {
    warped(project, id).is_ok_and(|(_, _, warp)| warp.transients().next().is_some())
}

/// The frames of the transients of the warped clip `clip` that Quantize
/// moves, in order: `frames`, which must be its transients', or else those
/// in the Sample Editor's time selection while it shows the clip, or else
/// all those the clip plays.
pub(crate) fn to_quantize(
    project: &Project,
    editor: &Editor,
    clip: ClipId,
    frames: Option<Vec<i64>>,
) -> Result<Vec<u64>, CommandError> {
    let (c, a, warp) = warped(project, clip)?;
    if warp.onsets.is_none() {
        return Err(not_found(clip));
    }
    if let Some(frames) = frames {
        let mut frames = file_frames(Some(frames))?;
        let transients: BTreeSet<u64> = warp.transients().map(|o| o.frame).collect();
        if let Some(frame) = frames.iter().find(|f| !transients.contains(f)) {
            return Err(CommandError::invalid(format!(
                "clip {} has no transient at frame {frame}",
                clip.0
            )));
        }
        frames.sort_unstable();
        frames.dedup();
        return Ok(frames);
    }
    if let Some((shown, frames)) = selected_transients(project, editor)
        && shown == clip
        && !frames.is_empty()
    {
        return Ok(frames);
    }
    Ok(played(c, a, warp, editor.sample_rate_of(&a.file)))
}

/// The frames of the transients the warped clip `clip` plays, from its
/// start to its end, in order. `audio` is what it plays, and `warp` its
/// warp, for a file at `rate`.
pub(crate) fn played(clip: &Clip, audio: &AudioClip, warp: &Warp, rate: u32) -> Vec<u64> {
    let offset = audio.offset.beats().unwrap_or(Beats::ZERO);
    let first = warp.frame_at(offset.to_f64(), rate);
    let last = warp.frame_at((offset + clip.length).to_f64(), rate);
    (warp.transients())
        .map(|o| o.frame)
        .filter(|&f| first <= f as f64 && (f as f64) < last)
        .collect()
}

/// The warp of the warped clip `clip` with its transients at `frames`
/// moved towards the nearest lines of the grid at `zoom`, by `amount` of
/// the way: what Quantize does to audio. Each gets a warp marker where it
/// plays, or keeps the one on it, and the marker moves, stretching the
/// audio either side. The transient nearest its line goes first, and a
/// marker in its way that isn't on a transient goes. One that would then
/// come onto or past a line another took, or past a marker on a transient,
/// stays where it plays. `None` if nothing changes.
pub(crate) fn quantized(
    project: &Project,
    editor: &Editor,
    clip: ClipId,
    frames: &[u64],
    zoom: f64,
    amount: f64,
) -> Option<Warp> {
    let (c, a, warp) = warped(project, clip).ok()?;
    let rate = editor.sample_rate_of(&a.file);
    let index = |warp: &Warp, frame: u64| warp.markers.binary_search_by_key(&frame, |m| m.frame);
    // Pinned first, which moves nothing.
    let mut new = warp.clone();
    for &frame in frames {
        if index(&new, frame).is_err() {
            new.insert_at_frame(frame, rate);
        }
    }
    // A transient within a tick of a marker beside it has none of its own.
    let mut moves: Vec<(i64, u64, Beats)> = (frames.iter())
        .filter_map(|&frame| {
            let beat = new.markers[index(&new, frame).ok()?].beat;
            let at = song_position(c, a, beat);
            let line = editor.grid.nearest(&project.tempo, at, zoom);
            let away = (line - at).ticks();
            let by = Beats::from_ticks((away as f64 * amount).round() as i64);
            Some((away.abs(), frame, by))
        })
        .collect();
    moves.sort_unstable();
    let transients: BTreeSet<u64> = warp.transients().map(|o| o.frame).collect();
    for (_, frame, by) in moves {
        while by != Beats::ZERO
            && let Ok(i) = index(&new, frame)
        {
            if new.move_markers(&[i], by) {
                break;
            }
            // The marker in the way: the one beside it, the way it moves.
            let beside = if by > Beats::ZERO { i + 1 } else { i - 1 };
            if transients.contains(&new.markers[beside].frame) {
                break;
            }
            new.delete(&[beside], rate);
        }
    }
    (new != *warp).then_some(new)
}

/// The transients of `warp`, the warp of clip `clip`, to change: an error
/// if it hasn't taken them from analysis yet.
fn own_onsets(warp: &Warp, clip: ClipId) -> Result<Onsets, CommandError> {
    warp.onsets.clone().ok_or_else(|| not_found(clip))
}

fn not_found(clip: ClipId) -> CommandError {
    CommandError::invalid(format!(
        "clip {}'s file hasn't been analysed yet, so it has no transients",
        clip.0
    ))
}

/// Gives the clip `clip`, whose warp is `warp`, the transients `onsets`, as
/// a transaction called `name`.
fn change(ctx: &mut Context<'_>, name: &str, clip: ClipId, warp: Warp, onsets: Onsets) {
    let warp = Warp {
        onsets: Some(onsets),
        ..warp
    };
    ctx.document
        .transact(name, |tx| change_warp(tx, clip, warp));
}

#[cfg(test)]
mod tests {
    use musicable_model::time::Beats;

    use super::*;
    use crate::testing::Setup;
    use crate::warp::tests::{AT_100, FOUND, analysed_song, time, transients};
    use crate::{Args, edit, warp};

    fn frames(frames: &[u64]) -> Args {
        let frames: Vec<i64> = frames.iter().map(|&f| f as i64).collect();
        Args::new().with("frames", frames)
    }

    /// The clip's warp markers, as (frame, beat).
    fn markers(setup: &Setup, clip: ClipId) -> Vec<(u64, f64)> {
        let (_, _, warp) = warped(setup.document.project(), clip).unwrap();
        (warp.markers.iter())
            .map(|m| (m.frame, m.beat.to_f64()))
            .collect()
    }

    /// The song with the loop's transient on its second beat a tenth of a
    /// beat late, and the one on its fourth a fifth early: at 1.1 and 2.8
    /// in its own time, which plays from beat 4 of the song. At 30 points
    /// a beat, the grid steps by quarter notes, in the Sample Editor and
    /// the arrangement.
    fn loose() -> (Setup, ClipId, [u64; 2]) {
        let (mut setup, clip) = analysed_song();
        let (late, early) = (AT_100 + AT_100 / 10, 3 * AT_100 - AT_100 / 5);
        for (frame, to) in [(AT_100, late), (3 * AT_100, early)] {
            let args = Args::new().with("frame", frame as i64);
            setup.run_with(&MOVE, args.with("to", to as i64)).unwrap();
        }
        let editor = setup.document.editor_mut();
        editor.sample_editor.viewport.zoom = 30.0;
        editor.viewport.zoom = 30.0;
        setup.committed_names();
        (setup, clip, [late, early])
    }

    #[test]
    fn quantize_moves_transients_to_the_grid_with_warp_markers() {
        let (mut setup, clip, [late, early]) = loose();
        let [first, half, _, two, _] = FOUND;
        assert_eq!(markers(&setup, clip), [(0, 0.0)]);
        // The Sample Editor has the focus, and none are selected: all of
        // them. Each gets a marker, the first keeping the one on it, and
        // those off the grid move to it. The one halfway through the first
        // beat is as near the clip's start as its second beat, and goes
        // for the start, which the first took: it stays.
        assert!(setup.applies(&edit::QUANTIZE));
        setup.run(&edit::QUANTIZE);
        assert_eq!(
            markers(&setup, clip),
            [
                (first, 0.0),
                (half, 0.5),
                (late, 1.0),
                (two, 2.0),
                (early, 3.0)
            ]
        );
        assert_eq!(setup.committed_names(), ["Quantize"]);
        setup.run(&edit::UNDO);
        assert_eq!(markers(&setup, clip), [(0, 0.0)]);

        // Half way, those in the time selected.
        setup.run_with(&warp::SELECT_TIME, time(2.5, 3.0)).unwrap();
        let half_way = Args::new().with("amount", 0.5);
        setup.run_with(&edit::QUANTIZE, half_way).unwrap();
        assert_eq!(markers(&setup, clip), [(0, 0.0), (early, 2.9)]);

        // While the arrangement has the focus, the selected clips'.
        setup.run(&edit::UNDO);
        setup.select_clips(&[clip]);
        assert!(setup.applies(&edit::QUANTIZE));
        setup.run(&edit::QUANTIZE);
        assert_eq!(markers(&setup, clip).len(), 5);
    }

    #[test]
    fn a_marker_in_a_transient_s_way_goes_unless_it_s_on_a_transient() {
        let (mut setup, clip, [late, _]) = loose();
        // A marker between the late transient and the line it goes to.
        let at = Args::new().with("at", vec![Beats::from_f64(1.05)]);
        setup.run_with(&warp::INSERT_MARKERS, at).unwrap();
        let between = (AT_100 as f64 * 1.05) as u64;
        assert_eq!(markers(&setup, clip), [(0, 0.0), (between, 1.05)]);
        let quantize = |frames: &[u64]| self::frames(frames).with("clip", clip);
        setup.run_with(&edit::QUANTIZE, quantize(&[late])).unwrap();
        assert_eq!(markers(&setup, clip), [(0, 0.0), (late, 1.0)]);
        // With a transient where the marker is, it's on one: the late
        // transient can't pass it, and stays where it plays.
        setup.run(&edit::UNDO);
        setup.run_with(&INSERT, frames(&[between])).unwrap();
        setup.run_with(&edit::QUANTIZE, quantize(&[late])).unwrap();
        assert_eq!(
            markers(&setup, clip),
            [(0, 0.0), (between, 1.05), (late, 1.1)]
        );
        // Transients only, of audio clips.
        assert!(
            setup
                .run_with(&edit::QUANTIZE, quantize(&[late + 1]))
                .is_err()
        );
        let notes = Args::new().with("clip", clip).with("notes", vec![0]);
        assert!(setup.run_with(&edit::QUANTIZE, notes).is_err());
    }

    fn ints(frames: &[u64]) -> Value {
        Value::List(frames.iter().map(|&f| Value::Int(f as i64)).collect())
    }

    #[test]
    fn transients_go_in_move_and_go_and_reset_brings_back_analysis_s() {
        let (mut setup, clip) = analysed_song();
        // At the start marker: song beat 5.5 is the clip's 1.5.
        setup.document.editor_mut().start_marker = Beats::from_f64(5.5);
        let added = setup.run_with(&INSERT, Args::new()).unwrap();
        let middle = AT_100 * 3 / 2;
        assert_eq!(added, ints(&[middle]));
        let [first, half, one, two, three] = FOUND;
        assert_eq!(
            transients(&setup, clip),
            [first, half, one, middle, two, three]
        );
        // Not where there's one.
        assert_eq!(
            setup.run_with(&INSERT, frames(&[middle])).unwrap(),
            ints(&[])
        );
        // Between the ones either side only.
        let to = |frame: u64, to: u64| {
            let args = Args::new().with("frame", frame as i64);
            args.with("to", to as i64)
        };
        setup.run_with(&MOVE, to(middle, middle + 100)).unwrap();
        assert!(setup.run_with(&MOVE, to(middle + 100, two)).is_err());
        assert!(
            setup.run_with(&MOVE, to(middle, middle + 1)).is_err(),
            "gone"
        );
        let deleted = setup
            .run_with(&DELETE, frames(&[half, middle + 100]))
            .unwrap();
        assert_eq!(deleted, Value::Int(2));
        assert_eq!(transients(&setup, clip), [first, one, two, three]);
        // Without frames, those in the Sample Editor's time selection.
        setup.run_with(&warp::SELECT_TIME, time(0.9, 2.1)).unwrap();
        assert_eq!(setup.run_with(&DELETE, Args::new()).unwrap(), Value::Int(2));
        assert_eq!(transients(&setup, clip), [first, three]);
        setup.run(&RESET);
        assert_eq!(transients(&setup, clip), FOUND);
        assert_eq!(
            setup.committed_names(),
            [
                "Insert Transient",
                "Move Transient",
                "Delete Transients",
                "Delete Transients",
                "Reset Transients"
            ]
        );
        setup.run(&edit::UNDO);
        assert_eq!(transients(&setup, clip), [first, three]);
    }

    #[test]
    fn a_found_transient_moved_away_comes_back_on_reset_without_analysis() {
        let (mut setup, clip) = analysed_song();
        let to = Args::new()
            .with("frame", AT_100 as i64)
            .with("to", AT_100 as i64 + 500);
        setup.run_with(&MOVE, to).unwrap();
        setup.run_with(&INSERT, frames(&[AT_100 * 5 / 2])).unwrap();
        // The analysis can go, as when the file's been changed or moved.
        setup.document.editor_mut().analyses.clear();
        setup.run(&RESET);
        assert_eq!(transients(&setup, clip), FOUND);
    }

    #[test]
    fn a_clip_has_no_transients_to_change_before_analysis() {
        let (mut setup, clip) = analysed_song();
        let mut args = Args::new().with("clip", clip);
        // Unwarped and warped again without the analysis.
        setup.run(&warp::SET_WARP);
        setup.document.editor_mut().analyses.clear();
        setup.run(&warp::SET_WARP);
        for (command, more) in [(INSERT, frames(&[100])), (DELETE, frames(&[0]))] {
            for (key, value) in more.iter() {
                args = args.with(key, value.clone());
            }
            assert!(setup.run_with(&command, args.clone()).is_err(), "{command}");
        }
        assert!(setup.run_with(&RESET, Args::new()).is_err());
    }
}

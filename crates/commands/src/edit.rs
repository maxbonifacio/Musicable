//! The edit commands: undo and redo, the clipboard, and what acts on the
//! arrangement's selection.
//!
//! Undo and redo step through the document's undo history (see
//! [`Document::undo`](crate::Document::undo)). Playing, stopping and moving
//! the playhead aren't transactions, so undo leaves the transport alone.
//!
//! Cut, Copy, Duplicate, Delete and Split act on the selected clips, or else
//! on the time selection, as Live's do. The select commands set what's
//! selected, for the arrangement's mouse gestures, scripts and agents.

use std::collections::BTreeSet;

use musicable_model::clip::{Clip, ClipId};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackId};

use crate::arrange;
use crate::editor::{Clipboard, Editor, Selection, TimeSelection};
use crate::{
    Command, CommandError, CommandId, Context, Named, Param, Registry, Request, Transaction, Type,
    Value, When,
};

/// Takes back the last change to the project.
pub const UNDO: CommandId = CommandId::new("edit.undo");
/// Makes the last change undone again.
pub const REDO: CommandId = CommandId::new("edit.redo");
/// Copies the selection, then deletes it.
pub const CUT: CommandId = CommandId::new("edit.cut");
/// Copies the selected clips, or the parts of clips in the time selection,
/// for Paste.
pub const COPY: CommandId = CommandId::new("edit.copy");
/// Puts what Cut or Copy took at the start marker, from the selected track
/// down, and moves the start marker to its end.
pub const PASTE: CommandId = CommandId::new("edit.paste");
/// Puts a copy of the selection just after it, and selects the copy.
pub const DUPLICATE: CommandId = CommandId::new("edit.duplicate");
/// Deletes the selected clips, or else what the time selection covers, or
/// else the selected tracks.
pub const DELETE: CommandId = CommandId::new("edit.delete");
/// Splits the clips at the time selection's edges, or else the selected
/// clips at the start marker.
pub const SPLIT: CommandId = CommandId::new("edit.split");
/// Selects every clip.
pub const SELECT_ALL: CommandId = CommandId::new("edit.select_all");
/// Asks for a new name for a clip, a track or a locator.
pub const RENAME: CommandId = CommandId::new("edit.rename");
/// Selects clips.
pub const SELECT_CLIPS: CommandId = CommandId::new("edit.select_clips");
/// Selects a stretch of time on some tracks.
pub const SELECT_TIME: CommandId = CommandId::new("edit.select_time");
/// Selects tracks.
pub const SELECT_TRACKS: CommandId = CommandId::new("edit.select_tracks");

type Handler = fn(&mut Context<'_>);

/// The edit commands that take no arguments, with their titles, when they
/// apply and what they run.
const COMMANDS: [(CommandId, &str, When, Handler); 9] = [
    (UNDO, "Undo", When::Always, |ctx| {
        ctx.document.undo();
    }),
    (REDO, "Redo", When::Always, |ctx| {
        ctx.document.redo();
    }),
    (CUT, "Cut", When::ClipsOrTimeSelected, cut),
    (COPY, "Copy", When::ClipsOrTimeSelected, copy),
    (PASTE, "Paste", When::CanPaste, paste),
    (DUPLICATE, "Duplicate", When::ClipsOrTimeSelected, duplicate),
    (DELETE, "Delete", When::SomethingSelected, delete),
    (SPLIT, "Split", When::ClipsOrTimeSelected, split),
    (SELECT_ALL, "Select All", When::Always, select_all),
];

/// Registers the edit commands.
pub fn register(registry: &mut Registry) {
    for (id, title, when, run) in COMMANDS {
        registry.add(Command::new(id, title).when(when), move |ctx, _| {
            run(ctx);
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(RENAME, "Rename")
            .when(When::CanRename)
            .with_params(&[
                Param::optional("clip", Type::Clip, "A clip to name."),
                Param::optional("track", Type::Track, "A track to name."),
                Param::optional("locator", Type::Locator, "A locator to name."),
            ]),
        |ctx, args| {
            let named = match (args.opt("clip")?, args.opt("track")?, args.opt("locator")?) {
                (Some(clip), ..) => Named::Clip(clip),
                (_, Some(track), _) => Named::Track(track),
                (.., Some(locator)) => Named::Locator(locator),
                // The selected clip, if just one is, or else the selected
                // track.
                _ => {
                    let editor = ctx.document.editor();
                    match (editor.selection.clips.first(), editor.selected_track) {
                        (Some(&clip), _) if editor.selection.clips.len() == 1 => Named::Clip(clip),
                        (_, Some(track)) => Named::Track(track),
                        _ => return Err(CommandError::NotNow(RENAME)),
                    }
                }
            };
            ctx.requests.push(Request::Rename(named));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT_CLIPS, "Select Clips").with_params(&[
            Param::required("clips", Type::CLIPS, "The clips to select."),
            Param::optional(
                "toggle",
                Type::Bool,
                "Adds the clips to the selection, or takes away those already in it, \
                 instead of selecting just them.",
            ),
        ]),
        |ctx, args| {
            let clips: Vec<ClipId> = args.get("clips")?;
            let toggle = args.opt("toggle")?.unwrap_or(false);
            select_clips(ctx.document.parts(), &clips, toggle)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT_TIME, "Select Time").with_params(&[
            Param::required("start", Type::Beats, "Where the stretch starts."),
            Param::required(
                "end",
                Type::Beats,
                "Where it ends. At the start, nothing is selected, and the first track \
                 becomes the selected track.",
            ),
            Param::required("tracks", Type::TRACKS, "The tracks it covers."),
        ]),
        |ctx, args| {
            let (start, end): (Beats, Beats) = (args.get("start")?, args.get("end")?);
            let tracks: Vec<TrackId> = args.get("tracks")?;
            let range = BeatRange::new(start.min(end).max(Beats::ZERO), start.max(end));
            select_time(ctx.document.parts(), range, &tracks)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT_TRACKS, "Select Tracks").with_params(&[
            Param::required("tracks", Type::TRACKS, "The tracks to select."),
            Param::optional(
                "toggle",
                Type::Bool,
                "Adds the tracks to the selection, or takes away those already in it, \
                 instead of selecting just them.",
            ),
        ]),
        |ctx, args| {
            let tracks: Vec<TrackId> = args.get("tracks")?;
            let toggle = args.opt("toggle")?.unwrap_or(false);
            select_tracks(ctx.document.parts(), &tracks, toggle)?;
            Ok(Value::None)
        },
    );
}

/// Selects `clips`, or toggles them in the selection, and makes the track
/// of the last one the selected track.
fn select_clips(
    (project, editor): (&Project, &mut Editor),
    clips: &[ClipId],
    toggle: bool,
) -> Result<(), CommandError> {
    let mut tracks = Vec::with_capacity(clips.len());
    for &clip in clips {
        let (track, _) = project
            .find_clip(clip)
            .ok_or_else(|| CommandError::invalid(format!("there's no clip {}", clip.0)))?;
        tracks.push(track.id);
    }
    let selection = &mut editor.selection;
    if !toggle || selection.time.is_some() || !selection.tracks.is_empty() {
        *selection = Selection::default();
    }
    for &clip in clips {
        if !toggle || !selection.clips.remove(&clip) {
            selection.clips.insert(clip);
        }
    }
    if let Some(&track) = tracks.last() {
        editor.selected_track = Some(track);
    }
    Ok(())
}

/// Selects `range` on `tracks`. An empty range selects no time, but still
/// makes one of the tracks the selected track.
fn select_time(
    (project, editor): (&Project, &mut Editor),
    range: BeatRange,
    tracks: &[TrackId],
) -> Result<(), CommandError> {
    let tracks = existing(project, tracks)?;
    if !tracks.contains(&editor.selected_track.unwrap_or(TrackId(u32::MAX))) {
        editor.selected_track = project
            .tracks
            .iter()
            .find(|t| tracks.contains(&t.id))
            .map(|t| t.id)
            .or(editor.selected_track);
    }
    let time = (!range.is_empty() && !tracks.is_empty()).then_some(TimeSelection { range, tracks });
    editor.selection = Selection {
        time,
        ..Selection::default()
    };
    Ok(())
}

/// Selects `tracks`, or toggles them in the selection, and makes the last
/// one still selected the selected track.
fn select_tracks(
    (project, editor): (&Project, &mut Editor),
    tracks: &[TrackId],
    toggle: bool,
) -> Result<(), CommandError> {
    existing(project, tracks)?;
    let selection = &mut editor.selection;
    if !toggle || selection.time.is_some() || !selection.clips.is_empty() {
        *selection = Selection::default();
    }
    for &track in tracks {
        if !toggle || !selection.tracks.remove(&track) {
            selection.tracks.insert(track);
        }
    }
    if let Some(&track) = tracks.iter().rev().find(|t| selection.tracks.contains(t)) {
        editor.selected_track = Some(track);
    }
    Ok(())
}

/// `tracks` as a set, if the project has each of them.
fn existing(project: &Project, tracks: &[TrackId]) -> Result<BTreeSet<TrackId>, CommandError> {
    match tracks.iter().find(|&&id| project.track(id).is_none()) {
        Some(track) => Err(CommandError::invalid(format!(
            "there's no track {}",
            track.0
        ))),
        None => Ok(tracks.iter().copied().collect()),
    }
}

fn select_all(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    let clips = project.tracks.iter().flat_map(|t| &t.clips).map(|c| c.id);
    editor.selection = Selection::clips(clips);
}

/// What the selection holds, as Copy takes it, and the index of the first
/// track it was taken from.
fn taken(project: &Project, editor: &Editor) -> Option<(usize, Clipboard)> {
    let selection = &editor.selection;
    if let Some(time) = &selection.time {
        let rows: Vec<usize> = (0..project.tracks.len())
            .filter(|&index| time.tracks.contains(&project.tracks[index].id))
            .collect();
        let first = *rows.first()?;
        let clips = rows
            .iter()
            .flat_map(|&index| {
                project.tracks[index].clips.iter().filter_map(move |clip| {
                    let part = arrange::crop(project, editor, clip, time.range)?;
                    Some((index - first, part))
                })
            })
            .collect();
        return Some((
            first,
            Clipboard {
                clips,
                range: time.range,
            },
        ));
    }
    let clips: Vec<(usize, Clip)> = project
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(index, track)| {
            track
                .clips
                .iter()
                .filter(|clip| selection.clips.contains(&clip.id))
                .map(move |clip| (index, clip.clone()))
        })
        .collect();
    let first = clips.iter().map(|(index, _)| *index).min()?;
    let start = clips.iter().map(|(_, clip)| clip.start).min()?;
    let end = clips.iter().map(|(_, clip)| clip.end()).max()?;
    let clips = clips
        .into_iter()
        .map(|(index, clip)| (index - first, clip))
        .collect();
    Some((
        first,
        Clipboard {
            clips,
            range: BeatRange::new(start, end),
        },
    ))
}

fn copy(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    if let Some((_, clipboard)) = taken(project, editor) {
        editor.clipboard = Some(clipboard);
    }
}

fn cut(ctx: &mut Context<'_>) {
    copy(ctx);
    remove_selected(ctx, "Cut");
}

/// Copies of `clipboard`'s clips with new ids, `by` later, on the tracks
/// from the one at `first` down, for [`arrange::place`]. Clips for tracks
/// that aren't there, or can't hold them, are left out.
fn copies(
    tx: &mut Transaction<'_>,
    clipboard: &Clipboard,
    first: usize,
    by: Beats,
) -> Vec<(TrackId, Clip)> {
    let mut copies = Vec::with_capacity(clipboard.clips.len());
    for (row, clip) in &clipboard.clips {
        let Some(track) = tx.project().tracks.get(first + row) else {
            continue;
        };
        if !track.holds(&clip.content) {
            continue;
        }
        let track = track.id;
        let copy = Clip {
            id: tx.new_clip_id(),
            start: clip.start + by,
            ..clip.clone()
        };
        copies.push((track, copy));
    }
    copies
}

fn paste(ctx: &mut Context<'_>) {
    let editor = ctx.document.editor();
    let project = ctx.document.project();
    let Some(clipboard) = editor.clipboard.clone() else {
        return;
    };
    let at = editor
        .selection
        .time
        .as_ref()
        .map_or(editor.start_marker, |time| time.range.start);
    // From the selected track down, or else from the first track that can
    // hold the first clip.
    let first = editor
        .selected_track
        .and_then(|id| project.track_index(id))
        .or_else(|| {
            let (_, clip) = clipboard.clips.first()?;
            project.tracks.iter().position(|t| t.holds(&clip.content))
        });
    let Some(first) = first else {
        return;
    };
    let by = at - clipboard.range.start;
    let pasted = ctx.document.transact("Paste", |tx| {
        let clips = copies(tx, &clipboard, first, by);
        arrange::place(tx, clips)
    });
    let editor = ctx.document.editor_mut();
    editor.selection = Selection::clips(pasted);
    editor.start_marker = at + clipboard.range.len();
}

fn duplicate(ctx: &mut Context<'_>) {
    let Some((first, clipboard)) = taken(ctx.document.project(), ctx.document.editor()) else {
        return;
    };
    let by = clipboard.range.len();
    let copies = ctx.document.transact("Duplicate", |tx| {
        let clips = copies(tx, &clipboard, first, by);
        arrange::place(tx, clips)
    });
    let selection = &mut ctx.document.editor_mut().selection;
    match &mut selection.time {
        Some(time) => {
            time.range = BeatRange::new(time.range.start + by, time.range.end + by);
        }
        None => *selection = Selection::clips(copies),
    }
}

fn delete(ctx: &mut Context<'_>) {
    let selection = &ctx.document.editor().selection;
    if !selection.clips.is_empty() || selection.time.is_some() {
        remove_selected(ctx, "Delete");
        return;
    }
    let tracks: Vec<TrackId> = selection.tracks.iter().copied().collect();
    crate::track::delete(ctx.document, &tracks);
}

/// Removes the selected clips, or else what the time selection covers, as
/// one transaction called `name`.
fn remove_selected(ctx: &mut Context<'_>, name: &str) {
    let selection = ctx.document.editor().selection.clone();
    ctx.document.transact(name, |tx| {
        if !selection.clips.is_empty() {
            for &clip in &selection.clips {
                tx.remove_clip(clip);
            }
        } else if let Some(time) = &selection.time {
            for &track in &time.tracks {
                arrange::clear(tx, track, time.range, &BTreeSet::new());
            }
        }
    });
}

fn split(ctx: &mut Context<'_>) {
    let editor = ctx.document.editor();
    let selection = editor.selection.clone();
    let at = editor.start_marker;
    ctx.document.transact("Split", |tx| match &selection.time {
        Some(time) => {
            for &track in &time.tracks {
                split_track(tx, track, time.range.start);
                split_track(tx, track, time.range.end);
            }
        }
        None => {
            for &clip in &selection.clips {
                arrange::split(tx, clip, at);
            }
        }
    });
}

/// Splits every clip on the track with id `track` that plays across `at`.
fn split_track(tx: &mut Transaction<'_>, track: TrackId, at: Beats) {
    let Some(t) = tx.project().track(track) else {
        return;
    };
    let across: Vec<ClipId> = t
        .clips
        .iter()
        .filter(|clip| clip.start < at && at < clip.end())
        .map(|clip| clip.id)
        .collect();
    for clip in across {
        arrange::split(tx, clip, at);
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::{AudioClip, ClipContent, MidiClip};
    use musicable_model::{LocatorId, TrackKind};

    use super::*;
    use crate::testing::Setup;
    use crate::{Args, Document, Keymap};

    /// Counts what undo and redo ask of the transport, which is nothing.
    #[derive(Default)]
    struct Untouched(usize);

    impl crate::Transport for Untouched {
        fn is_playing(&self) -> bool {
            false
        }

        fn play(&mut self) {
            self.0 += 1;
        }

        fn stop(&mut self) {
            self.0 += 1;
        }

        fn locate(&mut self, _: Beats) {
            self.0 += 1;
        }

        fn playhead(&self) -> Beats {
            Beats::ZERO
        }
    }

    #[test]
    fn undo_and_redo_step_through_the_history() {
        let mut registry = Registry::new();
        register(&mut registry);
        crate::transport::register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut transport = Untouched::default();
        let mut requests = Vec::new();
        let mut run = |id: &CommandId, document: &mut Document| {
            let mut ctx = Context {
                document,
                transport: &mut transport,
                requests: &mut requests,
            };
            registry.run(id, &mut ctx).unwrap();
        };

        run(&crate::transport::TOGGLE_METRONOME, &mut document);
        run(&crate::transport::GO_TO_START, &mut document);
        run(&UNDO, &mut document);
        assert!(!document.project().metronome);
        run(&UNDO, &mut document);
        run(&REDO, &mut document);
        assert!(document.project().metronome);
        run(&REDO, &mut document);
        assert!(document.project().metronome);

        let names: Vec<_> = document
            .take_committed()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "Toggle Metronome",
                "Undo Toggle Metronome",
                "Redo Toggle Metronome"
            ]
        );
        // Going to the start was the only call on the transport.
        assert_eq!(transport.0, 1);
        assert_eq!(requests, []);
    }

    #[test]
    fn the_edit_commands_have_live_s_keys() {
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        assert_eq!(chords(UNDO), ["Ctrl+Z"]);
        assert_eq!(chords(REDO), ["Ctrl+Y", "Ctrl+Shift+Z"]);
        assert_eq!(chords(CUT), ["Ctrl+X"]);
        assert_eq!(chords(COPY), ["Ctrl+C"]);
        assert_eq!(chords(PASTE), ["Ctrl+V"]);
        assert_eq!(chords(DUPLICATE), ["Ctrl+D"]);
        assert_eq!(chords(DELETE), ["Backspace", "Delete"]);
        assert_eq!(chords(SPLIT), ["Ctrl+E"]);
        assert_eq!(chords(SELECT_ALL), ["Ctrl+A"]);
        assert_eq!(chords(RENAME), ["Ctrl+R"]);
        let mut registry = Registry::new();
        register(&mut registry);
        for (id, title, when, _) in COMMANDS {
            let command = registry.get(&id).unwrap();
            assert_eq!((command.title.as_str(), command.when), (title, when));
        }
        // The select commands are for the mouse, scripts and agents.
        for id in [SELECT_CLIPS, SELECT_TIME, SELECT_TRACKS] {
            assert!(registry.get(&id).unwrap().needs_arguments(), "{id}");
            assert_eq!(keymap.chords_for(&id).count(), 0);
        }
    }

    /// Bass and Keys, MIDI tracks, and Vox, an audio track: a clip on each,
    /// Keys' from beat 4 to 12, Bass's from 0 to 8, and Vox's from 2 to 6.
    fn arrangement() -> (Setup, [TrackId; 3], [ClipId; 3]) {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Midi);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let vox = project.add_track("Vox", TrackKind::Audio);
        let mut add = |track, name: &str, start, end, content| {
            let range = BeatRange::new(Beats::new(start), Beats::new(end));
            let clip = project.new_clip(name, range, content);
            let id = clip.id;
            project.track_mut(track).unwrap().clips.push(clip);
            id
        };
        let midi = || ClipContent::Midi(MidiClip::default());
        let b = add(bass, "B", 0, 8, midi());
        let k = add(keys, "K", 4, 12, midi());
        let v = add(vox, "V", 2, 6, ClipContent::Audio(AudioClip::new("v.wav")));
        (Setup::new(project), [bass, keys, vox], [b, k, v])
    }

    /// Each clip on `track`, as (name, start, end) in beats, in order.
    fn clips(setup: &Setup, track: TrackId) -> Vec<(String, f64, f64)> {
        let track = setup.document.project().track(track).unwrap();
        let mut clips: Vec<_> = track
            .clips
            .iter()
            .map(|c| (c.name.clone(), c.start.to_f64(), c.end().to_f64()))
            .collect();
        clips.sort_by(|a, b| a.1.total_cmp(&b.1));
        clips
    }

    fn row(name: &str, start: f64, end: f64) -> (String, f64, f64) {
        (name.to_owned(), start, end)
    }

    #[test]
    fn copy_and_paste_put_clips_at_the_start_marker() {
        let (mut setup, [bass, keys, _], [b, k, _]) = arrangement();
        // Nothing to copy, nothing to paste.
        assert!(!setup.applies(&COPY) && !setup.applies(&PASTE));
        setup.select_clips(&[b, k]);
        setup.run(&COPY);
        assert!(setup.applies(&PASTE));
        // Copying changes nothing.
        assert_eq!(setup.document.take_committed(), []);

        setup.select_tracks(&[keys]);
        setup.document.editor_mut().start_marker = Beats::new(16);
        setup.run(&PASTE);
        // From the selected track down: Keys gets Bass's clip, and Vox, an
        // audio track, can't take Keys'.
        assert_eq!(
            clips(&setup, keys),
            [row("K", 4.0, 12.0), row("B", 16.0, 24.0)]
        );
        assert_eq!(clips(&setup, bass), [row("B", 0.0, 8.0)]);
        let editor = setup.document.editor();
        // The paste is selected, and the start marker moves past it, for the
        // next.
        assert_eq!(editor.selection.clips.len(), 1);
        assert_eq!(editor.start_marker, Beats::new(28));
        let names: Vec<_> = setup.committed_names();
        assert_eq!(names, ["Paste"]);
        // The pasted clip has an id of its own.
        let pasted = setup.document.project().tracks[1].clips[1].id;
        assert!(pasted != b && pasted != k);
    }

    #[test]
    fn cut_takes_the_clips_away_for_paste() {
        let (mut setup, [bass, ..], [b, ..]) = arrangement();
        setup.select_clips(&[b]);
        setup.run(&CUT);
        assert_eq!(clips(&setup, bass), []);
        assert!(setup.document.editor().selection.clips.is_empty());
        setup.select_tracks(&[bass]);
        setup.document.editor_mut().start_marker = Beats::new(4);
        setup.run(&PASTE);
        assert_eq!(clips(&setup, bass), [row("B", 4.0, 12.0)]);
        assert_eq!(setup.committed_names(), ["Cut", "Paste"]);
        setup.run(&UNDO);
        setup.run(&UNDO);
        assert_eq!(clips(&setup, bass), [row("B", 0.0, 8.0)]);
    }

    #[test]
    fn the_time_selection_copies_the_parts_of_clips_in_it() {
        let (mut setup, [bass, keys, vox], _) = arrangement();
        setup.select_time(6, 10, &[bass, keys]);
        setup.run(&COPY);
        let clipboard = setup.document.editor().clipboard.clone().unwrap();
        let parts: Vec<_> = clipboard
            .clips
            .iter()
            .map(|(row, clip)| (*row, clip.start.to_f64(), clip.end().to_f64()))
            .collect();
        assert_eq!(parts, [(0, 6.0, 8.0), (1, 6.0, 10.0)]);
        // Pasting keeps the stretch's shape: Bass's part starts 0 beats in,
        // and the start marker moves past the whole stretch.
        setup.select_time(20, 20, &[keys]);
        setup.document.editor_mut().start_marker = Beats::new(20);
        setup.run(&PASTE);
        assert_eq!(
            clips(&setup, keys),
            [row("K", 4.0, 12.0), row("B", 20.0, 22.0)]
        );
        // Vox, the next track down, can't hold Keys' MIDI.
        assert_eq!(clips(&setup, vox), [row("V", 2.0, 6.0)]);
        assert_eq!(setup.document.editor().start_marker, Beats::new(24));
    }

    #[test]
    fn duplicate_puts_a_copy_just_after() {
        let (mut setup, [bass, keys, _], [b, ..]) = arrangement();
        setup.select_clips(&[b]);
        setup.run(&DUPLICATE);
        // The copy of Bass's clip lands at 8, and is selected.
        assert_eq!(
            clips(&setup, bass),
            [row("B", 0.0, 8.0), row("B", 8.0, 16.0)]
        );
        let copy = setup.document.project().tracks[0].clips[1].id;
        assert_eq!(setup.document.editor().selection, Selection::clips([copy]));
        // Again, after the copy.
        setup.run(&DUPLICATE);
        assert_eq!(clips(&setup, bass).last(), Some(&row("B", 16.0, 24.0)));

        // A time selection duplicates what's in it, the part of Keys' clip
        // from 4 to 6, overwriting what's there, and moves along.
        setup.select_time(2, 6, &[keys]);
        setup.run(&DUPLICATE);
        assert_eq!(
            clips(&setup, keys),
            [
                row("K", 4.0, 8.0),
                row("K", 8.0, 10.0),
                row("K", 10.0, 12.0)
            ]
        );
        let time = setup.document.editor().selection.time.clone().unwrap();
        assert_eq!(time.range, BeatRange::new(Beats::new(6), Beats::new(10)));
        assert_eq!(
            setup.committed_names(),
            ["Duplicate", "Duplicate", "Duplicate"]
        );
    }

    #[test]
    fn delete_takes_clips_then_time_then_tracks() {
        let (mut setup, [bass, keys, vox], [_, k, _]) = arrangement();
        assert!(!setup.applies(&DELETE));
        setup.select_clips(&[k]);
        setup.run(&DELETE);
        assert_eq!(clips(&setup, keys), []);

        setup.select_time(2, 4, &[bass, vox]);
        setup.run(&DELETE);
        assert_eq!(
            clips(&setup, bass),
            [row("B", 0.0, 2.0), row("B", 4.0, 8.0)]
        );
        assert_eq!(clips(&setup, vox), [row("V", 4.0, 6.0)]);

        setup.select_tracks(&[keys, vox]);
        setup.run(&DELETE);
        let names: Vec<_> = setup
            .document
            .project()
            .tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["Bass"]);
        assert_eq!(
            setup.committed_names(),
            ["Delete", "Delete", "Delete Tracks"]
        );
        // The selection forgot what went.
        assert!(setup.document.editor().selection.is_empty());
    }

    #[test]
    fn split_cuts_at_the_time_selection_or_the_start_marker() {
        let (mut setup, [bass, keys, _], [b, ..]) = arrangement();
        setup.select_clips(&[b]);
        setup.document.editor_mut().start_marker = Beats::new(3);
        setup.run(&SPLIT);
        assert_eq!(
            clips(&setup, bass),
            [row("B", 0.0, 3.0), row("B", 3.0, 8.0)]
        );
        // Outside the clip, the start marker splits nothing.
        setup.document.editor_mut().start_marker = Beats::new(20);
        setup.run(&SPLIT);
        assert_eq!(setup.committed_names(), ["Split"]);

        setup.select_time(6, 8, &[keys]);
        setup.run(&SPLIT);
        assert_eq!(
            clips(&setup, keys),
            [row("K", 4.0, 6.0), row("K", 6.0, 8.0), row("K", 8.0, 12.0)]
        );
    }

    #[test]
    fn selecting_follows_the_mouse() {
        let (mut setup, [bass, keys, vox], [b, k, v]) = arrangement();
        let select = |setup: &mut Setup, clips: &[ClipId], toggle: bool| {
            let args = Args::new()
                .with("clips", clips.to_vec())
                .with("toggle", toggle);
            setup.run_with(&SELECT_CLIPS, args).unwrap();
        };
        select(&mut setup, &[b], false);
        select(&mut setup, &[v, k], true);
        let editor = setup.document.editor();
        assert_eq!(editor.selection.clips, [b, k, v].into());
        // The track of the last clip clicked.
        assert_eq!(editor.selected_track, Some(keys));
        select(&mut setup, &[k], true);
        assert_eq!(setup.document.editor().selection.clips, [b, v].into());
        assert_eq!(
            setup.run_with(&SELECT_CLIPS, Args::new().with("clips", vec![ClipId(99)])),
            Err(CommandError::invalid("there's no clip 99"))
        );

        // A time selection replaces the clips.
        setup.select_time(8, 2, &[vox, bass]);
        let editor = setup.document.editor();
        let time = editor.selection.time.clone().unwrap();
        assert_eq!(time.range, BeatRange::new(Beats::new(2), Beats::new(8)));
        assert!(editor.selection.clips.is_empty());
        // Keys isn't one of them: the topmost one is selected now.
        assert_eq!(editor.selected_track, Some(bass));
        // A click without a stretch selects the track, and no time.
        setup.select_time(5, 5, &[vox]);
        let editor = setup.document.editor();
        assert_eq!(editor.selection, Selection::default());
        assert_eq!(editor.selected_track, Some(vox));

        setup.select_tracks(&[keys]);
        let args = Args::new().with("tracks", vec![bass]).with("toggle", true);
        setup.run_with(&SELECT_TRACKS, args).unwrap();
        let editor = setup.document.editor();
        assert_eq!(editor.selection.tracks, [bass, keys].into());
        assert_eq!(editor.selected_track, Some(bass));

        setup.run(&SELECT_ALL);
        assert_eq!(setup.document.editor().selection.clips, [b, k, v].into());
        // None of it is a change to the project.
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn rename_asks_the_app_for_a_name() {
        let (mut setup, [_, keys, _], [b, k, _]) = arrangement();
        assert!(!setup.applies(&RENAME));
        setup.select_clips(&[b]);
        setup.run(&RENAME);
        setup.select_clips(&[b, k]);
        assert!(setup.applies(&RENAME));
        setup.run(&RENAME);
        let args = Args::new().with("locator", 3_i64);
        setup.run_with(&RENAME, args).unwrap();
        assert_eq!(
            setup.requests,
            [
                Request::Rename(Named::Clip(b)),
                // Two clips: the selected track, Keys'.
                Request::Rename(Named::Track(keys)),
                Request::Rename(Named::Locator(LocatorId(3))),
            ]
        );
    }
}

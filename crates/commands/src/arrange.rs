//! Arranging clips: moving and copying them, trimming, splitting and
//! deleting them, their fades and names, and new clips.
//!
//! The functions here make the changes inside a transaction. The edit
//! commands (see [`edit`](crate::edit)) run them on the selection, and the
//! commands here run them on the clips their arguments name, for the
//! arrangement's mouse gestures, scripts and agents.
//!
//! Clips that land where others are overwrite them, as in Live: what was
//! there is trimmed back, split around them or removed (see [`clear`]).
//! Splitting and trimming keep each clip's content where it plays in the
//! song, so an audio clip split in two plays exactly as before.

use std::collections::BTreeSet;
use std::path::Path;

use musicable_model::clip::{AudioClip, Clip, ClipContent, ClipId, Fades, MidiClip, Timing};
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{Project, TrackId, TrackKind};

use crate::editor::{Editor, Selection};
use crate::{
    Args, Command, CommandError, CommandId, Context, Param, Registry, Transaction, Type, Value,
};

/// Moves clips, or copies them, by some beats and some tracks.
pub const MOVE: CommandId = CommandId::new("clip.move");
/// Moves a clip's start or end, keeping its content where it plays.
pub const TRIM: CommandId = CommandId::new("clip.trim");
/// Sets an audio clip's fades.
pub const SET_FADES: CommandId = CommandId::new("clip.set_fades");
/// Names a clip.
pub const RENAME: CommandId = CommandId::new("clip.rename");
/// Splits clips at a position.
pub const SPLIT: CommandId = CommandId::new("clip.split");
/// Deletes clips.
pub const DELETE: CommandId = CommandId::new("clip.delete");
/// Adds an empty MIDI clip.
pub const INSERT_MIDI_CLIP: CommandId = CommandId::new("clip.insert_midi_clip");
/// Adds clips playing whole audio files, one after another.
pub const INSERT_AUDIO: CommandId = CommandId::new("clip.insert_audio");

/// What an inserted MIDI clip is called.
const MIDI_CLIP: &str = "MIDI Clip";

/// Registers the clip commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(MOVE, "Move Clips").with_params(&[
            Param::required("clips", Type::CLIPS, "The clips to move."),
            Param::optional("by", Type::Beats, "How far later, or earlier if negative."),
            Param::optional(
                "tracks",
                Type::Int,
                "How many tracks down, or up if negative.",
            ),
            Param::optional(
                "copy",
                Type::Bool,
                "Copies the clips instead of moving them.",
            ),
        ]),
        |ctx, args| {
            let clips: Vec<ClipId> = args.get("clips")?;
            let by = args.opt("by")?.unwrap_or(Beats::ZERO);
            let tracks = args.opt("tracks")?.unwrap_or(0);
            let copy = args.opt("copy")?.unwrap_or(false);
            let name = if copy { "Copy Clips" } else { "Move Clips" };
            let moved = ctx
                .document
                .transact(name, |tx| move_clips(tx, &clips, by, tracks, copy));
            // Copies are selected, as in Live.
            ctx.document.editor_mut().selection = Selection::clips(moved.clone());
            Ok(moved.into())
        },
    );
    registry.add(
        Command::new(TRIM, "Trim Clip").with_params(&[
            Param::required("clip", Type::Clip, "The clip to trim."),
            Param::optional("start", Type::Beats, "Where it starts from now on."),
            Param::optional("end", Type::Beats, "Where it ends from now on."),
        ]),
        |ctx, args| {
            let clip = args.get("clip")?;
            let (start, end) = (args.opt("start")?, args.opt("end")?);
            ctx.document
                .transact("Trim Clip", |tx| trim(tx, clip, start, end));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_FADES, "Set Fades").with_params(&[
            Param::required("clip", Type::Clip, "An audio clip."),
            Param::optional("fade_in", Type::Beats, "How long it fades in."),
            Param::optional("fade_out", Type::Beats, "How long it fades out."),
        ]),
        |ctx, args| {
            let clip = args.get("clip")?;
            let (fade_in, fade_out) = (args.opt("fade_in")?, args.opt("fade_out")?);
            ctx.document.transact("Fade", |tx| {
                let Some(fades) = fades_of(tx, clip) else {
                    return;
                };
                let fades = Fades {
                    fade_in: fade_in.unwrap_or(fades.fade_in),
                    fade_out: fade_out.unwrap_or(fades.fade_out),
                };
                tx.set_clip_fades(clip, fades);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RENAME, "Rename Clip").with_params(&[
            Param::required("clip", Type::Clip, "The clip to name."),
            Param::required("name", Type::Text, "Its new name."),
        ]),
        |ctx, args| {
            let clip = args.get("clip")?;
            let name: String = args.get("name")?;
            ctx.document
                .transact("Rename Clip", |tx| tx.set_clip_name(clip, &name));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SPLIT, "Split Clips").with_params(&[
            Param::required("clips", Type::CLIPS, "The clips to split."),
            Param::required("at", Type::Beats, "Where to split them."),
        ]),
        |ctx, args| {
            let clips: Vec<ClipId> = args.get("clips")?;
            let at = args.get("at")?;
            let parts = ctx.document.transact("Split", |tx| {
                clips
                    .iter()
                    .filter_map(|&clip| split(tx, clip, at))
                    .collect::<Vec<_>>()
            });
            Ok(parts.into())
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Clips").with_params(&[Param::required(
            "clips",
            Type::CLIPS,
            "The clips to delete.",
        )]),
        |ctx, args| {
            let clips: Vec<ClipId> = args.get("clips")?;
            ctx.document.transact("Delete", |tx| {
                for clip in clips {
                    tx.remove_clip(clip);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(INSERT_MIDI_CLIP, "Insert MIDI Clip").with_params(&[
            Param::optional(
                "track",
                Type::Track,
                "A MIDI track. Without one, the clip goes on the MIDI tracks of the time \
                 selection, or else on the selected track.",
            ),
            Param::optional(
                "start",
                Type::Beats,
                "Where it starts: the time selection's start, or else the start marker.",
            ),
            Param::optional(
                "end",
                Type::Beats,
                "Where it ends: the time selection's end, or else a bar after its start.",
            ),
        ]),
        |ctx, args| {
            let clips = insert_midi_clips(ctx, args)?;
            ctx.document.editor_mut().selection = Selection::clips(clips.clone());
            Ok(clips.into())
        },
    );
    registry.add(
        Command::new(INSERT_AUDIO, "Insert Audio").with_params(&[
            Param::required(
                "files",
                Type::List(&Type::Text),
                "The files' paths, as clips name files. The app must have read each file \
                 first: see Editor::files.",
            ),
            Param::optional(
                "track",
                Type::Track,
                "An audio track. Without one, the clips go on the selected track if it's \
                 an audio track, or else on a new one.",
            ),
            Param::optional(
                "at",
                Type::Beats,
                "Where the first clip starts: the time selection's start, or else the start \
                 marker.",
            ),
        ]),
        |ctx, args| {
            let files: Vec<String> = args.get("files")?;
            let (track, clips) = insert_audio(ctx, &files, args.opt("track")?, args.opt("at")?)?;
            let editor = ctx.document.editor_mut();
            editor.selection = Selection::clips(clips.clone());
            editor.selected_track = Some(track);
            Ok(clips.into())
        },
    );
}

/// Clears `range` on the track with id `track` for something to go there,
/// as Live does: clips inside it go, clips across one of its edges are
/// trimmed back to it, and a clip across both is split around it. The clips
/// in `keep` stay as they are.
pub fn clear(tx: &mut Transaction<'_>, track: TrackId, range: BeatRange, keep: &BTreeSet<ClipId>) {
    if range.is_empty() {
        return;
    }
    let Some(t) = tx.project().track(track) else {
        return;
    };
    let hit: Vec<Clip> = t
        .clips
        .iter()
        .filter(|c| !keep.contains(&c.id) && c.start < range.end && c.end() > range.start)
        .cloned()
        .collect();
    for clip in hit {
        match (clip.start < range.start, clip.end() > range.end) {
            (false, false) => {
                tx.remove_clip(clip.id);
            }
            (true, false) => {
                retime(tx, &clip, BeatRange::new(clip.start, range.start));
            }
            (false, true) => {
                retime(tx, &clip, BeatRange::new(range.end, clip.end()));
            }
            (true, true) => {
                let tail = BeatRange::new(range.end, clip.end());
                if let Some(tail) = copy_part(tx, &clip, tail) {
                    let index = after(tx, track, clip.id);
                    tx.add_clip(track, index, tail);
                }
                cut_fade_out(tx, clip.id);
                retime(tx, &clip, BeatRange::new(clip.start, range.start));
            }
        }
    }
}

/// Puts each clip on its track, overwriting what's there but not each
/// other, and returns their ids.
pub fn place(tx: &mut Transaction<'_>, clips: Vec<(TrackId, Clip)>) -> Vec<ClipId> {
    for (track, clip) in &clips {
        clear(tx, *track, clip.range(), &BTreeSet::new());
    }
    clips
        .into_iter()
        .map(|(track, clip)| {
            let id = clip.id;
            tx.add_clip(track, usize::MAX, clip);
            id
        })
        .collect()
}

/// Moves the clips with ids `ids` by `by` and `tracks` tracks down, or
/// copies them there with `copy`, and returns the ids of the clips where
/// they landed. They keep their places relative to each other: a move that
/// would take one before the song's start stops it there, and one that
/// would take one off the tracks, or onto a track of another kind, keeps
/// them all on their tracks.
pub fn move_clips(
    tx: &mut Transaction<'_>,
    ids: &[ClipId],
    by: Beats,
    tracks: i64,
    copy: bool,
) -> Vec<ClipId> {
    let project = tx.project();
    let moving: Vec<(usize, Clip)> = ids
        .iter()
        .filter_map(|&id| {
            let (track, clip) = project.find_clip(id)?;
            Some((project.track_index(track.id)?, clip.clone()))
        })
        .collect();
    let Some(earliest) = moving.iter().map(|(_, clip)| clip.start).min() else {
        return Vec::new();
    };
    let by = by.max(-earliest);
    let lands = |&(index, ref clip): &(usize, Clip)| {
        let to = index as i64 + tracks;
        usize::try_from(to)
            .ok()
            .and_then(|to| project.tracks.get(to))
            .is_some_and(|track| track.holds(&clip.content))
    };
    let tracks = if moving.iter().all(lands) { tracks } else { 0 };
    if by == Beats::ZERO && tracks == 0 {
        return moving.into_iter().map(|(_, clip)| clip.id).collect();
    }
    let targets: Vec<TrackId> = moving
        .iter()
        .map(|(index, _)| project.tracks[(*index as i64 + tracks) as usize].id)
        .collect();
    let sources: Vec<TrackId> = moving
        .iter()
        .map(|(index, _)| project.tracks[*index].id)
        .collect();
    let keep: BTreeSet<ClipId> = match copy {
        true => BTreeSet::new(),
        false => moving.iter().map(|(_, clip)| clip.id).collect(),
    };
    for ((_, clip), &target) in moving.iter().zip(&targets) {
        let range = BeatRange::new(clip.start + by, clip.end() + by);
        clear(tx, target, range, &keep);
    }
    let mut landed = Vec::with_capacity(moving.len());
    for (((_, clip), target), source) in moving.into_iter().zip(targets).zip(sources) {
        let start = clip.start + by;
        if copy {
            let copy = Clip {
                id: tx.new_clip_id(),
                start,
                ..clip
            };
            landed.push(copy.id);
            tx.add_clip(target, usize::MAX, copy);
        } else if source == target {
            let timing = Timing {
                start,
                ..clip.timing()
            };
            tx.set_clip_timing(clip.id, timing);
            landed.push(clip.id);
        } else if let Some(clip) = tx.remove_clip(clip.id) {
            landed.push(clip.id);
            tx.add_clip(target, usize::MAX, Clip { start, ..clip });
        }
    }
    landed
}

/// Moves the start or the end of the clip with id `clip`, or both, keeping
/// its content where it plays, and overwrites what it comes to cover. An
/// audio clip can't start before its file does.
pub fn trim(tx: &mut Transaction<'_>, clip: ClipId, start: Option<Beats>, end: Option<Beats>) {
    let Some((track, current)) = tx.project().find_clip(clip) else {
        return;
    };
    let (track, current) = (track.id, current.clone());
    let Some(timing) = trimmed(tx.project(), tx.editor(), &current, start, end) else {
        return;
    };
    let keep = BTreeSet::from([clip]);
    clear(
        tx,
        track,
        BeatRange::new(timing.start, current.start),
        &keep,
    );
    clear(
        tx,
        track,
        BeatRange::new(current.end(), timing.end()),
        &keep,
    );
    tx.set_clip_timing(clip, timing);
}

/// Where [`trim`] would have `clip`, in `project`, play with its start or
/// its end, or both, moved there: from the song's start at the earliest,
/// and for an audio clip, only where its file has audio. `None` if that
/// leaves nothing of it.
pub fn trimmed(
    project: &Project,
    editor: &Editor,
    clip: &Clip,
    start: Option<Beats>,
    end: Option<Beats>,
) -> Option<Timing> {
    let start = start.unwrap_or(clip.start).max(Beats::ZERO);
    let range = BeatRange::new(start, end.unwrap_or(clip.end()));
    timing_over(project, editor, clip, range)
}

/// Splits the clip with id `clip` at `at`, if `at` is inside it, and
/// returns the id of the part after it, a new clip. The first part keeps
/// the fade in and the second the fade out.
pub fn split(tx: &mut Transaction<'_>, clip: ClipId, at: Beats) -> Option<ClipId> {
    let (track, current) = tx.project().find_clip(clip)?;
    let (track, current) = (track.id, current.clone());
    if at <= current.start || at >= current.end() {
        return None;
    }
    let tail = copy_part(tx, &current, BeatRange::new(at, current.end()))?;
    let id = tail.id;
    cut_fade_out(tx, clip);
    retime(tx, &current, BeatRange::new(current.start, at));
    let index = after(tx, track, clip);
    tx.add_clip(track, index, tail);
    Some(id)
}

/// The part of `clip` over `range` as a clip of its own, with a new id. It
/// keeps the fades of the edges it shares with `clip`.
pub fn copy_part(tx: &mut Transaction<'_>, clip: &Clip, range: BeatRange) -> Option<Clip> {
    let mut part = crop(tx.project(), tx.editor(), clip, range)?;
    part.id = tx.new_clip_id();
    Some(part)
}

/// The part of `clip`, in `project`, over `range`, keeping its id, and the
/// fades of the edges it shares with `clip`. `None` if nothing of the clip
/// is there.
pub fn crop(project: &Project, editor: &Editor, clip: &Clip, range: BeatRange) -> Option<Clip> {
    let range = BeatRange::new(range.start.max(clip.start), range.end.min(clip.end()));
    let timing = timing_over(project, editor, clip, range)?;
    let mut part = clip.clone();
    if let ClipContent::Audio(audio) = &mut part.content {
        if timing.start != clip.start {
            audio.fade_in = Beats::ZERO;
        }
        if timing.end() != clip.end() {
            audio.fade_out = Beats::ZERO;
        }
        let fade_in = audio.fade_in.min(timing.length);
        audio.fade_out = audio.fade_out.min(timing.length - fade_in);
        audio.fade_in = fade_in;
    }
    part.set_timing(timing).then_some(part)
}

/// Adds a clip playing all of the audio file at `file`, as clips name
/// files, to the track with id `track` from `at`, overwriting what's there,
/// and returns its id. The app must have read the file's header into the
/// editor's [`files`](crate::Editor::files).
pub fn insert_audio_clip(
    tx: &mut Transaction<'_>,
    track: TrackId,
    at: Beats,
    file: &str,
) -> Result<ClipId, CommandError> {
    let Some(info) = tx.editor().files.get(file).copied() else {
        return Err(CommandError::invalid(format!(
            "the audio file {file} hasn't been read"
        )));
    };
    let Some(t) = tx.project().track(track) else {
        return Err(CommandError::invalid("there's no such track"));
    };
    if t.kind != TrackKind::Audio {
        return Err(CommandError::invalid("audio clips go on audio tracks"));
    }
    if info.frames == 0 {
        return Err(CommandError::invalid(format!(
            "the audio file {file} is empty"
        )));
    }
    let at = at.max(Beats::ZERO);
    let tempo = &tx.project().tempo;
    let rate = info.sample_rate;
    // Long enough for the file's last frame.
    let first = tempo.frame_at(at, rate);
    let end = after_frames(tempo, rate, first, info.frames).max(at + Beats::from_ticks(1));
    let content = ClipContent::Audio(AudioClip::new(file));
    let clip = tx.new_clip(file_name(file), BeatRange::new(at, end), content);
    let id = clip.id;
    place(tx, vec![(track, clip)]);
    Ok(id)
}

/// Inserts a clip playing the whole of each of `files`, one after another,
/// from `at` on the audio track `track`, or where [`INSERT_AUDIO`] says
/// without them. Returns the track and the clips' ids.
fn insert_audio(
    ctx: &mut Context<'_>,
    files: &[String],
    track: Option<TrackId>,
    at: Option<Beats>,
) -> Result<(TrackId, Vec<ClipId>), CommandError> {
    let editor = ctx.document.editor();
    let project = ctx.document.project();
    let Some(first) = files.first() else {
        return Err(CommandError::invalid("there are no files to insert"));
    };
    for file in files {
        match editor.files.get(file) {
            None => {
                let problem = format!("the audio file {file} hasn't been read");
                return Err(CommandError::invalid(problem));
            }
            Some(info) if info.frames == 0 => {
                return Err(CommandError::invalid(format!(
                    "the audio file {file} is empty"
                )));
            }
            Some(_) => {}
        }
    }
    let audio = |id: TrackId| {
        project
            .track(id)
            .is_some_and(|t| t.kind == TrackKind::Audio)
    };
    let track = match track {
        Some(id) if project.track(id).is_none() => {
            return Err(CommandError::invalid(format!("there's no track {}", id.0)));
        }
        Some(id) if !audio(id) => {
            return Err(CommandError::invalid("audio clips go on audio tracks"));
        }
        Some(id) => Some(id),
        None => editor.selected_track.filter(|&id| audio(id)),
    };
    let time = editor.selection.time.as_ref();
    let at = at.unwrap_or_else(|| time.map_or(editor.start_marker, |time| time.range.start));
    Ok(ctx.document.transact("Insert Audio", |tx| {
        // A new track is named after the first file.
        let track = track
            .unwrap_or_else(|| crate::track::add_track(tx, file_name(first), TrackKind::Audio));
        let mut at = at.max(Beats::ZERO);
        let mut clips = Vec::with_capacity(files.len());
        for file in files {
            // The files were all checked.
            if let Ok(clip) = insert_audio_clip(tx, track, at, file) {
                at = tx.project().find_clip(clip).map_or(at, |(_, c)| c.end());
                clips.push(clip);
            }
        }
        (track, clips)
    }))
}

/// A file's name without its folder or its extension, for a clip or a
/// track playing it.
fn file_name(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map_or(file.to_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// Inserts empty MIDI clips, as the insert command's arguments or the
/// selection say, and returns their ids.
fn insert_midi_clips(ctx: &mut Context<'_>, args: &Args) -> Result<Vec<ClipId>, CommandError> {
    let editor = ctx.document.editor();
    let project = ctx.document.project();
    let time = editor.selection.time.as_ref();
    let tracks: Vec<TrackId> = match args.opt::<TrackId>("track")? {
        Some(track) => vec![track],
        None => match time {
            Some(time) => project
                .tracks
                .iter()
                .filter(|t| time.tracks.contains(&t.id))
                .map(|t| t.id)
                .collect(),
            None => editor.selected_track.into_iter().collect(),
        },
    };
    let tracks: Vec<TrackId> = tracks
        .into_iter()
        .filter(|&id| project.track(id).is_some_and(|t| t.kind == TrackKind::Midi))
        .collect();
    if tracks.is_empty() {
        return Err(CommandError::invalid("MIDI clips go on MIDI tracks"));
    }
    let start = match args.opt("start")? {
        Some(start) => start,
        None => time.map_or(editor.start_marker, |time| time.range.start),
    };
    let start = start.max(Beats::ZERO);
    let end = match args.opt("end")? {
        Some(end) => end,
        None => match time {
            Some(time) if args.opt::<Beats>("start")?.is_none() => time.range.end,
            // A bar long.
            _ => {
                let tempo = &project.tempo;
                let bar = tempo.bar_at(start).bar;
                start + (tempo.bar_start(bar + 1) - tempo.bar_start(bar))
            }
        },
    };
    if end <= start {
        return Err(CommandError::invalid("a clip must end after it starts"));
    }
    Ok(ctx.document.transact("Insert MIDI Clip", |tx| {
        let clips = tracks
            .into_iter()
            .map(|track| {
                let content = ClipContent::Midi(MidiClip::default());
                (
                    track,
                    tx.new_clip(MIDI_CLIP, BeatRange::new(start, end), content),
                )
            })
            .collect();
        place(tx, clips)
    }))
}

/// Takes away the fade out of the clip with id `clip`, for a split that
/// cuts its end off.
fn cut_fade_out(tx: &mut Transaction<'_>, clip: ClipId) {
    if let Some(fades) = fades_of(tx, clip) {
        let fades = Fades {
            fade_out: Beats::ZERO,
            ..fades
        };
        tx.set_clip_fades(clip, fades);
    }
}

/// An audio clip's fades.
fn fades_of(tx: &Transaction<'_>, clip: ClipId) -> Option<Fades> {
    match &tx.project().find_clip(clip)?.1.content {
        ClipContent::Audio(audio) => Some(audio.fades()),
        ClipContent::Midi(_) => None,
    }
}

/// Moves `clip`'s edges to `range`'s, keeping its content where it plays.
fn retime(tx: &mut Transaction<'_>, clip: &Clip, range: BeatRange) {
    // The clip as it is now: its fades may have changed since.
    let Some(current) = tx.project().find_clip(clip.id).map(|(_, c)| c.clone()) else {
        return;
    };
    if let Some(timing) = timing_over(tx.project(), tx.editor(), &current, range) {
        tx.set_clip_timing(clip.id, timing);
    }
}

/// The timing that plays `clip`'s content over `range`, for its file's
/// sample rate. An audio clip ends where its file does, at the latest, once
/// the app has read the file.
fn timing_over(
    project: &Project,
    editor: &Editor,
    clip: &Clip,
    mut range: BeatRange,
) -> Option<Timing> {
    let tempo = &project.tempo;
    let rate = match &clip.content {
        ClipContent::Audio(audio) => {
            let rate = editor.sample_rate_of(&audio.file);
            if let Some(file) = editor.files.get(&audio.file) {
                // The song's frame that the file's first frame plays on.
                let first = tempo.frame_at(clip.start, rate) - audio.offset as i64;
                range.end = range.end.min(after_frames(tempo, rate, first, file.frames));
            }
            rate
        }
        ClipContent::Midi(_) => editor.sample_rate,
    };
    clip.timing_over(range, tempo, rate)
}

/// Where `frames` frames from the song's frame `first`, at `rate`, have all
/// played: the position of the frame after them, or just after it if that
/// falls between ticks.
pub(crate) fn after_frames(tempo: &TempoMap, rate: u32, first: i64, frames: u64) -> Beats {
    let last = first + frames as i64;
    let mut end = Beats::from_f64(tempo.beats_at(last as f64, rate));
    while tempo.frame_at(end, rate) < last {
        end += Beats::from_ticks(1);
    }
    end
}

/// The place just after the clip with id `clip` in its track's list, or the
/// end.
fn after(tx: &Transaction<'_>, track: TrackId, clip: ClipId) -> usize {
    tx.project()
        .track(track)
        .and_then(|t| t.clip_index(clip))
        .map_or(usize::MAX, |index| index + 1)
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::Offset;

    use super::*;
    use crate::editor::{AudioFile, TimeSelection};
    use crate::testing::Setup;
    use crate::{Args, edit};

    /// Drums, an audio track with a clip from beat 0 to 8 playing a 48 kHz
    /// file that lasts 12 beats at 120 BPM, and Bass and Keys, MIDI tracks,
    /// with clips from 4 to 12 and 0 to 16.
    fn arrangement() -> (Setup, [TrackId; 3], [ClipId; 3]) {
        let mut project = Project::new();
        let drums = project.add_track("Drums", TrackKind::Audio);
        let bass = project.add_track("Bass", TrackKind::Midi);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut add = |track, name: &str, start, end, content| {
            let range = BeatRange::new(Beats::new(start), Beats::new(end));
            let clip = project.new_clip(name, range, content);
            let id = clip.id;
            project.track_mut(track).unwrap().clips.push(clip);
            id
        };
        let mut audio = AudioClip::new("loop.wav");
        audio.fade_in = Beats::new(1);
        audio.fade_out = Beats::new(2);
        let d = add(drums, "D", 0, 8, ClipContent::Audio(audio));
        let midi = || ClipContent::Midi(MidiClip::default());
        let b = add(bass, "B", 4, 12, midi());
        let k = add(keys, "K", 0, 16, midi());
        let mut setup = Setup::new(project);
        let file = AudioFile {
            sample_rate: 48_000,
            frames: 288_000,
            channels: 2,
        };
        setup
            .document
            .editor_mut()
            .files
            .insert("loop.wav".into(), file);
        (setup, [drums, bass, keys], [d, b, k])
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

    fn clip(setup: &Setup, id: ClipId) -> Clip {
        setup.document.project().find_clip(id).unwrap().1.clone()
    }

    fn moved(setup: &mut Setup, clips: &[ClipId], by: i64, tracks: i64, copy: bool) -> Value {
        let args = Args::new()
            .with("clips", clips.to_vec())
            .with("by", Beats::new(by))
            .with("tracks", tracks)
            .with("copy", copy);
        setup.run_with(&MOVE, args).unwrap()
    }

    #[test]
    fn moved_clips_overwrite_what_they_land_on() {
        let (mut setup, [_, bass, keys], [_, b, k]) = arrangement();
        // Bass's clip, one track down, lands in the middle of Keys': that
        // one is split around it.
        let landed = moved(&mut setup, &[b], 0, 1, false);
        assert_eq!(landed, Value::List(vec![Value::Clip(b)]));
        assert_eq!(clips(&setup, bass), []);
        assert_eq!(
            clips(&setup, keys),
            [
                row("K", 0.0, 4.0),
                row("B", 4.0, 12.0),
                row("K", 12.0, 16.0)
            ]
        );
        // The part after keeps playing what it played: its notes start 12
        // beats into the clip.
        let tail = setup.document.project().track(keys).unwrap().clips[1].clone();
        assert_eq!(tail.content.offset(), Offset::Beats(Beats::new(12)));
        // One step to undo.
        assert_eq!(setup.committed_names(), ["Move Clips"]);
        setup.run(&edit::UNDO);
        assert_eq!(clips(&setup, keys), [row("K", 0.0, 16.0)]);
        assert_eq!(clips(&setup, bass), [row("B", 4.0, 12.0)]);
        assert_eq!(clip(&setup, k).content.offset(), Offset::Beats(Beats::ZERO));
    }

    #[test]
    fn moves_keep_clips_together() {
        let (mut setup, [drums, bass, keys], [d, b, k]) = arrangement();
        // The earliest stops at the song's start: Keys' is there already.
        moved(&mut setup, &[b], -10, 0, false);
        assert_eq!(clips(&setup, bass), [row("B", 0.0, 8.0)]);
        moved(&mut setup, &[b, k], -10, 0, false);
        assert_eq!(clips(&setup, keys), [row("K", 0.0, 16.0)]);
        // Bass's clip can't go up onto Drums, an audio track, so neither
        // moves to another track.
        moved(&mut setup, &[b, k], 4, -1, false);
        assert_eq!(clips(&setup, bass), [row("B", 4.0, 12.0)]);
        assert_eq!(clips(&setup, keys), [row("K", 4.0, 20.0)]);
        // A copy leaves the clips where they are, and gets new ids.
        let copies = moved(&mut setup, &[d], 16, 0, true);
        assert_eq!(
            clips(&setup, drums),
            [row("D", 0.0, 8.0), row("D", 16.0, 24.0)]
        );
        let Value::List(copies) = copies else {
            panic!("{copies:?}")
        };
        assert_eq!(copies.len(), 1);
        assert_ne!(copies[0], Value::Clip(d));
        assert_eq!(
            setup.committed_names(),
            ["Move Clips", "Move Clips", "Copy Clips"]
        );
    }

    #[test]
    fn trimming_keeps_the_audio_where_it_plays() {
        let (mut setup, [drums, ..], [d, ..]) = arrangement();
        let trim = |setup: &mut Setup, start: Option<i64>, end: Option<i64>| {
            let mut args = Args::new().with("clip", d);
            if let Some(start) = start {
                args.set("start", Beats::new(start));
            }
            if let Some(end) = end {
                args.set("end", Beats::new(end));
            }
            setup.run_with(&TRIM, args).unwrap();
        };
        trim(&mut setup, Some(2), None);
        let trimmed = clip(&setup, d);
        assert_eq!(
            trimmed.range(),
            BeatRange::new(Beats::new(2), Beats::new(8))
        );
        // Two beats at 120 BPM into a 48 kHz file, and the fade in that
        // was at the start went with it.
        assert_eq!(trimmed.content.offset(), Offset::Frames(48_000));
        let ClipContent::Audio(audio) = &trimmed.content else {
            panic!()
        };
        assert_eq!(audio.fades().fade_in, Beats::new(1));
        // Back out, it can't start before the file does, or end after, as
        // `trimmed` says beforehand.
        let (project, editor) = (setup.document.project(), setup.document.editor());
        let (start, end) = (Some(Beats::new(-4)), Some(Beats::new(40)));
        let planned = super::trimmed(project, editor, &trimmed, start, end);
        trim(&mut setup, Some(-4), Some(40));
        let trimmed = clip(&setup, d);
        assert_eq!(Some(trimmed.timing()), planned);
        assert_eq!(trimmed.range(), BeatRange::new(Beats::ZERO, Beats::new(12)));
        assert_eq!(trimmed.content.offset(), Offset::Frames(0));
        // Trimmed past another clip, it overwrites it.
        let args = Args::new()
            .with("clips", vec![d])
            .with("by", Beats::new(20))
            .with("copy", true);
        setup.run_with(&MOVE, args).unwrap();
        trim(&mut setup, None, Some(10));
        trim(&mut setup, Some(0), Some(12));
        assert_eq!(
            clips(&setup, drums),
            [row("D", 0.0, 12.0), row("D", 20.0, 32.0)]
        );
        let ends: Vec<_> = clips(&setup, drums).into_iter().map(|c| c.2).collect();
        assert_eq!(ends, [12.0, 32.0]);
    }

    #[test]
    fn split_parts_play_on_as_before() {
        let (mut setup, [drums, ..], [d, ..]) = arrangement();
        let args = Args::new().with("clips", vec![d]).with("at", Beats::new(4));
        let parts = setup.run_with(&SPLIT, args).unwrap();
        let Value::List(parts) = parts else {
            panic!("{parts:?}")
        };
        let Value::Clip(tail) = parts[0] else {
            panic!("{parts:?}")
        };
        assert_eq!(
            clips(&setup, drums),
            [row("D", 0.0, 4.0), row("D", 4.0, 8.0)]
        );
        let fades = |id| match clip(&setup, id).content {
            ClipContent::Audio(audio) => audio.fades(),
            ClipContent::Midi(_) => panic!("an audio clip"),
        };
        // The fade in stays at the start, and the fade out at the end.
        assert_eq!(
            fades(d),
            Fades {
                fade_in: Beats::new(1),
                fade_out: Beats::ZERO
            }
        );
        assert_eq!(
            fades(tail),
            Fades {
                fade_in: Beats::ZERO,
                fade_out: Beats::new(2)
            }
        );
        assert_eq!(clip(&setup, tail).content.offset(), Offset::Frames(96_000));
        // Outside the clip, nothing.
        let args = Args::new()
            .with("clips", vec![d])
            .with("at", Beats::new(30));
        assert_eq!(setup.run_with(&SPLIT, args), Ok(Value::List(vec![])));
    }

    #[test]
    fn fades_fit_the_clip() {
        let (mut setup, _, [d, b, _]) = arrangement();
        let fade = |setup: &mut Setup, clip, fade_in: i64, fade_out: Option<i64>| {
            let mut args = Args::new()
                .with("clip", clip)
                .with("fade_in", Beats::new(fade_in));
            if let Some(fade_out) = fade_out {
                args.set("fade_out", Beats::new(fade_out));
            }
            setup.run_with(&SET_FADES, args).unwrap();
        };
        fade(&mut setup, d, 7, None);
        // The fade out keeps what's left.
        let ClipContent::Audio(audio) = clip(&setup, d).content else {
            panic!()
        };
        assert_eq!(audio.fades().fade_in, Beats::new(7));
        assert_eq!(audio.fades().fade_out, Beats::new(1));
        fade(&mut setup, d, -1, Some(0));
        // MIDI clips have no fades.
        fade(&mut setup, b, 1, None);
        assert_eq!(setup.committed_names(), ["Fade", "Fade"]);
        let args = Args::new().with("clip", d).with("name", "Beat");
        setup.run_with(&RENAME, args).unwrap();
        assert_eq!(clip(&setup, d).name, "Beat");
    }

    #[test]
    fn inserted_clips_take_the_selection_s_place() {
        let (mut setup, [drums, bass, keys], [d, _, k]) = arrangement();
        // MIDI clips over the time selection, on its MIDI tracks.
        setup.document.editor_mut().selection.time = Some(TimeSelection {
            range: BeatRange::new(Beats::new(2), Beats::new(6)),
            tracks: [drums, bass].into(),
        });
        setup.run(&INSERT_MIDI_CLIP);
        assert_eq!(
            clips(&setup, bass),
            [row("MIDI Clip", 2.0, 6.0), row("B", 6.0, 12.0)]
        );
        // Without one, a bar from the start marker on the selected track.
        setup.select_clips(&[k]);
        setup.document.editor_mut().start_marker = Beats::new(20);
        setup.run(&INSERT_MIDI_CLIP);
        assert_eq!(
            clips(&setup, keys).last(),
            Some(&row("MIDI Clip", 20.0, 24.0))
        );
        let inserted = &setup.document.editor().selection.clips;
        assert_eq!(inserted.len(), 1);
        assert!(!inserted.contains(&k));
        // Not on audio tracks.
        setup.select_clips(&[d]);
        assert_eq!(
            setup.run_with(&INSERT_MIDI_CLIP, Args::new().with("start", Beats::new(1))),
            Err(CommandError::invalid("MIDI clips go on MIDI tracks"))
        );

        // An audio clip lasts as long as its file.
        let args = Args::new()
            .with("files", vec!["loop.wav"])
            .with("track", drums)
            .with("at", Beats::new(4));
        setup.run_with(&INSERT_AUDIO, args.clone()).unwrap();
        assert_eq!(
            clips(&setup, drums),
            [row("D", 0.0, 4.0), row("loop", 4.0, 16.0)]
        );
        let unread = args.clone().with("files", vec!["loop.wav", "other.wav"]);
        assert_eq!(
            setup.run_with(&INSERT_AUDIO, unread),
            Err(CommandError::invalid(
                "the audio file other.wav hasn't been read"
            ))
        );
        let midi = args.with("track", bass);
        assert_eq!(
            setup.run_with(&INSERT_AUDIO, midi),
            Err(CommandError::invalid("audio clips go on audio tracks"))
        );
    }

    #[test]
    fn audio_without_a_track_goes_on_a_new_one() {
        let (mut setup, [drums, _, _], [d, b, _]) = arrangement();
        // The selected track is a MIDI track, so the files go on a new
        // track, one after another from the start marker, in one step.
        setup.select_clips(&[b]);
        setup.document.editor_mut().start_marker = Beats::new(2);
        let files = Args::new().with("files", vec!["loop.wav", "loop.wav"]);
        let inserted = setup.run_with(&INSERT_AUDIO, files.clone()).unwrap();
        let project = setup.document.project();
        let track = project.tracks.last().unwrap();
        assert_eq!(
            (track.name.as_str(), track.kind),
            ("loop", TrackKind::Audio)
        );
        assert_eq!(
            clips(&setup, track.id),
            [row("loop", 2.0, 14.0), row("loop", 14.0, 26.0)]
        );
        let editor = setup.document.editor();
        assert_eq!(editor.selected_track, Some(track.id));
        let Value::List(inserted) = inserted else {
            panic!("{inserted:?}")
        };
        assert_eq!(editor.selection.clips.len(), inserted.len());
        assert_eq!(setup.committed_names(), ["Insert Audio"]);
        // On the selected audio track, from the time selection's start.
        setup.select_clips(&[d]);
        setup.select_time(20, 24, &[drums]);
        setup.run_with(&INSERT_AUDIO, files).unwrap();
        assert_eq!(
            clips(&setup, drums),
            [
                row("D", 0.0, 8.0),
                row("loop", 20.0, 32.0),
                row("loop", 32.0, 44.0)
            ]
        );
        assert_eq!(setup.document.project().tracks.len(), 4);
        let none = Args::new().with("files", Vec::<String>::new());
        assert_eq!(
            setup.run_with(&INSERT_AUDIO, none),
            Err(CommandError::invalid("there are no files to insert"))
        );
    }

    #[test]
    fn copies_are_selected() {
        let (mut setup, _, [d, b, _]) = arrangement();
        setup.select_clips(&[d, b]);
        let copies = moved(&mut setup, &[d, b], 32, 0, true);
        let Value::List(copies) = copies else {
            panic!("{copies:?}")
        };
        let selected: Vec<Value> = setup
            .document
            .editor()
            .selection
            .clips
            .iter()
            .map(|&c| Value::Clip(c))
            .collect();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|c| copies.contains(c)));
    }
}

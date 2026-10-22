//! Editing notes: the piano roll's commands.
//!
//! The commands here change the notes of a MIDI clip: the clip their `clip`
//! argument names, or else the one the piano roll shows. They name notes by
//! their ids in the clip, and place them in the clip's own time, as the
//! project file does: a note at the clip's offset plays at the clip's start.
//! The piano roll's mouse gestures run them, and so can scripts and agents.
//! Those without arguments act on the notes selected in the piano roll while
//! it has the focus, with Live's keys.
//!
//! While the piano roll has the focus, the edit commands act on its notes
//! too (see [`edit`](crate::edit)): the functions for that are here.

use musicable_model::Project;
use musicable_model::clip::{Clip, ClipContent, ClipId, Expression, MidiClip, Note, NoteId};
use musicable_model::time::{BeatRange, Beats};

use crate::editor::{Editor, Focus, NoteClipboard};
use crate::{
    Args, Command, CommandError, CommandId, Context, Param, Registry, Request, Transaction, Type,
    Value, When,
};

/// Selects notes in the piano roll.
pub const SELECT: CommandId = CommandId::new("notes.select");
/// Adds a note.
pub const ADD: CommandId = CommandId::new("notes.add");
/// Adds notes along a drag, as Draw Mode does.
pub const DRAW: CommandId = CommandId::new("notes.draw");
/// Deletes notes.
pub const DELETE: CommandId = CommandId::new("notes.delete");
/// Moves notes in time or to other keys, or copies them.
pub const MOVE: CommandId = CommandId::new("notes.move");
/// Moves notes' starts or ends.
pub const TRIM: CommandId = CommandId::new("notes.trim");
/// Sets how hard notes start, or changes it.
pub const SET_VELOCITY: CommandId = CommandId::new("notes.set_velocity");
/// Moves notes to the grid.
pub const QUANTIZE: CommandId = CommandId::new("notes.quantize");
/// Shows a MIDI clip in the piano roll.
pub const OPEN: CommandId = CommandId::new("notes.open");
/// Moves the selected notes up a semitone.
pub const TRANSPOSE_UP: CommandId = CommandId::new("notes.transpose_up");
/// Moves the selected notes down a semitone.
pub const TRANSPOSE_DOWN: CommandId = CommandId::new("notes.transpose_down");
/// Moves the selected notes up an octave.
pub const OCTAVE_UP: CommandId = CommandId::new("notes.octave_up");
/// Moves the selected notes down an octave.
pub const OCTAVE_DOWN: CommandId = CommandId::new("notes.octave_down");
/// Moves the selected notes a step of the grid earlier.
pub const NUDGE_LEFT: CommandId = CommandId::new("notes.nudge_left");
/// Moves the selected notes a step of the grid later.
pub const NUDGE_RIGHT: CommandId = CommandId::new("notes.nudge_right");
/// Makes the selected notes a step of the grid longer.
pub const LENGTHEN: CommandId = CommandId::new("notes.lengthen");
/// Makes the selected notes a step of the grid shorter.
pub const SHORTEN: CommandId = CommandId::new("notes.shorten");
/// Makes the selected notes start harder.
pub const VELOCITY_UP: CommandId = CommandId::new("notes.velocity_up");
/// Makes the selected notes start softer.
pub const VELOCITY_DOWN: CommandId = CommandId::new("notes.velocity_down");
/// Turns the piano roll's Draw Mode on or off.
pub const TOGGLE_DRAW: CommandId = CommandId::new("notes.toggle_draw");

/// How hard a new note starts: 100 out of 127, as in Live.
pub const VELOCITY: f32 = 100.0 / 127.0;
/// How much Velocity Up and Down change velocities by: 10 out of 127, as in
/// Live.
pub const VELOCITY_STEP: f32 = 10.0 / 127.0;

pub(crate) const CLIP: Param = Param::optional(
    "clip",
    Type::Clip,
    "The MIDI clip: the one the piano roll shows, without it.",
);
const NOTES: Param = Param::required("notes", Type::NOTES, "The notes, by their ids in the clip.");
const LENGTH: Param = Param::optional(
    "length",
    Type::Beats,
    "How long it lasts: a step of the piano roll's grid where it starts, without it.",
);
const VELOCITY_PARAM: Param = Param::optional(
    "velocity",
    Type::Number,
    "How hard it starts, from 0 to 1: 100 out of 127, without it.",
);

type Handler = fn(&mut Context<'_>);

/// The note commands that take no arguments, for keys, with their titles,
/// when they apply and what they run.
const COMMANDS: [(CommandId, &str, When, Handler); 11] = [
    (TRANSPOSE_UP, "Transpose Up", When::NotesSelected, |ctx| {
        transpose(ctx, "Transpose Up", 1)
    }),
    (
        TRANSPOSE_DOWN,
        "Transpose Down",
        When::NotesSelected,
        |ctx| transpose(ctx, "Transpose Down", -1),
    ),
    (OCTAVE_UP, "Octave Up", When::NotesSelected, |ctx| {
        transpose(ctx, "Octave Up", 12);
    }),
    (OCTAVE_DOWN, "Octave Down", When::NotesSelected, |ctx| {
        transpose(ctx, "Octave Down", -12);
    }),
    (NUDGE_LEFT, "Nudge Notes Left", When::NotesSelected, |ctx| {
        nudge(ctx, "Nudge Notes Left", -1)
    }),
    (
        NUDGE_RIGHT,
        "Nudge Notes Right",
        When::NotesSelected,
        |ctx| nudge(ctx, "Nudge Notes Right", 1),
    ),
    (LENGTHEN, "Lengthen Notes", When::NotesSelected, |ctx| {
        lengthen(ctx, "Lengthen Notes", 1);
    }),
    (SHORTEN, "Shorten Notes", When::NotesSelected, |ctx| {
        lengthen(ctx, "Shorten Notes", -1);
    }),
    (VELOCITY_UP, "Velocity Up", When::NotesSelected, |ctx| {
        change_velocity(ctx, "Velocity Up", VELOCITY_STEP);
    }),
    (VELOCITY_DOWN, "Velocity Down", When::NotesSelected, |ctx| {
        change_velocity(ctx, "Velocity Down", -VELOCITY_STEP);
    }),
    (TOGGLE_DRAW, "Draw Mode", When::Always, |ctx| {
        let roll = &mut ctx.document.editor_mut().piano_roll;
        roll.draw = !roll.draw;
    }),
];

/// Registers the note commands.
pub fn register(registry: &mut Registry) {
    for (id, title, when, run) in COMMANDS {
        registry.add(Command::new(id, title).when(when), move |ctx, _| {
            run(ctx);
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(SELECT, "Select Notes").with_params(&[
            Param::required("notes", Type::NOTES, "The notes to select, by their ids."),
            CLIP,
            Param::optional(
                "toggle",
                Type::Bool,
                "Adds the notes to the selection, or takes away those already in it, \
                 instead of selecting just them.",
            ),
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let notes: Vec<NoteId> = args.get("notes")?;
            let toggle = args.opt("toggle")?.unwrap_or(false);
            let (project, editor) = ctx.document.parts();
            let (c, midi) = midi(project, clip)?;
            find_notes(midi, clip, &notes)?;
            editor.open_in_piano_roll(c);
            let selected = &mut editor.piano_roll.notes;
            if !toggle {
                selected.clear();
            }
            for note in notes {
                if !toggle || !selected.remove(&note) {
                    selected.insert(note);
                }
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(ADD, "Add Note").with_params(&[
            Param::required(
                "start",
                Type::Beats,
                "Where it starts, in the clip's own time.",
            ),
            Param::required(
                "key",
                Type::Int,
                "Its MIDI note number, from 0 to 127: 60 is middle C.",
            ),
            LENGTH,
            VELOCITY_PARAM,
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let start: Beats = args.get("start")?;
            let key = key(args.get("key")?)?;
            let velocity = velocity(args)?;
            let added = add_notes(ctx, clip, "Add Note", &[(start, key)], args, velocity)?;
            Ok(added.first().map_or(Value::None, |&id| Value::Note(id)))
        },
    );
    registry.add(
        Command::new(DRAW, "Draw Notes").with_params(&[
            Param::required(
                "starts",
                Type::List(&Type::Beats),
                "Where each starts, in the clip's own time.",
            ),
            Param::required(
                "keys",
                Type::List(&Type::Int),
                "Each one's MIDI note number, from 0 to 127, in the same order.",
            ),
            LENGTH,
            VELOCITY_PARAM,
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let starts: Vec<Beats> = args.get("starts")?;
            let keys: Vec<i64> = args.get("keys")?;
            if starts.len() != keys.len() {
                return Err(CommandError::invalid("each note needs a start and a key"));
            }
            let keys = keys.into_iter().map(key).collect::<Result<Vec<u8>, _>>()?;
            let velocity = velocity(args)?;
            let notes: Vec<(Beats, u8)> = starts.into_iter().zip(keys).collect();
            let added = add_notes(ctx, clip, "Draw Notes", &notes, args, velocity)?;
            Ok(added.into())
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Notes").with_params(&[NOTES, CLIP]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let notes: Vec<NoteId> = args.get("notes")?;
            let (_, m) = midi(ctx.document.project(), clip)?;
            find_notes(m, clip, &notes)?;
            ctx.document.transact("Delete Notes", |tx| {
                for note in notes {
                    tx.remove_note(clip, note);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(MOVE, "Move Notes").with_params(&[
            NOTES,
            Param::optional("by", Type::Beats, "How far later, or earlier if negative."),
            Param::optional(
                "keys",
                Type::Int,
                "How many semitones up, or down if negative.",
            ),
            Param::optional(
                "copy",
                Type::Bool,
                "Copies the notes instead of moving them.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let notes: Vec<NoteId> = args.get("notes")?;
            let by = args.opt("by")?.unwrap_or(Beats::ZERO);
            let keys = args.opt("keys")?.unwrap_or(0);
            let copy = args.opt("copy")?.unwrap_or(false);
            let (_, m) = midi(ctx.document.project(), clip)?;
            find_notes(m, clip, &notes)?;
            let name = if copy { "Copy Notes" } else { "Move Notes" };
            let moved = ctx
                .document
                .transact(name, |tx| move_notes(tx, clip, &notes, by, keys, copy));
            // Copies are selected, as in Live.
            select(ctx, clip, moved.iter().copied());
            Ok(moved.into())
        },
    );
    registry.add(
        Command::new(TRIM, "Trim Notes").with_params(&[
            NOTES,
            Param::optional(
                "start_by",
                Type::Beats,
                "How far later each note starts, or earlier if negative, keeping its end.",
            ),
            Param::optional(
                "end_by",
                Type::Beats,
                "How far later each note ends, or earlier if negative.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let notes: Vec<NoteId> = args.get("notes")?;
            let start_by = args.opt("start_by")?.unwrap_or(Beats::ZERO);
            let end_by = args.opt("end_by")?.unwrap_or(Beats::ZERO);
            let (_, m) = midi(ctx.document.project(), clip)?;
            find_notes(m, clip, &notes)?;
            ctx.document.transact("Trim Notes", |tx| {
                trim_notes(tx, clip, &notes, start_by, end_by);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_VELOCITY, "Set Velocity").with_params(&[
            NOTES,
            Param::optional(
                "velocity",
                Type::Number,
                "How hard they start from now on, from 0 to 1.",
            ),
            Param::optional(
                "by",
                Type::Number,
                "How much harder they start, or softer if negative, after `velocity`. \
                 Velocities stop at 0 and 1.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let notes: Vec<NoteId> = args.get("notes")?;
            let velocity: Option<f64> = args.opt("velocity")?;
            let by: f64 = args.opt("by")?.unwrap_or(0.0);
            let (_, m) = midi(ctx.document.project(), clip)?;
            find_notes(m, clip, &notes)?;
            ctx.document.transact("Set Velocity", |tx| {
                set_velocities(tx, clip, &notes, velocity.map(|v| v as f32), by as f32);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(QUANTIZE, "Quantize")
            .when(When::CanQuantize)
            .with_params(&[
                Param::optional(
                    "notes",
                    Type::NOTES,
                    "The notes. Without them, the notes selected in the piano roll, or all \
                     of the clip's if none are; or, while the arrangement has the focus, \
                     all the notes of the selected clips.",
                ),
                CLIP,
                Param::optional(
                    "amount",
                    Type::Number,
                    "How far towards the grid each note moves, from 0 to 1: all the way, \
                     without it.",
                ),
                Param::optional(
                    "ends",
                    Type::Bool,
                    "Moves the notes' ends to the grid too, not just their starts.",
                ),
            ]),
        |ctx, args| {
            let amount: f64 = args.opt("amount")?.unwrap_or(1.0);
            if !(0.0..=1.0).contains(&amount) {
                return Err(CommandError::invalid("the amount goes from 0 to 1"));
            }
            let ends = args.opt("ends")?.unwrap_or(false);
            let notes: Option<Vec<NoteId>> = args.opt("notes")?;
            let editor = ctx.document.editor();
            let arranging = editor.focus == Focus::Arrangement
                && notes.is_none()
                && args.value("clip").is_none();
            let (targets, zoom) = if arranging {
                let project = ctx.document.project();
                let clips: Vec<(ClipId, Vec<NoteId>)> = editor
                    .selection
                    .clips
                    .iter()
                    .filter_map(|&id| {
                        let (_, m) = midi(project, id).ok()?;
                        Some((id, m.notes.iter().map(|n| n.id).collect()))
                    })
                    .collect();
                (clips, editor.viewport.zoom)
            } else {
                let clip = clip_arg(ctx, args)?;
                let (_, m) = midi(ctx.document.project(), clip)?;
                let roll = &ctx.document.editor().piano_roll;
                let notes = match notes {
                    Some(notes) => {
                        find_notes(m, clip, &notes)?;
                        notes
                    }
                    None if roll.clip == Some(clip) && !roll.notes.is_empty() => {
                        roll.notes.iter().copied().collect()
                    }
                    None => m.notes.iter().map(|n| n.id).collect(),
                };
                (vec![(clip, notes)], roll.viewport.zoom)
            };
            ctx.document.transact("Quantize", |tx| {
                for (clip, notes) in &targets {
                    quantize(tx, *clip, notes, zoom, amount, ends);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(OPEN, "Open in Piano Roll").with_params(&[Param::required(
            "clip",
            Type::Clip,
            "A MIDI clip.",
        )]),
        |ctx, args| {
            let clip = args.get("clip")?;
            let (project, editor) = ctx.document.parts();
            let (c, _) = midi(project, clip)?;
            editor.open_in_piano_roll(c);
            ctx.requests.push(Request::ShowPanel("piano_roll"));
            Ok(Value::None)
        },
    );
}

/// A MIDI note number, if it is one.
fn key(key: i64) -> Result<u8, CommandError> {
    u8::try_from(key)
        .ok()
        .filter(|&key| key <= Note::MAX_KEY)
        .ok_or_else(|| CommandError::invalid("a key goes from 0 to 127"))
}

/// The `velocity` argument, or else 100 out of 127.
fn velocity(args: &Args) -> Result<f32, CommandError> {
    let velocity: f64 = args.opt("velocity")?.unwrap_or(f64::from(VELOCITY));
    if !(0.0..=1.0).contains(&velocity) {
        return Err(CommandError::invalid("a velocity goes from 0 to 1"));
    }
    Ok(velocity as f32)
}

/// Adds a note at each of `notes`' starts, in the clip's own time, on its
/// key, as one transaction called `name`, and selects them. They last as
/// long as the `length` argument, or else a step of the grid where each
/// starts. Returns their ids.
fn add_notes(
    ctx: &mut Context<'_>,
    clip: ClipId,
    name: &str,
    notes: &[(Beats, u8)],
    args: &Args,
    velocity: f32,
) -> Result<Vec<NoteId>, CommandError> {
    let length: Option<Beats> = args.opt("length")?;
    if length.is_some_and(|length| length <= Beats::ZERO) {
        return Err(CommandError::invalid("a note lasts more than zero"));
    }
    let lengths: Vec<Beats> = {
        let (c, m) = midi(ctx.document.project(), clip)?;
        let (project, editor) = (ctx.document.project(), ctx.document.editor());
        let step = |at| step(project, editor, c, m, at);
        notes
            .iter()
            .map(|&(start, _)| length.unwrap_or_else(|| step(start)))
            .collect()
    };
    let added = ctx.document.transact(name, |tx| {
        let mut added = Vec::with_capacity(notes.len());
        for (&(start, key), length) in notes.iter().zip(lengths) {
            let Some(id) = tx.new_note_id(clip) else {
                break;
            };
            let note = Note {
                id,
                start,
                length,
                key,
                velocity,
                release_velocity: Note::RELEASE_VELOCITY,
                expression: Expression::default(),
            };
            tx.add_note(clip, usize::MAX, note);
            added.push(id);
        }
        added
    });
    select(ctx, clip, added.iter().copied());
    Ok(added)
}

/// The clip the `clip` argument names, or else the one the piano roll
/// shows, if it's a MIDI clip on a track.
pub(crate) fn clip_arg(ctx: &Context<'_>, args: &Args) -> Result<ClipId, CommandError> {
    let clip = match args.opt("clip")? {
        Some(clip) => clip,
        None => ctx
            .document
            .editor()
            .piano_roll
            .clip
            .ok_or_else(|| CommandError::invalid("the piano roll shows no clip"))?,
    };
    midi(ctx.document.project(), clip)?;
    Ok(clip)
}

/// The clip with id `id` on one of the project's tracks, and its notes, if
/// it's a MIDI clip.
pub(crate) fn midi(project: &Project, id: ClipId) -> Result<(&Clip, &MidiClip), CommandError> {
    let (_, clip) = project
        .find_clip(id)
        .ok_or_else(|| CommandError::invalid(format!("there's no clip {}", id.0)))?;
    match &clip.content {
        ClipContent::Midi(midi) => Ok((clip, midi)),
        ClipContent::Audio(_) => Err(CommandError::invalid(format!(
            "clip {} isn't a MIDI clip",
            id.0
        ))),
    }
}

/// An error if one of `notes` isn't in `midi`, the clip with id `clip`.
fn find_notes(midi: &MidiClip, clip: ClipId, notes: &[NoteId]) -> Result<(), CommandError> {
    match notes.iter().find(|&&id| midi.note(id).is_none()) {
        Some(note) => Err(CommandError::invalid(format!(
            "clip {} has no note {}",
            clip.0, note.0
        ))),
        None => Ok(()),
    }
}

/// Where `at`, in `clip`'s own time, plays in the song. `midi` is the
/// clip's content.
pub fn song_position(clip: &Clip, midi: &MidiClip, at: Beats) -> Beats {
    clip.start + (at - midi.offset)
}

/// The moment in `clip`'s own time that plays at the song position `at`.
pub fn clip_position(clip: &Clip, midi: &MidiClip, at: Beats) -> Beats {
    at - clip.start + midi.offset
}

/// A step of the piano roll's grid at `at` in `clip`'s own time, at the
/// piano roll's zoom.
fn step(project: &Project, editor: &Editor, clip: &Clip, midi: &MidiClip, at: Beats) -> Beats {
    let at = song_position(clip, midi, at).max(Beats::ZERO);
    let zoom = editor.piano_roll.viewport.zoom;
    editor.grid.step_at(&project.tempo, at, zoom)
}

/// Selects just `notes` of `clip` in the piano roll, and gives it the focus.
fn select(ctx: &mut Context<'_>, clip: ClipId, notes: impl IntoIterator<Item = NoteId>) {
    let (project, editor) = ctx.document.parts();
    if let Ok((c, _)) = midi(project, clip) {
        editor.open_in_piano_roll(c);
        editor.piano_roll.notes = notes.into_iter().collect();
    }
}

/// The notes with ids `ids` in the clip with id `clip`, as they are now.
fn notes_of(tx: &Transaction<'_>, clip: ClipId, ids: &[NoteId]) -> Vec<Note> {
    let Ok((_, m)) = midi(tx.project(), clip) else {
        return Vec::new();
    };
    ids.iter().filter_map(|&id| m.note(id).cloned()).collect()
}

/// Moves the notes with ids `ids` in the clip with id `clip` by `by` and
/// `keys` semitones up, or copies them there with `copy`, and returns the
/// ids of the notes where they landed. They keep their places relative to
/// each other: a move that would take one before the song's start stops it
/// there, and one that would take one past the lowest or the highest key
/// stops it at that key.
pub fn move_notes(
    tx: &mut Transaction<'_>,
    clip: ClipId,
    ids: &[NoteId],
    by: Beats,
    keys: i64,
    copy: bool,
) -> Vec<NoteId> {
    let notes = notes_of(tx, clip, ids);
    let Ok((c, m)) = midi(tx.project(), clip) else {
        return Vec::new();
    };
    let Some(earliest) = notes.iter().map(|n| n.start).min() else {
        return Vec::new();
    };
    let by = by.max(-song_position(c, m, earliest).max(Beats::ZERO));
    let (low, high) = notes.iter().fold((Note::MAX_KEY, 0), |(low, high), n| {
        (low.min(n.key), high.max(n.key))
    });
    let keys = keys.clamp(-i64::from(low), i64::from(Note::MAX_KEY - high));
    let key = |note: &Note| (i64::from(note.key) + keys) as u8;
    let mut landed = Vec::with_capacity(notes.len());
    for note in notes {
        let range = BeatRange::new(note.start + by, note.end() + by);
        if copy {
            let Some(id) = tx.new_note_id(clip) else {
                continue;
            };
            let copy = Note {
                id,
                start: range.start,
                key: key(&note),
                ..note
            };
            tx.add_note(clip, usize::MAX, copy);
            landed.push(id);
        } else {
            tx.set_note_timing(clip, note.id, range);
            tx.set_note_key(clip, note.id, key(&note));
            landed.push(note.id);
        }
    }
    landed
}

/// Moves the starts of the notes with ids `ids` by `start_by`, keeping
/// their ends, and their ends by `end_by`. A note never gets shorter than a
/// tick, and a start never goes before the song's start.
pub fn trim_notes(
    tx: &mut Transaction<'_>,
    clip: ClipId,
    ids: &[NoteId],
    start_by: Beats,
    end_by: Beats,
) {
    let notes = notes_of(tx, clip, ids);
    let Ok((c, m)) = midi(tx.project(), clip) else {
        return;
    };
    let song_start = clip_position(c, m, Beats::ZERO);
    let tick = Beats::from_ticks(1);
    for note in notes {
        let mut start = (note.start + start_by).min(note.end() - tick);
        if start_by < Beats::ZERO {
            start = start.max(song_start.min(note.start));
        }
        let end = (note.end() + end_by).max(start + tick);
        tx.set_note_timing(clip, note.id, BeatRange::new(start, end));
    }
}

/// Sets the velocities of the notes with ids `ids` to `velocity`, or keeps
/// them, and changes them by `by`.
fn set_velocities(
    tx: &mut Transaction<'_>,
    clip: ClipId,
    ids: &[NoteId],
    velocity: Option<f32>,
    by: f32,
) {
    for note in notes_of(tx, clip, ids) {
        let velocity = velocity.unwrap_or(note.velocity) + by;
        tx.set_note_velocity(clip, note.id, velocity);
    }
}

/// Moves the starts of the notes with ids `ids` towards the nearest line of
/// the grid at `zoom`, by `amount` of the way, and with `ends` their ends
/// too. A note whose end would come at its start or before keeps its
/// length.
pub fn quantize(
    tx: &mut Transaction<'_>,
    clip: ClipId,
    ids: &[NoteId],
    zoom: f64,
    amount: f64,
    ends: bool,
) {
    let notes = notes_of(tx, clip, ids);
    let grid = tx.editor().grid;
    let Ok((c, m)) = midi(tx.project(), clip) else {
        return;
    };
    let tempo = &tx.project().tempo;
    let towards = |at: Beats| {
        let song = song_position(c, m, at);
        let line = grid.nearest(tempo, song, zoom);
        let moved = ((line - song).ticks() as f64 * amount).round() as i64;
        at + Beats::from_ticks(moved)
    };
    let quantized: Vec<(NoteId, BeatRange)> = notes
        .iter()
        .map(|note| {
            let start = towards(note.start);
            let end = match ends {
                true => Some(towards(note.end())).filter(|&end| end > start),
                false => None,
            };
            let end = end.unwrap_or(start + note.length);
            (note.id, BeatRange::new(start, end))
        })
        .collect();
    for (note, range) in quantized {
        tx.set_note_timing(clip, note, range);
    }
}

/// The piano roll's clip and its selected notes, while it has the focus and
/// some are selected.
fn selected(ctx: &Context<'_>) -> Option<(ClipId, Vec<NoteId>)> {
    let editor = ctx.document.editor();
    let roll = &editor.piano_roll;
    let clip = roll.clip.filter(|_| editor.focus == Focus::PianoRoll)?;
    (!roll.notes.is_empty()).then(|| (clip, roll.notes.iter().copied().collect()))
}

/// Transposes the selected notes by `keys` semitones, as a transaction
/// called `name`.
fn transpose(ctx: &mut Context<'_>, name: &str, keys: i64) {
    let Some((clip, notes)) = selected(ctx) else {
        return;
    };
    ctx.document.transact(name, |tx| {
        move_notes(tx, clip, &notes, Beats::ZERO, keys, false);
    });
}

/// Moves the selected notes by `steps` steps of the grid, as long as the
/// step where the first of them starts.
fn nudge(ctx: &mut Context<'_>, name: &str, steps: i64) {
    let Some((clip, notes)) = selected(ctx) else {
        return;
    };
    let project = ctx.document.project();
    let Ok((c, m)) = midi(project, clip) else {
        return;
    };
    let Some(first) = notes
        .iter()
        .filter_map(|&id| m.note(id))
        .map(|n| n.start)
        .min()
    else {
        return;
    };
    let by = step(project, ctx.document.editor(), c, m, first) * steps;
    ctx.document.transact(name, |tx| {
        move_notes(tx, clip, &notes, by, 0, false);
    });
}

/// Makes each selected note `steps` steps of the grid longer, or shorter.
/// A note no longer than the steps it would lose keeps its length.
fn lengthen(ctx: &mut Context<'_>, name: &str, steps: i64) {
    let Some((clip, ids)) = selected(ctx) else {
        return;
    };
    let project = ctx.document.project();
    let editor = ctx.document.editor();
    let Ok((c, m)) = midi(project, clip) else {
        return;
    };
    let changes: Vec<(NoteId, BeatRange)> = ids
        .iter()
        .filter_map(|&id| m.note(id))
        .filter_map(|note| {
            let by = step(project, editor, c, m, note.end()) * steps;
            let length = note.length + by;
            (length > Beats::ZERO)
                .then(|| (note.id, BeatRange::new(note.start, note.start + length)))
        })
        .collect();
    ctx.document.transact(name, |tx| {
        for (note, range) in changes {
            tx.set_note_timing(clip, note, range);
        }
    });
}

/// Changes the selected notes' velocities by `by`.
fn change_velocity(ctx: &mut Context<'_>, name: &str, by: f32) {
    let Some((clip, notes)) = selected(ctx) else {
        return;
    };
    ctx.document.transact(name, |tx| {
        set_velocities(tx, clip, &notes, None, by);
    });
}

/// The selected notes, as Copy takes them in the piano roll.
fn taken(project: &Project, editor: &Editor) -> Option<NoteClipboard> {
    let roll = &editor.piano_roll;
    let (_, m) = midi(project, roll.clip?).ok()?;
    let notes: Vec<Note> = m
        .notes
        .iter()
        .filter(|n| roll.notes.contains(&n.id))
        .cloned()
        .collect();
    let start = notes.iter().map(|n| n.start).min()?;
    let end = notes.iter().map(|n| n.end()).max()?;
    Some(NoteClipboard {
        notes,
        range: BeatRange::new(start, end),
    })
}

/// Copies the selected notes, for Paste.
pub(crate) fn copy(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    if let Some(clipboard) = taken(project, editor) {
        editor.piano_roll.clipboard = Some(clipboard);
    }
}

/// Copies the selected notes, then deletes them.
pub(crate) fn cut(ctx: &mut Context<'_>) {
    copy(ctx);
    delete_selected(ctx, "Cut");
}

/// Deletes the selected notes.
pub(crate) fn delete(ctx: &mut Context<'_>) {
    delete_selected(ctx, "Delete");
}

fn delete_selected(ctx: &mut Context<'_>, name: &str) {
    let roll = &ctx.document.editor().piano_roll;
    let Some(clip) = roll.clip else {
        return;
    };
    let notes: Vec<NoteId> = roll.notes.iter().copied().collect();
    ctx.document.transact(name, |tx| {
        for note in notes {
            tx.remove_note(clip, note);
        }
    });
}

/// Puts copies of `clipboard`'s notes into the clip with id `clip`, `by`
/// later, and returns their ids.
fn add_copies(
    tx: &mut Transaction<'_>,
    clip: ClipId,
    clipboard: &NoteClipboard,
    by: Beats,
) -> Vec<NoteId> {
    let mut ids = Vec::with_capacity(clipboard.notes.len());
    for note in &clipboard.notes {
        let Some(id) = tx.new_note_id(clip) else {
            break;
        };
        let copy = Note {
            id,
            start: note.start + by,
            ..note.clone()
        };
        tx.add_note(clip, usize::MAX, copy);
        ids.push(id);
    }
    ids
}

/// Puts what Cut or Copy took in the piano roll at the start marker, in the
/// clip it shows, selects it, and moves the start marker to its end.
pub(crate) fn paste(ctx: &mut Context<'_>) {
    let editor = ctx.document.editor();
    let roll = &editor.piano_roll;
    let (Some(clip), Some(clipboard)) = (roll.clip, roll.clipboard.clone()) else {
        return;
    };
    let Ok((c, m)) = midi(ctx.document.project(), clip) else {
        return;
    };
    let at = editor.start_marker;
    let by = clip_position(c, m, at) - clipboard.range.start;
    let pasted = ctx
        .document
        .transact("Paste", |tx| add_copies(tx, clip, &clipboard, by));
    select(ctx, clip, pasted);
    ctx.document.editor_mut().start_marker = at + clipboard.range.len();
}

/// Puts a copy of the selected notes just after them, and selects it.
pub(crate) fn duplicate(ctx: &mut Context<'_>) {
    let (project, editor) = (ctx.document.project(), ctx.document.editor());
    let (Some(clip), Some(clipboard)) = (editor.piano_roll.clip, taken(project, editor)) else {
        return;
    };
    let by = clipboard.range.len();
    let copies = ctx
        .document
        .transact("Duplicate", |tx| add_copies(tx, clip, &clipboard, by));
    select(ctx, clip, copies);
}

/// Selects all the notes of the clip the piano roll shows.
pub(crate) fn select_all(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    let Some(clip) = editor.piano_roll.clip else {
        return;
    };
    if let Ok((_, m)) = midi(project, clip) {
        editor.piano_roll.notes = m.notes.iter().map(|n| n.id).collect();
    }
}

/// The song positions the selected notes sound over, from the first one's
/// start to the last one's end, with the lowest and the highest of their
/// keys, for zooming to them.
pub(crate) fn selected_range(project: &Project, editor: &Editor) -> Option<(BeatRange, u8, u8)> {
    let roll = &editor.piano_roll;
    let (c, m) = midi(project, roll.clip?).ok()?;
    let notes = m.notes.iter().filter(|n| roll.notes.contains(&n.id));
    notes.fold(None, |range, note| {
        let start = song_position(c, m, note.start);
        let end = song_position(c, m, note.end());
        Some(match range {
            None => (BeatRange::new(start, end), note.key, note.key),
            Some((range, low, high)) => (
                BeatRange::new(range.start.min(start), range.end.max(end)),
                low.min(note.key),
                high.max(note.key),
            ),
        })
    })
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::AudioClip;
    use musicable_model::{TrackId, TrackKind};

    use super::*;
    use crate::editor::Viewport;
    use crate::testing::Setup;
    use crate::{Keymap, edit, view};

    /// Keys, a MIDI track with a clip from beat 4 to 12 that starts playing
    /// a beat into its own time, and Vox, an audio track with a clip. The
    /// MIDI clip's notes, in its own time: C3 (60) from 1 for a beat, at
    /// velocity 0.5; E3 (64) from 2.25 for half a beat, at 0.75; and G3 (67)
    /// from 4 for two beats, at 1.
    fn roll() -> (Setup, [TrackId; 2], [ClipId; 2], [NoteId; 3]) {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let vox = project.add_track("Vox", TrackKind::Audio);
        let mut midi = MidiClip {
            offset: Beats::new(1),
            ..MidiClip::default()
        };
        let notes = [
            (1.0, 1.0, 60, 0.5),
            (2.25, 0.5, 64, 0.75),
            (4.0, 2.0, 67, 1.0),
        ]
        .map(|(start, length, key, velocity)| {
            let (start, length) = (Beats::from_f64(start), Beats::from_f64(length));
            midi.add_note(start, length, key, velocity).id
        });
        let range = BeatRange::new(Beats::new(4), Beats::new(12));
        let k = project.new_clip("K", range, ClipContent::Midi(midi));
        let audio = ClipContent::Audio(AudioClip::new("v.wav"));
        let v = project.new_clip("V", range, audio);
        let ids = [k.id, v.id];
        project.track_mut(keys).unwrap().clips.push(k);
        project.track_mut(vox).unwrap().clips.push(v);
        (Setup::new(project), [keys, vox], ids, notes)
    }

    /// The MIDI clip's notes, as (id, start, length, key, velocity), by id.
    fn notes(setup: &Setup, clip: ClipId) -> Vec<(u32, f64, f64, u8, f32)> {
        let (_, m) = midi(setup.document.project(), clip).unwrap();
        let mut notes: Vec<_> = m
            .notes
            .iter()
            .map(|n| {
                (
                    n.id.0,
                    n.start.to_f64(),
                    n.length.to_f64(),
                    n.key,
                    n.velocity,
                )
            })
            .collect();
        notes.sort_by_key(|n| n.0);
        notes
    }

    fn select(setup: &mut Setup, clip: ClipId, notes: &[NoteId]) {
        let args = Args::new().with("notes", notes.to_vec()).with("clip", clip);
        setup.run_with(&SELECT, args).unwrap();
    }

    fn selected(setup: &Setup) -> Vec<u32> {
        let roll = &setup.document.editor().piano_roll;
        roll.notes.iter().map(|n| n.0).collect()
    }

    #[test]
    fn notes_are_added_moved_trimmed_and_deleted_as_steps_in_the_undo_history() {
        let (mut setup, _, [k, _], [c, e, g]) = roll();
        let before = notes(&setup, k);
        select(&mut setup, k, &[]);
        let editor = setup.document.editor();
        assert_eq!(editor.focus, Focus::PianoRoll);
        assert_eq!(editor.piano_roll.clip, Some(k));
        // Showing the clip zooms to its 8 beats, so the grid steps by
        // sixteenths: a note drawn without a length lasts one.
        let args = Args::new().with("start", Beats::new(3)).with("key", 62_i64);
        let added = setup.run_with(&ADD, args).unwrap();
        assert_eq!(added, Value::Note(NoteId(3)));
        assert_eq!(notes(&setup, k)[3], (3, 3.0, 0.25, 62, VELOCITY));
        assert_eq!(selected(&setup), [3]);

        // Up a tone and a beat later, keeping their places.
        let args = Args::new()
            .with("notes", vec![c, NoteId(3)])
            .with("by", Beats::new(1))
            .with("keys", 2_i64);
        let moved = setup.run_with(&MOVE, args).unwrap();
        assert_eq!(moved, Value::from(vec![c, NoteId(3)]));
        // Copies are new notes, and selected.
        let args = Args::new()
            .with("notes", vec![e])
            .with("by", Beats::new(-1))
            .with("copy", true);
        let copied = setup.run_with(&MOVE, args).unwrap();
        assert_eq!(copied, Value::from(vec![NoteId(4)]));
        assert_eq!(selected(&setup), [4]);
        let args = Args::new()
            .with("notes", vec![g])
            .with("start_by", Beats::from_f64(0.5))
            .with("end_by", Beats::from_f64(-0.5));
        setup.run_with(&TRIM, args).unwrap();
        let args = Args::new()
            .with("notes", vec![e, g])
            .with("velocity", 0.25)
            .with("by", 0.5);
        setup.run_with(&SET_VELOCITY, args).unwrap();
        assert_eq!(
            notes(&setup, k),
            [
                (0, 2.0, 1.0, 62, 0.5),
                (1, 2.25, 0.5, 64, 0.75),
                (2, 4.5, 1.0, 67, 0.75),
                (3, 4.0, 0.25, 64, VELOCITY),
                (4, 1.25, 0.5, 64, 0.75),
            ]
        );
        setup
            .run_with(&DELETE, Args::new().with("notes", vec![e, NoteId(4)]))
            .unwrap();
        // The deleted copy isn't selected any more.
        assert_eq!(selected(&setup), Vec::<u32>::new());
        assert_eq!(
            setup.committed_names(),
            [
                "Add Note",
                "Move Notes",
                "Copy Notes",
                "Trim Notes",
                "Set Velocity",
                "Delete Notes"
            ]
        );
        for _ in 0..6 {
            setup.run(&edit::UNDO);
        }
        assert_eq!(notes(&setup, k), before);
    }

    #[test]
    fn drawing_adds_notes_as_one_step() {
        let (mut setup, _, [k, _], _) = roll();
        let args = Args::new()
            .with("starts", vec![Beats::new(6), Beats::from_f64(6.25)])
            .with("keys", vec![70_i64, 72])
            .with("clip", k);
        let drawn = setup.run_with(&DRAW, args).unwrap();
        assert_eq!(drawn, Value::from(vec![NoteId(3), NoteId(4)]));
        // A step of the grid long: an eighth, at the zoom the piano roll had
        // before it showed the clip.
        assert_eq!(
            notes(&setup, k)[3..],
            [(3, 6.0, 0.5, 70, VELOCITY), (4, 6.25, 0.5, 72, VELOCITY)]
        );
        assert_eq!(selected(&setup), [3, 4]);
        assert_eq!(setup.committed_names(), ["Draw Notes"]);
        let args = Args::new()
            .with("starts", vec![Beats::new(6)])
            .with("keys", Vec::<i64>::new());
        assert_eq!(
            setup.run_with(&DRAW, args),
            Err(CommandError::invalid("each note needs a start and a key"))
        );
    }

    #[test]
    fn note_commands_check_their_arguments() {
        let (mut setup, _, [k, v], _) = roll();
        let add = |key: i64| Args::new().with("start", Beats::ZERO).with("key", key);
        // Nothing in the piano roll yet, and no clip named.
        assert_eq!(
            setup.run_with(&ADD, add(60)),
            Err(CommandError::invalid("the piano roll shows no clip"))
        );
        assert_eq!(
            setup.run_with(&ADD, add(128).with("clip", k)),
            Err(CommandError::invalid("a key goes from 0 to 127"))
        );
        assert_eq!(
            setup.run_with(&ADD, add(60).with("clip", k).with("velocity", 1.5)),
            Err(CommandError::invalid("a velocity goes from 0 to 1"))
        );
        assert_eq!(
            setup.run_with(&ADD, add(60).with("clip", k).with("length", Beats::ZERO)),
            Err(CommandError::invalid("a note lasts more than zero"))
        );
        assert_eq!(
            setup.run_with(&ADD, add(60).with("clip", v)),
            Err(CommandError::invalid(format!(
                "clip {} isn't a MIDI clip",
                v.0
            )))
        );
        let args = Args::new().with("notes", vec![NoteId(9)]).with("clip", k);
        assert_eq!(
            setup.run_with(&DELETE, args),
            Err(CommandError::invalid(format!("clip {} has no note 9", k.0)))
        );
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn moves_stop_at_the_song_s_start_and_the_ends_of_the_keyboard() {
        let (mut setup, _, [k, _], [c, e, g]) = roll();
        // C3 plays at beat 4: it can go 4 beats earlier, and the others keep
        // their places after it. G3 can go up 60 semitones.
        let args = Args::new()
            .with("notes", vec![c, e, g])
            .with("by", Beats::new(-10))
            .with("keys", 100_i64)
            .with("clip", k);
        setup.run_with(&MOVE, args).unwrap();
        let moved: Vec<_> = notes(&setup, k).iter().map(|n| (n.1, n.3)).collect();
        assert_eq!(moved, [(-3.0, 120), (-1.75, 124), (0.0, 127)]);
        // A start dragged back stops at the song's start too, and a note never
        // gets shorter than a tick.
        let args = Args::new()
            .with("notes", vec![e, g])
            .with("start_by", Beats::new(-10))
            .with("end_by", Beats::new(-10))
            .with("clip", k);
        setup.run_with(&TRIM, args).unwrap();
        let tick = Beats::from_ticks(1).to_f64();
        let trimmed: Vec<_> = notes(&setup, k).iter().map(|n| (n.1, n.2)).collect();
        assert_eq!(trimmed, [(-3.0, 1.0), (-3.0, tick), (-3.0, tick)]);
    }

    #[test]
    fn the_keys_change_the_selected_notes_as_live_s_do() {
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        let expected = [
            (TRANSPOSE_UP, "Up"),
            (TRANSPOSE_DOWN, "Down"),
            (OCTAVE_UP, "Shift+Up"),
            (OCTAVE_DOWN, "Shift+Down"),
            (NUDGE_LEFT, "Left"),
            (NUDGE_RIGHT, "Right"),
            (SHORTEN, "Shift+Left"),
            (LENGTHEN, "Shift+Right"),
            (VELOCITY_UP, "Ctrl+Up"),
            (VELOCITY_DOWN, "Ctrl+Down"),
            (TOGGLE_DRAW, "B"),
            (QUANTIZE, "Ctrl+U"),
        ];
        for (id, chord) in expected {
            assert_eq!(chords(id.clone()), [chord], "{id}");
        }

        let (mut setup, _, [k, _], [c, e, _]) = roll();
        // Nothing selected: the keys do nothing, and go on to the controls.
        assert!(!setup.applies(&TRANSPOSE_UP));
        select(&mut setup, k, &[c, e]);
        assert!(setup.applies(&TRANSPOSE_UP));
        setup.run(&TRANSPOSE_UP);
        setup.run(&OCTAVE_DOWN);
        // A sixteenth, the grid's step at the zoom the clip came up in.
        setup.run(&NUDGE_RIGHT);
        setup.run(&LENGTHEN);
        setup.run(&VELOCITY_UP);
        let step = VELOCITY_STEP;
        assert_eq!(
            notes(&setup, k)[..2],
            [
                (0, 1.25, 1.25, 49, 0.5 + step),
                (1, 2.5, 0.75, 53, 0.75 + step)
            ]
        );
        // E3 is three steps long now: it can lose two of them, but not the
        // last one.
        for _ in 0..4 {
            setup.run(&SHORTEN);
        }
        assert_eq!(notes(&setup, k)[1].2, 0.25);
        assert_eq!(notes(&setup, k)[0].2, 0.25);
        setup.run(&VELOCITY_DOWN);
        setup.run(&TRANSPOSE_DOWN);
        setup.run(&OCTAVE_UP);
        setup.run(&NUDGE_LEFT);
        assert_eq!(
            notes(&setup, k)[..2],
            [(0, 1.0, 0.25, 60, 0.5), (1, 2.25, 0.25, 64, 0.75)]
        );
        // Selecting a clip gives the arrangement the focus, and the keys
        // leave the notes alone.
        setup.select_clips(&[k]);
        assert!(!setup.applies(&TRANSPOSE_UP));
        setup.run(&TOGGLE_DRAW);
        assert!(setup.document.editor().piano_roll.draw);
    }

    #[test]
    fn the_edit_commands_act_on_notes_while_the_piano_roll_has_the_focus() {
        let (mut setup, _, [k, _], [c, e, _]) = roll();
        select(&mut setup, k, &[c, e]);
        assert!(setup.applies(&edit::COPY) && !setup.applies(&edit::PASTE));
        // Split is for clips.
        assert!(!setup.applies(&edit::SPLIT));
        setup.run(&edit::COPY);
        assert!(setup.applies(&edit::PASTE));
        assert_eq!(setup.document.editor().clipboard, None);
        // Paste puts them at the start marker, song position 8: four beats
        // into the clip, past its offset of one, so at 5 in its own time.
        setup.document.editor_mut().start_marker = Beats::new(8);
        setup.run(&edit::PASTE);
        assert_eq!(
            notes(&setup, k)[3..],
            [(3, 5.0, 1.0, 60, 0.5), (4, 6.25, 0.5, 64, 0.75)]
        );
        assert_eq!(selected(&setup), [3, 4]);
        assert_eq!(setup.document.editor().start_marker, Beats::from_f64(9.75));
        // Duplicate puts a copy of the pasted notes just after them.
        setup.run(&edit::DUPLICATE);
        assert_eq!(
            notes(&setup, k)[5..],
            [(5, 6.75, 1.0, 60, 0.5), (6, 8.0, 0.5, 64, 0.75)]
        );
        assert_eq!(selected(&setup), [5, 6]);
        setup.run(&edit::DELETE);
        assert_eq!(notes(&setup, k).len(), 5);
        assert!(!setup.applies(&edit::DELETE));
        setup.run(&edit::SELECT_ALL);
        assert_eq!(selected(&setup), [0, 1, 2, 3, 4]);
        setup.run(&edit::CUT);
        assert_eq!(notes(&setup, k), []);
        assert_eq!(
            setup.committed_names(),
            ["Paste", "Duplicate", "Delete", "Cut"]
        );

        // Selecting the clip gives the arrangement the focus: Copy takes the
        // clip, and the notes wait in the piano roll's clipboard.
        setup.select_clips(&[k]);
        setup.run(&edit::COPY);
        let editor = setup.document.editor();
        assert_eq!(editor.focus, Focus::Arrangement);
        assert_eq!(editor.clipboard.as_ref().map(|c| c.clips.len()), Some(1));
        assert_eq!(
            editor.piano_roll.clipboard.as_ref().map(|c| c.notes.len()),
            Some(5)
        );
    }

    #[test]
    fn quantize_moves_notes_to_the_grid() {
        let (mut setup, _, [k, v], [c, e, g]) = roll();
        // At 30 points a beat, the grid steps by quarter notes.
        let viewport = |setup: &mut Setup, id: &CommandId| {
            let mut args = Args::new()
                .with("start", Beats::ZERO)
                .with("zoom", 30.0)
                .with("top", 0.0)
                .with("width", 900.0);
            if *id == view::SET_PIANO_ROLL_VIEWPORT {
                args = args.with("height", 200.0);
            }
            setup.run_with(id, args).unwrap();
        };
        // Nothing selected in the arrangement, and the piano roll shows no
        // clip.
        assert!(!setup.applies(&QUANTIZE));
        select(&mut setup, k, &[e]);
        viewport(&mut setup, &view::SET_PIANO_ROLL_VIEWPORT);
        // Half way: E3 plays at 5.25, so it moves to 5.125.
        setup
            .run_with(&QUANTIZE, Args::new().with("amount", 0.5))
            .unwrap();
        assert_eq!(notes(&setup, k)[1], (1, 2.125, 0.5, 64, 0.75));
        // All the way, its end too: from 5 to 6.
        setup
            .run_with(&QUANTIZE, Args::new().with("ends", true))
            .unwrap();
        assert_eq!(notes(&setup, k)[1], (1, 2.0, 1.0, 64, 0.75));

        // In the arrangement, the selected MIDI clips' notes, at its zoom.
        // An audio clip has none.
        setup.select_clips(&[v]);
        assert!(!setup.applies(&QUANTIZE));
        setup.document.transact("Move Notes", |tx| {
            for (note, start) in [(c, 1.75), (g, 4.25)] {
                let start = Beats::from_f64(start);
                tx.set_note_timing(k, note, BeatRange::new(start, start + Beats::new(1)));
            }
        });
        setup.select_clips(&[k, v]);
        viewport(&mut setup, &view::SET_VIEWPORT);
        assert_eq!(setup.document.editor().viewport.zoom, 30.0);
        setup.run(&QUANTIZE);
        let starts: Vec<_> = notes(&setup, k).iter().map(|n| n.1).collect();
        assert_eq!(starts, [2.0, 2.0, 4.0]);
        assert_eq!(
            setup.committed_names(),
            ["Quantize", "Quantize", "Move Notes", "Quantize"]
        );
    }

    #[test]
    fn selecting_a_midi_clip_shows_it_in_the_piano_roll() {
        let (mut setup, [keys, _], [k, v], [c, ..]) = roll();
        setup.select_clips(&[k, v]);
        let editor = setup.document.editor();
        // The last MIDI clip selected, without the focus.
        assert_eq!(editor.piano_roll.clip, Some(k));
        assert_eq!(editor.focus, Focus::Arrangement);
        // Its whole length in sight, its notes' keys in the middle.
        let visible = editor.piano_roll.viewport.visible();
        assert!(visible.start < Beats::new(4) && Beats::new(12) < visible.end);
        // Opening it gives the piano roll the focus, and asks for its panel.
        select(&mut setup, k, &[c]);
        setup.run_with(&OPEN, Args::new().with("clip", k)).unwrap();
        assert_eq!(setup.requests, [Request::ShowPanel("piano_roll")]);
        assert_eq!(selected(&setup), [0]);
        // Selecting time or tracks gives the arrangement the focus back.
        setup.select_tracks(&[keys]);
        assert_eq!(setup.document.editor().focus, Focus::Arrangement);
        // The clip going takes it out of the piano roll.
        setup.document.transact("Delete", |tx| {
            tx.remove_clip(k);
        });
        let roll = &setup.document.editor().piano_roll;
        assert_eq!((roll.clip, roll.notes.len()), (None, 0));
        assert_eq!(
            setup.run_with(&OPEN, Args::new().with("clip", v)),
            Err(CommandError::invalid(format!(
                "clip {} isn't a MIDI clip",
                v.0
            )))
        );
        assert!(Viewport::default().zoom > 0.0);
    }
}

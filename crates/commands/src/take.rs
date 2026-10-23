//! Takes: what tracks record, kept in lanes under them, and comping, which
//! copies the best parts of takes to the tracks' own clips.
//!
//! A recording becomes a take for each pass the playhead made over the
//! song while it recorded: a loop recorded four times makes four takes.
//! Each pass is comped onto its track as it's added, later passes over
//! earlier ones, so the track plays the last pass, as a tape machine
//! would, and comping by hand picks other parts from there. A MIDI track's
//! pass holds the notes played and how the controllers moved, such as the
//! sustain pedal. One without notes makes no take.

use std::collections::BTreeSet;

use musicable_model::clip::{
    AudioClip, Clip, ClipContent, ClipId, Controller, Dimension, MidiClip, Point,
};
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{TakeId, TrackId, TrackKind};

use crate::arrange::{after_frames, clear, crop};
use crate::{
    Command, CommandError, CommandId, Context, Document, Param, Registry, Transaction, Type, Value,
};

/// Copies a stretch of a take to its track, replacing what the track has
/// there.
pub const COMP: CommandId = CommandId::new("take.comp");
/// Deletes a take.
pub const DELETE: CommandId = CommandId::new("take.delete");
/// Names a take.
pub const RENAME: CommandId = CommandId::new("take.rename");

const TAKE: Param = Param::required("take", Type::Take, "The take.");

/// Registers the take commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(COMP, "Comp").with_params(&[
            TAKE,
            Param::required("start", Type::Beats, "Where the stretch to copy starts."),
            Param::required("end", Type::Beats, "Where it ends."),
        ]),
        |ctx, args| {
            let take = existing(ctx, args.get("take")?)?;
            let (a, b): (Beats, Beats) = (args.get("start")?, args.get("end")?);
            let range = BeatRange::new(a.min(b).max(Beats::ZERO), a.max(b).max(Beats::ZERO));
            if range.is_empty() {
                return Err(CommandError::invalid("a comp must end after it starts"));
            }
            let clips = ctx.document.transact("Comp", |tx| comp(tx, take, range));
            Ok(clips.into())
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Take").with_params(&[TAKE]),
        |ctx, args| {
            let take = existing(ctx, args.get("take")?)?;
            ctx.document.transact("Delete Take", |tx| {
                tx.remove_take(take);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RENAME, "Rename Take")
            .with_params(&[TAKE, Param::required("name", Type::Text, "Its new name.")]),
        |ctx, args| {
            let take = existing(ctx, args.get("take")?)?;
            let name: String = args.get("name")?;
            ctx.document
                .transact("Rename Take", |tx| tx.set_take_name(take, &name));
            Ok(Value::None)
        },
    );
}

/// The take with id `take`, if the project has it.
fn existing(ctx: &Context<'_>, take: TakeId) -> Result<TakeId, CommandError> {
    match ctx.document.project().find_take(take) {
        Some(_) => Ok(take),
        None => Err(CommandError::invalid(format!("there's no take {}", take.0))),
    }
}

/// Copies what the take with id `take` has over `range` to its track,
/// replacing what the track has there, even where the take has nothing,
/// and returns the ids of the copies. The copies are new clips, and the
/// take keeps its own.
pub fn comp(tx: &mut Transaction<'_>, take: TakeId, range: BeatRange) -> Vec<ClipId> {
    let Some((track, t)) = tx.project().find_take(take) else {
        return Vec::new();
    };
    let track = track.id;
    let parts: Vec<Clip> = t
        .clips
        .iter()
        .filter_map(|clip| crop(tx.project(), tx.editor(), clip, range))
        .collect();
    clear(tx, track, range, &BTreeSet::new());
    let mut copies = Vec::with_capacity(parts.len());
    for part in parts {
        let copy = Clip {
            id: tx.new_clip_id(),
            ..part
        };
        copies.push(copy.id);
        tx.add_clip(track, usize::MAX, copy);
    }
    copies
}

/// What a track recorded in one pass of the playhead over the song: a
/// stretch of an audio file, to play from a place in the song. A pass that
/// went on in another file, because the first was full, is a `Pass` for
/// each file, each starting where the one before it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    /// The song position it starts at, in frames at the recording's sample
    /// rate.
    pub start: u64,
    /// How long it is, in frames.
    pub frames: u64,
    /// The file it's in, as clips name files.
    pub file: String,
    /// The frame of the file it starts at.
    pub offset: u64,
}

/// What a MIDI track recorded in one pass of the playhead over the song:
/// the notes played over it, and how the controllers moved.
#[derive(Debug, Clone, PartialEq)]
pub struct NotePass {
    /// The song position it starts at, in frames at the recording's sample
    /// rate.
    pub start: u64,
    /// How long it is, in frames.
    pub frames: u64,
    /// In the order they started.
    pub notes: Vec<PlayedNote>,
    /// How the controllers moved, in order: where, which, and to what,
    /// from 0.0 to 1.0. The moves at the start say what the controllers
    /// held as the pass began.
    pub controllers: Vec<(u64, Controller, f32)>,
}

/// A note played, at song positions in frames at the recording's sample
/// rate. A note held as its pass ended stops at the end.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayedNote {
    pub start: u64,
    pub end: u64,
    /// The MIDI note number, from 0 to 127.
    pub key: u8,
    /// From 0.0 to 1.0, as [`Note::velocity`](musicable_model::clip::Note).
    pub velocity: f32,
    pub release_velocity: f32,
    /// How its expression changed, in order: where, which dimension, and
    /// to what, in the values [`Expression`](musicable_model::clip::Expression)
    /// takes.
    pub expression: Vec<(u64, Dimension, f32)>,
}

/// What a track recorded, a pass at a time, in the order they were played.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Audio {
        track: TrackId,
        passes: Vec<Pass>,
    },
    Midi {
        track: TrackId,
        passes: Vec<NotePass>,
    },
}

/// Adds what tracks recorded at `rate`, as one transaction: a take for each
/// pass, under its track, each comped onto the track in turn, so later
/// passes replace earlier ones where they overlap. An audio pass that goes
/// on in another file is one take of a clip for each file; a MIDI pass
/// without notes makes no take. Returns the takes' ids.
///
/// Passes start on the frame they were recorded at: a clip's start is the
/// first position that falls on that frame, or in the rare tempos where no
/// position does, the next frame, with an audio clip starting that much
/// further into its file. Notes start and stop on their frames the same
/// way. Tracks that went, or of another kind, are left out.
pub fn add_recording(document: &mut Document, rate: u32, recorded: &[Recorded]) -> Vec<TakeId> {
    add_takes(document, "Record", rate, recorded)
}

/// Adds what Capture took as [`add_recording`] adds a recording, as one
/// transaction named Capture.
pub fn add_capture(document: &mut Document, rate: u32, recorded: &[Recorded]) -> Vec<TakeId> {
    add_takes(document, "Capture", rate, recorded)
}

/// Adds what tracks recorded, as one transaction called `name`: see
/// [`add_recording`].
fn add_takes(document: &mut Document, name: &str, rate: u32, recorded: &[Recorded]) -> Vec<TakeId> {
    document.transact(name, |tx| {
        let mut takes = Vec::new();
        for recorded in recorded {
            let (track, kind) = match recorded {
                Recorded::Audio { track, .. } => (*track, TrackKind::Audio),
                Recorded::Midi { track, .. } => (*track, TrackKind::Midi),
            };
            let Some(name) = tx.project().track(track).filter(|t| t.kind == kind) else {
                continue;
            };
            let name = name.name.clone();
            let clips = match recorded {
                Recorded::Audio { passes, .. } => audio_takes(tx, &name, rate, passes),
                Recorded::Midi { passes, .. } => midi_takes(tx, &name, rate, passes),
            };
            for clips in clips {
                let (Some(first), Some(last)) = (clips.first(), clips.last()) else {
                    continue;
                };
                let range = BeatRange::new(first.start, last.end());
                let mut take = tx.new_take(next_take_name(tx, track));
                take.clips = clips;
                let id = take.id;
                tx.add_take(track, usize::MAX, take);
                if tx.project().find_take(id).is_some() {
                    comp(tx, id, range);
                    takes.push(id);
                }
            }
        }
        takes
    })
}

/// The clips of an audio track's takes of `passes`, named `name`, a
/// take's after another. A pass that starts where the one before stopped
/// goes on from it, in another file.
fn audio_takes(tx: &mut Transaction<'_>, name: &str, rate: u32, passes: &[Pass]) -> Vec<Vec<Clip>> {
    passes
        .chunk_by(|a, b| a.start + a.frames == b.start)
        .map(|pass| {
            pass.iter()
                .filter_map(|part| clip_of(tx, name, rate, part))
                .collect()
        })
        .collect()
}

/// The clips of a MIDI track's takes of `passes`, named `name`: a clip
/// each, for the passes with notes.
fn midi_takes(
    tx: &mut Transaction<'_>,
    name: &str,
    rate: u32,
    passes: &[NotePass],
) -> Vec<Vec<Clip>> {
    passes
        .iter()
        .filter_map(|pass| {
            let (range, midi) = notes_of(&tx.project().tempo, rate, pass)?;
            Some(vec![tx.new_clip(name, range, ClipContent::Midi(midi))])
        })
        .collect()
}

/// A clip playing `pass`, named `name`, with a new id, or `None` if the
/// pass is too short to place.
fn clip_of(tx: &mut Transaction<'_>, name: &str, rate: u32, pass: &Pass) -> Option<Clip> {
    let tempo = &tx.project().tempo;
    let first = i64::try_from(pass.start).ok()?;
    let start = after_frames(tempo, rate, first, 0);
    // Where the clip starts playing, at or just after the pass's start.
    let skip = u64::try_from(tempo.frame_at(start, rate) - first).ok()?;
    let end = after_frames(tempo, rate, first, pass.frames);
    if skip >= pass.frames || end <= start {
        return None;
    }
    let mut audio = AudioClip::new(pass.file.clone());
    audio.offset = pass.offset + skip;
    Some(tx.new_clip(name, BeatRange::new(start, end), ClipContent::Audio(audio)))
}

/// Where a clip of `pass` plays, and its notes, or `None` if it has none
/// that can be placed.
fn notes_of(tempo: &TempoMap, rate: u32, pass: &NotePass) -> Option<(BeatRange, MidiClip)> {
    // The first position on the frame, or just after.
    let at = |frame: u64| after_frames(tempo, rate, i64::try_from(frame).unwrap_or(i64::MAX), 0);
    let range = BeatRange::new(at(pass.start), at(pass.start + pass.frames));
    let mut midi = MidiClip::default();
    for played in &pass.notes {
        let start = at(played.start).max(range.start);
        let end = at(played.end).min(range.end);
        if end <= start {
            continue;
        }
        let note = midi.add_note(
            start - range.start,
            end - start,
            played.key,
            played.velocity,
        );
        note.release_velocity = played.release_velocity;
        for &(frame, dimension, value) in &played.expression {
            let point = Point::new(at(frame).max(start) - start, value);
            note.expression.curve_mut(dimension).push(point);
        }
    }
    for &(frame, controller, value) in &pass.controllers {
        let position = at(frame).max(range.start);
        if position >= range.end {
            continue;
        }
        let point = Point::new(position - range.start, value);
        let curve = midi.controllers.entry(controller).or_default();
        // Of two moves on the same tick, the later holds, and a move that
        // changes nothing is left out.
        match curve.last_mut() {
            Some(last) if last.at == point.at => *last = point,
            Some(last) if last.value == value => {}
            _ => curve.push(point),
        }
    }
    (!range.is_empty() && !midi.notes.is_empty()).then_some((range, midi))
}

/// "Take" and a number one past the highest the track's takes have.
fn next_take_name(tx: &Transaction<'_>, track: TrackId) -> String {
    let highest = tx
        .project()
        .track(track)
        .into_iter()
        .flat_map(|t| &t.takes)
        .filter_map(|take| take.name.strip_prefix("Take ")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("Take {}", highest.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::MidiClip;
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::Args;
    use crate::editor::AudioFile;
    use crate::testing::Setup;

    /// 120 BPM at 48 kHz: 24,000 frames to the beat.
    const RATE: u32 = 48_000;
    const BEAT: u64 = 24_000;

    fn beats(start: i64, end: i64) -> BeatRange {
        BeatRange::new(Beats::new(start), Beats::new(end))
    }

    /// A vocal track and a MIDI track.
    fn setup() -> (Setup, TrackId, TrackId) {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        setup.document.editor_mut().sample_rate = RATE;
        (setup, vocal, keys)
    }

    fn pass(start: u64, frames: u64, offset: u64) -> Pass {
        Pass {
            start,
            frames,
            file: "Recordings/Vocal 1.wav".into(),
            offset,
        }
    }

    /// The track's clips, by position, as (start, end, file offset).
    fn clips(setup: &Setup, track: TrackId) -> Vec<(Beats, Beats, u64)> {
        let mut clips: Vec<_> = setup
            .document
            .project()
            .track(track)
            .unwrap()
            .clips
            .iter()
            .map(|clip| {
                let ClipContent::Audio(audio) = &clip.content else {
                    panic!("an audio clip");
                };
                (clip.start, clip.end(), audio.offset)
            })
            .collect();
        clips.sort();
        clips
    }

    #[test]
    fn each_pass_is_a_take_and_the_last_one_plays() {
        let (mut setup, vocal, _) = setup();
        // From beat 2, then three times round a loop from beat 4 to 8: one
        // file, the passes one after another in it.
        let first = pass(2 * BEAT, 6 * BEAT, 0);
        let loops = (0..3).map(|i| pass(4 * BEAT, 4 * BEAT, (6 + 4 * i) * BEAT));
        let recorded = Recorded::Audio {
            track: vocal,
            passes: std::iter::once(first).chain(loops).collect(),
        };
        let takes = add_recording(&mut setup.document, RATE, &[recorded]);
        assert_eq!(takes.len(), 4);
        let track = setup.document.project().track(vocal).unwrap();
        let names: Vec<&str> = track.takes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Take 1", "Take 2", "Take 3", "Take 4"]);
        let last = &track.takes[3].clips[0];
        assert_eq!((last.range(), last.name.as_str()), (beats(4, 8), "Vocal"));
        // The pre-roll from the first pass, and the loop from the last.
        assert_eq!(
            clips(&setup, vocal),
            [
                (Beats::new(2), Beats::new(4), 0),
                (Beats::new(4), Beats::new(8), 14 * BEAT),
            ]
        );
        assert_eq!(setup.committed_names(), ["Record"]);
        // One undo takes it all back.
        assert!(setup.document.undo());
        let track = setup.document.project().track(vocal).unwrap();
        assert!(track.takes.is_empty() && track.clips.is_empty());
    }

    #[test]
    fn a_pass_that_goes_on_in_another_file_is_one_take() {
        let (mut setup, vocal, _) = setup();
        // The first file filled up two beats in.
        let rest = Pass {
            file: "Recordings/Vocal 2.wav".into(),
            ..pass(4 * BEAT, 2 * BEAT, 0)
        };
        let recorded = Recorded::Audio {
            track: vocal,
            passes: vec![pass(2 * BEAT, 2 * BEAT, 0), rest],
        };
        let takes = add_recording(&mut setup.document, RATE, &[recorded]);
        assert_eq!(takes.len(), 1);
        let track = setup.document.project().track(vocal).unwrap();
        let files: Vec<_> = track.takes[0]
            .clips
            .iter()
            .map(|clip| match &clip.content {
                ClipContent::Audio(audio) => (clip.range(), audio.file.as_str()),
                ClipContent::Midi(_) => panic!("an audio clip"),
            })
            .collect();
        let first = (beats(2, 4), "Recordings/Vocal 1.wav");
        assert_eq!(files, [first, (beats(4, 6), "Recordings/Vocal 2.wav")]);
        assert_eq!(
            clips(&setup, vocal),
            [
                (Beats::new(2), Beats::new(4), 0),
                (Beats::new(4), Beats::new(6), 0),
            ]
        );
    }

    #[test]
    fn passes_land_on_the_frame_they_were_recorded_at() {
        let (mut setup, vocal, _) = setup();
        let mut tempo = setup.document.project().tempo.clone();
        tempo.set_tempo(97.3);
        setup.document.transact("Tempo", |tx| tx.set_tempo(tempo));
        let start = 123_457;
        let recorded = Recorded::Audio {
            track: vocal,
            passes: vec![pass(start, 48_001, 17)],
        };
        add_recording(&mut setup.document, RATE, &[recorded]);
        let project = setup.document.project();
        let clip = &project.tracks[0].clips[0];
        let frame = |at| project.tempo.frame_at(at, RATE);
        assert_eq!(frame(clip.start), start as i64);
        assert_eq!(frame(clip.end()), start as i64 + 48_001);
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("an audio clip");
        };
        assert_eq!(audio.offset, 17);
    }

    #[test]
    fn take_names_count_on_from_the_highest() {
        let (mut setup, vocal, keys) = setup();
        setup.document.transact("Takes", |tx| {
            for name in ["Take 4", "Take x", "Best"] {
                let take = tx.new_take(name);
                tx.add_take(vocal, usize::MAX, take);
            }
        });
        let recorded = [
            Recorded::Audio {
                track: vocal,
                passes: vec![pass(0, BEAT, 0)],
            },
            // MIDI tracks don't record audio, and passes too short to place
            // are left out.
            Recorded::Audio {
                track: keys,
                passes: vec![pass(0, BEAT, 0)],
            },
            Recorded::Audio {
                track: vocal,
                passes: vec![pass(BEAT, 0, 0)],
            },
        ];
        let takes = add_recording(&mut setup.document, RATE, &recorded);
        assert_eq!(takes.len(), 1);
        let (_, take) = setup.document.project().find_take(takes[0]).unwrap();
        assert_eq!(take.name, "Take 5");
        assert!(setup.document.project().tracks[1].takes.is_empty());
    }

    #[test]
    fn comping_copies_a_stretch_of_a_take_over_the_track() {
        let (mut setup, vocal, _) = setup();
        let passes = (0..2).map(|i| pass(0, 8 * BEAT, i * 8 * BEAT)).collect();
        let takes = add_recording(
            &mut setup.document,
            RATE,
            &[Recorded::Audio {
                track: vocal,
                passes,
            }],
        );
        setup.document.take_committed();
        let comp = |setup: &mut Setup, take: TakeId, start: i64, end: i64| {
            let args = Args::new()
                .with("take", take)
                .with("start", Beats::new(start))
                .with("end", Beats::new(end));
            setup.run_with(&COMP, args)
        };
        // The second take plays; beats 2 to 5 of the first go in its place.
        let copied = comp(&mut setup, takes[0], 5, 2).unwrap();
        assert_eq!(
            clips(&setup, vocal),
            [
                (Beats::ZERO, Beats::new(2), 8 * BEAT),
                (Beats::new(2), Beats::new(5), 2 * BEAT),
                (Beats::new(5), Beats::new(8), 13 * BEAT),
            ]
        );
        let Value::List(copied) = copied else {
            panic!("the copies' ids");
        };
        assert_eq!(copied.len(), 1);
        // The take keeps its own clip.
        let (_, take) = setup.document.project().find_take(takes[0]).unwrap();
        assert_eq!(take.clips[0].range(), beats(0, 8));
        // Where the take has nothing, the comp leaves silence.
        comp(&mut setup, takes[1], 7, 12).unwrap();
        assert_eq!(
            clips(&setup, vocal).last(),
            Some(&(Beats::new(7), Beats::new(8), 15 * BEAT))
        );
        assert_eq!(setup.committed_names(), ["Comp", "Comp"]);
        assert!(setup.document.undo() && setup.document.undo());
        assert_eq!(
            clips(&setup, vocal),
            [(Beats::ZERO, Beats::new(8), 8 * BEAT)]
        );

        assert_eq!(
            comp(&mut setup, takes[0], 3, 3),
            Err(CommandError::invalid("a comp must end after it starts"))
        );
        assert_eq!(
            comp(&mut setup, TakeId(99), 0, 1),
            Err(CommandError::invalid("there's no take 99"))
        );
    }

    #[test]
    fn comping_stops_at_the_end_of_a_takes_file() {
        let (mut setup, vocal, _) = setup();
        add_recording(
            &mut setup.document,
            RATE,
            &[Recorded::Audio {
                track: vocal,
                passes: vec![pass(0, 4 * BEAT, 0)],
            }],
        );
        // A take whose clip says it's longer than its file.
        let take = setup.document.transact("Take", |tx| {
            let mut take = tx.new_take("Short");
            let audio = ClipContent::Audio(AudioClip::new("short.wav"));
            take.clips.push(tx.new_clip("Short", beats(0, 4), audio));
            let id = take.id;
            tx.add_take(vocal, usize::MAX, take);
            id
        });
        setup.document.editor_mut().files.insert(
            "short.wav".into(),
            AudioFile {
                sample_rate: RATE,
                channels: 1,
                frames: BEAT,
            },
        );
        let args = Args::new()
            .with("take", take)
            .with("start", Beats::ZERO)
            .with("end", Beats::new(4));
        setup.run_with(&COMP, args).unwrap();
        let ends: Vec<Beats> = clips(&setup, vocal).iter().map(|c| c.1).collect();
        assert_eq!(ends, [Beats::new(1)]);
    }

    #[test]
    fn takes_are_deleted_and_renamed_by_id() {
        let (mut setup, vocal, keys) = setup();
        let take = setup.document.transact("Take", |tx| {
            let mut take = tx.new_take("Take 1");
            let midi = ClipContent::Midi(MidiClip::default());
            take.clips.push(tx.new_clip("Keys", beats(0, 4), midi));
            let id = take.id;
            tx.add_take(keys, usize::MAX, take);
            // Only clips of the track's kind.
            let mut wrong = tx.new_take("Take 1");
            let midi = ClipContent::Midi(MidiClip::default());
            wrong.clips.push(tx.new_clip("Keys", beats(0, 4), midi));
            tx.add_take(vocal, usize::MAX, wrong);
            id
        });
        assert!(setup.document.project().tracks[0].takes.is_empty());
        let named = Args::new().with("take", take).with("name", "Keeper");
        setup.run_with(&RENAME, named).unwrap();
        assert_eq!(
            setup.document.project().find_take(take).unwrap().1.name,
            "Keeper"
        );
        setup
            .run_with(&DELETE, Args::new().with("take", take))
            .unwrap();
        assert!(setup.document.project().find_take(take).is_none());
        assert_eq!(
            setup.committed_names(),
            ["Take", "Rename Take", "Delete Take"]
        );
    }

    /// A note on key 60 played from frame `start` to `end`.
    fn played(start: u64, end: u64) -> PlayedNote {
        PlayedNote {
            start,
            end,
            key: 60,
            velocity: 0.75,
            release_velocity: 0.25,
            expression: Vec::new(),
        }
    }

    /// The notes of the track's clips, by position, as (song position,
    /// length, key).
    fn notes(setup: &Setup, track: TrackId) -> Vec<(Beats, Beats, u8)> {
        let track = setup.document.project().track(track).unwrap();
        let mut notes: Vec<_> = (track.clips.iter())
            .flat_map(|clip| {
                let ClipContent::Midi(midi) = &clip.content else {
                    panic!("a MIDI clip");
                };
                (midi.notes.iter())
                    .map(|note| (clip.start + note.start - midi.offset, note.length, note.key))
            })
            .collect();
        notes.sort_by_key(|&(start, length, key)| (start, length, key));
        notes
    }

    #[test]
    fn a_midi_pass_is_a_take_of_the_notes_played() {
        let (mut setup, _, keys) = setup();
        let mut note = played(3 * BEAT, 4 * BEAT + BEAT / 2);
        note.expression = vec![
            (3 * BEAT, Dimension::Pressure, 0.25),
            (4 * BEAT, Dimension::Pressure, 0.5),
            (4 * BEAT, Dimension::PitchBend, -2.0),
        ];
        // From beat 2 to 6, and a note still held at the end.
        let pass = NotePass {
            start: 2 * BEAT,
            frames: 4 * BEAT,
            notes: vec![note, played(5 * BEAT, 6 * BEAT)],
            controllers: Vec::new(),
        };
        let recorded = Recorded::Midi {
            track: keys,
            passes: vec![pass],
        };
        let takes = add_recording(&mut setup.document, RATE, &[recorded]);
        assert_eq!(takes.len(), 1);
        let (_, take) = setup.document.project().find_take(takes[0]).unwrap();
        assert_eq!(take.name, "Take 1");
        let clip = &take.clips[0];
        assert_eq!((clip.range(), clip.name.as_str()), (beats(2, 6), "Keys"));
        let ClipContent::Midi(midi) = &clip.content else {
            panic!("a MIDI clip");
        };
        let first = &midi.notes[0];
        let half = Beats::from_f64(0.5);
        assert_eq!(
            (first.start, first.length, first.key),
            (Beats::new(1), Beats::new(1) + half, 60)
        );
        assert_eq!((first.velocity, first.release_velocity), (0.75, 0.25));
        let pressure = [
            Point::new(Beats::ZERO, 0.25),
            Point::new(Beats::new(1), 0.5),
        ];
        assert_eq!(first.expression.pressure, pressure);
        assert_eq!(
            first.expression.pitch_bend,
            [Point::new(Beats::new(1), -2.0)]
        );
        // The track plays it.
        assert_eq!(
            notes(&setup, keys),
            [
                (Beats::new(3), Beats::new(1) + half, 60),
                (Beats::new(5), Beats::new(1), 60)
            ]
        );
        assert!(setup.document.undo());
        assert!(
            setup
                .document
                .project()
                .track(keys)
                .unwrap()
                .takes
                .is_empty()
        );
    }

    #[test]
    fn a_midi_pass_without_notes_makes_no_take() {
        let (mut setup, _, keys) = setup();
        // Three times round a loop from beat 4 to 8, playing only the first
        // two times.
        let pass = |notes| NotePass {
            start: 4 * BEAT,
            frames: 4 * BEAT,
            notes,
            controllers: vec![(4 * BEAT, Controller::SUSTAIN, 1.0)],
        };
        let recorded = Recorded::Midi {
            track: keys,
            passes: vec![
                pass(vec![played(4 * BEAT, 5 * BEAT)]),
                pass(vec![played(6 * BEAT, 7 * BEAT)]),
                pass(Vec::new()),
            ],
        };
        let takes = add_recording(&mut setup.document, RATE, &[recorded]);
        assert_eq!(takes.len(), 2);
        // The last pass that played plays.
        assert_eq!(notes(&setup, keys), [(Beats::new(6), Beats::new(1), 60)]);
    }

    #[test]
    fn notes_land_on_the_frames_they_were_played_at() {
        let (mut setup, vocal, keys) = setup();
        let mut tempo = setup.document.project().tempo.clone();
        tempo.set_tempo(97.3);
        setup.document.transact("Tempo", |tx| tx.set_tempo(tempo));
        let (start, end) = (123_457, 171_458);
        let pass = NotePass {
            start: 100_001,
            frames: 100_000,
            notes: vec![played(start, end)],
            controllers: vec![(start + 1, Controller::SUSTAIN, 1.0)],
        };
        let recorded = [
            Recorded::Midi {
                track: keys,
                passes: vec![pass.clone()],
            },
            // Audio tracks don't record notes.
            Recorded::Midi {
                track: vocal,
                passes: vec![pass],
            },
        ];
        assert_eq!(add_recording(&mut setup.document, RATE, &recorded).len(), 1);
        let project = setup.document.project();
        let clip = &project.track(keys).unwrap().clips[0];
        let ClipContent::Midi(midi) = &clip.content else {
            panic!("a MIDI clip");
        };
        let note = &midi.notes[0];
        let frame = |at| project.tempo.frame_at(at, RATE);
        assert_eq!(frame(clip.start), 100_001);
        assert_eq!(frame(clip.start + note.start), start as i64);
        assert_eq!(frame(clip.start + note.end()), end as i64);
        let pedal = midi.controller(Controller::SUSTAIN)[0];
        assert_eq!(frame(clip.start + pedal.at), start as i64 + 1);
        assert!(project.track(vocal).unwrap().takes.is_empty());
    }

    #[test]
    fn a_midi_pass_keeps_how_its_controllers_moved() {
        let (mut setup, _, keys) = setup();
        let half = BEAT / 2;
        // From beat 2 to 6, with the pedal down from the start, and the mod
        // wheel moving.
        let (pedal, wheel) = (Controller::SUSTAIN, Controller::MOD_WHEEL);
        let pass = NotePass {
            start: 2 * BEAT,
            frames: 4 * BEAT,
            notes: vec![played(2 * BEAT, 3 * BEAT)],
            controllers: vec![
                (2 * BEAT, pedal, 1.0),
                (2 * BEAT, wheel, 0.0),
                (3 * BEAT, wheel, 0.25),
                // On one tick, the later holds.
                (3 * BEAT + half, wheel, 0.5),
                (3 * BEAT + half, wheel, 0.75),
                // One that changes nothing is left out.
                (4 * BEAT, wheel, 0.75),
                (5 * BEAT, pedal, 0.0),
                // At the end, it's past the clip.
                (6 * BEAT, pedal, 1.0),
            ],
        };
        let recorded = Recorded::Midi {
            track: keys,
            passes: vec![pass],
        };
        let takes = add_recording(&mut setup.document, RATE, &[recorded]);
        let (_, take) = setup.document.project().find_take(takes[0]).unwrap();
        let ClipContent::Midi(midi) = &take.clips[0].content else {
            panic!("a MIDI clip");
        };
        let beat = |b: f64| Beats::from_f64(b);
        assert_eq!(
            midi.controller(pedal),
            [Point::new(beat(0.0), 1.0), Point::new(beat(3.0), 0.0)]
        );
        assert_eq!(
            midi.controller(wheel),
            [
                Point::new(beat(0.0), 0.0),
                Point::new(beat(1.0), 0.25),
                Point::new(beat(1.5), 0.75),
            ]
        );
        assert_eq!(midi.controllers.len(), 2);
    }
}

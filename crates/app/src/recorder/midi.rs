//! What a MIDI track records: the notes its MIDI input plays and how it
//! moves the controllers, such as the sustain pedal, each message placed
//! by the engine frame heard as it came in, as an audio input's frames are,
//! or the notes another track plays, each placed by the engine frame it
//! played at.
//!
//! A MIDI input goes through the engine's own [`MidiNotes`], as it does to
//! play, so the notes recorded are the notes heard: MPE included. What came
//! in before the recording started goes through it too, without being
//! recorded, so it knows the notes held, the pitch bend ranges set and
//! where the controllers are, and the same goes for another track's notes.
//! A note held as recording punches in isn't recorded. A note held as a
//! pass ends stops there, and if the next pass records on from it, as round
//! a loop, starts again at its start. Each pass starts with the controllers
//! where they are, so a pedal held down as it starts holds the notes in the
//! take as it did while recording.

use std::ops::Range;

use musicable_audio::{MidiBuffer, MidiEvent, MidiReader};
use musicable_commands::take::{NotePass, PlayedNote};
use musicable_engine::{MidiNotes, MidiSource, NodeEventKind, NoteEvent, NoteId, Played, Segment};
use musicable_model::clip::{Controller, Dimension, Note};

use super::{Armed, NoteBuffer, NoteReader};

/// What one MIDI track records.
pub(super) struct MidiTape {
    pub armed: Armed,
    /// What it takes the notes from.
    taken: Taken,
    /// The engine time the recording started at: what came in before only
    /// goes through `sounding` and `controllers`.
    start: u64,
    /// The engine time it has recorded up to.
    pub done: u64,
    pub passes: Vec<NotePass>,
    /// Whether the last pass runs on: the track records, and the song
    /// plays on from where it ends.
    open: bool,
    sounding: Vec<Sounding>,
    /// What each controller holds, as what it takes last moved it, for
    /// those that have moved.
    controllers: [Option<f32>; Controller::MAX as usize + 1],
    /// Scratch space for what it takes over a stretch, in order, each with
    /// its frame.
    played: Vec<(u64, Played)>,
    /// What the buffers it no longer reads lost.
    lost: u64,
}

/// What a MIDI track takes the notes it records from.
enum Taken {
    /// MIDI inputs, each message with the engine frame heard as it came
    /// in.
    Inputs {
        /// Which of the stream's MIDI inputs it takes, and which channels.
        source: MidiSource,
        /// Each input it takes, and where it has read it up to.
        readers: Vec<(MidiBuffer, MidiReader)>,
        notes: Box<MidiNotes>,
        /// Scratch space for the messages of a stretch, in order.
        events: Vec<MidiEvent>,
    },
    /// Another track's notes, as the engine wrote them from a tap, each
    /// with the engine frame it played at, and where it has read them up
    /// to.
    Tap(NoteBuffer, NoteReader),
}

/// A note the input holds.
struct Sounding {
    note: NoteId,
    key: u8,
    velocity: f32,
    /// Its expression now, by [`Dimension::ALL`]'s order, for starting it
    /// again in the next pass.
    expression: [Option<f32>; 3],
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// It started outside a pass, so it isn't recorded.
    Played,
    /// It's recorded in the last pass, as its note with this index.
    Recorded(usize),
    /// It was recorded when the last pass ended, and starts again with the
    /// next if that records on.
    Held,
}

impl MidiTape {
    /// Records `armed` from the MIDI inputs `source` takes, which have the
    /// `buffers` given, from engine time `start`.
    pub fn new(armed: Armed, source: MidiSource, buffers: Vec<MidiBuffer>, start: u64) -> Self {
        let taken = Taken::Inputs {
            source,
            readers: readers(buffers),
            notes: Box::new(MidiNotes::new(source.channel, 0)),
            events: Vec::new(),
        };
        Self::taking(armed, taken, start)
    }

    /// Records `armed` from another track's notes, which the engine writes
    /// into `buffer`, from engine time `start`.
    pub fn from_tap(armed: Armed, buffer: NoteBuffer, start: u64) -> Self {
        let reader = buffer.reader();
        Self::taking(armed, Taken::Tap(buffer, reader), start)
    }

    fn taking(armed: Armed, taken: Taken, start: u64) -> Self {
        Self {
            armed,
            taken,
            start,
            done: start,
            passes: Vec::new(),
            open: false,
            sounding: Vec::new(),
            controllers: [None; Controller::MAX as usize + 1],
            played: Vec::new(),
            lost: 0,
        }
    }

    /// Goes on from the MIDI inputs' new `buffers`, which `source` says
    /// where they are of the stream's, as when the stream opens again. A
    /// track taking another's notes goes on as it was.
    pub fn set_inputs(&mut self, source: MidiSource, buffers: Vec<MidiBuffer>) {
        let lost = self.lost();
        if let Taken::Inputs {
            source: taken,
            readers: taking,
            ..
        } = &mut self.taken
        {
            self.lost = lost;
            *taken = source;
            *taking = readers(buffers);
        }
    }

    /// Goes on from the new `buffer` of the tap it takes another track's
    /// notes from, as when the engine writes it anew, or from where it got
    /// to if it's the buffer it reads. A track taking MIDI inputs goes on
    /// as it was.
    pub fn set_tap(&mut self, buffer: NoteBuffer) {
        let lost = self.lost();
        if let Taken::Tap(taken, reader) = &mut self.taken
            && !taken.same_buffer(&buffer)
        {
            self.lost = lost;
            *reader = buffer.reader();
            *taken = buffer;
        }
    }

    /// Messages, or another track's notes, it lost, overwritten in their
    /// buffers before it read them: it fell more than a buffer behind.
    pub fn lost(&self) -> u64 {
        let lost: u64 = match &self.taken {
            Taken::Inputs { readers, .. } => readers.iter().map(|(_, reader)| reader.lost()).sum(),
            Taken::Tap(_, reader) => reader.lost(),
        };
        self.lost + lost
    }

    /// Whether it takes another track's notes, not MIDI inputs.
    pub fn takes_notes(&self) -> bool {
        matches!(self.taken, Taken::Tap(..))
    }

    /// The frame below which what it takes has brought everything: an
    /// input frame for MIDI inputs, or an engine frame for another track's
    /// notes.
    pub fn settled(&self) -> u64 {
        match &self.taken {
            Taken::Inputs { readers, .. } => (readers.iter())
                .map(|(buffer, _)| buffer.settled())
                .min()
                .unwrap_or(u64::MAX),
            Taken::Tap(buffer, _) => buffer.settled(),
        }
    }

    /// How many frames after the engine rendered a stretch of `segment`
    /// what it takes has what played there. A MIDI input has what was
    /// heard as it played, the graph's latency later, and the input's
    /// offset after that. Another track's notes are there as the engine
    /// played them.
    pub fn lag(&self, segment: &Segment) -> i128 {
        match self.taken {
            Taken::Inputs { .. } => i128::from(segment.latency) + i128::from(self.armed.offset),
            Taken::Tap(..) => 0,
        }
    }

    /// Records what it takes from engine time `done` up to `until`, all in
    /// `segment`: what came at frame `w` played as the engine rendered `w`
    /// less its [`lag`](Self::lag). What it takes must have brought
    /// everything up to there.
    pub fn record(&mut self, segment: &Segment, until: u64) {
        let e = self.done;
        if until <= e {
            return;
        }
        let lag = self.lag(segment);
        let bound = u64::try_from((i128::from(until) + lag).max(0)).unwrap_or(u64::MAX);
        self.played.clear();
        self.taken.read(bound, &mut self.played);
        let recording = segment.playing && segment.recording;
        // What came in before the recording started goes first, and isn't
        // recorded, so a pass starting now starts with the controllers
        // where that left them.
        let start = i128::from(self.start);
        let early = (self.played).partition_point(|&(frame, _)| i128::from(frame) - lag < start);
        self.play(0..early, segment, e..until, lag, false);
        if recording {
            let position = segment.position_at(e);
            let runs_on = self.open
                && (self.passes.last()).is_some_and(|pass| pass.start + pass.frames == position);
            if !runs_on {
                self.end_pass(true);
                let controllers = (Controller::all())
                    .filter_map(|c| Some((position, c, self.controllers[usize::from(c.number())]?)))
                    .collect();
                self.passes.push(NotePass {
                    start: position,
                    frames: 0,
                    notes: Vec::new(),
                    controllers,
                });
                self.open = true;
                self.start_held(position);
            }
        } else {
            self.end_pass(false);
        }
        self.play(early..self.played.len(), segment, e..until, lag, recording);
        if recording && let Some(pass) = self.passes.last_mut() {
            pass.frames += until - e;
        }
        self.done = until;
    }

    /// Follows what it took in `range` of the stretch, at engine times in
    /// `times`, their frames less `lag`, and records it in the last pass if
    /// `recorded`.
    fn play(
        &mut self,
        range: Range<usize>,
        segment: &Segment,
        times: Range<u64>,
        lag: i128,
        recorded: bool,
    ) {
        let Self {
            sounding,
            passes,
            controllers,
            played,
            ..
        } = self;
        for &(frame, played) in &played[range] {
            // What came in late for the last stretch goes at the start of
            // this one.
            let at = u64::try_from(i128::from(frame) - lag).unwrap_or(0);
            let position = segment.position_at(at.clamp(times.start, times.end - 1));
            let pass = passes.last_mut().filter(|_| recorded);
            match played {
                Played::Note(note) => follow(sounding, pass, note, position),
                Played::Controller { controller, value } => {
                    controllers[usize::from(controller.number())] = Some(value);
                    if let Some(pass) = pass {
                        pass.controllers.push((position, controller, value));
                    }
                }
            }
        }
    }

    /// What it recorded once the recording stopped: the passes with notes.
    pub fn finish(mut self) -> Vec<NotePass> {
        self.end_pass(false);
        self.passes.retain(|pass| !pass.notes.is_empty());
        self.passes
    }

    /// Ends the last pass if it runs on, stopping the notes held there.
    /// With `carry`, the next pass records on from it, and they start
    /// again there.
    fn end_pass(&mut self, carry: bool) {
        if !std::mem::take(&mut self.open) {
            return;
        }
        let pass = self.passes.last_mut().expect("an open pass");
        let end = pass.start + pass.frames;
        for sounding in &mut self.sounding {
            if let State::Recorded(i) = sounding.state {
                pass.notes[i].end = end;
                sounding.state = if carry { State::Held } else { State::Played };
            }
        }
    }

    /// Starts the notes held over from the last pass again at song
    /// position `position`, in the pass that just started.
    fn start_held(&mut self, position: u64) {
        let pass = self.passes.last_mut().expect("a pass just started");
        for sounding in &mut self.sounding {
            if sounding.state != State::Held {
                continue;
            }
            let expression = (Dimension::ALL.into_iter())
                .zip(sounding.expression)
                .filter_map(|(dimension, value)| Some((position, dimension, value?)))
                .collect();
            sounding.state = State::Recorded(pass.notes.len());
            pass.notes.push(PlayedNote {
                start: position,
                end: position,
                key: sounding.key,
                velocity: sounding.velocity,
                release_velocity: Note::RELEASE_VELOCITY,
                expression,
            });
        }
    }
}

impl Taken {
    /// Adds what it has brought with frames below `before` to `played`, in
    /// order, each with its frame.
    fn read(&mut self, before: u64, played: &mut Vec<(u64, Played)>) {
        match self {
            Taken::Inputs {
                readers,
                notes,
                events,
                ..
            } => {
                events.clear();
                for (_, reader) in readers {
                    while let Some(event) = reader.next_before(before) {
                        events.push(event);
                    }
                }
                // Each input's are in order: a stable sort keeps them so.
                events.sort_by_key(|event| event.frame);
                for event in events.iter() {
                    notes.take(event.bytes, |p| played.push((event.frame, p)));
                }
            }
            Taken::Tap(_, reader) => {
                while let Some((frame, words)) = reader.next_before(before) {
                    played.extend(unpack(words).map(|p| (frame, p)));
                }
            }
        }
    }
}

/// A reader of each of `buffers`, from the oldest message each holds.
fn readers(buffers: Vec<MidiBuffer>) -> Vec<(MidiBuffer, MidiReader)> {
    buffers
        .into_iter()
        .map(|buffer| {
            let reader = buffer.reader();
            (buffer, reader)
        })
        .collect()
}

/// What an event of a [`NoteBuffer`] is: the second word's byte above its
/// value.
const ON: u8 = 1;
const EXPRESSION: u8 = 2;
const OFF: u8 = 3;
const CONTROLLER: u8 = 4;

/// A note's start, change or end, or a controller's move, in a
/// [`NoteBuffer`]'s two words: the note's id, or the controller's number,
/// and then the value's bits, what it is, and the note's key or the
/// dimension of its expression. `None` for what isn't one. Clips' notes are
/// on no channel, so a channel isn't kept.
pub(super) fn pack(kind: NodeEventKind) -> Option<[u64; 2]> {
    let (id, what, detail, value) = match kind {
        NodeEventKind::Note(NoteEvent::On {
            note,
            key,
            velocity,
            ..
        }) => (note.0, ON, key, velocity),
        NodeEventKind::Note(NoteEvent::Expression {
            note,
            dimension,
            value,
        }) => {
            let d = Dimension::ALL.iter().position(|&d| d == dimension);
            (note.0, EXPRESSION, d.unwrap_or(0) as u8, value)
        }
        NodeEventKind::Note(NoteEvent::Off { note, velocity }) => (note.0, OFF, 0, velocity),
        NodeEventKind::Controller {
            controller, value, ..
        } => (u64::from(controller.number()), CONTROLLER, 0, value),
        _ => return None,
    };
    let word = u64::from(value.to_bits()) | u64::from(what) << 32 | u64::from(detail) << 40;
    Some([id, word])
}

/// What [`pack`] packed into `words`.
pub(super) fn unpack([id, word]: [u64; 2]) -> Option<Played> {
    let value = f32::from_bits(word as u32);
    let detail = (word >> 40) as u8;
    let note = NoteId(id);
    Some(match (word >> 32) as u8 {
        ON => Played::Note(NoteEvent::On {
            note,
            key: detail,
            velocity: value,
            channel: None,
        }),
        EXPRESSION => Played::Note(NoteEvent::Expression {
            note,
            dimension: *Dimension::ALL.get(usize::from(detail))?,
            value,
        }),
        OFF => Played::Note(NoteEvent::Off {
            note,
            velocity: value,
        }),
        CONTROLLER => Played::Controller {
            controller: Controller::new(u8::try_from(id).ok()?)?,
            value,
        },
        _ => return None,
    })
}

/// Follows what happens to a note at song position `position`, recording
/// it in `pass` if one records.
fn follow(
    sounding: &mut Vec<Sounding>,
    pass: Option<&mut NotePass>,
    note: NoteEvent,
    position: u64,
) {
    match note {
        NoteEvent::On {
            note,
            key,
            velocity,
            ..
        } => {
            let state = match pass {
                Some(pass) => {
                    pass.notes.push(PlayedNote {
                        start: position,
                        end: position,
                        key,
                        velocity,
                        release_velocity: Note::RELEASE_VELOCITY,
                        expression: Vec::new(),
                    });
                    State::Recorded(pass.notes.len() - 1)
                }
                None => State::Played,
            };
            sounding.push(Sounding {
                note,
                key,
                velocity,
                expression: [None; 3],
                state,
            });
        }
        NoteEvent::Expression {
            note,
            dimension,
            value,
        } => {
            let Some(held) = sounding.iter_mut().find(|s| s.note == note) else {
                return;
            };
            let d = Dimension::ALL
                .iter()
                .position(|&d| d == dimension)
                .unwrap_or(0);
            held.expression[d] = Some(value);
            if let (State::Recorded(i), Some(pass)) = (held.state, pass) {
                pass.notes[i].expression.push((position, dimension, value));
            }
        }
        NoteEvent::Off { note, velocity } => {
            let Some(i) = sounding.iter().position(|s| s.note == note) else {
                return;
            };
            let held = sounding.swap_remove(i);
            if let (State::Recorded(i), Some(pass)) = (held.state, pass) {
                let played = &mut pass.notes[i];
                played.end = position;
                played.release_velocity = velocity;
            }
        }
    }
}

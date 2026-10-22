//! What a MIDI track records: the notes its MIDI input plays, each message
//! placed by the engine frame heard as it came in, as an audio input's
//! frames are.
//!
//! The track's input goes through the engine's own [`MidiNotes`], as it
//! does to play, so the notes recorded are the notes heard: MPE included.
//! What came in before the recording started goes through it too, without
//! being recorded, so it knows the notes held and the pitch bend ranges
//! set. A note held as recording punches in isn't recorded. A note held as
//! a pass ends stops there, and if the next pass records on from it, as
//! round a loop, starts again at its start.

use musicable_audio::{MidiBuffer, MidiEvent, MidiReader};
use musicable_commands::take::{NotePass, PlayedNote};
use musicable_engine::{MidiNotes, MidiSource, NoteEvent, NoteId, Segment};
use musicable_model::clip::{Dimension, Note};

use super::Armed;

/// What one MIDI track records.
pub(super) struct MidiTape {
    pub armed: Armed,
    /// Which of the stream's MIDI inputs it takes, and which channels.
    pub source: MidiSource,
    /// Each input it takes, and where it has read it up to.
    inputs: Vec<(MidiBuffer, MidiReader)>,
    notes: MidiNotes,
    /// The engine time the recording started at: what came in before only
    /// goes through `notes`.
    start: u64,
    /// The engine time it has recorded up to.
    pub done: u64,
    pub passes: Vec<NotePass>,
    /// Whether the last pass runs on: the track records, and the song
    /// plays on from where it ends.
    open: bool,
    sounding: Vec<Sounding>,
    /// Scratch space for the messages of a stretch, in order.
    events: Vec<MidiEvent>,
    /// Messages the inputs it no longer reads lost.
    lost: u64,
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
        Self {
            armed,
            source,
            inputs: readers(buffers),
            notes: MidiNotes::new(source.channel, 0),
            start,
            done: start,
            passes: Vec::new(),
            open: false,
            sounding: Vec::new(),
            events: Vec::new(),
            lost: 0,
        }
    }

    /// Goes on from the MIDI inputs' new `buffers`, as when the stream
    /// opens again.
    pub fn set_inputs(&mut self, buffers: Vec<MidiBuffer>) {
        self.lost = self.lost();
        self.inputs = readers(buffers);
    }

    /// Messages it lost, overwritten in their buffers before it read them:
    /// it fell more than a buffer behind.
    pub fn lost(&self) -> u64 {
        let lost: u64 = self.inputs.iter().map(|(_, reader)| reader.lost()).sum();
        self.lost + lost
    }

    /// The input frame below which its inputs have brought every message.
    pub fn settled(&self) -> u64 {
        (self.inputs.iter())
            .map(|(buffer, _)| buffer.settled())
            .min()
            .unwrap_or(u64::MAX)
    }

    /// Records what its inputs played from engine time `done` up to `until`,
    /// all in `segment`. A message heard at input frame `w` played as the
    /// engine rendered `w` less the graph's latency and the input's offset.
    /// The inputs must have brought the messages up to there.
    pub fn record(&mut self, segment: &Segment, until: u64) {
        let e = self.done;
        if until <= e {
            return;
        }
        let lag = i128::from(segment.latency) + i128::from(self.armed.offset);
        let bound = u64::try_from((i128::from(until) + lag).max(0)).unwrap_or(u64::MAX);
        self.events.clear();
        for (_, reader) in &mut self.inputs {
            while let Some(event) = reader.next_before(bound) {
                self.events.push(event);
            }
        }
        // Each input's are in order: a stable sort keeps them so.
        self.events.sort_by_key(|event| event.frame);
        let recording = segment.playing && segment.recording;
        if recording {
            let position = segment.position_at(e);
            let runs_on = self.open
                && (self.passes.last()).is_some_and(|pass| pass.start + pass.frames == position);
            if !runs_on {
                self.end_pass(true);
                self.passes.push(NotePass {
                    start: position,
                    frames: 0,
                    notes: Vec::new(),
                });
                self.open = true;
                self.start_held(position);
            }
        } else {
            self.end_pass(false);
        }
        let Self {
            notes,
            sounding,
            passes,
            events,
            start,
            ..
        } = self;
        for event in events.iter() {
            let at = i128::from(event.frame) - lag;
            let recorded = recording && at >= i128::from(*start);
            // One that came in late for the last stretch goes at the start
            // of this one.
            let at = u64::try_from(at).unwrap_or(0).clamp(e, until - 1);
            let position = segment.position_at(at);
            let mut pass = passes.last_mut().filter(|_| recorded);
            notes.take(event.bytes, |note| {
                follow(sounding, pass.as_deref_mut(), note, position);
            });
        }
        if recording && let Some(pass) = self.passes.last_mut() {
            pass.frames += until - e;
        }
        self.done = until;
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

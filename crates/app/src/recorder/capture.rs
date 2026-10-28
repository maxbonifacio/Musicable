//! Capture: the last few minutes of what the inputs of armed tracks, and of
//! those monitoring In, brought, held in memory with the timeline over as
//! long, and recorded when asked for as if the tracks had recorded the
//! transport's last run.
//!
//! As it goes, every 10 ms, the recorder copies what the stream's inputs
//! bring into buffers of its own: an audio input's channels that the
//! tracks take, at the engine frames the stream's buffer has them, a tap
//! that they take, at those the engine wrote it at, a MIDI input's
//! messages, with the frames heard, and another track's notes, with the
//! frames they played at. A capture runs the
//! recording's own code over those, on the run's timeline as a recording
//! would have had it: recording on throughout, and recording wherever the
//! transport didn't.

use std::collections::VecDeque;
use std::ops::Range;

use musicable_audio::{InputBuffer, InputWriter, MidiBuffer, MidiReader, MidiWriter};
use musicable_engine::{InputChannels, MidiSource, Segment};
use musicable_model::routing::Tap;
use musicable_model::{AudioInput, Input, MidiInput};

use super::{
    Armed, CHUNK, MidiTape, NoteBuffer, NoteReader, NoteWriter, PATIENCE_SECONDS, Report, Running,
    Tape, TrackTape, find_audio,
};

/// The most memory the held inputs take together, in bytes.
pub(super) const HELD_BYTES: u64 = 2 << 30;
/// How much further back than its inputs Capture holds the timeline, in
/// seconds, for the plugins' latency.
const SLACK_SECONDS: u64 = 60;
/// How many messages a second a held MIDI input has room for: enough for
/// an MPE controller's expression on several notes at once.
const MIDI_RATE: u64 = 2000;
/// The memory a held MIDI message takes, in bytes: its slot in the
/// buffer, with its sequence number, frame and bytes.
const MIDI_MESSAGE_BYTES: u64 = 24;
/// The memory a held event of another track's notes takes, in bytes: its
/// slot, with its sequence number, frame and two words. A held tap of
/// notes has room for as many a second as a MIDI input.
const NOTE_EVENT_BYTES: u64 = 32;

/// What Capture takes from: the inputs of the tracks it captures, each
/// holding the newest of what it brought, and the timeline over as long.
#[derive(Default)]
pub(super) struct Held {
    /// How long each input holds, in seconds, at least: 0 holds nothing.
    seconds: u64,
    /// The tracks Capture takes: armed tracks and those monitoring In.
    tracks: Vec<Armed>,
    /// The audio inputs they take, by their channels, and the taps.
    pub(super) audio: Vec<HeldAudio>,
    /// The MIDI inputs they take.
    pub(super) midi: Vec<HeldMidi>,
    /// The taps of other tracks' notes they take.
    pub(super) notes: Vec<HeldNotes>,
    /// The timeline, from before the oldest frame held.
    segments: VecDeque<Segment>,
    /// Whether the stream's inputs are an old engine's, until the next
    /// stream's come: they're on other times. The taps never are: the
    /// session sends the new engine's with it.
    stale: bool,
}

/// An audio input of the tracks Capture takes, by its channels, or a tap.
pub(super) struct HeldAudio {
    /// An audio input or a tap.
    input: Input,
    /// Where the stream's input captures it, or the engine writes the tap:
    /// a buffer and its channels there.
    source: Option<(InputBuffer, InputChannels)>,
    /// The newest frames of it, at the source buffer's engine frames, once
    /// it has any.
    pub(super) held: Option<(InputBuffer, InputWriter)>,
    /// Whether it would have taken the held inputs past their memory: it
    /// holds nothing then.
    refused: bool,
}

/// A MIDI input of the tracks Capture takes.
pub(super) struct HeldMidi {
    /// Its id, as the stream's layout has it.
    id: String,
    /// Where the stream brings what it plays, and how far it has been read.
    source: Option<(MidiBuffer, MidiReader)>,
    /// The newest of what it played, with the frames heard, from when it
    /// opened.
    pub(super) held: Option<(MidiBuffer, MidiWriter)>,
    /// Whether it would have taken the held inputs past their memory.
    refused: bool,
}

/// Another track's notes that the MIDI tracks Capture takes take: a tap
/// of notes.
pub(super) struct HeldNotes {
    tap: Tap,
    /// Where the engine writes them, and how far it has been read.
    source: Option<(NoteBuffer, NoteReader)>,
    /// The newest of them, with the frames they played at, from when the
    /// engine wrote them.
    pub(super) held: Option<(NoteBuffer, NoteWriter)>,
    /// Whether it would have taken the held inputs past their memory.
    refused: bool,
}

impl Held {
    /// The held notes a track taking `input` takes, if it takes another
    /// track's notes.
    fn notes_of(&self, input: &Input) -> Option<&HeldNotes> {
        let tap = input.tap()?;
        self.notes.iter().find(|held| held.tap == tap)
    }
}

impl Tape {
    /// Holds the inputs of `tracks`, for Capture, the last `seconds` of
    /// each, from now on. An input that stays held keeps what it holds.
    pub(super) fn set_held(&mut self, tracks: Vec<Armed>, seconds: u64) {
        if seconds != self.held.seconds {
            self.held.seconds = seconds;
            self.drop_held();
        }
        self.held.tracks = tracks;
        let audio: Vec<&Input> = (self.held.tracks.iter())
            .map(|track| &track.input)
            .filter(|input| matches!(input, Input::Audio(_) | Input::Tap(_)))
            .filter(|input| self.find_notes(input).is_none())
            .collect();
        (self.held.audio).retain(|held| audio.iter().any(|&i| same_source(i, &held.input)));
        for input in audio {
            if !(self.held.audio.iter()).any(|held| same_source(input, &held.input)) {
                let source = match (input, self.held.stale) {
                    (Input::Audio(_), true) => None,
                    _ => self.find_audio(input),
                };
                self.held.audio.push(HeldAudio {
                    input: input.clone(),
                    source,
                    held: None,
                    refused: false,
                });
            }
        }
        self.hold_midi_inputs(false);
        self.hold_note_taps(false);
    }

    /// Holds the taps of other tracks' notes that the held MIDI tracks
    /// take, of those the engine writes, the ones held already from where
    /// they got to, or with `renewed`, from the oldest of a new buffer the
    /// engine writes one into. They're never stale: the session sends a new
    /// engine's taps with it.
    fn hold_note_taps(&mut self, renewed: bool) {
        let mut taps: Vec<(Tap, NoteBuffer)> = Vec::new();
        for track in &self.held.tracks {
            if let (Some(tap), Some(buffer)) = (track.input.tap(), self.find_notes(&track.input))
                && !taps.iter().any(|&(held, _)| held == tap)
            {
                taps.push((tap, buffer));
            }
        }
        self.held
            .notes
            .retain(|held| taps.iter().any(|&(tap, _)| tap == held.tap));
        for (tap, buffer) in taps {
            let source = || {
                let reader = buffer.reader();
                Some((buffer.clone(), reader))
            };
            match self.held.notes.iter_mut().find(|held| held.tap == tap) {
                Some(held) if renewed => {
                    let reading = (held.source.as_ref())
                        .is_some_and(|(reading, _)| reading.same_buffer(&buffer));
                    if !reading {
                        held.source = source();
                    }
                }
                Some(_) => {}
                None => self.held.notes.push(HeldNotes {
                    tap,
                    source: source(),
                    held: None,
                    refused: false,
                }),
            }
        }
    }

    /// Holds the MIDI inputs the held MIDI tracks take, of those the
    /// stream's layout has, the ones held already from where they got to,
    /// or with `reopened`, from the stream's new buffers.
    fn hold_midi_inputs(&mut self, reopened: bool) {
        let mut ids: Vec<String> = Vec::new();
        for track in &self.held.tracks {
            if let Input::Midi(input) = &track.input {
                let taken = (self.layout.midi_ids()).filter(|&id| takes_midi(input, id));
                for id in taken {
                    if !ids.iter().any(|held| held == id) {
                        ids.push(id.to_owned());
                    }
                }
            }
        }
        self.held.midi.retain(|held| ids.contains(&held.id));
        for id in ids {
            let source = self.midi_source(&id);
            match self.held.midi.iter_mut().find(|held| held.id == id) {
                Some(held) if reopened => held.source = source,
                Some(_) => {}
                None => self.held.midi.push(HeldMidi {
                    id,
                    source,
                    held: None,
                    refused: false,
                }),
            }
        }
    }

    /// The stream's buffer of the MIDI input `id`, if it opened, and a
    /// reader from the oldest message it holds.
    fn midi_source(&self, id: &str) -> Option<(MidiBuffer, MidiReader)> {
        if self.held.stale {
            return None;
        }
        let index = self.layout.midi_ids().position(|midi| midi == id)?;
        let buffer = self.buffers.midi.get(index)?.clone()?;
        let reader = buffer.reader();
        Some((buffer, reader))
    }

    /// Goes on holding the inputs from the stream's new buffers, after the
    /// last of what the old ones had, with silence for what came in
    /// between.
    pub(super) fn hold_from_new_inputs(&mut self) {
        self.held.stale = false;
        let (layout, buffers, taps) = (&self.layout, &self.buffers.audio, &self.taps);
        for held in &mut self.held.audio {
            if let Input::Audio(_) = held.input {
                held.source = find_audio(layout, buffers, taps, &held.input);
            }
        }
        self.hold_midi_inputs(true);
    }

    /// Goes on holding the taps from their new buffers, the same way, and
    /// the taps of other tracks' notes from where they got to, or from the
    /// oldest of a new buffer.
    pub(super) fn hold_from_new_taps(&mut self) {
        let (layout, buffers, taps) = (&self.layout, &self.buffers.audio, &self.taps);
        for held in &mut self.held.audio {
            if let Input::Tap(_) = held.input {
                held.source = find_audio(layout, buffers, taps, &held.input);
            }
        }
        self.hold_note_taps(true);
    }

    /// Forgets what the held inputs hold: they start again with what their
    /// inputs bring next.
    pub(super) fn drop_held(&mut self) {
        for input in &mut self.held.audio {
            input.held = None;
            input.refused = false;
        }
        for input in &mut self.held.midi {
            input.held = None;
            input.refused = false;
        }
        for input in &mut self.held.notes {
            input.held = None;
            input.refused = false;
        }
    }

    /// The next timeline starts its own times, as when the engine is
    /// replaced: what the held inputs hold is on the old one's, and so are
    /// the stream's inputs, until the next stream's come.
    pub(super) fn restart_held(&mut self) {
        self.drop_held();
        for input in &mut self.held.audio {
            input.source = None;
        }
        for input in &mut self.held.midi {
            input.source = None;
        }
        for input in &mut self.held.notes {
            input.source = None;
        }
        self.held.stale = true;
    }

    /// Keeps the timeline's new `segments` for as long as the inputs are
    /// held, and the plugins' latency.
    pub(super) fn hold_timeline(&mut self, segments: &[Segment]) {
        let seconds = self.held.seconds + SLACK_SECONDS;
        let oldest = self.until.saturating_sub(u64::from(self.rate) * seconds);
        let history = &mut self.held.segments;
        history.extend(segments);
        while history.len() > 1 && history[1].time <= oldest {
            history.pop_front();
        }
    }

    /// Forgets the timeline held, as when it's cut.
    pub(super) fn drop_timeline(&mut self) {
        self.held.segments.clear();
    }

    /// Copies what the held inputs' streams brought since into their own
    /// buffers, making them as the inputs open.
    pub(super) fn hold(&mut self) {
        let frames = u64::from(self.rate) * self.held.seconds;
        if frames == 0 {
            return;
        }
        let audio = (self.held.audio.iter())
            .filter_map(|input| input.held.as_ref())
            .map(|(buffer, _)| buffer.capacity() * buffer.channels() as u64 * 4);
        let midi = (self.held.midi.iter())
            .filter_map(|input| input.held.as_ref())
            .map(|(buffer, _)| buffer.capacity() as u64 * MIDI_MESSAGE_BYTES);
        let notes = (self.held.notes.iter())
            .filter_map(|input| input.held.as_ref())
            .map(|(buffer, _)| buffer.capacity() as u64 * NOTE_EVENT_BYTES);
        let mut used: u64 = audio.chain(midi).chain(notes).sum();
        for input in &mut self.held.audio {
            let Some((source, channels)) = &input.source else {
                continue;
            };
            if source.frames().is_empty() {
                continue;
            }
            if input.held.is_none() && !input.refused {
                let size = frames.next_power_of_two() * channels.count as u64 * 4;
                if used + size <= self.held_bytes {
                    used += size;
                    input.held = Some(InputBuffer::new(channels.count, frames));
                } else {
                    input.refused = true;
                    let tracks =
                        (self.held.tracks.iter()).filter(|track| takes_input(track, &input.input));
                    let refused = refusal(tracks, "input", self.held_bytes);
                    self.reports.push(Report::Problem(refused));
                }
            }
            if let Some((_, writer)) = &mut input.held {
                let (scratch, interleaved) = (&mut self.scratch, &mut self.interleaved);
                copy(source, *channels, writer, scratch, interleaved);
            }
        }
        let messages = self.held.seconds * MIDI_RATE;
        for input in &mut self.held.midi {
            let Some((source, reader)) = &mut input.source else {
                continue;
            };
            if input.held.is_none() && !input.refused {
                let size = messages.next_power_of_two() * MIDI_MESSAGE_BYTES;
                if used + size <= self.held_bytes {
                    used += size;
                    input.held = Some(MidiBuffer::new(messages as usize));
                } else {
                    input.refused = true;
                    let tracks = (self.held.tracks.iter()).filter(|track| match &track.input {
                        Input::Midi(midi) => takes_midi(midi, &input.id),
                        Input::Audio(_) | Input::Tap(_) => false,
                    });
                    let refused = refusal(tracks, "MIDI input", self.held_bytes);
                    self.reports.push(Report::Problem(refused));
                }
            }
            if let Some((buffer, writer)) = &mut input.held {
                // What it has settled before it's read: every message before
                // that is there to read.
                let ended = source.has_ended();
                let settled = source.settled();
                while let Some(event) = reader.next_before(u64::MAX) {
                    writer.push(event.frame, event.bytes);
                }
                if !ended {
                    buffer.settle(settled);
                }
            }
        }
        for input in &mut self.held.notes {
            let Some((source, reader)) = &mut input.source else {
                continue;
            };
            if input.held.is_none() && !input.refused {
                let size = messages.next_power_of_two() * NOTE_EVENT_BYTES;
                if used + size <= self.held_bytes {
                    used += size;
                    input.held = Some(NoteBuffer::new(messages as usize));
                } else {
                    input.refused = true;
                    let tracks = (self.held.tracks.iter())
                        .filter(|track| track.input == Input::Tap(input.tap));
                    let refused = refusal(tracks, "input", self.held_bytes);
                    self.reports.push(Report::Problem(refused));
                }
            }
            if let Some((buffer, writer)) = &mut input.held {
                let ended = source.has_ended();
                let settled = source.settled();
                while let Some((frame, words)) = reader.next_before(u64::MAX) {
                    writer.push(frame, words);
                }
                if !ended {
                    buffer.settle(settled);
                }
            }
        }
    }

    /// Asks for a capture of what's held up to engine time `at`, which
    /// waits for the timeline to get there, and the held inputs to catch up
    /// with what was heard then.
    pub(super) fn ask_capture(&mut self, at: u64) {
        let nothing = if self.held.seconds == 0 {
            "Capture is off: its length, on the Recording page of Settings, is 0."
        } else if self.held.tracks.is_empty() {
            "Nothing to capture: no track is armed or monitoring In."
        } else {
            self.capture_at.get_or_insert(at);
            return;
        };
        self.reports.push(Report::Problem(nothing.to_owned()));
    }

    /// Takes the capture asked for once the timeline has got there, and the
    /// held inputs have what was heard then, or have stopped or stalled.
    pub(super) fn take_capture(&mut self) {
        if let Some(at) = self.capture_at
            && self.until >= at
            && self.held_up_to(at)
        {
            self.capture_at = None;
            self.capture(at);
        }
    }

    /// Takes the capture asked for now, if one was, with what's held.
    pub(super) fn take_capture_now(&mut self) {
        if let Some(at) = self.capture_at.take() {
            self.capture(at);
        }
    }

    /// Whether the held inputs of the tracks Capture takes have what was
    /// heard as engine time `at` played, or have stopped, or stalled.
    fn held_up_to(&self, at: u64) -> bool {
        let stalled = self.until.saturating_sub(at) > u64::from(self.rate) * PATIENCE_SECONDS;
        let latency = (self.held.segments.iter().rev())
            .find(|segment| segment.time <= at)
            .map_or(0, |segment| segment.latency);
        let heard = |track: &Armed| i128::from(at) + i128::from(latency) + i128::from(track.offset);
        let has = |track: &Armed| {
            // Another track's notes are there as the engine played them.
            if let Some(held) = self.held.notes_of(&track.input) {
                return match (&held.source, &held.held) {
                    (Some((source, _)), Some((buffer, _))) => {
                        source.has_ended() || buffer.settled() >= at
                    }
                    _ => true,
                };
            }
            match &track.input {
                input @ (Input::Audio(_) | Input::Tap(_)) => (self.held.audio.iter())
                    .filter(|held| same_source(input, &held.input))
                    .all(|held| match (&held.source, &held.held) {
                        (Some((source, _)), Some((buffer, _))) => {
                            source.has_ended() || i128::from(buffer.frames().end) >= heard(track)
                        }
                        _ => true,
                    }),
                Input::Midi(input) => (self.held.midi.iter())
                    .filter(|held| takes_midi(input, &held.id))
                    .all(|held| match (&held.source, &held.held) {
                        (Some((source, _)), Some((buffer, _))) => {
                            source.has_ended() || i128::from(buffer.settled()) >= heard(track)
                        }
                        _ => true,
                    }),
            }
        };
        stalled || self.held.tracks.iter().all(has)
    }

    /// Records what the held inputs of the tracks Capture takes played over
    /// the transport's last run up to engine time `at`, where it didn't
    /// record, as if they had recorded it then: the same passes, files and
    /// takes, a pass for each time round a loop.
    fn capture(&mut self, at: u64) {
        let Some((run, end)) = last_run(&self.held.segments, at) else {
            self.reports.push(Report::Problem(
                "Nothing to capture: the transport hasn't played while the tracks' inputs were \
                 held."
                    .to_owned(),
            ));
            return;
        };
        let start = self.held.segments[run.start].time;
        // The run as a recording sees it: recording on throughout, and
        // recording wherever the transport didn't, up to where it ended.
        let mut segments: VecDeque<Segment> = (self.held.segments.range(run))
            .map(|segment| Segment {
                record: true,
                recording: !segment.recording,
                ..*segment
            })
            .collect();
        let last = *segments.back().expect("a run has a segment");
        segments.push_back(Segment {
            time: end,
            position: last.position_at(end),
            playing: false,
            record: false,
            recording: false,
            latency: last.latency,
        });
        let mut tracks = Vec::new();
        let mut notes = Vec::new();
        for track in &self.held.tracks {
            if let Some(held) = self.held.notes_of(&track.input) {
                if let Some((buffer, _)) = &held.held {
                    notes.push(MidiTape::from_tap(track.clone(), buffer.clone(), start));
                }
                continue;
            }
            match &track.input {
                input @ (Input::Audio(_) | Input::Tap(_)) => {
                    let held = (self.held.audio.iter())
                        .find(|held| same_source(input, &held.input))
                        .and_then(|held| held.held.as_ref());
                    let Some((buffer, _)) = held else {
                        continue;
                    };
                    tracks.push(TrackTape {
                        armed: track.clone(),
                        input: input.clone(),
                        buffer: buffer.clone(),
                        channels: InputChannels {
                            index: 0,
                            channel: 0,
                            first: 0,
                            count: buffer.channels(),
                        },
                        // Where it holds the input from: what played at an
                        // engine time was heard then or later.
                        done: start.max(buffer.frames().start),
                        file: None,
                        files: Vec::new(),
                        passes: Vec::new(),
                        lost: 0,
                        failed: None,
                    });
                }
                Input::Midi(input) => {
                    let buffers: Vec<MidiBuffer> = (self.held.midi.iter())
                        .filter(|held| takes_midi(input, &held.id))
                        .filter_map(|held| held.held.as_ref().map(|(buffer, _)| buffer.clone()))
                        .collect();
                    if !buffers.is_empty() {
                        let source = MidiSource {
                            input: None,
                            channel: input.channel,
                        };
                        notes.push(MidiTape::new(track.clone(), source, buffers, start));
                    }
                }
            }
        }
        let mut tape = Tape {
            rate: self.rate,
            place: self.place.clone(),
            project: self.project,
            segments,
            until: at,
            file_frames: self.file_frames,
            ..Tape::new()
        };
        let mut running = Running {
            start,
            rate: self.rate,
            place: self.place.clone(),
            project: self.project,
            tracks,
            notes,
            end: Some(end),
            captured: true,
        };
        tape.advance(&mut running, end, true);
        tape.finish(running);
        let reports = tape.take_reports();
        if reports.is_empty() {
            self.reports.push(Report::Problem(
                "Nothing to capture: the transport recorded what it last played, or the tracks' \
                 inputs didn't play then."
                    .to_owned(),
            ));
        }
        self.reports.extend(reports);
    }
}

/// Why the inputs `tracks` take, of `kind`, aren't held: they'd take the
/// held inputs past `limit` bytes.
fn refusal<'a>(tracks: impl Iterator<Item = &'a Armed>, kind: &str, limit: u64) -> String {
    let names: Vec<&str> = tracks.map(|track| track.name.as_str()).collect();
    format!(
        "Capture doesn't hold {}'s {kind}: holding it would take Capture past {} of memory. A \
         shorter length, on the Recording page of Settings, holds more inputs.",
        names.join(" and "),
        memory(limit)
    )
}

/// Whether two audio tracks' inputs take the same channels of the same
/// input, or the same tap, so one held input serves both.
fn same_source(a: &Input, b: &Input) -> bool {
    match (a, b) {
        (Input::Audio(a), Input::Audio(b)) => same_channels(a, b),
        (Input::Tap(a), Input::Tap(b)) => a == b,
        _ => false,
    }
}

/// Whether two track inputs take the same channels of the same input.
fn same_channels(a: &AudioInput, b: &AudioInput) -> bool {
    (&a.device, a.channel, a.stereo) == (&b.device, b.channel, b.stereo)
}

/// Whether `track` takes the held audio input or tap `input`.
fn takes_input(track: &Armed, input: &Input) -> bool {
    same_source(&track.input, input)
}

/// Whether a track taking `input` takes the MIDI input `id`.
fn takes_midi(input: &MidiInput, id: &str) -> bool {
    input.device.is_empty() || input.device == id
}

/// `bytes` as people read it, in whole GiB, MiB or KiB.
fn memory(bytes: u64) -> String {
    match bytes {
        1_073_741_824.. => format!("{} GiB", bytes >> 30),
        1_048_576.. => format!("{} MiB", bytes >> 20),
        _ => format!("{} KiB", bytes >> 10),
    }
}

/// Copies the frames of `source`'s `channels` that `writer` hasn't got
/// yet, interleaved, as the next frames, after silence for those it never
/// will, such as from while the stream opened again.
fn copy(
    source: &InputBuffer,
    channels: InputChannels,
    writer: &mut InputWriter,
    scratch: &mut Vec<f32>,
    interleaved: &mut Vec<f32>,
) {
    let available = source.frames();
    if available.is_empty() {
        return;
    }
    if !writer.has_begun() {
        writer.begin(available.start);
    }
    let mut from = writer.end();
    if from < available.start {
        writer.append_silence(available.start - from);
        from = available.start;
    }
    let count = channels.count;
    while from < available.end {
        let n = (available.end - from).min(CHUNK) as usize;
        interleaved.clear();
        interleaved.resize(n * count, 0.0);
        scratch.resize(n, 0.0);
        let mut read = Ok(());
        for c in 0..count {
            read = source.read_channel(from, channels.channel + c, &mut scratch[..n]);
            if read.is_err() {
                break;
            }
            for (i, &sample) in scratch[..n].iter().enumerate() {
                interleaved[i * count + c] = sample;
            }
        }
        match read {
            Ok(()) => writer.append(interleaved),
            // Overwritten before it was copied: the recorder fell more than
            // the stream's buffer behind.
            Err(_) => writer.append_silence(n as u64),
        }
        from += n as u64;
    }
}

/// The transport's last run up to engine time `at`, as far as the
/// timeline's `segments` go back: the run's segments, from where it last
/// started playing or the oldest, and where it stopped, or `at`. `None` if
/// it hasn't played.
fn last_run(segments: &VecDeque<Segment>, at: u64) -> Option<(Range<usize>, u64)> {
    let last = (segments.iter()).rposition(|segment| segment.playing && segment.time < at)?;
    let end = segments.get(last + 1).map_or(at, |next| next.time.min(at));
    let first = (segments.range(..last).rposition(|segment| !segment.playing)).map_or(0, |i| i + 1);
    Some((first..last + 1, end))
}

//! Recording: what armed tracks' inputs capture while the transport
//! records, written to audio files on a thread of its own, or for MIDI
//! tracks, the notes their inputs play, made into takes when recording
//! stops.
//!
//! The recorder reads what the audio thread writes, without asking it
//! anything: the engine's [`Timeline`], which says where in the song each
//! frame it rendered played and whether it recorded, the inputs'
//! [`InputBuffer`]s, where frame `w` is what an input captured as engine
//! frame `w` was heard, and the MIDI inputs' [`MidiBuffer`]s, where each
//! message has the engine frame heard as it came in. What was heard at `w`
//! left the clips the graph's latency before, so a frame captured at `w`
//! goes where the song was at `w` minus the latency, and minus the input's
//! own calibration offset, and a MIDI message the same way.
//!
//! For Capture, it also holds the last few minutes of what the inputs of
//! armed tracks, and of those monitoring In, brought, and the timeline over
//! as long. Capturing records what they held over the transport's last
//! run, where it didn't record, as if they had recorded it then.
//! `docs/app.md` describes it.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use musicable_audio::{InputBuffer, MidiBuffer, ReadError};
use musicable_commands::editor::AudioFile;
use musicable_commands::take::{Pass, Recorded};
use musicable_engine::timeline::Read;
use musicable_engine::{InputChannels, InputLayout, MidiSource, Segment, Timeline};
use musicable_io::audio::WavWriter;
use musicable_model::{AudioInput, Input, TrackId};

use self::capture::{HELD_BYTES, Held};
use self::midi::MidiTape;
use crate::audio_files::clip_path;

/// How often the recorder's thread looks for more to write.
const POLL: Duration = Duration::from_millis(10);
/// The most frames the recorder writes at a time, for each track.
const CHUNK: u64 = 4096;
/// How much audio a file gets between updates of the length in its header,
/// in seconds: a file cut off by a crash reads back to about there.
const FLUSH_SECONDS: u64 = 1;
/// How far behind the engine, in seconds, an input can fall before a
/// recording stops waiting for it and records silence in its place: an
/// input that stalled.
const PATIENCE_SECONDS: u64 = 2;
/// A track to record when the transport records: an armed audio or MIDI
/// track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Armed {
    pub track: TrackId,
    /// The track's name, which its files are named after.
    pub name: String,
    /// An audio input for an audio track, a MIDI input for a MIDI track.
    pub input: Input,
    /// How many frames late the input buffer has what the input captured:
    /// the input's calibration, beyond the latency its driver reports.
    pub offset: i64,
}

/// Where recordings go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    /// The folder new files go in.
    pub folder: PathBuf,
    /// The folder of the project's file, which clips' relative paths start
    /// from. A project that isn't saved has none, and its clips name their
    /// recordings by their absolute paths.
    pub project: Option<PathBuf>,
}

/// What the recorder's thread reports.
#[derive(Debug)]
pub(crate) enum Report {
    /// A recording stopped, or with `captured`, a capture was taken: what
    /// each track recorded, a pass at a time, at `rate`, and the files it's
    /// in, as clips name them. `project` is what [`Recorder::set_tracks`]
    /// was given as it started.
    Finished {
        project: u64,
        rate: u32,
        recorded: Vec<Recorded>,
        files: Vec<(String, AudioFile)>,
        captured: bool,
    },
    /// Something went wrong, for the status bar.
    Problem(String),
}

/// A stretch of the song a track has recorded so far, while it records,
/// for showing it: from song position `start`, `frames` long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recording {
    pub track: TrackId,
    pub start: u64,
    pub frames: u64,
}

enum Command {
    Timeline(Timeline),
    Inputs(InputLayout, Buffers),
    Tracks {
        rate: u32,
        armed: Vec<Armed>,
        place: Option<Place>,
        project: u64,
    },
    Hold {
        tracks: Vec<Armed>,
        seconds: u64,
    },
    Capture(u64),
}

/// Records on a thread of its own, `musicable-recorder`: the control
/// thread's side. It starts a recording when the timeline says the
/// transport plays with recording on, and reports it when the timeline says
/// it stopped.
pub(crate) struct Recorder {
    commands: Option<Sender<Command>>,
    reports: Receiver<Report>,
    live: Arc<Mutex<Vec<Recording>>>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn spawn() -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (report_tx, reports) = mpsc::channel();
        let live = Arc::new(Mutex::new(Vec::new()));
        let shown = live.clone();
        let thread = thread::Builder::new()
            .name("musicable-recorder".into())
            .spawn(move || run(&command_rx, &report_tx, &shown))
            .expect("failed to spawn the recorder's thread");
        Self {
            commands: Some(commands),
            reports,
            live,
            thread: Some(thread),
        }
    }

    /// Follows the engine's `timeline` from now on, such as a new engine's.
    /// A recording on the old one stops where the old one got to.
    pub fn set_timeline(&self, timeline: Timeline) {
        self.send(Command::Timeline(timeline));
    }

    /// Records from the stream's inputs from now on: their buffers, in the
    /// order `layout` has them, its inputs' and then its MIDI inputs'.
    pub fn set_inputs(&self, layout: InputLayout, buffers: Buffers) {
        self.send(Command::Inputs(layout, buffers));
    }

    /// Records `armed` at `rate` into files in `place` from the next
    /// recording on, and reports it as a recording of `project`. Without a
    /// place, armed tracks fail to record.
    pub fn set_tracks(&self, rate: u32, armed: Vec<Armed>, place: Option<Place>, project: u64) {
        self.send(Command::Tracks {
            rate,
            armed,
            place,
            project,
        });
    }

    /// Holds what the inputs of `tracks`, armed tracks and those
    /// monitoring In, bring from now on, the last `seconds` of it at least,
    /// for [`capture`](Self::capture). With 0 seconds, it holds nothing.
    pub fn set_held(&self, tracks: Vec<Armed>, seconds: u64) {
        self.send(Command::Hold { tracks, seconds });
    }

    /// Records what the held tracks' inputs played over the transport's
    /// last run up to engine time `at`, as Capture was asked for, where it
    /// didn't record, as if they had recorded it then, and reports it as
    /// [`Report::Finished`], `captured`, or else says why there's nothing
    /// to take.
    pub fn capture(&self, at: u64) {
        self.send(Command::Capture(at));
    }

    /// The next report, if there is one.
    pub fn try_recv(&self) -> Option<Report> {
        self.reports.try_recv().ok()
    }

    /// What the tracks have recorded so far, while they record.
    pub fn live(&self) -> Vec<Recording> {
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            // Only fails if the thread is gone, which nothing reports: what
            // it recorded is lost then anyway.
            let _ = commands.send(command);
        }
    }
}

impl Drop for Recorder {
    /// Stops the thread, which finishes a recording that's running first.
    fn drop(&mut self) {
        drop(self.commands.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The recorder's thread.
fn run(commands: &Receiver<Command>, reports: &Sender<Report>, live: &Mutex<Vec<Recording>>) {
    let mut tape = Tape::new();
    let mut timeline: Option<Timeline> = None;
    let mut segments = Vec::new();
    let mut follow = |timeline: &mut Option<Timeline>, tape: &mut Tape| {
        if let Some(timeline) = timeline {
            segments.clear();
            let read = timeline.read(&mut segments);
            tape.add(&segments, read);
        }
    };
    loop {
        let mut next = match commands.recv_timeout(POLL) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                follow(&mut timeline, &mut tape);
                tape.cut();
                for report in tape.take_reports() {
                    let _ = reports.send(report);
                }
                return;
            }
        };
        // The tape takes what the timeline says before it carries out the
        // commands sent so far, and follows it after, so what was sent
        // before the transport changed, such as the inputs of a stream
        // opened just before recording started, comes first, however late
        // this thread gets to it.
        follow(&mut timeline, &mut tape);
        while let Some(command) = next.take().or_else(|| commands.try_recv().ok()) {
            match command {
                Command::Timeline(new) => {
                    // As far as the old engine got.
                    follow(&mut timeline, &mut tape);
                    tape.cut();
                    tape.restart_held();
                    timeline = Some(new);
                }
                Command::Inputs(layout, buffers) => tape.set_inputs(layout, buffers),
                Command::Tracks {
                    rate,
                    armed,
                    place,
                    project,
                } => tape.set_tracks(rate, armed, place, project),
                Command::Hold { tracks, seconds } => tape.set_held(tracks, seconds),
                Command::Capture(at) => tape.ask_capture(at),
            }
        }
        tape.step();
        for report in tape.take_reports() {
            let _ = reports.send(report);
        }
        *live.lock().unwrap_or_else(PoisonError::into_inner) = tape.live();
    }
}

/// The buffers of a stream's inputs and of its MIDI inputs, each in the
/// order the stream has them: `None` for one that didn't open.
#[derive(Clone, Default)]
pub(crate) struct Buffers {
    pub audio: Vec<Option<InputBuffer>>,
    pub midi: Vec<Option<MidiBuffer>>,
}

/// What the recorder's thread keeps: the timeline so far, the inputs, the
/// tracks to record, the recording running, and what Capture holds.
struct Tape {
    rate: u32,
    armed: Vec<Armed>,
    place: Option<Place>,
    project: u64,
    layout: InputLayout,
    buffers: Buffers,
    /// The timeline, from the segment the recording running, or the
    /// transport, is in now.
    segments: VecDeque<Segment>,
    /// The engine time up to which the segments describe the transport.
    until: u64,
    running: Option<Running>,
    reports: Vec<Report>,
    /// Scratch space for reading inputs and interleaving them.
    scratch: Vec<f32>,
    interleaved: Vec<f32>,
    /// The most frames a file holds, short of the 4 GB a WAV file can:
    /// fewer in tests, to see a recording go on in another file.
    file_frames: u64,
    held: Held,
    /// The engine time Capture was asked for at, while it waits for the
    /// timeline to get there, and the held inputs to have what was heard up
    /// to then.
    capture_at: Option<u64>,
    /// The most memory the held inputs take together, in bytes: less in
    /// tests.
    held_bytes: u64,
}

/// A recording running.
struct Running {
    /// The engine time it started at.
    start: u64,
    /// The sample rate it records at, where its files go, and what it's a
    /// recording of.
    rate: u32,
    place: Option<Place>,
    project: u64,
    tracks: Vec<TrackTape>,
    /// The MIDI tracks.
    notes: Vec<MidiTape>,
    /// The engine time recording stopped at, once the timeline says.
    end: Option<u64>,
    /// Whether it's a capture, of what was held.
    captured: bool,
}

impl Running {
    /// Whether every track has recorded up to where recording stopped, or
    /// failed.
    fn is_finished(&self) -> bool {
        self.end.is_some_and(|end| {
            let audio = (self.tracks.iter()).all(|t| t.done >= end || t.failed.is_some());
            audio && self.notes.iter().all(|t| t.done >= end)
        })
    }
}

/// What one audio track records.
struct TrackTape {
    armed: Armed,
    input: AudioInput,
    buffer: InputBuffer,
    channels: InputChannels,
    /// The engine time it has recorded up to.
    done: u64,
    file: Option<OpenFile>,
    /// The files it finished, as clips name them.
    files: Vec<(String, AudioFile)>,
    passes: Vec<Pass>,
    /// Frames it recorded as silence, because its input didn't have them.
    lost: u64,
    /// Why it stopped recording, if writing failed.
    failed: Option<String>,
}

struct OpenFile {
    writer: WavWriter<BufWriter<File>>,
    path: PathBuf,
    /// What clips call it.
    name: String,
    /// Frames written when the header last got the length.
    flushed: u64,
}

impl Tape {
    fn new() -> Self {
        Self {
            rate: 48_000,
            armed: Vec::new(),
            place: None,
            project: 0,
            layout: InputLayout::new(),
            buffers: Buffers::default(),
            segments: VecDeque::new(),
            until: 0,
            running: None,
            reports: Vec::new(),
            scratch: Vec::new(),
            interleaved: Vec::new(),
            file_frames: u64::MAX,
            held: Held::default(),
            capture_at: None,
            held_bytes: HELD_BYTES,
        }
    }

    fn set_tracks(&mut self, rate: u32, armed: Vec<Armed>, place: Option<Place>, project: u64) {
        if project != self.project {
            // What the last project's tracks played isn't the next's.
            self.drop_held();
        }
        self.rate = rate;
        self.armed = armed;
        self.place = place;
        self.project = project;
    }

    /// Takes the stream's inputs. Tracks recording now go on from their
    /// inputs' new buffers: what the old ones didn't capture is silent, and
    /// what the old MIDI inputs hadn't brought is missing. The held inputs
    /// go on the same way.
    fn set_inputs(&mut self, layout: InputLayout, buffers: Buffers) {
        // The last of what the old buffers have.
        self.hold();
        if let Some(running) = &mut self.running {
            for track in &mut running.tracks {
                if let Some((buffer, channels)) = find(&layout, &buffers.audio, &track.input) {
                    track.buffer = buffer;
                    track.channels = channels;
                }
            }
            for track in &mut running.notes {
                if let Input::Midi(input) = &track.armed.input
                    && let Some(source) = layout.find_midi(input)
                {
                    track.source = source;
                    track.set_inputs(midi_buffers(source, &buffers.midi));
                }
            }
        }
        self.layout = layout;
        self.buffers = buffers;
        self.hold_from_new_inputs();
    }

    /// Takes the segments the timeline published since the last read.
    fn add(&mut self, segments: &[Segment], read: Read) {
        if read.lost {
            // They don't tell the transport's whole course since the last
            // read: a recording stops where that left off, and the tape
            // waits for the transport to change course.
            if self.running.is_some() || segments.iter().any(|s| s.record) {
                self.reports.push(Report::Problem(
                    "Recording stopped: the recorder lost track of the transport.".to_owned(),
                ));
            }
            self.cut();
            self.until = read.until;
            return;
        }
        self.segments.extend(segments);
        self.until = self.until.max(read.until);
        self.hold_timeline(segments);
    }

    /// The timeline ends where it has got to, such as when the engine is
    /// replaced: a recording running stops there, with what its inputs have
    /// captured, Capture takes what's held if it was asked to, and the next
    /// timeline starts its own times.
    fn cut(&mut self) {
        self.take_capture_now();
        self.drop_timeline();
        if let Some(mut running) = self.running.take() {
            let end = running
                .end
                .or_else(|| self.end_after(running.start))
                .unwrap_or(self.until);
            running.end = Some(end);
            self.advance(&mut running, end, true);
            self.finish(running);
        }
        self.segments.clear();
        self.until = 0;
    }

    /// Holds what the held inputs brought since, takes a capture once its
    /// inputs have caught up, writes what it can, and finishes a recording
    /// that stopped once its tracks have caught up.
    fn step(&mut self) {
        self.hold();
        self.take_capture();
        while self.running.is_some() || self.start() {
            let mut running = self.running.take().expect("a recording runs");
            if running.end.is_none() {
                running.end = self.end_after(running.start);
            }
            let limit = running.end.unwrap_or(self.until);
            self.advance(&mut running, limit, false);
            if !running.is_finished() {
                self.running = Some(running);
                self.prune();
                return;
            }
            self.finish(running);
        }
    }

    /// Starts a recording if the timeline has the transport play with
    /// recording on. Returns whether it did.
    fn start(&mut self) -> bool {
        let Some(at) = self.segments.iter().position(|s| s.playing && s.record) else {
            // Only the last segment can matter: it runs on.
            let past = self.segments.len().saturating_sub(1);
            self.segments.drain(..past);
            return false;
        };
        self.segments.drain(..at);
        let start = self.segments[0].time;
        let mut tracks = Vec::new();
        let mut notes = Vec::new();
        for armed in &self.armed {
            match &armed.input {
                Input::Audio(input) => match find(&self.layout, &self.buffers.audio, input) {
                    Some((buffer, channels)) => tracks.push(TrackTape {
                        armed: armed.clone(),
                        input: input.clone(),
                        buffer,
                        channels,
                        done: start,
                        file: None,
                        files: Vec::new(),
                        passes: Vec::new(),
                        lost: 0,
                        failed: None,
                    }),
                    None => self.reports.push(Report::Problem(format!(
                        "{} records nothing: its input isn't open.",
                        armed.name
                    ))),
                },
                Input::Midi(input) => {
                    let source = self.layout.find_midi(input);
                    let buffers = source.map(|s| midi_buffers(s, &self.buffers.midi));
                    match source.zip(buffers.filter(|b| !b.is_empty())) {
                        Some((source, buffers)) => {
                            notes.push(MidiTape::new(armed.clone(), source, buffers, start));
                        }
                        None => self.reports.push(Report::Problem(format!(
                            "{} records nothing: its MIDI input isn't open.",
                            armed.name
                        ))),
                    }
                }
            }
        }
        self.running = Some(Running {
            start,
            rate: self.rate,
            place: self.place.clone(),
            project: self.project,
            tracks,
            notes,
            end: None,
            captured: false,
        });
        true
    }

    /// Where the transport stopped playing with recording on, after a
    /// recording started at engine time `start`, if the timeline says yet.
    fn end_after(&self, start: u64) -> Option<u64> {
        self.segments
            .iter()
            .find(|s| s.time > start && !(s.playing && s.record))
            .map(|s| s.time)
    }

    /// Records what each of `running`'s tracks captured, from where it got
    /// to up to engine time `limit`, as far as its input has it, or with
    /// `forced`, all the way, with silence for what the input hasn't.
    fn advance(&mut self, running: &mut Running, limit: u64, forced: bool) {
        let Running {
            rate,
            place,
            tracks,
            notes,
            ..
        } = running;
        for track in tracks {
            self.advance_track(track, *rate, place.as_ref(), limit, forced);
        }
        for track in notes {
            self.advance_notes(track, *rate, limit, forced);
        }
    }

    /// Records what `track`'s MIDI inputs played, from where it got to up
    /// to engine time `limit`, as far as they have brought every message,
    /// or with `forced`, all the way.
    fn advance_notes(&self, track: &mut MidiTape, rate: u32, limit: u64, forced: bool) {
        while track.done < limit {
            let e = track.done;
            let index = self.segments.partition_point(|s| s.time <= e);
            let Some(segment) = index.checked_sub(1).map(|i| self.segments[i]) else {
                // Before the timeline starts: nothing played.
                track.done = self.segments.front().map_or(limit, |s| s.time.min(limit));
                continue;
            };
            let mut until = (self.segments.get(index)).map_or(limit, |next| next.time.min(limit));
            // How far its inputs have brought every message, in engine time.
            let lag = i128::from(segment.latency) + i128::from(track.armed.offset);
            let settled = i128::from(track.settled()) - lag;
            let settled = u64::try_from(settled.max(0)).unwrap_or(u64::MAX);
            let stalled = self.until.saturating_sub(settled) > u64::from(rate) * PATIENCE_SECONDS;
            if settled < until && !forced && !stalled {
                if settled <= e {
                    break;
                }
                until = settled;
            }
            track.record(&segment, until);
        }
    }

    fn advance_track(
        &mut self,
        track: &mut TrackTape,
        rate: u32,
        place: Option<&Place>,
        limit: u64,
        forced: bool,
    ) {
        while track.done < limit && track.failed.is_none() {
            let e = track.done;
            let index = self.segments.partition_point(|s| s.time <= e);
            let Some(segment) = index.checked_sub(1).map(|i| self.segments[i]) else {
                // Before the timeline starts: nothing played.
                track.done = self.segments.front().map_or(limit, |s| s.time.min(limit));
                continue;
            };
            let until = self
                .segments
                .get(index)
                .map_or(limit, |next| next.time.min(limit));
            if !segment.recording {
                track.done = until;
                continue;
            }
            // Where the input has what was heard as the song played `e`.
            let at = i128::from(e) + i128::from(segment.latency) + i128::from(track.armed.offset);
            let want = (until - e).min(CHUNK);
            let Some(frames) = self.read(track, at, want, rate, forced) else {
                break;
            };
            let position = segment.position_at(e);
            if let Err(error) = self.write(track, rate, place, position, frames) {
                let path = track.file.as_ref().map(|f| f.path.display().to_string());
                let failed = format!(
                    "Couldn't write {}'s recording to {}: {error}. It stopped recording.",
                    track.armed.name,
                    path.unwrap_or_else(|| "a new file".to_owned())
                );
                self.reports.push(Report::Problem(failed.clone()));
                track.failed = Some(failed);
                break;
            }
            track.done += frames;
        }
    }

    /// Reads up to `want` frames of `track`'s input from input frame `at`
    /// into `self.interleaved`, and returns how many, or `None` if the input
    /// hasn't captured them yet. Frames it never will have are silent: ones
    /// from before it started or that it no longer holds, and once it has
    /// ended, stalled, or with `forced`, ones it hasn't captured.
    fn read(
        &mut self,
        track: &mut TrackTape,
        at: i128,
        want: u64,
        rate: u32,
        forced: bool,
    ) -> Option<u64> {
        let count = track.channels.count;
        // Whether it has ended first: then it has every frame it will have.
        let ended = track.buffer.has_ended();
        let available = track.buffer.frames();
        let mut silent = |frames: u64| {
            track.lost += frames;
            self.interleaved.clear();
            self.interleaved.resize(frames as usize * count, 0.0);
            Some(frames)
        };
        let Ok(from) = u64::try_from(at) else {
            // Before the engine started.
            let before = u64::try_from(-at).unwrap_or(u64::MAX);
            return silent(want.min(before));
        };
        if from < available.start {
            return silent(want.min(available.start - from));
        }
        if from >= available.end {
            let stalled = self.until.saturating_sub(from) > u64::from(rate) * PATIENCE_SECONDS;
            return if ended || stalled || forced {
                silent(want)
            } else {
                None
            };
        }
        let frames = want.min(available.end - from);
        let n = frames as usize;
        self.interleaved.clear();
        self.interleaved.resize(n * count, 0.0);
        self.scratch.resize(n, 0.0);
        for c in 0..count {
            let channel = track.channels.channel + c;
            match track
                .buffer
                .read_channel(from, channel, &mut self.scratch[..n])
            {
                Ok(()) => {
                    for (i, &sample) in self.scratch[..n].iter().enumerate() {
                        self.interleaved[i * count + c] = sample;
                    }
                }
                // Overwritten while it was read: the recorder fell more
                // than the buffer behind.
                Err(ReadError::Gone) => {
                    track.lost += frames;
                    self.interleaved.fill(0.0);
                    return Some(frames);
                }
                // It only reads what the input has, so never.
                Err(ReadError::NotYet) => return None,
            }
        }
        Some(frames)
    }

    /// Writes the `frames` frames in `self.interleaved` to `track`'s file,
    /// at song position `position`, in a new pass unless they carry on the
    /// last one.
    fn write(
        &mut self,
        track: &mut TrackTape,
        rate: u32,
        place: Option<&Place>,
        position: u64,
        frames: u64,
    ) -> io::Result<()> {
        let count = track.channels.count;
        let room = |file: &OpenFile| {
            let frames = file.writer.frames();
            file.writer
                .room()
                .min(self.file_frames.saturating_sub(frames))
        };
        let mut written = 0;
        while written < frames {
            if track.file.as_ref().is_none_or(|f| room(f) == 0) {
                close(track)?;
                track.file = Some(create(place, rate, &track.armed.name, count as u16)?);
            }
            let file = track.file.as_mut().expect("just opened");
            let n = (frames - written).min(room(file));
            let position = position + written;
            let carries_on = track
                .passes
                .last()
                .is_some_and(|pass| pass.file == file.name && pass.start + pass.frames == position);
            if !carries_on {
                track.passes.push(Pass {
                    start: position,
                    frames: 0,
                    file: file.name.clone(),
                    offset: file.writer.frames(),
                });
            }
            let from = written as usize * count;
            file.writer
                .write(&self.interleaved[from..from + n as usize * count])?;
            if file.writer.frames() - file.flushed >= u64::from(rate) * FLUSH_SECONDS {
                file.writer.flush()?;
                file.flushed = file.writer.frames();
            }
            track.passes.last_mut().expect("a pass runs").frames += n;
            written += n;
        }
        Ok(())
    }

    /// Reports what a recording that stopped recorded, and forgets the
    /// timeline up to where it stopped.
    fn finish(&mut self, running: Running) {
        let end = running
            .end
            .expect("a recording finishes once it has stopped");
        let what = if running.captured {
            "capture"
        } else {
            "recording"
        };
        let mut recorded = Vec::new();
        let mut files = Vec::new();
        for mut track in running.tracks {
            if track.failed.is_some() {
                continue;
            }
            if let Err(error) = close(&mut track) {
                self.reports.push(Report::Problem(format!(
                    "Couldn't finish {}'s {what}: {error}.",
                    track.armed.name
                )));
                continue;
            }
            if track.lost > 0 {
                let seconds = track.lost as f64 / f64::from(running.rate);
                self.reports.push(Report::Problem(format!(
                    "{seconds:.2} seconds of {}'s {what} are silent: its input didn't have them.",
                    track.armed.name
                )));
            }
            files.extend(track.files);
            track.passes.retain(|pass| pass.frames > 0);
            if !track.passes.is_empty() {
                recorded.push(Recorded::Audio {
                    track: track.armed.track,
                    passes: track.passes,
                });
            }
        }
        for tape in running.notes {
            let lost = tape.lost();
            if lost > 0 {
                self.reports.push(Report::Problem(format!(
                    "{lost} MIDI messages of {}'s recording are missing: the recorder fell \
                     behind.",
                    tape.armed.name
                )));
            }
            let track = tape.armed.track;
            let passes = tape.finish();
            if !passes.is_empty() {
                recorded.push(Recorded::Midi { track, passes });
            }
        }
        if !recorded.is_empty() {
            self.reports.push(Report::Finished {
                project: running.project,
                rate: running.rate,
                recorded,
                files,
                captured: running.captured,
            });
        }
        // The next recording starts after this one.
        while self.segments.front().is_some_and(|s| s.time < end) {
            self.segments.pop_front();
        }
    }

    /// Forgets the segments behind every track of the recording running.
    fn prune(&mut self) {
        let Some(running) = &self.running else {
            return;
        };
        let audio = (running.tracks.iter())
            .filter(|t| t.failed.is_none())
            .map(|t| t.done);
        let notes = running.notes.iter().map(|t| t.done);
        let oldest = audio.chain(notes).min().unwrap_or(self.until);
        while self.segments.len() > 1 && self.segments[1].time <= oldest {
            self.segments.pop_front();
        }
    }

    /// What the tracks have recorded so far, while they record.
    fn live(&self) -> Vec<Recording> {
        let Some(running) = &self.running else {
            return Vec::new();
        };
        let audio = running.tracks.iter().flat_map(|track| {
            (track.passes.iter()).map(|pass| (track.armed.track, pass.start, pass.frames))
        });
        let notes = running.notes.iter().flat_map(|track| {
            (track.passes.iter()).map(|pass| (track.armed.track, pass.start, pass.frames))
        });
        audio
            .chain(notes)
            .map(|(track, start, frames)| Recording {
                track,
                start,
                frames,
            })
            .collect()
    }

    fn take_reports(&mut self) -> Vec<Report> {
        std::mem::take(&mut self.reports)
    }
}

/// A new file for a track called `name`, in the recordings' folder.
fn create(place: Option<&Place>, rate: u32, name: &str, channels: u16) -> io::Result<OpenFile> {
    let place = place.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "there's no folder for recordings")
    })?;
    std::fs::create_dir_all(&place.folder)?;
    let stem = file_stem(name);
    for n in 1.. {
        let path = place.folder.join(format!("{stem} {n}.wav"));
        match File::create_new(&path) {
            Ok(file) => {
                let writer = WavWriter::new(BufWriter::new(file), rate, channels)?;
                return Ok(OpenFile {
                    writer,
                    name: clip_path(&path, place.project.as_deref()),
                    path,
                    flushed: 0,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("some number is free")
}

/// Finishes `track`'s open file, if it has one.
fn close(track: &mut TrackTape) -> io::Result<()> {
    if let Some(file) = track.file.take() {
        let audio = AudioFile {
            sample_rate: file.writer.sample_rate(),
            frames: file.writer.frames(),
            channels: usize::from(file.writer.channels()),
        };
        file.writer.finish()?;
        track.files.push((file.name, audio));
    }
    Ok(())
}

/// The buffers of the MIDI inputs `source` takes, of the stream's
/// `buffers`, that opened.
fn midi_buffers(source: MidiSource, buffers: &[Option<MidiBuffer>]) -> Vec<MidiBuffer> {
    match source.input {
        Some(index) => buffers.get(index).cloned().flatten().into_iter().collect(),
        None => buffers.iter().flatten().cloned().collect(),
    }
}

/// The buffer `input` takes from, and where its channels are in it.
fn find(
    layout: &InputLayout,
    buffers: &[Option<InputBuffer>],
    input: &AudioInput,
) -> Option<(InputBuffer, InputChannels)> {
    let channels = layout.find(input)?;
    let buffer = buffers.get(channels.index)?.clone()?;
    (channels.channel + channels.count <= buffer.channels()).then_some((buffer, channels))
}

/// A file name for a track called `name`: its name, without what file
/// names can't have.
fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows drops dots and spaces at the end of a name.
    let stem = stem.trim().trim_end_matches('.').trim();
    if stem.is_empty() {
        "Recording".to_owned()
    } else {
        stem.to_owned()
    }
}

/// Where recordings of projects that aren't saved go: the Musicable
/// folder in the user's music folder.
pub(crate) fn unsaved_folder() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let home = if cfg!(windows) {
        var("USERPROFILE")
    } else {
        var("HOME")
    }?;
    Some(
        Path::new(&home)
            .join("Music")
            .join("Musicable")
            .join("Unsaved Recordings"),
    )
}

mod capture;
mod midi;
#[cfg(test)]
mod tests;

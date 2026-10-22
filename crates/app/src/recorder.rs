//! Recording: what armed tracks' inputs capture while the transport
//! records, written to audio files on a thread of its own, and made into
//! takes when recording stops.
//!
//! The recorder reads two things the audio thread writes, without asking it
//! anything: the engine's [`Timeline`], which says where in the song each
//! frame it rendered played and whether it recorded, and the inputs'
//! [`InputBuffer`]s, where frame `w` is what an input captured as engine
//! frame `w` was heard. What was heard at `w` left the clips the graph's
//! latency before, so a frame captured at `w` goes where the song was at
//! `w` minus the latency, and minus the input's own calibration offset.
//! `docs/app.md` describes it.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use musicable_audio::{InputBuffer, ReadError};
use musicable_commands::editor::AudioFile;
use musicable_commands::take::{Pass, Recorded};
use musicable_engine::timeline::Read;
use musicable_engine::{InputChannels, InputLayout, Segment, Timeline};
use musicable_io::audio::WavWriter;
use musicable_model::{AudioInput, TrackId};

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

/// A track to record when the transport records: an armed audio track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Armed {
    pub track: TrackId,
    /// The track's name, which its files are named after.
    pub name: String,
    pub input: AudioInput,
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
    /// A recording stopped: what each track recorded, a pass at a time, at
    /// `rate`, and the files it's in, as clips name them. `project` is what
    /// [`Recorder::set_tracks`] was given as it started.
    Finished {
        project: u64,
        rate: u32,
        recorded: Vec<Recorded>,
        files: Vec<(String, AudioFile)>,
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
    Inputs(InputLayout, Vec<Option<InputBuffer>>),
    Tracks {
        rate: u32,
        armed: Vec<Armed>,
        place: Option<Place>,
        project: u64,
    },
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
    /// order `layout` has them.
    pub fn set_inputs(&self, layout: InputLayout, buffers: Vec<Option<InputBuffer>>) {
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
        match commands.recv_timeout(POLL) {
            Ok(Command::Timeline(new)) => {
                // As far as the old engine got.
                follow(&mut timeline, &mut tape);
                tape.cut();
                timeline = Some(new);
            }
            Ok(Command::Inputs(layout, buffers)) => tape.set_inputs(layout, buffers),
            Ok(Command::Tracks {
                rate,
                armed,
                place,
                project,
            }) => tape.set_tracks(rate, armed, place, project),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                follow(&mut timeline, &mut tape);
                tape.cut();
                for report in tape.take_reports() {
                    let _ = reports.send(report);
                }
                return;
            }
        }
        follow(&mut timeline, &mut tape);
        tape.step();
        for report in tape.take_reports() {
            let _ = reports.send(report);
        }
        *live.lock().unwrap_or_else(PoisonError::into_inner) = tape.live();
    }
}

/// What the recorder's thread keeps: the timeline so far, the inputs, the
/// tracks to record, and the recording running.
struct Tape {
    rate: u32,
    armed: Vec<Armed>,
    place: Option<Place>,
    project: u64,
    layout: InputLayout,
    buffers: Vec<Option<InputBuffer>>,
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
    /// The engine time recording stopped at, once the timeline says.
    end: Option<u64>,
}

impl Running {
    /// Whether every track has recorded up to where recording stopped, or
    /// failed.
    fn is_finished(&self) -> bool {
        self.end.is_some_and(|end| {
            self.tracks
                .iter()
                .all(|t| t.done >= end || t.failed.is_some())
        })
    }
}

/// What one track records.
struct TrackTape {
    armed: Armed,
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
            buffers: Vec::new(),
            segments: VecDeque::new(),
            until: 0,
            running: None,
            reports: Vec::new(),
            scratch: Vec::new(),
            interleaved: Vec::new(),
            file_frames: u64::MAX,
        }
    }

    fn set_tracks(&mut self, rate: u32, armed: Vec<Armed>, place: Option<Place>, project: u64) {
        self.rate = rate;
        self.armed = armed;
        self.place = place;
        self.project = project;
    }

    /// Takes the stream's inputs. Tracks recording now go on from their
    /// inputs' new buffers: what the old ones didn't capture is silent.
    fn set_inputs(&mut self, layout: InputLayout, buffers: Vec<Option<InputBuffer>>) {
        if let Some(running) = &mut self.running {
            for track in &mut running.tracks {
                if let Some((buffer, channels)) = find(&layout, &buffers, &track.armed.input) {
                    track.buffer = buffer;
                    track.channels = channels;
                }
            }
        }
        self.layout = layout;
        self.buffers = buffers;
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
    }

    /// The timeline ends where it has got to, such as when the engine is
    /// replaced: a recording running stops there, with what its inputs have
    /// captured, and the next timeline starts its own times.
    fn cut(&mut self) {
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

    /// Writes what it can, and finishes a recording that stopped once its
    /// tracks have caught up.
    fn step(&mut self) {
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
        for armed in &self.armed {
            match find(&self.layout, &self.buffers, &armed.input) {
                Some((buffer, channels)) => tracks.push(TrackTape {
                    armed: armed.clone(),
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
            }
        }
        self.running = Some(Running {
            start,
            rate: self.rate,
            place: self.place.clone(),
            project: self.project,
            tracks,
            end: None,
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
            ..
        } = running;
        for track in tracks {
            self.advance_track(track, *rate, place.as_ref(), limit, forced);
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
        let mut recorded = Vec::new();
        let mut files = Vec::new();
        for mut track in running.tracks {
            if track.failed.is_some() {
                continue;
            }
            if let Err(error) = close(&mut track) {
                self.reports.push(Report::Problem(format!(
                    "Couldn't finish {}'s recording: {error}.",
                    track.armed.name
                )));
                continue;
            }
            if track.lost > 0 {
                let seconds = track.lost as f64 / f64::from(running.rate);
                self.reports.push(Report::Problem(format!(
                    "{seconds:.2} seconds of {}'s recording are silent: its input didn't have \
                     them.",
                    track.armed.name
                )));
            }
            files.extend(track.files);
            track.passes.retain(|pass| pass.frames > 0);
            if !track.passes.is_empty() {
                recorded.push(Recorded {
                    track: track.armed.track,
                    passes: track.passes,
                });
            }
        }
        if !recorded.is_empty() {
            self.reports.push(Report::Finished {
                project: running.project,
                rate: running.rate,
                recorded,
                files,
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
        let oldest = running
            .tracks
            .iter()
            .filter(|t| t.failed.is_none())
            .map(|t| t.done)
            .min()
            .unwrap_or(self.until);
        while self.segments.len() > 1 && self.segments[1].time <= oldest {
            self.segments.pop_front();
        }
    }

    /// What the tracks have recorded so far, while they record.
    fn live(&self) -> Vec<Recording> {
        let Some(running) = &self.running else {
            return Vec::new();
        };
        running
            .tracks
            .iter()
            .flat_map(|track| {
                track.passes.iter().map(|pass| Recording {
                    track: track.armed.track,
                    start: pass.start,
                    frames: pass.frames,
                })
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

#[cfg(test)]
mod tests;

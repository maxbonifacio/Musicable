//! Exporting audio: the project rendered faster than real time, on a thread
//! of its own, to WAV, AIFF or FLAC files: the mix, and each track on its
//! own (stems). `docs/app.md` describes it.
//!
//! An export plays a copy of the project offline, in a session of its own,
//! as the null test's [`Player`](crate::null_test::Player) does, so it
//! renders what the app plays, sample for sample. The session has an engine
//! of its own, with the parallel scheduler on workers that don't take
//! real-time priority, and instances of its own of the project's plugins,
//! loaded on the main thread, which [`Export::tick`] services as the app
//! services its own session's.
//!
//! The copy plays without the metronome, looping or punching in, from the
//! start of the range to its end, where the transport stops. With the tail,
//! it renders on until everything has been quiet for two seconds, for 30
//! seconds at most, and leaves the quiet end off. Files start with the
//! range: the frames the graph's latency delays the output by come first,
//! and are skipped.
//!
//! Stems are taps on the tracks before their output stages
//! ([`compile_with_stems`](musicable_engine::compile_with_stems)): each
//! track as it feeds the mix, before mute and solo. They render with solo
//! cleared, so the buses get what isn't muted; when solo is on and the mix
//! is wanted too, the mix renders as it's heard, in a pass of its own.
//!
//! Each file is written as `NAME.part` and renamed once it's whole.
//! Normalising renders to a float file first, `NAME.float.part`, then
//! writes the file from it with the gain that takes its peak to full scale.

use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use musicable_commands::{Args, Editor, transport};
use musicable_engine::{AudioMut, Engine, EngineConfig, ParallelOptions, ParallelRenderer};
use musicable_io::audio::{Decoder, Format, SampleFormat, Writer};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackId};
use musicable_plugins::{PluginInfo, Waker};
use serde::{Deserialize, Serialize};

use crate::Session;

/// The app plays, and exports, in stereo.
const CHANNELS: usize = 2;
/// Frames rendered at a time.
const CHUNK: usize = 4096;
/// Quieter than this, a sample is silence as far as the tail goes: half a
/// 16-bit step, about −96 dBFS.
const QUIET: f32 = 1.0 / 65_536.0;
/// The tail ends once everything has been quiet this long.
const QUIET_FOR: Duration = Duration::from_secs(2);
/// The longest tail.
const MAX_TAIL: Duration = Duration::from_secs(30);

/// How an export writes its files: the export window's settings, kept in
/// the settings file from one export to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    /// `wav`, `aiff` or `flac`.
    pub format: FileFormat,
    /// 16, 24, or 32 for floats, which FLAC hasn't got.
    pub bits: Bits,
    /// None for the engine's rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Dither integer samples: see [`Writer::with_dither`].
    pub dither: bool,
    /// Raise or lower each file so its peak is at full scale.
    pub normalize: bool,
    /// Render on after the range until everything is quiet.
    pub tail: bool,
    /// Write the mix.
    pub mixdown: bool,
    /// Write each track on its own.
    pub stems: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: FileFormat::Wav,
            bits: Bits::Int24,
            sample_rate: None,
            dither: true,
            normalize: false,
            tail: true,
            mixdown: true,
            stems: false,
        }
    }
}

impl ExportSettings {
    /// How the files store samples. FLAC has no floats, so it's 24-bit
    /// there.
    pub fn sample_format(&self) -> SampleFormat {
        match (self.bits, self.format) {
            (Bits::Int16, _) => SampleFormat::Int16,
            (Bits::Int24, _) | (Bits::Float32, FileFormat::Flac) => SampleFormat::Int24,
            (Bits::Float32, _) => SampleFormat::Float32,
        }
    }
}

/// The file formats an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Wav,
    Aiff,
    Flac,
}

impl FileFormat {
    pub const ALL: [FileFormat; 3] = [Self::Wav, Self::Aiff, Self::Flac];

    pub fn name(self) -> &'static str {
        match self {
            Self::Wav => "WAV",
            Self::Aiff => "AIFF",
            Self::Flac => "FLAC",
        }
    }

    pub fn format(self) -> Format {
        match self {
            Self::Wav => Format::Wav,
            Self::Aiff => Format::Aiff,
            Self::Flac => Format::Flac,
        }
    }

    /// The extensions files of the format have, the usual one first.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Wav => &["wav", "wave"],
            Self::Aiff => &["aif", "aiff", "aifc"],
            Self::Flac => &["flac"],
        }
    }
}

/// How samples are stored: 16- or 24-bit integers, or 32-bit floats. In
/// the settings file, `16`, `24` or `32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum Bits {
    Int16,
    Int24,
    Float32,
}

impl Bits {
    pub const ALL: [Bits; 3] = [Self::Int16, Self::Int24, Self::Float32];

    pub fn name(self) -> &'static str {
        match self {
            Self::Int16 => "16-bit",
            Self::Int24 => "24-bit",
            Self::Float32 => "32-bit float",
        }
    }
}

impl TryFrom<u8> for Bits {
    type Error = String;

    fn try_from(bits: u8) -> Result<Self, String> {
        match bits {
            16 => Ok(Self::Int16),
            24 => Ok(Self::Int24),
            32 => Ok(Self::Float32),
            _ => Err(format!(
                "exports are 16-bit, 24-bit or 32-bit float, not {bits}-bit"
            )),
        }
    }
}

impl From<Bits> for u8 {
    fn from(bits: Bits) -> u8 {
        match bits {
            Bits::Int16 => 16,
            Bits::Int24 => 24,
            Bits::Float32 => 32,
        }
    }
}

/// Which stretch of the song an export renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// From the start of the song to the end of its last clip.
    Song,
    /// The loop region.
    Loop,
    /// The time selection, or else the selected clips.
    Selection,
}

impl Span {
    pub const ALL: [Span; 3] = [Self::Song, Self::Loop, Self::Selection];

    pub fn name(self) -> &'static str {
        match self {
            Self::Song => "Song",
            Self::Loop => "Loop",
            Self::Selection => "Selection",
        }
    }

    /// The stretch it is in `project`, with `editor`'s selection, if there
    /// is one and it isn't empty.
    pub fn range(self, project: &Project, editor: &Editor) -> Option<BeatRange> {
        let range = match self {
            Self::Song => BeatRange::new(Beats::ZERO, song_end(project)?),
            Self::Loop => project.loop_region,
            Self::Selection => editor.selected_range(project)?,
        };
        (!range.is_empty()).then_some(range)
    }

    /// The span an export starts out with: the selection if there is one,
    /// or else the song, or else the loop.
    pub fn default_for(project: &Project, editor: &Editor) -> Span {
        [Self::Selection, Self::Song]
            .into_iter()
            .find(|span| span.range(project, editor).is_some())
            .unwrap_or(Self::Loop)
    }
}

/// Where the song ends: at the end of its last clip. `None` without clips.
pub fn song_end(project: &Project) -> Option<Beats> {
    let clips = project.tracks.iter().flat_map(|track| &track.clips);
    clips.map(|clip| clip.end()).max()
}

/// The files an export to `file` writes: `file` for the mix, if the
/// settings want it, with the format's extension unless it has it; and next
/// to it, if the settings want stems, a file for each of `tracks`, named
/// after `file`, the track's number and its name: `Song 03 Bass.wav`.
pub fn files_for(
    file: &Path,
    project: &Project,
    settings: &ExportSettings,
    tracks: &BTreeSet<TrackId>,
) -> (Option<PathBuf>, Vec<(TrackId, PathBuf)>) {
    let file = with_extension(file, settings.format);
    let stem = file
        .file_stem()
        .map_or("Export".into(), |stem| stem.to_string_lossy().into_owned());
    let extension = settings.format.extensions()[0];
    let width = project.tracks.len().to_string().len().max(2);
    let stems = match settings.stems {
        true => project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| tracks.contains(&track.id))
            .map(|(i, track)| {
                let name = format!(
                    "{stem} {:0width$} {}.{extension}",
                    i + 1,
                    file_name(&track.name)
                );
                (track.id, file.with_file_name(name))
            })
            .collect(),
        false => Vec::new(),
    };
    let mixdown = settings.mixdown.then_some(file);
    (mixdown, stems)
}

/// `file`, with `format`'s extension added unless it has one of them.
fn with_extension(file: &Path, format: FileFormat) -> PathBuf {
    let has = file
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            format
                .extensions()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        });
    if has {
        return file.to_owned();
    }
    let mut file = file.as_os_str().to_owned();
    file.push(".");
    file.push(format.extensions()[0]);
    file.into()
}

/// A track's name as part of a file's name: without the characters file
/// names can't have on Windows, or at their end.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim_end();
    match cleaned {
        "" => "Track".to_owned(),
        name => name.to_owned(),
    }
}

/// One export: what to render, and the files to render it to.
#[derive(Clone)]
pub struct Job {
    /// The project as it's saved, plugins' states and all.
    pub project: Project,
    /// The folder of the project's file, which audio clips' paths start
    /// from.
    pub folder: Option<PathBuf>,
    pub range: BeatRange,
    pub settings: ExportSettings,
    /// The app engine's config. The files are at its sample rate unless
    /// the settings say otherwise, and plugins get its block size.
    pub config: EngineConfig,
    /// The plugins installed, to load the project's from.
    pub installed: Vec<PluginInfo>,
    /// Called, from any thread, when a plugin needs [`Export::tick`].
    pub waker: Option<Waker>,
    /// The mix's file, if the mix is wanted.
    pub mixdown: Option<PathBuf>,
    /// The tracks wanted on their own, and their files.
    pub stems: Vec<(TrackId, PathBuf)>,
}

impl Job {
    /// The files' sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.settings.sample_rate.unwrap_or(self.config.sample_rate)
    }

    /// How many frames the range takes at the files' sample rate.
    pub fn length(&self) -> u64 {
        let rate = self.sample_rate();
        let start = self.project.tempo.frame_at(self.range.start, rate);
        let end = self.project.tempo.frame_at(self.range.end, rate);
        end.saturating_sub(start).max(0) as u64
    }
}

/// How an export went.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The files written, in the order they were.
    pub files: Vec<Written>,
    /// What went wrong without stopping the export, such as an audio file
    /// or a plugin that's missing.
    pub problems: Vec<String>,
    /// Why the export stopped before it finished, if it did.
    pub failed: Option<String>,
    pub cancelled: bool,
    pub took: Duration,
}

/// A file an export wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub path: PathBuf,
    /// The loudest sample, before normalising: 1.0 is full scale.
    pub peak: f32,
    /// The gain normalising applied, if it did.
    pub gain: Option<f32>,
    /// The samples over full scale, which integer samples clip at. None
    /// after normalising.
    pub over: u64,
    pub frames: u64,
}

/// What an export is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Loading the plugins and compiling the graph for pass `pass` of
    /// `passes`, counting from 1.
    Preparing {
        pass: usize,
        passes: usize,
    },
    /// Rendering pass `pass`: `done` frames of the range's `length`, then
    /// the tail.
    Rendering {
        pass: usize,
        passes: usize,
        done: u64,
        length: u64,
        tail: bool,
        /// How many times faster than real time, so far.
        speed: f64,
    },
    /// Normalising pass `pass`'s files: `done` of `files`.
    Normalising {
        pass: usize,
        passes: usize,
        done: usize,
        files: usize,
    },
    Finished,
}

impl Stage {
    /// How far along the export is, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        let of = |pass: usize, passes: usize, within: f32| {
            ((pass - 1) as f32 + within.clamp(0.0, 1.0)) / passes.max(1) as f32
        };
        match *self {
            Self::Preparing { pass, passes } => of(pass, passes, 0.0),
            Self::Rendering {
                pass,
                passes,
                done,
                length,
                ..
            } => of(pass, passes, done as f32 / length.max(1) as f32 * 0.95),
            Self::Normalising {
                pass,
                passes,
                done,
                files,
            } => of(
                pass,
                passes,
                0.95 + 0.05 * done as f32 / files.max(1) as f32,
            ),
            Self::Finished => 1.0,
        }
    }
}

/// An export under way. [`start`](Self::start) it, then call
/// [`tick`](Self::tick) often, such as every UI frame, until it's
/// [finished](Self::is_finished). Dropping it cancels it.
pub struct Export {
    job: Job,
    passes: VecDeque<Pass>,
    total: usize,
    running: Option<Running>,
    report: Report,
    started: Instant,
    finished: bool,
}

/// What one pass renders: the mix, the stems, or both.
struct Pass {
    project: Project,
    mixdown: Option<PathBuf>,
    stems: Vec<(TrackId, PathBuf)>,
}

/// A pass under way: its session, and the thread rendering it. Dropping it
/// stops the thread, and its engine with it, before the session and its
/// plugins go.
struct Running {
    number: usize,
    progress: Arc<Progress>,
    /// The render thread, once the session has sent the engine everything.
    thread: Option<JoinHandle<Result<Vec<Written>, String>>>,
    /// The engine, until the thread takes it.
    engine: Option<Engine>,
    files: Files,
    session: Session,
}

impl Running {
    /// Starts the thread that renders the pass, with the engine.
    fn spawn(&mut self) -> io::Result<()> {
        let engine = self.engine.take().expect("the engine waits for the thread");
        let files = self.files.clone();
        let progress = self.progress.clone();
        let thread = thread::Builder::new()
            .name("musicable-export".into())
            .spawn(move || render(engine, &files, &progress))?;
        self.thread = Some(thread);
        Ok(())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.progress.cancel.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.engine = None;
    }
}

/// What a render thread says of how it's doing.
#[derive(Debug, Default)]
struct Progress {
    cancel: AtomicBool,
    stage: AtomicU8,
    /// Frames of the range rendered.
    done: AtomicU64,
    /// Files normalised.
    normalised: AtomicUsize,
    /// Microseconds spent rendering.
    took: AtomicU64,
}

const RENDERING: u8 = 1;
const TAIL: u8 = 2;
const NORMALISING: u8 = 3;

/// What a render thread writes, and how.
#[derive(Debug, Clone)]
struct Files {
    mixdown: Option<PathBuf>,
    stems: Vec<PathBuf>,
    format: Format,
    sample_format: SampleFormat,
    sample_rate: u32,
    dither: bool,
    normalize: bool,
    tail: bool,
    length: u64,
}

impl Export {
    /// Starts exporting. Fails if there's nothing to export: no files, or an
    /// empty range.
    pub fn start(job: Job) -> Result<Self, String> {
        if job.mixdown.is_none() && job.stems.is_empty() {
            return Err("there's nothing to export: choose the mix or some tracks".into());
        }
        if job.length() == 0 {
            return Err("there's nothing to export: the range is empty".into());
        }
        let prepared = |unsoloed: bool| {
            let mut project = job.project.clone();
            project.metronome = false;
            project.looping = false;
            project.punch_in = false;
            project.punch_out = false;
            if unsoloed {
                for track in &mut project.tracks {
                    track.solo = false;
                }
            }
            project
        };
        let soloed = job.project.tracks.iter().any(|track| track.solo);
        let mut passes = VecDeque::new();
        if job.mixdown.is_some() && !job.stems.is_empty() && soloed {
            passes.push_back(Pass {
                project: prepared(false),
                mixdown: job.mixdown.clone(),
                stems: Vec::new(),
            });
            passes.push_back(Pass {
                project: prepared(true),
                mixdown: None,
                stems: job.stems.clone(),
            });
        } else {
            passes.push_back(Pass {
                project: prepared(!job.stems.is_empty()),
                mixdown: job.mixdown.clone(),
                stems: job.stems.clone(),
            });
        }
        Ok(Self {
            total: passes.len(),
            job,
            passes,
            running: None,
            report: Report::default(),
            started: Instant::now(),
            finished: false,
        })
    }

    /// Carries the export on: starts its passes, services their plugins,
    /// and takes in what they rendered.
    pub fn tick(&mut self) {
        while !self.finished {
            let Some(running) = &mut self.running else {
                match self.passes.pop_front() {
                    Some(pass) => self.running = Some(self.begin(pass)),
                    None => self.finish(),
                }
                continue;
            };
            running.session.tick();
            if running.thread.is_none() {
                if !running.session.is_caught_up() {
                    return;
                }
                if let Err(error) = running.spawn() {
                    self.fail(format!("couldn't start rendering: {error}"));
                }
                return;
            }
            if !running.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                return;
            }
            let mut running = self.running.take().expect("it's running");
            let result = running.thread.take().expect("it finished").join();
            for problem in running.session.take_problems() {
                if !self.report.problems.contains(&problem) {
                    self.report.problems.push(problem);
                }
            }
            drop(running);
            match result {
                Ok(Ok(files)) => self.report.files.extend(files),
                Ok(Err(error)) if error == CANCELLED => self.cancelled(),
                Ok(Err(error)) => self.fail(error),
                Err(_) => self.fail("rendering stopped with an error".into()),
            }
        }
    }

    /// Stops the export, as soon as the pass under way stops. The files
    /// already finished stay; the ones it was writing go.
    pub fn cancel(&mut self) {
        if self.finished {
            return;
        }
        self.running = None;
        self.cancelled();
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// How it went, once it's finished.
    pub fn report(&self) -> Option<&Report> {
        self.finished.then_some(&self.report)
    }

    /// What it's doing now.
    pub fn stage(&self) -> Stage {
        let Some(running) = &self.running else {
            return match self.finished {
                true => Stage::Finished,
                false => Stage::Preparing {
                    pass: self.total - self.passes.len() + 1,
                    passes: self.total,
                },
            };
        };
        let (pass, passes) = (running.number, self.total);
        let progress = &running.progress;
        match progress.stage.load(Relaxed) {
            RENDERING | TAIL => {
                let done = progress.done.load(Relaxed);
                let took = progress.took.load(Relaxed) as f64 / 1e6;
                let seconds = done as f64 / f64::from(running.files.sample_rate);
                Stage::Rendering {
                    pass,
                    passes,
                    done,
                    length: running.files.length,
                    tail: progress.stage.load(Relaxed) == TAIL,
                    speed: if took > 0.0 { seconds / took } else { 0.0 },
                }
            }
            NORMALISING => Stage::Normalising {
                pass,
                passes,
                done: progress.normalised.load(Relaxed),
                files: running.files.mixdown.iter().count() + running.files.stems.len(),
            },
            _ => Stage::Preparing { pass, passes },
        }
    }

    /// The job it's doing.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Sets a pass up: its session, playing the range on an engine of its
    /// own, for the thread that renders it to take once the session has
    /// sent the engine everything.
    fn begin(&self, pass: Pass) -> Running {
        let job = &self.job;
        let config = EngineConfig {
            sample_rate: job.sample_rate(),
            max_block: job.config.max_block,
        };
        let options = ParallelOptions {
            realtime: false,
            ..ParallelOptions::default()
        };
        let renderer = ParallelRenderer::new(options);
        let (engine, handle) = Engine::with_renderer(config, Box::new(renderer));
        let stems: Vec<TrackId> = pass.stems.iter().map(|&(track, _)| track).collect();
        let installed = job.installed.clone();
        let folder = job.folder.as_deref();
        let mut session = Session::exporting(pass.project, folder, handle, installed, stems);
        if let Some(waker) = &job.waker {
            session.set_plugin_waker(waker.clone());
        }
        let at = Args::new().with("at", job.range.start);
        session
            .run_with(&transport::LOCATE, &at)
            .expect("the app can locate");
        session.run(&transport::PLAY).expect("the app can play");
        let length = job.length();
        session.stop_at(length);
        let files = Files {
            mixdown: pass.mixdown,
            stems: pass.stems.into_iter().map(|(_, path)| path).collect(),
            format: job.settings.format.format(),
            sample_format: job.settings.sample_format(),
            sample_rate: config.sample_rate,
            dither: job.settings.dither,
            normalize: job.settings.normalize,
            tail: job.settings.tail,
            length,
        };
        Running {
            number: self.total - self.passes.len(),
            progress: Arc::new(Progress::default()),
            thread: None,
            engine: Some(engine),
            files,
            session,
        }
    }

    fn fail(&mut self, error: String) {
        self.running = None;
        self.report.failed = Some(error);
        self.finish();
    }

    fn cancelled(&mut self) {
        self.report.cancelled = true;
        self.finish();
    }

    fn finish(&mut self) {
        self.passes.clear();
        self.report.took = self.started.elapsed();
        self.finished = true;
    }
}

/// What a render thread returns when it was cancelled.
const CANCELLED: &str = "cancelled";

/// Renders a pass to its files, on the render thread: from the engine's
/// first frame, which the session has had play from the start of the
/// range, to the end of the range, or the tail's. Returns what it wrote,
/// or why it stopped: [`CANCELLED`] if it was told to.
fn render(mut engine: Engine, files: &Files, progress: &Progress) -> Result<Vec<Written>, String> {
    let mut sinks = Vec::new();
    let paths = files.mixdown.iter().chain(&files.stems);
    for path in paths {
        match Sink::create(path, files) {
            Ok(sink) => sinks.push(sink),
            Err(error) => {
                remove_parts(&mut sinks);
                let _ = fs::remove_file(part(path));
                let _ = fs::remove_file(float_part(path));
                return Err(format!("couldn't write {}: {error}", path.display()));
            }
        }
    }
    let rendered = render_into(&mut engine, files, progress, &mut sinks);
    // The engine goes before the session and its plugins.
    drop(engine);
    let result = rendered.and_then(|()| finish(files, progress, &mut sinks));
    if result.is_err() {
        remove_parts(&mut sinks);
    }
    result
}

/// Renders into `sinks`: the mix's first if it's wanted, then the stems'.
fn render_into(
    engine: &mut Engine,
    files: &Files,
    progress: &Progress,
    sinks: &mut [Sink],
) -> Result<(), String> {
    let started = Instant::now();
    let rate = u64::from(files.sample_rate);
    let quiet_for = QUIET_FOR.as_secs() * rate;
    let max_tail = MAX_TAIL.as_secs() * rate;
    let mut output = vec![0.0; CHANNELS * CHUNK];
    let mut taps = vec![vec![0.0; CHANNELS * CHUNK]; files.stems.len()];
    let mut interleaved = Vec::with_capacity(CHANNELS * CHUNK);
    // Takes in the graphs and the transport the session sent, and with them
    // the latency to skip.
    engine.process_with_taps(&mut AudioMut::from_planar(&mut [], CHANNELS), &mut []);
    let mut skip = engine.stats().latency;
    let mut done = 0;
    // The tail since its last sound, held back by sink, interleaved, until
    // it's known whether more sound comes; how long that is; and how long
    // the tail is so far.
    let mut held: Vec<Vec<f32>> = vec![Vec::new(); sinks.len()];
    let (mut quiet, mut tail) = (0, 0);
    progress.stage.store(RENDERING, Relaxed);
    loop {
        if progress.cancel.load(Relaxed) {
            return Err(CANCELLED.into());
        }
        let left = files.length - done;
        if left == 0 && (!files.tail || quiet >= quiet_for || tail >= max_tail) {
            return Ok(());
        }
        let frames = match left {
            0 => CHUNK,
            left => (left + skip).min(CHUNK as u64) as usize,
        };
        let mut views: Vec<AudioMut<'_>> = taps
            .iter_mut()
            .map(|tap| AudioMut::from_planar(&mut tap[..CHANNELS * frames], CHANNELS))
            .collect();
        let mut mix = AudioMut::from_planar(&mut output[..CHANNELS * frames], CHANNELS);
        engine.process_with_taps(&mut mix, &mut views);
        drop(views);
        let took = started.elapsed().as_micros();
        progress
            .took
            .store(u64::try_from(took).unwrap_or(u64::MAX), Relaxed);
        let skipped = skip.min(frames as u64) as usize;
        skip -= skipped as u64;
        let fresh = skipped..frames;
        let mixdown = files
            .mixdown
            .is_some()
            .then_some(&output[..CHANNELS * frames]);
        let stems = taps.iter().map(|tap| &tap[..CHANNELS * frames]);
        let streams: Vec<&[f32]> = mixdown.into_iter().chain(stems).collect();
        if left > 0 {
            for (sink, stream) in sinks.iter_mut().zip(&streams) {
                interleave(stream, frames, fresh.clone(), &mut interleaved);
                sink.write(&interleaved)?;
            }
            done += fresh.len() as u64;
            progress.done.store(done, Relaxed);
            if done == files.length && files.tail {
                progress.stage.store(TAIL, Relaxed);
            }
            continue;
        }
        let loud = |i: usize| {
            let sample = |stream: &&[f32], c: usize| stream[c * frames + i].abs();
            streams
                .iter()
                .any(|stream| (0..CHANNELS).any(|c| sample(stream, c) >= QUIET))
        };
        let last_loud = fresh.clone().rev().find(|&i| loud(i));
        for ((sink, stream), held) in sinks.iter_mut().zip(&streams).zip(&mut held) {
            let quiet_from = match last_loud {
                // What was held back wasn't the end after all.
                Some(last) => {
                    sink.write(held)?;
                    held.clear();
                    interleave(stream, frames, fresh.start..last + 1, &mut interleaved);
                    sink.write(&interleaved)?;
                    last + 1
                }
                None => fresh.start,
            };
            interleave(stream, frames, quiet_from..fresh.end, &mut interleaved);
            held.extend_from_slice(&interleaved);
        }
        quiet = match last_loud {
            Some(last) => (fresh.end - last - 1) as u64,
            None => quiet + fresh.len() as u64,
        };
        tail += fresh.len() as u64;
    }
}

/// Writes `range` of the frames of planar `stream`, `frames` long, into
/// `out`, interleaved.
fn interleave(stream: &[f32], frames: usize, range: Range<usize>, out: &mut Vec<f32>) {
    out.clear();
    for i in range {
        out.extend((0..CHANNELS).map(|c| stream[c * frames + i]));
    }
}

/// Finishes the sinks' files, normalising them if the files are to be, and
/// gives them their names.
fn finish(files: &Files, progress: &Progress, sinks: &mut [Sink]) -> Result<Vec<Written>, String> {
    for sink in sinks.iter_mut() {
        let writer = sink.writer.take().expect("written to the end");
        writer.finish().map_err(|e| sink.error(e))?;
    }
    if files.normalize {
        progress.stage.store(NORMALISING, Relaxed);
        for sink in sinks.iter_mut() {
            if progress.cancel.load(Relaxed) {
                return Err(CANCELLED.into());
            }
            normalise(sink, files)
                .map_err(|e| format!("couldn't normalise {}: {e}", sink.path.display()))?;
            progress.normalised.fetch_add(1, Relaxed);
        }
    }
    let mut written = Vec::new();
    for sink in sinks.iter() {
        fs::rename(part(&sink.path), &sink.path).map_err(|e| sink.error(e))?;
        written.push(Written {
            path: sink.path.clone(),
            peak: sink.peak,
            gain: files.normalize.then(|| gain(sink.peak)),
            over: if files.normalize { 0 } else { sink.over },
            frames: sink.frames,
        });
    }
    Ok(written)
}

/// The gain that takes `peak` to full scale.
fn gain(peak: f32) -> f32 {
    if peak > 0.0 { 1.0 / peak } else { 1.0 }
}

/// Writes the sink's file from its float file, at the gain that takes its
/// peak to full scale, and removes the float file.
fn normalise(sink: &Sink, files: &Files) -> Result<(), String> {
    let float = float_part(&sink.path);
    let mut decoder = Decoder::open(&float).map_err(|e| e.to_string())?;
    let mut writer = files.writer(&part(&sink.path)).map_err(|e| e.to_string())?;
    let gain = gain(sink.peak);
    let mut buffer = vec![0.0; CHANNELS * CHUNK];
    let mut at = 0;
    loop {
        let read = decoder.read(at, &mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        let samples = &mut buffer[..read * CHANNELS];
        for sample in samples.iter_mut() {
            *sample *= gain;
        }
        writer.write(samples).map_err(|e| e.to_string())?;
        at += read as u64;
    }
    writer.finish().map_err(|e| e.to_string())?;
    drop(decoder);
    fs::remove_file(&float).map_err(|e| e.to_string())
}

impl Files {
    /// A writer for the files, at `path`.
    fn writer(
        &self,
        path: &Path,
    ) -> Result<Writer<BufWriter<File>>, musicable_io::audio::AudioError> {
        let out = BufWriter::with_capacity(1 << 16, File::create(path)?);
        let channels = CHANNELS as u16;
        let writer = Writer::new(
            out,
            self.format,
            self.sample_format,
            self.sample_rate,
            channels,
        )?;
        Ok(if self.dither {
            writer.with_dither()
        } else {
            writer
        })
    }
}

/// Where one stream of a pass goes, and what its samples were like.
struct Sink {
    /// The file, as it's named once it's whole.
    path: PathBuf,
    /// What's being written: the file, or for normalising, the float file
    /// it's made from.
    writer: Option<Writer<BufWriter<File>>>,
    peak: f32,
    over: u64,
    frames: u64,
}

impl Sink {
    fn create(path: &Path, files: &Files) -> Result<Self, musicable_io::audio::AudioError> {
        let writer = if files.normalize {
            let out = BufWriter::with_capacity(1 << 16, File::create(float_part(path))?);
            let channels = CHANNELS as u16;
            Writer::new(
                out,
                Format::Wav,
                SampleFormat::Float32,
                files.sample_rate,
                channels,
            )?
        } else {
            files.writer(&part(path))?
        };
        Ok(Self {
            path: path.to_owned(),
            writer: Some(writer),
            peak: 0.0,
            over: 0,
            frames: 0,
        })
    }

    /// Writes interleaved samples.
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        for &sample in samples {
            let level = sample.abs();
            self.peak = self.peak.max(level);
            self.over += u64::from(level > 1.0);
        }
        self.frames += (samples.len() / CHANNELS) as u64;
        let writer = self.writer.as_mut().expect("written until finished");
        writer.write(samples).map_err(|e| self.error(e))
    }

    fn error(&self, error: io::Error) -> String {
        format!("couldn't write {}: {error}", self.path.display())
    }
}

/// Where a file is written until it's whole.
fn part(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    part.into()
}

/// Where a file to normalise renders to first.
fn float_part(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".float.part");
    part.into()
}

/// Closes the files the sinks were writing, and removes them.
fn remove_parts(sinks: &mut [Sink]) {
    for sink in sinks {
        sink.writer = None;
        let _ = fs::remove_file(part(&sink.path));
        let _ = fs::remove_file(float_part(&sink.path));
    }
}

#[cfg(test)]
mod tests {
    use musicable_commands::editor::{Selection, TimeSelection};
    use musicable_model::TrackKind;
    use musicable_model::clip::{ClipContent, MidiClip};

    use super::*;

    #[test]
    fn settings_are_kept_as_the_settings_file_has_them() {
        let settings = ExportSettings {
            format: FileFormat::Flac,
            bits: Bits::Int16,
            sample_rate: Some(96_000),
            normalize: true,
            stems: true,
            ..ExportSettings::default()
        };
        let text = toml::to_string(&settings).unwrap();
        assert!(text.contains("format = \"flac\"\nbits = 16\n"), "{text}");
        assert_eq!(toml::from_str::<ExportSettings>(&text).unwrap(), settings);
        let default = toml::to_string(&ExportSettings::default()).unwrap();
        assert!(!default.contains("sample_rate"), "{default}");
        assert_eq!(
            toml::from_str::<ExportSettings>("").unwrap(),
            ExportSettings::default()
        );
        let error = toml::from_str::<ExportSettings>("bits = 20").unwrap_err();
        assert!(error.to_string().contains("not 20-bit"), "{error}");
    }

    #[test]
    fn flac_has_no_floats_so_it_gets_24_bits() {
        let settings = |format, bits| ExportSettings {
            format,
            bits,
            ..ExportSettings::default()
        };
        let float = settings(FileFormat::Wav, Bits::Float32);
        assert_eq!(float.sample_format(), SampleFormat::Float32);
        let flac = settings(FileFormat::Flac, Bits::Float32);
        assert_eq!(flac.sample_format(), SampleFormat::Int24);
        let aiff = settings(FileFormat::Aiff, Bits::Int16);
        assert_eq!(aiff.sample_format(), SampleFormat::Int16);
    }

    fn song() -> Project {
        let mut project = Project::new();
        for name in ["Drums", "Bass: DI/Amp?", "  ", "Pad."] {
            project.add_track(name, TrackKind::Midi);
        }
        project
    }

    #[test]
    fn files_are_named_after_the_file_and_the_tracks() {
        let project = song();
        let ids: Vec<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        let settings = ExportSettings {
            stems: true,
            ..ExportSettings::default()
        };
        let all = ids.iter().copied().collect();
        let (mixdown, stems) = files_for(Path::new("out/Song"), &project, &settings, &all);
        assert_eq!(mixdown, Some(PathBuf::from("out/Song.wav")));
        let names: Vec<_> = stems.iter().map(|(_, path)| path.clone()).collect();
        assert_eq!(
            names,
            [
                "out/Song 01 Drums.wav",
                "out/Song 02 Bass_ DI_Amp_.wav",
                "out/Song 03 Track.wav",
                "out/Song 04 Pad.wav",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(stems.iter().map(|&(id, _)| id).collect::<Vec<_>>(), ids);

        // The extension stays if it's the format's, in any case, and only
        // the tracks asked for get files.
        let settings = ExportSettings {
            format: FileFormat::Aiff,
            mixdown: false,
            ..settings
        };
        let bass = [ids[1]].into_iter().collect();
        let (mixdown, stems) = files_for(Path::new("Song.AIF"), &project, &settings, &bass);
        assert_eq!(mixdown, None);
        let expected = (ids[1], PathBuf::from("Song 02 Bass_ DI_Amp_.aif"));
        assert_eq!(stems, [expected]);
        let flac = ExportSettings {
            format: FileFormat::Flac,
            ..ExportSettings::default()
        };
        let (mixdown, stems) = files_for(Path::new("Song.wav"), &project, &flac, &bass);
        assert_eq!(mixdown, Some(PathBuf::from("Song.wav.flac")));
        assert!(stems.is_empty(), "the settings want no stems");
    }

    #[test]
    fn spans_are_the_song_the_loop_or_the_selection() {
        let mut project = song();
        let mut editor = Editor::default();
        assert_eq!(song_end(&project), None);
        assert_eq!(Span::Song.range(&project, &editor), None);
        assert_eq!(Span::default_for(&project, &editor), Span::Loop);
        let at = |beats: i64| Beats::new(beats);
        let clip = project.new_clip(
            "",
            BeatRange::new(at(4), at(10)),
            ClipContent::Midi(MidiClip::default()),
        );
        let id = clip.id;
        project.tracks[2].clips.push(clip);
        let song = Some(BeatRange::new(Beats::ZERO, at(10)));
        assert_eq!(Span::Song.range(&project, &editor), song);
        assert_eq!(Span::default_for(&project, &editor), Span::Song);
        assert_eq!(
            Span::Loop.range(&project, &editor),
            Some(project.loop_region)
        );

        editor.selection = Selection::clips([id]);
        let clip = Some(BeatRange::new(at(4), at(10)));
        assert_eq!(Span::Selection.range(&project, &editor), clip);
        assert_eq!(Span::default_for(&project, &editor), Span::Selection);
        editor.selection.time = Some(TimeSelection {
            range: BeatRange::new(at(1), at(2)),
            tracks: BTreeSet::new(),
        });
        let time = Some(BeatRange::new(at(1), at(2)));
        assert_eq!(Span::Selection.range(&project, &editor), time);
    }

    #[test]
    fn the_stage_says_how_far_along_an_export_is() {
        let rendering = Stage::Rendering {
            pass: 2,
            passes: 2,
            done: 50,
            length: 100,
            tail: false,
            speed: 10.0,
        };
        assert!((rendering.fraction() - 0.7375).abs() < 1e-6);
        let preparing = Stage::Preparing { pass: 1, passes: 2 };
        assert_eq!(preparing.fraction(), 0.0);
        let normalising = Stage::Normalising {
            pass: 1,
            passes: 1,
            done: 1,
            files: 2,
        };
        assert!((normalising.fraction() - 0.975).abs() < 1e-6);
        assert_eq!(Stage::Finished.fraction(), 1.0);
    }
}

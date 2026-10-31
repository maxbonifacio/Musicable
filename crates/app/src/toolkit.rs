//! The audio toolkit: commands that cut, convert and measure audio files,
//! for agents and for people at the command line, with no window, audio
//! device or project. Each is written once, here, and reached two ways:
//! `musicable audio <command>` on the command line ([`cli`]), and a tool of
//! its own in `musicable mcp`, which the bridge runs in its own process
//! ([`Tools`]), so it works while Musicable is closed.
//!
//! A command takes [`Args`], checked against its parameters as project
//! commands' are, and gives back a JSON object, with the unit in each
//! field's name, `null` for silence, and the files it wrote described as
//! `audio.info` describes them. It never writes over a file unless it's
//! given `overwrite`, never writes to a file it reads, and writes
//! `NAME.part` first, renaming it once it's whole, so a file that's there
//! is whole.
//!
//! `docs/agents.md` describes the tools, and the manual's Audio commands
//! page every command, from what's here.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use musicable_analysis::loudness::{Levels, Meter};
use musicable_commands::{Args, CommandError, FromValue, Param, Type, json as args_json};
use musicable_io::audio::{
    AudioError, Decoder, Encoding, Format, Info, SampleFormat, SamplesMd5, Writer,
};
use musicable_mcp::local::{LocalTool, LocalTools};
use serde_json::{Map, Value as Json, json};

mod cli;
mod compare;
mod files;
mod levels;
#[cfg(test)]
mod manual;
mod resample;
mod schema;
#[cfg(test)]
mod tests;

pub use cli::run as command_line;

/// How many frames a command reads or writes at a time.
const CHUNK: usize = 65_536;
/// The most files a command takes at once, and the most items a list in
/// its result holds.
const MOST: usize = 200;
/// What a command cancelled by the agent says.
const CANCELLED: &str = "Cancelled: nothing was written.";

/// An audio command.
pub struct AudioCommand {
    /// Its id, such as `audio.cut`: `musicable audio cut` on the command
    /// line, and the tool `audio_cut`.
    pub id: &'static str,
    pub title: &'static str,
    /// What it does, for `--help`, its tool's description and the manual.
    pub about: &'static str,
    pub params: Vec<Param>,
    /// The parameters the command line takes without their names, before
    /// the options, in order: the files in `musicable audio info a.wav
    /// b.wav`. A list takes every file there, so it's the only one.
    pub positional: &'static [&'static str],
    /// Whether it only reads files, writing none.
    pub read_only: bool,
    /// An example of its arguments on the command line, after its name, on
    /// files the tests make, which the tests run and the manual shows.
    pub example: &'static [&'static str],
    run: fn(&Args, &Job) -> Result<Json, String>,
    /// The JSON Schema of what it gives back.
    output: fn() -> Json,
}

impl AudioCommand {
    /// Its name on the command line, such as `cut`.
    pub fn name(&self) -> &'static str {
        self.id.trim_start_matches("audio.")
    }

    /// Its tool's name over MCP, such as `audio_cut`: tools' names can't
    /// hold dots.
    pub fn tool(&self) -> String {
        self.id.replace('.', "_")
    }

    /// The JSON Schema of its arguments, as an agent gives them.
    pub fn input_schema(&self) -> Map<String, Json> {
        args_json::schema(&self.params)
    }

    /// The JSON Schema of what it gives back.
    pub fn output_schema(&self) -> Map<String, Json> {
        match (self.output)() {
            Json::Object(schema) => schema,
            _ => unreachable!("each output schema is an object"),
        }
    }

    /// Runs it with `args`, checked against its parameters.
    pub fn run(&self, args: &Args, job: &Job) -> Result<Json, String> {
        args.check(&self.params).map_err(|error| sentence(&error))?;
        (self.run)(args, job)
    }
}

/// Every audio command, in order of id.
pub fn commands() -> Vec<AudioCommand> {
    let mut commands = files::commands();
    commands.extend(levels::commands());
    commands.extend(compare::commands());
    commands.sort_by_key(|command| command.id);
    commands
}

/// The audio commands as `musicable api` lists them: each one's id, title,
/// tool, and the JSON Schema of its arguments and of what it gives back.
pub fn catalogue() -> Json {
    let commands: Vec<Json> = (commands().iter())
        .map(|command| {
            json!({
                "id": command.id,
                "title": command.title,
                "tool": command.tool(),
                "input_schema": command.input_schema(),
                "output_schema": command.output_schema(),
            })
        })
        .collect();
    Json::Array(commands)
}

/// What a command runs with, besides its arguments.
#[derive(Clone, Copy)]
pub struct Job<'a> {
    /// The folder relative paths are in: the command line's working
    /// folder. Agents' calls have none, so their paths must be whole.
    pub folder: Option<&'a Path>,
    /// Whether to stop, as when the agent gives up on the call.
    pub cancelled: &'a dyn Fn() -> bool,
    /// How much is done, from 0 to 1.
    pub progress: &'a dyn Fn(f64),
}

fn never() -> bool {
    false
}

fn unheard(_done: f64) {}

impl<'a> Job<'a> {
    /// A job that never stops early and keeps its progress to itself, with
    /// relative paths in `folder`.
    pub fn quiet(folder: Option<&'a Path>) -> Self {
        Job {
            folder,
            cancelled: &never,
            progress: &unheard,
        }
    }

    /// The job for what's left once a file is written, which goes on to
    /// the end: cancelled then, it would leave the file written but not
    /// checked.
    fn to_the_end(self) -> Self {
        Job {
            cancelled: &never,
            ..self
        }
    }

    fn check(&self) -> Result<(), String> {
        match (self.cancelled)() {
            true => Err(CANCELLED.to_owned()),
            false => Ok(()),
        }
    }

    /// The file at `text`, a whole path or one in the job's folder.
    fn path(&self, text: &str) -> Result<PathBuf, String> {
        let path = Path::new(text);
        if text.trim().is_empty() {
            return Err("A file's path is empty.".to_owned());
        }
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        match self.folder {
            // Without the `.`s of `./a.wav`.
            Some(folder) => Ok(folder.join(path).components().collect()),
            None => Err(format!(
                "Give the whole path of {text}, from the top of the disk: the tools don't know \
                 which folder it's in."
            )),
        }
    }

    /// The file the argument `name` names.
    fn file(&self, args: &Args, name: &str) -> Result<PathBuf, String> {
        self.path(&arg::<String>(args, name)?)
    }

    /// The files the argument `name` lists, 200 at most.
    fn files(&self, args: &Args, name: &str) -> Result<Vec<PathBuf>, String> {
        let texts: Vec<String> = arg(args, name)?;
        if texts.is_empty() {
            return Err(format!("{name} lists no files."));
        }
        if texts.len() > MOST {
            return Err(format!(
                "{name} lists {} files, and a command takes {MOST} at most: give the rest in \
                 another call.",
                texts.len()
            ));
        }
        texts.iter().map(|text| self.path(text)).collect()
    }

    /// Reports that `share` of the work is `done`, from 0 to 1.
    fn report(&self, share: Share, done: f64) {
        (self.progress)(share.start + share.size * done.clamp(0.0, 1.0));
    }
}

/// A part of a command's work, for its progress: from `start`, `size` of
/// it, of the whole from 0 to 1.
#[derive(Debug, Clone, Copy)]
struct Share {
    start: f64,
    size: f64,
}

impl Share {
    const ALL: Share = Share {
        start: 0.0,
        size: 1.0,
    };

    /// The part of this share from `from` to `to` of it.
    fn between(self, from: f64, to: f64) -> Share {
        Share {
            start: self.start + self.size * from,
            size: self.size * (to - from),
        }
    }

    /// Part `index` of `parts` equal parts of this share.
    fn part(self, index: usize, parts: usize) -> Share {
        let parts = parts.max(1) as f64;
        self.between(index as f64 / parts, (index + 1) as f64 / parts)
    }
}

/// The argument `name`, which must be there.
fn arg<T: FromValue>(args: &Args, name: &str) -> Result<T, String> {
    args.get(name).map_err(|error| sentence(&error))
}

/// The argument `name`, if it's there.
fn opt<T: FromValue>(args: &Args, name: &str) -> Result<Option<T>, String> {
    args.opt(name).map_err(|error| sentence(&error))
}

/// The true-or-false argument `name`: false without it.
fn flag(args: &Args, name: &str) -> Result<bool, String> {
    Ok(opt(args, name)?.unwrap_or(false))
}

/// `error` as a sentence.
fn sentence(error: &CommandError) -> String {
    let mut text = crate::capital(&error.to_string());
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

/// A list of paths, as commands' parameters take files.
const PATHS: Type = Type::List(&Type::Text);

/// The file a command reads.
const FILE: Param = Param::required(
    "file",
    Type::Text,
    "The audio file to read, by its path: a WAV, AIFF or FLAC file.",
);

/// The files a command reads.
const FILES: Param = Param::required(
    "files",
    PATHS,
    "The audio files to read, by their paths: WAV, AIFF or FLAC files, 200 at most.",
);

/// The file a command writes.
const OUT: Param = Param::required(
    "out",
    Type::Text,
    "Where to write the new file, by its path. Its extension says the format: .wav, .aif or \
     .flac.",
);

const OVERWRITE: Param = Param::optional(
    "overwrite",
    Type::Bool,
    "Writes over a file at out. Without it, a file there stops the command.",
);

/// The parameters for a stretch of a file: from and to, in seconds or in
/// frames, and with `lengths`, length and length_frames.
fn stretch_params(lengths: bool) -> Vec<Param> {
    let to = match lengths {
        true => {
            "Where the stretch ends, in seconds from the start of the file. Without it, \
             to_frame or a length, it ends where the file does."
        }
        false => {
            "Where the stretch ends, in seconds from the start of the file. Without it or \
             to_frame, it ends where the file does."
        }
    };
    let mut params = vec![
        Param::optional(
            "from",
            Type::Number,
            "Where the stretch starts, in seconds from the start of the file. Without it or \
             from_frame, it starts where the file does.",
        ),
        Param::optional("to", Type::Number, to),
        Param::optional(
            "from_frame",
            Type::Int,
            "Where the stretch starts, in frames from the start of the file, instead of from.",
        ),
        Param::optional(
            "to_frame",
            Type::Int,
            "Where the stretch ends, in frames from the start of the file, instead of to.",
        ),
    ];
    if lengths {
        params.extend([
            Param::optional(
                "length",
                Type::Number,
                "How long the stretch is, in seconds, instead of where it ends.",
            ),
            Param::optional(
                "length_frames",
                Type::Int,
                "How long the stretch is, in frames, instead of where it ends.",
            ),
        ]);
    }
    params
}

/// `value` to `places` decimal places, and 0 rather than -0.
fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale + 0.0
}

/// A level in decibels, to 2 places, or `null` for silence.
fn decibels(value: Option<f64>) -> Json {
    value
        .filter(|value| value.is_finite())
        .map_or(Json::Null, |value| json!(round(value, 2)))
}

/// A linear level in decibels, to 2 places, or `null` for silence.
fn level(linear: f64) -> Json {
    decibels((linear > 0.0).then(|| 20.0 * linear.log10()))
}

/// `frames` at `rate` in seconds, to 6 places, which tells frames apart up
/// to 192 kHz.
fn seconds(frames: u64, rate: u32) -> Json {
    json!(round(frames as f64 / f64::from(rate), 6))
}

/// A frame of a file at `rate`, as results give positions.
fn position(frame: u64, rate: u32) -> Json {
    json!({"seconds": seconds(frame, rate), "frame": frame})
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Wav => "wav",
        Format::Aiff => "aiff",
        Format::Flac => "flac",
    }
}

/// The largest sample an encoding stores: a step below 1.0 for integers.
fn full_scale(encoding: Encoding) -> f64 {
    match encoding {
        Encoding::Int(bits) => 1.0 - 1.0 / f64::from(1u32 << (bits - 1)),
        Encoding::Float32 | Encoding::Float64 => 1.0,
    }
}

/// An audio file being read, in whole frames of 64-bit floats, which hold
/// its samples exactly.
struct Source {
    path: PathBuf,
    decoder: Decoder,
    info: Info,
    encoding: Encoding,
}

impl Source {
    fn open(path: &Path) -> Result<Self, String> {
        if path.is_dir() {
            return Err(format!(
                "{} is a folder, not an audio file.",
                path.display()
            ));
        }
        let decoder = Decoder::open(path).map_err(|error| read_error(path, &error))?;
        let info = decoder.info();
        if info.channels == 0 || info.sample_rate == 0 {
            return Err(format!(
                "Couldn't read {}: it has no channels, or no sample rate.",
                path.display()
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            info,
            encoding: decoder.encoding(),
            decoder,
        })
    }

    fn rate(&self) -> u32 {
        self.info.sample_rate
    }

    fn channels(&self) -> usize {
        self.info.channels
    }

    fn frames(&self) -> u64 {
        self.info.frames
    }

    /// Reads frames `from` to `to` in chunks, handing each to `each` and
    /// reporting progress through `share`, until they're done or the job
    /// is cancelled.
    fn each_chunk(
        &mut self,
        from: u64,
        to: u64,
        job: &Job,
        share: Share,
        mut each: impl FnMut(&[f64]) -> Result<(), String>,
    ) -> Result<(), String> {
        let channels = self.channels();
        let mut buffer = vec![0.0; CHUNK.min((to - from) as usize) * channels];
        let mut at = from;
        while at < to {
            job.check()?;
            let frames = ((to - at) as usize).min(CHUNK);
            let read = self.read(at, &mut buffer[..frames * channels])?;
            each(&buffer[..read * channels])?;
            at += read as u64;
            job.report(share, (at - from) as f64 / (to - from) as f64);
        }
        Ok(())
    }

    /// Reads the frames from `at` that fill `out`, or as many as there
    /// are, but at least one.
    fn read(&mut self, at: u64, out: &mut [f64]) -> Result<usize, String> {
        let read =
            (self.decoder.read_f64(at, out)).map_err(|error| read_error(&self.path, &error))?;
        match read {
            0 => Err(format!("{} ended early.", self.path.display())),
            read => Ok(read),
        }
    }

    /// Frames `from` to `to`, interleaved.
    fn read_frames(&mut self, from: u64, to: u64) -> Result<Vec<f64>, String> {
        let channels = self.channels();
        let frames = (to - from) as usize;
        let mut samples = vec![0.0; frames * channels];
        let mut read = 0;
        while read < frames {
            read += self.read(from + read as u64, &mut samples[read * channels..])?;
        }
        Ok(samples)
    }

    /// Measures frames `from` to `to` with `meter`.
    fn measure(
        &mut self,
        (from, to): (u64, u64),
        mut meter: Meter,
        job: &Job,
        share: Share,
    ) -> Result<Levels, String> {
        self.each_chunk(from, to, job, share, |samples| {
            meter.add(samples);
            Ok(())
        })?;
        Ok(meter.finish())
    }

    /// A meter for its samples, which counts its largest as clipped.
    fn meter(&self) -> Meter {
        Meter::new(self.rate(), self.channels()).with_full_scale(full_scale(self.encoding))
    }
}

/// What went wrong reading `path`, in a sentence.
fn read_error(path: &Path, error: &AudioError) -> String {
    match error {
        AudioError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            format!("There's no file at {}.", path.display())
        }
        AudioError::UnknownFormat => format!(
            "{} isn't a WAV, AIFF or FLAC file, which are the files Musicable reads.",
            path.display()
        ),
        error => format!("Couldn't read {}: {error}.", path.display()),
    }
}

/// Runs `each` on each of `paths`, in a share of the work each, and gives
/// back what it gives back for each, or the file and why it failed: one
/// file failing doesn't stop the others, but the job being cancelled does.
fn each_file(
    paths: &[PathBuf],
    job: &Job,
    mut each: impl FnMut(&Path, Share) -> Result<Json, String>,
) -> Result<Vec<Json>, String> {
    let mut results = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        match each(path, Share::ALL.part(i, paths.len())) {
            Ok(result) => results.push(result),
            Err(why) => {
                job.check()?;
                results.push(json!({"file": path.display().to_string(), "error": why}));
            }
        }
    }
    Ok(results)
}

/// A file as `audio.info` describes it, read through for its peak and the
/// MD5 of its samples.
fn describe(path: &Path, job: &Job, share: Share) -> Result<Json, String> {
    let mut source = Source::open(path)?;
    let mut md5 = SamplesMd5::new(source.encoding);
    let mut peak = 0.0f64;
    let frames = source.frames();
    source.each_chunk(0, frames, job, share, |samples| {
        md5.update(samples);
        peak = samples
            .iter()
            .fold(peak, |peak, sample| peak.max(sample.abs()));
        Ok(())
    })?;
    let tempo = source.decoder.tempo().map(|bpm| round(bpm, 4));
    Ok(json!({
        "file": path.display().to_string(),
        "format": format_name(source.info.format),
        "encoding": source.encoding.name(),
        "channels": source.channels(),
        "sample_rate": source.rate(),
        "frames": frames,
        "seconds": seconds(frames, source.rate()),
        "peak_dbfs": level(peak),
        "acid_bpm": tempo,
        "audio_md5": md5.finish(),
    }))
}

/// The frames from and to which `args` say, of `source`: from `from` or
/// `from_frame` to `to` or `to_frame`, or with `lengths`, for `length` or
/// `length_frames`; from the start to the end without them.
fn range(args: &Args, source: &Source, lengths: bool) -> Result<(u64, u64), String> {
    let rate = f64::from(source.rate());
    let frames = source.frames();
    let seconds = |name: &str| -> Result<Option<u64>, String> {
        match opt::<f64>(args, name)? {
            Some(value) if value < 0.0 => Err(format!("{name} is a number of seconds, 0 or more.")),
            Some(value) => Ok(Some((value * rate).round() as u64)),
            None => Ok(None),
        }
    };
    let frame = |name: &str| -> Result<Option<u64>, String> {
        match opt::<i64>(args, name)? {
            Some(value) => u64::try_from(value)
                .map(Some)
                .map_err(|_| format!("{name} is a number of frames, 0 or more.")),
            None => Ok(None),
        }
    };
    // The one of `names` given, by its place in them, if one is.
    let one = |names: &[&str], values: Vec<Option<u64>>| -> Result<Option<(usize, u64)>, String> {
        let given: Vec<(usize, u64)> = (values.into_iter().enumerate())
            .filter_map(|(i, value)| value.map(|value| (i, value)))
            .collect();
        match given.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(*only)),
            _ => Err(format!("Give one of {}, not more.", names.join(", "))),
        }
    };
    let start = one(
        &["from", "from_frame"],
        vec![seconds("from")?, frame("from_frame")?],
    )?;
    let end = match lengths {
        true => one(
            &["to", "to_frame", "length", "length_frames"],
            vec![
                seconds("to")?,
                frame("to_frame")?,
                seconds("length")?,
                frame("length_frames")?,
            ],
        )?,
        false => one(
            &["to", "to_frame"],
            vec![seconds("to")?, frame("to_frame")?],
        )?,
    };
    if start.is_none() && end.is_none() {
        return Ok((0, frames));
    }
    let start = start.map_or(0, |(_, frame)| frame);
    let end = match end {
        None => frames,
        Some((0 | 1, frame)) => frame,
        Some((_, length)) => start.saturating_add(length),
    };
    let long = || {
        format!(
            "{} is {} s long ({frames} frames)",
            source.path.display(),
            round(frames as f64 / rate, 6),
        )
    };
    if start >= frames {
        return Err(format!("{}, so it ends before frame {start}.", long()));
    }
    if end > frames {
        return Err(format!("{}, so it ends before frame {end}.", long()));
    }
    if end <= start {
        return Err(format!(
            "The stretch from frame {start} to frame {end} is empty: it must end after it starts."
        ));
    }
    Ok((start, end))
}

/// The narrowest sample format that holds every sample of each of
/// `encodings` exactly, in a `format` file.
fn holding(format: Format, encodings: &[Encoding]) -> Result<SampleFormat, String> {
    use SampleFormat::{Float32, Float64, Int8, Int16, Int24, Int32};
    let sample_format = ([Int8, Int16, Int24, Int32, Float32, Float64].into_iter())
        .find(|sample_format| encodings.iter().all(|e| sample_format.holds(*e)))
        .unwrap_or(Float64);
    if format == Format::Flac && !flac_holds(sample_format) {
        return Err(format!(
            "FLAC files hold integers of up to 24 bits, not {} samples: write a WAV or AIFF \
             file, or change the samples with audio.convert.",
            sample_format.name()
        ));
    }
    Ok(sample_format)
}

/// Whether FLAC files take samples of `sample_format`.
fn flac_holds(sample_format: SampleFormat) -> bool {
    !sample_format.is_float() && sample_format.bits() <= 24
}

/// The format a file at `path` is written in, by its extension.
fn format_of(path: &Path) -> Result<Format, String> {
    let extension = (path.extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase);
    match extension.as_deref() {
        Some("wav") => Ok(Format::Wav),
        Some("aif" | "aiff" | "aifc") => Ok(Format::Aiff),
        Some("flac") => Ok(Format::Flac),
        _ => Err(format!(
            "{} needs the extension .wav, .aif or .flac, which says what to write.",
            path.display()
        )),
    }
}

/// Whether `a` and `b` are the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// An audio file being written, as `NAME.part` until it's whole, so that a
/// file by its name is always whole. Dropped unfinished, it takes the part
/// away.
struct Output {
    path: PathBuf,
    part: PathBuf,
    writer: Option<Writer<BufWriter<File>>>,
    sample_format: SampleFormat,
    /// Samples written beyond full scale, which integers clip.
    over: u64,
}

impl Output {
    /// Checks that a command may write to `path`: that it has an extension
    /// it can write, that it isn't one of the files the command reads,
    /// `inputs`, and that there's no file there unless `overwrite` says to
    /// write over it.
    fn check(path: &Path, overwrite: bool, inputs: &[&Path]) -> Result<Format, String> {
        let format = format_of(path)?;
        if inputs
            .iter()
            .any(|input| *input == path || same_file(path, input))
        {
            return Err(format!(
                "{} is a file the command reads, which it never writes to: write somewhere \
                 else.",
                path.display()
            ));
        }
        if path.is_dir() {
            return Err(format!("{} is a folder.", path.display()));
        }
        if path.exists() && !overwrite {
            return Err(format!(
                "There's a file at {} already: set overwrite to write over it.",
                path.display()
            ));
        }
        Ok(format)
    }

    /// Starts writing a `format` file at `path` of `channels` channels of
    /// `sample_format` samples at `rate`, dithered if `dither`, once
    /// [`check`](Self::check) has said it may.
    fn create(
        path: &Path,
        format: Format,
        sample_format: SampleFormat,
        rate: u32,
        channels: usize,
        dither: bool,
    ) -> Result<Self, String> {
        let failed =
            |error: &dyn std::fmt::Display| format!("Couldn't write {}: {error}.", path.display());
        if let Some(folder) = path
            .parent()
            .filter(|folder| !folder.as_os_str().is_empty())
        {
            fs::create_dir_all(folder).map_err(|error| failed(&error))?;
        }
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let channels = u16::try_from(channels).map_err(|_| failed(&"too many channels"))?;
        let file = File::create(&part).map_err(|error| failed(&error))?;
        let mut output = Self {
            path: path.to_owned(),
            part,
            writer: None,
            sample_format,
            over: 0,
        };
        let writer = Writer::new(BufWriter::new(file), format, sample_format, rate, channels)
            .map_err(|error| failed(&error))?;
        output.writer = Some(match dither && !sample_format.is_float() {
            true => writer.with_dither(),
            false => writer,
        });
        Ok(output)
    }

    fn write(&mut self, samples: &[f64]) -> Result<(), String> {
        let writer = (self.writer.as_mut()).expect("an output writes until it's finished");
        let top = full_scale(self.sample_format.encoding());
        self.over += (samples.iter())
            .filter(|sample| **sample > top || **sample < -1.0)
            .count() as u64;
        (writer.write_f64(samples))
            .map_err(|error| format!("Couldn't write {}: {error}.", self.path.display()))
    }

    /// Writes `frames` frames of silence.
    fn silence(&mut self, frames: u64, channels: usize) -> Result<(), String> {
        let zeros = vec![0.0; CHUNK.min(frames as usize) * channels];
        let mut left = frames;
        while left > 0 {
            let now = left.min(CHUNK as u64) as usize;
            self.write(&zeros[..now * channels])?;
            left -= now as u64;
        }
        Ok(())
    }

    /// Finishes the file and renames it to its name, writing over a file
    /// there.
    fn finish(mut self) -> Result<PathBuf, String> {
        let writer = self.writer.take().expect("an output finishes once");
        let failed = |error: io::Error| format!("Couldn't write {}: {error}.", self.path.display());
        let result = (|| {
            let mut out = writer.finish()?;
            out.flush()?;
            let file = out.into_inner().map_err(|error| error.into_error())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&self.part, &self.path)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&self.part);
            return Err(failed(error));
        }
        Ok(self.path.clone())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // Unfinished: the part goes.
        if self.writer.take().is_some() {
            let _ = fs::remove_file(&self.part);
        }
    }
}

/// Whether frames `from` on of `a` are frames `b_from` on of `b`, sample
/// for sample, for `frames` frames, but for the frames in `skip`, which
/// changed on purpose, as fades do: `(start, end)` of `a`'s frames. A mono
/// `a` can be a `b` of more channels, each a copy of its one.
#[allow(clippy::too_many_arguments)]
fn same_frames(
    a: &mut Source,
    from: u64,
    b: &mut Source,
    b_from: u64,
    frames: u64,
    skip: &[(u64, u64)],
    job: &Job,
    share: Share,
) -> Result<bool, String> {
    let (channels, copies) = (a.channels(), b.channels());
    if (copies != channels && channels != 1) || b_from + frames > b.frames() {
        return Ok(false);
    }
    let mut theirs = vec![0.0; CHUNK.min(frames as usize) * copies];
    let mut at = 0;
    let mut same = true;
    a.each_chunk(from, from + frames, job, share, |ours| {
        let count = ours.len() / channels;
        let mut read = 0;
        while read < count {
            read += b.read(
                b_from + at + read as u64,
                &mut theirs[read * copies..count * copies],
            )?;
        }
        let pairs = ours.chunks_exact(channels).zip(theirs.chunks_exact(copies));
        for (i, (x, y)) in pairs.enumerate() {
            let frame = from + at + i as u64;
            let skipped = skip
                .iter()
                .any(|&(start, end)| (start..end).contains(&frame));
            let differs = (y.iter().enumerate())
                .any(|(channel, y)| x[channel % channels].to_bits() != y.to_bits());
            if !skipped && differs {
                same = false;
            }
        }
        at += count as u64;
        Ok(())
    })?;
    Ok(same)
}

/// The audio commands as tools the bridge runs itself.
pub struct Tools;

/// What agents are told about the audio tools.
const INSTRUCTIONS: &str = "\
The audio_ tools cut, convert and measure audio files: WAV, AIFF and FLAC. \
They run in this process, so they work while Musicable is closed, and they \
don't touch the open project. Give files by their whole paths. They never \
write over a file unless overwrite is true, and never write to a file they \
read. Results give times in seconds, with frames where they must be exact, \
the unit in each field's name, and null for silence. audio_info describes \
files, audio_find finds the loudest stretch or where sound starts, and \
audio_loudness measures loudness as EBU R 128 does.";

impl LocalTools for Tools {
    fn tools(&self) -> Vec<LocalTool> {
        (commands().iter())
            .map(|command| LocalTool {
                name: command.tool(),
                title: command.title.to_owned(),
                description: command.about.to_owned(),
                input_schema: command.input_schema(),
                output_schema: command.output_schema(),
                read_only: command.read_only,
            })
            .collect()
    }

    fn instructions(&self) -> String {
        INSTRUCTIONS.to_owned()
    }

    fn call(
        &self,
        name: &str,
        arguments: &Map<String, Json>,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(f64),
    ) -> Result<Json, String> {
        let commands = commands();
        let Some(command) = commands.iter().find(|command| command.tool() == name) else {
            return Err(format!("There's no tool called {name}."));
        };
        let args = args_json::args_from_json(&command.params, arguments)
            .map_err(|error| sentence(&error))?;
        let job = Job {
            folder: None,
            cancelled,
            progress,
        };
        command.run(&args, &job)
    }
}

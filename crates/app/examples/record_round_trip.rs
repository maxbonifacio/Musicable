//! Records clicks as the app records: through the app's own `Output`,
//! `Session` and recorder, an armed track records an input while another
//! track plays clicks out, through a loopback cable or an output's loopback.
//! Then it finds each click in the take and says where it landed, comps the
//! take and undoes that and the recording. With `--minutes`, it records for
//! that long and checks that nothing was lost on the way.
//!
//! ```text
//! cargo run --release -p musicable-app --example record_round_trip -- --loopback
//! cargo run --release -p musicable-app --example record_round_trip -- --output RME --input RME
//! cargo run --release -p musicable-app --features asio --example record_round_trip -- --asio
//! cargo run --release -p musicable-app --example record_round_trip -- --loopback --minutes 60
//! ```
//!
//! `cargo run -p musicable-audio --example round_trip -- --list` lists the
//! inputs and outputs. Through a loopback, a click lands on the frame it
//! played. Through a cable, it lands late by the converters' latency that
//! the drivers don't report, the same every time. `--help` lists the
//! options.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use musicable_app::{Output, Session};
use musicable_audio::{self as audio, DriverType, InputClock, Mode, StreamRequest};
use musicable_commands::{Args, Keymap, edit, take, transport};
use musicable_io::audio::{Decoder, SampleFormat, save_wav};
use musicable_model::clip::{AudioClip, ClipContent};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{AudioInput, Input, Monitor, Project, TrackId, TrackKind};

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
Records clicks through the app's recorder, and says where each one landed.

Usage: record_round_trip [options]

  --output <text>     an output device id, or part of its name
                      (default: the default output)
  --asio              play through an ASIO driver: the one --output names,
                      or the only one installed, and record from its own
                      inputs unless --input or --loopback says otherwise
                      (Windows builds with the asio feature)
  --exclusive         use the output in exclusive mode (Windows only)
  --rate <hz>         sample rate (default: the output's)
  --period <frames>   frames per period, the buffer size
                      (default: the device's)
  --input <text>      an input id, or part of its name
                      (default: the default input)
  --loopback          record what an output device plays, through its
                      loopback: the --output device's, or for an ASIO
                      output, the default output's (Windows only)
  --channel <n>       the input's first channel to record, from 1; the
                      track records it and the next (default: 1)
  --level <0-1>       how loud the clicks are (default: 0.25)
  --every <s>         seconds between clicks (default: 0.5)
  --clicks <n>        clicks to record (default: 20)
  --minutes <n>       record for this long instead, a click every --every
  --report <s>        seconds between status lines (default: 60)
  --keep              keep the recordings, and say where they are

Plug a cable from an output into an input, or use --loopback, and turn
down anything else playing. The recording track doesn't monitor its input,
so it never hears itself.
";

/// The clicks' file is a minute long at most: a long run plays it again
/// each minute.
const FILE_SECONDS: u64 = 60;
/// The song's tempo, the default: a minute is 120 beats.
const BPM: f64 = 120.0;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    run(&options).unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    })
}

struct Options {
    output: Option<String>,
    asio: bool,
    mode: Mode,
    rate: Option<u32>,
    period: Option<u32>,
    input: Option<String>,
    loopback: bool,
    channel: u16,
    level: f32,
    every: f64,
    clicks: u64,
    minutes: Option<f64>,
    report: Duration,
    keep: bool,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Self {
            output: None,
            asio: false,
            mode: Mode::Shared,
            rate: None,
            period: None,
            input: None,
            loopback: false,
            channel: 0,
            level: 0.25,
            every: 0.5,
            clicks: 20,
            minutes: None,
            report: Duration::from_secs(60),
            keep: false,
        };
        let mut args = args.into_iter();
        while let Some(option) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{option} needs a value"));
            match option.as_str() {
                "--asio" => options.asio = true,
                "--exclusive" => options.mode = Mode::Exclusive,
                "--loopback" => options.loopback = true,
                "--keep" => options.keep = true,
                "--output" => options.output = Some(value()?),
                "--input" => options.input = Some(value()?),
                "--rate" => options.rate = Some(positive(&option, &value()?)?),
                "--period" => options.period = Some(positive(&option, &value()?)?),
                "--channel" => {
                    let channel = positive(&option, &value()?)? - 1;
                    options.channel =
                        u16::try_from(channel).map_err(|_| "--channel is too high")?;
                }
                "--level" => {
                    let level: f32 = number(&option, &value()?)?;
                    if !(0.0..=1.0).contains(&level) || level == 0.0 {
                        return Err(format!("{option} is more than 0, up to 1"));
                    }
                    options.level = level;
                }
                "--every" => options.every = seconds(&option, &value()?)?,
                "--clicks" => options.clicks = positive(&option, &value()?)?.into(),
                "--minutes" => options.minutes = Some(seconds(&option, &value()?)?),
                "--report" => {
                    let report = seconds(&option, &value()?)?;
                    options.report = Duration::from_secs_f64(report);
                }
                _ => return Err(format!("unknown option {option}")),
            }
        }
        if options.every < 0.05 {
            return Err("--every is 0.05 seconds at least".to_owned());
        }
        if options.loopback && options.input.is_some() {
            return Err("--loopback and --input both say what to record from".to_owned());
        }
        Ok(options)
    }

    /// How long to record, in seconds: a quarter of a second before the
    /// first click, and the clicks, or the minutes asked for.
    fn length(&self) -> f64 {
        match self.minutes {
            Some(minutes) => minutes * 60.0,
            None => 0.25 + self.clicks as f64 * self.every,
        }
    }
}

fn number<T: FromStr>(option: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{option} takes a number, not {text}"))
}

fn positive(option: &str, text: &str) -> Result<u32, String> {
    match number(option, text)? {
        0 => Err(format!("{option} must be more than 0")),
        n => Ok(n),
    }
}

fn seconds(option: &str, text: &str) -> Result<f64, String> {
    let seconds: f64 = number(option, text)?;
    if seconds.is_finite() && seconds > 0.0 {
        Ok(seconds)
    } else {
        Err(format!("{option} must be more than 0"))
    }
}

/// Finds the output `text` names: its id, or part of its name. With `asio`,
/// only ASIO drivers count, and with no `text`, the only one installed does.
/// `None` is the default output.
fn find_output(text: Option<&str>, asio: bool) -> Result<Option<String>, String> {
    if !asio && text.is_none() {
        return Ok(None);
    }
    let devices: Vec<_> = audio::output_devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|device| !asio || device.driver_type == DriverType::Asio)
        .collect();
    let found: Vec<_> = match text {
        Some(text) => matching(&devices, text, |d| (&d.id, &d.name)),
        None => devices.iter().collect(),
    };
    match found.as_slice() {
        [device] => Ok(Some(device.id.clone())),
        [] if asio && text.is_none() => Err("no ASIO drivers are installed".to_owned()),
        [] => Err(format!(
            "no output matches \"{}\" (see --list in the round_trip example)",
            text.unwrap_or_default()
        )),
        _ => Err(format!(
            "pick one of {} with --output",
            names(found.iter().map(|device| device.name.as_str()))
        )),
    }
}

/// The input to record from, for a stream on `output`, and its name and
/// channels.
fn find_input(options: &Options, output: Option<&str>) -> Result<(String, String, u16), String> {
    let inputs = audio::input_devices().map_err(|e| e.to_string())?;
    let asio = output.is_some_and(|id| DriverType::of(id) == DriverType::Asio);
    let id = if options.loopback {
        let device = match output {
            Some(id) if !asio => id.to_owned(),
            // ASIO drivers have no loopback: the system's default output's
            // is on another clock.
            _ => audio::output_devices()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|device| device.is_default && device.driver_type == DriverType::System)
                .map(|device| device.id)
                .ok_or("there's no default output to record the loopback of")?,
        };
        audio::loopback_id(&device)
    } else if let Some(text) = &options.input {
        let found = matching(&inputs, text, |input| (&input.id, &input.name));
        match found.as_slice() {
            [input] => input.id.clone(),
            [] => return Err(format!("no input matches \"{text}\"")),
            _ => {
                return Err(format!(
                    "\"{text}\" matches {}",
                    names(found.iter().map(|input| input.name.as_str()))
                ));
            }
        }
    } else if asio && options.asio {
        output.unwrap_or_default().to_owned()
    } else {
        inputs
            .iter()
            .find(|input| input.is_default)
            .map(|input| input.id.clone())
            .ok_or("there's no default input: name one with --input")?
    };
    let input = inputs.iter().find(|input| input.id == id);
    let name = input.map_or_else(|| id.clone(), |input| input.name.clone());
    // An ASIO driver says how many channels it has once it's loaded.
    let channels = input.map_or(0, |input| input.channels);
    Ok((id, name, channels))
}

/// The `items` whose id is `text`, or failing that, whose name has `text`
/// in it.
fn matching<'a, T>(
    items: &'a [T],
    text: &str,
    id_and_name: impl Fn(&T) -> (&String, &String),
) -> Vec<&'a T> {
    if let Some(item) = items.iter().find(|item| id_and_name(item).0 == text) {
        return vec![item];
    }
    let wanted = text.to_lowercase();
    items
        .iter()
        .filter(|item| id_and_name(item).1.to_lowercase().contains(&wanted))
        .collect()
}

fn names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

/// The clicks: where in the song each one plays, in frames.
struct Clicks {
    at: Vec<u64>,
    /// Frames between them.
    every: u64,
}

/// Writes the clicks' file into `folder`, and puts clips playing it on a
/// track of `project`, from the start of the song for `length` frames at
/// `rate`: a click of a single frame every `every` frames, from a quarter
/// of a second in.
fn add_clicks(
    project: &mut Project,
    folder: &Path,
    rate: u32,
    length: u64,
    every: u64,
    level: f32,
) -> Result<Clicks, String> {
    let file_frames = length.min(FILE_SECONDS * u64::from(rate));
    let first = u64::from(rate) / 4;
    let mut samples = vec![0.0; file_frames as usize];
    let in_file: Vec<u64> = (first..file_frames).step_by(every as usize).collect();
    for &at in &in_file {
        samples[at as usize] = level;
    }
    let path = std::path::absolute(folder.join("Clicks.wav")).map_err(|e| e.to_string())?;
    save_wav(&path, rate, SampleFormat::Float32, &[&samples])
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    let file = path.to_string_lossy().into_owned();
    let track = project.add_track("Clicks", TrackKind::Audio);
    // A minute is 120 beats, so each minute's clip starts on its frame.
    let beats_per_frame = BPM / 60.0 / f64::from(rate);
    let mut at = Vec::new();
    let mut start = 0;
    while start < length {
        let end = (start + file_frames).min(length);
        let range = BeatRange::new(
            Beats::from_f64(start as f64 * beats_per_frame),
            Beats::from_f64(end as f64 * beats_per_frame),
        );
        let clip = project.new_clip("Clicks", range, ClipContent::Audio(AudioClip::new(&file)));
        let t = project.track_mut(track).expect("the track was just added");
        t.clips.push(clip);
        t.input = None;
        // The last click of a clip cut short may not play whole.
        at.extend(in_file.iter().map(|&i| start + i).filter(|&i| i + 1 < end));
        start = end;
    }
    Ok(Clicks { at, every })
}

/// Opens the inputs the session's tracks want, as the app does, unless that
/// would cut a recording short, and hands a new stream's inputs to the
/// session.
fn follow_inputs(output: &mut Output, session: &mut Session, started: &mut u64) {
    if !(session.is_recording() && session.stats().playing) {
        let wanted: BTreeSet<String> = session.wanted_inputs();
        output.want_inputs(&wanted, session);
    }
    if output.started() != *started {
        *started = output.started();
        session.set_inputs(output.input_layout(), output.input_buffers());
    }
}

/// The app's loop, for this example: what the window does each frame.
struct Studio {
    output: Output,
    session: Session,
    started: u64,
    problems: Vec<String>,
}

impl Studio {
    fn tick(&mut self) {
        self.session.tick();
        let mut notices = Vec::new();
        self.output.tick(&mut self.session, &mut notices);
        follow_inputs(&mut self.output, &mut self.session, &mut self.started);
        notices.extend(self.session.take_problems());
        for notice in notices {
            println!("  {notice}");
            self.problems.push(notice);
        }
        thread::sleep(Duration::from_millis(10));
    }

    /// Ticks until `done` says so, failing after `seconds` seconds.
    fn wait(
        &mut self,
        what: &str,
        seconds: u64,
        done: impl Fn(&Self) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while !done(self) {
            if Instant::now() > deadline {
                return Err(format!("no {what} after {seconds} seconds"));
            }
            self.tick();
        }
        Ok(())
    }
}

fn run(options: &Options) -> Result<ExitCode, String> {
    let output = find_output(options.output.as_deref(), options.asio)?;
    let (input, input_name, channels) = find_input(options, output.as_deref())?;
    let request = StreamRequest {
        device: output,
        mode: options.mode,
        sample_rate: options.rate,
        period: options.period,
        inputs: Vec::new(),
        midi_inputs: Vec::new(),
    };
    let mut notices = Vec::new();
    let (out, handle) = Output::open(request, &mut notices);
    for notice in &notices {
        println!("{notice}");
    }
    let Some(info) = out.info() else {
        return Err(format!(
            "the audio device didn't open: {}",
            out.problem().unwrap_or("no reason given")
        ));
    };
    let rate = info.sample_rate;
    println!("Output: {}, {}", out.device_name(), out.summary());

    let folder = env::temp_dir().join(format!(
        "musicable-record-round-trip-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).map_err(|e| format!("couldn't make {}: {e}", folder.display()))?;
    let length = (options.length() * f64::from(rate)).round() as u64;
    let every = (options.every * f64::from(rate)).round() as u64;
    let mut project = Project::new();
    let clicks = add_clicks(&mut project, &folder, rate, length, every, options.level)?;
    let recorder = project.add_track("Recorder", TrackKind::Audio);
    let track = project
        .track_mut(recorder)
        .expect("the track was just added");
    track.armed = true;
    // Monitoring a loopback would play what it records back into it.
    track.monitor = Monitor::Off;
    track.input = Some(Input::Audio(AudioInput {
        device: input.clone(),
        name: input_name.clone(),
        channel: options.channel,
        stereo: channels != 1,
    }));

    let mut session = Session::new(project, None, Keymap::default_keymap(), handle);
    session.set_unsaved_recordings(Some(folder.join("Recordings")));
    let mut studio = Studio {
        output: out,
        session,
        started: 0,
        problems: Vec::new(),
    };
    let result = record(&mut studio, options, &clicks, recorder, length);
    if options.keep {
        println!("The clicks and the recording are in {}.", folder.display());
    } else {
        // Closes the files first.
        drop(studio);
        let _ = fs::remove_dir_all(&folder);
    }
    result
}

fn record(
    studio: &mut Studio,
    options: &Options,
    clicks: &Clicks,
    recorder: TrackId,
    length: u64,
) -> Result<ExitCode, String> {
    let rate = studio.session.engine_config().sample_rate;
    // The stream opens again with the input, which lines up in a fraction
    // of a second, unless it doesn't open.
    studio.wait("input", 10, |studio| {
        let opened = studio.output.info().and_then(|info| info.inputs.first());
        let aligned = studio
            .output
            .input_stats()
            .first()
            .is_some_and(|i| i.aligned);
        opened.is_some_and(|input| input.error.is_some()) || aligned
    })?;
    if let Some(input) = studio.output.info().and_then(|info| info.inputs.first()) {
        match &input.error {
            None => println!(
                "Input: {}, {} channels at {} Hz, {:?} clock",
                input.name, input.channels, input.sample_rate, input.clock
            ),
            Some(error) => return Err(format!("{} didn't open: {error}", input.name)),
        }
    }
    studio.session.wait_for_graph();
    // A second to settle.
    let settle = Instant::now() + Duration::from_secs(1);
    studio.wait("time", 2, |_| Instant::now() >= settle)?;
    studio.problems.clear();
    let dropouts = studio.output.dropouts();
    let lost = studio.output.input_stats().first().map_or(0, |i| i.lost);

    println!(
        "Recording {:.1} seconds, a click every {} seconds...",
        length as f64 / f64::from(rate),
        options.every
    );
    studio
        .session
        .run(&transport::RECORD)
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(length / u64::from(rate) + 30);
    let mut report = started + options.report;
    while studio.session.stats().position < length {
        if Instant::now() > deadline {
            return Err("the transport didn't get to the end of the recording".to_owned());
        }
        studio.tick();
        if Instant::now() >= report {
            report += options.report;
            status(studio, started, dropouts, lost);
        }
    }
    status(studio, started, dropouts, lost);
    studio
        .session
        .run(&transport::STOP)
        .map_err(|e| e.to_string())?;
    studio.wait("take", 60, |studio| {
        let track = studio.session.project().track(recorder);
        track.is_some_and(|track| !track.takes.is_empty())
    })?;
    let mut good = true;
    let dropouts = studio.output.dropouts().saturating_sub(dropouts);
    let input = studio.output.input_stats().first().cloned();
    let lost = input.as_ref().map_or(0, |i| i.lost).saturating_sub(lost);
    if dropouts > 0 || lost > 0 || !studio.problems.is_empty() {
        println!("Dropouts: {dropouts}, input frames lost: {lost}.");
        good = false;
    }
    if let Some(input) = &input
        && input.clock == InputClock::Resampled
    {
        println!(
            "The input runs {:+.2} ppm fast, resampled.",
            input.drift_ppm
        );
    }

    let track = studio
        .session
        .project()
        .track(recorder)
        .expect("the track is there");
    let [take] = track.takes.as_slice() else {
        return Err(format!("{} takes, not 1", track.takes.len()));
    };
    let [clip] = take.clips.as_slice() else {
        return Err(format!("the take has {} clips, not 1", take.clips.len()));
    };
    let ClipContent::Audio(audio) = &clip.content else {
        return Err("the take isn't audio".to_owned());
    };
    let tempo = &studio.session.project().tempo;
    // The song's frame that the file's first frame plays on.
    let first = tempo.frame_at(clip.start, rate) - audio.offset as i64;
    let recorded = tempo.frame_at(clip.end(), rate) - tempo.frame_at(clip.start, rate);
    println!(
        "The take: {} frames from song frame {}, in {}",
        recorded,
        tempo.frame_at(clip.start, rate),
        audio.file
    );
    if recorded.unsigned_abs() + 1 < length {
        println!("It's {} frames short.", length - recorded.unsigned_abs());
        good = false;
    }
    let found = find_clicks(Path::new(&audio.file), first, clicks)?;
    good &= summarize(&found);

    // Comping a beat of the take splits the track's clip around it, and
    // undo takes that back, and then the recording.
    let take = take.id;
    let start = clip.start + Beats::new(1);
    let args = Args::new()
        .with("take", take)
        .with("start", start)
        .with("end", start + Beats::new(1));
    let before = clips(&studio.session, recorder);
    studio
        .session
        .run_with(&take::COMP, &args)
        .map_err(|e| e.to_string())?;
    let comped = clips(&studio.session, recorder);
    studio.session.run(&edit::UNDO).map_err(|e| e.to_string())?;
    let undone = clips(&studio.session, recorder);
    let undo = studio.session.undo_name().map(str::to_owned);
    studio.session.run(&edit::UNDO).map_err(|e| e.to_string())?;
    let track = studio
        .session
        .project()
        .track(recorder)
        .expect("the track is there");
    let comps = comped.len() == 3 && undone == before && undo.as_deref() == Some("Record");
    let gone = track.takes.is_empty() && track.clips.is_empty();
    println!(
        "Comping a beat of the take made {} clips of 1; undo put back {}, then took the \
         recording back{}.",
        comped.len(),
        if undone == before {
            "the one"
        } else {
            "something else"
        },
        if gone { "" } else { ", or didn't" }
    );
    good &= comps && gone;
    for problem in &studio.problems {
        println!("Problem: {problem}");
    }
    println!("{}", if good { "Good." } else { "Not good!" });
    Ok(if good {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The track's clips, by where they are.
fn clips(session: &Session, track: TrackId) -> Vec<BeatRange> {
    let track = session.project().track(track).expect("the track is there");
    let mut clips: Vec<BeatRange> = track.clips.iter().map(|clip| clip.range()).collect();
    clips.sort_by_key(|range| range.start);
    clips
}

/// A line on how the recording goes.
fn status(studio: &Studio, started: Instant, dropouts: u64, lost: u64) {
    let input = studio
        .output
        .input_stats()
        .first()
        .cloned()
        .unwrap_or_default();
    let recorded: u64 = studio.session.recordings().iter().map(|r| r.frames).sum();
    println!(
        "  {:.0} s: {} frames recorded so far, dropouts {}, input lost {}, slips {}",
        started.elapsed().as_secs_f64(),
        recorded,
        studio.output.dropouts().saturating_sub(dropouts),
        input.lost.saturating_sub(lost),
        input.slips,
    );
}

/// Finds each click in the recording at `path`, whose first frame is at
/// song frame `first`: how far after where it played it landed, to a
/// fraction of a frame, or `None` where there's nothing to find.
fn find_clicks(path: &Path, first: i64, clicks: &Clicks) -> Result<Vec<Option<f64>>, String> {
    let mut decoder = Decoder::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let channels = decoder.info().channels;
    // Each click is looked for from 40% of the way back to the one before
    // to 40% of the way on to the next.
    let reach = clicks.every * 2 / 5;
    let mut window = vec![0.0; (2 * reach as usize + 1) * channels];
    let mut found = Vec::with_capacity(clicks.at.len());
    for &at in &clicks.at {
        let from = at as i64 - reach as i64 - first;
        let Ok(from) = u64::try_from(from) else {
            found.push(None);
            continue;
        };
        window.fill(0.0);
        let read = decoder
            .read(from, &mut window)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // The loudest channel's click.
        let best = (0..channels)
            .filter_map(|c| {
                let samples: Vec<f32> = window[..read * channels]
                    .iter()
                    .skip(c)
                    .step_by(channels)
                    .copied()
                    .collect();
                click_in(&samples)
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        found.push(best.map(|(_, offset)| offset - reach as f64));
    }
    Ok(found)
}

/// The click in `samples`, if there's one: how loud its peak is, and where,
/// to a fraction of a frame.
fn click_in(samples: &[f32]) -> Option<(f32, f64)> {
    let (at, peak) = samples
        .iter()
        .map(|sample| sample.abs())
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let mut sorted: Vec<f32> = samples.iter().map(|sample| sample.abs()).collect();
    sorted.sort_by(f32::total_cmp);
    let noise = sorted[sorted.len() / 2];
    if peak < 1e-3 || peak < 20.0 * noise {
        return None;
    }
    // A parabola through the peak and the frames either side of it finds
    // where between frames the peak was.
    let mut fraction = 0.0;
    if at > 0 && at + 1 < samples.len() {
        let sign = samples[at].signum();
        let (a, b, c) = (
            f64::from(samples[at - 1] * sign),
            f64::from(samples[at] * sign),
            f64::from(samples[at + 1] * sign),
        );
        let curve = a - 2.0 * b + c;
        if curve < 0.0 {
            fraction = (0.5 * (a - c) / curve).clamp(-0.5, 0.5);
        }
    }
    Some((peak, at as f64 + fraction))
}

/// Says where the clicks landed. Returns whether every one was found, and
/// all landed within two frames of each other.
fn summarize(found: &[Option<f64>]) -> bool {
    let offsets: Vec<f64> = found.iter().flatten().copied().collect();
    let missed = found.len() - offsets.len();
    if offsets.is_empty() {
        println!(
            "No click came back. Is the cable plugged in, or the loopback's output the one \
             playing?"
        );
        return false;
    }
    let low = offsets.iter().copied().fold(f64::INFINITY, f64::min);
    let high = offsets.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = offsets.iter().sum::<f64>() / offsets.len() as f64;
    let worst = (high - mean).max(mean - low);
    println!(
        "{} clicks found: {mean:+.2} frames from where they played, from {low:+.2} to \
         {high:+.2}, so every click lands within {worst:.2} frames of the mean.",
        offsets.len(),
    );
    if found.len() <= 40 {
        let each: Vec<String> = found
            .iter()
            .map(|found| found.map_or_else(|| "-".to_owned(), |offset| format!("{offset:+.1}")))
            .collect();
        println!("  each: {}", each.join(" "));
    }
    if missed > 0 {
        println!("{missed} clicks weren't found.");
    }
    missed == 0 && worst <= 2.0
}

//! Plays clicks and records them back, through a loopback cable or an
//! output's loopback, and says where each one landed: the check that a
//! recording lands where it was played. With `--soak`, it plays a tone
//! instead, for as long as asked, and checks every frame it records.
//!
//! ```text
//! cargo run --release -p musicable-audio --example round_trip -- --list
//! cargo run --release -p musicable-audio --example round_trip -- --loopback
//! cargo run --release -p musicable-audio --example round_trip -- --output RME --input RME
//! cargo run --release -p musicable-audio --example round_trip -- --asio
//! cargo run --release -p musicable-audio --example round_trip -- --input RME --soak 60
//! ```
//!
//! Each run opens the stream again, so the runs show whether the offset
//! stays put from one stream to the next: a recording can only be lined up
//! by a calibrated offset if it does. Through a cable, the offset is the
//! converters' latency that the drivers don't report. Through a loopback,
//! there are no converters. `--help` lists the options.

use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};
use std::{env, thread};

use musicable_audio::{
    self as audio, Device, DriverType, InputBuffer, InputClock, InputDevice, InputKind, Mode,
    Stream, StreamInfo, StreamRequest,
};
use musicable_engine::nodes::Sine;
use musicable_engine::{
    AudioMut, Engine, EngineHandle, Event, Graph, Inputs, Node, NodeKey, ProcessContext,
    TransportCommand,
};

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
Plays clicks and records them back, and says where each one landed.

Usage: round_trip [options]

  --list              list the inputs and outputs
  --output <text>     an output device id, or part of its name
                      (default: the default output)
  --asio              play through an ASIO driver: the one --output names,
                      or the only one installed, and record from its own
                      inputs unless --input or --loopback says otherwise
                      (Windows only)
  --exclusive         use the output in exclusive mode (Windows only)
  --rate <hz>         sample rate (default: the output's)
  --period <frames>   frames per period, the buffer size
                      (default: the smallest the output supports)
  --input <text>      an input id, or part of its name
                      (default: the default input)
  --loopback          record what an output device plays, through its
                      loopback: the --output device's, or for an ASIO
                      output, the default output's (Windows only)
  --channel <n>       the input channel to look at, from 1
                      (default: the loudest)
  --level <0-1>       how loud the clicks or the tone are (default: 0.25)
  --clicks <n>        clicks a run (default: 20)
  --every <s>         seconds between clicks (default: 0.5)
  --runs <n>          times to open the stream and measure (default: 3)
  --soak <minutes>    play a tone for this long instead, and check every
                      frame recorded for a glitch
  --tone <hz>         the soak's tone (default: 997)
  --report <s>        seconds between the soak's status lines (default: 60)

Plug a cable from an output into an input, or use --loopback, and turn
down anything else playing. Through a microphone, it measures the room.
";

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
    let result = if options.list { list() } else { run(&options) };
    result.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    })
}

struct Options {
    list: bool,
    output: Option<String>,
    asio: bool,
    mode: Mode,
    rate: Option<u32>,
    period: Option<u32>,
    input: Option<String>,
    loopback: bool,
    channel: Option<usize>,
    level: f32,
    clicks: u32,
    every: Duration,
    runs: u32,
    soak: Option<Duration>,
    tone: f64,
    report: Duration,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Self {
            list: false,
            output: None,
            asio: false,
            mode: Mode::Shared,
            rate: None,
            period: None,
            input: None,
            loopback: false,
            channel: None,
            level: 0.25,
            clicks: 20,
            every: Duration::from_millis(500),
            runs: 3,
            soak: None,
            tone: 997.0,
            report: Duration::from_secs(60),
        };
        let mut args = args.into_iter();
        while let Some(option) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{option} needs a value"));
            match option.as_str() {
                "--list" => options.list = true,
                "--asio" => options.asio = true,
                "--exclusive" => options.mode = Mode::Exclusive,
                "--loopback" => options.loopback = true,
                "--output" => options.output = Some(value()?),
                "--input" => options.input = Some(value()?),
                "--rate" => options.rate = Some(positive(&option, &value()?)?),
                "--period" => options.period = Some(positive(&option, &value()?)?),
                "--channel" => {
                    options.channel = Some(positive(&option, &value()?)? as usize - 1);
                }
                "--level" => {
                    let level: f32 = number(&option, &value()?)?;
                    if !(0.0..=1.0).contains(&level) || level == 0.0 {
                        return Err(format!("{option} is more than 0, up to 1"));
                    }
                    options.level = level;
                }
                "--clicks" => options.clicks = positive(&option, &value()?)?,
                "--every" => options.every = duration(&option, &value()?, SECOND)?,
                "--runs" => options.runs = positive(&option, &value()?)?,
                "--soak" => options.soak = Some(duration(&option, &value()?, MINUTE)?),
                "--tone" => {
                    let tone: f64 = number(&option, &value()?)?;
                    if !(20.0..=5_000.0).contains(&tone) {
                        return Err(format!("{option} is from 20 to 5000 Hz"));
                    }
                    options.tone = tone;
                }
                "--report" => options.report = duration(&option, &value()?, SECOND)?,
                _ => return Err(format!("unknown option {option}")),
            }
        }
        if options.every < Duration::from_millis(50) {
            return Err("--every is 0.05 seconds at least".to_owned());
        }
        if options.loopback && options.input.is_some() {
            return Err("--loopback and --input both say what to record from".to_owned());
        }
        Ok(options)
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

const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);

/// A positive number of `unit`s.
fn duration(option: &str, text: &str, unit: Duration) -> Result<Duration, String> {
    let count: f64 = number(option, text)?;
    Duration::try_from_secs_f64(count * unit.as_secs_f64())
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| format!("{option} must be more than 0"))
}

fn list() -> Result<ExitCode, String> {
    println!("Inputs:");
    let inputs = audio::input_devices().map_err(|e| e.to_string())?;
    if inputs.is_empty() {
        println!("  none");
    }
    for input in inputs {
        let default = if input.is_default { " (default)" } else { "" };
        match input.kind {
            InputKind::Loopback => println!(
                "  {}: what it plays, {} channels",
                input.name, input.channels
            ),
            _ if DriverType::of(&input.id) == DriverType::Asio => {
                let channels = audio::capabilities(Some(&input.id))
                    .ok()
                    .and_then(|capabilities| capabilities.asio)
                    .map_or_else(|| "?".to_owned(), |asio| asio.inputs.to_string());
                println!(
                    "  {}: the ASIO driver's inputs, {channels} channels, with its own \
                     output only",
                    input.name
                );
            }
            _ => println!("  {}{default}, {} channels", input.name, input.channels),
        }
        println!("    id: {}", input.id);
    }
    println!("Outputs:");
    for device in audio::output_devices().map_err(|e| e.to_string())? {
        let default = if device.is_default { " (default)" } else { "" };
        let asio = if device.driver_type == DriverType::Asio {
            " (ASIO driver)"
        } else {
            ""
        };
        println!("  {}{default}{asio}", device.name);
        println!("    id: {}", device.id);
    }
    Ok(ExitCode::SUCCESS)
}

/// Finds the output `text` names: its id, or part of its name. With `asio`,
/// only ASIO drivers count, and with no `text`, the only one installed does.
/// `None` is the default output.
fn find_output(text: Option<&str>, asio: bool) -> Result<Option<String>, String> {
    if !asio && text.is_none() {
        return Ok(None);
    }
    let devices: Vec<Device> = audio::output_devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|device| !asio || device.driver_type == DriverType::Asio)
        .collect();
    let Some(text) = text else {
        return match devices.as_slice() {
            [device] => Ok(Some(device.id.clone())),
            [] if audio::driver_types().contains(&DriverType::Asio) => {
                Err("no ASIO drivers are installed".to_owned())
            }
            [] => Err("ASIO is for Windows only".to_owned()),
            _ => Err(format!(
                "pick one of the ASIO drivers with --output: {}",
                names(devices.iter().map(|device| device.name.as_str()))
            )),
        };
    };
    let found = matching(&devices, text, |device| (&device.id, &device.name))?;
    match found.as_slice() {
        [device] => Ok(Some(device.id.clone())),
        [] => Err(format!("no output matches \"{text}\" (see --list)")),
        _ => Err(format!(
            "\"{text}\" matches {}",
            names(found.iter().map(|device| device.name.as_str()))
        )),
    }
}

/// The input to record from, for a stream on `output`.
fn find_input(options: &Options, output: Option<&str>) -> Result<String, String> {
    let outputs = || audio::output_devices().map_err(|e| e.to_string());
    if options.loopback {
        let asio = output.is_some_and(|id| DriverType::of(id) == DriverType::Asio);
        let device = match output {
            Some(id) if !asio => id.to_owned(),
            // ASIO drivers have no loopback: the system's default output's
            // is on another clock.
            _ => outputs()?
                .into_iter()
                .find(|device| device.is_default && device.driver_type == DriverType::System)
                .map(|device| device.id)
                .ok_or("there's no default output to record the loopback of")?,
        };
        return Ok(audio::loopback_id(&device));
    }
    let inputs = audio::input_devices().map_err(|e| e.to_string())?;
    let Some(text) = &options.input else {
        if options.asio
            && let Some(id) = output
        {
            return Ok(id.to_owned());
        }
        return inputs
            .into_iter()
            .find(|input| input.is_default)
            .map(|input| input.id)
            .ok_or_else(|| "there's no default input: name one with --input".to_owned());
    };
    let found = matching(&inputs, text, |input: &InputDevice| {
        (&input.id, &input.name)
    })?;
    match found.as_slice() {
        [input] => Ok(input.id.clone()),
        [] => Err(format!("no input matches \"{text}\" (see --list)")),
        _ => Err(format!(
            "\"{text}\" matches {}",
            names(found.iter().map(|input| input.name.as_str()))
        )),
    }
}

/// The `items` whose id is `text`, or failing that, whose name has `text`
/// in it.
fn matching<'a, T>(
    items: &'a [T],
    text: &str,
    id_and_name: impl Fn(&T) -> (&String, &String),
) -> Result<Vec<&'a T>, String> {
    if let Some(item) = items.iter().find(|item| id_and_name(item).0 == text) {
        return Ok(vec![item]);
    }
    let wanted = text.to_lowercase();
    Ok(items
        .iter()
        .filter(|item| id_and_name(item).1.to_lowercase().contains(&wanted))
        .collect())
}

fn names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

fn run(options: &Options) -> Result<ExitCode, String> {
    let output = find_output(options.output.as_deref(), options.asio)?;
    let input = find_input(options, output.as_deref())?;
    let request = StreamRequest {
        device: output,
        mode: options.mode,
        sample_rate: options.rate,
        period: options.period,
        inputs: vec![input],
        midi_inputs: Vec::new(),
    };
    match options.soak {
        Some(length) => soak(&request, options, length),
        None => round_trip(&request, options),
    }
}

/// Opens the stream `request` asks for, with its input, and starts it
/// playing the node `node` makes for the stream's rate.
fn open<N: Node>(
    request: &StreamRequest,
    node: impl FnOnce(u32) -> N,
    print: bool,
) -> Result<(Stream, InputBuffer, EngineHandle), String> {
    let open = audio::open(request).map_err(|e| e.to_string())?;
    let info = open.info().clone();
    if print {
        print_info(&info);
    }
    let input = &info.inputs[0];
    if let Some(error) = &input.error {
        return Err(format!("{} didn't open: {error}", input.name));
    }
    let buffer = open.input_buffers()[0]
        .clone()
        .ok_or("the input didn't open")?;
    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let node = graph.add(NodeKey(1), node(info.sample_rate));
    graph.set_output(node).map_err(|e| format!("{e:?}"))?;
    let graph = graph.prepare(&config).map_err(|e| format!("{e:?}"))?;
    handle.swap_graph(graph).map_err(|e| format!("{e:?}"))?;
    handle
        .send(Event::Transport(TransportCommand::Play))
        .map_err(|e| format!("{e:?}"))?;
    let stream = open.start(engine).map_err(|e| e.to_string())?;
    Ok((stream, buffer, handle))
}

fn print_info(info: &StreamInfo) {
    let rate = info.sample_rate;
    println!(
        "Output: {}, {}, {rate} Hz, period {} frames, latency {} frames ({})",
        info.device.name,
        info.api,
        info.period,
        info.latency,
        ms(info.latency.into(), rate),
    );
    for input in &info.inputs {
        match &input.error {
            None => println!(
                "Input: {}, {} channels at {} Hz, {:?} clock",
                input.name, input.channels, input.sample_rate, input.clock
            ),
            Some(error) => println!("Input: {} didn't open: {error}", input.name),
        }
    }
}

/// Plays `count` clicks, `every` frames apart from engine frame `first`:
/// each a single frame at `level`, in every output channel.
#[derive(Debug, Clone, Copy)]
struct Clicks {
    first: u64,
    every: u64,
    count: u64,
    level: f32,
}

impl Clicks {
    /// The clicks `options` ask for, at `rate`: from two seconds in, once
    /// the input has lined up and settled.
    fn new(options: &Options, rate: u32) -> Clicks {
        Clicks {
            first: 2 * u64::from(rate),
            every: (options.every.as_secs_f64() * f64::from(rate)).round() as u64,
            count: u64::from(options.clicks),
            level: options.level,
        }
    }

    /// Where click `click` plays.
    fn at(&self, click: u64) -> u64 {
        self.first + click * self.every
    }
}

impl Node for Clicks {
    fn channels(&self) -> usize {
        1
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.clear();
        let end = ctx.time + ctx.frames as u64;
        let mut click = ctx.time.saturating_sub(self.first).div_ceil(self.every);
        while click < self.count && self.at(click) < end {
            let i = (self.at(click) - ctx.time) as usize;
            output.channel_mut(0)[i] = self.level;
            click += 1;
        }
    }
}

/// Where a click landed in what an input recorded.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// Frames after where it was played, to a fraction of a frame.
    offset: f64,
    channel: usize,
}

fn round_trip(request: &StreamRequest, options: &Options) -> Result<ExitCode, String> {
    let mut all: Vec<f64> = Vec::new();
    let mut missed = 0;
    for run in 1..=options.runs {
        let (stream, buffer, _handle) = open(request, |rate| Clicks::new(options, rate), run == 1)?;
        let started = Instant::now();
        let clicks = Clicks::new(options, stream.info().sample_rate);
        let found = find_clicks(&stream, &buffer, &clicks, options.channel, started)?;
        let stats = stream.stats();
        let input = stream.input_stats().remove(0);
        let rate = stream.info().sample_rate;
        drop(stream.stop());

        let offsets: Vec<f64> = found.iter().flatten().map(|found| found.offset).collect();
        let channels: Vec<usize> = found.iter().flatten().map(|found| found.channel).collect();
        missed += found.len() - offsets.len();
        let summary = match spread(&offsets) {
            Some((low, mean, high)) => format!(
                "offset {mean:+.2} frames ({}), from {low:+.2} to {high:+.2}",
                ms_signed(mean, rate),
            ),
            None => "no clicks found".to_owned(),
        };
        let missing = match found.len() - offsets.len() {
            0 => String::new(),
            n => format!(", {n} not found"),
        };
        let channel = match (
            channels.first(),
            channels.iter().all(|&c| Some(&c) == channels.first()),
        ) {
            (Some(c), true) => format!(" on input channel {}", c + 1),
            (Some(_), false) => " on several input channels".to_owned(),
            (None, _) => String::new(),
        };
        let each: Vec<String> = found
            .iter()
            .map(|found| found.map_or_else(|| "-".to_owned(), |f| format!("{:+.1}", f.offset)))
            .collect();
        println!(
            "Run {run}: {summary}{missing}{channel}. Monitoring delay {} frames ({}). Input: \
             lost {}, slips {}, discontinuities {}, {:?} clock{}{}\n  each: {}",
            stats.input_delay,
            ms(stats.input_delay, rate),
            input.lost,
            input.slips,
            input.discontinuities,
            input.clock,
            match input.clock {
                InputClock::Resampled => format!(", drift {:+.2} ppm", input.drift_ppm),
                _ => String::new(),
            },
            input
                .error
                .map_or_else(String::new, |error| format!(", stopped: {error}")),
            each.join(" "),
        );
        all.extend(offsets);
    }
    let Some((low, mean, high)) = spread(&all) else {
        println!(
            "\nNo click came back. Is the cable plugged in, or the loopback's output the one \
             playing?"
        );
        return Ok(ExitCode::FAILURE);
    };
    let worst = (high - mean).max(mean - low);
    println!(
        "\nAll {} clicks found: offset {mean:+.2} frames, from {low:+.2} to {high:+.2}. \
         Calibrated by the mean, every click lands within {worst:.2} frames of where it was \
         played.",
        all.len()
    );
    if missed > 0 {
        println!("{missed} clicks weren't found.");
    }
    if missed == 0 && worst <= 2.0 {
        println!("Within two frames: good.");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("Not within two frames!");
        Ok(ExitCode::FAILURE)
    }
}

/// The lowest, mean and highest of `values`, if there are any.
fn spread(values: &[f64]) -> Option<(f64, f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    Some((low, mean, high))
}

/// Finds each click in what the input records, as it records it: where it
/// landed, or `None` where there was nothing to find. The stream `started`
/// then.
fn find_clicks(
    stream: &Stream,
    buffer: &InputBuffer,
    clicks: &Clicks,
    channel: Option<usize>,
    started: Instant,
) -> Result<Vec<Option<Found>>, String> {
    // Each click is looked for from 40% of the way back to the one before
    // to 40% of the way on to the next.
    let reach = clicks.every * 2 / 5;
    let channels: Vec<usize> = match channel {
        Some(channel) if channel < buffer.channels() => vec![channel],
        Some(channel) => {
            return Err(format!(
                "the input has {} channels, not {}",
                buffer.channels(),
                channel + 1
            ));
        }
        None => (0..buffer.channels()).collect(),
    };
    let rate = f64::from(stream.info().sample_rate);
    let mut found = Vec::new();
    let mut samples = vec![0.0; 2 * reach as usize + 1];
    for click in 0..clicks.count {
        let at = clicks.at(click);
        let (from, to) = (at - reach, at + reach + 1);
        // Waits for the input to record past the click, giving up if it
        // doesn't within a few seconds of when it should have.
        let deadline = started + Duration::from_secs_f64(to as f64 / rate + 3.0);
        while buffer.frames().end < to {
            if stream.has_ended() {
                return Err("the stream ended".to_owned());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "the input recorded nothing for a few seconds: {:?}",
                    stream.input_stats()[0]
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
        // What the buffer still has, if the input lined up after the
        // window started.
        let start = from.max(buffer.frames().start);
        let mut best: Option<(f32, Found)> = None;
        for &channel in &channels {
            let (before, part) = samples.split_at_mut((start - from) as usize);
            before.fill(0.0);
            if start > at || buffer.read_channel(start, channel, part).is_err() {
                continue;
            }
            if let Some((peak, offset)) = click_in(&samples)
                && best.is_none_or(|(best, _)| peak > best)
            {
                let offset = offset - reach as f64;
                best = Some((peak, Found { offset, channel }));
            }
        }
        found.push(best.map(|(_, found)| found));
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

fn soak(request: &StreamRequest, options: &Options, length: Duration) -> Result<ExitCode, String> {
    let tone = |_| Sine::new(options.tone, options.level).with_channels(1);
    let (stream, buffer, _handle) = open(request, tone, true)?;
    let rate = stream.info().sample_rate;
    let started = Instant::now();
    let end = started + length;
    let mut next_report = started + options.report;
    let mut checker = Checker::new(options.tone, rate, options.channel);
    let mut next = None::<u64>;
    let mut chunk = vec![0.0; 4096 * buffer.channels()];
    let mut ended = None;
    while Instant::now() < end {
        thread::sleep(Duration::from_millis(50));
        if stream.has_ended() {
            ended = Some("the stream ended");
            break;
        }
        let frames = buffer.frames();
        if frames.is_empty() {
            // Not lined up yet.
            continue;
        }
        let mut at = *next.get_or_insert(frames.start);
        if at < frames.start {
            // Fell behind what the buffer keeps.
            checker.missed += frames.start - at;
            at = frames.start;
        }
        while at < frames.end {
            let count = (frames.end - at).min(4096) as usize;
            let samples = &mut chunk[..count * buffer.channels()];
            if buffer.read(at, samples).is_err() {
                break;
            }
            checker.check(at, samples, buffer.channels());
            at += count as u64;
        }
        next = Some(at);
        let now = Instant::now();
        if now >= next_report {
            println!("{}", status(&stream, &checker, started));
            next_report += options.report;
        }
    }
    let input = stream.input_stats().remove(0);
    let line = status(&stream, &checker, started);
    drop(stream.stop());
    println!("{line}");
    if let Some(reason) = ended {
        println!("Stopped early: {reason}.");
    }
    if checker.level < 1e-3 {
        println!("The input heard no tone. Is the cable plugged in?");
        return Ok(ExitCode::FAILURE);
    }
    let clean = checker.glitches.is_empty()
        && checker.missed == 0
        && input.lost == 0
        && input.discontinuities == 0
        && input.slips == 0
        && input.error.is_none()
        && ended.is_none();
    for glitch in checker.glitches.iter().take(10) {
        println!(
            "Glitch at {:.3} s of engine time, {} frames over {}",
            glitch.first as f64 / f64::from(rate),
            glitch.frames,
            ms(glitch.last - glitch.first + 1, rate),
        );
    }
    if clean {
        println!("Recorded {} without a glitch.", minutes(started.elapsed()));
        Ok(ExitCode::SUCCESS)
    } else {
        println!("Glitches!");
        Ok(ExitCode::FAILURE)
    }
}

fn status(stream: &Stream, checker: &Checker, started: Instant) -> String {
    let stats = stream.stats();
    let input = &stream.input_stats()[0];
    let rate = stream.info().sample_rate;
    format!(
        "{:>9.1} s  checked {} frames, glitches {}, missed {}; input: lost {}, slips {}, \
         discontinuities {}, {:?} clock, drift {:+.2} ppm; monitoring delay {}, late {} frames; \
         output dropouts {}",
        started.elapsed().as_secs_f64(),
        checker.checked,
        checker.glitches.len(),
        checker.missed,
        input.lost,
        input.slips,
        input.discontinuities,
        input.clock,
        input.drift_ppm,
        ms(stats.input_delay, rate),
        stats.input_late,
        stats.underruns,
    )
}

/// Checks a recorded tone for glitches: a sine changes smoothly, so how much
/// its slope changes from one frame to the next has a limit, and anything
/// that jumps, a frame lost or repeated or a click, goes over it.
struct Checker {
    /// How much the slope can change, for a tone at full scale.
    limit: f64,
    rate: u32,
    channel: Option<usize>,
    /// Frames recorded so far, before the check starts: the first second,
    /// in which the tone starts. The second half of it sets the channel
    /// and level.
    settling: u64,
    /// Each channel's loudest sample in the second half of the first
    /// second.
    peaks: Vec<f32>,
    /// The tone's level at the input.
    level: f64,
    /// The last two frames.
    last: Option<(f64, f64)>,
    checked: u64,
    /// Frames the buffer dropped before they were read.
    missed: u64,
    /// Where the check found a glitch: frames that went over the limit
    /// within 10 ms of each other count as one.
    glitches: Vec<Glitch>,
}

impl Checker {
    fn new(tone: f64, rate: u32, channel: Option<usize>) -> Checker {
        let step = std::f64::consts::PI * tone / f64::from(rate);
        Checker {
            limit: 4.0 * step.sin().powi(2),
            rate,
            channel,
            settling: 0,
            peaks: Vec::new(),
            level: 0.0,
            last: None,
            checked: 0,
            missed: 0,
            glitches: Vec::new(),
        }
    }

    /// Checks `samples`, `channels` interleaved, from engine frame `at`.
    fn check(&mut self, at: u64, samples: &[f32], channels: usize) {
        let second = u64::from(self.rate);
        for (i, frame) in samples.chunks_exact(channels).enumerate() {
            if self.settling < second {
                self.settling += 1;
                if self.settling > second / 2 {
                    self.peaks.resize(channels, 0.0);
                    for (peak, sample) in self.peaks.iter_mut().zip(frame) {
                        *peak = peak.max(sample.abs());
                    }
                }
                if self.settling == second {
                    let loudest = (0..channels)
                        .max_by(|&a, &b| self.peaks[a].total_cmp(&self.peaks[b]))
                        .unwrap_or(0);
                    let channel = *self.channel.get_or_insert(loudest);
                    self.level = f64::from(self.peaks.get(channel).copied().unwrap_or(0.0));
                }
                continue;
            }
            let Some(&sample) = frame.get(self.channel.unwrap_or(0)) else {
                continue;
            };
            let sample = f64::from(sample);
            if let Some((before, last)) = self.last {
                let change = sample - 2.0 * last + before;
                // Three times what the tone can do, and room for noise.
                if change.abs() > 3.0 * self.limit * self.level + 2e-3 {
                    self.glitch(at + i as u64);
                }
            }
            self.last = Some((self.last.map_or(sample, |(_, last)| last), sample));
            self.checked += 1;
        }
    }
}

impl Checker {
    fn glitch(&mut self, frame: u64) {
        match self.glitches.last_mut() {
            Some(glitch) if frame <= glitch.last + u64::from(self.rate) / 100 => {
                glitch.last = frame;
                glitch.frames += 1;
            }
            _ => self.glitches.push(Glitch {
                first: frame,
                last: frame,
                frames: 1,
            }),
        }
    }
}

/// Frames close together where the tone jumped.
struct Glitch {
    first: u64,
    last: u64,
    frames: u64,
}

fn ms(frames: u64, rate: u32) -> String {
    format!("{:.2} ms", frames as f64 * 1000.0 / f64::from(rate))
}

fn ms_signed(frames: f64, rate: u32) -> String {
    format!("{:+.3} ms", frames * 1000.0 / f64::from(rate))
}

fn minutes(duration: Duration) -> String {
    format!("{:.1} minutes", duration.as_secs_f64() / 60.0)
}

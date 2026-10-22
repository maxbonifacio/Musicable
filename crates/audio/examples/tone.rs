//! Plays a test tone and counts dropouts: the long-run check for a device and
//! the stream code.
//!
//! ```text
//! cargo run --release -p musicable-audio --example tone -- --period 128 --seconds 3600
//! cargo run --release -p musicable-audio --features asio --example tone -- --asio --seconds 3600
//! ```
//!
//! It prints the stream's counters every few seconds and a summary at the
//! end, and fails if there was a dropout it didn't cause on purpose with
//! `--stall-every`. When the device goes away, or an ASIO driver asks to be
//! reset, it opens the stream again once a second, as an app would. `--help`
//! lists the options.

use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};
use std::{env, thread};

use musicable_audio::{
    self as audio, Capabilities, Device, DriverType, Mode, Stopped, Stream, StreamInfo,
    StreamRequest, StreamStats,
};
use musicable_engine::nodes::Sine;
use musicable_engine::{
    AudioMut, Engine, EngineHandle, Event, Graph, Inputs, Node, NodeEventKind, NodeKey, ParamId,
    ProcessContext, TransportCommand,
};

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
Plays a test tone and counts dropouts.

Usage: tone [options]

  --list              list the output devices and what they support
  --device <text>     a device id, or part of a device name
                      (default: the default output)
  --asio              play through an ASIO driver: the one --device names,
                      or the only one installed (Windows builds with the
                      asio feature)
  --driver-settings   open the ASIO driver's own settings once it plays
  --exclusive         use the device in exclusive mode (Windows only)
  --rate <hz>         sample rate (default: the device's)
  --period <frames>   frames per period, the buffer size
                      (default: the smallest the device supports)
  --seconds <s>       how long to play (default: 10)
  --report <s>        seconds between status lines (default: 10)
  --level <0-1>       the tone's amplitude (default: 0.1)
  --stall-every <s>   hold up the audio thread every <s> seconds, to see the
                      dropouts it causes counted
  --stall-ms <ms>     how long each hold-up lasts (default: five periods)
";

const TONE: NodeKey = NodeKey(1);
const STALL: NodeKey = NodeKey(2);

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
    let result = if options.list { list() } else { play(&options) };
    result.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    })
}

struct Options {
    list: bool,
    device: Option<String>,
    asio: bool,
    driver_settings: bool,
    mode: Mode,
    rate: Option<u32>,
    period: Option<u32>,
    seconds: Duration,
    report: Duration,
    level: f32,
    stall_every: Option<Duration>,
    stall: Option<Duration>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Self {
            list: false,
            device: None,
            asio: false,
            driver_settings: false,
            mode: Mode::Shared,
            rate: None,
            period: None,
            seconds: Duration::from_secs(10),
            report: Duration::from_secs(10),
            level: 0.1,
            stall_every: None,
            stall: None,
        };
        let mut args = args.into_iter();
        while let Some(option) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{option} needs a value"));
            match option.as_str() {
                "--list" => options.list = true,
                "--asio" => options.asio = true,
                "--driver-settings" => options.driver_settings = true,
                "--exclusive" => options.mode = Mode::Exclusive,
                "--device" => options.device = Some(value()?),
                "--rate" => options.rate = Some(positive(&option, &value()?)?),
                "--period" => options.period = Some(positive(&option, &value()?)?),
                "--seconds" => options.seconds = duration(&option, &value()?, SECOND)?,
                "--report" => options.report = duration(&option, &value()?, SECOND)?,
                "--level" => {
                    let level: f32 = number(&option, &value()?)?;
                    if !(0.0..=1.0).contains(&level) {
                        return Err(format!("{option} is from 0 to 1"));
                    }
                    options.level = level;
                }
                "--stall-every" => {
                    options.stall_every = Some(duration(&option, &value()?, SECOND)?);
                }
                "--stall-ms" => options.stall = Some(duration(&option, &value()?, MILLISECOND)?),
                _ => return Err(format!("unknown option {option}")),
            }
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
const MILLISECOND: Duration = Duration::from_millis(1);

/// A positive number of `unit`s.
fn duration(option: &str, text: &str, unit: Duration) -> Result<Duration, String> {
    let count: f64 = number(option, text)?;
    Duration::try_from_secs_f64(count * unit.as_secs_f64())
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| format!("{option} must be more than 0"))
}

fn list() -> Result<ExitCode, String> {
    let devices = audio::output_devices().map_err(|e| e.to_string())?;
    if devices.is_empty() {
        println!("No output devices.");
    }
    for device in devices {
        let default = if device.is_default { " (default)" } else { "" };
        let asio = if device.driver_type == DriverType::Asio {
            " (ASIO driver)"
        } else {
            ""
        };
        println!("{}{default}{asio}", device.name);
        println!("  id: {}", device.id);
        match audio::capabilities(Some(&device.id)) {
            Ok(capabilities) => print_capabilities(&capabilities),
            Err(error) => println!("  {error}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_capabilities(capabilities: &Capabilities) {
    let rate = capabilities.sample_rate;
    let period = |frames: Option<u32>| match frames {
        Some(frames) => format!("{frames} frames ({})", ms(frames.into(), rate)),
        None => "set when the stream opens".to_owned(),
    };
    if let Some(asio) = &capabilities.asio {
        let list = |values: &[u32]| {
            let values: Vec<String> = values.iter().map(u32::to_string).collect();
            values.join(", ")
        };
        println!(
            "  {rate} Hz now, {} channels, preferred buffer {}",
            capabilities.channels,
            period(capabilities.default_period),
        );
        println!("  rates: {} Hz", list(&asio.sample_rates));
        println!("  buffer sizes: {} frames", list(&asio.buffer_sizes));
        let dropouts = if asio.reports_dropouts {
            "the driver reports its own, and a stream counts only those"
        } else {
            "a stream looks for the signs"
        };
        println!("  dropouts: {dropouts}");
        return;
    }
    println!(
        "  shared: {rate} Hz, {} channels, smallest period {}, default period {}",
        capabilities.channels,
        period(capabilities.min_period),
        period(capabilities.default_period),
    );
    match &capabilities.exclusive {
        Some(exclusive) => {
            let rates: Vec<String> = exclusive.sample_rates.iter().map(u32::to_string).collect();
            println!(
                "  exclusive: {} Hz, smallest period {}",
                rates.join(", "),
                period(Some(exclusive.min_period)),
            );
        }
        None => println!("  exclusive: no"),
    }
}

/// Finds the device `text` names: its id, or part of its name. With `asio`,
/// only ASIO drivers count, and with no `text`, the only one installed does.
/// `None` is the default output.
fn find_device(text: Option<&str>, asio: bool) -> Result<Option<String>, String> {
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
            [] if cfg!(windows) => {
                Err("this build has no ASIO: build it with --features asio".to_owned())
            }
            [] => Err("ASIO is for Windows only".to_owned()),
            _ => {
                let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
                Err(format!(
                    "pick one of the ASIO drivers with --device: {}",
                    names.join(", ")
                ))
            }
        };
    };
    if devices.iter().any(|device| device.id == text) {
        return Ok(Some(text.to_owned()));
    }
    let wanted = text.to_lowercase();
    let found: Vec<&Device> = devices
        .iter()
        .filter(|device| device.name.to_lowercase().contains(&wanted))
        .collect();
    match found.as_slice() {
        [device] => Ok(Some(device.id.clone())),
        [] => Err(format!("no output device matches \"{text}\" (see --list)")),
        _ => {
            let names: Vec<&str> = found.iter().map(|device| device.name.as_str()).collect();
            Err(format!("\"{text}\" matches {}", names.join(", ")))
        }
    }
}

fn play(options: &Options) -> Result<ExitCode, String> {
    let request = StreamRequest {
        device: find_device(options.device.as_deref(), options.asio)?,
        mode: options.mode,
        sample_rate: options.rate,
        period: options.period,
        inputs: Vec::new(),
        midi_inputs: Vec::new(),
    };
    let open = audio::open(&request).map_err(|e| e.to_string())?;
    print_info(open.info());
    let stall = options
        .stall
        .unwrap_or_else(|| open.info().period_duration() * 5);
    let (engine, handle) = new_engine(open.info(), options.level)?;
    let stream = open.start(engine).map_err(|e| e.to_string())?;
    if options.driver_settings && !stream.show_driver_settings() {
        println!("The device has no settings of its own: only ASIO drivers do.");
    }

    let mut run = Run {
        request,
        level: options.level,
        handle,
        stream: Some(stream),
        spare: None,
        ended: Totals::default(),
        streams: 1,
        stalls: 0,
        earlier_xruns: 0,
        last_error: None,
    };
    let started = Instant::now();
    let end = started + options.seconds;
    let mut next_report = started + options.report;
    let mut next_stall = options.stall_every.map(|every| started + every);
    let mut next_open = started;
    while Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
        run.handle.collect_garbage();
        let now = Instant::now();
        if run.stream.as_ref().is_some_and(Stream::has_ended) {
            run.end_stream(started);
            next_open = now;
        }
        if run.stream.is_none() && now >= next_open {
            run.reopen(started);
            next_open = now + Duration::from_secs(1);
        }
        if let Some(at) = next_stall.filter(|&at| now >= at) {
            // Not so near the end that the dropout can't be counted.
            if now + Duration::from_millis(500) < end {
                run.stall(stall);
            }
            next_stall = options.stall_every.map(|every| at + every);
        }
        if now >= next_report {
            println!("{}", run.status(started));
            next_report += options.report;
        }
    }
    Ok(run.finish(started))
}

fn print_info(info: &StreamInfo) {
    let rate = info.sample_rate;
    let default = if info.device.is_default {
        ", the default output"
    } else {
        ""
    };
    let priority = if info.realtime {
        "real-time priority"
    } else {
        "normal priority"
    };
    println!("{}{default}, {}", info.device.name, info.api);
    println!(
        "{rate} Hz, {} channels, {:?}, period {} frames ({}), buffer {} frames, \
         latency {} frames ({}), {priority}",
        info.channels,
        info.format,
        info.period,
        ms(info.period.into(), rate),
        info.buffer,
        info.latency,
        ms(info.latency.into(), rate),
    );
}

/// An engine for `info`'s stream, playing the tone.
fn new_engine(info: &StreamInfo, level: f32) -> Result<(Engine, EngineHandle), String> {
    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let tone = graph.add(TONE, Sine::new(440.0, level));
    let stall = graph.add(STALL, Stall);
    graph.connect(tone, stall).map_err(|e| format!("{e:?}"))?;
    graph.set_output(stall).map_err(|e| format!("{e:?}"))?;
    let graph = graph.prepare(&config).map_err(|e| format!("{e:?}"))?;
    handle.swap_graph(graph).map_err(|e| format!("{e:?}"))?;
    handle
        .send(Event::Transport(TransportCommand::Play))
        .map_err(|e| format!("{e:?}"))?;
    Ok((engine, handle))
}

/// The run so far, across the streams it took if the device went away.
struct Run {
    request: StreamRequest,
    level: f32,
    handle: EngineHandle,
    stream: Option<Stream>,
    /// The engine while no stream plays it.
    spare: Option<Engine>,
    /// The counters of the streams that ended.
    ended: Totals,
    streams: u32,
    /// Stalls caused on purpose.
    stalls: u64,
    /// Xruns the engines that were replaced counted.
    earlier_xruns: u64,
    /// The last reason reopening failed, so it's printed once.
    last_error: Option<String>,
}

impl Run {
    fn end_stream(&mut self, started: Instant) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        self.ended.add(&stream.stats(), stream.info().sample_rate);
        let Stopped { engine, error } = stream.stop();
        let reason = error.map_or_else(|| "no reason given".to_owned(), |e| e.to_string());
        println!("{}  the stream ended: {reason}", elapsed(started));
        self.spare = Some(engine);
    }

    fn reopen(&mut self, started: Instant) {
        match self.try_reopen() {
            Ok(stream) => {
                println!("{}  playing again", elapsed(started));
                print_info(stream.info());
                self.stream = Some(stream);
                self.streams += 1;
                self.last_error = None;
            }
            Err(error) => {
                if self.last_error.as_ref() != Some(&error) {
                    println!("{}  can't open the device yet: {error}", elapsed(started));
                    self.last_error = Some(error);
                }
            }
        }
    }

    fn try_reopen(&mut self) -> Result<Stream, String> {
        let open = audio::open(&self.request).map_err(|e| e.to_string())?;
        let engine = match self.spare.take() {
            Some(engine) if engine.config().sample_rate == open.info().sample_rate => engine,
            // A new sample rate needs a new engine. The old one is dropped
            // here, off the audio thread.
            _ => {
                let (engine, handle) = new_engine(open.info(), self.level)?;
                self.earlier_xruns += self.handle.stats().xruns;
                self.handle = handle;
                engine
            }
        };
        open.start(engine).map_err(|failed| {
            let message = failed.to_string();
            self.spare = Some(*failed.engine);
            message
        })
    }

    fn stall(&mut self, length: Duration) {
        // The graph is never swapped after the start, so the handle never
        // sends this parameter again.
        let event = Event::Param {
            node: STALL,
            param: Stall::MILLIS,
            value: length.as_secs_f64() * 1000.0,
        };
        if self.stream.is_some() && self.handle.send(event).is_ok() {
            self.stalls += 1;
        }
    }

    /// The counters so far.
    fn totals(&self) -> Totals {
        let mut totals = self.ended;
        if let Some(stream) = &self.stream {
            totals.add(&stream.stats(), stream.info().sample_rate);
        }
        totals
    }

    fn status(&self, started: Instant) -> String {
        let totals = self.totals();
        let queued = match &self.stream {
            Some(stream) => {
                let info = stream.info();
                let queued = stream.stats().queued;
                format!("queued {queued} frames ({})", ms(queued, info.sample_rate))
            }
            None => "not playing".to_owned(),
        };
        format!(
            "{}  dropouts {} {}, late wake-ups {}, longest gap {}, longest wake-up {}, {queued}",
            elapsed(started),
            totals.underruns,
            totals.breakdown(),
            totals.late_wakeups,
            ms_of(totals.max_interval),
            ms_of(totals.max_busy),
        )
    }

    fn finish(mut self, started: Instant) -> ExitCode {
        if let Some(stream) = self.stream.take() {
            self.ended.add(&stream.stats(), stream.info().sample_rate);
            drop(stream.stop());
        }
        let totals = self.ended;
        let xruns = self.earlier_xruns + self.handle.stats().xruns;
        println!(
            "\nPlayed for {} through {} stream{}: {} wake-ups, {} dropouts {}, \
             {} late wake-ups, longest gap {}, longest wake-up {}. The engine counted {xruns} \
             xruns.",
            elapsed(started).trim_start(),
            self.streams,
            if self.streams == 1 { "" } else { "s" },
            totals.wakeups,
            totals.underruns,
            totals.breakdown(),
            totals.late_wakeups,
            ms_of(totals.max_interval),
            ms_of(totals.max_busy),
        );
        if totals.system > 0 {
            println!(
                "{} of the dropouts happened in the system's audio engine or driver, with the \
                 stream's audio still queued. Each one moved the output latency by the audio it \
                 lost.",
                totals.system
            );
        }
        if self.stalls > 0 {
            println!(
                "Stalled the audio thread {} times, so {} dropouts were expected.",
                self.stalls, self.stalls
            );
        }
        if totals.underruns == self.stalls {
            println!("No unexpected dropouts.");
            ExitCode::SUCCESS
        } else {
            println!("Unexpected dropouts!");
            ExitCode::FAILURE
        }
    }
}

/// Counters added up across streams.
#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    wakeups: u64,
    underruns: u64,
    /// The dropouts in the system's audio engine or driver.
    system: u64,
    lost: Duration,
    late_wakeups: u64,
    max_interval: Duration,
    max_busy: Duration,
}

impl Totals {
    fn add(&mut self, stats: &StreamStats, rate: u32) {
        self.wakeups += stats.wakeups;
        self.underruns += stats.underruns;
        self.system += stats.system_dropouts;
        self.lost += Duration::from_secs_f64(stats.lost_frames as f64 / f64::from(rate));
        self.late_wakeups += stats.late_wakeups;
        self.max_interval = self.max_interval.max(stats.max_interval);
        self.max_busy = self.max_busy.max(stats.max_busy);
    }

    /// What the dropouts lost, and how many were in the system, in
    /// brackets.
    fn breakdown(&self) -> String {
        if self.system > 0 {
            format!("({} in the system, {} lost)", self.system, ms_of(self.lost))
        } else {
            format!("({} lost)", ms_of(self.lost))
        }
    }
}

/// Passes its input through, and holds up the audio thread for as long as
/// each `MILLIS` event says, like a render that takes too long.
struct Stall;

impl Stall {
    const MILLIS: ParamId = ParamId(0);
}

impl Node for Stall {
    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        match inputs.get(0) {
            Some(input) => output.copy_from(input),
            None => output.clear(),
        }
        for event in ctx.events {
            if let NodeEventKind::Param {
                param: Self::MILLIS,
                value,
            } = event.kind
            {
                let length = Duration::try_from_secs_f64(value / 1000.0).unwrap_or_default();
                let until = Instant::now() + length;
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
        }
    }
}

fn elapsed(started: Instant) -> String {
    format!("{:>9.1} s", started.elapsed().as_secs_f64())
}

fn ms(frames: u64, rate: u32) -> String {
    format!("{:.2} ms", frames as f64 * 1000.0 / f64::from(rate))
}

fn ms_of(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

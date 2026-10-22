//! Plays from MIDI inputs: a click for each note played, heard through the
//! output a fixed delay later, and each message printed with the engine
//! frame heard as it came in. The check that MIDI inputs open, line up and
//! keep their timing.
//!
//! ```text
//! cargo run --release -p musicable-audio --example midi_inputs -- --list
//! cargo run --release -p musicable-audio --example midi_inputs -- --seconds 60
//! ```
//!
//! `--help` lists the options. Windows only so far: elsewhere MIDI inputs
//! don't open.

use std::f32::consts::TAU;
use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};
use std::{env, thread};

use musicable_audio::{self as audio, MidiReader, StreamInfo, StreamRequest};
use musicable_engine::{
    AudioMut, Engine, EngineHandle, Graph, Inputs, Node, NodeKey, ProcessContext,
};

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
Plays a click for each note a MIDI input plays, and prints what comes in.

Usage: midi_inputs [options]

  --list              list the MIDI inputs and output devices
  --input <text>      a MIDI input id, or part of its name; give it again for
                      more (default: every MIDI input)
  --device <text>     an output device id, or part of its name
                      (default: the default output)
  --period <frames>   frames per period, the buffer size
                      (default: the smallest the device supports)
  --seconds <s>       how long to play (default: 30)
  --level <0-1>       the clicks' amplitude (default: 0.3)
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
    let result = if options.list { list() } else { play(&options) };
    result.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    })
}

struct Options {
    list: bool,
    inputs: Vec<String>,
    device: Option<String>,
    period: Option<u32>,
    seconds: f64,
    level: f32,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Self {
            list: false,
            inputs: Vec::new(),
            device: None,
            period: None,
            seconds: 30.0,
            level: 0.3,
        };
        let mut args = args.into_iter();
        while let Some(option) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{option} needs a value"));
            match option.as_str() {
                "--list" => options.list = true,
                "--input" => options.inputs.push(value()?),
                "--device" => options.device = Some(value()?),
                "--period" => options.period = Some(number(&option, &value()?)?),
                "--seconds" => options.seconds = number(&option, &value()?)?,
                "--level" => options.level = number(&option, &value()?)?,
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

fn list() -> Result<ExitCode, String> {
    let inputs = audio::midi_input_devices().map_err(|e| e.to_string())?;
    match inputs.is_empty() {
        true => println!("No MIDI inputs."),
        false => println!("MIDI inputs:"),
    }
    for input in inputs {
        println!("  {} (id: {})", input.name, input.id);
    }
    println!("Output devices:");
    for device in audio::output_devices().map_err(|e| e.to_string())? {
        let default = if device.is_default { " (default)" } else { "" };
        println!("  {}{default} (id: {})", device.name, device.id);
    }
    Ok(ExitCode::SUCCESS)
}

/// The ids among `ids` (with their `names`) that `wanted` names, by id or
/// part of the name: all of them if it names none.
fn pick(ids: &[(String, String)], wanted: &[String], what: &str) -> Result<Vec<String>, String> {
    if wanted.is_empty() {
        return Ok(ids.iter().map(|(id, _)| id.clone()).collect());
    }
    let mut picked = Vec::new();
    for text in wanted {
        let lower = text.to_lowercase();
        let found: Vec<&String> = match ids.iter().find(|(id, _)| id == text) {
            Some((id, _)) => vec![id],
            None => ids
                .iter()
                .filter(|(_, name)| name.to_lowercase().contains(&lower))
                .map(|(id, _)| id)
                .collect(),
        };
        match found.as_slice() {
            [id] => picked.push((*id).clone()),
            [] => return Err(format!("no {what} matches \"{text}\" (see --list)")),
            _ => {
                let ids: Vec<&str> = found.iter().map(|id| id.as_str()).collect();
                return Err(format!("\"{text}\" matches {}", ids.join(", ")));
            }
        }
    }
    Ok(picked)
}

fn play(options: &Options) -> Result<ExitCode, String> {
    let midi: Vec<(String, String)> = audio::midi_input_devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|input| (input.id, input.name))
        .collect();
    if midi.is_empty() {
        return Err("no MIDI inputs are plugged in".to_owned());
    }
    let outputs: Vec<(String, String)> = audio::output_devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|device| (device.id, device.name))
        .collect();
    let device = match &options.device {
        Some(text) => pick(&outputs, std::slice::from_ref(text), "output device")?.pop(),
        None => None,
    };
    let request = StreamRequest {
        device,
        period: options.period,
        midi_inputs: pick(&midi, &options.inputs, "MIDI input")?,
        ..StreamRequest::default()
    };
    let open = audio::open(&request).map_err(|e| e.to_string())?;
    let info = open.info().clone();
    print_info(&info);
    let buffers = open.midi_buffers();
    let (engine, mut handle) = new_engine(&info, options.level)?;
    let stream = open.start(engine).map_err(|e| e.to_string())?;
    // A reader for each input that opened, with its name.
    let mut readers: Vec<(String, MidiReader)> = (info.midi_inputs.iter())
        .zip(&buffers)
        .filter_map(|(input, buffer)| Some((input.name.clone(), buffer.as_ref()?.reader())))
        .collect();
    if readers.is_empty() {
        return Err("none of the MIDI inputs opened".to_owned());
    }
    println!("Play something. Each note on clicks.");
    let rate = info.sample_rate;
    let started = Instant::now();
    let mut last = None;
    while started.elapsed().as_secs_f64() < options.seconds && !stream.has_ended() {
        thread::sleep(Duration::from_millis(5));
        handle.collect_garbage();
        for (name, reader) in &mut readers {
            while let Some(event) = reader.next_before(u64::MAX) {
                let since = match last.replace(event.frame) {
                    Some(last) => format!("+{}", ms(event.frame.saturating_sub(last), rate)),
                    None => String::new(),
                };
                let [status, a, b] = event.bytes;
                println!(
                    "frame {:>10} {since:>12}  {status:02X} {a:02X} {b:02X}  {}  ({name})",
                    event.frame,
                    describe(event.bytes),
                );
            }
        }
    }
    let stats = stream.stats();
    let lost: u64 = readers.iter().map(|(_, reader)| reader.lost()).sum();
    let ended = stream.has_ended();
    let stopped = stream.stop();
    println!(
        "MIDI delay {} frames ({}), {} messages late, {lost} lost to the readers, {} underruns",
        stats.midi_delay,
        ms(stats.midi_delay, rate),
        stats.midi_late,
        stats.underruns,
    );
    if ended {
        let error = stopped.error.map(|e| e.to_string()).unwrap_or_default();
        return Err(format!("the stream ended: {error}"));
    }
    Ok(ExitCode::SUCCESS)
}

fn print_info(info: &StreamInfo) {
    let rate = info.sample_rate;
    println!(
        "{}, {}: {rate} Hz, period {} frames ({}), latency {} frames ({})",
        info.device.name,
        info.api,
        info.period,
        ms(info.period.into(), rate),
        info.latency,
        ms(info.latency.into(), rate),
    );
    for input in &info.midi_inputs {
        match &input.error {
            None => println!("MIDI input {}: open", input.name),
            Some(error) => println!("MIDI input {}: didn't open: {error}", input.id),
        }
    }
}

/// What a channel message does, in words.
fn describe([status, a, b]: [u8; 3]) -> String {
    let channel = status & 0x0F;
    match status & 0xF0 {
        0x90 if b > 0 => format!("note on {a}, velocity {b}, channel {}", channel + 1),
        0x80 | 0x90 => format!("note off {a}, channel {}", channel + 1),
        0xA0 => format!("aftertouch {a}: {b}, channel {}", channel + 1),
        0xB0 => format!("controller {a}: {b}, channel {}", channel + 1),
        0xC0 => format!("program {a}, channel {}", channel + 1),
        0xD0 => format!("channel pressure {a}, channel {}", channel + 1),
        0xE0 => {
            let bend = ((i32::from(b) << 7) | i32::from(a)) - 8192;
            format!("pitch bend {bend}, channel {}", channel + 1)
        }
        _ => String::new(),
    }
}

/// An engine for `info`'s stream, clicking on each note on.
fn new_engine(info: &StreamInfo, level: f32) -> Result<(Engine, EngineHandle), String> {
    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let clicks = graph.add(NodeKey(1), Clicks::new(info.sample_rate, level));
    graph.set_output(clicks).map_err(|e| format!("{e:?}"))?;
    let graph = graph.prepare(&config).map_err(|e| format!("{e:?}"))?;
    handle.swap_graph(graph).map_err(|e| format!("{e:?}"))?;
    Ok((engine, handle))
}

/// A short, decaying 1 kHz click on the frame each note on plays at.
struct Clicks {
    rate: f32,
    level: f32,
    /// The click's length, in frames.
    length: usize,
    /// How far into its click it is, if it's clicking.
    age: Option<usize>,
}

impl Clicks {
    fn new(rate: u32, level: f32) -> Clicks {
        Clicks {
            rate: rate as f32,
            level,
            length: rate as usize / 50,
            age: None,
        }
    }
}

impl Node for Clicks {
    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        let mut starts = (ctx.midi.iter())
            .filter(|message| message.bytes[0] & 0xF0 == 0x90 && message.bytes[2] > 0)
            .map(|message| message.time.saturating_sub(ctx.time) as usize)
            .peekable();
        for i in 0..ctx.frames {
            while starts.next_if(|&at| at <= i).is_some() {
                self.age = Some(0);
            }
            let sample = match self.age {
                Some(age) if age < self.length => {
                    self.age = Some(age + 1);
                    let fade = 1.0 - age as f32 / self.length as f32;
                    (TAU * 1000.0 * age as f32 / self.rate).sin() * fade * fade * self.level
                }
                _ => {
                    self.age = None;
                    0.0
                }
            };
            for c in 0..output.channels() {
                output.channel_mut(c)[i] = sample;
            }
        }
    }
}

fn ms(frames: u64, rate: u32) -> String {
    format!("{:.2} ms", frames as f64 * 1000.0 / f64::from(rate))
}

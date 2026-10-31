//! audio.generate, which writes test signals whose answers are known:
//! sines, sweeps, white and pink noise, clicks at a tempo, an impulse and
//! silence.

use std::f64::consts::TAU;

use musicable_commands::{Args, Param, Type};
use musicable_io::audio::{Format, SampleFormat};
use serde_json::{Value as Json, json};

use super::files::{ENCODINGS, sample_format_named};
use super::{AudioCommand, CHUNK, Job, OVERWRITE, Output, Share, arg, describe, flag, opt, schema};

/// The signals.
const SIGNALS: &[&str] = &[
    "sine", "sweep", "noise", "pink", "clicks", "impulse", "silence",
];
/// The longest signal, in seconds.
const LONGEST: f64 = 3600.0;
/// A click is a tone of this pitch that starts at its peak and dies away
/// with this time constant, for this long.
const CLICK_HZ: f64 = 2000.0;
const CLICK_DECAY: f64 = 0.001;
const CLICK_SECONDS: f64 = 0.010;

pub(super) fn commands() -> Vec<AudioCommand> {
    vec![generate()]
}

fn generate() -> AudioCommand {
    AudioCommand {
        id: "audio.generate",
        title: "Generate Test Signal",
        about: "Writes a test signal: a sine; a sweep, rising exponentially from one pitch to \
                another; white or pink noise; clicks at a tempo, each a 2 kHz tone that starts \
                at its peak on the beat and dies away in about 5 ms; an impulse, a single \
                sample at the start; or silence. Each channel's noise is its own, and the same \
                for the same seed. Sines, sweeps, clicks and impulses peak at level_dbfs, and \
                noise has it as its RMS level.",
        params: vec![
            Param::required(
                "out",
                Type::Text,
                "Where to write the new file, by its path. Its extension says the format: \
                 .wav, .aif or .flac.",
            ),
            Param::required("signal", Type::Choice(SIGNALS), "The signal to write."),
            Param::optional(
                "seconds",
                Type::Number,
                "How long it is, in seconds, up to 3600. Without it, 1.",
            ),
            Param::optional(
                "level_dbfs",
                Type::Number,
                "Its peak, or noise's RMS level, in dBFS. Without it, -12.",
            ),
            Param::optional(
                "hz",
                Type::Number,
                "A sine's pitch, in hertz. Without it, 1000.",
            ),
            Param::optional(
                "from_hz",
                Type::Number,
                "Where a sweep starts, in hertz. Without it, 20.",
            ),
            Param::optional(
                "to_hz",
                Type::Number,
                "Where a sweep ends, in hertz. Without it, 20000, or just below half the \
                 sample rate.",
            ),
            Param::optional(
                "bpm",
                Type::Number,
                "The clicks' tempo, in beats a minute, the first on the first frame. Without \
                 it, 120.",
            ),
            Param::optional(
                "rate",
                Type::Int,
                "The sample rate, in hertz, from 1000 to 768000. Without it, 48000.",
            ),
            Param::optional(
                "channels",
                Type::Int,
                "How many channels, from 1 to 8. Without it, 2.",
            ),
            Param::optional(
                "encoding",
                Type::Choice(ENCODINGS),
                "How to store the samples: int8 to int32 for integers of that many bits, or \
                 float32 or float64. Without it, float32, or int24 in a FLAC file.",
            ),
            Param::optional(
                "seed",
                Type::Int,
                "The noise's seed: the same seed makes the same noise. Without it, 1.",
            ),
            Param::optional(
                "dither",
                Type::Bool,
                "Dithers samples written as integers, so what rounding loses is noise, not \
                 distortion. Without it, false, so silence stays silent.",
            ),
            OVERWRITE,
        ],
        positional: &["out"],
        read_only: false,
        example: &["sweep.wav", "--signal", "sweep", "--seconds", "0.5"],
        run: run_generate,
        output: || {
            schema::object(
                "The signal written.",
                &[
                    ("signal", schema::choice(SIGNALS, "Which signal it is.")),
                    ("out", schema::file("The new file.")),
                ],
            )
        },
    }
}

/// Random numbers from -1 to 1, the same on every system for a seed:
/// xorshift64*.
struct Noise(u64);

impl Noise {
    fn new(seed: u64) -> Self {
        // SplitMix64, so nearby seeds start far apart, and none is 0.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        bits as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Paul Kellet's filter, which turns white noise pink, to within 0.05 dB
/// from 9.2 Hz up at 44.1 kHz.
#[derive(Default)]
struct Pink([f64; 7]);

impl Pink {
    fn next(&mut self, white: f64) -> f64 {
        let b = &mut self.0;
        b[0] = 0.99886 * b[0] + white * 0.0555179;
        b[1] = 0.99332 * b[1] + white * 0.0750759;
        b[2] = 0.96900 * b[2] + white * 0.1538520;
        b[3] = 0.86650 * b[3] + white * 0.3104856;
        b[4] = 0.55000 * b[4] + white * 0.5329522;
        b[5] = -0.7616 * b[5] - white * 0.0168980;
        let pink = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + white * 0.5362;
        b[6] = white * 0.115926;
        pink
    }
}

/// A signal, a frame at a time.
struct Signal {
    kind: &'static str,
    rate: f64,
    level: f64,
    hz: f64,
    from_hz: f64,
    to_hz: f64,
    frames: u64,
    beat: f64,
    noises: Vec<Noise>,
    pinks: Vec<Pink>,
    at: u64,
}

impl Signal {
    /// Fills `out` with the next frames, `channels` to a frame.
    fn fill(&mut self, out: &mut [f64], channels: usize) {
        for frame in out.chunks_exact_mut(channels) {
            let n = self.at;
            let t = n as f64 / self.rate;
            match self.kind {
                "noise" | "pink" => {
                    for (c, sample) in frame.iter_mut().enumerate() {
                        let white = self.noises[c].next();
                        *sample = match self.kind {
                            "pink" => self.pinks[c].next(white),
                            _ => white,
                        };
                    }
                }
                kind => {
                    let sample = match kind {
                        "sine" => self.level * (TAU * self.hz * t).sin(),
                        "sweep" => {
                            let length = self.frames as f64 / self.rate;
                            let ratio = (self.to_hz / self.from_hz).ln();
                            let phase = TAU * self.from_hz * length / ratio
                                * ((t / length * ratio).exp() - 1.0);
                            self.level * phase.sin()
                        }
                        "clicks" => {
                            // The last beat's frame, rounded, as the grid has it.
                            let frames = self.beat * self.rate;
                            let start = |k: f64| (k * frames).round() as u64;
                            let mut k = (n as f64 / frames).floor();
                            if start(k) > n {
                                k -= 1.0;
                            }
                            let since = (n - start(k)) as f64 / self.rate;
                            match since < CLICK_SECONDS {
                                true => {
                                    self.level
                                        * (-since / CLICK_DECAY).exp()
                                        * (TAU * CLICK_HZ * since).cos()
                                }
                                false => 0.0,
                            }
                        }
                        "impulse" if n == 0 => self.level,
                        _ => 0.0,
                    };
                    frame.fill(sample);
                }
            }
            self.at += 1;
        }
    }
}

fn run_generate(args: &Args, job: &Job) -> Result<Json, String> {
    let out = job.file(args, "out")?;
    let format = Output::check(&out, flag(args, "overwrite")?, &[])?;
    let signal: String = arg(args, "signal")?;
    let kind = *SIGNALS
        .iter()
        .find(|name| **name == signal)
        .expect("the signal is one of the signals");
    let seconds = opt::<f64>(args, "seconds")?.unwrap_or(1.0);
    if !(seconds > 0.0 && seconds <= LONGEST) {
        return Err(format!(
            "seconds is more than 0 and at most {LONGEST}, not {seconds}."
        ));
    }
    let rate = match opt::<i64>(args, "rate")? {
        None => 48_000,
        Some(rate @ 1_000..=768_000) => rate as u32,
        Some(_) => return Err("rate is a sample rate in hertz, from 1000 to 768000.".to_owned()),
    };
    let channels = match opt::<i64>(args, "channels")? {
        None => 2,
        Some(n @ 1..=8) => n as usize,
        Some(n) => return Err(format!("channels is from 1 to 8, not {n}.")),
    };
    let nyquist = f64::from(rate) / 2.0;
    let pitch = |name: &str, default: f64| -> Result<f64, String> {
        let hz = opt::<f64>(args, name)?.unwrap_or(default);
        match hz > 0.0 && hz < nyquist {
            true => Ok(hz),
            false => Err(format!(
                "{name} is above 0 and below half the sample rate, {nyquist} Hz, not {hz}."
            )),
        }
    };
    let hz = pitch("hz", 1000.0)?;
    let from_hz = pitch("from_hz", 20.0)?;
    let to_hz = pitch("to_hz", 20_000f64.min(nyquist * 0.99))?;
    if kind == "sweep" && to_hz == from_hz {
        return Err("A sweep needs from_hz and to_hz to differ.".to_owned());
    }
    let bpm = opt::<f64>(args, "bpm")?.unwrap_or(120.0);
    if !(bpm > 0.0 && bpm <= 1000.0) {
        return Err(format!("bpm is above 0 and at most 1000, not {bpm}."));
    }
    let level_dbfs = opt::<f64>(args, "level_dbfs")?.unwrap_or(-12.0);
    if !(-200.0..=20.0).contains(&level_dbfs) {
        return Err(format!("level_dbfs is from -200 to 20, not {level_dbfs}."));
    }
    let sample_format = match opt::<String>(args, "encoding")? {
        Some(name) => sample_format_named(&name),
        None if format == Format::Flac => SampleFormat::Int24,
        None => SampleFormat::Float32,
    };
    if format == Format::Flac && (sample_format.is_float() || sample_format.bits() > 24) {
        return Err(format!(
            "FLAC files hold integers of up to 24 bits, not {} samples: write a WAV or AIFF \
             file, or give another encoding.",
            sample_format.name()
        ));
    }
    let seed = opt::<i64>(args, "seed")?.unwrap_or(1) as u64;
    let frames = (seconds * f64::from(rate)).round().max(1.0) as u64;
    let level = 10f64.powf(level_dbfs / 20.0);
    let make = || Signal {
        kind,
        rate: f64::from(rate),
        level,
        hz,
        from_hz,
        to_hz,
        frames,
        beat: 60.0 / bpm,
        noises: (0..channels as u64)
            .map(|c| Noise::new(seed.wrapping_mul(1_000_003).wrapping_add(c)))
            .collect(),
        pinks: (0..channels).map(|_| Pink::default()).collect(),
        at: 0,
    };
    let mut buffer = vec![0.0; CHUNK.min(frames as usize) * channels];
    // Noise has the level as its RMS: the same noise, measured first.
    let scale = match kind {
        "noise" | "pink" => {
            let mut signal = make();
            let (mut sum, mut left) = (0.0, frames);
            while left > 0 {
                job.check()?;
                let now = left.min(CHUNK as u64) as usize;
                signal.fill(&mut buffer[..now * channels], channels);
                sum += buffer[..now * channels].iter().map(|v| v * v).sum::<f64>();
                left -= now as u64;
            }
            let rms = (sum / (frames as f64 * channels as f64)).sqrt();
            if rms > 0.0 { level / rms } else { 0.0 }
        }
        _ => 1.0,
    };
    let dither = !sample_format.is_float() && opt(args, "dither")?.unwrap_or(false);
    let mut output = Output::create(&out, format, sample_format, rate, channels, dither)?;
    let mut signal = make();
    let mut left = frames;
    let share = Share::ALL.between(0.0, 0.9);
    while left > 0 {
        job.check()?;
        let now = left.min(CHUNK as u64) as usize;
        let samples = &mut buffer[..now * channels];
        signal.fill(samples, channels);
        if scale != 1.0 {
            samples.iter_mut().for_each(|sample| *sample *= scale);
        }
        output.write(samples)?;
        left -= now as u64;
        job.report(share, 1.0 - left as f64 / frames as f64);
    }
    let out = output.finish()?;
    let job = job.to_the_end();
    Ok(json!({
        "signal": kind,
        "out": describe(&out, &job, Share::ALL.between(0.9, 1.0))?,
    }))
}

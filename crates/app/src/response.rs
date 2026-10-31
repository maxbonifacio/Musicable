//! Measuring a device's response, for checks: `debug.measure_response`.
//!
//! A probe plays a known signal through a copy of a device, offline, and
//! measures what comes out against what went in. The signal is a WAV file
//! in a folder of its own, played by two audio tracks of a project made for
//! it: Dry, with no devices, and Wet, with the copy. An export renders the
//! two as stems, as the app plays them ([`Export`]), with plugins' latency
//! made up for and plugins of its own, so the device in the project isn't
//! disturbed. Measuring Wet against Dry takes out everything but the
//! device, such as the faders.
//!
//! The signal has three parts:
//!
//! - **Noise.** White noise at the level asked for, an RMS level. Its
//!   transfer function (H1: the cross-spectrum over the dry spectrum,
//!   averaged over Hann windows of about 0.3 s that overlap by half) gives
//!   the gain and phase at each frequency, and the coherence, which falls
//!   where the output isn't the input filtered: distortion, noise, or a
//!   level that moves. The peak of its impulse response gives any delay
//!   left once latency is made up for, which a plugin reporting the wrong
//!   latency shows as.
//! - **Steps.** A 1 kHz sine whose peak climbs from -60 to 0 dBFS in 6 dB
//!   steps of half a second, each measured over its last 0.2 s: the static
//!   curve of a compressor, gate or saturator.
//! - **A jump.** The sine at -40 dBFS for a second, at -10 for a second, and
//!   back at -40 for two. The gain, measured every 2 ms, gives the attack
//!   and release times: how long the gain takes to get 90% of the way to
//!   where it settles.
//!
//! The copy plays on its own: wires into its side inputs, a sidechain's
//! among them, and modulators and lanes that move it don't come with it.

use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use musicable_engine::EngineConfig;
use musicable_io::audio::{self, Decoder, SampleFormat};
use musicable_model::clip::{AudioClip, ClipContent};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceId, Project, TrackKind};
use musicable_plugins::{PluginInfo, Waker};
use realfft::RealFftPlanner;
use realfft::num_complex::Complex64;
use serde_json::{Value as Json, json};

use crate::export::{Bits, Export, ExportSettings, FileFormat, Job};
use crate::toolkit::spectra;

/// The noise's RMS level unless asked otherwise, in dBFS.
pub const LEVEL: f64 = -18.0;
/// The quietest and loudest noise.
pub const LEVELS: (f64, f64) = (-60.0, -6.0);

/// The noise, in seconds, and how much of its start is left out, while the
/// device settles.
const NOISE: f64 = 4.0;
const SETTLE: f64 = 0.5;
/// Silence between the parts, for tails to die away in.
const GAP: f64 = 0.5;
/// The sine's frequency.
const SINE_HZ: f64 = 1000.0;
/// The steps' peaks, in dBFS, each held this long and measured over this
/// much of its end.
const STEPS: [f64; 11] = [
    -60.0, -54.0, -48.0, -42.0, -36.0, -30.0, -24.0, -18.0, -12.0, -6.0, 0.0,
];
const STEP: f64 = 0.5;
const STEP_MEASURED: f64 = 0.2;
/// The jump's levels, peaks in dBFS, and how long each is held.
const JUMP: [(f64, f64); 3] = [(-40.0, 1.0), (-10.0, 1.0), (-40.0, 2.0)];
/// The jump's gain is measured over windows this long, and has settled
/// over this much of each level's end.
const ENVELOPE: f64 = 0.002;
const SETTLED: f64 = 0.1;
/// A gain that moves less than this has no attack or release to time.
const MOVES_DB: f64 = 0.5;
/// How far to the gain where it settles counts as there.
const THERE: f64 = 0.9;
/// The spectra's windows are about this long.
const WINDOW_SECONDS: f64 = 0.3;
/// The range the gain is followed over, at most.
const AUDIBLE: (f64, f64) = (20.0, 20_000.0);
/// The noise's seed, so every run measures with the same noise.
const SEED: u64 = 0x6D75_7369_6361_626C;

/// Where each part of the signal is, in frames.
#[derive(Debug, Clone)]
struct Layout {
    rate: u32,
    /// The noise measured.
    noise: (usize, usize),
    /// Each step's stretch measured.
    steps: Vec<(usize, usize)>,
    /// The jump's three levels.
    jump: [(usize, usize); 3],
    frames: usize,
}

impl Layout {
    fn new(rate: u32) -> Self {
        let frames = |seconds: f64| (seconds * f64::from(rate)).round() as usize;
        let mut at = frames(NOISE);
        let noise = (frames(SETTLE), at);
        at += frames(GAP);
        let steps = (STEPS.iter())
            .map(|_| {
                let end = at + frames(STEP);
                at = end;
                (end - frames(STEP_MEASURED), end)
            })
            .collect();
        at += frames(GAP);
        let jump = JUMP.map(|(_, seconds)| {
            let start = at;
            at += frames(seconds);
            (start, at)
        });
        Self {
            rate,
            noise,
            steps,
            jump,
            frames: at + frames(GAP),
        }
    }

    /// The signal, with the noise at `level`, an RMS level in dBFS.
    fn signal(&self, level: f64) -> Vec<f32> {
        let mut signal = vec![0.0f32; self.frames];
        let mut random = Random(SEED);
        let rms = 10f64.powf(level / 20.0);
        for sample in &mut signal[..self.noise.1] {
            *sample = (random.normal() * rms) as f32;
        }
        let rate = f64::from(self.rate);
        let mut sine = |from: usize, to: usize, peak: f64| {
            let amplitude = 10f64.powf(peak / 20.0);
            for (n, sample) in signal.iter_mut().enumerate().take(to).skip(from) {
                *sample = (amplitude * (TAU * SINE_HZ * n as f64 / rate).sin()) as f32;
            }
        };
        let step = (STEP * rate).round() as usize;
        for (&(_, end), &peak) in self.steps.iter().zip(&STEPS) {
            sine(end - step, end, peak);
        }
        for (&(from, to), &(peak, _)) in self.jump.iter().zip(&JUMP) {
            sine(from, to, peak);
        }
        signal
    }
}

/// xorshift64*, for noise that's the same on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        (bits as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// A normal deviate: Box and Muller's.
    fn normal(&mut self) -> f64 {
        let (u, v) = (self.next(), self.next());
        (-2.0 * u.ln()).sqrt() * (TAU * v).cos()
    }
}

/// What to measure: the device, the noise's level, and frequencies to give
/// the gain and phase at.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked {
    pub device: DeviceId,
    pub level: f64,
    pub at: Vec<f64>,
}

/// A measurement under way. [`start`](Self::start) it, then
/// [`tick`](Self::tick) it often, as an export, until it answers.
/// Dropping it cancels it, and its files go.
pub struct Probe {
    export: Export,
    folder: PathBuf,
    stems: [PathBuf; 2],
    layout: Layout,
    asked: Asked,
    name: String,
}

impl Probe {
    /// Starts measuring `asked.device` of `project`, as it's saved, at
    /// `config`'s sample rate, with plugins from `installed`.
    pub fn start(
        project: &Project,
        asked: Asked,
        config: EngineConfig,
        installed: Vec<PluginInfo>,
        waker: Option<Waker>,
    ) -> Result<Self, String> {
        let (level, (quietest, loudest)) = (asked.level, LEVELS);
        if !(quietest..=loudest).contains(&level) {
            return Err(format!(
                "level_dbfs is from {quietest} to {loudest}, not {level}."
            ));
        }
        let nyquist = f64::from(config.sample_rate) / 2.0;
        if let Some(hz) = (asked.at.iter()).find(|&&hz| !(hz > 0.0 && hz < nyquist)) {
            return Err(format!(
                "Each of at is a frequency above 0 and below half the sample rate, {nyquist} \
                 Hz, not {hz}."
            ));
        }
        let Some((_, _, device)) = project.find_device(asked.device) else {
            return Err(format!("There's no device {}.", asked.device.0));
        };
        let name = device.kind.name().to_owned();
        if device.kind.is_instrument() || device.kind.is_midi_effect() {
            return Err(format!(
                "{name} plays notes: only a device that takes audio has a response."
            ));
        }
        let layout = Layout::new(config.sample_rate);
        static PROBES: AtomicU64 = AtomicU64::new(0);
        let folder = std::env::temp_dir().join(format!(
            "musicable-response-{}-{}",
            std::process::id(),
            PROBES.fetch_add(1, Ordering::Relaxed)
        ));
        let made = |error: std::io::Error| format!("Couldn't write the test signal: {error}.");
        fs::create_dir_all(&folder).map_err(made)?;
        let signal = layout.signal(level);
        let file = folder.join("Signal.wav");
        audio::save_wav(
            &file,
            layout.rate,
            SampleFormat::Float32,
            &[&signal, &signal],
        )
        .map_err(|error| {
            let _ = fs::remove_dir_all(&folder);
            made(error)
        })?;

        let mut probe = Project::new();
        probe.tempo = project.tempo.clone();
        probe.tempo.set_tempo(120.0);
        let seconds = layout.frames as f64 / f64::from(layout.rate);
        let range = BeatRange::new(Beats::ZERO, Beats::from_f64(seconds * 2.0));
        let mut tracks = Vec::new();
        for (track, copy) in [("Dry", false), ("Wet", true)] {
            let id = probe.add_track(track, TrackKind::Audio);
            let clip = probe.new_clip(
                "Signal",
                range,
                ClipContent::Audio(AudioClip::new("Signal.wav")),
            );
            let added = copy.then(|| probe.new_device(device.kind.clone()));
            let track = probe.track_mut(id).expect("just added");
            track.clips.push(clip);
            track.devices.extend(added);
            tracks.push(id);
        }
        let stems = [folder.join("Dry.wav"), folder.join("Wet.wav")];
        let job = Job {
            project: probe,
            folder: Some(folder.clone()),
            range,
            settings: ExportSettings {
                format: FileFormat::Wav,
                bits: Bits::Float32,
                sample_rate: None,
                dither: false,
                normalize: false,
                tail: false,
                mixdown: false,
                stems: true,
            },
            config,
            installed,
            waker,
            mixdown: None,
            stems: vec![(tracks[0], stems[0].clone()), (tracks[1], stems[1].clone())],
        };
        let export = Export::start(job).map_err(|error| {
            let _ = fs::remove_dir_all(&folder);
            format!("The measurement couldn't start: {error}.")
        })?;
        Ok(Self {
            export,
            folder,
            stems,
            layout,
            asked,
            name,
        })
    }

    /// The device being measured, and what it's called.
    pub fn device(&self) -> (DeviceId, &str) {
        (self.asked.device, &self.name)
    }

    /// Carries the measurement on, and once it's done, gives its result.
    pub fn tick(&mut self) -> Option<Result<Json, String>> {
        self.export.tick();
        let report = self.export.report()?;
        if let Some(failed) = &report.failed {
            return Some(Err(format!("The measurement stopped: {failed}.")));
        }
        if report.cancelled {
            return Some(Err("The measurement was cancelled.".into()));
        }
        if !report.problems.is_empty() {
            return Some(Err(format!(
                "{} couldn't be measured: {}.",
                self.name,
                report.problems.join("; ")
            )));
        }
        Some(self.measure())
    }

    fn measure(&self) -> Result<Json, String> {
        let read = |path: &PathBuf| {
            Decoder::open(path)
                .and_then(|mut decoder| decoder.read_all())
                .map_err(|error| format!("Couldn't read what the device played: {error}."))
        };
        let (dry, wet) = (read(&self.stems[0])?, read(&self.stems[1])?);
        let mut result = measure(&self.layout, &self.asked, &dry, &wet)?;
        result["device"] = json!(self.asked.device.0);
        result["name"] = json!(self.name);
        Ok(result)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.export.cancel();
        let _ = fs::remove_dir_all(&self.folder);
    }
}

/// Measures `wet` against `dry`, each a list of channels, played as
/// `layout` says.
fn measure(
    layout: &Layout,
    asked: &Asked,
    dry: &[Vec<f32>],
    wet: &[Vec<f32>],
) -> Result<Json, String> {
    let frames = |channels: &[Vec<f32>]| channels.iter().map(Vec::len).min().unwrap_or(0);
    if dry.is_empty() || wet.is_empty() || frames(dry).min(frames(wet)) < layout.frames / 2 {
        return Err("The device played too little to measure.".into());
    }
    let rate = f64::from(layout.rate);
    let size = ((rate * WINDOW_SECONDS) as usize).next_power_of_two();
    let first = Cross::of(dry, wet, layout.noise, 0, size);
    let delay = first.delay();
    let cross = Cross::of(dry, wet, layout.noise, delay, size);
    let frequency = cross.response(rate, asked);
    let dynamics = dynamics(layout, dry, wet, delay);
    Ok(json!({
        "sample_rate": layout.rate,
        "delay_frames": delay,
        "delay_ms": round(delay as f64 * 1000.0 / rate, 3),
        "frequency": frequency,
        "dynamics": dynamics,
    }))
}

/// The dry and wet spectra and their cross-spectrum, summed over channels
/// and windows.
struct Cross {
    size: usize,
    xx: Vec<f64>,
    yy: Vec<f64>,
    xy: Vec<Complex64>,
}

impl Cross {
    /// Over `stretch` of `dry`, and `wet` `delay` frames later.
    fn of(
        dry: &[Vec<f32>],
        wet: &[Vec<f32>],
        stretch: (usize, usize),
        delay: i64,
        size: usize,
    ) -> Self {
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
        let window: Vec<f64> = (0..size)
            .map(|i| 0.5 - 0.5 * (TAU * i as f64 / size as f64).cos())
            .collect();
        let bins = size / 2 + 1;
        let mut cross = Self {
            size,
            xx: vec![0.0; bins],
            yy: vec![0.0; bins],
            xy: vec![Complex64::new(0.0, 0.0); bins],
        };
        let (mut x, mut y) = (fft.make_input_vec(), fft.make_input_vec());
        let (mut xs, mut ys) = (fft.make_output_vec(), fft.make_output_vec());
        for (dry, wet) in dry.iter().zip(wet) {
            let mut start = stretch.0;
            while start + size <= stretch.1.min(dry.len()) {
                for i in 0..size {
                    x[i] = f64::from(dry[start + i]) * window[i];
                    let n = (start + i) as i64 + delay;
                    let sample = usize::try_from(n).ok().and_then(|n| wet.get(n));
                    y[i] = f64::from(sample.copied().unwrap_or(0.0)) * window[i];
                }
                fft.process(&mut x, &mut xs)
                    .expect("the buffers are the plan's");
                fft.process(&mut y, &mut ys)
                    .expect("the buffers are the plan's");
                for k in 0..bins {
                    cross.xx[k] += xs[k].norm_sqr();
                    cross.yy[k] += ys[k].norm_sqr();
                    cross.xy[k] += xs[k].conj() * ys[k];
                }
                start += size / 2;
            }
        }
        cross
    }

    /// How many frames the output lags the input by: the peak of the
    /// impulse response, from half a window early to half a window late.
    fn delay(&self) -> i64 {
        let inverse = RealFftPlanner::<f64>::new().plan_fft_inverse(self.size);
        let mut h: Vec<Complex64> = (self.xx.iter().zip(&self.xy))
            .map(|(&xx, &xy)| match xx > 0.0 {
                true => xy / xx,
                false => Complex64::new(0.0, 0.0),
            })
            .collect();
        let last = h.len() - 1;
        h[0].im = 0.0;
        h[last].im = 0.0;
        let mut ir = inverse.make_output_vec();
        if inverse.process(&mut h, &mut ir).is_err() {
            return 0;
        }
        let peak = (ir.iter().enumerate())
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .filter(|(_, value)| value.abs() > 0.0)
            .map_or(0, |(n, _)| n);
        match peak > self.size / 2 {
            true => peak as i64 - self.size as i64,
            false => peak as i64,
        }
    }

    fn hz(&self, k: usize, rate: f64) -> f64 {
        k as f64 * rate / self.size as f64
    }

    /// The gain, phase and coherence over the bins `bins`, weighted by the
    /// input's power in each: `None` where no input went in, and no gain
    /// where none came out.
    fn over(&self, bins: impl Iterator<Item = usize>) -> Option<(Option<f64>, f64, f64)> {
        let (mut input, mut linear, mut coherent) = (0.0, 0.0, 0.0);
        let mut sum = Complex64::new(0.0, 0.0);
        for k in bins {
            let (xx, yy, xy) = (self.xx[k], self.yy[k], self.xy[k]);
            if xx <= 0.0 {
                continue;
            }
            input += xx;
            linear += xy.norm_sqr() / xx;
            if yy > 0.0 {
                coherent += xy.norm_sqr() / yy;
            }
            sum += xy;
        }
        (input > 0.0).then(|| {
            let gain = (linear > 0.0).then(|| 10.0 * (linear / input).log10());
            (gain, sum.arg().to_degrees(), coherent / input)
        })
    }

    /// The frequency response, for the noise at `asked.level`.
    fn response(&self, rate: f64, asked: &Asked) -> Json {
        let width = rate / self.size as f64;
        let bin = |hz: f64| ((hz / width).round() as usize).min(self.xx.len() - 1);
        let bands: Vec<Json> = (spectra::bands(false, rate / 2.0).into_iter())
            .filter_map(|band| {
                let (from, to) = (bin(band.from), bin(band.to));
                let (gain, phase, coherence) = self.over(from..to.max(from + 1))?;
                Some(json!({
                    "centre_hz": band.centre,
                    "gain_db": decibels(gain),
                    "phase_degrees": round(phase, 1),
                    "coherence": round(coherence, 4),
                }))
            })
            .collect();
        let at: Vec<Json> = (asked.at.iter())
            .filter_map(|&hz| {
                let k = bin(hz);
                let (gain, phase, coherence) = self.over(k.saturating_sub(1)..=k + 1)?;
                Some(json!({
                    "hz": hz,
                    "gain_db": decibels(gain),
                    "phase_degrees": round(phase, 1),
                    "coherence": round(coherence, 4),
                }))
            })
            .collect();
        // Bin by bin, over what's heard.
        let top = AUDIBLE.1.min(rate * 0.45);
        let mut lowest: Option<(f64, f64)> = None;
        let mut highest: Option<(f64, f64)> = None;
        for k in bin(AUDIBLE.0)..=bin(top) {
            let Some((Some(gain), _, _)) = self.over(k..=k) else {
                continue;
            };
            let hz = self.hz(k, rate);
            if lowest.is_none_or(|(_, low)| gain < low) {
                lowest = Some((hz, gain));
            }
            if highest.is_none_or(|(_, high)| gain > high) {
                highest = Some((hz, gain));
            }
        }
        let extreme = |found: Option<(f64, f64)>| {
            found.map_or(
                Json::Null,
                |(hz, gain)| json!({ "hz": round(hz, 1), "gain_db": round(gain, 3) }),
            )
        };
        json!({
            "level_dbfs": asked.level,
            "bin_hz": round(width, 3),
            "lowest": extreme(lowest),
            "highest": extreme(highest),
            "bands": bands,
            "at": at,
        })
    }
}

/// The mean square of `channels` from `from` to `to`, averaged over them.
fn power(channels: &[Vec<f32>], from: usize, to: usize) -> f64 {
    let sum: f64 = (channels.iter())
        .map(|channel| {
            (channel
                .get(from..to.min(channel.len()))
                .unwrap_or(&[])
                .iter())
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
        })
        .sum();
    sum / ((to - from) * channels.len()).max(1) as f64
}

fn db(power: f64) -> Option<f64> {
    (power > 0.0).then(|| 10.0 * power.log10())
}

/// The static curve and the attack and release.
fn dynamics(layout: &Layout, dry: &[Vec<f32>], wet: &[Vec<f32>], delay: i64) -> Json {
    let later = |n: usize| (n as i64 + delay).max(0) as usize;
    let gain = |from: usize, to: usize| {
        let (input, output) = (power(dry, from, to), power(wet, later(from), later(to)));
        (db(input), db(output))
    };
    let curve: Vec<Json> = (layout.steps.iter())
        .map(|&(from, to)| {
            let (input, output) = gain(from, to);
            let change = input.zip(output).map(|(i, o)| o - i);
            json!({
                "in_dbfs": decibels(input),
                "out_dbfs": decibels(output),
                "gain_db": decibels(change),
            })
        })
        .collect();

    // The gain every few milliseconds through the jump.
    let rate = f64::from(layout.rate);
    let window = ((ENVELOPE * rate).round() as usize).max(1);
    let settled = (SETTLED * rate).round() as usize;
    let envelope = |from: usize, to: usize| -> Vec<Option<f64>> {
        (from..to)
            .step_by(window)
            .take_while(|start| start + window <= to)
            .map(|start| {
                let (input, output) = gain(start, start + window);
                input.zip(output).map(|(i, o)| o - i)
            })
            .collect()
    };
    let median = |values: &[Option<f64>]| {
        let mut known: Vec<f64> = values.iter().flatten().copied().collect();
        known.sort_by(f64::total_cmp);
        (!known.is_empty()).then(|| known[known.len() / 2])
    };
    let [low, high, after] = layout.jump;
    let before = median(&envelope(low.1 - settled, low.1));
    let during = median(&envelope(high.1 - settled, high.1));
    let ending = median(&envelope(after.1 - settled, after.1));
    // How long from `from` until the gain stays within 10% of the way
    // from `start` to `end`.
    let time = |stretch: (usize, usize), start: Option<f64>, end: Option<f64>| {
        let (start, end) = (start?, end?);
        if (end - start).abs() < MOVES_DB {
            return None;
        }
        let near = (1.0 - THERE) * (end - start).abs();
        let gains = envelope(stretch.0, stretch.1);
        let last_away = gains
            .iter()
            .rposition(|gain| gain.is_none_or(|gain| (gain - end).abs() > near));
        let windows = last_away.map_or(0, |n| n + 1);
        Some(round(windows as f64 * window as f64 * 1000.0 / rate, 1))
    };
    let attack = time(high, before, during);
    let release = time(after, during, ending);
    let level = |stretch: (usize, usize)| decibels(db(power(dry, stretch.0, stretch.1)));
    json!({
        "hz": SINE_HZ,
        "curve": curve,
        "jump": {
            "low_dbfs": level(low),
            "high_dbfs": level(high),
            "gain_before_db": decibels(before),
            "gain_high_db": decibels(during),
            "gain_after_db": decibels(ending),
            "attack_ms": attack,
            "release_ms": release,
        },
    })
}

/// A measurement in a sentence, for the status bar: the range of the
/// gain, any delay, and the gain at the loudest step.
pub fn summary(name: &str, result: &Json) -> String {
    let gain = |extreme: &str| result["frequency"][extreme]["gain_db"].as_f64();
    let range = match (gain("lowest"), gain("highest")) {
        (Some(low), Some(high)) => format!("its gain is from {low:.2} to {high:.2} dB"),
        _ => "it's silent".to_owned(),
    };
    let delay = match result["delay_frames"].as_i64().unwrap_or(0) {
        0 => String::new(),
        frames => format!(", {frames} frames late"),
    };
    let loudest = (result["dynamics"]["curve"].as_array())
        .and_then(|curve| curve.last()?["gain_db"].as_f64())
        .map_or(String::new(), |gain| {
            format!(", and {gain:.2} dB for a full-scale sine")
        });
    format!("{name}'s response: {range} from 20 Hz to 20 kHz{delay}{loudest}.")
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale + 0.0
}

/// A level in decibels, to 3 places, or `null` for silence.
fn decibels(value: Option<f64>) -> Json {
    value
        .filter(|value| value.is_finite())
        .map_or(Json::Null, |value| json!(round(value, 3)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(signal: &[f32]) -> Vec<Vec<f32>> {
        vec![signal.to_vec(), signal.to_vec()]
    }

    fn asked(at: &[f64]) -> Asked {
        Asked {
            device: DeviceId(1),
            level: LEVEL,
            at: at.to_vec(),
        }
    }

    #[test]
    fn a_gain_reads_flat_with_no_delay_and_no_dynamics() {
        let layout = Layout::new(48_000);
        let dry = layout.signal(LEVEL);
        let wet: Vec<f32> = dry.iter().map(|s| s * 0.5).collect();
        let result = measure(&layout, &asked(&[1000.0]), &stereo(&dry), &stereo(&wet)).unwrap();
        assert_eq!(result["delay_frames"], 0, "{result}");
        let frequency = &result["frequency"];
        let half = 20.0 * 0.5f64.log10();
        for extreme in ["lowest", "highest"] {
            let gain = frequency[extreme]["gain_db"].as_f64().unwrap();
            assert!((gain - half).abs() < 0.01, "{frequency}");
        }
        let at = &frequency["at"][0];
        assert!(
            (at["gain_db"].as_f64().unwrap() - half).abs() < 0.01,
            "{at}"
        );
        assert!(at["phase_degrees"].as_f64().unwrap().abs() < 0.1, "{at}");
        assert!(at["coherence"].as_f64().unwrap() > 0.999, "{at}");
        let curve = result["dynamics"]["curve"].as_array().unwrap();
        assert_eq!(curve.len(), STEPS.len());
        for step in curve {
            assert!(
                (step["gain_db"].as_f64().unwrap() - half).abs() < 0.01,
                "{step}"
            );
        }
        // The top step is a full-scale sine: an RMS of -3.01 dBFS.
        assert_eq!(curve[10]["in_dbfs"], -3.01, "{result}");
        let jump = &result["dynamics"]["jump"];
        assert_eq!(jump["attack_ms"], Json::Null, "{jump}");
        assert_eq!(jump["release_ms"], Json::Null, "{jump}");
    }

    #[test]
    fn a_delay_a_filter_and_a_compressor_read_as_they_are() {
        let layout = Layout::new(48_000);
        let dry = layout.signal(LEVEL);
        // 7 frames late, and the average of two neighbours: a lowpass,
        // with a gain of cos(πf/rate) and a phase of -πf/rate on top.
        let wet: Vec<f32> = (0..dry.len())
            .map(|n| match n {
                0..=7 => 0.0,
                n => 0.5 * (dry[n - 7] + dry[n - 8]),
            })
            .collect();
        let result = measure(&layout, &asked(&[6000.0]), &stereo(&dry), &stereo(&wet)).unwrap();
        assert_eq!(result["delay_frames"], 7, "{result}");
        let at = &result["frequency"]["at"][0];
        let expected = 20.0 * (std::f64::consts::PI * 6000.0 / 48_000.0).cos().log10();
        assert!(
            (at["gain_db"].as_f64().unwrap() - expected).abs() < 0.02,
            "{at}"
        );
        let phase = -180.0 * 6000.0 / 48_000.0;
        assert!(
            (at["phase_degrees"].as_f64().unwrap() - phase).abs() < 0.5,
            "{at}"
        );

        // Above -20 dBFS, half the level over it comes through, with the
        // gain following in 10 ms going down and 100 ms coming back.
        let rate = 48_000.0;
        let threshold = 10f64.powf(-20.0 / 20.0);
        let (attack, release) = (
            (-1.0 / (0.010f64 * rate / 2.2)).exp(),
            (-1.0 / (0.100f64 * rate / 2.2)).exp(),
        );
        let mut gain = 1.0f64;
        let mut level = 0.0f64;
        let wet: Vec<f32> = (dry.iter())
            .map(|&s| {
                // The peak over the last millisecond or so.
                level = (f64::from(s).abs()).max(level * (-1.0f64 / 48.0).exp());
                let wanted = match level > threshold {
                    true => (threshold / level).sqrt(),
                    false => 1.0,
                };
                let coefficient = if wanted < gain { attack } else { release };
                gain = wanted + (gain - wanted) * coefficient;
                (f64::from(s) * gain) as f32
            })
            .collect();
        let result = measure(&layout, &asked(&[]), &stereo(&dry), &stereo(&wet)).unwrap();
        let jump = &result["dynamics"]["jump"];
        let attack = jump["attack_ms"].as_f64().unwrap();
        let release = jump["release_ms"].as_f64().unwrap();
        assert!((5.0..20.0).contains(&attack), "{jump}");
        assert!((50.0..200.0).contains(&release), "{jump}");
        let curve = result["dynamics"]["curve"].as_array().unwrap();
        let quiet = curve[0]["gain_db"].as_f64().unwrap();
        let loud = curve[10]["gain_db"].as_f64().unwrap();
        assert!(quiet.abs() < 0.05, "{curve:?}");
        assert!(loud < -8.0, "{curve:?}");
    }
}

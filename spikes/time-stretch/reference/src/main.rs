//! Spike tooling for the own time-stretch engine (docs research/own-time-stretch.md).
//!
//! stretch-reference onsets IN.wav OUT.json
//! stretch-reference r3 IN.wav OUT.wav RATIO
//! stretch-reference signalsmith IN.wav OUT.wav RATIO
//! stretch-reference timing IN.wav OUT.wav RATIO ONSETS.json [--shift-to FIXED.wav]
//!
//! RATIO is output length over input length. `timing` measures, for the
//! clearest onsets, where each attack lands in OUT against round(t * RATIO),
//! less where the same measure finds them in IN itself, prints the median
//! and spread, and with --shift-to writes OUT moved by the
//! median lag (a constant latency, which the app compensates), trimmed or
//! padded to exactly round(N * RATIO) frames.

use std::path::Path;

use musicable_stretch::Stretch;
use musicable_stretch::rubber_band::{self, RubberBand, MOST_FRAMES};

fn read(path: &Path) -> (u32, Vec<Vec<f32>>) {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap() as f32 * scale).collect()
        }
    };
    let frames = samples.len() / channels;
    let mut out = vec![Vec::with_capacity(frames); channels];
    for frame in samples.chunks_exact(channels) {
        for (c, s) in frame.iter().enumerate() {
            out[c].push(*s);
        }
    }
    (spec.sample_rate, out)
}

fn write(path: &Path, rate: u32, channels: &[Vec<f32>]) {
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
    for i in 0..channels[0].len() {
        for c in channels {
            writer.write_sample(c[i]).unwrap();
        }
    }
    writer.finalize().unwrap();
}

fn r3(input: &[Vec<f32>], rate: u32, ratio: f64) -> Vec<Vec<f32>> {
    let channels = input.len();
    let n = input[0].len();
    let mut stretch = RubberBand::offline(channels, rate, ratio, 1.0);
    stretch.set_formants(None);
    stretch.study(n);
    let delay = stretch.start_delay();
    let mut output: Vec<Vec<f32>> = vec![Vec::new(); channels];
    let mut fed = 0;
    let drain = |stretch: &mut RubberBand, output: &mut Vec<Vec<f32>>| {
        while let Some(ready) = stretch.available().filter(|&r| r > 0) {
            let at = output[0].len();
            for c in output.iter_mut() {
                c.resize(at + ready, 0.0);
            }
            let mut outs: Vec<&mut [f32]> = output.iter_mut().map(|c| &mut c[at..]).collect();
            let got = stretch.retrieve(&mut outs);
            for c in output.iter_mut() {
                c.truncate(at + got);
            }
        }
    };
    while fed < n {
        let frames = stretch.wants().min(n - fed).min(MOST_FRAMES);
        let slices: Vec<&[f32]> = input.iter().map(|c| &c[fed..fed + frames]).collect();
        stretch.process(&slices, false);
        fed += frames;
        drain(&mut stretch, &mut output);
    }
    let none: Vec<&[f32]> = vec![&[]; channels];
    stretch.process(&none, true);
    drain(&mut stretch, &mut output);
    let _ = rubber_band::lookahead(rate);
    for c in &mut output {
        c.drain(..delay.min(c.len()));
    }
    output
}

fn signalsmith(input: &[Vec<f32>], rate: u32, ratio: f64) -> Vec<Vec<f32>> {
    let channels = input.len();
    let n = input[0].len();
    let mut stretch = Stretch::new(channels, rate, 1);
    let sizes = stretch.sizes();
    let interval = sizes.interval.max(1);
    // Input padded with silence so the latencies flush out at the end.
    let pad = sizes.input_latency + sizes.output_latency + sizes.block;
    let padded: Vec<Vec<f32>> = input
        .iter()
        .map(|c| {
            let mut v = c.clone();
            v.resize(n + pad * 2, 0.0);
            v
        })
        .collect();
    let want = (n as f64 * ratio).round() as usize + sizes.output_latency * 2 + sizes.block;
    let mut output = vec![vec![0.0f32; want]; channels];
    let mut read = 0.0f64;
    let mut written = 0;
    while written < want {
        let out_frames = interval.min(want - written);
        let next = read + out_frames as f64 / ratio;
        let (from, to) = (read.round() as usize, (next.round() as usize).min(padded[0].len()));
        let slices: Vec<&[f32]> = padded.iter().map(|c| &c[from..to]).collect();
        let mut outs: Vec<&mut [f32]> = output
            .iter_mut()
            .map(|c| &mut c[written..written + out_frames])
            .collect();
        stretch.process(&slices, &mut outs);
        read = next;
        written += out_frames;
    }
    output
}

/// Spectral flux of a mono mix, one value per `hop` frames.
fn flux(channels: &[Vec<f32>], hop: usize) -> Vec<f32> {
    let n = channels[0].len();
    let size = 1024;
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(size);
    let window: Vec<f32> = (0..size)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
        .collect();
    let mut prev = vec![0.0f32; size / 2 + 1];
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < n {
        for i in 0..size {
            // Frame centred on `at`.
            let j = at as i64 + i as i64 - (size / 2) as i64;
            let s = if j >= 0 && (j as usize) < n {
                channels.iter().map(|c| c[j as usize]).sum::<f32>()
            } else {
                0.0
            };
            buf[i] = s * window[i];
        }
        fft.process(&mut buf, &mut spec).unwrap();
        let mut sum = 0.0;
        for (k, v) in spec.iter().enumerate() {
            let m = (1.0 + 100.0 * v.norm()).ln();
            sum += (m - prev[k]).max(0.0);
            prev[k] = m;
        }
        out.push(sum);
        at += hop;
    }
    out
}

fn timing(input: &[Vec<f32>], output: &[Vec<f32>], ratio: f64, onsets: &serde_json::Value) -> (f64, Vec<f64>) {
    let hop = 32;
    let f = flux(output, hop);
    let mut list: Vec<(u64, f64)> = onsets
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["frame"].as_u64().unwrap(), o["strength"].as_f64().unwrap() * o["percussiveness"].as_f64().unwrap()))
        .collect();
    list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    list.truncate(40);
    let _ = input;
    let mut errors = Vec::new();
    let win = (0.03 * 44100.0 / hop as f64) as i64;
    for (frame, _) in list {
        let expect = (frame as f64 * ratio / hop as f64).round() as i64;
        let (mut best, mut at) = (0.0f32, None);
        for k in (expect - win).max(1)..(expect + win).min(f.len() as i64 - 1) {
            let v = f[k as usize];
            if v > best {
                best = v;
                at = Some(k);
            }
        }
        if let Some(k) = at {
            errors.push((k - expect) as f64 * hop as f64 / 44.1);
        }
    }
    let mut sorted = errors.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if sorted.is_empty() { 0.0 } else { sorted[sorted.len() / 2] };
    (median, errors)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("onsets") => {
            let mut file = musicable_analysis::File::open(Path::new(&args[2])).expect("open");
            let onsets = musicable_analysis::onsets(&mut file).expect("onsets");
            let json: Vec<serde_json::Value> = onsets
                .iter()
                .map(|o| serde_json::json!({"frame": o.frame, "strength": o.strength, "percussiveness": o.percussiveness}))
                .collect();
            std::fs::write(&args[3], serde_json::to_string(&json).unwrap()).unwrap();
            eprintln!("{} onsets", json.len());
        }
        Some(engine @ ("r3" | "signalsmith")) => {
            let (rate, input) = read(Path::new(&args[2]));
            let ratio: f64 = args[4].parse().unwrap();
            let start = std::time::Instant::now();
            let output = if engine == "r3" { r3(&input, rate, ratio) } else { signalsmith(&input, rate, ratio) };
            eprintln!("{engine} {:.2} s", start.elapsed().as_secs_f64());
            write(Path::new(&args[3]), rate, &output);
        }
        Some("timing") => {
            let (_, input) = read(Path::new(&args[2]));
            let (rate, output) = read(Path::new(&args[3]));
            let ratio: f64 = args[4].parse().unwrap();
            let onsets: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[5]).unwrap()).unwrap();
            // The measure's own bias: the same onsets found in the input.
            let (bias, _) = timing(&input, &input, 1.0, &onsets);
            let (median, errors) = timing(&input, &output, ratio, &onsets);
            let errors: Vec<f64> = errors.iter().map(|e| e - bias).collect();
            let median = median - bias;
            let spread: f64 = {
                let mut d: Vec<f64> = errors.iter().map(|e| (e - median).abs()).collect();
                d.sort_by(|a, b| a.partial_cmp(b).unwrap());
                if d.is_empty() { 0.0 } else { d[d.len() * 3 / 4] }
            };
            println!("median {median:+.2} ms, 75% within {spread:.2} ms of it, {} onsets, length {} want {}",
                errors.len(), output[0].len(), (input[0].len() as f64 * ratio).round());
            if args.get(6).map(String::as_str) == Some("--shift-to") {
                let lag = (median * 44.1).round() as i64;
                let want = (input[0].len() as f64 * ratio).round() as usize;
                let fixed: Vec<Vec<f32>> = output
                    .iter()
                    .map(|c| (0..want).map(|i| {
                        let j = i as i64 + lag;
                        if j >= 0 && (j as usize) < c.len() { c[j as usize] } else { 0.0 }
                    }).collect())
                    .collect();
                write(Path::new(&args[7]), rate, &fixed);
            }
        }
        _ => eprintln!("see the doc comment in src/main.rs"),
    }
}

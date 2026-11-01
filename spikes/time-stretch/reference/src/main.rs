//! Spike tooling for the own time-stretch engine (docs research/own-time-stretch.md).
//!
//! stretch-reference onsets IN.wav OUT.json
//! stretch-reference r3 IN.wav OUT.wav RATIO
//! stretch-reference signalsmith IN.wav OUT.wav RATIO
//! stretch-reference timing IN.wav OUT.wav RATIO ONSETS.json [--shift-to FIXED.wav]
//! stretch-reference prepare --material DIR --out DIR --engine TSM_LAB --musicable MUSICABLE
//!     [--ratios 0.8,1.25,2] [--jobs N] [--lufs -23]
//!
//! RATIO is output length over input length. `timing` measures, for the
//! clearest onsets, where each attack lands in OUT against round(t * RATIO),
//! less where the same measure finds them in IN itself, prints the median
//! and spread, and with --shift-to writes OUT moved by the median lag (a
//! constant latency, which the app compensates), trimmed or padded to
//! exactly round(N * RATIO) frames.
//!
//! `prepare` does all of it for every WAV file in a folder: onsets, renders
//! from R3, Signalsmith and three variants of the spike engine at each
//! ratio, each moved by its own median lag, then `musicable audio match`
//! to bring every render of a file and ratio to one loudness. It writes
//! `timing.tsv` and `round.json`, the two lists for `debug.make_blind_test`
//! (the spike engine first, R3 second).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use musicable_stretch::Stretch;
use musicable_stretch::rubber_band::{MOST_FRAMES, RubberBand};

type Channels = Vec<Vec<f32>>;

/// The renders `prepare` makes of each file at each ratio, by name.
const ENGINES: &[&str] = &["ours-full", "ours-pv", "ours-hptsm", "r3", "signalsmith"];

fn read(path: &Path) -> (u32, Channels) {
    let mut reader = hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.unwrap() as f32 * scale)
                .collect()
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
    let mut writer = hound::WavWriter::create(path, spec)
        .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    for i in 0..channels[0].len() {
        for c in channels {
            writer.write_sample(c[i]).unwrap();
        }
    }
    writer.finalize().unwrap();
}

fn r3(input: &[Vec<f32>], rate: u32, ratio: f64) -> Channels {
    let channels = input.len();
    let n = input[0].len();
    let mut stretch = RubberBand::offline(channels, rate, ratio, 1.0);
    stretch.set_formants(None);
    stretch.study(n);
    let delay = stretch.start_delay();
    let mut output: Channels = vec![Vec::new(); channels];
    let drain = |stretch: &mut RubberBand, output: &mut Channels| {
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
    let mut fed = 0;
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
    for c in &mut output {
        c.drain(..delay.min(c.len()));
    }
    output
}

fn signalsmith(input: &[Vec<f32>], rate: u32, ratio: f64) -> Channels {
    let channels = input.len();
    let n = input[0].len();
    let mut stretch = Stretch::new(channels, rate, 1);
    let sizes = stretch.sizes();
    let interval = sizes.interval.max(1);
    // Input padded with silence so the latencies flush out at the end.
    let pad = sizes.input_latency + sizes.output_latency + sizes.block;
    let padded: Channels = input
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
        let (from, to) = (
            read.round() as usize,
            (next.round() as usize).min(padded[0].len()),
        );
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

/// Where the clearest 40 onsets' flux peaks fall in `output`, in ms from
/// round(frame * ratio), and their median.
fn lags(output: &[Vec<f32>], rate: u32, ratio: f64, onsets: &serde_json::Value) -> (f64, Vec<f64>) {
    let hop = 32;
    let f = flux(output, hop);
    let mut list: Vec<(u64, f64)> = onsets
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let strength = o["strength"].as_f64().unwrap();
            let sharp = o["percussiveness"].as_f64().unwrap();
            (o["frame"].as_u64().unwrap(), strength * sharp)
        })
        .collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    list.truncate(40);
    let ms = |frames: f64| frames * 1000.0 / f64::from(rate);
    let win = (0.03 * f64::from(rate) / hop as f64) as i64;
    let mut errors = Vec::new();
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
            errors.push(ms(((k - expect) * hop as i64) as f64));
        }
    }
    let mut sorted = errors.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    (median, errors)
}

struct Timing {
    median_ms: f64,
    /// Three quarters of the attacks are within this of the median.
    spread_ms: f64,
    onsets: usize,
}

/// Where `output`'s attacks land, less where the same measure finds them in
/// `input` itself.
fn timing(input: &[Vec<f32>], output: &[Vec<f32>], rate: u32, ratio: f64, onsets: &serde_json::Value) -> Timing {
    let (bias, _) = lags(input, rate, 1.0, onsets);
    let (median, errors) = lags(output, rate, ratio, onsets);
    let mut spread: Vec<f64> = errors.iter().map(|e| (e - median).abs()).collect();
    spread.sort_by(f64::total_cmp);
    Timing {
        median_ms: median - bias,
        spread_ms: spread.get(spread.len() * 3 / 4).copied().unwrap_or(0.0),
        onsets: errors.len(),
    }
}

/// `output` moved earlier by `lag_ms` and made exactly `frames` long.
fn shifted(output: &[Vec<f32>], rate: u32, lag_ms: f64, frames: usize) -> Channels {
    let lag = (lag_ms * f64::from(rate) / 1000.0).round() as i64;
    output
        .iter()
        .map(|c| {
            (0..frames)
                .map(|i| {
                    let j = i as i64 + lag;
                    if j >= 0 && (j as usize) < c.len() {
                        c[j as usize]
                    } else {
                        0.0
                    }
                })
                .collect()
        })
        .collect()
}

fn onsets_json(path: &Path) -> serde_json::Value {
    let mut file = musicable_analysis::File::open(path).expect("open for onsets");
    let onsets = musicable_analysis::onsets(&mut file).expect("onsets");
    onsets
        .iter()
        .map(|o| {
            serde_json::json!({
                "frame": o.frame,
                "strength": o.strength,
                "percussiveness": o.percussiveness,
            })
        })
        .collect()
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn prepare(args: &[String]) {
    let need = |name: &str| option(args, name).unwrap_or_else(|| panic!("{name} is needed"));
    let material = PathBuf::from(need("--material"));
    let out = PathBuf::from(need("--out"));
    let engine = PathBuf::from(need("--engine"));
    let musicable = PathBuf::from(need("--musicable"));
    let ratios: Vec<f64> = option(args, "--ratios")
        .unwrap_or_else(|| "0.8,1.25,2".into())
        .split(',')
        .map(|r| r.trim().parse().expect("a ratio"))
        .collect();
    let jobs: usize = option(args, "--jobs").map_or_else(
        || std::thread::available_parallelism().map_or(4, |n| n.get()),
        |j| j.parse().expect("jobs"),
    );
    let lufs = option(args, "--lufs").unwrap_or_else(|| "-23".into());
    for dir in ["onsets", "raw", "aligned", "matched"] {
        std::fs::create_dir_all(out.join(dir)).unwrap();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&material)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
        .collect();
    files.sort();
    eprintln!("{} files, ratios {ratios:?}, {jobs} jobs", files.len());

    // Onsets first: the engine takes them.
    for file in &files {
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let path = out.join("onsets").join(format!("{name}.json"));
        if !path.exists() {
            std::fs::write(&path, serde_json::to_string(&onsets_json(file)).unwrap()).unwrap();
        }
    }

    let mut work = Vec::new();
    for file in &files {
        for &ratio in &ratios {
            for engine_name in ENGINES {
                work.push((file.clone(), ratio, *engine_name));
            }
        }
    }
    let queue = Mutex::new(work.into_iter());
    let rows = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let Some((file, ratio, name)) = queue.lock().unwrap().next() else {
                    break;
                };
                let stem = file.file_stem().unwrap().to_string_lossy().to_string();
                let label = format!("{stem} @{ratio} {name}");
                let raw = out.join("raw").join(format!("{label}.wav"));
                let onsets_path = out.join("onsets").join(format!("{stem}.json"));
                let (rate, input) = read(&file);
                let start = std::time::Instant::now();
                match name {
                    "r3" => write(&raw, rate, &r3(&input, rate, ratio)),
                    "signalsmith" => write(&raw, rate, &signalsmith(&input, rate, ratio)),
                    _ => {
                        let variant = name.trim_start_matches("ours-");
                        let status = Command::new(&engine)
                            .arg("--in")
                            .arg(&file)
                            .arg("--out")
                            .arg(&raw)
                            .arg("--ratio")
                            .arg(ratio.to_string())
                            .arg("--onsets")
                            .arg(&onsets_path)
                            .arg("--variant")
                            .arg(variant)
                            .output()
                            .expect("run the engine");
                        assert!(status.status.success(), "{label}: {}", String::from_utf8_lossy(&status.stderr));
                    }
                }
                let seconds = start.elapsed().as_secs_f64();
                let (_, output) = read(&raw);
                let onsets: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&onsets_path).unwrap()).unwrap();
                let t = timing(&input, &output, rate, ratio, &onsets);
                let frames = (input[0].len() as f64 * ratio).round() as usize;
                let aligned = out.join("aligned").join(format!("{label}.wav"));
                write(&aligned, rate, &shifted(&output, rate, t.median_ms, frames));
                eprintln!("{label}: {seconds:.1} s, attacks {:+.2} ms", t.median_ms);
                rows.lock().unwrap().push(format!(
                    "{stem}\t{ratio}\t{name}\t{:.2}\t{:.2}\t{}\t{seconds:.2}",
                    t.median_ms, t.spread_ms, t.onsets
                ));
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort();
    let mut tsv = String::from("file\tratio\tengine\tmedian_ms\tspread75_ms\tonsets\tseconds\n");
    for row in rows {
        tsv.push_str(&row);
        tsv.push('\n');
    }
    std::fs::write(out.join("timing.tsv"), tsv).unwrap();

    // One loudness for every render of a file and ratio, lined up already.
    let mut first = Vec::new();
    let mut second = Vec::new();
    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        for &ratio in &ratios {
            let path = |name: &str| out.join("aligned").join(format!("{stem} @{ratio} {name}.wav"));
            let mut command = Command::new(&musicable);
            command.arg("audio").arg("match").arg(path(ENGINES[0]));
            for name in &ENGINES[1..] {
                command.arg(path(name));
            }
            command
                .arg("--out-dir")
                .arg(out.join("matched"))
                .arg("--to-lufs")
                .arg(&lufs)
                .arg("--no-align")
                .arg("--overwrite");
            let result = command.output().expect("run musicable audio match");
            assert!(
                result.status.success(),
                "match {stem} @{ratio}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let matched = |name: &str| out.join("matched").join(format!("{stem} @{ratio} {name}.wav"));
            first.push(matched("ours-full"));
            second.push(matched("r3"));
        }
    }
    let round = serde_json::json!({
        "first": first,
        "second": second,
        "first_name": "Ours",
        "second_name": "R3",
        "name": "Time-stretch spike, round 1",
    });
    std::fs::write(out.join("round.json"), serde_json::to_string_pretty(&round).unwrap()).unwrap();
    eprintln!("done: {}", out.display());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("onsets") => {
            let json = onsets_json(Path::new(&args[2]));
            std::fs::write(&args[3], serde_json::to_string(&json).unwrap()).unwrap();
            eprintln!("{} onsets", json.as_array().map_or(0, Vec::len));
        }
        Some(engine @ ("r3" | "signalsmith")) => {
            let (rate, input) = read(Path::new(&args[2]));
            let ratio: f64 = args[4].parse().unwrap();
            let start = std::time::Instant::now();
            let output = if engine == "r3" {
                r3(&input, rate, ratio)
            } else {
                signalsmith(&input, rate, ratio)
            };
            eprintln!("{engine} {:.2} s", start.elapsed().as_secs_f64());
            write(Path::new(&args[3]), rate, &output);
        }
        Some("timing") => {
            let (_, input) = read(Path::new(&args[2]));
            let (rate, output) = read(Path::new(&args[3]));
            let ratio: f64 = args[4].parse().unwrap();
            let onsets: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&args[5]).unwrap()).unwrap();
            let t = timing(&input, &output, rate, ratio, &onsets);
            let frames = (input[0].len() as f64 * ratio).round() as usize;
            println!(
                "median {:+.2} ms, 75% within {:.2} ms of it, {} onsets, length {} want {frames}",
                t.median_ms,
                t.spread_ms,
                t.onsets,
                output[0].len()
            );
            if args.get(6).map(String::as_str) == Some("--shift-to") {
                write(Path::new(&args[7]), rate, &shifted(&output, rate, t.median_ms, frames));
            }
        }
        Some("prepare") => prepare(&args[2..]),
        _ => eprintln!("see the doc comment in src/main.rs"),
    }
}

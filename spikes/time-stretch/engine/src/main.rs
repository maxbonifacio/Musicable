use std::path::PathBuf;
use std::process::ExitCode;
use tsm_lab::audio::Audio;
use tsm_lab::{Options, Variant, onsets, process};

const USAGE: &str = "usage:
  tsm-lab --in a.wav --out b.wav --ratio R [--pitch SEMITONES] [--onsets o.json] [--variant full|full-2way|pv|hptsm]
  tsm-lab check [--material DIR] [--quick]

  --ratio R   output length / input length (2.0 = twice as long)
  --pitch S   pitch shift in semitones (time-stretch then sinc resample)
  --onsets F  JSON list [{\"frame\":u64,\"strength\":f32,\"percussiveness\":f32},...]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("check") {
        let mut material: Option<PathBuf> = Some(PathBuf::from("/home/user/stretch-material"));
        let mut quick = false;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--material" => {
                    material = args.get(i + 1).map(PathBuf::from);
                    i += 1;
                }
                "--no-material" => material = None,
                "--quick" => quick = true,
                _ => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            }
            i += 1;
        }
        let ok = tsm_lab::checks::run_and_print(material.as_deref(), quick);
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    let mut inp = None;
    let mut out = None;
    let mut ratio = None;
    let mut pitch = 0.0;
    let mut ons = None;
    let mut variant = Variant::Full;
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned();
        let need = |v: Option<String>| -> Result<String, String> { v.ok_or_else(|| format!("missing value for {}", args[i])) };
        let r: Result<(), String> = (|| {
            match args[i].as_str() {
                "--in" => inp = Some(PathBuf::from(need(v)?)),
                "--out" => out = Some(PathBuf::from(need(v)?)),
                "--ratio" => ratio = Some(need(v)?.parse::<f64>().map_err(|e| e.to_string())?),
                "--pitch" => pitch = need(v)?.parse::<f64>().map_err(|e| e.to_string())?,
                "--onsets" => ons = Some(PathBuf::from(need(v)?)),
                "--variant" => {
                    let s = need(v)?;
                    variant = Variant::parse(&s).ok_or(format!("unknown variant {s}"))?
                }
                "-h" | "--help" => return Err(String::new()),
                a => return Err(format!("unknown argument {a}")),
            }
            Ok(())
        })();
        if let Err(e) = r {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        i += 2;
    }
    let (Some(inp), Some(out), Some(ratio)) = (inp, out, ratio) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if !(ratio > 0.0 && ratio.is_finite()) {
        eprintln!("ratio must be > 0");
        return ExitCode::from(2);
    }
    let audio = match Audio::read_wav(&inp) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let mut opt = Options::new(ratio).variant(variant).pitch(pitch);
    if let Some(p) = ons {
        match onsets::load(&p) {
            Ok(o) => opt.onsets = o,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }
    let t = std::time::Instant::now();
    let res = process(&audio, &opt);
    let secs = t.elapsed().as_secs_f64();
    if let Err(e) = res.write_wav(&out) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    eprintln!(
        "{} -> {}: {} -> {} frames, variant {}, {:.2} s ({:.2} s per output minute)",
        inp.display(),
        out.display(),
        audio.len(),
        res.len(),
        variant.name(),
        secs,
        secs / (res.len() as f64 / res.sr as f64 / 60.0)
    );
    ExitCode::SUCCESS
}

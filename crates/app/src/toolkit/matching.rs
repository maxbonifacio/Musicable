//! audio.match, which lines files up with a reference and matches their
//! loudness, as blind pairs need: each file written again to start where
//! the reference does and to be as long, at one loudness.

use std::collections::HashSet;
use std::path::PathBuf;

use musicable_analysis::loudness::Meter;
use musicable_commands::{Args, Param, Type};
use serde_json::{Value as Json, json};

use super::compare::{Audio, Lineup, best_offset, correlation_at, fits, load};
use super::{
    AudioCommand, CHUNK, FILES, Job, OVERWRITE, Output, Share, Source, decibels, describe, flag,
    holding, opt, round, schema,
};

/// How far a file may be from the reference, without max_offset_ms, and at
/// most.
const MAX_OFFSET_MS: f64 = 1000.0;
const MOST_OFFSET_MS: f64 = 10_000.0;
/// Which loudness to match.
const LOUDNESS: &[&str] = &["quietest", "loudest"];

pub(super) fn commands() -> Vec<AudioCommand> {
    vec![matching()]
}

fn matching() -> AudioCommand {
    AudioCommand {
        id: "audio.match",
        title: "Match Files to a Reference",
        about: "Lines files up with a reference and matches their loudness, as blind pairs \
                need: each file, and the reference, is written again into out_dir, under its \
                own name, starting where the reference does and as long, with silence where \
                it has none, and at one integrated loudness: the quietest's, the loudest's, or \
                to_lufs. A file is lined up where its samples correlate best with the \
                reference's, as audio.compare lines files up, and measured once it is, over the \
                reference's length. Each must have the reference's sample rate and channels.",
        params: vec![
            Param::required(
                "reference",
                Type::Text,
                "The file to line the others up with, by its path: a WAV, AIFF or FLAC file.",
            ),
            FILES,
            Param::required(
                "out_dir",
                Type::Text,
                "The folder to write the files into, by its path. Each keeps its name and its \
                 format.",
            ),
            Param::optional(
                "loudness",
                Type::Choice(LOUDNESS),
                "Which loudness to bring them all to: the quietest's, which needs no file \
                 louder than it was, or the loudest's. Without it or to_lufs, quietest.",
            ),
            Param::optional(
                "to_lufs",
                Type::Number,
                "The integrated loudness to bring them all to, in LUFS, instead.",
            ),
            Param::optional(
                "align",
                Type::Bool,
                "Lines each file up with the reference. Without it, true: --no-align leaves \
                 them where they start.",
            ),
            Param::optional(
                "max_offset_ms",
                Type::Number,
                "How far a file may be from the reference, either way, in milliseconds, up to \
                 10000. Without it, 1000.",
            ),
            Param::optional(
                "dither",
                Type::Bool,
                "Dithers samples written as integers, so what rounding loses is noise, not \
                 distortion. Without it, true.",
            ),
            OVERWRITE,
        ],
        positional: &["reference", "files"],
        read_only: false,
        example: &["tone.wav", "take.wav", "--out-dir", "matched"],
        run: run_match,
        output: || {
            let file = schema::object(
                "A file, lined up and matched.",
                &[
                    ("file", schema::text("Its whole path.")),
                    (
                        "offset_frames",
                        schema::number(
                            "How much later its sound came than the reference's, in frames: \
                             early if negative.",
                        ),
                    ),
                    (
                        "offset_ms",
                        schema::number("The same, in milliseconds, to 3 places."),
                    ),
                    (
                        "correlation",
                        schema::level(
                            "How alike its samples are to the reference's, lined up, from -1 \
                             to 1: null if either is silent where they overlap.",
                        ),
                    ),
                    (
                        "lufs",
                        schema::number(
                            "Its integrated loudness, lined up, before the gain, in LUFS.",
                        ),
                    ),
                    (
                        "gain_db",
                        schema::number("The gain it was given, in decibels."),
                    ),
                    (
                        "clipped_samples",
                        schema::count(
                            "Samples the gain took beyond full scale, which integers clip, and \
                             floats keep.",
                        ),
                    ),
                    ("out", schema::file("The file written.")),
                ],
            );
            schema::object(
                "The files, lined up and matched.",
                &[
                    (
                        "target_lufs",
                        schema::number("The loudness they were brought to, in LUFS."),
                    ),
                    ("reference", file.clone()),
                    (
                        "files",
                        schema::files(file, "Each file, in the order given."),
                    ),
                ],
            )
        },
    }
}

/// What the first pass found of a file: how to line it up, and how loud it
/// is then.
struct Found {
    offset: i64,
    correlation: Option<f64>,
    lufs: f64,
}

fn run_match(args: &Args, job: &Job) -> Result<Json, String> {
    let reference_path = job.file(args, "reference")?;
    let paths = job.files(args, "files")?;
    let out_dir = job.file(args, "out_dir")?;
    let overwrite = flag(args, "overwrite")?;
    let align = opt::<bool>(args, "align")?.unwrap_or(true);
    let max_offset_ms = opt::<f64>(args, "max_offset_ms")?.unwrap_or(MAX_OFFSET_MS);
    if !(0.0..=MOST_OFFSET_MS).contains(&max_offset_ms) {
        return Err(format!(
            "max_offset_ms is from 0 to {MOST_OFFSET_MS}, not {max_offset_ms}."
        ));
    }
    let to_lufs = opt::<f64>(args, "to_lufs")?;
    let loudest = match (opt::<String>(args, "loudness")?, to_lufs) {
        (Some(_), Some(_)) => return Err("Give loudness or to_lufs, not both.".to_owned()),
        (loudness, _) => loudness.as_deref() == Some("loudest"),
    };

    // Where each goes, which must all differ and be none of the files read.
    let mut all = vec![reference_path.clone()];
    all.extend(paths.iter().cloned());
    let inputs: Vec<&std::path::Path> = all.iter().map(PathBuf::as_path).collect();
    let mut names = HashSet::new();
    let mut outs = Vec::with_capacity(all.len());
    for path in &all {
        let name = path
            .file_name()
            .ok_or_else(|| format!("{} isn't a file.", path.display()))?;
        if !names.insert(name.to_owned()) {
            return Err(format!(
                "Two files are called {}, and out_dir would hold one: rename one first.",
                name.to_string_lossy()
            ));
        }
        let out = out_dir.join(name);
        Output::check(&out, overwrite, &inputs)?;
        outs.push(out);
    }

    let parts = all.len() * 2;
    let share = |i: usize| Share::ALL.part(i, parts);
    let mut reference = Source::open(&reference_path)?;
    fits(&reference, "match")?;
    let (rate, channels) = (reference.rate(), reference.channels());
    let reference_audio = load(&mut reference, job, share(0))?;
    let reference_mono = reference_audio.mono();
    let most = (max_offset_ms / 1000.0 * f64::from(rate)).round() as usize;

    // The first pass: how to line each up, and how loud it is then.
    let loudness = |audio: &Audio| {
        let mut meter = Meter::new(rate, channels).without_true_peak();
        let mut chunk = Vec::with_capacity(CHUNK * channels);
        for samples in audio.samples.chunks(CHUNK * channels) {
            chunk.clear();
            chunk.extend(samples.iter().map(|&sample| f64::from(sample)));
            meter.add(&chunk);
        }
        meter.finish().integrated_lufs
    };
    let silent = |path: &std::path::Path| {
        format!(
            "{} is silent, or too quiet to measure, so it has no loudness to match.",
            path.display()
        )
    };
    let reference_lufs = loudness(&reference_audio).ok_or_else(|| silent(&reference_path))?;
    let mut found: Vec<Result<Found, String>> = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        let first = (|| {
            let mut source = Source::open(path)?;
            same_shape(&source, rate, channels)?;
            fits(&source, "match")?;
            let audio = load(&mut source, job, share(i + 1).between(0.0, 0.5))?;
            let mono = audio.mono();
            let offset = match align {
                true => {
                    let share = share(i + 1).between(0.5, 1.0);
                    best_offset(&reference_mono, &mono, most, job, share)?.0
                }
                false => 0,
            };
            let correlation = Lineup::new(offset, reference_mono.len(), mono.len())
                .and_then(|lineup| correlation_at(&reference_mono, &mono, lineup));
            let lined = lined_up(&audio, offset, reference_audio.frames());
            let lufs = loudness(&lined).ok_or_else(|| silent(path))?;
            Ok(Found {
                offset,
                correlation,
                lufs,
            })
        })();
        if first.is_err() {
            job.check()?;
        }
        found.push(first);
    }
    let measured =
        std::iter::once(reference_lufs).chain(found.iter().flatten().map(|found| found.lufs));
    let target = match to_lufs {
        Some(lufs) => lufs,
        None if loudest => measured.fold(f64::NEG_INFINITY, f64::max),
        None => measured.fold(f64::INFINITY, f64::min),
    };

    // The second pass: each written again, lined up and at the target.
    let dither = opt::<bool>(args, "dither")?.unwrap_or(true);
    let write = |audio: &Audio, encoding, out: &std::path::Path, lufs: f64, share: Share| {
        let gain_db = target - lufs;
        if gain_db.abs() > 200.0 {
            return Err(format!(
                "That's a gain of {} dB, and audio.match makes from -200 to 200 dB.",
                round(gain_db, 2)
            ));
        }
        let gain = 10f64.powf(gain_db / 20.0);
        let format = super::format_of(out)?;
        let sample_format = holding(format, &[encoding])?;
        let mut output = Output::create(out, format, sample_format, rate, channels, dither)?;
        let mut chunk = Vec::with_capacity(CHUNK * channels);
        for samples in audio.samples.chunks(CHUNK * channels) {
            job.check()?;
            chunk.clear();
            chunk.extend(samples.iter().map(|&sample| f64::from(sample) * gain));
            output.write(&chunk)?;
        }
        let clipped = output.over;
        let path = output.finish()?;
        let described = describe(&path, &job.to_the_end(), share)?;
        Ok((gain_db, clipped, described))
    };
    let entry = |path: &std::path::Path, found: &Found, written: (f64, u64, Json)| {
        json!({
            "file": path.display().to_string(),
            "offset_frames": found.offset,
            "offset_ms": round(found.offset as f64 / f64::from(rate) * 1000.0, 3),
            "correlation": found.correlation.map_or(Json::Null, |c| json!(round(c, 6))),
            "lufs": round(found.lufs, 2),
            "gain_db": round(written.0, 2),
            "clipped_samples": written.1,
            "out": written.2,
        })
    };
    let reference_found = Found {
        offset: 0,
        correlation: Some(1.0),
        lufs: reference_lufs,
    };
    let written = write(
        &reference_audio,
        reference.encoding,
        &outs[0],
        reference_lufs,
        share(paths.len() + 1),
    )?;
    let reference_json = entry(&reference_path, &reference_found, written);
    drop(reference_audio);
    let mut files = Vec::with_capacity(paths.len());
    for (i, (path, first)) in paths.iter().zip(found).enumerate() {
        let second = first.and_then(|found| {
            let mut source = Source::open(path)?;
            let audio = load(
                &mut source,
                job,
                share(paths.len() + 2 + i).between(0.0, 0.5),
            )?;
            let lined = lined_up(&audio, found.offset, reference.frames() as usize);
            let written = write(
                &lined,
                source.encoding,
                &outs[i + 1],
                found.lufs,
                share(paths.len() + 2 + i).between(0.5, 1.0),
            )?;
            Ok(entry(path, &found, written))
        });
        match second {
            Ok(file) => files.push(file),
            Err(why) => {
                job.check()?;
                files.push(json!({"file": path.display().to_string(), "error": why}));
            }
        }
    }
    Ok(json!({
        "target_lufs": decibels(Some(target)),
        "reference": reference_json,
        "files": files,
    }))
}

/// Checks that `source` has `rate` and `channels`, as the reference has.
fn same_shape(source: &Source, rate: u32, channels: usize) -> Result<(), String> {
    if source.rate() != rate {
        return Err(format!(
            "{} is at {} Hz and the reference at {rate} Hz: convert it with audio.convert \
             first.",
            source.path.display(),
            source.rate()
        ));
    }
    if source.channels() != channels {
        return Err(format!(
            "{} has {} channels and the reference {channels}: convert it with audio.convert \
             first.",
            source.path.display(),
            source.channels()
        ));
    }
    Ok(())
}

/// `audio`, lined up as `offset` says, `frames` long: its frame `n +
/// offset` at frame `n`, and silence where it has none.
fn lined_up(audio: &Audio, offset: i64, frames: usize) -> Audio {
    let channels = audio.channels;
    let mut samples = vec![0.0f32; frames * channels];
    for (n, frame) in samples.chunks_exact_mut(channels).enumerate() {
        let theirs = n as i64 + offset;
        if theirs >= 0 && (theirs as usize) < audio.frames() {
            let at = theirs as usize * channels;
            frame.copy_from_slice(&audio.samples[at..at + channels]);
        }
    }
    Audio { samples, channels }
}

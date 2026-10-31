//! The commands that write files: cut, join, convert and gain, and info,
//! which describes them.

use std::f64::consts::{FRAC_PI_2, PI};
use std::path::{Path, PathBuf};

use musicable_commands::{Args, Param, Type};
use musicable_io::audio::{Encoding, Format, SampleFormat};
use serde_json::{Value as Json, json};

use super::resample::Resampler;
use super::{
    AudioCommand, FILE, FILES, Job, MOST, OUT, OVERWRITE, Output, PATHS, Share, Source, arg,
    decibels, describe, each_file, flac_holds, flag, holding, opt, position, range, round,
    same_frames, schema, stretch_params,
};

pub(super) fn commands() -> Vec<AudioCommand> {
    vec![info(), cut(), join(), convert(), gain()]
}

fn info() -> AudioCommand {
    AudioCommand {
        id: "audio.info",
        title: "Describe Audio Files",
        about: "Says what each file holds: its format, how it stores its samples, its channels, \
                sample rate and length, its peak, the tempo its ACID chunk gives, and the MD5 \
                of its samples.",
        params: vec![FILES],
        positional: &["files"],
        read_only: true,
        example: &["tone.wav", "loop.flac", "voice.aif"],
        run: |args, job| {
            let paths = job.files(args, "files")?;
            let files = each_file(&paths, job, |path, share| describe(path, job, share))?;
            Ok(json!({"files": files}))
        },
        output: || {
            schema::object(
                "What each file holds.",
                &[(
                    "files",
                    schema::files(schema::file("A file."), "Each file, in the order given."),
                )],
            )
        },
    }
}

fn cut() -> AudioCommand {
    let mut params = vec![FILE, OUT];
    params.extend(stretch_params(true));
    params.extend([
        Param::optional(
            "fade_ms",
            Type::Number,
            "Fades the stretch in and out over this many milliseconds each, with a raised \
             cosine, so its ends don't click. Without it, every frame is copied unchanged.",
        ),
        OVERWRITE,
    ]);
    AudioCommand {
        id: "audio.cut",
        title: "Cut Audio File",
        about: "Copies a stretch of a file to a new file, its frames unchanged, stored as the \
                file stores them, and reads the new file back to check them. The stretch starts \
                at from or from_frame, and ends at to or to_frame, or after length or \
                length_frames: seconds, or frames for a cut to the frame.",
        params,
        positional: &["file"],
        read_only: false,
        example: &[
            "tone.wav", "--from", "0.5", "--length", "1", "--out", "cut.wav",
        ],
        run: run_cut,
        output: || {
            schema::object(
                "The stretch cut, and the new file.",
                &[
                    (
                        "from",
                        schema::position("Where the stretch starts in the file."),
                    ),
                    (
                        "to",
                        schema::position("Where the stretch ends: the frame after its last."),
                    ),
                    ("out", schema::file("The new file.")),
                    (
                        "exact",
                        schema::boolean(
                            "Whether the new file's frames read back as the stretch's, sample \
                             for sample, but for its fades.",
                        ),
                    ),
                ],
            )
        },
    }
}

fn run_cut(args: &Args, job: &Job) -> Result<Json, String> {
    let input = job.file(args, "file")?;
    let out = job.file(args, "out")?;
    let format = Output::check(&out, flag(args, "overwrite")?, &[&input])?;
    let mut source = Source::open(&input)?;
    let (start, end) = range(args, &source, true)?;
    let sample_format = holding(format, &[source.encoding])?;
    let (rate, channels, frames) = (source.rate(), source.channels(), end - start);
    let fade = match opt::<f64>(args, "fade_ms")? {
        Some(ms) if ms < 0.0 => {
            return Err("fade_ms is a number of milliseconds, 0 or more.".to_owned());
        }
        Some(ms) => frames_in(ms / 1000.0, rate).min(frames / 2),
        None => 0,
    };
    let mut output = Output::create(&out, format, sample_format, rate, channels, false)?;
    let mut at = 0;
    let mut faded = Vec::new();
    source.each_chunk(start, end, job, Share::ALL.between(0.0, 0.6), |samples| {
        let first = at;
        at += (samples.len() / channels) as u64;
        // Only the frames in the fades change.
        if first >= fade && at <= frames - fade {
            return output.write(samples);
        }
        faded.clear();
        faded.extend_from_slice(samples);
        for (i, frame) in faded.chunks_exact_mut(channels).enumerate() {
            let gain = fade_gain(first + i as u64, frames, fade);
            frame.iter_mut().for_each(|sample| *sample *= gain);
        }
        output.write(&faded)
    })?;
    let out = output.finish()?;
    let job = job.to_the_end();
    let mut copy = Source::open(&out)?;
    let fades = [(start, start + fade), (end - fade, end)];
    let checking = Share::ALL.between(0.6, 0.8);
    let exact = copy.rate() == rate
        && copy.frames() == frames
        && same_frames(
            &mut source,
            start,
            &mut copy,
            0,
            frames,
            &fades,
            &job,
            checking,
        )?;
    Ok(json!({
        "from": position(start, rate),
        "to": position(end, rate),
        "out": describe(&out, &job, Share::ALL.between(0.8, 1.0))?,
        "exact": exact,
    }))
}

/// `seconds` at `rate`, in whole frames.
fn frames_in(seconds: f64, rate: u32) -> u64 {
    (seconds * f64::from(rate)).round() as u64
}

/// The gain of frame `n` of `frames` frames that fade in and out over
/// `fade` frames each, along a raised cosine: 0 at either end.
fn fade_gain(n: u64, frames: u64, fade: u64) -> f64 {
    let edge = n.min(frames - 1 - n);
    match edge < fade {
        true => 0.5 - 0.5 * (PI * edge as f64 / fade as f64).cos(),
        false => 1.0,
    }
}

fn join() -> AudioCommand {
    AudioCommand {
        id: "audio.join",
        title: "Join Audio Files",
        about: "Joins files end to end in a new file, or plays them over and over, as for \
                looping a drum break: with silence between them, or each crossfading into the \
                next. They must have the same sample rate and channels. The new file stores \
                samples as the widest of them do, so frames outside the crossfades are copied \
                unchanged, which it reads back to check.",
        params: vec![
            Param::required(
                "files",
                PATHS,
                "The audio files to join, in order, by their paths: WAV, AIFF or FLAC files, \
                 200 at most. A file can be there more than once.",
            ),
            OUT,
            Param::optional(
                "repeat",
                Type::Int,
                "How many times to play the files, one after another: 4 plays them four \
                 times. Without it, once.",
            ),
            Param::optional(
                "gap",
                Type::Number,
                "Seconds of silence between one file and the next. Without it, none.",
            ),
            Param::optional(
                "crossfade_ms",
                Type::Number,
                "Crossfades each file into the next over this many milliseconds, keeping \
                 their power the same, instead of a gap. They overlap by as much, so the new \
                 file is shorter.",
            ),
            OVERWRITE,
        ],
        positional: &["files"],
        read_only: false,
        example: &[
            "loop.flac",
            "tone.wav",
            "--repeat",
            "2",
            "--crossfade-ms",
            "10",
            "--out",
            "joined.flac",
        ],
        run: run_join,
        output: || {
            let part = schema::object(
                "A file's place in the new file.",
                &[
                    ("file", schema::text("The file's whole path.")),
                    ("from", schema::position("Where it starts in the new file.")),
                    (
                        "to",
                        schema::position(
                            "Where it ends in the new file, the frame after its last: it \
                             overlaps the next by a crossfade.",
                        ),
                    ),
                ],
            );
            schema::object(
                "Where each file is in the new file, and the new file.",
                &[
                    (
                        "parts",
                        schema::list(
                            part,
                            "Each file's place in the new file, in order: the first 200 if \
                             there are more.",
                        ),
                    ),
                    (
                        "count",
                        schema::count("How many parts there are: the files, times repeat."),
                    ),
                    ("out", schema::file("The new file.")),
                    (
                        "exact",
                        schema::boolean(
                            "Whether each part's frames read back as its file's, sample for \
                             sample, but for crossfades.",
                        ),
                    ),
                ],
            )
        },
    }
}

fn run_join(args: &Args, job: &Job) -> Result<Json, String> {
    let paths = job.files(args, "files")?;
    let out = job.file(args, "out")?;
    let repeat = match opt::<i64>(args, "repeat")? {
        None => 1,
        Some(repeat @ 1..=10_000) => repeat as usize,
        Some(_) => {
            return Err("repeat is how many times to play the files, from 1 to 10000.".to_owned());
        }
    };
    let gap = opt::<f64>(args, "gap")?;
    let crossfade = opt::<f64>(args, "crossfade_ms")?;
    if gap.is_some() && crossfade.is_some() {
        return Err("Give gap or crossfade_ms, not both.".to_owned());
    }
    if gap.is_some_and(|gap| gap < 0.0) {
        return Err("gap is a number of seconds, 0 or more.".to_owned());
    }
    if crossfade.is_some_and(|ms| ms < 0.0) {
        return Err("crossfade_ms is a number of milliseconds, 0 or more.".to_owned());
    }
    let inputs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let format = Output::check(&out, flag(args, "overwrite")?, &inputs)?;
    let mut sources: Vec<Source> = (paths.iter())
        .map(|path| Source::open(path))
        .collect::<Result<_, _>>()?;
    let (rate, channels) = (sources[0].rate(), sources[0].channels());
    let first = sources[0].path.display();
    for source in &sources[1..] {
        if source.rate() != rate {
            return Err(format!(
                "{} is at {} Hz, and {first} at {rate} Hz: give them the same rate with \
                 audio.convert first.",
                source.path.display(),
                source.rate(),
            ));
        }
        if source.channels() != channels {
            return Err(format!(
                "{} has {} channels, and {first} {channels}: give them the same with \
                 audio.convert first.",
                source.path.display(),
                source.channels(),
            ));
        }
    }
    let encodings: Vec<Encoding> = sources.iter().map(|source| source.encoding).collect();
    let sample_format = holding(format, &encodings)?;
    let gap = gap.map_or(0, |gap| frames_in(gap, rate));
    let fade = crossfade.map_or(0, |ms| frames_in(ms / 1000.0, rate));
    let order: Vec<usize> = (0..repeat).flat_map(|_| 0..sources.len()).collect();
    let parts = order.len();
    // The frames of part `k` that crossfade into it and out of it.
    let fades = |k: usize| {
        let into = if k > 0 { fade } else { 0 };
        let out_of = if k + 1 < parts { fade } else { 0 };
        (into, out_of)
    };
    for (k, &i) in order.iter().enumerate() {
        let (into, out_of) = fades(k);
        let frames = sources[i].frames();
        if frames < into + out_of {
            return Err(format!(
                "{} is shorter than its crossfades: it's {} s long, and they take {} s of it.",
                sources[i].path.display(),
                round(frames as f64 / f64::from(rate), 6),
                round((into + out_of) as f64 / f64::from(rate), 6),
            ));
        }
    }

    let mut output = Output::create(&out, format, sample_format, rate, channels, false)?;
    let mut written = 0;
    let mut starts = Vec::with_capacity(parts);
    // The end of the part before, which crossfades into the next.
    let mut held: Vec<f64> = Vec::new();
    let mut mixed: Vec<f64> = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        let share = Share::ALL.between(0.0, 0.7).part(k, parts);
        if k > 0 && gap > 0 {
            output.silence(gap, channels)?;
            written += gap;
        }
        let source = &mut sources[i];
        let frames = source.frames();
        let (into, out_of) = fades(k);
        starts.push(written);
        if into > 0 {
            mixed.clear();
            source.each_chunk(0, into, job, share, |samples| {
                mixed.extend_from_slice(samples);
                Ok(())
            })?;
            let pairs = mixed
                .chunks_exact_mut(channels)
                .zip(held.chunks_exact(channels));
            for (n, (ours, theirs)) in pairs.enumerate() {
                let along = FRAC_PI_2 * (n as f64 + 0.5) / into as f64;
                let (rise, fall) = along.sin_cos();
                for (ours, theirs) in ours.iter_mut().zip(theirs) {
                    *ours = *ours * rise + theirs * fall;
                }
            }
            output.write(&mixed)?;
        }
        source.each_chunk(into, frames - out_of, job, share, |samples| {
            output.write(samples)
        })?;
        written += frames - out_of;
        held.clear();
        source.each_chunk(frames - out_of, frames, job, share, |samples| {
            held.extend_from_slice(samples);
            Ok(())
        })?;
    }
    let out = output.finish()?;

    let job = job.to_the_end();
    let mut copy = Source::open(&out)?;
    let mut exact = copy.frames() == written;
    for (k, &i) in order.iter().enumerate() {
        let (into, out_of) = fades(k);
        let frames = sources[i].frames() - into - out_of;
        let share = Share::ALL.between(0.7, 0.9).part(k, parts);
        let at = starts[k] + into;
        exact = exact
            && same_frames(
                &mut sources[i],
                into,
                &mut copy,
                at,
                frames,
                &[],
                &job,
                share,
            )?;
    }
    let listed: Vec<Json> = (order.iter().zip(&starts).take(MOST))
        .map(|(&i, &start)| {
            json!({
                "file": paths[i].display().to_string(),
                "from": position(start, rate),
                "to": position(start + sources[i].frames(), rate),
            })
        })
        .collect();
    Ok(json!({
        "parts": listed,
        "count": parts,
        "out": describe(&out, &job, Share::ALL.between(0.9, 1.0))?,
        "exact": exact,
    }))
}

/// The encodings `audio.convert` writes.
pub(super) const ENCODINGS: &[&str] = &["int8", "int16", "int24", "int32", "float32", "float64"];

fn convert() -> AudioCommand {
    AudioCommand {
        id: "audio.convert",
        title: "Convert Audio File",
        about: "Writes a file again at another sample rate, in another encoding, with other \
                channels or in another format, which out's extension says. Resampling keeps \
                everything up to 20 kHz from 44.1 kHz, and integers too narrow for the samples \
                are dithered. A conversion that changes only the format, widens the samples or \
                makes a mono file stereo copies them exactly, which it reads back to check.",
        params: vec![
            FILE,
            OUT,
            Param::optional(
                "rate",
                Type::Int,
                "The sample rate to write, in hertz, such as 48000. Without it, the file's.",
            ),
            Param::optional(
                "encoding",
                Type::Choice(ENCODINGS),
                "How to store the samples: int8 to int32 for integers of that many bits, or \
                 float32 or float64. FLAC files take integers of up to 24 bits. Without it, as \
                 the file does.",
            ),
            Param::optional(
                "channels",
                Type::Int,
                "1 mixes the channels down to one, and 2 copies a mono file's channel to two. \
                 Without it, as many as the file has.",
            ),
            Param::optional(
                "dither",
                Type::Bool,
                "Dithers samples written as integers too narrow to hold them exactly, so what \
                 rounding loses is noise, not distortion. Without it, true.",
            ),
            OVERWRITE,
        ],
        positional: &["file"],
        read_only: false,
        example: &[
            "tone.wav",
            "--rate",
            "48000",
            "--encoding",
            "int16",
            "--out",
            "tone-48k.flac",
        ],
        run: run_convert,
        output: || {
            schema::object(
                "The new file.",
                &[
                    ("out", schema::file("The new file.")),
                    (
                        "clipped_samples",
                        schema::count(
                            "Samples beyond full scale, which integers clip, and floats keep.",
                        ),
                    ),
                    (
                        "dithered",
                        schema::boolean("Whether the samples were dithered."),
                    ),
                    (
                        "exact",
                        schema::boolean(
                            "Whether the new file's samples read back as the file's, sample for \
                             sample: so when only the format changed, the samples got wider, or \
                             a mono file's channel was copied to two.",
                        ),
                    ),
                ],
            )
        },
    }
}

fn run_convert(args: &Args, job: &Job) -> Result<Json, String> {
    let input = job.file(args, "file")?;
    let out = job.file(args, "out")?;
    let format = Output::check(&out, flag(args, "overwrite")?, &[&input])?;
    let mut source = Source::open(&input)?;
    let (from_rate, from_channels, frames) = (source.rate(), source.channels(), source.frames());
    let rate = match opt::<i64>(args, "rate")? {
        None => from_rate,
        Some(rate @ 1_000..=768_000) => rate as u32,
        Some(_) => return Err("rate is a sample rate in hertz, from 1000 to 768000.".to_owned()),
    };
    let channels = match opt::<i64>(args, "channels")? {
        None => from_channels,
        Some(n) if n == from_channels as i64 => from_channels,
        Some(1) => 1,
        Some(2) if from_channels == 1 => 2,
        Some(n) => {
            return Err(format!(
                "audio.convert mixes channels down to 1, or copies a mono file's channel to 2, \
                 so it can't make {n} channels of {}'s {from_channels}.",
                input.display()
            ));
        }
    };
    let sample_format = match opt::<String>(args, "encoding")? {
        Some(name) => {
            let sample_format = sample_format_named(&name);
            if format == Format::Flac && !flac_holds(sample_format) {
                return Err(format!(
                    "FLAC files hold integers of up to 24 bits, not {name} samples: give \
                     encoding int24, int16 or int8, or write a WAV or AIFF file."
                ));
            }
            sample_format
        }
        None => holding(format, &[source.encoding]).map_err(|_| {
            format!(
                "FLAC files hold integers of up to 24 bits, and {} holds {} samples: give \
                 encoding int24 or int16 to write them as those.",
                input.display(),
                source.encoding.name()
            )
        })?,
    };
    let (resampling, remixing) = (rate != from_rate, channels != from_channels);
    // Mixing down changes the samples, and copying a mono file's channel
    // to two doesn't.
    let mixing_down = channels < from_channels;
    let unchanged = !resampling && !mixing_down && sample_format.holds(source.encoding);
    let dither = !unchanged && !sample_format.is_float() && opt(args, "dither")?.unwrap_or(true);

    let mut output = Output::create(&out, format, sample_format, rate, channels, dither)?;
    let mut resampler = match resampling {
        true => Some(Resampler::new(from_rate, rate, channels)?),
        false => None,
    };
    let mut mixed = Vec::new();
    source.each_chunk(0, frames, job, Share::ALL.between(0.0, 0.8), |samples| {
        let samples = match remixing {
            true => {
                remix(samples, from_channels, channels, &mut mixed);
                &mixed
            }
            false => samples,
        };
        match &mut resampler {
            Some(resampler) => resampler.process(samples, &mut |samples| output.write(samples)),
            None => output.write(samples),
        }
    })?;
    if let Some(resampler) = resampler {
        resampler.finish(&mut |samples| output.write(samples))?;
    }
    let clipped = output.over;
    let out = output.finish()?;

    let job = job.to_the_end();
    let checking = Share::ALL.between(0.8, 0.9);
    let exact = unchanged && {
        let mut copy = Source::open(&out)?;
        copy.frames() == frames
            && same_frames(&mut source, 0, &mut copy, 0, frames, &[], &job, checking)?
    };
    Ok(json!({
        "out": describe(&out, &job, Share::ALL.between(0.9, 1.0))?,
        "clipped_samples": clipped,
        "dithered": dither,
        "exact": exact,
    }))
}

/// The sample format `audio.convert`'s encoding names.
pub(super) fn sample_format_named(name: &str) -> SampleFormat {
    use SampleFormat::{Float32, Float64, Int8, Int16, Int24, Int32};
    ([Int8, Int16, Int24, Int32, Float32, Float64].into_iter())
        .find(|sample_format| sample_format.name() == name)
        .expect("the encoding is one of the encodings")
}

/// Puts `samples` of `from` channels in `out` as `to` channels: mixed down
/// to one, their mean, or one copied to each.
fn remix(samples: &[f64], from: usize, to: usize, out: &mut Vec<f64>) {
    out.clear();
    for frame in samples.chunks_exact(from) {
        match to {
            1 => out.push(frame.iter().sum::<f64>() / from as f64),
            _ => out.extend(std::iter::repeat_n(frame[0], to)),
        }
    }
}

fn gain() -> AudioCommand {
    AudioCommand {
        id: "audio.gain",
        title: "Change Audio Level",
        about: "Writes a file again louder or quieter: by a number of decibels with db, or to \
                an integrated loudness, a sample peak or a true peak with to_lufs, \
                to_peak_dbfs or to_true_peak_dbtp, or to another file's loudness with like. \
                Give one of them. It measures the new file, and says how many samples went \
                beyond full scale, which integers clip.",
        params: vec![
            FILE,
            OUT,
            Param::optional(
                "db",
                Type::Number,
                "The gain, in decibels: 6 doubles the level, and -6 halves it.",
            ),
            Param::optional(
                "to_lufs",
                Type::Number,
                "The integrated loudness to bring the file to, in LUFS, such as -14 or -23.",
            ),
            Param::optional(
                "to_peak_dbfs",
                Type::Number,
                "The sample peak to bring the file to, in dBFS, such as -1.",
            ),
            Param::optional(
                "to_true_peak_dbtp",
                Type::Number,
                "The true peak to bring the file to, in dBTP, such as -1.",
            ),
            Param::optional(
                "like",
                Type::Text,
                "Another audio file, by its path, whose integrated loudness to bring the file \
                 to.",
            ),
            Param::optional(
                "dither",
                Type::Bool,
                "Dithers samples written as integers, so what rounding loses is noise, not \
                 distortion. Without it, true.",
            ),
            OVERWRITE,
        ],
        positional: &["file"],
        read_only: false,
        example: &["voice.aif", "--to-lufs", "-23", "--out", "voice-23.wav"],
        run: run_gain,
        output: || {
            schema::object(
                "The gain, and the new file's levels.",
                &[
                    (
                        "gain_db",
                        schema::number("The gain, in decibels, to 2 places."),
                    ),
                    (
                        "integrated_lufs",
                        schema::level("The new file's integrated loudness, in LUFS."),
                    ),
                    (
                        "sample_peak_dbfs",
                        schema::level("The new file's loudest sample, in dBFS."),
                    ),
                    (
                        "true_peak_dbtp",
                        schema::level(
                            "The new file's true peak, between its samples too, in dBTP.",
                        ),
                    ),
                    (
                        "clipped_samples",
                        schema::count(
                            "Samples the gain took beyond full scale, which integers clip, and \
                             floats keep.",
                        ),
                    ),
                    ("out", schema::file("The new file.")),
                ],
            )
        },
    }
}

fn run_gain(args: &Args, job: &Job) -> Result<Json, String> {
    let input = job.file(args, "file")?;
    let out = job.file(args, "out")?;
    let like = (opt::<String>(args, "like")?)
        .map(|text| job.path(&text))
        .transpose()?;
    let mut inputs = vec![input.as_path()];
    inputs.extend(like.as_deref());
    let format = Output::check(&out, flag(args, "overwrite")?, &inputs)?;
    let ways = ["db", "to_lufs", "to_peak_dbfs", "to_true_peak_dbtp", "like"];
    let given: Vec<&str> = ways
        .into_iter()
        .filter(|way| args.value(way).is_some())
        .collect();
    let [way] = given.as_slice() else {
        return Err(
            "Give one of db, to_lufs, to_peak_dbfs, to_true_peak_dbtp or like, which says how \
             much gain."
                .to_owned(),
        );
    };
    let mut source = Source::open(&input)?;
    let sample_format = holding(format, &[source.encoding])?;
    let (rate, channels, frames) = (source.rate(), source.channels(), source.frames());
    let measuring = Share::ALL.between(0.0, 0.3);
    let silent = |to: String| {
        format!(
            "{} is silent, or too quiet to measure, so no gain brings it to {to}.",
            input.display()
        )
    };
    let gain_db = match *way {
        "db" => arg::<f64>(args, "db")?,
        "to_lufs" => {
            let target = arg::<f64>(args, "to_lufs")?;
            let meter = source.meter().without_true_peak();
            let levels = source.measure((0, frames), meter, job, measuring)?;
            let lufs = levels.integrated_lufs;
            target - lufs.ok_or_else(|| silent(format!("{target} LUFS")))?
        }
        "to_peak_dbfs" => {
            let target = arg::<f64>(args, "to_peak_dbfs")?;
            let meter = source.meter().without_true_peak();
            let levels = source.measure((0, frames), meter, job, measuring)?;
            let peak = levels.sample_peak_dbfs;
            target - peak.ok_or_else(|| silent(format!("a peak of {target} dBFS")))?
        }
        "to_true_peak_dbtp" => {
            let target = arg::<f64>(args, "to_true_peak_dbtp")?;
            let meter = source.meter();
            let levels = source.measure((0, frames), meter, job, measuring)?;
            let peak = levels.true_peak_dbtp;
            target - peak.ok_or_else(|| silent(format!("a true peak of {target} dBTP")))?
        }
        _ => {
            let like = like.as_deref().expect("like is given");
            let mut other = Source::open(like)?;
            let meter = other.meter().without_true_peak();
            let theirs =
                other.measure((0, other.frames()), meter, job, measuring.between(0.0, 0.5))?;
            let theirs = theirs.integrated_lufs.ok_or_else(|| {
                format!(
                    "{} is silent, or too quiet to measure, so it has no loudness to match.",
                    like.display()
                )
            })?;
            let meter = source.meter().without_true_peak();
            let ours = source.measure((0, frames), meter, job, measuring.between(0.5, 1.0))?;
            let lufs = ours.integrated_lufs;
            theirs - lufs.ok_or_else(|| silent(format!("{} LUFS", round(theirs, 2))))?
        }
    };
    if gain_db.abs() > 200.0 {
        return Err(format!(
            "That's a gain of {} dB, and audio.gain makes from -200 to 200 dB.",
            round(gain_db, 2)
        ));
    }
    let gain = 10f64.powf(gain_db / 20.0);
    let dither = gain != 1.0 && opt(args, "dither")?.unwrap_or(true);

    let mut output = Output::create(&out, format, sample_format, rate, channels, dither)?;
    let mut louder = Vec::new();
    source.each_chunk(0, frames, job, Share::ALL.between(0.3, 0.8), |samples| {
        louder.clear();
        louder.extend(samples.iter().map(|sample| sample * gain));
        output.write(&louder)
    })?;
    let clipped = output.over;
    let out = output.finish()?;

    let job = job.to_the_end();
    let mut copy = Source::open(&out)?;
    let meter = copy.meter();
    let after = copy.measure(
        (0, copy.frames()),
        meter,
        &job,
        Share::ALL.between(0.8, 0.95),
    )?;
    Ok(json!({
        "gain_db": round(gain_db, 2),
        "integrated_lufs": decibels(after.integrated_lufs),
        "sample_peak_dbfs": decibels(after.sample_peak_dbfs),
        "true_peak_dbtp": decibels(after.true_peak_dbtp),
        "clipped_samples": clipped,
        "out": describe(&out, &job, Share::ALL.between(0.95, 1.0))?,
    }))
}

//! The commands that measure: loudness, and find, which finds stretches by
//! how loud they are.

use musicable_analysis::loudness::{Block, Meter, block_start, energy_to_lufs};
use musicable_commands::{Args, Param, Type};
use serde_json::{Value as Json, json};

use super::{
    AudioCommand, FILE, FILES, Job, MOST, Share, Source, arg, decibels, each_file, level, opt,
    position, range, round, schema, seconds, stretch_params,
};

pub(super) fn commands() -> Vec<AudioCommand> {
    vec![loudness(), find()]
}

fn loudness() -> AudioCommand {
    let mut params = vec![FILES];
    params.extend(stretch_params(false));
    AudioCommand {
        id: "audio.loudness",
        title: "Measure Loudness",
        about: "Measures each file's loudness as ITU-R BS.1770-4 and EBU R 128 do: its \
                integrated loudness and loudness range, and its loudest momentary and \
                short-term loudness, with its sample peak, true peak, RMS level, DC offset and \
                clipped samples. With from or to, it measures that stretch of each file.",
        params,
        positional: &["files"],
        read_only: true,
        example: &["tone.wav", "voice.aif"],
        run: run_loudness,
        output: || {
            let levels = schema::object(
                "A file's levels.",
                &[
                    ("file", schema::text("Its whole path.")),
                    (
                        "seconds",
                        schema::number("How long the stretch measured is, in seconds."),
                    ),
                    (
                        "integrated_lufs",
                        schema::level(
                            "Its integrated loudness, in LUFS, gated as BS.1770-4 gates it: \
                             null without 0.4 s louder than -70 LUFS.",
                        ),
                    ),
                    (
                        "range_lu",
                        schema::level(
                            "Its loudness range, in LU, as EBU Tech 3342 measures it: null \
                             without 3 s louder than -70 LUFS.",
                        ),
                    ),
                    (
                        "momentary_max_lufs",
                        schema::level("Its loudest momentary loudness, over 0.4 s, in LUFS."),
                    ),
                    (
                        "short_term_max_lufs",
                        schema::level("Its loudest short-term loudness, over 3 s, in LUFS."),
                    ),
                    (
                        "sample_peak_dbfs",
                        schema::level("Its loudest sample, in dBFS."),
                    ),
                    (
                        "true_peak_dbtp",
                        schema::level(
                            "Its true peak, between its samples too, in dBTP: oversampled 4 \
                             times, as BS.1770-4 has it.",
                        ),
                    ),
                    (
                        "rms_dbfs",
                        schema::level(
                            "The RMS level of its samples, in dBFS: a full-scale sine reads \
                             -3.01.",
                        ),
                    ),
                    (
                        "dc_offset",
                        schema::number(
                            "The mean of the channel whose mean is furthest from 0, to 6 \
                             places: 0 without a DC offset.",
                        ),
                    ),
                    (
                        "clipped_samples",
                        schema::count("Its samples at full scale or beyond, either way."),
                    ),
                ],
            );
            schema::object(
                "Each file's levels.",
                &[(
                    "files",
                    schema::files(levels, "Each file's levels, in the order given."),
                )],
            )
        },
    }
}

fn run_loudness(args: &Args, job: &Job) -> Result<Json, String> {
    let paths = job.files(args, "files")?;
    let files = each_file(&paths, job, |path, share| {
        let mut source = Source::open(path)?;
        let stretch = range(args, &source, false)?;
        let meter = source.meter();
        let levels = source.measure(stretch, meter, job, share)?;
        Ok(json!({
            "file": path.display().to_string(),
            "seconds": seconds(stretch.1 - stretch.0, source.rate()),
            "integrated_lufs": decibels(levels.integrated_lufs),
            "range_lu": decibels(levels.range_lu),
            "momentary_max_lufs": decibels(levels.momentary_max_lufs),
            "short_term_max_lufs": decibels(levels.short_term_max_lufs),
            "sample_peak_dbfs": decibels(levels.sample_peak_dbfs),
            "true_peak_dbtp": decibels(levels.true_peak_dbtp),
            "rms_dbfs": decibels(levels.rms_dbfs),
            "dc_offset": round(levels.dc_offset, 6),
            "clipped_samples": levels.clipped_samples,
        }))
    })?;
    Ok(json!({"files": files}))
}

/// What `audio.find` finds.
const WHATS: &[&str] = &["loudest", "quietest", "sound", "silence"];

fn find() -> AudioCommand {
    let mut params = vec![
        FILE,
        Param::required(
            "what",
            Type::Choice(WHATS),
            "What to find: the loudest or quietest stretches of a length, or each stretch of \
             sound, or of silence between sounds.",
        ),
        Param::optional(
            "length",
            Type::Number,
            "How long the loudest or quietest stretches are, in seconds. Without it, 10, or \
             the whole file if it's shorter.",
        ),
        Param::optional(
            "count",
            Type::Int,
            "How many stretches to give at most, from 1 to 200. Without it, the loudest or \
             quietest, or the first 200 sounds or silences.",
        ),
        Param::optional(
            "threshold_dbfs",
            Type::Number,
            "How loud a sample must be to be sound, in dBFS. Without it, -60.",
        ),
        Param::optional(
            "min_silence",
            Type::Number,
            "The shortest silence, in seconds: shorter ones are part of the sound around \
             them. Without it, 0.5.",
        ),
    ];
    params.extend(stretch_params(false));
    AudioCommand {
        id: "audio.find",
        title: "Find Stretches of Audio",
        about: "Finds stretches of a file by how loud they are: the loudest or quietest \
                stretches of a length that don't overlap, loudest or quietest first, or the \
                stretches of sound, or of silence, in order. It says where the sound starts and \
                ends too, to the frame. The quietest are looked for between the first sound and \
                the last. With from or to, it looks in that stretch of the file.",
        params,
        positional: &["file"],
        read_only: true,
        example: &["tone.wav", "--what", "loudest", "--length", "0.5"],
        run: run_find,
        output: || {
            let stretch = schema::object(
                "A stretch of the file.",
                &[
                    ("from", schema::position("Where it starts.")),
                    (
                        "to",
                        schema::position("Where it ends: the frame after its last."),
                    ),
                    ("seconds", schema::number("How long it is.")),
                    (
                        "loudness_lufs",
                        schema::level(
                            "Its loudness, in LUFS: the mean power of its K-weighted samples, \
                             as BS.1770-4 weights them, ungated.",
                        ),
                    ),
                    ("peak_dbfs", schema::level("Its loudest sample, in dBFS.")),
                ],
            );
            let sound = schema::object(
                "Where the sound is.",
                &[
                    ("from", schema::position("Where the first sound starts.")),
                    (
                        "to",
                        schema::position("Where the last sound ends: the frame after it."),
                    ),
                ],
            );
            schema::object(
                "The stretches found.",
                &[
                    ("what", schema::choice(WHATS, "What was looked for.")),
                    (
                        "stretches",
                        schema::list(
                            stretch,
                            "The stretches found: the loudest or quietest first, or sounds or \
                             silences in order.",
                        ),
                    ),
                    (
                        "found",
                        schema::count(
                            "How many there are in all, for sounds and silences: those after \
                             the first count, or 200, aren't listed.",
                        ),
                    ),
                    (
                        "sound",
                        schema::or_null(
                            sound,
                            "Where the first sound starts and the last ends, to the frame: \
                             null if it's silent throughout.",
                        ),
                    ),
                ],
            )
        },
    }
}

fn run_find(args: &Args, job: &Job) -> Result<Json, String> {
    let path = job.file(args, "file")?;
    let what: String = arg(args, "what")?;
    let extremes = matches!(what.as_str(), "loudest" | "quietest");
    let length = match opt::<f64>(args, "length")? {
        None => 10.0,
        Some(length) if length > 0.0 => length,
        Some(_) => return Err("length is a number of seconds, more than 0.".to_owned()),
    };
    let count = match opt::<i64>(args, "count")? {
        None if extremes => 1,
        None => MOST,
        Some(count @ 1..=200) => count as usize,
        Some(_) => return Err("count is how many stretches to give, from 1 to 200.".to_owned()),
    };
    let threshold = 10f64.powf(opt::<f64>(args, "threshold_dbfs")?.unwrap_or(-60.0) / 20.0);
    let min_silence = match opt::<f64>(args, "min_silence")? {
        None => 0.5,
        Some(seconds) if seconds >= 0.0 => seconds,
        Some(_) => return Err("min_silence is a number of seconds, 0 or more.".to_owned()),
    };
    let mut source = Source::open(&path)?;
    let (from, to) = range(args, &source, false)?;
    let rate = source.rate();
    let meter = Meter::new(rate, source.channels())
        .keeping_blocks()
        .without_true_peak();
    let levels = source.measure((from, to), meter, job, Share::ALL.between(0.0, 0.9))?;
    let blocks = Blocks {
        list: &levels.blocks,
        from,
        to,
        rate,
    };

    let runs = blocks.sounds(threshold, (min_silence * f64::from(rate)).round() as u64);
    // The sounds to the frame, as far as they're listed, and the last.
    let listed = runs.len().min(count + 1);
    let sounds = (runs[..listed].iter())
        .map(|&run| blocks.refine(run, threshold, &mut source))
        .collect::<Result<Vec<_>, String>>()?;
    let last = match runs.last() {
        Some(&run) => Some(blocks.refine(run, threshold, &mut source)?),
        None => None,
    };
    let sound = sounds.first().zip(last).map(
        |(first, last)| json!({"from": position(first.0, rate), "to": position(last.1, rate)}),
    );

    let (stretches, found) = match what.as_str() {
        "loudest" | "quietest" => {
            let quietest = what == "quietest";
            let within = match quietest {
                true => runs
                    .first()
                    .zip(runs.last())
                    .map_or((0, 0), |(a, b)| (a.0, b.1)),
                false => (0, blocks.list.len()),
            };
            let found = blocks.extremes(within, length, count, quietest);
            let total = found.len();
            (found, total)
        }
        "sound" => (sounds.iter().take(count).copied().collect(), runs.len()),
        _ => {
            let shortest = (min_silence * f64::from(rate)).round() as u64;
            silences(&sounds, last, runs.len(), (from, to), shortest, count)
        }
    };
    let stretches = (stretches.iter())
        .map(|&(start, end)| {
            let (lufs, peak) = blocks.measure((start, end), &mut source)?;
            Ok(json!({
                "from": position(start, rate),
                "to": position(end, rate),
                "seconds": seconds(end - start, rate),
                "loudness_lufs": decibels(lufs),
                "peak_dbfs": level(peak),
            }))
        })
        .collect::<Result<Vec<Json>, String>>()?;
    Ok(json!({
        "what": what,
        "stretches": stretches,
        "found": found,
        "sound": sound,
    }))
}

/// The silences in `stretch`, the first `count` of them, and how many
/// there are: between the `runs` sounds, the first of which are `sounds`
/// and the last `last`, and before and after them if they're at least
/// `shortest` frames long. Without sounds, it's all silence.
fn silences(
    sounds: &[(u64, u64)],
    last: Option<(u64, u64)>,
    runs: usize,
    (from, to): (u64, u64),
    shortest: u64,
    count: usize,
) -> (Vec<(u64, u64)>, usize) {
    let (Some(first), Some(last)) = (sounds.first(), last) else {
        let all = (to > from).then_some((from, to));
        return (all.into_iter().collect(), usize::from(to > from));
    };
    let before = first.0 - from >= shortest && first.0 > from;
    let after = to - last.1 >= shortest && to > last.1;
    let mut silences: Vec<(u64, u64)> = Vec::new();
    if before {
        silences.push((from, first.0));
    }
    silences.extend(sounds.windows(2).map(|pair| (pair[0].1, pair[1].0)));
    // Those after the sounds listed aren't needed.
    if after && sounds.len() == runs {
        silences.push((last.1, to));
    }
    silences.truncate(count);
    let found = runs - 1 + usize::from(before) + usize::from(after);
    (silences, found)
}

/// A stretch's 10 ms blocks, as a meter keeping them measured them.
struct Blocks<'a> {
    list: &'a [Block],
    /// The frames they measured.
    from: u64,
    to: u64,
    rate: u32,
}

impl Blocks<'_> {
    /// The frames of block `k`.
    fn frames(&self, k: usize) -> (u64, u64) {
        let start = self.from + block_start(self.rate, k as u64);
        let end = self.from + block_start(self.rate, k as u64 + 1);
        (start, end.min(self.to))
    }

    /// The block holding frame `frame`.
    fn at(&self, frame: u64) -> usize {
        let mut k = ((frame - self.from) * 100 / u64::from(self.rate)) as usize;
        k = k.min(self.list.len().saturating_sub(1));
        while k > 0 && self.frames(k).0 > frame {
            k -= 1;
        }
        while k + 1 < self.list.len() && self.frames(k + 1).0 <= frame {
            k += 1;
        }
        k
    }

    /// The runs of blocks with a sample louder than `threshold`, from the
    /// first such block to the last, joined across silences shorter than
    /// `shortest` frames.
    fn sounds(&self, threshold: f64, shortest: u64) -> Vec<(usize, usize)> {
        let loud = |k: usize| f64::from(self.list[k].peak) > threshold;
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut k = 0;
        while k < self.list.len() {
            if !loud(k) {
                k += 1;
                continue;
            }
            let start = k;
            while k < self.list.len() && loud(k) {
                k += 1;
            }
            match runs.last_mut() {
                Some(run) if self.frames(start).0 - self.frames(run.1 - 1).1 < shortest => {
                    run.1 = k;
                }
                _ => runs.push((start, k)),
            }
        }
        runs
    }

    /// The frames of the run of blocks `run` from its first sample louder
    /// than `threshold` to the frame after its last, read from `source`.
    fn refine(
        &self,
        (first, last): (usize, usize),
        threshold: f64,
        source: &mut Source,
    ) -> Result<(u64, u64), String> {
        let channels = source.channels();
        let loud = |frame: &[f64]| frame.iter().any(|sample| sample.abs() > threshold);
        let (start, end) = self.frames(first);
        let samples = source.read_frames(start, end)?;
        let from = (samples.chunks_exact(channels)).position(loud).unwrap_or(0);
        let (start_of_last, end) = self.frames(last - 1);
        let samples = source.read_frames(start_of_last, end)?;
        let to = (samples.chunks_exact(channels))
            .rposition(loud)
            .map_or(end - start_of_last, |frame| frame as u64 + 1);
        Ok((start + from as u64, start_of_last + to))
    }

    /// The loudest or quietest windows of `length` seconds within blocks
    /// `within`, `count` of them at most, that don't overlap, as frames:
    /// loudest or quietest first, the earlier first of two as loud.
    fn extremes(
        &self,
        (first, last): (usize, usize),
        length: f64,
        count: usize,
        quietest: bool,
    ) -> Vec<(u64, u64)> {
        let span = last - first;
        if span == 0 {
            return Vec::new();
        }
        let width = ((length * 100.0).round() as usize).clamp(1, span);
        // Running sums of the blocks' energy, by their frames, and frames.
        let mut energy = vec![0.0; span + 1];
        let mut frames = vec![0u64; span + 1];
        for i in 0..span {
            let (start, end) = self.frames(first + i);
            energy[i + 1] = energy[i] + self.list[first + i].energy * (end - start) as f64;
            frames[i + 1] = frames[i] + (end - start);
        }
        let mean =
            |i: usize| (energy[i + width] - energy[i]) / (frames[i + width] - frames[i]) as f64;
        let mut order: Vec<usize> = (0..=span - width).collect();
        order.sort_by(|&a, &b| {
            let louder = mean(b).total_cmp(&mean(a));
            let louder = if quietest { louder.reverse() } else { louder };
            louder.then(a.cmp(&b))
        });
        let mut chosen: Vec<usize> = Vec::new();
        for i in order {
            if chosen.len() == count {
                break;
            }
            if chosen.iter().all(|&c| i + width <= c || c + width <= i) {
                chosen.push(i);
            }
        }
        (chosen.into_iter())
            .map(|i| {
                (
                    self.frames(first + i).0,
                    self.frames(first + i + width - 1).1,
                )
            })
            .collect()
    }

    /// The loudness of frames `start` to `end`, from the energy of the
    /// blocks wholly in them, or without any, of the blocks they're in by
    /// how much of each they hold; and their loudest sample, read from
    /// `source` where they hold part of a block.
    fn measure(
        &self,
        (start, end): (u64, u64),
        source: &mut Source,
    ) -> Result<(Option<f64>, f64), String> {
        // The energy and frames of whole blocks, and of parts of blocks.
        let (mut whole, mut parts) = ((0.0, 0), (0.0, 0));
        let mut peak = 0.0f64;
        for k in self.at(start)..=self.at(end - 1) {
            let block = self.frames(k);
            let (from, to) = (block.0.max(start), block.1.min(end));
            let energy = self.list[k].energy * (to - from) as f64;
            if (from, to) == block {
                whole = (whole.0 + energy, whole.1 + (to - from));
                peak = peak.max(f64::from(self.list[k].peak));
            } else {
                parts = (parts.0 + energy, parts.1 + (to - from));
                let samples = source.read_frames(from, to)?;
                peak = samples
                    .iter()
                    .fold(peak, |peak, sample| peak.max(sample.abs()));
            }
        }
        let (energy, frames) = if whole.1 > 0 { whole } else { parts };
        // Silence has no loudness, though the filters ring on into it.
        let loudness = match peak > 0.0 {
            true => energy_to_lufs(energy / frames as f64),
            false => None,
        };
        Ok((loudness, peak))
    }
}

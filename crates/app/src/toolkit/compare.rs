//! audio.compare, which says how two files differ: lines B up with A, finds
//! the gain that matches its level to A's, and measures what's left when A
//! is taken from it, their loudness and bands, how their onsets' timing
//! differs, and the clicks one has and the other doesn't.

use std::sync::Arc;

use musicable_analysis::{Samples, loudness::Meter};
use musicable_commands::{Args, Param, Type};
use realfft::num_complex::Complex64;
use realfft::{RealFftPlanner, RealToComplex};
use serde_json::{Value as Json, json};

use super::spectra::{self, Welch, power_db};
use super::{
    AudioCommand, CHUNK, Job, MOST, Share, Source, decibels, level, opt, position, round, schema,
};

/// The most samples a file compared can have, all its channels': it's held
/// in memory. About 6 minutes of stereo at 48 kHz.
const MOST_SAMPLES: usize = 1 << 25;
/// How far apart B may be from A, without max_offset_ms, and at most.
const MAX_OFFSET_MS: f64 = 1000.0;
const MOST_OFFSET_MS: f64 = 10_000.0;
/// How long the stretches where what's left is loudest are.
const WINDOW: f64 = 0.1;
/// How far apart onsets may be and still be the same.
const ONSET_WINDOW: f64 = 0.050;
/// How much a click must rise above the high frequencies around it, in
/// decibels, without click_db; and how loud it must be at least, in dBFS.
const CLICK_DB: f64 = 20.0;
const CLICK_FLOOR_DBFS: f64 = -80.0;
/// A click is found in blocks of this long, against the median of the
/// blocks this far either side, and clicks this close are one.
const CLICK_BLOCK: f64 = 0.001;
const CLICK_AROUND: f64 = 0.050;
const CLICK_MERGE: f64 = 0.005;
/// A click's spread is the share of its energy within this much of its
/// peak, of that within the wider span.
const SPREAD_NEAR: f64 = 0.001;
const SPREAD_SPAN: f64 = 0.005;
/// How far either side of a position given in `at` it's measured.
const AT_SPAN: f64 = 0.002;
/// The quietest level a median of the high frequencies is taken as, so a
/// click in digital silence still rises by a number of decibels.
const QUIETEST: f64 = 1e-6;
/// The bands: below 200 Hz, 200 Hz to 4 kHz, and above, as onsets' bands
/// are split.
const BANDS: [(&str, f64, f64); 3] = [
    ("low", 0.0, 200.0),
    ("mid", 200.0, 4_000.0),
    ("high", 4_000.0, f64::INFINITY),
];
pub(super) fn commands() -> Vec<AudioCommand> {
    vec![compare()]
}

fn compare() -> AudioCommand {
    AudioCommand {
        id: "audio.compare",
        title: "Compare Two Files",
        about: "Says how B differs from A, which it's measured against: a render against a \
                reference, or a recording against an export. It lines B up with A, matches B's \
                level to A's, and takes A from it, then gives how far apart they were, the gain, \
                how loud what's left is and where it's loudest, their loudness and their levels \
                in three bands and 31 third octaves, how early or late B's onsets come against \
                A's, and the clicks one has and the other doesn't. Their sample rates and \
                channels must be the same, and each can have about 6 minutes of stereo at \
                48 kHz.",
        params: vec![
            Param::required(
                "a",
                Type::Text,
                "The file to measure against, by its path: a WAV, AIFF or FLAC file.",
            ),
            Param::required(
                "b",
                Type::Text,
                "The file to measure, by its path: a WAV, AIFF or FLAC file.",
            ),
            Param::optional(
                "align",
                Type::Bool,
                "Lines B up with A first, where their samples correlate best. Without it, true: \
                 --no-align compares them frame for frame, from their starts.",
            ),
            Param::optional(
                "max_offset_ms",
                Type::Number,
                "How far B may be from A, either way, in milliseconds, up to 10000. Without \
                 it, 1000.",
            ),
            Param::optional(
                "match_level",
                Type::Bool,
                "Matches B's level to A's before taking A from it, with the gain that leaves \
                 the least. Without it, true: --no-match-level takes A from B as it is, as a \
                 recording that should play the export exactly needs.",
            ),
            Param::optional(
                "count",
                Type::Int,
                "How many of the stretches where what's left is loudest to give, from 1 to \
                 200. Without it, 5.",
            ),
            Param::optional(
                "click_db",
                Type::Number,
                "How far a click's high frequencies must rise above those around it, in \
                 decibels. Without it, 20.",
            ),
            Param::optional(
                "at",
                Type::List(&Type::Number),
                "A position in A, in seconds, to measure the high frequencies at, such as a \
                 slice's loop turn, where a click would be: on the command line, --at once for each.",
            ),
        ],
        positional: &["a", "b"],
        read_only: true,
        example: &["tone.wav", "take.wav"],
        run: run_compare,
        output,
    }
}

fn output() -> Json {
    let stretch = schema::object(
        "A stretch of A where what's left is loud.",
        &[
            ("from", schema::position("Where it starts, in A.")),
            ("to", schema::position("Where it ends, in A.")),
            (
                "residual_dbfs",
                schema::level("The RMS level of what's left there, in dBFS."),
            ),
            ("a_dbfs", schema::level("A's RMS level there, in dBFS.")),
        ],
    );
    let band = |about: &str, name: (&str, Json)| {
        schema::object(
            about,
            &[
                name,
                (
                    "a_dbfs",
                    schema::level("A's level in the band, as an RMS level, in dBFS."),
                ),
                ("b_dbfs", schema::level("B's, once its level is matched.")),
                (
                    "difference_db",
                    schema::level("How much louder B is in the band than A."),
                ),
            ],
        )
    };
    let bands = band(
        "A band: below 200 Hz, from 200 Hz to 4 kHz, or above.",
        (
            "band",
            schema::choice(&["low", "mid", "high"], "Which band it is."),
        ),
    );
    let third = band(
        "A third octave.",
        (
            "centre_hz",
            schema::number("Its centre, in hertz, as IEC 61260 names it."),
        ),
    );
    let lag = schema::object(
        "An onset of A's and B's that match.",
        &[
            ("at", schema::position("A's onset.")),
            (
                "lag_ms",
                schema::number("How much later B's comes, in milliseconds: early if negative."),
            ),
        ],
    );
    let lag_stat = |about: &str| schema::level(about);
    let onsets = schema::object(
        "How the timing of their onsets differs, B's lined up with A's.",
        &[
            ("a_count", schema::count("A's onsets.")),
            ("b_count", schema::count("B's onsets.")),
            (
                "matched",
                schema::count("Onsets of A's with one of B's within 50 ms."),
            ),
            (
                "median_lag_ms",
                lag_stat(
                    "The median of how much later B's matched onsets come, in milliseconds: \
                     null if none matched.",
                ),
            ),
            (
                "mean_lag_ms",
                lag_stat("The mean, in milliseconds: null if none matched."),
            ),
            (
                "earliest_lag_ms",
                lag_stat("The earliest, in milliseconds: null if none matched."),
            ),
            (
                "latest_lag_ms",
                lag_stat("The latest, in milliseconds: null if none matched."),
            ),
            (
                "lags",
                schema::list(lag, "Each matched pair, in order, the first 200."),
            ),
        ],
    );
    let click = schema::object(
        "A click.",
        &[
            ("at", schema::position("Its peak, in A.")),
            (
                "in",
                schema::choice(&["a", "b"], "Which file has it: the louder there."),
            ),
            (
                "level_dbfs",
                schema::level("How loud its high frequencies peak, in dBFS."),
            ),
            (
                "above_db",
                schema::number("How far they rise above the high frequencies around it."),
            ),
            (
                "spread",
                schema::number(
                    "The share of its energy within 1 ms of its peak, of that within 5 ms, \
                     from 0 to 1: the nearer 1, the sharper it is.",
                ),
            ),
        ],
    );
    let clicks = schema::object(
        "The clicks one file has and the other doesn't: high frequencies in what's left \
         that rise click_db above those around them, peaking at -80 dBFS or louder.",
        &[
            ("count", schema::count("How many there are.")),
            (
                "found",
                schema::list(click, "Each of them, in order, the first 200."),
            ),
        ],
    );
    let at = schema::object(
        "A position at was given.",
        &[
            ("at", schema::position("The position, in A.")),
            (
                "residual_peak_dbfs",
                schema::level(
                    "How loud the high frequencies of what's left peak within 2 ms of it, in \
                     dBFS.",
                ),
            ),
            (
                "high_difference_db",
                schema::level(
                    "How much more energy B's high frequencies have than A's within 2 ms of \
                     it, in decibels: null if either has none.",
                ),
            ),
        ],
    );
    schema::object(
        "How B differs from A.",
        &[
            ("a", schema::text("A's whole path.")),
            ("b", schema::text("B's whole path.")),
            ("sample_rate", schema::count("Their sample rate, in hertz.")),
            ("channels", schema::count("Their channels.")),
            (
                "identical",
                schema::boolean(
                    "Whether they have the same samples, every one, as they are: without \
                     being lined up or their levels matched.",
                ),
            ),
            ("a_frames", schema::count("A's length, in frames.")),
            ("b_frames", schema::count("B's length, in frames.")),
            (
                "length_difference_ms",
                schema::number("How much longer B is than A, in milliseconds."),
            ),
            (
                "offset_frames",
                schema::number(
                    "How much later B's sound comes than A's, in frames: early if negative. \
                     0 with --no-align.",
                ),
            ),
            (
                "offset_ms",
                schema::number("The same, in milliseconds, to 3 places."),
            ),
            (
                "correlation",
                schema::level(
                    "How alike their samples are, lined up, from -1 to 1: 1 if B is A at \
                     another level. Null if either is silent where they overlap.",
                ),
            ),
            (
                "compared",
                schema::object(
                    "The stretch of A that B overlaps, lined up, which the rest measures.",
                    &[
                        ("from", schema::position("Where it starts, in A.")),
                        ("to", schema::position("Where it ends, in A.")),
                        ("seconds", schema::number("How long it is.")),
                    ],
                ),
            ),
            (
                "a_lufs",
                schema::level("A's integrated loudness, in LUFS, all of it."),
            ),
            (
                "b_lufs",
                schema::level("B's, in LUFS, all of it, as it is."),
            ),
            (
                "loudness_difference_lu",
                schema::level("How much louder B is than A, in LU."),
            ),
            (
                "gain_db",
                schema::level(
                    "The gain that matched B's level to A's, in decibels: 0 with \
                     --no-match-level, and null if it had to turn B upside down.",
                ),
            ),
            (
                "residual_dbfs",
                schema::level(
                    "The RMS level of what's left, B lined up and matched, less A, in dBFS: \
                     null if nothing is.",
                ),
            ),
            (
                "residual_db",
                schema::level(
                    "The same, against A's RMS level over the stretch compared, in decibels: \
                     null if nothing is left, or A is silent.",
                ),
            ),
            (
                "loudest_residual",
                schema::list(
                    stretch,
                    "The 100 ms stretches where what's left is loudest, loudest first: count \
                     of them.",
                ),
            ),
            ("bands", schema::list(bands, "Their levels in three bands.")),
            (
                "third_octaves",
                schema::list(
                    third,
                    "Their levels in each third octave below half the sample rate. Below \
                     about 60 Hz, each takes a few of the spectrum's bins.",
                ),
            ),
            ("onsets", onsets),
            ("clicks", clicks),
            (
                "at",
                schema::list(at, "Each position at gave, in the order given."),
            ),
        ],
    )
}

/// A file in memory, interleaved.
pub(super) struct Audio {
    pub(super) samples: Vec<f32>,
    pub(super) channels: usize,
}

impl Audio {
    pub(super) fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Its channels mixed to one.
    pub(super) fn mono(&self) -> Vec<f32> {
        let scale = 1.0 / self.channels as f32;
        (self.samples.chunks_exact(self.channels))
            .map(|frame| frame.iter().sum::<f32>() * scale)
            .collect()
    }
}

/// How B lines up with A: B's frame `n + offset` is A's frame `n`, for A's
/// frames from `from` to `to`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Lineup {
    offset: i64,
    from: usize,
    to: usize,
}

impl Lineup {
    pub(super) fn new(offset: i64, a: usize, b: usize) -> Option<Self> {
        let from = (-offset).max(0) as usize;
        let to = (a as i64).min(b as i64 - offset).max(0) as usize;
        (from < to).then_some(Self { offset, from, to })
    }

    fn frames(&self) -> usize {
        self.to - self.from
    }

    /// B's frame for A's frame `n`.
    fn b(&self, n: usize) -> usize {
        (n as i64 + self.offset) as usize
    }
}

fn run_compare(args: &Args, job: &Job) -> Result<Json, String> {
    let (a_path, b_path) = (job.file(args, "a")?, job.file(args, "b")?);
    let align = opt::<bool>(args, "align")?.unwrap_or(true);
    let matched = opt::<bool>(args, "match_level")?.unwrap_or(true);
    let max_offset_ms = opt::<f64>(args, "max_offset_ms")?.unwrap_or(MAX_OFFSET_MS);
    if !(0.0..=MOST_OFFSET_MS).contains(&max_offset_ms) {
        return Err(format!(
            "max_offset_ms is from 0 to {MOST_OFFSET_MS}, not {max_offset_ms}."
        ));
    }
    let count = opt::<i64>(args, "count")?.unwrap_or(5);
    if !(1..=MOST as i64).contains(&count) {
        return Err(format!("count is from 1 to {MOST}, not {count}."));
    }
    let click_db = opt::<f64>(args, "click_db")?.unwrap_or(CLICK_DB);
    if click_db <= 0.0 {
        return Err(format!("click_db is more than 0, not {click_db}."));
    }
    let at: Vec<f64> = opt(args, "at")?.unwrap_or_default();
    if at.len() > MOST {
        return Err(format!(
            "at lists {} positions, and compare takes {MOST} at most.",
            at.len()
        ));
    }

    let mut a_source = Source::open(&a_path)?;
    let mut b_source = Source::open(&b_path)?;
    let rate = a_source.rate();
    if b_source.rate() != rate {
        return Err(format!(
            "A is at {rate} Hz and B at {} Hz: convert one to the other's rate with \
             audio.convert first.",
            b_source.rate()
        ));
    }
    let channels = a_source.channels();
    if b_source.channels() != channels {
        return Err(format!(
            "A has {channels} channels and B {}: convert one with audio.convert first.",
            b_source.channels()
        ));
    }
    for source in [&a_source, &b_source] {
        fits(source, "compare")?;
    }
    let share = |from, to| Share::ALL.between(from, to);
    let identical = identical(&mut a_source, &mut b_source, job, share(0.0, 0.1))?;
    let a = load(&mut a_source, job, share(0.1, 0.2))?;
    let b = load(&mut b_source, job, share(0.2, 0.3))?;
    let (a_mono, b_mono) = (a.mono(), b.mono());
    let rate_f = f64::from(rate);

    let (offset, _) = match align {
        true => {
            let most = (max_offset_ms / 1000.0 * rate_f).round() as usize;
            best_offset(&a_mono, &b_mono, most, job, share(0.3, 0.6))?
        }
        false => (0, None),
    };
    let lineup = Lineup::new(offset, a.frames(), b.frames()).ok_or_else(|| {
        "Lined up, A and B don't overlap: give a larger max_offset_ms, or --no-align.".to_owned()
    })?;
    let correlation = correlation_at(&a_mono, &b_mono, lineup);

    // The gain that leaves the least: B's samples times it, less A's.
    let (mut ab, mut bb, mut aa) = (0.0f64, 0.0f64, 0.0f64);
    for n in lineup.from..lineup.to {
        let (fa, fb) = (n * channels, lineup.b(n) * channels);
        for c in 0..channels {
            let (x, y) = (f64::from(a.samples[fa + c]), f64::from(b.samples[fb + c]));
            ab += x * y;
            bb += y * y;
            aa += x * x;
        }
    }
    let gain = match matched && bb > 0.0 {
        true => ab / bb,
        false => 1.0,
    };
    job.check()?;

    // What's left, over the stretch compared, and its level in windows.
    let window = ((WINDOW * rate_f).round() as usize).max(1);
    let mut residual_energy = 0.0f64;
    let mut residual_mono = Vec::with_capacity(lineup.frames());
    let mut windows: Vec<(usize, f64, f64)> = Vec::new();
    let mut a_mono_compared = Vec::with_capacity(lineup.frames());
    let mut b_mono_compared = Vec::with_capacity(lineup.frames());
    let scale = 1.0 / channels as f64;
    for start in (lineup.from..lineup.to).step_by(window) {
        let end = (start + window).min(lineup.to);
        let (mut left, mut there) = (0.0f64, 0.0f64);
        for n in start..end {
            let (fa, fb) = (n * channels, lineup.b(n) * channels);
            let mut mono = 0.0;
            for c in 0..channels {
                let x = f64::from(a.samples[fa + c]);
                let difference = gain * f64::from(b.samples[fb + c]) - x;
                left += difference * difference;
                there += x * x;
                mono += difference;
            }
            residual_mono.push(mono * scale);
            a_mono_compared.push(f64::from(a_mono[n]));
            b_mono_compared.push(gain * f64::from(b_mono[lineup.b(n)]));
        }
        residual_energy += left;
        let samples = ((end - start) * channels) as f64;
        windows.push((start, left / samples, there / samples));
    }
    job.report(share(0.6, 0.7), 1.0);
    let samples = (lineup.frames() * channels) as f64;
    let residual_dbfs = power_db(residual_energy / samples);
    let residual_db = match aa > 0.0 && residual_energy > 0.0 {
        true => Some(10.0 * (residual_energy / aa).log10()),
        false => None,
    };
    windows.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    let loudest_residual: Vec<Json> = (windows.iter())
        .filter(|(_, left, _)| *left > 0.0)
        .take(count as usize)
        .map(|&(start, left, there)| {
            let end = (start + window).min(lineup.to);
            json!({
                "from": position(start as u64, rate),
                "to": position(end as u64, rate),
                "residual_dbfs": decibels(power_db(left)),
                "a_dbfs": decibels(power_db(there)),
            })
        })
        .collect();

    // Loudness, of each file all through, as it is.
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
    let (a_lufs, b_lufs) = (loudness(&a), loudness(&b));
    job.check()?;

    // Their spectra, over the stretch compared.
    let spectrum = |mono: &[f64]| {
        let mut welch = Welch::new(rate);
        for chunk in mono.chunks(CHUNK) {
            welch.push(chunk);
        }
        welch.finish()
    };
    let a_spectrum = spectrum(&a_mono_compared);
    job.check()?;
    let b_spectrum = spectrum(&b_mono_compared);
    job.report(share(0.7, 0.8), 1.0);
    let band_levels = |from: f64, to: f64| {
        let a_db = a_spectrum.as_ref().and_then(|s| s.level(from, to));
        let b_db = b_spectrum.as_ref().and_then(|s| s.level(from, to));
        json!({
            "a_dbfs": decibels(a_db),
            "b_dbfs": decibels(b_db),
            "difference_db": decibels(a_db.zip(b_db).map(|(a, b)| b - a)),
        })
    };
    let bands: Vec<Json> = (BANDS.iter())
        .map(|&(name, from, to)| {
            let mut levels = band_levels(from, to.min(rate_f / 2.0));
            levels["band"] = json!(name);
            levels
        })
        .collect();
    let third_octaves: Vec<Json> = (spectra::bands(false, rate_f / 2.0).into_iter())
        .map(|band| {
            let mut levels = band_levels(band.from, band.to);
            levels["centre_hz"] = json!(band.centre);
            levels
        })
        .collect();

    // Their onsets: B's, lined up, matched with A's.
    let onsets_of = |mono: &[f32]| {
        let mut audio = Samples {
            samples: mono,
            sample_rate: rate,
        };
        musicable_analysis::onsets(&mut audio)
            .map_err(|error| format!("Couldn't find the onsets: {error}."))
    };
    let a_onsets = onsets_of(&a_mono)?;
    job.check()?;
    let b_onsets = onsets_of(&b_mono)?;
    job.report(share(0.8, 0.9), 1.0);
    let a_frames: Vec<i64> = a_onsets.iter().map(|onset| onset.frame as i64).collect();
    let b_frames: Vec<i64> = (b_onsets.iter())
        .map(|onset| onset.frame as i64 - offset)
        .collect();
    let pairs = match_onsets(&a_frames, &b_frames, (ONSET_WINDOW * rate_f).round() as i64);
    let ms = |frames: f64| frames / rate_f * 1000.0;
    let mut lags: Vec<f64> = pairs.iter().map(|&(a, b)| ms((b - a) as f64)).collect();
    let lag_list: Vec<Json> = (pairs.iter().zip(&lags))
        .take(MOST)
        .map(|(&(a, _), &lag)| json!({"at": position(a as u64, rate), "lag_ms": round(lag, 3)}))
        .collect();
    let mean = (!lags.is_empty()).then(|| lags.iter().sum::<f64>() / lags.len() as f64);
    lags.sort_by(f64::total_cmp);
    let median = match lags.len() {
        0 => None,
        n if n % 2 == 1 => Some(lags[n / 2]),
        n => Some((lags[n / 2 - 1] + lags[n / 2]) / 2.0),
    };
    let lag = |value: Option<f64>| value.map_or(Json::Null, |value| json!(round(value, 3)));
    let onsets = json!({
        "a_count": a_onsets.len(),
        "b_count": b_onsets.len(),
        "matched": pairs.len(),
        "median_lag_ms": lag(median),
        "mean_lag_ms": lag(mean),
        "earliest_lag_ms": lag(lags.first().copied()),
        "latest_lag_ms": lag(lags.last().copied()),
        "lags": lag_list,
    });

    // The clicks: the high frequencies of what's left, where they rise
    // above those around them.
    // The second difference, centred, so a click peaks on its own sample.
    let high = |x: &[f64]| -> Vec<f64> {
        (0..x.len())
            .map(|n| match n {
                n if n == 0 || n + 1 >= x.len() => 0.0,
                n => (x[n + 1] - 2.0 * x[n] + x[n - 1]) / 4.0,
            })
            .collect()
    };
    let (residual_high, a_high, b_high) = (
        high(&residual_mono),
        high(&a_mono_compared),
        high(&b_mono_compared),
    );
    let span = |seconds: f64| ((seconds * rate_f).round() as usize).max(1);
    let energy = |x: &[f64], at: usize, half: usize| -> f64 {
        let (from, to) = (at.saturating_sub(half), (at + half + 1).min(x.len()));
        x[from..to].iter().map(|v| v * v).sum()
    };
    let clicks = find_clicks(
        &residual_high,
        span(CLICK_BLOCK),
        span(CLICK_AROUND),
        click_db,
    );
    let clicks = merge_clicks(clicks, span(CLICK_MERGE));
    let found: Vec<Json> = (clicks.iter())
        .take(MOST)
        .map(|click| {
            let near = energy(&residual_high, click.at, span(SPREAD_NEAR));
            let wide = energy(&residual_high, click.at, span(SPREAD_SPAN));
            let near_b = energy(&b_high, click.at, span(SPREAD_NEAR));
            let near_a = energy(&a_high, click.at, span(SPREAD_NEAR));
            json!({
                "at": position((lineup.from + click.at) as u64, rate),
                "in": if near_b >= near_a { "b" } else { "a" },
                "level_dbfs": level(click.peak),
                "above_db": round(20.0 * (click.peak / click.around).log10(), 2),
                "spread": round(if wide > 0.0 { near / wide } else { 0.0 }, 3),
            })
        })
        .collect();
    let clicks = json!({"count": clicks.len(), "found": found});

    // The positions asked about.
    let mut measured = Vec::with_capacity(at.len());
    for seconds in at {
        let frame = (seconds * rate_f).round();
        if !(seconds >= 0.0 && frame >= lineup.from as f64 && frame < lineup.to as f64) {
            return Err(format!(
                "at {seconds} s isn't in the stretch of A compared, from {} s to {} s.",
                round(lineup.from as f64 / rate_f, 6),
                round(lineup.to as f64 / rate_f, 6),
            ));
        }
        let i = frame as usize - lineup.from;
        let half = span(AT_SPAN);
        let (from, to) = (
            i.saturating_sub(half),
            (i + half + 1).min(residual_high.len()),
        );
        let peak = (residual_high[from..to].iter()).fold(0.0f64, |peak, v| peak.max(v.abs()));
        let (a_energy, b_energy) = (energy(&a_high, i, half), energy(&b_high, i, half));
        let difference =
            (a_energy > 0.0 && b_energy > 0.0).then(|| 10.0 * (b_energy / a_energy).log10());
        measured.push(json!({
            "at": position(frame as u64, rate),
            "residual_peak_dbfs": level(peak),
            "high_difference_db": decibels(difference),
        }));
    }
    job.report(Share::ALL, 1.0);

    Ok(json!({
        "a": a_path.display().to_string(),
        "b": b_path.display().to_string(),
        "sample_rate": rate,
        "channels": channels,
        "identical": identical,
        "a_frames": a.frames(),
        "b_frames": b.frames(),
        "length_difference_ms": round(ms(b.frames() as f64 - a.frames() as f64), 3),
        "offset_frames": offset,
        "offset_ms": round(ms(offset as f64), 3),
        "correlation": correlation.map_or(Json::Null, |value| json!(round(value, 6))),
        "compared": {
            "from": position(lineup.from as u64, rate),
            "to": position(lineup.to as u64, rate),
            "seconds": round(lineup.frames() as f64 / rate_f, 6),
        },
        "a_lufs": decibels(a_lufs),
        "b_lufs": decibels(b_lufs),
        "loudness_difference_lu": decibels(a_lufs.zip(b_lufs).map(|(a, b)| b - a)),
        "gain_db": level(gain),
        "residual_dbfs": decibels(residual_dbfs),
        "residual_db": decibels(residual_db),
        "loudest_residual": loudest_residual,
        "bands": bands,
        "third_octaves": third_octaves,
        "onsets": onsets,
        "clicks": clicks,
        "at": measured,
    }))
}

/// Whether `a` and `b` have the same samples, every one, read exactly.
fn identical(a: &mut Source, b: &mut Source, job: &Job, share: Share) -> Result<bool, String> {
    if a.frames() != b.frames() || a.channels() != b.channels() || a.rate() != b.rate() {
        return Ok(false);
    }
    let (frames, channels) = (a.frames(), a.channels());
    let mut x = vec![0.0; CHUNK * channels];
    let mut y = vec![0.0; CHUNK * channels];
    let mut at = 0;
    while at < frames {
        job.check()?;
        let want = ((frames - at) as usize).min(CHUNK) * channels;
        let read = a.read(at, &mut x[..want])?;
        let mut done = 0;
        while done < read {
            done += b.read(at + done as u64, &mut y[done * channels..read * channels])?;
        }
        if x[..read * channels] != y[..read * channels] {
            return Ok(false);
        }
        at += read as u64;
        job.report(share, at as f64 / frames as f64);
    }
    Ok(true)
}

/// Checks that `source` isn't too long to hold in memory, for `command`.
pub(super) fn fits(source: &Source, command: &str) -> Result<(), String> {
    match source.frames() as u128 * source.channels() as u128 > MOST_SAMPLES as u128 {
        true => Err(format!(
            "{} is too long to {command}: it takes about 6 minutes of stereo at 48 kHz. Cut \
             the stretch you need with audio.cut first.",
            source.path.display()
        )),
        false => Ok(()),
    }
}

/// All of `source`, in memory.
pub(super) fn load(source: &mut Source, job: &Job, share: Share) -> Result<Audio, String> {
    let channels = source.channels();
    let mut samples = Vec::with_capacity(source.frames() as usize * channels);
    source.each_chunk(0, source.frames(), job, share, |chunk| {
        samples.extend(chunk.iter().map(|&sample| sample as f32));
        Ok(())
    })?;
    Ok(Audio { samples, channels })
}

/// How well `a` and `b` correlate, lined up as `lineup` says: `None` if
/// either is silent there.
pub(super) fn correlation_at(a: &[f32], b: &[f32], lineup: Lineup) -> Option<f64> {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for n in lineup.from..lineup.to {
        let (x, y) = (f64::from(a[n]), f64::from(b[lineup.b(n)]));
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    (aa > 0.0 && bb > 0.0).then(|| ab / (aa * bb).sqrt())
}

/// The offset from `-most` to `most` at which `b` correlates best with
/// `a`, normalised over where they overlap, and that correlation: B's
/// frame `n + offset` is A's frame `n`. Offsets that leave less than half
/// of the shorter overlapping don't count. Without one, 0.
pub(super) fn best_offset(
    a: &[f32],
    b: &[f32],
    most: usize,
    job: &Job,
    share: Share,
) -> Result<(i64, Option<f64>), String> {
    let most = most.min(a.len().max(b.len()));
    let lags = 2 * most + 1;
    // r[j] is the sum of a[n] b[n + j - most], found a block of A at a
    // time, each against the stretch of B it can line up with.
    let block = (2 * most).next_power_of_two().max(4096);
    let size = (block + 2 * most).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward: Arc<dyn RealToComplex<f64>> = planner.plan_fft_forward(size);
    let inverse = planner.plan_fft_inverse(size);
    let mut r = vec![0.0f64; lags];
    let mut x = forward.make_input_vec();
    let mut y = forward.make_input_vec();
    let mut xs = forward.make_output_vec();
    let mut ys = forward.make_output_vec();
    let mut scratch = forward.make_scratch_vec();
    let mut out = inverse.make_output_vec();
    let mut inverse_scratch = inverse.make_scratch_vec();
    let blocks = a.len().div_ceil(block);
    for (index, start) in (0..a.len()).step_by(block).enumerate() {
        job.check()?;
        let end = (start + block).min(a.len());
        x.fill(0.0);
        for (i, sample) in a[start..end].iter().enumerate() {
            x[i] = f64::from(*sample);
        }
        y.fill(0.0);
        for (i, slot) in y.iter_mut().enumerate().take(end - start + 2 * most) {
            let n = start as i64 + i as i64 - most as i64;
            if n >= 0 && (n as usize) < b.len() {
                *slot = f64::from(b[n as usize]);
            }
        }
        forward
            .process_with_scratch(&mut x, &mut xs, &mut scratch)
            .map_err(|error| format!("Couldn't correlate them: {error}."))?;
        forward
            .process_with_scratch(&mut y, &mut ys, &mut scratch)
            .map_err(|error| format!("Couldn't correlate them: {error}."))?;
        for (x, y) in xs.iter_mut().zip(&ys) {
            *x = x.conj() * y;
        }
        // The inverse wants the first and last bins real, as they are.
        xs[0] = Complex64::new(xs[0].re, 0.0);
        let last = xs.len() - 1;
        xs[last] = Complex64::new(xs[last].re, 0.0);
        inverse
            .process_with_scratch(&mut xs, &mut out, &mut inverse_scratch)
            .map_err(|error| format!("Couldn't correlate them: {error}."))?;
        for (j, value) in r.iter_mut().enumerate() {
            *value += out[j] / size as f64;
        }
        job.report(share, (index + 1) as f64 / blocks as f64);
    }
    // Each offset's correlation, over where they overlap there.
    let prefix = |x: &[f32]| {
        let mut sums = Vec::with_capacity(x.len() + 1);
        let mut sum = 0.0f64;
        sums.push(0.0);
        for sample in x {
            sum += f64::from(*sample) * f64::from(*sample);
            sums.push(sum);
        }
        sums
    };
    let (a_sums, b_sums) = (prefix(a), prefix(b));
    let shortest = a.len().min(b.len()).div_ceil(2).max(1);
    let mut best: Option<(i64, f64)> = None;
    for (j, &value) in r.iter().enumerate() {
        let offset = j as i64 - most as i64;
        let Some(lineup) = Lineup::new(offset, a.len(), b.len()) else {
            continue;
        };
        if lineup.frames() < shortest {
            continue;
        }
        let aa = a_sums[lineup.to] - a_sums[lineup.from];
        let bb = b_sums[lineup.b(lineup.to)] - b_sums[lineup.b(lineup.from)];
        if aa <= 0.0 || bb <= 0.0 {
            continue;
        }
        let correlation = value / (aa * bb).sqrt();
        let better = match best {
            None => true,
            Some((at, most)) => {
                correlation > most + 1e-12
                    || (correlation > most - 1e-12 && offset.abs() < at.abs())
            }
        };
        if better {
            best = Some((offset, correlation));
        }
    }
    Ok(match best {
        Some((offset, correlation)) => (offset, Some(correlation)),
        None => (0, None),
    })
}

/// A's and B's onsets that match, each the nearest of the other's within
/// `window` frames that's not matched already, as pairs of their frames,
/// in order.
fn match_onsets(a: &[i64], b: &[i64], window: i64) -> Vec<(i64, i64)> {
    let mut taken = vec![false; b.len()];
    let mut pairs = Vec::new();
    for &onset in a {
        let at = b.partition_point(|&frame| frame < onset);
        let nearest = [at.checked_sub(1), Some(at), Some(at + 1)]
            .into_iter()
            .flatten()
            .filter(|&i| i < b.len() && !taken[i] && (b[i] - onset).abs() <= window)
            .min_by_key(|&i| (b[i] - onset).abs());
        if let Some(i) = nearest {
            taken[i] = true;
            pairs.push((onset, b[i]));
        }
    }
    pairs
}

/// A click in the high frequencies of what's left: its peak's sample, how
/// loud that is, and the median peak of the blocks around it.
#[derive(Debug, Clone, Copy)]
struct Click {
    at: usize,
    peak: f64,
    around: f64,
}

/// The blocks of `block` samples of `high` whose peak rises `click_db`
/// above the median peak of the blocks within `around` samples, and peaks
/// at -80 dBFS or more.
fn find_clicks(high: &[f64], block: usize, around: usize, click_db: f64) -> Vec<Click> {
    let peaks: Vec<(usize, f64)> = (high.chunks(block).enumerate())
        .map(|(i, samples)| {
            let (at, peak) =
                (samples.iter().enumerate()).fold((0, 0.0f64), |(at, peak), (j, v)| {
                    match v.abs() > peak {
                        true => (j, v.abs()),
                        false => (at, peak),
                    }
                });
            (i * block + at, peak)
        })
        .collect();
    let reach = around.div_ceil(block);
    let rise = 10f64.powf(click_db / 20.0);
    let floor = 10f64.powf(CLICK_FLOOR_DBFS / 20.0);
    let mut nearby = Vec::with_capacity(2 * reach + 1);
    let mut clicks = Vec::new();
    for (i, &(at, peak)) in peaks.iter().enumerate() {
        if peak < floor {
            continue;
        }
        nearby.clear();
        let (from, to) = (i.saturating_sub(reach), (i + reach + 1).min(peaks.len()));
        nearby.extend(peaks[from..to].iter().map(|&(_, peak)| peak));
        let middle = nearby.len() / 2;
        let (_, median, _) = nearby.select_nth_unstable_by(middle, f64::total_cmp);
        let around = median.max(QUIETEST);
        if peak >= around * rise {
            clicks.push(Click { at, peak, around });
        }
    }
    clicks
}

/// `clicks`, those within `merge` samples of the one before taken as one,
/// at the loudest of them.
fn merge_clicks(clicks: Vec<Click>, merge: usize) -> Vec<Click> {
    let mut merged: Vec<Click> = Vec::new();
    let mut last_at = None;
    for click in clicks {
        match (merged.last_mut(), last_at) {
            (Some(previous), Some(last)) if click.at - last <= merge => {
                if click.peak > previous.peak {
                    *previous = click;
                }
            }
            _ => merged.push(click),
        }
        last_at = Some(click.at);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onsets_match_the_nearest_within_the_window_once() {
        let pairs = match_onsets(&[100, 200, 300, 1000], &[95, 205, 210, 2000], 20);
        assert_eq!(pairs, [(100, 95), (200, 205)]);
    }

    #[test]
    fn a_click_rises_above_the_high_frequencies_around_it() {
        let mut high = vec![0.0; 10_000];
        for (i, v) in high.iter_mut().enumerate() {
            *v = if i % 2 == 0 { 0.001 } else { -0.001 };
        }
        high[5000] = 0.5;
        high[5003] = 0.4;
        let clicks = merge_clicks(find_clicks(&high, 48, 2400, 20.0), 240);
        assert_eq!(clicks.len(), 1);
        assert_eq!(clicks[0].at, 5000);
        assert!((20.0 * (clicks[0].peak / clicks[0].around).log10() - 53.98).abs() < 0.01);
    }
}

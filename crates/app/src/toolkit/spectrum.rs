//! audio.spectrum, which measures a file's mean spectrum: its levels in
//! third octaves or octaves, where its power is centred, how flat it is,
//! its strongest peaks as notes, and, given a pitch, how pure and steady
//! each of its partials is.

use musicable_commands::{Args, Param, Type};
use serde_json::{Value as Json, json};

use super::spectra::{self, Partial, Welch, note, power_db};
use super::{
    AudioCommand, FILE, Job, MOST, Source, decibels, opt, position, range, round, schema, seconds,
    stretch_params,
};

/// The band sets.
const BANDS: &[&str] = &["third_octaves", "octaves"];
/// The partials followed, at most, and the highest followed.
const MOST_PARTIALS: usize = 32;
const HIGHEST_PARTIAL: f64 = 20_000.0;
/// A partial's envelope is low-passed at a quarter of the pitch, or this,
/// whichever is lower: beats faster than it pass less.
const RIPPLE_CUTOFF: f64 = 25.0;
/// A partial this far below the loudest, as a share of its power (60 dB),
/// is too quiet for its purity or beating to mean anything.
const QUIET: f64 = 1e-6;
/// How much of the power the roll-off is below.
const ROLLOFF: f64 = 0.85;

pub(super) fn commands() -> Vec<AudioCommand> {
    vec![spectrum()]
}

fn spectrum() -> AudioCommand {
    let mut params = vec![
        FILE,
        Param::optional(
            "bands",
            Type::Choice(BANDS),
            "The bands to give the levels of: third_octaves, 31 from 20 Hz to 20 kHz, or \
             octaves, 10 from 31.5 Hz to 16 kHz. Without it, third_octaves.",
        ),
        Param::optional(
            "peaks",
            Type::Int,
            "How many of the strongest peaks to give, from 0 to 200. Without it, 10.",
        ),
        Param::optional(
            "fundamental_hz",
            Type::Number,
            "A pitch, in hertz, whose partials to follow: how loud each harmonic is, how \
             much of the power around it is on it, and how much its level ripples, as two \
             partials close together beat. Give a stretch where the note is held.",
        ),
        Param::optional(
            "partials",
            Type::Int,
            "How many of fundamental_hz's harmonics to follow, from 1 to 32. Without it, 16.",
        ),
    ];
    params.extend(stretch_params(false));
    AudioCommand {
        id: "audio.spectrum",
        title: "Measure Spectrum",
        about: "Measures a file's mean spectrum, its channels' power averaged, over Hann \
                windows of about 0.3 s that overlap by half: its levels in third octaves or \
                octaves, the centre of its power, where 85% of it is below, how flat it is, \
                and its strongest peaks, each with the note nearest it. Given a pitch, it \
                follows its harmonics too, up to 32 of them: each one's level, how pure it \
                is and how much it beats. With from or to, it measures that stretch of the \
                file.",
        params,
        positional: &["file"],
        read_only: true,
        example: &[
            "voice.aif",
            "--bands",
            "octaves",
            "--peaks",
            "3",
            "--fundamental-hz",
            "240",
            "--partials",
            "4",
        ],
        run: run_spectrum,
        output,
    }
}

fn output() -> Json {
    let band = schema::object(
        "A band.",
        &[
            (
                "centre_hz",
                schema::number("Its centre, in hertz, as IEC 61260 names it."),
            ),
            (
                "dbfs",
                schema::level(
                    "Its level, as an RMS level, in dBFS: a full-scale sine reads -3.01.",
                ),
            ),
        ],
    );
    let peak = schema::object(
        "A peak.",
        &[
            (
                "hz",
                schema::number("Its frequency, in hertz, to 2 places."),
            ),
            (
                "dbfs",
                schema::level("Its level, as an RMS level, in dBFS."),
            ),
            (
                "note",
                schema::text("The note nearest it, such as A4 for 440 Hz: middle C is C4."),
            ),
            (
                "cents",
                schema::number("How far above that note it is, in cents: below if negative."),
            ),
        ],
    );
    let partial = schema::object(
        "A harmonic of the pitch given.",
        &[
            (
                "number",
                schema::count("Which harmonic it is: 1 is the pitch itself."),
            ),
            ("hz", schema::number("Its frequency, in hertz.")),
            (
                "dbfs",
                schema::level(
                    "The level of the band around it, half the pitch either way, as an RMS \
                     level, in dBFS.",
                ),
            ),
            (
                "purity",
                schema::level(
                    "The share of the band's power within 2.5 bins of the harmonic, from 0 to \
                     1: near 1 for a clean partial, lower when its power is spread or split. \
                     Null if it's silent, or 60 dB or more below the loudest harmonic.",
                ),
            ),
            (
                "beating_db",
                schema::level(
                    "How much its level ripples, in decibels, from the 10th percentile of its \
                     envelope to the 90th: near 0 for a steady partial. Null as purity is.",
                ),
            ),
        ],
    );
    schema::object(
        "A file's mean spectrum.",
        &[
            ("file", schema::text("Its whole path.")),
            (
                "from",
                schema::position("Where the stretch measured starts."),
            ),
            ("to", schema::position("Where it ends.")),
            ("seconds", schema::number("How long it is.")),
            (
                "bin_hz",
                schema::number(
                    "How far apart the spectrum's bins are, in hertz: how finely it tells \
                     frequencies apart.",
                ),
            ),
            (
                "rms_dbfs",
                schema::level("Its RMS level, its channels' power averaged, in dBFS."),
            ),
            (
                "centroid_hz",
                schema::level("The centre of its power, in hertz: null for silence."),
            ),
            (
                "rolloff_hz",
                schema::level("The frequency 85% of its power is below: null for silence."),
            ),
            (
                "flatness",
                schema::level(
                    "How flat its spectrum is from 20 Hz to 20 kHz, from 0 to 1: 1 for white \
                     noise, near 0 for a tone. Null for silence.",
                ),
            ),
            (
                "bands",
                schema::list(band, "Its level in each band below half the sample rate."),
            ),
            (
                "peaks",
                schema::list(peak, "Its strongest peaks, strongest first."),
            ),
            (
                "partials",
                schema::list(
                    partial,
                    "The harmonics of fundamental_hz below half the sample rate and 20 kHz, \
                     as many as partials says: none without it.",
                ),
            ),
        ],
    )
}

fn run_spectrum(args: &Args, job: &Job) -> Result<Json, String> {
    let path = job.file(args, "file")?;
    let octaves = opt::<String>(args, "bands")?.as_deref() == Some("octaves");
    let count = opt::<i64>(args, "peaks")?.unwrap_or(10);
    if !(0..=MOST as i64).contains(&count) {
        return Err(format!("peaks is from 0 to {MOST}, not {count}."));
    }
    let mut source = Source::open(&path)?;
    let (rate, channels) = (source.rate(), source.channels());
    let nyquist = f64::from(rate) / 2.0;
    let fundamental = opt::<f64>(args, "fundamental_hz")?;
    if let Some(hz) = fundamental
        && !(hz > 0.0 && hz < nyquist)
    {
        return Err(format!(
            "fundamental_hz is a pitch above 0 and below half the sample rate, {nyquist} Hz, \
             not {hz}."
        ));
    }
    let most_partials = opt::<i64>(args, "partials")?.unwrap_or(16);
    if !(1..=MOST_PARTIALS as i64).contains(&most_partials) {
        return Err(format!(
            "partials is from 1 to {MOST_PARTIALS}, not {most_partials}."
        ));
    }
    let stretch = range(args, &source, false)?;

    let mut welches: Vec<Welch> = (0..channels).map(|_| Welch::new(rate)).collect();
    let harmonics: Vec<f64> = match fundamental {
        Some(hz) => (1..=most_partials as usize)
            .map(|n| n as f64 * hz)
            .take_while(|&partial| partial < nyquist && partial <= HIGHEST_PARTIAL)
            .collect(),
        None => Vec::new(),
    };
    let cutoff = fundamental.map_or(RIPPLE_CUTOFF, |hz| (hz / 4.0).min(RIPPLE_CUTOFF));
    let mut partials: Vec<Partial> = (harmonics.iter())
        .map(|&hz| Partial::new(hz, cutoff, rate))
        .collect();
    let mut channel = Vec::new();
    let mut mono = Vec::new();
    let share = super::Share::ALL;
    source.each_chunk(stretch.0, stretch.1, job, share, |samples| {
        for (c, welch) in welches.iter_mut().enumerate() {
            channel.clear();
            channel.extend(samples.iter().skip(c).step_by(channels));
            welch.push(&channel);
        }
        if !partials.is_empty() {
            mono.clear();
            let scale = 1.0 / channels as f64;
            mono.extend(
                (samples.chunks_exact(channels)).map(|frame| frame.iter().sum::<f64>() * scale),
            );
            for partial in &mut partials {
                partial.push(&mono);
            }
        }
        Ok(())
    })?;
    // The channels' power, averaged.
    let mut spectra = welches.into_iter().map(Welch::finish);
    let first = spectra.next().flatten();
    let spectrum = first.map(|mut spectrum| {
        let mut others = 0;
        for other in spectra.flatten() {
            spectrum
                .bins
                .iter_mut()
                .zip(&other.bins)
                .for_each(|(a, b)| *a += b);
            others += 1;
        }
        let scale = 1.0 / (others + 1) as f64;
        spectrum.bins.iter_mut().for_each(|bin| *bin *= scale);
        spectrum
    });
    let Some(spectrum) = spectrum else {
        return Err(format!(
            "The stretch from frame {} to frame {} is too short to measure: give 256 frames or \
             more.",
            stretch.0, stretch.1
        ));
    };

    let bands: Vec<Json> = (spectra::bands(octaves, nyquist).into_iter())
        .map(|band| {
            json!({
                "centre_hz": band.centre,
                "dbfs": decibels(spectrum.level(band.from, band.to)),
            })
        })
        .collect();
    let peaks: Vec<Json> = (spectrum.peaks(count as usize).into_iter())
        .map(|peak| {
            let (name, cents) = note(peak.hz);
            json!({
                "hz": round(peak.hz, 2),
                "dbfs": decibels(power_db(peak.power)),
                "note": name,
                "cents": round(cents, 1),
            })
        })
        .collect();
    let near = 2.5 * spectrum.width();
    let half = fundamental.unwrap_or(0.0) / 2.0;
    let arounds: Vec<f64> = (harmonics.iter())
        .map(|&hz| spectrum.power(hz - half, hz + half))
        .collect();
    // Partials far below the loudest, such as a square wave's even ones,
    // are noise: their purity and beating would say nothing.
    let loudest = arounds.iter().fold(0.0f64, |a, &b| a.max(b));
    let heard = loudest * QUIET;
    let partials: Vec<Json> = (harmonics.iter().zip(partials).zip(&arounds).enumerate())
        .map(|(i, ((&hz, partial), &around))| {
            let on = spectrum.power(hz - near, hz + near);
            let loud = around > 0.0 && around >= heard;
            let purity = loud.then(|| round((on / around).min(1.0), 3));
            let beating = partial.ripple().filter(|_| loud);
            json!({
                "number": i + 1,
                "hz": round(hz, 2),
                "dbfs": decibels(power_db(around)),
                "purity": purity,
                "beating_db": decibels(beating),
            })
        })
        .collect();
    let decimals =
        |value: Option<f64>, places| value.map_or(Json::Null, |v| json!(round(v, places)));
    Ok(json!({
        "file": path.display().to_string(),
        "from": position(stretch.0, rate),
        "to": position(stretch.1, rate),
        "seconds": seconds(stretch.1 - stretch.0, rate),
        "bin_hz": round(spectrum.width(), 3),
        "rms_dbfs": decibels(spectrum.total()),
        "centroid_hz": decimals(spectrum.centroid(), 1),
        "rolloff_hz": decimals(spectrum.rolloff(ROLLOFF), 1),
        "flatness": decimals(spectrum.flatness(), 4),
        "bands": bands,
        "peaks": peaks,
        "partials": partials,
    }))
}

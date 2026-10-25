//! High quality: a clip in Spectral rendered with Rubber Band's R3 engine
//! ([`musicable_stretch::rubber_band`]), as Live's Complex Pro renders it,
//! the best quality and slower to render.
//!
//! Each segment renders in one run of a fresh stretcher over the stretch of
//! the file it plays, with a quarter of a second of the file either side,
//! so the stretcher has settled where the segment starts and still hears
//! what follows where it ends. The run's key frames, where the warp's
//! markers and the song's tempo changes fall and at the segment's ends, put
//! each frame of the file out where the warp says it plays. Where the
//! clip's Transpose, Detune, Formants and Envelope hold still over a run,
//! it renders offline, at one setting. Otherwise it renders in real time,
//! still ahead of playback, each hop at the settings where it's centred in
//! the file, as they are every [`STEP`] frames of it.
//!
//! Formants keep the formants in place as R3's formant scale does, by the
//! inverse of the pitch at 100, and leave them to move with it at 0, as
//! [`SpectralSettings::formant_ratio`] has them for Signalsmith Stretch.
//! Envelope scales how finely the outline of the spectrum the formants are
//! kept by follows it: by Envelope / 128, so the default, 128, is R3's own.

use std::hash::Hasher;
use std::io;

use musicable_model::warp::{SpectralSettings, pitch_ratio};
use musicable_stretch::rubber_band::{self, MOST_FRAMES, RUBBER_BAND_VERSION, RubberBand};

use super::{Input, Pitches, Plan, SLACK, SegmentPlan, Value};
use crate::media::{AudioData, AudioReader};

/// How much of the file a run reads before its segment and after it, in
/// seconds of the file.
const MARGIN_SECONDS: f64 = 0.25;

/// How many frames of the file apart a run in real time takes its
/// settings: about every 5 ms.
const STEP: usize = 256;

/// What a stretcher plays a hop at: its pitch scale, its formant scale, or
/// none where the formants move with the pitch, and its envelope scale.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tone {
    pitch: f64,
    formants: Option<f64>,
    envelope: f64,
}

impl Tone {
    /// A transpose of `ratio`, with Formants at `formants` and Envelope at
    /// `envelope`: kept with a formant scale if `keep`, and otherwise left
    /// to the pitch, which costs least and makes the envelope moot.
    fn new(ratio: f64, formants: f64, envelope: f64, keep: bool) -> Self {
        match keep {
            true => Self {
                pitch: ratio,
                formants: Some(ratio.powf(-formants / SpectralSettings::FORMANTS_MAX)),
                envelope: envelope / SpectralSettings::ENVELOPE_DEFAULT,
            },
            false => Self {
                pitch: ratio,
                formants: None,
                envelope: 1.0,
            },
        }
    }

    fn apply(&self, stretch: &mut RubberBand) {
        stretch.set_pitch(self.pitch);
        stretch.set_formants(self.formants);
        stretch.set_envelope(self.envelope);
    }

    fn hash(&self, hasher: &mut impl Hasher) {
        hasher.write_u64(self.pitch.to_bits());
        hasher.write_u64(self.formants.map_or(0, f64::to_bits));
        hasher.write_u64(self.envelope.to_bits());
    }
}

/// The transpose's ratio, Formants and Envelope at `own` of the clip's own
/// time.
fn settings(pitches: &Pitches, own: f64) -> (f64, f64, f64) {
    let ratio = pitch_ratio(pitches.transpose.at(own), pitches.detune.at(own));
    (ratio, pitches.formants.at(own), pitches.envelope.at(own))
}

/// Whether a transpose of `ratio` with Formants at `formants` needs a
/// formant scale: whether it moves the pitch, and the formants stay.
fn keeps(ratio: f64, formants: f64) -> bool {
    ratio != 1.0 && formants != 0.0
}

/// The settings a run plays at.
#[derive(Debug)]
enum Tones {
    /// One, offline.
    Fixed(Tone),
    /// One for each [`STEP`] frames of the file from the run's start, in
    /// real time.
    Moving(Vec<Tone>),
}

/// How Rubber Band renders a segment: in one run of a fresh stretcher over
/// the file from frame `from`, `input` frames of it, putting out `output`
/// frames from grid frame `out` on.
#[derive(Debug)]
pub(super) struct Run {
    from: i64,
    input: usize,
    out: i64,
    output: usize,
    /// Its key frames: frames of its input, counted from `from`, each with
    /// the frame of its output it comes out on, counted from `out`.
    keys: Vec<(usize, usize)>,
    tones: Tones,
}

impl Run {
    /// How `plan`'s segment from grid frame `start`, `frames` long, renders.
    pub(super) fn new(plan: &Plan, start: i64, frames: usize) -> Self {
        let (map, tempo) = (&plan.map, &plan.tempo);
        let margin = (MARGIN_SECONDS * f64::from(map.rate)).round() as i64;
        // The file's frames the segment starts and ends on, and the run's.
        let end = start + frames as i64;
        let first = (map.position(tempo, start as f64) + SLACK).floor() as i64;
        let last = ((map.position(tempo, end as f64) - SLACK).ceil() as i64).max(first + 1);
        let (from, to) = (first - margin, last + margin);
        // The grid frame each of the file's frames plays on.
        let grid = |frame: i64| map.grid_of(tempo, map.own_of(frame as f64)).round() as i64;
        let out = grid(from);
        let (input, output) = ((to - from) as usize, (grid(to) - out).max(1) as usize);
        // Where the file's speed changes, at markers and tempo changes, and
        // the segment's ends.
        let mut turns = vec![first, last];
        turns.extend(map.segments[1..].iter().map(|s| s.frame as i64));
        let changes = tempo.tempos().windows(2).filter(|w| w[1].bpm != w[0].bpm);
        let frame_of = |own: f64| map.frame_of(own).round() as i64;
        turns.extend(changes.map(|w| frame_of(w[1].at.to_f64() + map.shift)));
        turns.retain(|&frame| from < frame && frame < to);
        turns.sort_unstable();
        turns.dedup();
        let mut keys: Vec<(usize, usize)> = Vec::with_capacity(turns.len());
        for frame in turns {
            let key = ((frame - from) as usize, (grid(frame) - out).max(0) as usize);
            let after = keys.last().map_or(0, |last| last.1);
            if key.1 > after && key.1 < output {
                keys.push(key);
            }
        }
        Self {
            from,
            input,
            out,
            output,
            keys,
            tones: tones(plan, from, input),
        }
    }

    /// Whether it renders offline, at one setting.
    #[cfg(test)]
    pub(super) fn is_offline(&self) -> bool {
        matches!(self.tones, Tones::Fixed(_))
    }

    /// Hashes what its render depends on, for a segment from grid frame
    /// `start`: not where in the song it plays.
    pub(super) fn hash(&self, start: i64, hasher: &mut impl Hasher) {
        hasher.write_u32(RUBBER_BAND_VERSION);
        hasher.write_i64(self.from);
        hasher.write_u64(self.input as u64);
        hasher.write_u64(self.output as u64);
        hasher.write_i64(start - self.out);
        hasher.write_u64(self.keys.len() as u64);
        for &(input, output) in &self.keys {
            hasher.write_u64(input as u64);
            hasher.write_u64(output as u64);
        }
        match &self.tones {
            Tones::Fixed(tone) => {
                hasher.write_u8(0);
                tone.hash(hasher);
            }
            Tones::Moving(tones) => {
                hasher.write_u8(1);
                for tone in tones {
                    tone.hash(hasher);
                }
            }
        }
    }

    /// Renders `segment` of `plan` from `reader`, the clip's file, in this
    /// run.
    pub(super) fn render(
        &self,
        plan: &Plan,
        segment: &SegmentPlan,
        reader: &mut dyn AudioReader,
    ) -> io::Result<AudioData> {
        let (channels, rate) = (plan.channels, plan.map.rate);
        let ratio = self.output as f64 / self.input as f64;
        let mut stretch = match &self.tones {
            Tones::Fixed(tone) => RubberBand::offline(channels, rate, ratio, tone.pitch),
            Tones::Moving(tones) => RubberBand::real_time(channels, rate, ratio, tones[0].pitch),
        };
        if !self.keys.is_empty() {
            stretch.set_key_frames(&self.keys);
        }
        if let Tones::Fixed(tone) = &self.tones {
            tone.apply(&mut stretch);
            stretch.study(self.input);
        }
        // In real time, the file before the run is its start pad, which it
        // puts out before its first frame, as its start delay.
        let (pad, delay) = (stretch.start_pad(), stretch.start_delay());
        let lookahead = rubber_band::lookahead(rate);
        let mut file = Input::new(channels, pad + self.input);
        let file = file.read(reader, self.from - pad as i64, pad + self.input)?;
        let mut output: Vec<Vec<f32>> = (0..channels)
            .map(|_| Vec::with_capacity(delay + self.output + MOST_FRAMES))
            .collect();
        let mut fed = 0;
        let mut playing = None;
        while fed < pad + self.input {
            let frames = stretch.wants().min(pad + self.input - fed).min(MOST_FRAMES);
            if let Tones::Moving(tones) = &self.tones {
                // The hop this call makes, and the settings nearest it.
                let centre = (fed + frames) as i64 - (pad + lookahead) as i64;
                let step = STEP as i64;
                let k = (centre + step / 2)
                    .div_euclid(step)
                    .clamp(0, tones.len() as i64 - 1);
                if playing != Some(k) {
                    tones[k as usize].apply(&mut stretch);
                    playing = Some(k);
                }
            }
            let input: Vec<&[f32]> = file[..channels]
                .iter()
                .map(|c| &c[fed..fed + frames])
                .collect();
            stretch.process(&input, false);
            fed += frames;
            drain(&mut stretch, &mut output);
        }
        let none: Vec<&[f32]> = vec![&[]; channels];
        stretch.process(&none, true);
        drain(&mut stretch, &mut output);
        // The segment's own frames, in what the run put out.
        let skip = delay + (segment.start() - self.out) as usize;
        let frames = segment.frames();
        for channel in &mut output {
            channel.drain(..skip.min(channel.len()));
            channel.truncate(frames);
            channel.resize(frames, 0.0);
        }
        Ok(AudioData::new(rate, output))
    }
}

/// Moves everything `stretch` has ready onto the ends of `output`'s
/// channels.
fn drain(stretch: &mut RubberBand, output: &mut [Vec<f32>]) {
    while let Some(ready) = stretch.available().filter(|&n| n > 0) {
        let at = output[0].len();
        for channel in output.iter_mut() {
            channel.resize(at + ready, 0.0);
        }
        let mut outs: Vec<&mut [f32]> = output.iter_mut().map(|c| &mut c[at..]).collect();
        let got = stretch.retrieve(&mut outs);
        for channel in output.iter_mut() {
            channel.truncate(at + got);
        }
    }
}

/// The settings a run of `plan`'s clip from file frame `from`, `input`
/// frames long, plays at: one, where they hold still over it, or else one
/// for each [`STEP`] frames, with the formants kept all through if they are
/// anywhere.
fn tones(plan: &Plan, from: i64, input: usize) -> Tones {
    let (map, pitches) = (&plan.map, &plan.pitches);
    let at = |frame: i64| settings(pitches, map.own_of(frame as f64));
    let values = [
        &pitches.transpose,
        &pitches.detune,
        &pitches.formants,
        &pitches.envelope,
    ];
    if values.iter().all(|value| matches!(value, Value::Fixed(_))) {
        let (ratio, formants, envelope) = at(from);
        return Tones::Fixed(Tone::new(ratio, formants, envelope, keeps(ratio, formants)));
    }
    let samples: Vec<(f64, f64, f64)> = (0..=input.div_ceil(STEP))
        .map(|k| at(from + (k * STEP) as i64))
        .collect();
    let keep = samples
        .iter()
        .any(|&(ratio, formants, _)| keeps(ratio, formants));
    let tones: Vec<Tone> = (samples.into_iter())
        .map(|(ratio, formants, envelope)| Tone::new(ratio, formants, envelope, keep))
        .collect();
    match tones.iter().all(|tone| *tone == tones[0]) {
        true => Tones::Fixed(tones[0]),
        false => Tones::Moving(tones),
    }
}

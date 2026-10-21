//! The current version of the format, as serde types.
//!
//! These types are the format: they read and write exactly the files that
//! `docs/project-format.md` describes. Changing them changes the format, so
//! it takes a new [`VERSION`] and a migration step (see `migrate.rs`).
//!
//! The model isn't serialized directly, so it can change shape without the
//! files changing. The conversions name every field of the model types they
//! can, so a new field doesn't compile until the format handles it.

use std::collections::HashMap;

use musicable_model::time::{self, Beats};
use musicable_model::{self as model, TrackId};
use serde::{Deserialize, Serialize};

use super::{FORMAT, Invalid, MAX_TICKS, VERSION};

/// A project file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct File {
    format: String,
    version: u32,
    ticks_per_beat: i64,
    tempo: TempoMap,
    loop_region: BeatRange,
    looping: bool,
    metronome: bool,
    next_track_id: u32,
    tracks: Vec<Track>,
}

/// Tempo and time signature changes, each list in order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TempoMap {
    tempos: Vec<TempoChange>,
    signatures: Vec<SignatureChange>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TempoChange {
    /// In ticks.
    at: i64,
    /// Quarter notes per minute.
    bpm: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureChange {
    bar: i64,
    numerator: u8,
    denominator: u8,
}

/// In ticks.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeatRange {
    start: i64,
    end: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    id: u32,
    name: String,
    kind: TrackKind,
    gain: f32,
    muted: bool,
    // Required like every other field: serde would read a missing `Option`
    // as `None`, but not through `deserialize_with`.
    #[serde(deserialize_with = "Option::deserialize")]
    tone: Option<TestTone>,
    clip_type: ClipType,
    console: ConsoleSettings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TrackKind {
    Audio,
    Midi,
    Return,
    Group,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestTone {
    frequency: f64,
    level: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClipType {
    Off,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleSettings {}

impl From<&model::Project> for File {
    fn from(project: &model::Project) -> Self {
        let model::Project {
            tempo,
            loop_region,
            looping,
            metronome,
            tracks,
            next_track_id,
        } = project;
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            ticks_per_beat: Beats::TICKS_PER_BEAT,
            tempo: TempoMap::from(tempo),
            loop_region: BeatRange::from(*loop_region),
            looping: *looping,
            metronome: *metronome,
            next_track_id: *next_track_id,
            tracks: tracks.iter().map(Track::from).collect(),
        }
    }
}

impl File {
    /// The project the file holds, or the first rule of the format it breaks.
    pub(super) fn to_project(&self) -> Result<model::Project, Invalid> {
        let Self {
            // The header was checked before the file was read.
            format: _,
            version: _,
            ticks_per_beat,
            tempo,
            loop_region,
            looping,
            metronome,
            next_track_id,
            tracks,
        } = self;
        if *ticks_per_beat != Beats::TICKS_PER_BEAT {
            let message = format!("must be {}", Beats::TICKS_PER_BEAT);
            return Err(Invalid::new("ticks_per_beat", message));
        }
        let tempo = tempo.to_model()?;
        let loop_region = loop_region.to_model("loop_region")?;
        let mut ids = HashMap::with_capacity(tracks.len());
        let mut model_tracks = Vec::with_capacity(tracks.len());
        for (i, track) in tracks.iter().enumerate() {
            let path = format!("tracks[{i}]");
            if let Some(first) = ids.insert(track.id, i) {
                let message = format!("tracks[{first}] has id {} too", track.id);
                return Err(Invalid::new(format!("{path}.id"), message));
            }
            model_tracks.push(track.to_model(&path)?);
        }
        let highest = tracks.iter().map(|t| t.id).max();
        if let Some(highest) = highest.filter(|&id| id >= *next_track_id) {
            let message = format!("must be above every track's id, and a track has id {highest}");
            return Err(Invalid::new("next_track_id", message));
        }
        Ok(model::Project {
            tempo,
            loop_region,
            looping: *looping,
            metronome: *metronome,
            tracks: model_tracks,
            next_track_id: *next_track_id,
        })
    }
}

impl From<&time::TempoMap> for TempoMap {
    fn from(map: &time::TempoMap) -> Self {
        let tempos = map.tempos().iter().map(|change| TempoChange {
            at: change.at.ticks(),
            bpm: change.bpm,
        });
        let signatures = map.signatures().iter().map(|change| SignatureChange {
            bar: change.bar,
            numerator: change.signature.numerator(),
            denominator: change.signature.denominator(),
        });
        Self {
            tempos: tempos.collect(),
            signatures: signatures.collect(),
        }
    }
}

impl TempoMap {
    fn to_model(&self) -> Result<time::TempoMap, Invalid> {
        let Self { tempos, signatures } = self;
        let bpms = time::TempoMap::MIN_BPM..=time::TempoMap::MAX_BPM;
        for (i, change) in tempos.iter().enumerate() {
            let path = format!("tempo.tempos[{i}]");
            if i == 0 && change.at != 0 {
                let message = "must be 0: the first tempo is where the song starts";
                return Err(Invalid::new(format!("{path}.at"), message));
            }
            if i > 0 && change.at <= tempos[i - 1].at {
                let message = "must be after the tempo change before it";
                return Err(Invalid::new(format!("{path}.at"), message));
            }
            position(change.at, || format!("{path}.at"))?;
            if !bpms.contains(&change.bpm) {
                let message = format!("must be from {} to {}", bpms.start(), bpms.end());
                return Err(Invalid::new(format!("{path}.bpm"), message));
            }
        }

        let mut checked: Vec<(i64, time::TimeSignature)> = Vec::with_capacity(signatures.len());
        // Where the previous change's bar starts, in ticks. An i128, so even
        // the last bar an i64 can number can't overflow it.
        let mut start = 0_i128;
        for (i, change) in signatures.iter().enumerate() {
            let path = format!("tempo.signatures[{i}]");
            let SignatureChange {
                bar,
                numerator,
                denominator,
            } = *change;
            let Some(signature) = time::TimeSignature::new(numerator, denominator) else {
                let message = format!(
                    "{numerator}/{denominator} isn't a time signature: the numerator must be \
                     1 or more, and the denominator a power of two up to 64"
                );
                return Err(Invalid::new(path, message));
            };
            match checked.last() {
                None if bar != 0 => {
                    let message = "must be 0: the first time signature is where the song starts";
                    return Err(Invalid::new(format!("{path}.bar"), message));
                }
                None => {}
                Some(&(previous, _)) if bar <= previous => {
                    let message = "must be after the time signature change before it";
                    return Err(Invalid::new(format!("{path}.bar"), message));
                }
                Some(&(previous, previous_signature)) => {
                    let bar_length = previous_signature.bar_length().ticks();
                    start += i128::from(bar_length) * i128::from(bar - previous);
                    if start > i128::from(MAX_TICKS) {
                        let message = format!(
                            "starts more than {MAX_TICKS} ticks after the start of the song"
                        );
                        return Err(Invalid::new(format!("{path}.bar"), message));
                    }
                }
            }
            checked.push((bar, signature));
        }

        let (Some(first_tempo), Some(&(_, first_signature))) = (tempos.first(), checked.first())
        else {
            let path = if tempos.is_empty() {
                "tempo.tempos"
            } else {
                "tempo.signatures"
            };
            return Err(Invalid::new(
                path,
                "needs at least one change, at the start",
            ));
        };
        let mut map = time::TempoMap::new(first_tempo.bpm, first_signature);
        for change in &tempos[1..] {
            map.insert_tempo(Beats::from_ticks(change.at), change.bpm);
        }
        for &(bar, signature) in &checked[1..] {
            map.insert_signature(bar, signature);
        }
        Ok(map)
    }
}

impl From<time::BeatRange> for BeatRange {
    fn from(range: time::BeatRange) -> Self {
        let time::BeatRange { start, end } = range;
        Self {
            start: start.ticks(),
            end: end.ticks(),
        }
    }
}

impl BeatRange {
    fn to_model(&self, path: &str) -> Result<time::BeatRange, Invalid> {
        let Self { start, end } = *self;
        Ok(time::BeatRange::new(
            position(start, || format!("{path}.start"))?,
            position(end, || format!("{path}.end"))?,
        ))
    }
}

impl From<&model::Track> for Track {
    fn from(track: &model::Track) -> Self {
        let model::Track {
            id,
            name,
            kind,
            gain,
            muted,
            tone,
            clip_type,
            console,
        } = track;
        let model::ConsoleSettings {} = console;
        Self {
            id: id.0,
            name: name.clone(),
            kind: match kind {
                model::TrackKind::Audio => TrackKind::Audio,
                model::TrackKind::Midi => TrackKind::Midi,
                model::TrackKind::Return => TrackKind::Return,
                model::TrackKind::Group => TrackKind::Group,
            },
            gain: *gain,
            muted: *muted,
            tone: tone.map(|model::TestTone { frequency, level }| TestTone { frequency, level }),
            clip_type: match clip_type {
                model::ClipType::Off => ClipType::Off,
            },
            console: ConsoleSettings {},
        }
    }
}

impl Track {
    fn to_model(&self, path: &str) -> Result<model::Track, Invalid> {
        let Self {
            id,
            name,
            kind,
            gain,
            muted,
            tone,
            clip_type,
            console,
        } = self;
        let ConsoleSettings {} = console;
        let tone = match tone {
            Some(TestTone { frequency, level }) => Some(model::TestTone {
                frequency: finite(*frequency, || format!("{path}.tone.frequency"))?,
                level: finite(*level, || format!("{path}.tone.level"))?,
            }),
            None => None,
        };
        Ok(model::Track {
            id: TrackId(*id),
            name: name.clone(),
            kind: match kind {
                TrackKind::Audio => model::TrackKind::Audio,
                TrackKind::Midi => model::TrackKind::Midi,
                TrackKind::Return => model::TrackKind::Return,
                TrackKind::Group => model::TrackKind::Group,
            },
            gain: finite(*gain, || format!("{path}.gain"))?,
            muted: *muted,
            tone,
            clip_type: match clip_type {
                ClipType::Off => model::ClipType::Off,
            },
            console: model::ConsoleSettings {},
        })
    }
}

/// A position in ticks, if it's within [`MAX_TICKS`] of the song's start.
fn position(ticks: i64, path: impl FnOnce() -> String) -> Result<Beats, Invalid> {
    if ticks.unsigned_abs() <= MAX_TICKS.unsigned_abs() {
        Ok(Beats::from_ticks(ticks))
    } else {
        let message = format!("must be within {MAX_TICKS} ticks of the start of the song");
        Err(Invalid::new(path(), message))
    }
}

/// JSON can't hold infinities or NaN, and a number too large for an f32
/// reads as infinite.
fn finite<T: Copy + Into<f64>>(value: T, path: impl FnOnce() -> String) -> Result<T, Invalid> {
    if value.into().is_finite() {
        Ok(value)
    } else {
        Err(Invalid::new(path(), "must be a finite number"))
    }
}

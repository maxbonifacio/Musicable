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

use musicable_model::clip::{self, ClipId, Dimension, NoteId};
use musicable_model::mixer::Bus;
use musicable_model::time::{self, Beats};
use musicable_model::{self as model, DeviceId, SendId, TrackId};
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
    next_send_id: u32,
    next_device_id: u32,
    next_clip_id: u32,
    tracks: Vec<Track>,
    master: Master,
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
    pan: f32,
    muted: bool,
    solo: bool,
    solo_safe: bool,
    armed: bool,
    /// The group track the output feeds, or `None` for the master.
    // Required like every other field: serde would read a missing `Option`
    // as `None`, but not through `deserialize_with`.
    #[serde(deserialize_with = "Option::deserialize")]
    output: Option<u32>,
    sends: Vec<Send>,
    #[serde(deserialize_with = "Option::deserialize")]
    tone: Option<TestTone>,
    clips: Vec<Clip>,
    devices: Vec<Device>,
    clip_type: ClipType,
    console: ConsoleSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TrackKind {
    Audio,
    Midi,
    Return,
    Group,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Send {
    id: u32,
    /// The return track it feeds.
    to: u32,
    level: f32,
    pre_fader: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestTone {
    frequency: f64,
    level: f32,
}

/// Positions and lengths in ticks.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clip {
    id: u32,
    name: String,
    start: i64,
    length: i64,
    content: ClipContent,
}

/// Externally tagged, `{"audio": {..}}`, for the reason [`DeviceKind`] is.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioClip {
    file: String,
    /// In frames of the file.
    offset: u64,
    gain: f32,
    fade_in: i64,
    fade_out: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MidiClip {
    offset: i64,
    next_note_id: u32,
    notes: Vec<Note>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    id: u32,
    start: i64,
    length: i64,
    key: u8,
    velocity: f32,
    release_velocity: f32,
    expression: Expression,
}

/// Each curve's points as `[at, value]` pairs, `at` in ticks from the
/// note's start.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expression {
    pitch_bend: Vec<(i64, f32)>,
    pressure: Vec<(i64, f32)>,
    timbre: Vec<(i64, f32)>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    id: u32,
    kind: DeviceKind,
}

/// Externally tagged, `{"test_gain": {..}}`: serde reads an internally
/// tagged enum through a buffer that holds numbers as f64s, and an f32 read
/// through an f64 is a bit off for a few values.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum DeviceKind {
    TestGain { gain: f32 },
    TestInstrument { level: f32 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClipType {
    Off,
    Hard,
    Soft,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleSettings {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Master {
    gain: f32,
    pan: f32,
    devices: Vec<Device>,
    clip_type: ClipType,
    console: ConsoleSettings,
}

impl From<&model::Project> for File {
    fn from(project: &model::Project) -> Self {
        let model::Project {
            tempo,
            loop_region,
            looping,
            metronome,
            master,
            tracks,
            next_track_id,
            next_send_id,
            next_device_id,
            next_clip_id,
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
            next_send_id: *next_send_id,
            next_device_id: *next_device_id,
            next_clip_id: *next_clip_id,
            tracks: tracks.iter().map(Track::from).collect(),
            master: Master::from(master),
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
            next_send_id,
            next_device_id,
            next_clip_id,
            tracks,
            master,
        } = self;
        if *ticks_per_beat != Beats::TICKS_PER_BEAT {
            let message = format!("must be {}", Beats::TICKS_PER_BEAT);
            return Err(Invalid::new("ticks_per_beat", message));
        }
        let tempo = tempo.to_model()?;
        let loop_region = loop_region.to_model("loop_region")?;
        let mut ids = Ids::new("track", "next_track_id", *next_track_id);
        let mut send_ids = Ids::new("send", "next_send_id", *next_send_id);
        let mut device_ids = Ids::new("device", "next_device_id", *next_device_id);
        let mut clip_ids = Ids::new("clip", "next_clip_id", *next_clip_id);
        let mut model_tracks = Vec::with_capacity(tracks.len());
        for (i, track) in tracks.iter().enumerate() {
            let path = format!("tracks[{i}]");
            ids.add(track.id, &path)?;
            for (j, send) in track.sends.iter().enumerate() {
                send_ids.add(send.id, &format!("{path}.sends[{j}]"))?;
            }
            for (j, clip) in track.clips.iter().enumerate() {
                clip_ids.add(clip.id, &format!("{path}.clips[{j}]"))?;
            }
            for (j, device) in track.devices.iter().enumerate() {
                device_ids.add(device.id, &format!("{path}.devices[{j}]"))?;
            }
            model_tracks.push(track.to_model(&path)?);
        }
        ids.check_next()?;
        send_ids.check_next()?;
        clip_ids.check_next()?;
        let kinds: HashMap<u32, TrackKind> = tracks.iter().map(|t| (t.id, t.kind)).collect();
        for (i, track) in tracks.iter().enumerate() {
            track.check_routing(&format!("tracks[{i}]"), &kinds)?;
        }
        for (j, device) in master.devices.iter().enumerate() {
            device_ids.add(device.id, &format!("master.devices[{j}]"))?;
        }
        device_ids.check_next()?;
        let project = model::Project {
            tempo,
            loop_region,
            looping: *looping,
            metronome: *metronome,
            master: master.to_model()?,
            tracks: model_tracks,
            next_track_id: *next_track_id,
            next_send_id: *next_send_id,
            next_device_id: *next_device_id,
            next_clip_id: *next_clip_id,
        };
        check_loops(&project)?;
        Ok(project)
    }
}

/// The ids of one kind of object seen so far, where each one is, and the
/// counter that must stay above them all.
struct Ids {
    what: &'static str,
    next_field: String,
    next: u32,
    paths: Vec<String>,
    seen: HashMap<u32, usize>,
}

impl Ids {
    fn new(what: &'static str, next_field: impl Into<String>, next: u32) -> Self {
        Self {
            what,
            next_field: next_field.into(),
            next,
            paths: Vec::new(),
            seen: HashMap::new(),
        }
    }

    /// Adds the id of the object at `path`, if nothing else has it.
    fn add(&mut self, id: u32, path: &str) -> Result<(), Invalid> {
        if let Some(&first) = self.seen.get(&id) {
            let message = format!("{} has id {id} too", self.paths[first]);
            return Err(Invalid::new(format!("{path}.id"), message));
        }
        self.seen.insert(id, self.paths.len());
        self.paths.push(path.to_owned());
        Ok(())
    }

    /// Whether the counter is above every id.
    fn check_next(&self) -> Result<(), Invalid> {
        let highest = self.seen.keys().copied().max();
        match highest.filter(|&id| id >= self.next) {
            Some(highest) => {
                let what = self.what;
                let message =
                    format!("must be above every {what}'s id, and a {what} has id {highest}");
                Err(Invalid::new(self.next_field.clone(), message))
            }
            None => Ok(()),
        }
    }
}

/// Refuses groups that feed each other in a loop. Every output names a
/// group track by now, so the first track whose output the model refuses
/// is in a loop.
fn check_loops(project: &model::Project) -> Result<(), Invalid> {
    for (i, track) in project.tracks.iter().enumerate() {
        if project.output_of(track) != track.output {
            let Bus::Group(group) = track.output else {
                unreachable!("the master is always a valid output");
            };
            let message = if group == track.id {
                "can't be the track's own id".to_owned()
            } else {
                format!("makes a loop: group track {} feeds this track", group.0)
            };
            return Err(Invalid::new(format!("tracks[{i}].output"), message));
        }
    }
    Ok(())
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
            pan,
            muted,
            solo,
            solo_safe,
            armed,
            output,
            sends,
            tone,
            clips,
            devices,
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
            pan: *pan,
            muted: *muted,
            solo: *solo,
            solo_safe: *solo_safe,
            armed: *armed,
            output: match output {
                Bus::Master => None,
                Bus::Group(group) => Some(group.0),
            },
            sends: sends.iter().map(Send::from).collect(),
            tone: tone.map(|model::TestTone { frequency, level }| TestTone { frequency, level }),
            clips: clips.iter().map(Clip::from).collect(),
            devices: devices.iter().map(Device::from).collect(),
            clip_type: ClipType::from(*clip_type),
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
            pan,
            muted,
            solo,
            solo_safe,
            armed,
            output,
            sends,
            tone,
            clips,
            devices,
            clip_type,
            console,
        } = self;
        let ConsoleSettings {} = console;
        let kind = match kind {
            TrackKind::Audio => model::TrackKind::Audio,
            TrackKind::Midi => model::TrackKind::Midi,
            TrackKind::Return => model::TrackKind::Return,
            TrackKind::Group => model::TrackKind::Group,
        };
        if *armed && kind.is_bus() {
            let message = "must be false: only audio and MIDI tracks record";
            return Err(Invalid::new(format!("{path}.armed"), message));
        }
        let tone = match tone {
            Some(TestTone { frequency, level }) => Some(model::TestTone {
                frequency: finite(*frequency, || format!("{path}.tone.frequency"))?,
                level: finite(*level, || format!("{path}.tone.level"))?,
            }),
            None => None,
        };
        let sends = sends
            .iter()
            .enumerate()
            .map(|(j, send)| send.to_model(&format!("{path}.sends[{j}]")))
            .collect::<Result<_, _>>()?;
        if kind.is_bus() && !clips.is_empty() {
            let message = "must be empty: only audio and MIDI tracks hold clips";
            return Err(Invalid::new(format!("{path}.clips"), message));
        }
        let clips = clips
            .iter()
            .enumerate()
            .map(|(j, clip)| clip.to_model(&format!("{path}.clips[{j}]"), kind))
            .collect::<Result<_, _>>()?;
        Ok(model::Track {
            id: TrackId(*id),
            name: name.clone(),
            kind,
            gain: finite(*gain, || format!("{path}.gain"))?,
            pan: balance(*pan, || format!("{path}.pan"))?,
            muted: *muted,
            solo: *solo,
            solo_safe: *solo_safe,
            armed: *armed,
            output: match output {
                None => Bus::Master,
                Some(group) => Bus::Group(TrackId(*group)),
            },
            sends,
            tone,
            clips,
            devices: devices_to_model(devices, &format!("{path}.devices"))?,
            clip_type: model::ClipType::from(*clip_type),
            console: model::ConsoleSettings {},
        })
    }

    /// Checks that the output feeds a group track and the sends feed return
    /// tracks. `kinds` has the kind of every track, by id. Loops through
    /// groups are left to [`check_loops`].
    fn check_routing(&self, path: &str, kinds: &HashMap<u32, TrackKind>) -> Result<(), Invalid> {
        if let Some(output) = self.output {
            let path = format!("{path}.output");
            if self.kind == TrackKind::Return {
                let message = "must be null: return tracks feed the master";
                return Err(Invalid::new(path, message));
            }
            match kinds.get(&output) {
                Some(TrackKind::Group) => {}
                Some(_) => {
                    let message = format!("track {output} isn't a group track");
                    return Err(Invalid::new(path, message));
                }
                None => return Err(Invalid::new(path, no_track(output))),
            }
        }
        if self.kind == TrackKind::Return && !self.sends.is_empty() {
            let message = "must be empty: return tracks have no sends";
            return Err(Invalid::new(format!("{path}.sends"), message));
        }
        for (j, send) in self.sends.iter().enumerate() {
            let path = format!("{path}.sends[{j}].to");
            match kinds.get(&send.to) {
                Some(TrackKind::Return) => {}
                Some(_) => {
                    let message = format!("track {} isn't a return track", send.to);
                    return Err(Invalid::new(path, message));
                }
                None => return Err(Invalid::new(path, no_track(send.to))),
            }
            if let Some(first) = self.sends[..j].iter().position(|s| s.to == send.to) {
                let message = format!("sends[{first}] goes to track {} too", send.to);
                return Err(Invalid::new(path, message));
            }
        }
        Ok(())
    }
}

impl From<&clip::Clip> for Clip {
    fn from(clip: &clip::Clip) -> Self {
        let clip::Clip {
            id,
            name,
            start,
            length,
            content,
        } = clip;
        Self {
            id: id.0,
            name: name.clone(),
            start: start.ticks(),
            length: length.ticks(),
            content: match content {
                clip::ClipContent::Audio(audio) => ClipContent::Audio(AudioClip::from(audio)),
                clip::ClipContent::Midi(midi) => ClipContent::Midi(MidiClip::from(midi)),
            },
        }
    }
}

impl Clip {
    /// The clip, on a track of `kind`.
    fn to_model(&self, path: &str, kind: model::TrackKind) -> Result<clip::Clip, Invalid> {
        let Self {
            id,
            name,
            start,
            length,
            content,
        } = self;
        if *start < 0 {
            let message = "must be 0 or more: clips start in the song";
            return Err(Invalid::new(format!("{path}.start"), message));
        }
        let start = position(*start, || format!("{path}.start"))?;
        let length = length_from(start, *length, || format!("{path}.length"))?;
        let content = match (content, kind) {
            (ClipContent::Audio(audio), model::TrackKind::Audio) => {
                clip::ClipContent::Audio(audio.to_model(&format!("{path}.content.audio"), length)?)
            }
            (ClipContent::Midi(midi), model::TrackKind::Midi) => {
                clip::ClipContent::Midi(midi.to_model(&format!("{path}.content.midi"))?)
            }
            (ClipContent::Audio(_), _) => {
                let message = "must be a MIDI clip: MIDI tracks hold MIDI clips";
                return Err(Invalid::new(format!("{path}.content"), message));
            }
            (ClipContent::Midi(_), _) => {
                let message = "must be an audio clip: audio tracks hold audio clips";
                return Err(Invalid::new(format!("{path}.content"), message));
            }
        };
        Ok(clip::Clip {
            id: ClipId(*id),
            name: name.clone(),
            start,
            length,
            content,
        })
    }
}

impl From<&clip::AudioClip> for AudioClip {
    fn from(audio: &clip::AudioClip) -> Self {
        let clip::AudioClip {
            file,
            offset,
            gain,
            fade_in,
            fade_out,
        } = audio;
        Self {
            file: file.clone(),
            offset: *offset,
            gain: *gain,
            fade_in: fade_in.ticks(),
            fade_out: fade_out.ticks(),
        }
    }
}

impl AudioClip {
    /// The clip's audio, for a clip `length` long.
    fn to_model(&self, path: &str, length: Beats) -> Result<clip::AudioClip, Invalid> {
        let Self {
            file,
            offset,
            gain,
            fade_in,
            fade_out,
        } = self;
        if file.is_empty() {
            return Err(Invalid::new(format!("{path}.file"), "must name a file"));
        }
        if *offset > MAX_TICKS.unsigned_abs() {
            let message = format!("must be {MAX_TICKS} or less");
            return Err(Invalid::new(format!("{path}.offset"), message));
        }
        for (fade, name) in [(*fade_in, "fade_in"), (*fade_out, "fade_out")] {
            if !(0..=length.ticks()).contains(&fade) {
                let message = "must be from 0 to the clip's length";
                return Err(Invalid::new(format!("{path}.{name}"), message));
            }
        }
        if fade_in + fade_out > length.ticks() {
            let message = "must fit in the clip's length with fade_in";
            return Err(Invalid::new(format!("{path}.fade_out"), message));
        }
        Ok(clip::AudioClip {
            file: file.clone(),
            offset: *offset,
            gain: finite(*gain, || format!("{path}.gain"))?,
            fade_in: Beats::from_ticks(*fade_in),
            fade_out: Beats::from_ticks(*fade_out),
        })
    }
}

impl From<&clip::MidiClip> for MidiClip {
    fn from(midi: &clip::MidiClip) -> Self {
        let clip::MidiClip {
            offset,
            next_note_id,
            notes,
        } = midi;
        Self {
            offset: offset.ticks(),
            next_note_id: *next_note_id,
            notes: notes.iter().map(Note::from).collect(),
        }
    }
}

impl MidiClip {
    fn to_model(&self, path: &str) -> Result<clip::MidiClip, Invalid> {
        let Self {
            offset,
            next_note_id,
            notes,
        } = self;
        let mut ids = Ids::new("note", format!("{path}.next_note_id"), *next_note_id);
        let mut model_notes = Vec::with_capacity(notes.len());
        for (k, note) in notes.iter().enumerate() {
            let path = format!("{path}.notes[{k}]");
            ids.add(note.id, &path)?;
            model_notes.push(note.to_model(&path)?);
        }
        ids.check_next()?;
        Ok(clip::MidiClip {
            offset: position(*offset, || format!("{path}.offset"))?,
            next_note_id: *next_note_id,
            notes: model_notes,
        })
    }
}

impl From<&clip::Note> for Note {
    fn from(note: &clip::Note) -> Self {
        let clip::Note {
            id,
            start,
            length,
            key,
            velocity,
            release_velocity,
            expression,
        } = note;
        let clip::Expression {
            pitch_bend,
            pressure,
            timbre,
        } = expression;
        let points =
            |curve: &[clip::Point]| curve.iter().map(|p| (p.at.ticks(), p.value)).collect();
        Self {
            id: id.0,
            start: start.ticks(),
            length: length.ticks(),
            key: *key,
            velocity: *velocity,
            release_velocity: *release_velocity,
            expression: Expression {
                pitch_bend: points(pitch_bend),
                pressure: points(pressure),
                timbre: points(timbre),
            },
        }
    }
}

impl Note {
    fn to_model(&self, path: &str) -> Result<clip::Note, Invalid> {
        let Self {
            id,
            start,
            length,
            key,
            velocity,
            release_velocity,
            expression,
        } = self;
        let start = position(*start, || format!("{path}.start"))?;
        let length = length_from(start, *length, || format!("{path}.length"))?;
        if *key > clip::Note::MAX_KEY {
            let message = format!("must be from 0 to {}", clip::Note::MAX_KEY);
            return Err(Invalid::new(format!("{path}.key"), message));
        }
        let unit = |value: f32, name: &str| {
            if (0.0..=1.0).contains(&value) {
                Ok(value)
            } else {
                Err(Invalid::new(
                    format!("{path}.{name}"),
                    "must be from 0 to 1",
                ))
            }
        };
        let Expression {
            pitch_bend,
            pressure,
            timbre,
        } = expression;
        let path = format!("{path}.expression");
        Ok(clip::Note {
            id: NoteId(*id),
            start,
            length,
            key: *key,
            velocity: unit(*velocity, "velocity")?,
            release_velocity: unit(*release_velocity, "release_velocity")?,
            expression: clip::Expression {
                pitch_bend: curve(
                    pitch_bend,
                    Dimension::PitchBend,
                    &format!("{path}.pitch_bend"),
                )?,
                pressure: curve(pressure, Dimension::Pressure, &format!("{path}.pressure"))?,
                timbre: curve(timbre, Dimension::Timbre, &format!("{path}.timbre"))?,
            },
        })
    }
}

/// An expression curve: points in order, from the note's start on, with
/// values `dimension` takes.
fn curve(
    points: &[(i64, f32)],
    dimension: Dimension,
    path: &str,
) -> Result<Vec<clip::Point>, Invalid> {
    let (low, high) = dimension.range();
    let mut curve = Vec::with_capacity(points.len());
    for (k, &(at, value)) in points.iter().enumerate() {
        let path = format!("{path}[{k}]");
        if at < 0 {
            let message = "must be 0 or more: points start from the note's start";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        if k > 0 && at < points[k - 1].0 {
            let message = "must be at or after the point before it";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        let at = position(at, || format!("{path}[0]"))?;
        if !(low..=high).contains(&value) {
            let message = format!("must be from {low} to {high}");
            return Err(Invalid::new(format!("{path}[1]"), message));
        }
        curve.push(clip::Point::new(at, value));
    }
    Ok(curve)
}

/// A length in ticks, if it's more than zero and what has it, starting at
/// `start`, ends within [`MAX_TICKS`] of the song's start.
fn length_from(start: Beats, ticks: i64, path: impl FnOnce() -> String) -> Result<Beats, Invalid> {
    if ticks <= 0 {
        return Err(Invalid::new(path(), "must be more than 0"));
    }
    match start.ticks().checked_add(ticks) {
        Some(end) if end <= MAX_TICKS => Ok(Beats::from_ticks(ticks)),
        _ => {
            let message = format!("must end within {MAX_TICKS} ticks of the start of the song");
            Err(Invalid::new(path(), message))
        }
    }
}

fn no_track(id: u32) -> String {
    format!("there's no track with id {id}")
}

impl From<&model::Send> for Send {
    fn from(send: &model::Send) -> Self {
        let model::Send {
            id,
            to,
            level,
            pre_fader,
        } = *send;
        Self {
            id: id.0,
            to: to.0,
            level,
            pre_fader,
        }
    }
}

impl Send {
    fn to_model(&self, path: &str) -> Result<model::Send, Invalid> {
        let Self {
            id,
            to,
            level,
            pre_fader,
        } = *self;
        Ok(model::Send {
            id: SendId(id),
            to: TrackId(to),
            level: finite(level, || format!("{path}.level"))?,
            pre_fader,
        })
    }
}

impl From<&model::Device> for Device {
    fn from(device: &model::Device) -> Self {
        let model::Device { id, kind } = device;
        Self {
            id: id.0,
            kind: match *kind {
                model::DeviceKind::TestGain { gain } => DeviceKind::TestGain { gain },
                model::DeviceKind::TestInstrument { level } => DeviceKind::TestInstrument { level },
            },
        }
    }
}

/// A device list, `path` being where it is in the file.
fn devices_to_model(devices: &[Device], path: &str) -> Result<Vec<model::Device>, Invalid> {
    let device = |(j, device): (usize, &Device)| {
        let Device { id, kind } = device;
        let path = format!("{path}[{j}].kind");
        Ok(model::Device {
            id: DeviceId(*id),
            kind: match *kind {
                DeviceKind::TestGain { gain } => model::DeviceKind::TestGain {
                    gain: finite(gain, || format!("{path}.test_gain.gain"))?,
                },
                DeviceKind::TestInstrument { level } => model::DeviceKind::TestInstrument {
                    level: finite(level, || format!("{path}.test_instrument.level"))?,
                },
            },
        })
    };
    devices.iter().enumerate().map(device).collect()
}

impl From<model::ClipType> for ClipType {
    fn from(clip_type: model::ClipType) -> Self {
        match clip_type {
            model::ClipType::Off => Self::Off,
            model::ClipType::Hard => Self::Hard,
            model::ClipType::Soft => Self::Soft,
        }
    }
}

impl From<ClipType> for model::ClipType {
    fn from(clip_type: ClipType) -> Self {
        match clip_type {
            ClipType::Off => Self::Off,
            ClipType::Hard => Self::Hard,
            ClipType::Soft => Self::Soft,
        }
    }
}

impl From<&model::Master> for Master {
    fn from(master: &model::Master) -> Self {
        let model::Master {
            gain,
            pan,
            devices,
            clip_type,
            console,
        } = master;
        let model::ConsoleSettings {} = console;
        Self {
            gain: *gain,
            pan: *pan,
            devices: devices.iter().map(Device::from).collect(),
            clip_type: ClipType::from(*clip_type),
            console: ConsoleSettings {},
        }
    }
}

impl Master {
    fn to_model(&self) -> Result<model::Master, Invalid> {
        let Self {
            gain,
            pan,
            devices,
            clip_type,
            console,
        } = self;
        let ConsoleSettings {} = console;
        Ok(model::Master {
            gain: finite(*gain, || "master.gain".to_owned())?,
            pan: balance(*pan, || "master.pan".to_owned())?,
            devices: devices_to_model(devices, "master.devices")?,
            clip_type: model::ClipType::from(*clip_type),
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

/// A balance setting, if it's from -1 to 1.
fn balance(pan: f32, path: impl FnOnce() -> String) -> Result<f32, Invalid> {
    if (-1.0..=1.0).contains(&pan) {
        Ok(pan)
    } else {
        Err(Invalid::new(path(), "must be from -1 to 1"))
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

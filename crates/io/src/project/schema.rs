//! The current version of the format, as serde types.
//!
//! These types are the format: they read and write exactly the files that
//! `docs/project-format.md` describes. Changing them changes the format, so
//! it takes a new [`VERSION`] and a migration step (see `migrate.rs`).
//!
//! The model isn't serialized directly, so it can change shape without the
//! files changing. The conversions name every field of the model types they
//! can, so a new field doesn't compile until the format handles it.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use musicable_model::automation;
use musicable_model::clip::{self, ClipId, Dimension, NoteId};
use musicable_model::mixer::Bus;
use musicable_model::param::{self as param, ParamAddress};
use musicable_model::time::{self, Beats};
use musicable_model::warp;
use musicable_model::{self as model, Chain, DeviceId, LocatorId, SendId, TakeId, TrackId};
use serde::de::{self, Deserializer, MapAccess};
use serde::ser::{SerializeMap, Serializer};
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
    punch_in: bool,
    punch_out: bool,
    next_track_id: u32,
    next_send_id: u32,
    next_device_id: u32,
    next_clip_id: u32,
    next_locator_id: u32,
    next_take_id: u32,
    tracks: Vec<Track>,
    master: Master,
    locators: Vec<Locator>,
    automation: Vec<Lane>,
    /// The workspace the project was saved with.
    #[serde(deserialize_with = "Option::deserialize")]
    workspace: Option<String>,
}

/// A named place in the song.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    id: u32,
    name: String,
    /// In ticks.
    at: i64,
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
    #[serde(deserialize_with = "Option::deserialize")]
    input: Option<Input>,
    monitor: Monitor,
    /// The group track the output feeds, or `None` for the master.
    // Required like every other field: serde would read a missing `Option`
    // as `None`, but not through `deserialize_with`.
    #[serde(deserialize_with = "Option::deserialize")]
    output: Option<u32>,
    sends: Vec<Send>,
    #[serde(deserialize_with = "Option::deserialize")]
    tone: Option<TestTone>,
    clips: Vec<Clip>,
    takes: Vec<Take>,
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

/// Externally tagged, `{"audio": {..}}`, like [`ClipContent`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Audio(AudioInput),
    Midi(MidiInput),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioInput {
    device: String,
    name: String,
    channel: u16,
    stereo: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MidiInput {
    device: String,
    name: String,
    /// From 0 to 15, or `None` for every channel.
    #[serde(deserialize_with = "Option::deserialize")]
    channel: Option<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Monitor {
    In,
    Auto,
    Off,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Take {
    id: u32,
    name: String,
    clips: Vec<Clip>,
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
    /// In frames of the file while the clip is unwarped, and in ticks of
    /// its own time while it's warped.
    offset: i64,
    gain: f32,
    fade_in: i64,
    fade_out: i64,
    #[serde(deserialize_with = "Option::deserialize")]
    warp: Option<Warp>,
    /// In order of parameter, at most one for each.
    automation: Vec<ClipLane>,
}

/// How a warped clip follows the tempo: its markers as `[frame, at]`
/// pairs, `at` in ticks of the clip's own time, and its transients as
/// `[frame, strength, percussiveness, origin]`, a line each, as a file can
/// have thousands.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Warp {
    mode: WarpMode,
    tempo: f64,
    transpose: i8,
    detune: i8,
    slice: SliceSettings,
    spectral: SpectralSettings,
    markers: Vec<(u64, i64)>,
    #[serde(deserialize_with = "Option::deserialize")]
    onsets: Option<Vec<(u64, f32, f32, Origin)>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WarpMode {
    RePitch,
    Slice,
    Spectral,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SliceSettings {
    preserve: Preserve,
    transient_loop: TransientLoop,
    envelope: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpectralSettings {
    high_quality: bool,
    formants: f64,
    envelope: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Preserve {
    Transients,
    Bar,
    #[serde(rename = "1/2")]
    Half,
    #[serde(rename = "1/4")]
    Quarter,
    #[serde(rename = "1/8")]
    Eighth,
    #[serde(rename = "1/16")]
    Sixteenth,
    #[serde(rename = "1/32")]
    ThirtySecond,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransientLoop {
    Off,
    Forward,
    BackAndForth,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Origin {
    Found,
    Added,
    Hidden,
}

/// The curve of one of an audio clip's own parameters: its points as `[at,
/// value]` pairs, `at` in ticks of the clip's own time.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipLane {
    param: ClipParam,
    points: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClipParam {
    Gain,
    Transpose,
    Detune,
    TransientEnvelope,
    Preserve,
    TransientLoop,
    Formants,
    Envelope,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MidiClip {
    offset: i64,
    next_note_id: u32,
    notes: Vec<Note>,
    /// In order of controller number.
    controllers: Vec<ControllerCurve>,
}

/// How a clip moves a controller: its points as `[at, value]` pairs, `at`
/// in ticks of the clip's own time.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllerCurve {
    controller: u8,
    points: Vec<(i64, f32)>,
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
    Plugin(PluginDevice),
    Native(NativeDevice),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginDevice {
    format: PluginFormat,
    id: String,
    name: String,
    instrument: bool,
    /// Base64, or `None` for a plugin as new.
    #[serde(deserialize_with = "Option::deserialize")]
    state: Option<String>,
    /// Parameter ids and values, in order of id.
    params: Vec<(u32, f64)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PluginFormat {
    Clap,
    Vst3,
}

/// One of Musicable's own devices.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeDevice {
    kind: NativeKind,
    /// A sampler's sample file, or `None`.
    #[serde(deserialize_with = "Option::deserialize")]
    sample: Option<String>,
    params: NativeParams,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NativeKind {
    Eq,
    Compressor,
    Gate,
    Delay,
    Reverb,
    Utility,
    Sampler,
    Synth,
}

/// A native device's parameter values, as an object of each parameter's
/// key and value, written in the order of the kind's table. Read in the
/// file's order, so [`NativeDevice::to_model`] can say which key is wrong.
#[derive(Debug)]
struct NativeParams(Vec<(String, f64)>);

impl Serialize for NativeParams {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for NativeParams {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = NativeParams;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of parameter values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<NativeParams, A::Error> {
                let mut params = Vec::new();
                while let Some(entry) = map.next_entry::<String, f64>()? {
                    params.push(entry);
                }
                Ok(NativeParams(params))
            }
        }

        deserializer.deserialize_map(Visitor)
    }
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

/// A parameter's automation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lane {
    param: Param,
    /// `[at, value]` pairs, `at` in ticks, in order.
    points: Vec<(i64, f64)>,
    overridden: bool,
}

/// Externally tagged, `{"volume": "master"}` or `{"send": 3}`, for the
/// reason [`DeviceKind`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Param {
    Volume(Channel),
    Pan(Channel),
    /// A send's id.
    Send(u32),
    /// A device's id, and the device's number for the parameter.
    Device {
        id: u32,
        param: u32,
    },
}

/// A track by id, `{"track": 3}`, or `"master"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Channel {
    Track(u32),
    Master,
}

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
            punch_in,
            punch_out,
            master,
            tracks,
            locators,
            automation,
            next_track_id,
            next_send_id,
            next_device_id,
            next_clip_id,
            next_locator_id,
            next_take_id,
            workspace,
        } = project;
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            ticks_per_beat: Beats::TICKS_PER_BEAT,
            tempo: TempoMap::from(tempo),
            loop_region: BeatRange::from(*loop_region),
            looping: *looping,
            metronome: *metronome,
            punch_in: *punch_in,
            punch_out: *punch_out,
            next_track_id: *next_track_id,
            next_send_id: *next_send_id,
            next_device_id: *next_device_id,
            next_clip_id: *next_clip_id,
            next_locator_id: *next_locator_id,
            next_take_id: *next_take_id,
            tracks: tracks.iter().map(Track::from).collect(),
            master: Master::from(master),
            locators: locators.iter().map(Locator::from).collect(),
            // A clip's own parameters' curves are in the clip.
            automation: (automation.iter())
                .filter(|lane| !matches!(lane.param, ParamAddress::Clip(..)))
                .map(Lane::from)
                .collect(),
            workspace: workspace.clone(),
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
            punch_in,
            punch_out,
            next_track_id,
            next_send_id,
            next_device_id,
            next_clip_id,
            next_locator_id,
            next_take_id,
            tracks,
            master,
            locators,
            automation,
            workspace,
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
        let mut take_ids = Ids::new("take", "next_take_id", *next_take_id);
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
            for (j, take) in track.takes.iter().enumerate() {
                let path = format!("{path}.takes[{j}]");
                take_ids.add(take.id, &path)?;
                for (k, clip) in take.clips.iter().enumerate() {
                    clip_ids.add(clip.id, &format!("{path}.clips[{k}]"))?;
                }
            }
            for (j, device) in track.devices.iter().enumerate() {
                device_ids.add(device.id, &format!("{path}.devices[{j}]"))?;
            }
            model_tracks.push(track.to_model(&path)?);
        }
        ids.check_next()?;
        send_ids.check_next()?;
        clip_ids.check_next()?;
        take_ids.check_next()?;
        let kinds: HashMap<u32, TrackKind> = tracks.iter().map(|t| (t.id, t.kind)).collect();
        for (i, track) in tracks.iter().enumerate() {
            track.check_routing(&format!("tracks[{i}]"), &kinds)?;
        }
        for (j, device) in master.devices.iter().enumerate() {
            device_ids.add(device.id, &format!("master.devices[{j}]"))?;
        }
        device_ids.check_next()?;
        let mut locator_ids = Ids::new("locator", "next_locator_id", *next_locator_id);
        let mut model_locators = Vec::with_capacity(locators.len());
        for (i, locator) in locators.iter().enumerate() {
            let path = format!("locators[{i}]");
            locator_ids.add(locator.id, &path)?;
            model_locators.push(locator.to_model(&path)?);
        }
        locator_ids.check_next()?;
        let params = Params {
            tracks: &kinds,
            sends: &send_ids,
            devices: &device_ids,
        };
        let mut automated: HashMap<Param, usize> = HashMap::new();
        let mut model_lanes = Vec::with_capacity(automation.len());
        for (i, lane) in automation.iter().enumerate() {
            let path = format!("automation[{i}]");
            if let Some(first) = automated.insert(lane.param, i) {
                let message = format!("automation[{first}] automates it too");
                return Err(Invalid::new(format!("{path}.param"), message));
            }
            model_lanes.push(lane.to_model(&path, &params)?);
        }
        let project = model::Project {
            tempo,
            loop_region,
            looping: *looping,
            metronome: *metronome,
            punch_in: *punch_in,
            punch_out: *punch_out,
            master: master.to_model()?,
            tracks: model_tracks,
            locators: model_locators,
            automation: model_lanes,
            next_track_id: *next_track_id,
            next_send_id: *next_send_id,
            next_device_id: *next_device_id,
            next_clip_id: *next_clip_id,
            next_locator_id: *next_locator_id,
            next_take_id: *next_take_id,
            workspace: workspace.clone(),
        };
        check_loops(&project)?;
        // Which parameters a device has depends on its kind, and a built-in
        // device's take the values its own do.
        for (i, lane) in project.automation.iter().enumerate() {
            if let ParamAddress::Device(device, n) = lane.param
                && !project.has_param(lane.param)
            {
                let native = (project.find_device(device)).and_then(|(_, _, d)| d.kind.native());
                let has = match native {
                    Some(native) => format!("parameters 0 to {}", native.params().len() - 1),
                    None => "only parameter 0".to_owned(),
                };
                let message = format!(
                    "names parameter {n} of device {}, which has {has}",
                    device.0
                );
                return Err(Invalid::new(format!("automation[{i}].param"), message));
            }
            let Some(info) = project.native_param(lane.param) else {
                continue;
            };
            for (j, point) in lane.points.iter().enumerate() {
                if !info.is_valid(point.value) {
                    let number = if info.stepped {
                        "a whole number"
                    } else {
                        "a number"
                    };
                    let message = format!("must be {number} from {} to {}", info.min, info.max);
                    let path = format!("automation[{i}].points[{j}][1]");
                    return Err(Invalid::new(path, message));
                }
            }
        }
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
            input,
            monitor,
            output,
            sends,
            tone,
            clips,
            takes,
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
            input: input.as_ref().map(Input::from),
            monitor: match monitor {
                model::Monitor::In => Monitor::In,
                model::Monitor::Auto => Monitor::Auto,
                model::Monitor::Off => Monitor::Off,
            },
            output: match output {
                Bus::Master => None,
                Bus::Group(group) => Some(group.0),
            },
            sends: sends.iter().map(Send::from).collect(),
            tone: tone.map(|model::TestTone { frequency, level }| TestTone { frequency, level }),
            clips: clips.iter().map(Clip::from).collect(),
            takes: takes.iter().map(Take::from).collect(),
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
            input,
            monitor,
            output,
            sends,
            tone,
            clips,
            takes,
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
        if kind.is_bus() && !takes.is_empty() {
            let message = "must be empty: only audio and MIDI tracks record";
            return Err(Invalid::new(format!("{path}.takes"), message));
        }
        let takes = takes
            .iter()
            .enumerate()
            .map(|(j, take)| take.to_model(&format!("{path}.takes[{j}]"), kind))
            .collect::<Result<_, _>>()?;
        let input = match input {
            Some(input) => Some(input.to_model(&format!("{path}.input"), kind)?),
            None => None,
        };
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
            input,
            monitor: match monitor {
                Monitor::In => model::Monitor::In,
                Monitor::Auto => model::Monitor::Auto,
                Monitor::Off => model::Monitor::Off,
            },
            output: match output {
                None => Bus::Master,
                Some(group) => Bus::Group(TrackId(*group)),
            },
            sends,
            tone,
            clips,
            takes,
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

impl From<&model::Input> for Input {
    fn from(input: &model::Input) -> Self {
        match input {
            model::Input::Audio(model::AudioInput {
                device,
                name,
                channel,
                stereo,
            }) => Input::Audio(AudioInput {
                device: device.clone(),
                name: name.clone(),
                channel: *channel,
                stereo: *stereo,
            }),
            model::Input::Midi(model::MidiInput {
                device,
                name,
                channel,
            }) => Input::Midi(MidiInput {
                device: device.clone(),
                name: name.clone(),
                channel: *channel,
            }),
        }
    }
}

impl Input {
    /// The input, for a track of `kind`.
    fn to_model(&self, path: &str, kind: model::TrackKind) -> Result<model::Input, Invalid> {
        match (self, kind) {
            (_, model::TrackKind::Return | model::TrackKind::Group) => {
                Err(Invalid::new(path, "must be null: buses record nothing"))
            }
            (Self::Audio(audio), model::TrackKind::Audio) => {
                let AudioInput {
                    device,
                    name,
                    channel,
                    stereo,
                } = audio;
                if *stereo && *channel == u16::MAX {
                    let message = "must be below 65535 for a stereo input, which takes the \
                                   channel after it too";
                    return Err(Invalid::new(format!("{path}.audio.channel"), message));
                }
                Ok(model::Input::Audio(model::AudioInput {
                    device: device.clone(),
                    name: name.clone(),
                    channel: *channel,
                    stereo: *stereo,
                }))
            }
            (Self::Midi(midi), model::TrackKind::Midi) => {
                let MidiInput {
                    device,
                    name,
                    channel,
                } = midi;
                if channel.is_some_and(|channel| channel > 15) {
                    let message = "must be from 0 to 15, or null for every channel";
                    return Err(Invalid::new(format!("{path}.midi.channel"), message));
                }
                Ok(model::Input::Midi(model::MidiInput {
                    device: device.clone(),
                    name: name.clone(),
                    channel: *channel,
                }))
            }
            (Self::Audio(_), _) => Err(Invalid::new(
                path,
                "must be a MIDI input: MIDI tracks record MIDI",
            )),
            (Self::Midi(_), _) => Err(Invalid::new(
                path,
                "must be an audio input: audio tracks record audio",
            )),
        }
    }
}

impl From<&model::Take> for Take {
    fn from(take: &model::Take) -> Self {
        let model::Take { id, name, clips } = take;
        Self {
            id: id.0,
            name: name.clone(),
            clips: clips.iter().map(Clip::from).collect(),
        }
    }
}

impl Take {
    /// The take, under a track of `kind`.
    fn to_model(&self, path: &str, kind: model::TrackKind) -> Result<model::Take, Invalid> {
        let Self { id, name, clips } = self;
        let clips = clips
            .iter()
            .enumerate()
            .map(|(k, clip)| clip.to_model(&format!("{path}.clips[{k}]"), kind))
            .collect::<Result<_, _>>()?;
        Ok(model::Take {
            id: TakeId(*id),
            name: name.clone(),
            clips,
        })
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
            warp,
            automation,
        } = audio;
        Self {
            file: file.clone(),
            offset: match *offset {
                // Beyond what an i64 holds, it's refused on the way back.
                clip::Offset::Frames(frames) => i64::try_from(frames).unwrap_or(i64::MAX),
                clip::Offset::Beats(beats) => beats.ticks(),
            },
            gain: *gain,
            fade_in: fade_in.ticks(),
            fade_out: fade_out.ticks(),
            warp: warp.as_ref().map(Warp::from),
            automation: automation.iter().map(ClipLane::from).collect(),
        }
    }
}

impl From<&warp::Warp> for Warp {
    fn from(warp: &warp::Warp) -> Self {
        let warp::Warp {
            mode,
            markers,
            tempo,
            onsets,
            transpose,
            detune,
            slice,
            spectral,
        } = warp;
        let origin = |origin| match origin {
            warp::Origin::Found => Origin::Found,
            warp::Origin::Added => Origin::Added,
            warp::Origin::Hidden => Origin::Hidden,
        };
        let warp::SliceSettings {
            preserve,
            transient_loop,
            envelope,
        } = slice;
        let warp::SpectralSettings {
            high_quality,
            formants,
            envelope: spectral_envelope,
        } = spectral;
        Self {
            mode: match mode {
                warp::WarpMode::RePitch => WarpMode::RePitch,
                warp::WarpMode::Slice => WarpMode::Slice,
                warp::WarpMode::Spectral => WarpMode::Spectral,
            },
            tempo: *tempo,
            transpose: *transpose,
            detune: *detune,
            slice: SliceSettings {
                preserve: match preserve {
                    warp::Preserve::Transients => Preserve::Transients,
                    warp::Preserve::Bar => Preserve::Bar,
                    warp::Preserve::Half => Preserve::Half,
                    warp::Preserve::Quarter => Preserve::Quarter,
                    warp::Preserve::Eighth => Preserve::Eighth,
                    warp::Preserve::Sixteenth => Preserve::Sixteenth,
                    warp::Preserve::ThirtySecond => Preserve::ThirtySecond,
                },
                transient_loop: match transient_loop {
                    warp::TransientLoop::Off => TransientLoop::Off,
                    warp::TransientLoop::Forward => TransientLoop::Forward,
                    warp::TransientLoop::BackAndForth => TransientLoop::BackAndForth,
                },
                envelope: *envelope,
            },
            spectral: SpectralSettings {
                high_quality: *high_quality,
                formants: *formants,
                envelope: *spectral_envelope,
            },
            markers: markers.iter().map(|m| (m.frame, m.beat.ticks())).collect(),
            onsets: onsets.as_ref().map(|onsets| {
                (onsets.all().iter())
                    .map(|o| (o.frame, o.strength, o.percussiveness, origin(o.origin)))
                    .collect()
            }),
        }
    }
}

impl From<&clip::ClipLane> for ClipLane {
    fn from(lane: &clip::ClipLane) -> Self {
        let clip::ClipLane { param, points } = lane;
        Self {
            param: match param {
                param::ClipParam::Gain => ClipParam::Gain,
                param::ClipParam::Transpose => ClipParam::Transpose,
                param::ClipParam::Detune => ClipParam::Detune,
                param::ClipParam::TransientEnvelope => ClipParam::TransientEnvelope,
                param::ClipParam::Preserve => ClipParam::Preserve,
                param::ClipParam::TransientLoop => ClipParam::TransientLoop,
                param::ClipParam::Formants => ClipParam::Formants,
                param::ClipParam::Envelope => ClipParam::Envelope,
            },
            points: points.iter().map(|p| (p.at.ticks(), p.value)).collect(),
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
            warp,
            automation,
        } = self;
        if file.is_empty() {
            return Err(Invalid::new(format!("{path}.file"), "must name a file"));
        }
        let offset_path = || format!("{path}.offset");
        let offset = match warp {
            Some(_) => clip::Offset::Beats(position(*offset, offset_path)?),
            None if *offset < 0 => {
                let message = "must be 0 or more: a frame of the file, while the clip is unwarped";
                return Err(Invalid::new(offset_path(), message));
            }
            None if *offset > MAX_TICKS => {
                let message = format!("must be {MAX_TICKS} or less");
                return Err(Invalid::new(offset_path(), message));
            }
            None => clip::Offset::Frames(offset.unsigned_abs()),
        };
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
        let warp = match warp {
            Some(warp) => Some(warp.to_model(&format!("{path}.warp"))?),
            None => None,
        };
        let mut lanes: Vec<clip::ClipLane> = Vec::with_capacity(automation.len());
        for (k, lane) in automation.iter().enumerate() {
            let path = format!("{path}.automation[{k}]");
            let lane = lane.to_model(&path)?;
            if lanes.iter().any(|l| l.param == lane.param) {
                let message = format!("the clip moves {} twice", lane.param.key());
                return Err(Invalid::new(format!("{path}.param"), message));
            }
            lanes.push(lane);
        }
        lanes.sort_by_key(|l| l.param);
        Ok(clip::AudioClip {
            file: file.clone(),
            offset,
            gain: finite(*gain, || format!("{path}.gain"))?,
            fade_in: Beats::from_ticks(*fade_in),
            fade_out: Beats::from_ticks(*fade_out),
            warp,
            automation: lanes,
        })
    }
}

impl Warp {
    fn to_model(&self, path: &str) -> Result<warp::Warp, Invalid> {
        let Self {
            mode,
            tempo,
            transpose,
            detune,
            slice,
            spectral,
            markers,
            onsets,
        } = self;
        let (low, high) = (warp::Warp::MIN_TEMPO, warp::Warp::MAX_TEMPO);
        if !(low..=high).contains(tempo) {
            let message = format!("must be from {low} to {high}");
            return Err(Invalid::new(format!("{path}.tempo"), message));
        }
        for (value, max, name) in [
            (*transpose, warp::Warp::TRANSPOSE_MAX, "transpose"),
            (*detune, warp::Warp::DETUNE_MAX, "detune"),
        ] {
            if !(-max..=max).contains(&value) {
                let message = format!("must be from -{max} to {max}");
                return Err(Invalid::new(format!("{path}.{name}"), message));
            }
        }
        let SliceSettings {
            preserve,
            transient_loop,
            envelope,
        } = slice;
        let most = warp::SliceSettings::ENVELOPE_MAX;
        if !(0.0..=most).contains(envelope) {
            let message = format!("must be from 0 to {most}");
            return Err(Invalid::new(format!("{path}.slice.envelope"), message));
        }
        let slice = warp::SliceSettings {
            preserve: match preserve {
                Preserve::Transients => warp::Preserve::Transients,
                Preserve::Bar => warp::Preserve::Bar,
                Preserve::Half => warp::Preserve::Half,
                Preserve::Quarter => warp::Preserve::Quarter,
                Preserve::Eighth => warp::Preserve::Eighth,
                Preserve::Sixteenth => warp::Preserve::Sixteenth,
                Preserve::ThirtySecond => warp::Preserve::ThirtySecond,
            },
            transient_loop: match transient_loop {
                TransientLoop::Off => warp::TransientLoop::Off,
                TransientLoop::Forward => warp::TransientLoop::Forward,
                TransientLoop::BackAndForth => warp::TransientLoop::BackAndForth,
            },
            envelope: *envelope,
        };
        let SpectralSettings {
            high_quality,
            formants,
            envelope: spectral_envelope,
        } = spectral;
        let most = warp::SpectralSettings::FORMANTS_MAX;
        if !(0.0..=most).contains(formants) {
            let message = format!("must be from 0 to {most}");
            return Err(Invalid::new(format!("{path}.spectral.formants"), message));
        }
        let (least, most) = (
            warp::SpectralSettings::ENVELOPE_MIN,
            warp::SpectralSettings::ENVELOPE_MAX,
        );
        if !(least..=most).contains(spectral_envelope) {
            let message = format!("must be from {least} to {most}");
            return Err(Invalid::new(format!("{path}.spectral.envelope"), message));
        }
        let spectral = warp::SpectralSettings {
            high_quality: *high_quality,
            formants: *formants,
            envelope: *spectral_envelope,
        };
        if markers.is_empty() {
            let message = "needs at least one marker: a warped clip has one";
            return Err(Invalid::new(format!("{path}.markers"), message));
        }
        let mut model_markers: Vec<warp::WarpMarker> = Vec::with_capacity(markers.len());
        for (k, &(frame, at)) in markers.iter().enumerate() {
            let path = format!("{path}.markers[{k}]");
            if frame > MAX_TICKS.unsigned_abs() {
                let message = format!("must be {MAX_TICKS} or less");
                return Err(Invalid::new(format!("{path}[0]"), message));
            }
            let beat = position(at, || format!("{path}[1]"))?;
            if let Some(before) = model_markers.last() {
                if frame <= before.frame {
                    let message = "must be after the frame of the marker before it";
                    return Err(Invalid::new(format!("{path}[0]"), message));
                }
                if beat <= before.beat {
                    let message = "must be after the position of the marker before it";
                    return Err(Invalid::new(format!("{path}[1]"), message));
                }
            }
            model_markers.push(warp::WarpMarker { frame, beat });
        }
        let onsets = match onsets {
            Some(onsets) => Some(onsets_to_model(onsets, &format!("{path}.onsets"))?),
            None => None,
        };
        Ok(warp::Warp {
            mode: match mode {
                WarpMode::RePitch => warp::WarpMode::RePitch,
                WarpMode::Slice => warp::WarpMode::Slice,
                WarpMode::Spectral => warp::WarpMode::Spectral,
            },
            markers: model_markers,
            tempo: *tempo,
            onsets,
            transpose: *transpose,
            detune: *detune,
            slice,
            spectral,
        })
    }
}

/// A warp's transients, the list at `path`: frames of the file rising, and
/// strengths and percussiveness from 0 to 1.
fn onsets_to_model(
    onsets: &[(u64, f32, f32, Origin)],
    path: &str,
) -> Result<warp::Onsets, Invalid> {
    let mut model: Vec<warp::Onset> = Vec::with_capacity(onsets.len());
    for (k, &(frame, strength, percussiveness, origin)) in onsets.iter().enumerate() {
        let path = format!("{path}[{k}]");
        if frame > MAX_TICKS.unsigned_abs() {
            let message = format!("must be {MAX_TICKS} or less");
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        if model.last().is_some_and(|before| frame <= before.frame) {
            let message = "must be after the frame of the transient before it";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        for (value, index) in [(strength, 1), (percussiveness, 2)] {
            if !(0.0..=1.0).contains(&value) {
                return Err(Invalid::new(
                    format!("{path}[{index}]"),
                    "must be from 0 to 1",
                ));
            }
        }
        model.push(warp::Onset {
            frame,
            strength,
            percussiveness,
            origin: match origin {
                Origin::Found => warp::Origin::Found,
                Origin::Added => warp::Origin::Added,
                Origin::Hidden => warp::Origin::Hidden,
            },
        });
    }
    Ok(warp::Onsets::new(model))
}

impl ClipLane {
    fn to_model(&self, path: &str) -> Result<clip::ClipLane, Invalid> {
        let Self { param, points } = self;
        let param = match param {
            ClipParam::Gain => param::ClipParam::Gain,
            ClipParam::Transpose => param::ClipParam::Transpose,
            ClipParam::Detune => param::ClipParam::Detune,
            ClipParam::TransientEnvelope => param::ClipParam::TransientEnvelope,
            ClipParam::Preserve => param::ClipParam::Preserve,
            ClipParam::TransientLoop => param::ClipParam::TransientLoop,
            ClipParam::Formants => param::ClipParam::Formants,
            ClipParam::Envelope => param::ClipParam::Envelope,
        };
        let spec = param.spec();
        let (low, high) = (spec.min, spec.max);
        let stepped = param.stepped();
        let range = |value: f64| match () {
            _ if !(low..=high).contains(&value) => Some(format!("must be from {low} to {high}")),
            _ if stepped && value.fract() != 0.0 => Some("must be a whole number".to_owned()),
            _ => None,
        };
        let points = lane_points(points, path, false, range)?;
        Ok(clip::ClipLane { param, points })
    }
}

impl From<&clip::MidiClip> for MidiClip {
    fn from(midi: &clip::MidiClip) -> Self {
        let clip::MidiClip {
            offset,
            next_note_id,
            notes,
            controllers,
        } = midi;
        Self {
            offset: offset.ticks(),
            next_note_id: *next_note_id,
            notes: notes.iter().map(Note::from).collect(),
            controllers: controllers
                .iter()
                .map(|(controller, points)| ControllerCurve {
                    controller: controller.number(),
                    points: points.iter().map(|p| (p.at.ticks(), p.value)).collect(),
                })
                .collect(),
        }
    }
}

impl MidiClip {
    fn to_model(&self, path: &str) -> Result<clip::MidiClip, Invalid> {
        let Self {
            offset,
            next_note_id,
            notes,
            controllers,
        } = self;
        let mut ids = Ids::new("note", format!("{path}.next_note_id"), *next_note_id);
        let mut model_notes = Vec::with_capacity(notes.len());
        for (k, note) in notes.iter().enumerate() {
            let path = format!("{path}.notes[{k}]");
            ids.add(note.id, &path)?;
            model_notes.push(note.to_model(&path)?);
        }
        ids.check_next()?;
        let mut curves = BTreeMap::new();
        for (k, curve) in controllers.iter().enumerate() {
            let path = format!("{path}.controllers[{k}]");
            let (controller, points) = curve.to_model(&path)?;
            if curves.insert(controller, points).is_some() {
                let message = format!("the clip moves controller {} twice", curve.controller);
                return Err(Invalid::new(format!("{path}.controller"), message));
            }
        }
        Ok(clip::MidiClip {
            offset: position(*offset, || format!("{path}.offset"))?,
            next_note_id: *next_note_id,
            notes: model_notes,
            controllers: curves,
        })
    }
}

impl ControllerCurve {
    fn to_model(&self, path: &str) -> Result<(clip::Controller, Vec<clip::Point>), Invalid> {
        let Self { controller, points } = self;
        let Some(controller) = clip::Controller::new(*controller) else {
            let message = format!("must be from 0 to {}", clip::Controller::MAX);
            return Err(Invalid::new(format!("{path}.controller"), message));
        };
        let path = format!("{path}.points");
        if points.is_empty() {
            let message = "must have a point: a clip that doesn't move a controller leaves it out";
            return Err(Invalid::new(path, message));
        }
        let points = curve(points, (0.0, 1.0), &path)?;
        Ok((controller, points))
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
                pitch_bend: expression_curve(
                    pitch_bend,
                    Dimension::PitchBend,
                    &path,
                    "pitch_bend",
                )?,
                pressure: expression_curve(pressure, Dimension::Pressure, &path, "pressure")?,
                timbre: expression_curve(timbre, Dimension::Timbre, &path, "timbre")?,
            },
        })
    }
}

/// An expression curve, `name` in the expression at `path`: points from
/// the note's start on, with values `dimension` takes.
fn expression_curve(
    points: &[(i64, f32)],
    dimension: Dimension,
    path: &str,
    name: &str,
) -> Result<Vec<clip::Point>, Invalid> {
    let path = format!("{path}.{name}");
    if let Some(k) = points.iter().position(|&(at, _)| at < 0) {
        let message = "must be 0 or more: points start from the note's start";
        return Err(Invalid::new(format!("{path}[{k}][0]"), message));
    }
    curve(points, dimension.range(), &path)
}

/// A curve of steps: points in order, with values from `low` to `high`.
fn curve(
    points: &[(i64, f32)],
    (low, high): (f32, f32),
    path: &str,
) -> Result<Vec<clip::Point>, Invalid> {
    let mut curve = Vec::with_capacity(points.len());
    for (k, &(at, value)) in points.iter().enumerate() {
        let path = format!("{path}[{k}]");
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
            kind: match kind {
                model::DeviceKind::TestGain { gain } => DeviceKind::TestGain { gain: *gain },
                model::DeviceKind::TestInstrument { level } => {
                    DeviceKind::TestInstrument { level: *level }
                }
                model::DeviceKind::Plugin(plugin) => DeviceKind::Plugin(PluginDevice::from(plugin)),
                model::DeviceKind::Native(native) => DeviceKind::Native(NativeDevice::from(native)),
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
            kind: match kind {
                DeviceKind::TestGain { gain } => model::DeviceKind::TestGain {
                    gain: finite(*gain, || format!("{path}.test_gain.gain"))?,
                },
                DeviceKind::TestInstrument { level } => model::DeviceKind::TestInstrument {
                    level: finite(*level, || format!("{path}.test_instrument.level"))?,
                },
                DeviceKind::Plugin(plugin) => {
                    model::DeviceKind::Plugin(plugin.to_model(&format!("{path}.plugin"))?)
                }
                DeviceKind::Native(native) => {
                    model::DeviceKind::Native(native.to_model(&format!("{path}.native"))?)
                }
            },
        })
    };
    devices.iter().enumerate().map(device).collect()
}

impl From<&model::native::NativeDevice> for NativeDevice {
    fn from(device: &model::native::NativeDevice) -> Self {
        let model::native::NativeDevice {
            kind,
            values,
            sample,
        } = device;
        let params = kind.params().iter().zip(values);
        Self {
            kind: NativeKind::from(*kind),
            sample: sample.clone(),
            params: NativeParams(params.map(|(p, &v)| (p.key.to_owned(), v)).collect()),
        }
    }
}

impl NativeDevice {
    /// Parameters missing from the file take their defaults, so a kind can
    /// gain parameters without a new version of the format.
    fn to_model(&self, path: &str) -> Result<model::native::NativeDevice, Invalid> {
        let Self {
            kind,
            sample,
            params,
        } = self;
        let kind = model::native::NativeKind::from(*kind);
        let mut device = model::native::NativeDevice::new(kind);
        let table = kind.params();
        let mut given = vec![false; table.len()];
        for (key, value) in &params.0 {
            let path = format!("{path}.params.{key}");
            let Some(i) = table.iter().position(|p| p.key == key) else {
                let message = format!("isn't one of the {}'s parameters", kind.name());
                return Err(Invalid::new(path, message));
            };
            if std::mem::replace(&mut given[i], true) {
                return Err(Invalid::new(path, "is given twice"));
            }
            let p = &table[i];
            if !p.is_valid(*value) {
                let number = if p.stepped {
                    "a whole number"
                } else {
                    "a number"
                };
                let message = format!("must be {number} from {} to {}", p.min, p.max);
                return Err(Invalid::new(path, message));
            }
            device.values[i] = *value;
        }
        device.sample = match sample {
            None => None,
            Some(_) if kind != model::native::NativeKind::Sampler => {
                let message = "must be null: only samplers have samples";
                return Err(Invalid::new(format!("{path}.sample"), message));
            }
            Some(file) if file.is_empty() => {
                return Err(Invalid::new(format!("{path}.sample"), "must name a file"));
            }
            Some(file) => Some(file.clone()),
        };
        Ok(device)
    }
}

impl From<model::native::NativeKind> for NativeKind {
    fn from(kind: model::native::NativeKind) -> Self {
        use model::native::NativeKind as Model;
        match kind {
            Model::Eq => Self::Eq,
            Model::Compressor => Self::Compressor,
            Model::Gate => Self::Gate,
            Model::Delay => Self::Delay,
            Model::Reverb => Self::Reverb,
            Model::Utility => Self::Utility,
            Model::Sampler => Self::Sampler,
            Model::Synth => Self::Synth,
        }
    }
}

impl From<NativeKind> for model::native::NativeKind {
    fn from(kind: NativeKind) -> Self {
        match kind {
            NativeKind::Eq => Self::Eq,
            NativeKind::Compressor => Self::Compressor,
            NativeKind::Gate => Self::Gate,
            NativeKind::Delay => Self::Delay,
            NativeKind::Reverb => Self::Reverb,
            NativeKind::Utility => Self::Utility,
            NativeKind::Sampler => Self::Sampler,
            NativeKind::Synth => Self::Synth,
        }
    }
}

impl From<&model::plugin::PluginDevice> for PluginDevice {
    fn from(plugin: &model::plugin::PluginDevice) -> Self {
        let model::plugin::PluginDevice {
            format,
            id,
            name,
            instrument,
            settings,
        } = plugin;
        let model::plugin::PluginSettings { state, params } = settings;
        Self {
            format: match format {
                model::PluginFormat::Clap => PluginFormat::Clap,
                model::PluginFormat::Vst3 => PluginFormat::Vst3,
            },
            id: id.clone(),
            name: name.clone(),
            instrument: *instrument,
            state: state.as_ref().map(|state| BASE64.encode(state.bytes())),
            params: params.iter().map(|p| (p.id, p.value)).collect(),
        }
    }
}

impl PluginDevice {
    fn to_model(&self, path: &str) -> Result<model::plugin::PluginDevice, Invalid> {
        let Self {
            format,
            id,
            name,
            instrument,
            state,
            params,
        } = self;
        if id.is_empty() {
            return Err(Invalid::new(format!("{path}.id"), "must not be empty"));
        }
        let state = match state {
            Some(text) => {
                let bytes = BASE64
                    .decode(text)
                    .map_err(|_| Invalid::new(format!("{path}.state"), "must be base64"))?;
                Some(model::plugin::PluginState::new(bytes))
            }
            None => None,
        };
        let mut checked = Vec::with_capacity(params.len());
        for (k, &(param, value)) in params.iter().enumerate() {
            if k > 0 && param <= params[k - 1].0 {
                let message = "must be after the parameter before it: in order of id, once each";
                return Err(Invalid::new(format!("{path}.params[{k}][0]"), message));
            }
            let value = finite(value, || format!("{path}.params[{k}][1]"))?;
            checked.push(model::plugin::PluginParam { id: param, value });
        }
        Ok(model::plugin::PluginDevice {
            format: match format {
                PluginFormat::Clap => model::PluginFormat::Clap,
                PluginFormat::Vst3 => model::PluginFormat::Vst3,
            },
            id: id.clone(),
            name: name.clone(),
            instrument: *instrument,
            settings: model::plugin::PluginSettings {
                state,
                params: checked,
            },
        })
    }
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

impl From<&model::Locator> for Locator {
    fn from(locator: &model::Locator) -> Self {
        let model::Locator { id, name, at } = locator;
        Self {
            id: id.0,
            name: name.clone(),
            at: at.ticks(),
        }
    }
}

impl Locator {
    fn to_model(&self, path: &str) -> Result<model::Locator, Invalid> {
        let Self { id, name, at } = self;
        Ok(model::Locator {
            id: LocatorId(*id),
            name: name.clone(),
            at: position(*at, || format!("{path}.at"))?,
        })
    }
}

/// What a lane's parameter can belong to: the file's tracks, sends and
/// devices.
struct Params<'a> {
    tracks: &'a HashMap<u32, TrackKind>,
    sends: &'a Ids,
    devices: &'a Ids,
}

impl From<&automation::Lane> for Lane {
    fn from(lane: &automation::Lane) -> Self {
        let automation::Lane {
            param,
            points,
            overridden,
        } = lane;
        let channel = |chain| match chain {
            Chain::Track(track) => Channel::Track(track.0),
            Chain::Master => Channel::Master,
        };
        Self {
            param: match *param {
                ParamAddress::Volume(chain) => Param::Volume(channel(chain)),
                ParamAddress::Pan(chain) => Param::Pan(channel(chain)),
                ParamAddress::Send(send) => Param::Send(send.0),
                ParamAddress::Device(device, param) => Param::Device {
                    id: device.0,
                    param,
                },
                ParamAddress::Clip(..) => unreachable!("a clip's curves are in the clip"),
            },
            points: points.iter().map(|p| (p.at.ticks(), p.value)).collect(),
            overridden: *overridden,
        }
    }
}

impl Lane {
    fn to_model(&self, path: &str, params: &Params<'_>) -> Result<automation::Lane, Invalid> {
        let Self {
            param,
            points,
            overridden,
        } = self;
        let channel = |channel| match channel {
            Channel::Master => Ok(Chain::Master),
            Channel::Track(id) if params.tracks.contains_key(&id) => Ok(Chain::Track(TrackId(id))),
            Channel::Track(id) => {
                let message = format!("names track {id}, which the file hasn't");
                Err(Invalid::new(format!("{path}.param"), message))
            }
        };
        let missing = |what: &str, id: u32| {
            let message = format!("names {what} {id}, which the file hasn't");
            Invalid::new(format!("{path}.param"), message)
        };
        let param = match *param {
            Param::Volume(on) => ParamAddress::Volume(channel(on)?),
            Param::Pan(on) => ParamAddress::Pan(channel(on)?),
            Param::Send(id) if params.sends.seen.contains_key(&id) => {
                ParamAddress::Send(SendId(id))
            }
            Param::Send(id) => return Err(missing("send", id)),
            Param::Device { id, param } if params.devices.seen.contains_key(&id) => {
                ParamAddress::Device(DeviceId(id), param)
            }
            Param::Device { id, .. } => return Err(missing("device", id)),
        };
        let pan = matches!(param, ParamAddress::Pan(_));
        let range = |value: f64| {
            (pan && !(-1.0..=1.0).contains(&value)).then(|| "must be from -1 to 1".to_owned())
        };
        Ok(automation::Lane {
            param,
            points: lane_points(points, path, true, range)?,
            overridden: *overridden,
        })
    }
}

/// The points of the lane at `path`, as `[at, value]` pairs: at least one,
/// in order of position, no more than two at one place, from the start of
/// the song on if `from_start`, and with finite values that `range` has no
/// complaint about.
fn lane_points(
    points: &[(i64, f64)],
    path: &str,
    from_start: bool,
    range: impl Fn(f64) -> Option<String>,
) -> Result<Vec<automation::Point>, Invalid> {
    if points.is_empty() {
        let message = "needs at least one point: a parameter without automation has no lane";
        return Err(Invalid::new(format!("{path}.points"), message));
    }
    let mut model_points: Vec<automation::Point> = Vec::with_capacity(points.len());
    for (j, &(at, value)) in points.iter().enumerate() {
        let path = format!("{path}.points[{j}]");
        let at = position(at, || format!("{path}[0]"))?;
        if from_start && at < Beats::ZERO {
            let message = "can't be before the start of the song";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        let value = finite(value, || format!("{path}[1]"))?;
        if let Some(message) = range(value) {
            return Err(Invalid::new(format!("{path}[1]"), message));
        }
        let earlier = model_points.len().checked_sub(1).map(|k| model_points[k]);
        if earlier.is_some_and(|p| p.at > at) {
            let message = "must be at or after the point before it";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        let twice = model_points.len() >= 2
            && model_points[model_points.len() - 2].at == at
            && earlier.is_some_and(|p| p.at == at);
        if twice {
            let message = "makes three points at one place: a step needs two";
            return Err(Invalid::new(format!("{path}[0]"), message));
        }
        model_points.push(automation::Point::new(at, value));
    }
    Ok(model_points)
}

/// Refuses an audio clip whose offset is of the other kind from what its
/// warp says, before it's saved: the file has one number for both, read as
/// ticks while the clip is warped and as a frame while it isn't.
pub(super) fn check_offsets(project: &model::Project) -> Result<(), Invalid> {
    for (i, track) in project.tracks.iter().enumerate() {
        let takes = track.takes.iter().enumerate();
        let lists = std::iter::once((format!("tracks[{i}].clips"), &track.clips))
            .chain(takes.map(|(j, take)| (format!("tracks[{i}].takes[{j}].clips"), &take.clips)));
        for (path, clips) in lists {
            for (k, clip) in clips.iter().enumerate() {
                let clip::ClipContent::Audio(audio) = &clip.content else {
                    continue;
                };
                let message = match (audio.offset, &audio.warp) {
                    (clip::Offset::Frames(_), Some(_)) => {
                        "must be in ticks of the clip's own time while it's warped"
                    }
                    (clip::Offset::Beats(_), None) => {
                        "must be a frame of the file while the clip is unwarped"
                    }
                    _ => continue,
                };
                let path = format!("{path}[{k}].content.audio.offset");
                return Err(Invalid::new(path, message));
            }
        }
    }
    Ok(())
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

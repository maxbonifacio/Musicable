//! Pure-data project model.
//!
//! Nothing in this crate touches audio devices, threads or the UI. The engine
//! compiles a [`Project`] into a processing graph; the UI edits it through
//! commands. [`time`] has musical time: beats, bars and the tempo map.
//! [`mixer`] has the mixer's rules: where tracks can send their signal, and
//! what mute and solo silence. [`clip`] has what tracks play: audio clips,
//! and MIDI clips of notes with per-note expression. [`Locator`]s name
//! places in the song. [`plugin`] has plugins in device chains, and
//! [`native`] Musicable's own devices. Tracks
//! record from an [`Input`] into [`Take`]s, and comping copies the best of
//! them to the track's own clips. [`param`] names every parameter in the
//! project, and [`automation`] has the lanes that move them as the song
//! plays, and [`modulation`] the modulators that move them on top. [`warp`]
//! has how audio clips follow the tempo. [`routing`] has the wires that
//! take one channel's signal into another's devices, such as a
//! compressor's sidechain.
//!
//! Project files (`musicable_io::project`) save every field of the project,
//! so adding or changing one changes the file format. See
//! `docs/project-format.md` for how.

pub mod automation;
pub mod clip;
pub mod mixer;
pub mod modulation;
pub mod native;
pub mod param;
pub mod plugin;
pub mod routing;
pub mod time;
pub mod warp;

use std::collections::BTreeMap;

use automation::Lane;
use clip::{Clip, ClipContent, ClipId};
use mixer::Bus;
use modulation::Modulation;
use native::NativeDevice;
use plugin::{PluginDevice, PluginKind};
use routing::{Tap, Wire};
use time::{BeatRange, Beats, TempoMap};

/// Stable identifier for a track within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Midi,
    /// A bus that other tracks' sends feed, usually for shared effects such
    /// as a reverb.
    Return,
    /// A bus that other tracks' outputs feed.
    Group,
}

impl TrackKind {
    /// Whether other tracks feed the track: a group or a return.
    pub fn is_bus(self) -> bool {
        matches!(self, Self::Return | Self::Group)
    }
}

/// A test oscillator: Tier 0's only sound source. It plays along with a
/// track's clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestTone {
    /// In Hz.
    pub frequency: f64,
    /// Linear amplitude, 1.0 = full scale.
    pub level: f32,
}

/// How a channel's clip stage clips. The engine mixes in floating point, so
/// nothing clips unless a channel asks for it. Tier 2 adds circuit-modelled
/// types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipType {
    /// Clean floating point.
    #[default]
    Off,
    /// Cuts everything beyond full scale.
    Hard,
    /// Rounds off peaks above half scale, so they get ever closer to full
    /// scale without reaching it.
    Soft,
}

/// A channel's console modelling settings. A placeholder until console mode
/// arrives (Tier 2 at the earliest), so project files have the field from
/// the start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConsoleSettings {}

/// Stable identifier for a device, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub u32);

/// A device in a channel's device chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub kind: DeviceKind,
    /// Whether it's bypassed: it stops, and what comes into it plays on in
    /// its place, delayed by its latency, so nothing else in the mix moves.
    /// Its Bypass lane automates it ([`ParamAddress::Bypass`]).
    ///
    /// [`ParamAddress::Bypass`]: param::ParamAddress::Bypass
    pub bypassed: bool,
    /// Whether it's in the signal at all. A deactivated device keeps its
    /// place and its settings, but the signal goes on as if it weren't
    /// there, latency and all.
    pub active: bool,
}

/// What a device is: one of Musicable's own, a plugin, or a test device.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceKind {
    /// Multiplies the signal by a linear gain, 1.0 = unity: a test device.
    TestGain { gain: f32 },
    /// Plays the notes of its track's MIDI clips with a plain tone, added to
    /// what comes into it: a test instrument. `level` is the linear
    /// amplitude of a note at full velocity.
    TestInstrument { level: f32 },
    /// A CLAP or VST3 plugin.
    Plugin(PluginDevice),
    /// One of Musicable's own devices.
    Native(NativeDevice),
}

impl DeviceKind {
    /// Whether the device plays notes. A MIDI track's notes go to its first
    /// instrument.
    pub fn is_instrument(&self) -> bool {
        match self {
            Self::TestInstrument { .. } => true,
            Self::Plugin(plugin) => plugin.kind == PluginKind::Instrument,
            Self::Native(native) => native.kind.is_instrument(),
            Self::TestGain { .. } => false,
        }
    }

    /// Whether the device plays the notes that come into it on, changed,
    /// to the instrument after it: a MIDI effect plugin. See
    /// [`Project::check_order`].
    pub fn is_midi_effect(&self) -> bool {
        match self {
            Self::Plugin(plugin) => plugin.kind == PluginKind::MidiEffect,
            Self::TestInstrument { .. } | Self::Native(_) | Self::TestGain { .. } => false,
        }
    }

    /// Whether the device plays notes out as it renders: a plugin with a
    /// note output that isn't a MIDI effect ([`PluginDevice::makes_notes`]).
    pub fn makes_notes(&self) -> bool {
        self.plugin().is_some_and(PluginDevice::makes_notes)
    }

    pub fn plugin(&self) -> Option<&PluginDevice> {
        match self {
            Self::Plugin(plugin) => Some(plugin),
            _ => None,
        }
    }

    pub fn plugin_mut(&mut self) -> Option<&mut PluginDevice> {
        match self {
            Self::Plugin(plugin) => Some(plugin),
            _ => None,
        }
    }

    pub fn native(&self) -> Option<&NativeDevice> {
        match self {
            Self::Native(native) => Some(native),
            _ => None,
        }
    }

    pub fn native_mut(&mut self) -> Option<&mut NativeDevice> {
        match self {
            Self::Native(native) => Some(native),
            _ => None,
        }
    }

    /// What the browser and the Edit menu call it: a built-in device by its
    /// kind, such as "EQ", and a plugin by its name.
    pub fn name(&self) -> &str {
        match self {
            Self::Native(native) => native.kind.name(),
            Self::Plugin(plugin) => match plugin.name.trim() {
                "" => "Plugin",
                name => name,
            },
            Self::TestGain { .. } | Self::TestInstrument { .. } => "Device",
        }
    }
}

/// Where a device chain is: on a track, or on the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Chain {
    Track(TrackId),
    Master,
}

impl Chain {
    /// The track it's on, unless it's the master's.
    pub fn track(self) -> Option<TrackId> {
        match self {
            Self::Track(id) => Some(id),
            Self::Master => None,
        }
    }
}

/// Whether `devices`, a chain's in order, are in an order notes can pass
/// through, on a channel that has notes or one that hasn't, and if not, why
/// not. A MIDI effect goes on a MIDI track, before its first instrument,
/// which plays the notes the MIDI effects play on to it, each to the next.
pub fn check_order<'a>(
    notes: bool,
    devices: impl IntoIterator<Item = &'a Device>,
) -> Result<(), Misplaced> {
    let mut instrument = false;
    for device in devices {
        if device.kind.is_midi_effect() {
            if !notes {
                return Err(Misplaced::NoNotes(device.id));
            }
            if instrument {
                return Err(Misplaced::AfterInstrument(device.id));
            }
        }
        instrument |= device.kind.is_instrument();
    }
    Ok(())
}

/// Why a chain's devices are in an order notes can't pass through: see
/// [`check_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misplaced {
    /// This MIDI effect is on an audio track, a group or a return track, or
    /// on the master, which have no notes.
    NoNotes(DeviceId),
    /// This MIDI effect comes after the track's first instrument, which
    /// plays the notes, so none reach it.
    AfterInstrument(DeviceId),
}

/// A plugin format Musicable hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluginFormat {
    Clap,
    Vst3,
}

/// Stable identifier for a send, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SendId(pub u32);

/// A send: some of a track's signal, fed to a return track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Send {
    pub id: SendId,
    /// The return track it feeds.
    pub to: TrackId,
    /// Linear gain, 1.0 = unity.
    pub level: f32,
    /// Whether it takes the signal before the track's fader and pan, rather
    /// than after them.
    pub pre_fader: bool,
}

/// Where a track records from: some channels of an audio input on an audio
/// track, a MIDI input on a MIDI track, or another track's signal at a tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Audio(AudioInput),
    Midi(MidiInput),
    /// What another track carries at a tap: its sound for an audio track,
    /// or another MIDI track's notes, its Pre FX, for a MIDI track. The
    /// master's Post Fader is [`RESAMPLING`](Self::RESAMPLING). See
    /// [`Project::check_input`].
    Tap(Tap),
}

impl Input {
    /// What a new track of `kind` records from: the default input's first
    /// two channels, in stereo, for audio; every MIDI input for MIDI; and
    /// nothing for a bus.
    pub fn default_for(kind: TrackKind) -> Option<Input> {
        match kind {
            TrackKind::Audio => Some(Input::Audio(AudioInput::default())),
            TrackKind::Midi => Some(Input::Midi(MidiInput::default())),
            TrackKind::Return | TrackKind::Group => None,
        }
    }

    /// Recording the master's Post Fader: the mix, which a track records
    /// but never plays, so it doesn't feed itself.
    pub const RESAMPLING: Input = Input::Tap(Tap::PostFader(Chain::Master));

    /// Whether it's [Resampling](Self::RESAMPLING).
    pub fn is_resampling(&self) -> bool {
        matches!(self, Self::Tap(Tap::PostFader(Chain::Master)))
    }

    pub fn audio(&self) -> Option<&AudioInput> {
        match self {
            Self::Audio(input) => Some(input),
            Self::Midi(_) | Self::Tap(_) => None,
        }
    }

    /// The tap it takes, for an input from another track.
    pub fn tap(&self) -> Option<Tap> {
        match *self {
            Self::Tap(tap) => Some(tap),
            Self::Audio(_) | Self::Midi(_) => None,
        }
    }
}

/// Some channels of an audio input. Defaults to the default input's first
/// two channels, in stereo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInput {
    /// The input, as the audio backend identifies it
    /// (`musicable_audio::InputDevice::id`), or empty for the default
    /// input: the system's, or the ASIO driver's while one plays.
    pub device: String,
    /// The input's name when it was chosen, to show while it's missing.
    /// Empty for the default input.
    pub name: String,
    /// The first channel, from 0.
    pub channel: u16,
    /// Whether it takes two channels, `channel` and the one after, rather
    /// than one. A mono input plays in the middle of a track's two
    /// channels.
    pub stereo: bool,
}

impl Default for AudioInput {
    fn default() -> Self {
        Self {
            device: String::new(),
            name: String::new(),
            channel: 0,
            stereo: true,
        }
    }
}

impl AudioInput {
    /// How many channels it takes.
    pub fn channels(&self) -> u16 {
        if self.stereo { 2 } else { 1 }
    }
}

/// A MIDI input, or all of them. Defaults to every channel of every input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MidiInput {
    /// The input, as the MIDI backend identifies it, or empty for every
    /// input.
    pub device: String,
    /// The input's name when it was chosen, to show while it's missing.
    pub name: String,
    /// The one MIDI channel to take, from 0 to 15, or `None` for all of
    /// them, as MPE controllers need.
    pub channel: Option<u8>,
}

/// When a track plays what comes into its input instead of its clips: its
/// input monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Monitor {
    /// Always.
    In,
    /// While the track is armed, and the transport is stopped or recording
    /// it: when a player hears themselves while they record, and the
    /// recording when it plays back.
    #[default]
    Auto,
    /// Never: the input is only recorded.
    Off,
}

/// Stable identifier for a take, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TakeId(pub u32);

/// A take lane: what a track recorded in one pass of a recording, kept
/// under the track's own clips, which comping copies the best parts of
/// takes to. Takes don't play.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    pub id: TakeId,
    pub name: String,
    /// Clips of the track's kind, in any order. Their ids are unique among
    /// all the project's clips.
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    /// Balance, from -1.0 (left) to 1.0 (right). At 0.0, the centre, both
    /// channels pass unchanged. See [`mixer::pan_gains`].
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    /// Whether other tracks' solo leaves the track playing. Return tracks
    /// start solo-safe.
    pub solo_safe: bool,
    /// Whether the track is armed for recording.
    pub armed: bool,
    /// Where it records from: `None` for a track that records nothing, as
    /// buses don't.
    pub input: Option<Input>,
    pub monitor: Monitor,
    /// Whether the track renders live, as the audio device asks for each
    /// buffer, while the rest of the project renders ahead of it: so its
    /// devices' knobs and windows keep time with what's heard, as they do
    /// on a track monitoring its input.
    pub always_live: bool,
    /// Where the track's output goes. See [`Project::can_feed`] for which
    /// buses a track can feed.
    pub output: Bus,
    /// At most one to each return track.
    pub sends: Vec<Send>,
    pub tone: Option<TestTone>,
    /// Audio clips on audio tracks, MIDI clips on MIDI tracks, and none on
    /// buses. In any order: the arrangement places them by position.
    pub clips: Vec<Clip>,
    /// The track's takes, top to bottom: none on buses.
    pub takes: Vec<Take>,
    /// In signal order.
    pub devices: Vec<Device>,
    /// The track's modulators and their routings.
    pub modulation: Modulation,
    pub clip_type: ClipType,
    pub console: ConsoleSettings,
}

impl Track {
    /// Whether the track can hold `content`: audio clips go on audio tracks
    /// and MIDI clips on MIDI tracks.
    pub fn holds(&self, content: &ClipContent) -> bool {
        matches!(
            (self.kind, content),
            (TrackKind::Audio, ClipContent::Audio(_)) | (TrackKind::Midi, ClipContent::Midi(_))
        )
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Where the clip with `id` is in the track's clip list.
    pub fn clip_index(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// The first of the track's devices that plays notes.
    pub fn instrument(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.kind.is_instrument())
    }

    pub fn take(&self, id: TakeId) -> Option<&Take> {
        self.takes.iter().find(|t| t.id == id)
    }

    pub fn take_mut(&mut self, id: TakeId) -> Option<&mut Take> {
        self.takes.iter_mut().find(|t| t.id == id)
    }

    /// Where the take with `id` is in the track's list of them.
    pub fn take_index(&self, id: TakeId) -> Option<usize> {
        self.takes.iter().position(|t| t.id == id)
    }

    /// Whether any of the track's clips, its takes' included, has `id`.
    pub fn uses_clip_id(&self, id: ClipId) -> bool {
        self.clip(id).is_some() || self.takes.iter().flat_map(|t| &t.clips).any(|c| c.id == id)
    }
}

/// Stable identifier for a locator, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocatorId(pub u32);

/// A named place in the song, shown on the arrangement's ruler, for finding
/// the parts of a song and jumping between them: a locator, as Live calls
/// them.
#[derive(Debug, Clone, PartialEq)]
pub struct Locator {
    pub id: LocatorId,
    pub name: String,
    pub at: Beats,
}

/// The master bus's channel: every track that doesn't feed a group feeds
/// the master, and the master feeds the audio device.
#[derive(Debug, Clone, PartialEq)]
pub struct Master {
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    /// Balance, as for a track.
    pub pan: f32,
    /// In signal order.
    pub devices: Vec<Device>,
    /// The master's modulators and their routings.
    pub modulation: Modulation,
    pub clip_type: ClipType,
    pub console: ConsoleSettings,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            devices: Vec::new(),
            modulation: Modulation::default(),
            clip_type: ClipType::Off,
            console: ConsoleSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Tempo and time signature.
    pub tempo: TempoMap,
    /// The region playback loops over. Kept while looping is off.
    pub loop_region: BeatRange,
    pub looping: bool,
    /// Whether the metronome clicks while the transport plays.
    pub metronome: bool,
    /// Whether recording waits for the loop region's start: it punches in
    /// there.
    pub punch_in: bool,
    /// Whether recording stops at the loop region's end: it punches out
    /// there.
    pub punch_out: bool,
    pub master: Master,
    pub tracks: Vec<Track>,
    /// In any order: the ruler shows them by position.
    pub locators: Vec<Locator>,
    /// The automated parameters' lanes, one for each, in any order.
    pub automation: Vec<Lane>,
    /// The wires between channels, such as the keys of compressors'
    /// sidechains, in the order they were made. See [`routing`].
    pub wires: Vec<Wire>,
    /// The id [`add_track`](Self::add_track) gives the next track. Ids are
    /// never reused, so it stays above the id of every track the project has
    /// had, deleted ones included.
    pub next_track_id: u32,
    /// The same for sends.
    pub next_send_id: u32,
    /// The same for devices, on tracks and on the master.
    pub next_device_id: u32,
    /// The same for clips.
    pub next_clip_id: u32,
    /// The same for locators.
    pub next_locator_id: u32,
    /// The same for takes.
    pub next_take_id: u32,
    /// The same for modulators, on tracks and on the master.
    pub next_modulator_id: u32,
    /// The same for routings.
    pub next_routing_id: u32,
    /// The same for wires.
    pub next_wire_id: u32,
    /// The name of the workspace the project was saved with, which lays the
    /// app's panels out, for the app to switch to when it opens the
    /// project. Edits leave it alone: the app sets it as it saves.
    pub workspace: Option<String>,
    /// Where the arrangement was when the project was saved, and the views
    /// kept in its slots. Like the workspace, edits leave it alone: the app
    /// sets it as it saves, and takes it as it opens the project.
    pub view: Option<SavedView>,
}

/// How many views a project keeps in slots, for Recall View 1 to 5.
pub const VIEW_SLOTS: usize = 5;

/// The arrangement's view as a project file keeps it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SavedView {
    /// Where the arrangement was.
    pub place: ViewPlace,
    /// Whether Follow was on in the arrangement, the piano roll and the
    /// Sample Editor.
    pub follow: [bool; 3],
    /// The views saved with Save View 1 to 5.
    pub slots: [Option<ViewPlace>; VIEW_SLOTS],
}

/// A place in the arrangement: where it starts, its zoom, how far down
/// it's scrolled, and the heights the user gave its rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewPlace {
    /// The song position at the left edge.
    pub start: Beats,
    /// How many points a beat takes.
    pub zoom: f64,
    /// How far down the tracks are scrolled, in points.
    pub top: f32,
    /// The heights the user gave tracks and the master's row, in points.
    pub heights: BTreeMap<Chain, f32>,
}

impl Default for Project {
    /// 120 BPM in 4/4, with the loop region on the first four bars.
    fn default() -> Self {
        let tempo = TempoMap::default();
        Self {
            loop_region: BeatRange::new(Beats::ZERO, tempo.bar_start(4)),
            tempo,
            looping: false,
            metronome: false,
            punch_in: false,
            punch_out: false,
            master: Master::default(),
            tracks: Vec::new(),
            locators: Vec::new(),
            automation: Vec::new(),
            wires: Vec::new(),
            next_track_id: 0,
            next_send_id: 0,
            next_device_id: 0,
            next_clip_id: 0,
            next_locator_id: 0,
            next_take_id: 0,
            next_modulator_id: 0,
            next_routing_id: 0,
            next_wire_id: 0,
            workspace: None,
            view: None,
        }
    }
}

impl Project {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a track at the end, and returns its id.
    pub fn add_track(&mut self, name: impl Into<String>, kind: TrackKind) -> TrackId {
        let track = self.new_track(name, kind);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// A track with the next track id, for the caller to add: unity gain,
    /// centred, feeding the master, recording from the default input
    /// ([`Input::default_for`]) with automatic monitoring. Return tracks
    /// start solo-safe.
    ///
    /// # Panics
    ///
    /// If the project has used up every track id.
    pub fn new_track(&mut self, name: impl Into<String>, kind: TrackKind) -> Track {
        let id = TrackId(next_id(&mut self.next_track_id, "track"));
        Track {
            id,
            name: name.into(),
            kind,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            solo_safe: kind == TrackKind::Return,
            armed: false,
            input: Input::default_for(kind),
            monitor: Monitor::Auto,
            always_live: false,
            output: Bus::Master,
            sends: Vec::new(),
            tone: None,
            clips: Vec::new(),
            takes: Vec::new(),
            devices: Vec::new(),
            modulation: Modulation::default(),
            clip_type: ClipType::Off,
            console: ConsoleSettings::default(),
        }
    }

    /// A send from a track to the return track `to`, with the next send id,
    /// for the caller to add. It starts silent, after the fader.
    pub fn new_send(&mut self, to: TrackId) -> Send {
        Send {
            id: SendId(next_id(&mut self.next_send_id, "send")),
            to,
            level: 0.0,
            pre_fader: false,
        }
    }

    /// A device with the next device id, for the caller to add: active,
    /// and not bypassed.
    pub fn new_device(&mut self, kind: DeviceKind) -> Device {
        Device {
            id: DeviceId(next_id(&mut self.next_device_id, "device")),
            kind,
            bypassed: false,
            active: true,
        }
    }

    /// A clip with the next clip id, for the caller to put on a track.
    pub fn new_clip(
        &mut self,
        name: impl Into<String>,
        range: BeatRange,
        content: ClipContent,
    ) -> Clip {
        Clip {
            id: self.new_clip_id(),
            name: name.into(),
            start: range.start,
            length: range.len(),
            content,
        }
    }

    /// The next clip id, for a copy of a clip.
    pub fn new_clip_id(&mut self) -> ClipId {
        ClipId(next_id(&mut self.next_clip_id, "clip"))
    }

    /// A locator with the next locator id, for the caller to add.
    pub fn new_locator(&mut self, name: impl Into<String>, at: Beats) -> Locator {
        Locator {
            id: LocatorId(next_id(&mut self.next_locator_id, "locator")),
            name: name.into(),
            at,
        }
    }

    /// An empty take with the next take id, for the caller to add.
    pub fn new_take(&mut self, name: impl Into<String>) -> Take {
        Take {
            id: TakeId(next_id(&mut self.next_take_id, "take")),
            name: name.into(),
            clips: Vec::new(),
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Where the track with `id` is in the track list.
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The devices of a chain, in signal order: `None` for a track the
    /// project doesn't have.
    pub fn devices(&self, chain: Chain) -> Option<&[Device]> {
        match chain {
            Chain::Track(id) => self.track(id).map(|t| t.devices.as_slice()),
            Chain::Master => Some(&self.master.devices),
        }
    }

    pub fn devices_mut(&mut self, chain: Chain) -> Option<&mut Vec<Device>> {
        match chain {
            Chain::Track(id) => self.track_mut(id).map(|t| &mut t.devices),
            Chain::Master => Some(&mut self.master.devices),
        }
    }

    /// Every device chain, the tracks' in order and then the master's.
    pub fn chains(&self) -> impl Iterator<Item = (Chain, &[Device])> {
        let tracks = self
            .tracks
            .iter()
            .map(|t| (Chain::Track(t.id), t.devices.as_slice()));
        tracks.chain([(Chain::Master, self.master.devices.as_slice())])
    }

    /// The device with `id`, the chain it's in, and where it is in it.
    pub fn find_device(&self, id: DeviceId) -> Option<(Chain, usize, &Device)> {
        self.chains().find_map(|(chain, devices)| {
            let index = devices.iter().position(|d| d.id == id)?;
            Some((chain, index, &devices[index]))
        })
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        let tracks = self.tracks.iter_mut().flat_map(|t| &mut t.devices);
        tracks.chain(&mut self.master.devices).find(|d| d.id == id)
    }

    /// Whether the devices of `chain` are in an order notes can pass
    /// through, and if not, why not: see [`check_order`]. A chain the
    /// project hasn't is in order.
    pub fn check_order(&self, chain: Chain) -> Result<(), Misplaced> {
        let devices = self.devices(chain).unwrap_or_default();
        check_order(self.has_notes(chain), devices)
    }

    /// Whether `chain` has notes for MIDI effects to play on: whether it's
    /// a MIDI track's.
    pub fn has_notes(&self, chain: Chain) -> bool {
        (chain.track())
            .and_then(|track| self.track(track))
            .is_some_and(|track| track.kind == TrackKind::Midi)
    }

    /// Where a MIDI effect goes in `chain` when nothing says where: just
    /// before its first instrument, or at the end of a chain without one.
    pub fn midi_effect_index(&self, chain: Chain) -> usize {
        let devices = self.devices(chain).unwrap_or_default();
        (devices.iter())
            .position(|device| device.kind.is_instrument())
            .unwrap_or(devices.len())
    }

    /// Where a device of `kind` put at `index` in `chain`, or past its end,
    /// goes, so the chain stays in order ([`check_order`]): a MIDI effect
    /// no later than just before the first instrument, and an instrument
    /// no earlier than just after the last MIDI effect. `None` for a MIDI
    /// effect where there are no notes, and for a chain the project hasn't.
    pub fn fit_device(&self, chain: Chain, index: usize, kind: &DeviceKind) -> Option<usize> {
        let devices = self.devices(chain)?;
        let index = index.min(devices.len());
        if kind.is_midi_effect() {
            let notes = self.has_notes(chain);
            return notes.then(|| index.min(self.midi_effect_index(chain)));
        }
        if kind.is_instrument() {
            let effects = devices.iter().rposition(|d| d.kind.is_midi_effect());
            return Some(index.max(effects.map_or(0, |last| last + 1)));
        }
        Some(index)
    }

    /// The clip with `id`, and the track it's on. Only tracks' own clips:
    /// takes' clips are found through their take.
    pub fn find_clip(&self, id: ClipId) -> Option<(&Track, &Clip)> {
        self.tracks
            .iter()
            .find_map(|track| Some((track, track.clip(id)?)))
    }

    /// Whether any clip in the project, takes' included, has `id`.
    pub fn uses_clip_id(&self, id: ClipId) -> bool {
        self.tracks.iter().any(|t| t.uses_clip_id(id))
    }

    /// The take with `id`, and the track it's under.
    pub fn find_take(&self, id: TakeId) -> Option<(&Track, &Take)> {
        self.tracks
            .iter()
            .find_map(|track| Some((track, track.take(id)?)))
    }

    pub fn locator(&self, id: LocatorId) -> Option<&Locator> {
        self.locators.iter().find(|l| l.id == id)
    }

    pub fn locator_mut(&mut self, id: LocatorId) -> Option<&mut Locator> {
        self.locators.iter_mut().find(|l| l.id == id)
    }

    /// Where the locator with `id` is in the project's list of them.
    pub fn locator_index(&self, id: LocatorId) -> Option<usize> {
        self.locators.iter().position(|l| l.id == id)
    }
}

/// Takes the next id from a counter in the project. An id is only handed out if
/// the counter can move past it, so `u32::MAX` is never an id.
pub(crate) fn next_id(counter: &mut u32, what: &str) -> u32 {
    let id = *counter;
    *counter = id
        .checked_add(1)
        .unwrap_or_else(|| panic!("the project has used every {what} id"));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_are_unique() {
        let mut p = Project::new();
        let a = p.add_track("Drums", TrackKind::Audio);
        let b = p.add_track("Bass", TrackKind::Midi);
        assert_ne!(a, b);
        assert_eq!(p.track(b).unwrap().name, "Bass");
        assert_eq!(p.track_index(b), Some(1));
    }

    #[test]
    fn new_tracks_dont_clip() {
        let mut p = Project::new();
        let id = p.add_track("Vocals", TrackKind::Audio);
        assert_eq!(p.track(id).unwrap().clip_type, ClipType::Off);
    }

    #[test]
    fn new_tracks_are_centred_on_the_master_and_only_returns_are_solo_safe() {
        let mut p = Project::new();
        for kind in [
            TrackKind::Audio,
            TrackKind::Midi,
            TrackKind::Return,
            TrackKind::Group,
        ] {
            let id = p.add_track("", kind);
            let track = p.track(id).unwrap();
            assert_eq!((track.gain, track.pan), (1.0, 0.0));
            assert_eq!(track.output, Bus::Master);
            assert_eq!(track.solo_safe, kind == TrackKind::Return);
        }
    }

    #[test]
    fn new_tracks_record_from_the_default_input_and_buses_from_nothing() {
        let mut p = Project::new();
        let input = |p: &mut Project, kind| {
            let id = p.add_track("", kind);
            let track = p.track(id).unwrap();
            assert_eq!(track.monitor, Monitor::Auto);
            track.input.clone()
        };
        let audio = input(&mut p, TrackKind::Audio).unwrap();
        assert_eq!(audio.audio().map(AudioInput::channels), Some(2));
        assert_eq!(audio.audio().map(|a| a.device.as_str()), Some(""));
        assert_eq!(
            input(&mut p, TrackKind::Midi),
            Some(Input::Midi(MidiInput::default()))
        );
        assert_eq!(input(&mut p, TrackKind::Return), None);
        assert_eq!(input(&mut p, TrackKind::Group), None);
    }

    #[test]
    fn takes_and_their_clips_are_found_by_id() {
        let mut p = Project::new();
        let track = p.add_track("Vocal", TrackKind::Audio);
        let mut take = p.new_take("Take 1");
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let audio = || ClipContent::Audio(clip::AudioClip::new("take.wav"));
        let clip = p.new_clip("Vocal", range, audio());
        let in_take = clip.id;
        take.clips.push(clip);
        let id = take.id;
        assert_ne!(p.new_take("Take 2").id, id);
        p.track_mut(track).unwrap().takes.push(take);

        let (found, take) = p.find_take(id).unwrap();
        assert_eq!((found.id, take.name.as_str()), (track, "Take 1"));
        assert_eq!(found.take_index(id), Some(0));
        assert!(p.find_take(TakeId(id.0 + 1)).is_none());
        // Takes' clips use clip ids, but aren't the track's own clips.
        assert!(p.uses_clip_id(in_take));
        assert!(p.find_clip(in_take).is_none());
        let own = p.new_clip("Vocal", range, audio());
        assert!(!p.uses_clip_id(own.id));
        p.track_mut(track).unwrap().clips.push(own.clone());
        assert!(p.uses_clip_id(own.id));
    }

    #[test]
    fn ids_are_never_reused() {
        let mut p = Project::new();
        let first = p.add_track("A", TrackKind::Audio);
        p.tracks.clear();
        assert_ne!(p.add_track("B", TrackKind::Audio), first);
        let send = p.new_send(TrackId(9));
        assert_ne!(p.new_send(TrackId(9)).id, send.id);
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        assert_ne!(
            p.new_device(DeviceKind::TestGain { gain: 1.0 }).id,
            device.id
        );
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let midi = || ClipContent::Midi(clip::MidiClip::default());
        let clip = p.new_clip("", range, midi());
        assert_ne!(p.new_clip("", range, midi()).id, clip.id);
        let locator = p.new_locator("Verse", Beats::new(16));
        assert_ne!(p.new_locator("Verse", Beats::new(16)).id, locator.id);
    }

    #[test]
    fn clips_and_locators_are_found_by_id() {
        let mut p = Project::new();
        let track = p.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::new(4), Beats::new(8));
        let clip = p.new_clip("Riff", range, ClipContent::Midi(clip::MidiClip::default()));
        let id = clip.id;
        p.track_mut(track).unwrap().clips.push(clip);
        let (found, clip) = p.find_clip(id).unwrap();
        assert_eq!((found.id, clip.name.as_str()), (track, "Riff"));
        assert!(p.find_clip(ClipId(id.0 + 1)).is_none());

        let chorus = p.new_locator("Chorus", Beats::new(32));
        let at = chorus.id;
        p.locators.push(chorus);
        p.locator_mut(at).unwrap().name = "Drop".into();
        assert_eq!(p.locator(at).unwrap().name, "Drop");
        assert_eq!(p.locator_index(at), Some(0));
    }

    #[test]
    fn tracks_hold_clips_of_their_kind() {
        let mut p = Project::new();
        let audio = ClipContent::Audio(clip::AudioClip::new("take.wav"));
        let midi = ClipContent::Midi(clip::MidiClip::default());
        let holds = |p: &mut Project, kind| {
            let id = p.add_track("", kind);
            let track = p.track(id).unwrap();
            (track.holds(&audio), track.holds(&midi))
        };
        assert_eq!(holds(&mut p, TrackKind::Audio), (true, false));
        assert_eq!(holds(&mut p, TrackKind::Midi), (false, true));
        assert_eq!(holds(&mut p, TrackKind::Return), (false, false));
        assert_eq!(holds(&mut p, TrackKind::Group), (false, false));
    }

    #[test]
    fn a_tracks_notes_go_to_its_first_instrument() {
        let mut p = Project::new();
        let id = p.add_track("Keys", TrackKind::Midi);
        let gain = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        let first = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let second = p.new_device(DeviceKind::TestInstrument { level: 0.25 });
        let track = p.track_mut(id).unwrap();
        assert!(track.instrument().is_none());
        track.devices = vec![gain, first.clone(), second];
        assert_eq!(track.instrument(), Some(&first));
    }

    fn plugin(kind: PluginKind) -> DeviceKind {
        DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.plugin".into(),
            name: "Plugin".into(),
            kind,
            inputs: Vec::new(),
            note_output: false,
            note_timing: crate::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: plugin::PluginSettings::default(),
        })
    }

    #[test]
    fn plugins_that_play_notes_are_instruments() {
        assert!(plugin(PluginKind::Instrument).is_instrument());
        assert!(!plugin(PluginKind::MidiEffect).is_instrument());
        assert!(plugin(PluginKind::MidiEffect).is_midi_effect());
        assert!(!plugin(PluginKind::AudioEffect).is_instrument());
        assert!(!plugin(PluginKind::AudioEffect).is_midi_effect());
        assert!(plugin(PluginKind::AudioEffect).plugin().is_some());
        assert!(DeviceKind::TestGain { gain: 1.0 }.plugin().is_none());
        for kind in PluginKind::ALL {
            assert_eq!(PluginKind::from_key(kind.key()), Some(kind));
        }
        assert_eq!(PluginKind::from_key("effect"), None);
    }

    #[test]
    fn midi_effects_go_on_midi_tracks_before_the_instrument() {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let vocal = p.add_track("Vocal", TrackKind::Audio);
        let [arp, chord] = [0; 2].map(|_| p.new_device(plugin(PluginKind::MidiEffect)));
        let synth = p.new_device(plugin(PluginKind::Instrument));
        let eq = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        let chain = Chain::Track(keys);
        assert_eq!(p.midi_effect_index(chain), 0);
        let devices = vec![arp.clone(), eq.clone(), synth.clone()];
        p.track_mut(keys).unwrap().devices = devices;
        assert_eq!(p.check_order(chain), Ok(()));
        assert_eq!(p.midi_effect_index(chain), 2);
        p.track_mut(keys).unwrap().devices.push(chord.clone());
        assert_eq!(
            p.check_order(chain),
            Err(Misplaced::AfterInstrument(chord.id))
        );
        // Without an instrument, they can go anywhere on the track.
        p.track_mut(keys).unwrap().devices = vec![eq.clone(), chord.clone()];
        assert_eq!(p.check_order(chain), Ok(()));
        assert_eq!(p.midi_effect_index(chain), 2);
        p.track_mut(vocal).unwrap().devices.push(arp.clone());
        let misplaced = Err(Misplaced::NoNotes(arp.id));
        assert_eq!(p.check_order(Chain::Track(vocal)), misplaced);
        p.master.devices.push(arp.clone());
        assert_eq!(p.check_order(Chain::Master), misplaced);
        assert_eq!(p.check_order(Chain::Track(TrackId(9))), Ok(()));
    }

    #[test]
    fn devices_fit_where_notes_pass_through_them_in_order() {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let vocal = p.add_track("Vocal", TrackKind::Audio);
        let arp = p.new_device(plugin(PluginKind::MidiEffect));
        let synth = p.new_device(plugin(PluginKind::Instrument));
        let eq = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        p.track_mut(keys).unwrap().devices = vec![eq, arp.clone(), synth];
        let chain = Chain::Track(keys);
        let fit = |kind: PluginKind, index| p.fit_device(chain, index, &plugin(kind));
        // Nothing comes after the synth, at 2, or before the arp, at 1.
        assert_eq!(fit(PluginKind::MidiEffect, 0), Some(0));
        assert_eq!(fit(PluginKind::MidiEffect, 3), Some(2));
        assert_eq!(fit(PluginKind::MidiEffect, usize::MAX), Some(2));
        assert_eq!(fit(PluginKind::Instrument, 0), Some(2));
        assert_eq!(fit(PluginKind::Instrument, usize::MAX), Some(3));
        assert_eq!(fit(PluginKind::AudioEffect, 0), Some(0));
        assert_eq!(fit(PluginKind::AudioEffect, 9), Some(3));
        let midi = plugin(PluginKind::MidiEffect);
        assert_eq!(p.fit_device(Chain::Track(vocal), 0, &midi), None);
        assert_eq!(p.fit_device(Chain::Master, 0, &midi), None);
        assert_eq!(p.fit_device(Chain::Track(TrackId(9)), 0, &midi), None);
        assert!(p.has_notes(chain) && !p.has_notes(Chain::Track(vocal)));
    }

    #[test]
    fn devices_are_found_by_id_on_tracks_and_the_master() {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let synth = p.new_device(plugin(PluginKind::Instrument));
        let gain = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        let limiter = p.new_device(plugin(PluginKind::AudioEffect));
        let ids = (synth.id, gain.id, limiter.id);
        p.track_mut(keys).unwrap().devices = vec![synth, gain];
        p.master.devices.push(limiter);

        assert_eq!(
            p.find_device(ids.1).map(|(c, i, _)| (c, i)),
            Some((Chain::Track(keys), 1))
        );
        assert_eq!(
            p.find_device(ids.2).map(|(c, i, _)| (c, i)),
            Some((Chain::Master, 0))
        );
        assert!(p.find_device(DeviceId(99)).is_none());
        p.device_mut(ids.2).unwrap().kind = plugin(PluginKind::Instrument);
        assert!(p.master.devices[0].kind.is_instrument());
        assert_eq!(p.devices(Chain::Track(keys)).map(<[Device]>::len), Some(2));
        assert_eq!(p.devices(Chain::Track(TrackId(7))), None);
        let chains: Vec<Chain> = p.chains().map(|(chain, _)| chain).collect();
        assert_eq!(chains, [Chain::Track(keys), Chain::Master]);
        p.devices_mut(Chain::Master).unwrap().clear();
        assert!(p.find_device(ids.2).is_none());
    }

    #[test]
    #[should_panic(expected = "every track id")]
    fn the_last_id_is_never_used() {
        let mut p = Project::new();
        p.next_track_id = u32::MAX;
        p.add_track("One too many", TrackKind::Audio);
    }
}

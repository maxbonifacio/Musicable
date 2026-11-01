//! Compiling the project model into a graph, off the audio thread.
//!
//! Each track becomes a channel strip: its source, its devices, its fader
//! and its output stage, with built-in slots for native stages along the way
//! (see [`Slot`]). An audio or MIDI track's source is its clip player
//! ([`AudioClips`], [`MidiClips`]), with the track's test tone feeding it.
//! An audio track with an input plays it through an [`InputMonitor`] after
//! its clip player, which plays the input in place of the clips while the
//! track monitors it: the [`InputLayout`] says where the input is. A MIDI
//! track's notes go to its first instrument through a [`MidiMonitor`],
//! which adds what comes into the track's MIDI input as the track monitors
//! it: the layout says which MIDI inputs it takes. On their way, they go
//! through the MIDI effects before the instrument, each playing them on to
//! the next. Group and return tracks
//! are buses with strips of their own: what feeds them sums at the head of
//! their strip. Tracks feed the master or a group, sends feed return tracks,
//! and the master's strip feeds the audio device. Empty slots add no nodes.
//! The metronome feeds the cue bus, which the engine plays alongside the
//! master, not through it. Node keys come from model ids and slots (see
//! [`keys`]), so a channel's nodes keep their identity when the project is
//! recompiled.
//!
//! Plugins process outside the engine, in plugin instances the app holds:
//! [`Devices`] has the app's [`NodeMaker`] for each plugin device. Each
//! device's node sits in a [`Switch`], which bypasses it as the device's
//! Bypass says, and a MIDI effect's in a [`NoteSwitch`], out of the signal.
//! A deactivated device's switch stays in the graph, so its plugin stops in
//! step with it, but out of the signal, latency and all, and out of the
//! notes' way.
//!
//! Each channel with modulators, or with parameters that modulators move,
//! has a [`Modulation`] node, which runs its modulators and modulates its
//! parameters ([`Graph::modulate`]), taking in what the modulators of other
//! channels add to them: see [`modulation`](crate::modulation). A MIDI
//! track's node gets the track's notes as its instrument does, after its
//! MIDI effects.
//!
//! Wires take a tap's signal through a gain into a device's side input,
//! such as a compressor's sidechain, once every strip is built, so they can
//! join any two. Taps come before the output stage, where mute and solo
//! cut, so a muted track's wires carry on. Delay compensation lines up a
//! side input with the main input, as it does every path that meets. A
//! note wire takes the notes at a MIDI track's tap through a [`NoteWire`]
//! into an instrument's switch, or a MIDI effect's that plays, which plays
//! them beside the notes of its own track: under ids of their own, on the
//! wire's channel if it has one. Unlike an audio wire's, a muted track's
//! note wires are muted with it, as playing on its own track's instrument
//! would be. The instrument's channel's modulators get the notes of the
//! wires into its first instrument as well as its own.
//!
//! An audio track taking its input from another track's tap
//! ([`Input::Tap`]) has the tap's signal come into its input monitor's side
//! input, which the monitor plays as it would an interface input's. One
//! resampling the master has no connection: it never plays the master, so
//! it can't feed itself. While it records, its monitor cuts what goes into
//! its devices and a [`ResamplingCut`] after them what they ring on with,
//! so none of the track is in the mix. Each tap an audio track takes its
//! input from has a record tap ([`Graph::add_record`]) keyed
//! [`keys::record`], which the engine hands to the tap's writer for the
//! recorder. A MIDI track taking another's notes has the node that plays
//! them at the tap send them into its own monitor's side input for notes
//! ([`Graph::send_side_notes`]), which plays them as it would a MIDI
//! input's: at the other's Pre FX, its [`MidiMonitor`]; after one of its
//! MIDI effects, the switch of the last one that plays up to there, or the
//! monitor; and at its Post FX, where the notes reach its instrument, the
//! last of them all. A device in the signal that plays notes out as it
//! renders, a plugin with a note output that isn't a MIDI effect, on any
//! channel, has its switch send them from the point after it once it has
//! rendered ([`Graph::send_made_notes`]), to the monitors and the relays
//! taking them, which render after it in the same block. The tap has a
//! note record ([`Graph::add_note_record`]) keyed [`keys::record_notes`].
//! A track that goes to Sends Only feeds nothing but its sends.
//!
//! Each lane of automation that isn't overridden goes to the node with its
//! parameter ([`keys::param`], [`Graph::automate`]), which follows it as the
//! song plays.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::iter;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use musicable_model::mixer::Bus;
use musicable_model::modulation::{ModulatorKind, Output, Routing, envelope, lfo};
use musicable_model::param::ParamAddress;
use musicable_model::routing::{self, Tap};
use musicable_model::time::TempoMap;
use musicable_model::{
    AudioInput, Chain, ClipType, ConsoleSettings, Device, DeviceId, DeviceKind, Input, Master,
    MidiInput, Project, Track, TrackId, TrackKind,
};

use crate::clips::{AudioClips, MidiClips};
use crate::media::Media;
use crate::meter::Meters;
use crate::midi::{MidiMonitor, NoteWire};
use crate::modulation::Modulation;
use crate::nodes::{
    Clipper, Gain, InputMonitor, Metronome, ResamplingCut, Sine, Sum, TestInstrument,
};
use crate::switch::{NoteSwitch, Switch};
use crate::{
    Automation, EngineConfig, Graph, GraphError, InputPort, Node, NodeId, NodeKey, ParamId,
    PreparedGraph,
};

/// The keys [`compile`] gives model objects' nodes.
///
/// A track's nodes have its id in the low 32 bits of the key and their role
/// in the 16 bits above: 1 for the fader, 2 for the test tone, 3 to 5 for
/// the built-in slots after the devices, 7 for the output stage, 8 for a
/// group's or a return's bus, 11 for an audio or MIDI track's clip player,
/// 12 for a track's input monitor, 13 for its modulators' node and 19 for
/// the cut after the devices of a track resampling the master. The
/// master's strip has the keys of a
/// track whose id is `u32::MAX`, which no track has. Sends and devices have
/// their own ids in the low 32 bits, with role 9 for sends and 10 for
/// devices, and so does the slot in front of a device, with role 6, so it
/// goes where the device goes. Wires have theirs with role 14. Keys with
/// role 0 belong to no channel. The record taps of the taps tracks take
/// their inputs from have keys too, which no node has: role 15 with a
/// track's id for its Pre FX, 16 with a device's for what it puts out, and
/// 17 and 18 with a channel's for its Post FX and its Post Fader.
pub mod keys {
    use musicable_model::param::ParamAddress;
    use musicable_model::routing::{Tap, WireId};
    use musicable_model::{Chain, DeviceId, Project, SendId, TrackId};

    use super::Slot;
    use crate::modulation;
    use crate::nodes::Gain;
    use crate::switch::Switch;
    use crate::{NodeKey, ParamId};

    /// The master bus, where the tracks that don't feed a group sum.
    pub const MASTER: NodeKey = NodeKey(0);

    /// The [`Metronome`](crate::nodes::Metronome).
    pub const METRONOME: NodeKey = NodeKey(1);

    /// The cue bus: the graph's [cue output](crate::Graph::set_cue), which
    /// the metronome feeds.
    pub const CUE: NodeKey = NodeKey(2);

    /// The track id the master's strip has the keys of. Ids are handed out
    /// only below it.
    const MASTER_STRIP: TrackId = TrackId(u32::MAX);

    /// A track's fader, a [`Gain`](crate::nodes::Gain) with the track's gain
    /// and pan.
    pub fn track_gain(track: TrackId) -> NodeKey {
        track_key(1, track)
    }

    /// A track's test tone, a [`Sine`](crate::nodes::Sine).
    pub fn track_tone(track: TrackId) -> NodeKey {
        track_key(2, track)
    }

    /// The native stage in one of a track's built-in slots. The one in
    /// front of a device has the device's id, not the track's, so it's the
    /// same on any track, and on the master.
    pub fn track_slot(track: TrackId, slot: Slot) -> NodeKey {
        let role = match slot {
            Slot::PreFader => 3,
            Slot::PostFader => 4,
            Slot::BusFeed => 5,
            Slot::BeforeDevice(device) => return NodeKey(6 << 32 | u64::from(device.0)),
        };
        track_key(role, track)
    }

    /// A track's output stage, a [`Gain`](crate::nodes::Gain) at unity
    /// where mute and solo cut the track's output and its meter measures.
    pub fn track_output(track: TrackId) -> NodeKey {
        track_key(7, track)
    }

    /// The bus of a group or return track, where what feeds it sums.
    pub fn track_bus(track: TrackId) -> NodeKey {
        track_key(8, track)
    }

    /// An audio or MIDI track's clip player, an
    /// [`AudioClips`](crate::clips::AudioClips) or a
    /// [`MidiClips`](crate::clips::MidiClips), at the head of its strip.
    pub fn track_clips(track: TrackId) -> NodeKey {
        track_key(11, track)
    }

    /// An audio track's [`InputMonitor`](crate::nodes::InputMonitor),
    /// right after its clip player, or a MIDI track's
    /// [`MidiMonitor`](crate::MidiMonitor), between its clip player and its
    /// instrument.
    pub fn track_input(track: TrackId) -> NodeKey {
        track_key(12, track)
    }

    /// The [`ResamplingCut`](crate::nodes::ResamplingCut) after the
    /// devices of an audio track resampling the master.
    pub fn track_cut(track: TrackId) -> NodeKey {
        track_key(19, track)
    }

    /// A send, a [`Gain`](crate::nodes::Gain) at the send's level that feeds
    /// a return track's bus.
    pub fn send(send: SendId) -> NodeKey {
        NodeKey(9 << 32 | u64::from(send.0))
    }

    /// A device, on a track or on the master: its [`Switch`], with the
    /// device's node inside.
    pub fn device(device: DeviceId) -> NodeKey {
        NodeKey(10 << 32 | u64::from(device.0))
    }

    /// A channel's [`Modulation`](crate::modulation::Modulation): its
    /// modulators, and what moves its parameters.
    pub fn modulation(chain: Chain) -> NodeKey {
        match chain {
            Chain::Track(track) => track_key(13, track),
            Chain::Master => track_key(13, MASTER_STRIP),
        }
    }

    /// A wire, a [`Gain`](crate::nodes::Gain) at the wire's gain that feeds
    /// a device's side input, or for a note wire, the
    /// [`NoteWire`](crate::NoteWire) that plays its notes into an
    /// instrument or a MIDI effect.
    pub fn wire(wire: WireId) -> NodeKey {
        NodeKey(14 << 32 | u64::from(wire.0))
    }

    /// The record tap ([`Graph::add_record`](crate::Graph::add_record)) of
    /// `tap`, for an audio track that takes its input from it: the key of
    /// the [`TapWriter`](crate::TapWriter) it goes to. No node has it.
    pub fn record(tap: Tap) -> NodeKey {
        match tap {
            Tap::PreFx(track) => track_key(15, track),
            Tap::Device(device) => NodeKey(16 << 32 | u64::from(device.0)),
            Tap::PostFx(on) => track_key(17, strip(on)),
            Tap::PostFader(on) => track_key(18, strip(on)),
        }
    }

    /// The note record ([`Graph::add_note_record`](crate::Graph::add_note_record))
    /// of `tap`, for a MIDI track that takes its input from it: the key of
    /// the [`TapWriter`](crate::TapWriter) it goes to, which isn't
    /// [`record`]'s, so a MIDI track's Post FX can have both. No node has
    /// it.
    pub fn record_notes(tap: Tap) -> NodeKey {
        match tap {
            Tap::PreFx(track) => track_key(20, track),
            Tap::Device(device) => NodeKey(21 << 32 | u64::from(device.0)),
            Tap::PostFx(on) => track_key(22, strip(on)),
            Tap::PostFader(on) => track_key(23, strip(on)),
        }
    }

    /// The track id whose keys `chain`'s strip has.
    fn strip(chain: Chain) -> TrackId {
        match chain {
            Chain::Track(track) => track,
            Chain::Master => MASTER_STRIP,
        }
    }

    /// The master's fader, a [`Gain`](crate::nodes::Gain).
    pub fn master_gain() -> NodeKey {
        track_gain(MASTER_STRIP)
    }

    /// The native stage in one of the master's built-in slots.
    pub fn master_slot(slot: Slot) -> NodeKey {
        track_slot(MASTER_STRIP, slot)
    }

    /// The master's output stage, where its meter measures: the graph's
    /// output.
    pub fn master_output() -> NodeKey {
        track_output(MASTER_STRIP)
    }

    /// What a node belongs to, by its key: see [`owner`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Owner {
        /// A device's node: its switch, with the device inside.
        Device(DeviceId),
        /// The native stage in front of a device, which is part of the
        /// strip of the channel the device is on.
        BeforeDevice(DeviceId),
        /// What a track plays: its clip player, its input monitor and its
        /// test tone.
        Clips(TrackId),
        /// A channel's modulators.
        Modulators(Chain),
        /// The rest of a channel's strip: its fader, its built-in slots,
        /// its output stage, its bus, and the cut after the devices of a
        /// track resampling the master. The master's bus, where tracks
        /// sum, is the master's.
        Strip(Chain),
        /// A send, which is part of the strip of the track that sends.
        Send(SendId),
        /// A wire, which feeds a side input of the device it runs to.
        Wire(WireId),
        /// The metronome, and the cue bus it plays into.
        Metronome,
    }

    /// What the node with `key` belongs to, if `key` is one this module
    /// gives a node: for telling whose time each node takes.
    pub fn owner(key: NodeKey) -> Option<Owner> {
        match key {
            MASTER => return Some(Owner::Strip(Chain::Master)),
            METRONOME | CUE => return Some(Owner::Metronome),
            _ => {}
        }
        let id = key.0 as u32;
        let chain = match TrackId(id) {
            MASTER_STRIP => Chain::Master,
            track => Chain::Track(track),
        };
        let track = chain.track();
        Some(match key.0 >> 32 {
            1 | 3 | 4 | 5 | 7 | 8 | 19 => Owner::Strip(chain),
            2 | 11 | 12 => Owner::Clips(track?),
            6 => Owner::BeforeDevice(DeviceId(id)),
            9 => Owner::Send(SendId(id)),
            10 => Owner::Device(DeviceId(id)),
            13 => Owner::Modulators(chain),
            14 => Owner::Wire(WireId(id)),
            _ => return None,
        })
    }

    /// The node with the parameter at `address` in `project`, and the
    /// parameter: a channel's fader's [`Gain::GAIN`] for its volume and
    /// [`Gain::PAN`] for its pan, a send's or a wire's [`Gain::GAIN`], a
    /// device's parameter `n` as `ParamId(n)`, its Bypass as its switch's
    /// [`Switch::BYPASS`], and a modulator's parameter as its channel's
    /// [`modulation::param_id`]. A clip's parameters have none: its clip
    /// player follows the curves the clip keeps. Nor has a modulator the
    /// project hasn't.
    pub fn param(project: &Project, address: ParamAddress) -> Option<(NodeKey, ParamId)> {
        let fader = |chain| match chain {
            Chain::Track(track) => track_gain(track),
            Chain::Master => master_gain(),
        };
        Some(match address {
            ParamAddress::Volume(chain) => (fader(chain), Gain::GAIN),
            ParamAddress::Pan(chain) => (fader(chain), Gain::PAN),
            ParamAddress::Send(send) => (self::send(send), Gain::GAIN),
            // The switch's own id, which no plugin has.
            ParamAddress::Device(_, n) if ParamId(n.into()) == Switch::BYPASS => return None,
            ParamAddress::Device(device, n) => (self::device(device), ParamId(n.into())),
            ParamAddress::Bypass(device) => (self::device(device), Switch::BYPASS),
            ParamAddress::Wire(wire) => (self::wire(wire), Gain::GAIN),
            ParamAddress::Clip(..) => return None,
            ParamAddress::Modulator(modulator, n) => {
                let (chain, ..) = project.find_modulator(modulator)?;
                (self::modulation(chain), modulation::param_id(modulator, n))
            }
        })
    }

    fn track_key(role: u64, track: TrackId) -> NodeKey {
        NodeKey(role << 32 | u64::from(track.0))
    }
}

/// The fixed points in a channel strip where a native stage, such as a clip
/// type or a console stage, can sit. They belong to the channel, not to its
/// device list. In signal order:
///
/// ```text
/// source → [before device 0] → device 0 → … → [pre-fader] → fader → [post-fader] → [bus feed] → output → bus
/// ```
///
/// Pre-fader sends take the signal after the pre-fader slot, post-fader
/// sends after the post-fader slot. A channel's clip stage sits in its
/// post-fader slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// In front of the channel's device with this id. It goes with the
    /// device, along the chain or to another, so its stage stays the
    /// device's own.
    BeforeDevice(DeviceId),
    /// After the devices, in front of the fader.
    PreFader,
    /// Right after the fader.
    PostFader,
    /// Where the channel feeds its bus.
    BusFeed,
}

/// A channel strip [`compile`] builds: a track's or the master's.
#[derive(Debug, Clone, Copy)]
pub enum Channel<'a> {
    Track(&'a Track),
    Master(&'a Master),
}

impl<'a> Channel<'a> {
    pub fn clip_type(self) -> ClipType {
        match self {
            Self::Track(track) => track.clip_type,
            Self::Master(master) => master.clip_type,
        }
    }

    pub fn console(self) -> &'a ConsoleSettings {
        match self {
            Self::Track(track) => &track.console,
            Self::Master(master) => &master.console,
        }
    }

    fn devices(self) -> &'a [Device] {
        match self {
            Self::Track(track) => &track.devices,
            Self::Master(master) => &master.devices,
        }
    }

    /// The fader's gain and pan.
    fn fader(self) -> Gain {
        let (gain, pan) = match self {
            Self::Track(track) => (track.gain, track.pan),
            Self::Master(master) => (master.gain, master.pan),
        };
        Gain::new(gain).with_pan(pan)
    }

    fn slot_key(self, slot: Slot) -> NodeKey {
        match self {
            Self::Track(track) => keys::track_slot(track.id, slot),
            Self::Master(_) => keys::master_slot(slot),
        }
    }

    fn fader_key(self) -> NodeKey {
        match self {
            Self::Track(track) => keys::track_gain(track.id),
            Self::Master(_) => keys::master_gain(),
        }
    }
}

/// How a bus sums its inputs. Every bus, including the master, sums through
/// the kind the compiler picks, so console mode can swap in a modelled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summing {
    /// Plain floating-point addition.
    Plain,
}

impl Summing {
    fn node(self) -> Box<dyn Node> {
        match self {
            Self::Plain => Box::new(Sum::default()),
        }
    }
}

/// Builds the graph for `project`, with meters nobody reads and no audio
/// files. See [`compile_with_media`].
pub fn compile(project: &Project) -> Graph {
    compile_metered(project, &Meters::new())
}

/// Builds the graph for `project`, without audio files: its audio clips and
/// samplers are silent. See [`compile_with_media`].
pub fn compile_metered(project: &Project, meters: &Meters) -> Graph {
    compile_with_media(project, meters, &Media::new())
}

/// Builds the graph for `project`, without plugins: plugin devices pass
/// their signal through. See [`compile_with_devices`].
pub fn compile_with_media(project: &Project, meters: &Meters, media: &Media) -> Graph {
    compile_with_devices(project, meters, media, &Devices::new())
}

/// Builds the graph for `project`, without audio inputs: tracks that
/// monitor their inputs monitor silence. See [`compile_with_inputs`].
pub fn compile_with_devices(
    project: &Project,
    meters: &Meters,
    media: &Media,
    devices: &Devices,
) -> Graph {
    compile_with_inputs(project, meters, media, devices, &InputLayout::new())
}

/// Builds the graph for `project`. Each channel's output stage measures
/// into its meter in `meters`: call [`Meters::follow`] first, or channels
/// without a meter there get one nobody reads. Audio clips play their audio
/// from `media`, and samplers their samples, and the ones it doesn't have
/// are silent. Plugin devices process through the nodes `devices` makes for
/// them, and the ones it has none for, such as a plugin that isn't
/// installed, pass their signal through. Tracks monitor their inputs where
/// `inputs` lays them out, and the ones it doesn't have are silent.
pub fn compile_with_inputs(
    project: &Project,
    meters: &Meters,
    media: &Media,
    devices: &Devices,
    inputs: &InputLayout,
) -> Graph {
    compile_with(project, meters, media, devices, inputs, native_stage)
}

/// Like [`compile_with_inputs`], with a tap ([`Graph::add_tap`]) on each
/// track in `stems`, in that order, for an export's stems: the track as it
/// feeds its bus, after its fader and its slots, and before its output
/// stage, where mute and solo would cut it. Tracks the project hasn't got
/// have no tap, so the taps after them move up.
pub fn compile_with_stems(
    project: &Project,
    meters: &Meters,
    media: &Media,
    devices: &Devices,
    inputs: &InputLayout,
    stems: &[TrackId],
) -> Graph {
    build(project, meters, media, devices, inputs, stems, native_stage)
}

/// Where the audio inputs tracks take are among the engine's device inputs
/// ([`ProcessContext::input`](crate::ProcessContext::input)), and which of
/// the stream's MIDI inputs MIDI tracks take
/// ([`ProcessContext::midi`](crate::ProcessContext::midi)): the inputs a
/// stream opened, by their ids. Tracks monitor their inputs through it,
/// and a recorder finds them in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputLayout {
    inputs: Vec<LaidOut>,
    /// The input that tracks taking the default input take.
    default: Option<String>,
    /// The MIDI inputs' ids, by their places among the stream's
    /// ([`MidiMessage::input`](crate::MidiMessage::input)).
    midi: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaidOut {
    id: String,
    first: usize,
    channels: usize,
}

/// Where the channels an audio input takes are, in an [`InputLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputChannels {
    /// Which of the layout's inputs it is, in the order they were added.
    pub index: usize,
    /// The first channel it takes, counting the input's own from 0.
    pub channel: usize,
    /// The first channel it takes among the engine's device inputs.
    pub first: usize,
    /// How many channels it takes: one or two.
    pub count: usize,
}

/// Which messages a MIDI track's input takes, in an [`InputLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiSource {
    /// The one MIDI input, by its place among the stream's
    /// ([`MidiMessage::input`](crate::MidiMessage::input)), or `None` for
    /// every one.
    pub input: Option<usize>,
    /// The one MIDI channel, from 0 to 15, or `None` for every channel.
    pub channel: Option<u8>,
}

impl InputLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lays out the input `id`, with `channels` channels from the engine's
    /// device input channel `first`. An input that didn't open has none.
    pub fn add(&mut self, id: impl Into<String>, first: usize, channels: usize) {
        self.inputs.push(LaidOut {
            id: id.into(),
            first,
            channels,
        });
    }

    /// Has tracks that take the default input take the input `id`.
    pub fn set_default(&mut self, id: impl Into<String>) {
        self.default = Some(id.into());
    }

    /// The input that tracks taking the default input take, if there is
    /// one.
    pub fn default_input(&self) -> Option<&str> {
        self.default.as_deref()
    }

    /// The inputs' ids, in the order they were added.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.inputs.iter().map(|input| input.id.as_str())
    }

    /// Where the channels `input` takes are, or `None` if its input isn't
    /// laid out, or hasn't those channels.
    pub fn find(&self, input: &AudioInput) -> Option<InputChannels> {
        let id = match input.device.as_str() {
            "" => self.default.as_deref()?,
            id => id,
        };
        let index = self.inputs.iter().position(|laid| laid.id == id)?;
        let laid = &self.inputs[index];
        let channel = usize::from(input.channel);
        let count = usize::from(input.channels());
        (channel + count <= laid.channels).then_some(InputChannels {
            index,
            channel,
            first: laid.first + channel,
            count,
        })
    }

    /// Lays out the MIDI input `id`: the stream's messages from it have the
    /// next place, after the MIDI inputs added so far.
    pub fn add_midi(&mut self, id: impl Into<String>) {
        self.midi.push(id.into());
    }

    /// The MIDI inputs' ids, in the order they were added.
    pub fn midi_ids(&self) -> impl Iterator<Item = &str> {
        self.midi.iter().map(String::as_str)
    }

    /// Which messages `input` takes, or `None` if its MIDI input isn't laid
    /// out. Taking every MIDI input, it takes those that are.
    pub fn find_midi(&self, input: &MidiInput) -> Option<MidiSource> {
        let index = match input.device.as_str() {
            "" => None,
            id => Some(self.midi.iter().position(|midi| midi == id)?),
        };
        Some(MidiSource {
            input: index,
            channel: input.channel,
        })
    }
}

/// Makes the nodes for a device that processes outside the engine, such as
/// a plugin: every node it makes processes the same device, so a graph
/// swap carries on where the last block left off.
pub trait NodeMaker: Send + Sync {
    fn make(&self) -> Box<dyn Node>;
}

/// What makes the nodes of the devices the engine can't make itself, by
/// device: plugins now, extension devices later, and which of them render
/// live. The app fills it from the plugin instances it holds.
#[derive(Clone, Default)]
pub struct Devices {
    makers: HashMap<DeviceId, Arc<dyn NodeMaker>>,
    live: HashSet<DeviceId>,
}

impl Devices {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, device: DeviceId, maker: Arc<dyn NodeMaker>) {
        self.makers.insert(device, maker);
    }

    pub fn get(&self, device: DeviceId) -> Option<&dyn NodeMaker> {
        self.makers.get(&device).map(|maker| &**maker)
    }

    /// Has `device` render live, as the audio device asks for each block,
    /// with what it plays into, while it's in the signal: a plugin that
    /// must run in real time, or one the user has render live. See
    /// [`Graph::set_live`].
    pub fn set_live(&mut self, device: DeviceId) {
        self.live.insert(device);
    }

    /// Whether `device` renders live: see [`set_live`](Self::set_live).
    pub fn renders_live(&self, device: DeviceId) -> bool {
        self.live.contains(&device)
    }

    pub fn len(&self) -> usize {
        self.makers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.makers.is_empty()
    }
}

impl fmt::Debug for Devices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut devices: Vec<_> = self.makers.keys().collect();
        devices.sort();
        let mut live: Vec<_> = self.live.iter().collect();
        live.sort();
        f.debug_struct("Devices")
            .field("devices", &devices)
            .field("live", &live)
            .finish()
    }
}

/// Like [`compile_with_inputs`], with `stages` choosing the native stage
/// for each slot of each channel. `None` leaves the slot empty.
pub(crate) fn compile_with(
    project: &Project,
    meters: &Meters,
    media: &Media,
    devices: &Devices,
    inputs: &InputLayout,
    stages: impl FnMut(Channel<'_>, Slot) -> Option<Box<dyn Node>>,
) -> Graph {
    build(project, meters, media, devices, inputs, &[], stages)
}

/// [`compile_with`], tapping the tracks in `stems` as
/// [`compile_with_stems`] does.
fn build(
    project: &Project,
    meters: &Meters,
    media: &Media,
    devices: &Devices,
    inputs: &InputLayout,
    stems: &[TrackId],
    mut stages: impl FnMut(Channel<'_>, Slot) -> Option<Box<dyn Node>>,
) -> Graph {
    let mut graph = Graph::new();
    let master = graph.add_boxed(keys::MASTER, Summing::Plain.node());
    // The cue bus isn't part of the mix, so it sums plainly whatever the
    // summing.
    let cue = graph.add(keys::CUE, Sum::default());
    let metronome = Metronome::new().with_enabled(project.metronome);
    let metronome = graph.add(keys::METRONOME, metronome);
    connect(&mut graph, metronome, cue);

    // The buses first, so tracks anywhere in the list can feed them. A bus
    // track's test tone sums in with what feeds it.
    let mut buses = HashMap::new();
    for track in project.tracks.iter().filter(|t| t.kind.is_bus()) {
        let bus = graph.add_boxed(keys::track_bus(track.id), Summing::Plain.node());
        if let Some(tone) = track.tone {
            let sine = Sine::new(tone.frequency, tone.level);
            let sine = graph.add(keys::track_tone(track.id), sine);
            connect(&mut graph, sine, bus);
        }
        buses.insert(track.id, bus);
    }

    let silenced = project.silenced();
    let mut tapped = HashMap::new();
    // Where each tap's signal is, and the switch of each device a wire can
    // feed, for the wires and the inputs from taps, and of each a note wire
    // can play into.
    let mut points = HashMap::new();
    let mut ports = HashMap::new();
    let mut note_ports = HashMap::new();
    // The inputs from taps that play, and the audio tracks' monitors that
    // take them, and the MIDI tracks'. Each MIDI track's monitor, where
    // its notes are, for the MIDI tracks taking them.
    let live_inputs: HashMap<TrackId, Tap> = project.live_inputs().into_iter().collect();
    let mut taking = Vec::new();
    let mut taking_notes = Vec::new();
    let mut note_points = HashMap::new();
    // And the points after the devices that play notes out as they render,
    // where those notes come from as each has rendered.
    let mut made_points = HashMap::new();
    // Where each channel's notes are as they reach its first instrument,
    // for its modulators: a MIDI track's after its MIDI effects, which are
    // its Post FX's too, and the note wires' into the instrument. And each
    // first instrument's channel, for those wires.
    let mut instrument_notes: HashMap<Chain, Vec<NodeId>> = HashMap::new();
    let mut first_instruments = HashMap::new();
    for track in &project.tracks {
        let player = clip_player(track, media, &project.tempo).map(|player| {
            let player = graph.add_boxed(keys::track_clips(track.id), player);
            if let Some(tone) = track.tone {
                let sine = Sine::new(tone.frequency, tone.level);
                let sine = graph.add(keys::track_tone(track.id), sine);
                connect(&mut graph, sine, player);
            }
            player
        });
        let tap = live_inputs.get(&track.id).copied();
        let resamples = tap.is_some_and(|tap| Input::Tap(tap).is_resampling());
        let monitor = match (&track.input, player) {
            (Some(input @ (Input::Audio(_) | Input::Tap(_))), Some(player))
                if track.kind == TrackKind::Audio =>
            {
                let mut monitor = InputMonitor::new(track.monitor, track.armed);
                match (input, tap) {
                    (Input::Audio(input), _) => {
                        if let Some(channels) = inputs.find(input) {
                            monitor = monitor.with_input(channels.first, channels.count);
                        }
                    }
                    (_, Some(_)) if resamples => monitor = monitor.resampling(),
                    (_, Some(_)) => monitor = monitor.with_side_input(),
                    // An input that doesn't play is silent.
                    (_, None) => {}
                }
                Some((monitor, player))
            }
            _ => None,
        };
        // A track resampling the master cuts what comes out of its devices
        // as well as what goes into them, so what they ring on with is cut
        // too. The master feeds neither.
        let mut cut = None;
        let monitor = match monitor {
            Some((mut monitor, player)) => {
                if resamples {
                    cut = Some(monitor.cut());
                }
                let node = graph.add(keys::track_input(track.id), monitor);
                connect(&mut graph, player, node);
                if let Some(tap) = tap.filter(|_| !resamples) {
                    taking.push((tap, node));
                }
                Some(node)
            }
            None => None,
        };
        // A MIDI track's input joins its clips' notes on their way to its
        // instrument: a MIDI input's, or another track's notes, which come
        // into its monitor's side input. A track without one has a monitor
        // too, so the notes held on an input it had end when the input
        // goes.
        let midi_monitor = match (track.kind, player) {
            (TrackKind::Midi, Some(_)) => {
                let mut monitor = MidiMonitor::new(track.monitor, track.armed);
                match (&track.input, tap) {
                    (Some(Input::Midi(input)), _) => {
                        if let Some(source) = inputs.find_midi(input) {
                            monitor = monitor.with_source(source);
                        }
                    }
                    (_, Some(tap)) => monitor = monitor.with_side_input(tap),
                    (_, None) => {}
                }
                let monitor = graph.add(keys::track_input(track.id), monitor);
                if let Some(tap) = tap {
                    taking_notes.push((tap, monitor));
                }
                note_points.insert(Tap::PreFx(track.id), monitor);
                Some(monitor)
            }
            _ => None,
        };
        let head = buses.get(&track.id).copied().or(monitor).or(player);
        if let Some(head) = head {
            points.insert(Tap::PreFx(track.id), head);
        }
        // A track set to Always Live renders live from its clips, its input
        // and its bus on, and so does what they reach.
        if track.always_live {
            let heads = [player, monitor, midi_monitor, buses.get(&track.id).copied()];
            for node in heads.into_iter().flatten() {
                graph.set_live(node).expect("the nodes were just added");
            }
        }
        let mut strip = Strip {
            graph: &mut graph,
            tail: head,
        };
        let taps = strip.channel(Channel::Track(track), devices, media, &mut stages, cut);
        // The cut follows the monitor's arming a block on, so the two
        // render on the same side of the engine, live or ahead, and so
        // does everything between.
        if let (true, Some(monitor)) = (resamples, monitor) {
            let cut = strip.graph.find(keys::track_cut(track.id));
            let cut = cut.expect("a track resampling the master has its cut");
            (strip.graph.render_together(monitor, cut)).expect("the nodes were just added");
        }
        taps.record(
            Chain::Track(track.id),
            &mut points,
            &mut ports,
            &mut note_ports,
            &mut made_points,
        );
        if let Some(instrument) = taps.instrument {
            first_instruments.insert(instrument, Chain::Track(track.id));
        }
        // The notes go from the clips through the monitor and the MIDI
        // effects, each to the next, to the instrument. Other MIDI tracks
        // take them after any MIDI effect, and at Post FX, where they reach
        // the instrument.
        if let Some(player) = player.filter(|_| track.kind == TrackKind::Midi) {
            let head = midi_monitor.unwrap_or(player);
            let chain: Vec<NodeId> = (iter::once(player).chain(midi_monitor))
                .chain(taps.midi_effects.iter().copied())
                .collect();
            let notes = *chain.last().expect("the chain starts with the player");
            for (from, to) in chain.windows(2).map(|pair| (pair[0], pair[1])) {
                (strip.graph.send_notes(from, to)).expect("the nodes were just added");
            }
            if let Some(instrument) = taps.instrument {
                (strip.graph.send_notes(notes, instrument)).expect("the nodes were just added");
            }
            (instrument_notes.entry(Chain::Track(track.id)).or_default()).push(notes);
            for &(effect, after) in &taps.note_taps {
                note_points.insert(Tap::Device(effect), after.unwrap_or(head));
            }
            note_points.insert(Tap::PostFx(Chain::Track(track.id)), notes);
        }
        if stems.contains(&track.id) {
            tapped.insert(track.id, strip.tail.expect("the strip has its fader"));
        }
        // Mute and solo cut the output here, after the sends take their
        // signal, so they can cut each path on its own.
        let meter = meters.track(track.id).cloned().unwrap_or_default();
        let output = Gain::new(1.0)
            .with_muted(silenced.output(track.id))
            .with_meter(meter);
        strip.push(keys::track_output(track.id), Box::new(output));
        // A track that goes to Sends Only still meters what it would feed.
        match project.output_of(track) {
            Bus::Master => strip.feed(master),
            Bus::Group(group) => strip.feed(buses[&group]),
            Bus::SendsOnly => {}
        }
        for send in project.sends_of(track) {
            let node = Gain::new(send.level)
                .with_max_gain(1.0)
                .with_muted(silenced.send(send.id));
            let node = graph.add(keys::send(send.id), node);
            let tap = if send.pre_fader {
                taps.pre_fader
            } else {
                taps.post_fader
            };
            if let Some(tap) = tap {
                connect(&mut graph, tap, node);
            }
            connect(&mut graph, node, buses[&send.to]);
        }
    }

    let mut strip = Strip {
        graph: &mut graph,
        tail: Some(master),
    };
    let taps = strip.channel(
        Channel::Master(&project.master),
        devices,
        media,
        &mut stages,
        None,
    );
    let made = &mut made_points;
    taps.record(
        Chain::Master,
        &mut points,
        &mut ports,
        &mut note_ports,
        made,
    );
    if let Some(instrument) = taps.instrument {
        first_instruments.insert(instrument, Chain::Master);
    }
    let output = Gain::new(1.0).with_meter(meters.master().clone());
    strip.push(keys::master_output(), Box::new(output));
    let output = strip.tail.expect("the master's strip has its output stage");
    graph.set_output(output).expect("the output was just added");

    // Once every strip is built, so a wire can join any two. A wire into a
    // device with no node in the signal, such as a deactivated one, has
    // nothing to feed. A note wire plays the notes at its tap into an
    // instrument or a MIDI effect through a relay, which gives them ids of
    // their own and its channel, and a muted track's cuts them.
    let notes_at = |tap: Tap| {
        let played = note_points.get(&tap).map(|&node| (node, false));
        played.or_else(|| made_points.get(&tap).map(|&node| (node, true)))
    };
    for wire in project.live_wires() {
        if let routing::Port::Notes(device) = wire.to {
            let (Some((from, made)), Some(&to)) = (notes_at(wire.from), note_ports.get(&device))
            else {
                continue;
            };
            let relay = NoteWire::new(wire.id, wire.channel).with_muted(silenced.wire(wire.id));
            let relay = graph.add(keys::wire(wire.id), relay);
            match made {
                true => graph.send_made_notes(from, relay),
                false => graph.send_notes(from, relay),
            }
            .expect("the relay was just added");
            graph
                .send_notes(relay, to)
                .expect("the model's routing rules leave no loops");
            if let Some(&chain) = first_instruments.get(&to) {
                instrument_notes.entry(chain).or_default().push(relay);
            }
            continue;
        }
        let (Some(&from), Some(&to)) = (points.get(&wire.from), ports.get(&wire.to.device()))
        else {
            continue;
        };
        let gain = Gain::new(wire.gain).with_max_gain(routing::Wire::GAIN_MAX);
        let gain = graph.add(keys::wire(wire.id), gain);
        connect(&mut graph, from, gain);
        // A plugin's node tells its side inputs apart by the plugin's ids
        // for them.
        let port = match wire.to {
            routing::Port::Sidechain(_) => InputPort::Side(0),
            routing::Port::PluginInput(_, input) => InputPort::Side(input),
            routing::Port::Notes(_) => unreachable!("note wires play through relays"),
        };
        graph
            .connect_to(gain, to, port)
            .expect("the model's routing rules leave no loops");
    }
    // Once every strip is built too, so a track can take any other's tap.
    for &(tap, monitor) in &taking {
        if let Some(&from) = points.get(&tap) {
            graph
                .connect_to(from, monitor, InputPort::Side(0))
                .expect("the model's routing rules leave no loops");
        }
    }
    // A MIDI track takes another's notes from the node that plays them at
    // the tap: that one's monitor, on their way to its devices, or one of
    // its MIDI effects, or from a device that plays them out as it
    // renders, as it has.
    for &(tap, monitor) in &taking_notes {
        let routed = match notes_at(tap) {
            Some((from, false)) => graph.send_side_notes(from, monitor),
            Some((from, true)) => graph.send_side_made_notes(from, monitor),
            None => continue,
        };
        routed.expect("the model's routing rules leave no loops");
    }
    graph.set_cue(cue).expect("the cue bus was just added");
    for node in stems.iter().filter_map(|track| tapped.get(track)) {
        graph
            .add_tap(*node)
            .expect("the stems' nodes were just added");
    }
    // After the stems, so theirs keep their numbers. Each tap once, however
    // many tracks take it. A MIDI track's input from another's notes has a
    // note record of the notes it takes.
    let recorded = |kind| {
        (project.tracks.iter())
            .filter(move |t| t.kind == kind)
            .filter_map(|t| live_inputs.get(&t.id))
    };
    for &tap in recorded(TrackKind::Audio) {
        if let Some(&node) = points.get(&tap) {
            graph
                .add_record(keys::record(tap), node)
                .expect("the taps' nodes were just added");
        }
    }
    for &tap in recorded(TrackKind::Midi) {
        if let Some((node, _)) = notes_at(tap) {
            graph
                .add_note_record(keys::record_notes(tap), node)
                .expect("the taps' nodes were just added");
        }
    }
    modulate(project, meters, &instrument_notes, &mut graph);
    // A track set to Always Live has its modulators and macros render live
    // too, which nothing live reaches on an audio track, so its knobs
    // answer at once as well.
    for track in project.tracks.iter().filter(|track| track.always_live) {
        if let Some(modulation) = graph.find(keys::modulation(Chain::Track(track.id))) {
            graph.set_live(modulation).expect("the node was just added");
        }
    }

    // A parameter whose node isn't in the graph, such as a missing
    // plugin's, has nothing to automate.
    for lane in project.automation.iter().filter(|lane| !lane.overridden) {
        let Some((key, param)) = keys::param(project, lane.param) else {
            continue;
        };
        let scale = project.param_scale(lane.param);
        if let (Some(node), Some(automation)) =
            (graph.find(key), Automation::new(&lane.points, scale))
        {
            graph
                .automate(node, param, automation)
                .expect("the node was found");
        }
    }
    graph
}

/// A channel's [`Modulation`] being built, and where its ports go, in
/// order.
struct Plan {
    chain: Chain,
    node: Modulation,
    ports: Vec<Port>,
}

enum Port {
    /// A parameter of the node with this key.
    Param(NodeKey, ParamId),
    /// A relay of this channel's node.
    Relay(Chain, ParamId),
}

/// Gives each channel that has modulators, or parameters that modulators
/// move, its [`Modulation`] node, with the routings that play
/// ([`Project::routing_plays`]) and aren't bypassed. A routing to another
/// channel's parameter goes through that channel's node, which relays it,
/// and one to a parameter whose node isn't in the graph, such as a missing
/// plugin's, moves nothing. A MIDI track's node gets the track's notes as
/// they reach its instrument, after its MIDI effects, from the node in
/// `instrument_notes`. The nodes set the readings in `meters` of their
/// modulators' outputs and of the parameters they move.
fn modulate(
    project: &Project,
    meters: &Meters,
    instrument_notes: &HashMap<Chain, Vec<NodeId>>,
    graph: &mut Graph,
) {
    let routings: Vec<(Chain, &Routing)> = (project.modulations())
        .flat_map(|(chain, modulation)| modulation.routings.iter().map(move |r| (chain, r)))
        .filter(|&(chain, r)| !r.bypassed && project.routing_plays(chain, r))
        .collect();
    let mut plans: Vec<Plan> = (project.modulations())
        .filter(|&(chain, modulation)| {
            let moved = |&(_, r): &(Chain, &Routing)| project.param_chain(r.to) == Some(chain);
            !modulation.modulators.is_empty() || routings.iter().any(moved)
        })
        .map(|(chain, modulation)| {
            let mut node = Modulation::new(&modulation.modulators);
            for modulator in &modulation.modulators {
                for n in modulator.outputs() {
                    let output = Output::new(modulator.id, n);
                    if let Some(reading) = meters.output(output) {
                        node.read_output(output, Arc::clone(reading));
                    }
                }
            }
            let ports = Vec::new();
            Plan { chain, node, ports }
        })
        .collect();
    let plan = |plans: &[Plan], chain: Chain| {
        let at = plans.iter().position(|p| p.chain == chain);
        at.expect("every channel with modulators or moved parameters has a plan")
    };

    let mut params: Vec<ParamAddress> = routings.iter().map(|(_, r)| r.to).collect();
    params.sort_unstable();
    params.dedup();
    for param in params {
        let chain = project
            .param_chain(param)
            .expect("a routing that plays has its parameter");
        let at = plan(&plans, chain);
        let target = match param {
            ParamAddress::Modulator(modulator, n) => plans[at].node.add_param(modulator, n),
            _ => keys::param(project, param)
                .filter(|&(key, _)| graph.find(key).is_some())
                .map(|(key, id)| {
                    let port = plans[at].ports.len();
                    plans[at].ports.push(Port::Param(key, id));
                    plans[at].node.add_port(port)
                }),
        };
        let Some(target) = target else {
            continue;
        };
        // The length of the range the routings' amounts are shares of: a
        // plugin's parameters take theirs in its own units.
        let span = project.param_spec(param).map_or(1.0, |spec| {
            spec.scale.position(spec.max) - spec.scale.position(spec.min)
        });
        if let Some(reading) = meters.moved(param) {
            plans[at]
                .node
                .read_target(target, Arc::clone(reading), span);
        }
        let mut owners: Vec<Chain> = (routings.iter())
            .filter(|(_, r)| r.to == param)
            .map(|&(owner, _)| owner)
            .collect();
        owners.dedup();
        for owner in owners {
            let from = plan(&plans, owner);
            let target = match owner == chain {
                true => target,
                false => {
                    let relay = plans[at].node.add_relay(target);
                    let port = plans[from].ports.len();
                    plans[from].ports.push(Port::Relay(chain, relay));
                    plans[from].node.add_port(port)
                }
            };
            for &(_, routing) in routings
                .iter()
                .filter(|&&(c, r)| c == owner && r.to == param)
            {
                let added = plans[from].node.add_routing(routing, span, target);
                debug_assert!(added, "a routing that plays has its outputs");
            }
        }
    }

    // An Auto note trigger acts as Never where something else moves the
    // modulator's trigger or gate.
    for (chain, modulation) in project.modulations() {
        for modulator in &modulation.modulators {
            let trigger = match modulator.kind {
                ModulatorKind::Macro => continue,
                ModulatorKind::Lfo => lfo::TRIGGER,
                ModulatorKind::Envelope => envelope::GATE,
            };
            let trigger = ParamAddress::Modulator(modulator.id, trigger as u32);
            let routed = routings.iter().any(|(_, r)| r.to == trigger);
            let automated =
                (project.automation.iter()).any(|l| !l.overridden && l.param == trigger);
            if routed || automated {
                let at = plan(&plans, chain);
                plans[at].node.move_trigger(modulator.id);
            }
        }
    }

    let mut nodes = Vec::with_capacity(plans.len());
    let mut wiring = Vec::with_capacity(plans.len());
    for plan in plans {
        let node = graph.add(keys::modulation(plan.chain), plan.node);
        graph.control(node).expect("the node was just added");
        nodes.push((plan.chain, node));
        wiring.push((node, plan.ports));
    }
    let node_of = |chain: Chain| nodes.iter().find(|n| n.0 == chain).map(|n| n.1);
    for (node, ports) in wiring {
        for (n, port) in ports.into_iter().enumerate() {
            let (to, param) = match port {
                Port::Param(key, param) => (graph.find(key), param),
                Port::Relay(chain, relay) => (node_of(chain), relay),
            };
            let to = to.expect("the ports go to nodes in the graph");
            let port = graph.modulate(node, to, param);
            let port =
                port.expect("modulation flows from the master and groups to what feeds them");
            debug_assert_eq!(port, n, "the ports are numbered in order");
        }
    }
    // But for notes made as something rendered that the modulators move,
    // directly or along the way: they'd have to move it before it made
    // them, so they don't hear those, nor anything else that comes the
    // same way (`Project::unheard_notes`). Each way is judged against the
    // graph before any of them joins it, in channel order, so it's the
    // same whatever order they come in.
    let mut heard: Vec<(Chain, NodeId, NodeId)> = (instrument_notes.iter())
        .filter_map(|(&chain, notes)| Some((chain, notes, node_of(chain)?)))
        .flat_map(|(chain, notes, node)| notes.iter().map(move |&notes| (chain, notes, node)))
        .collect();
    heard.sort_unstable();
    heard.retain(|&(_, notes, node)| !graph.notes_would_loop(notes, node));
    for (_, notes, node) in heard {
        match graph.send_notes(notes, node) {
            // Two channels' modulators each moving what makes the notes
            // the other's hear.
            Ok(()) | Err(GraphError::Cycle) => {}
            Err(e) => panic!("notes go from the channels to their nodes: {e}"),
        }
    }
}

/// The native stage the model puts in `slot`: the channel's clip stage, in
/// the post-fader slot, unless its clip type is off. Console mode (Tier 2)
/// will fill slots from `console` as well.
fn native_stage(channel: Channel<'_>, slot: Slot) -> Option<Box<dyn Node>> {
    match (channel.clip_type(), slot) {
        (ClipType::Off, _) => None,
        (clip_type, Slot::PostFader) => Some(Box::new(Clipper::new(clip_type))),
        _ => None,
    }
}

/// The clip player at the head of an audio or MIDI track's strip, in a song
/// whose tempo map is `tempo`. Buses have none.
fn clip_player(track: &Track, media: &Media, tempo: &TempoMap) -> Option<Box<dyn Node>> {
    // In order of start, so the clips sum in the same order however the
    // project lists them.
    let mut clips: Vec<_> = track.clips.iter().collect();
    clips.sort_by_key(|c| (c.start, c.id));
    match track.kind {
        TrackKind::Audio => {
            let mut player = AudioClips::new();
            for clip in clips {
                let (source, renders) = (media.get(clip.id), media.renders(clip.id));
                let marks = media.marks(clip.id).map(|m| &**m);
                player.add_rendered(clip, source.cloned(), renders.cloned(), marks, tempo);
            }
            Some(Box::new(player))
        }
        TrackKind::Midi => {
            let mut player = MidiClips::new();
            for clip in clips {
                player.add(clip);
            }
            Some(Box::new(player))
        }
        TrackKind::Return | TrackKind::Group => None,
    }
}

/// A device's node, if it has one: a plugin `devices` has nothing for has
/// none, and nor has a built-in device whose DSP isn't built yet. A sampler
/// plays the sample `media` has for it.
fn device_node(device: &Device, devices: &Devices, media: &Media) -> Option<Box<dyn Node>> {
    match device.kind {
        DeviceKind::TestGain { gain } => Some(Box::new(Gain::new(gain))),
        DeviceKind::TestInstrument { level } => Some(Box::new(TestInstrument::new(level))),
        DeviceKind::Plugin(_) => devices.get(device.id).map(NodeMaker::make),
        DeviceKind::Native(ref native) => crate::devices::node(native, media.sample(device.id)),
    }
}

/// A channel strip being built, front to back.
struct Strip<'a> {
    graph: &'a mut Graph,
    /// The last node so far, which feeds the next one.
    tail: Option<NodeId>,
}

/// Where a channel's sends take its signal: the last node before the
/// fader, its Post FX, and the last one after the post-fader slot, its Post
/// Fader. `None` where the channel has no node yet. And the channel's first
/// instrument, if it has one, the MIDI effects its notes go through on the
/// way, and where its devices are, for wires.
struct Taps {
    pre_fader: Option<NodeId>,
    post_fader: Option<NodeId>,
    instrument: Option<NodeId>,
    /// The switches of the MIDI effects that play before the instrument on
    /// a MIDI track, in order.
    midi_effects: Vec<NodeId>,
    /// Where the notes are after each MIDI effect the notes reach, for the
    /// MIDI tracks taking them: the switch of the last one that plays, up
    /// to and including it, or `None` before the first.
    note_taps: Vec<(DeviceId, Option<NodeId>)>,
    /// The switch of each instrument in the signal and each MIDI effect
    /// that plays, for the note wires into them.
    note_ports: Vec<(DeviceId, NodeId)>,
    /// The switch of each device in the signal that plays notes out as it
    /// renders, for the MIDI tracks and the note wires taking them.
    makers: Vec<(DeviceId, NodeId)>,
    /// What each device puts out: its switch's output, or where it has no
    /// node in the signal, what comes into it.
    devices: Vec<(DeviceId, Option<NodeId>)>,
    /// The switch of each device in the signal.
    switches: Vec<(DeviceId, NodeId)>,
}

impl Taps {
    /// Adds where the taps of `chain`, the channel's, are to `points`, but
    /// for its Pre FX, its devices' switches to `ports`, those of its
    /// instruments and MIDI effects to `note_ports`, and those of the
    /// devices that play notes out as they render to `made_points`, at the
    /// points after them.
    fn record(
        &self,
        chain: Chain,
        points: &mut HashMap<Tap, NodeId>,
        ports: &mut HashMap<DeviceId, NodeId>,
        note_ports: &mut HashMap<DeviceId, NodeId>,
        made_points: &mut HashMap<Tap, NodeId>,
    ) {
        let taps = [
            (Tap::PostFx(chain), self.pre_fader),
            (Tap::PostFader(chain), self.post_fader),
        ];
        let devices = self.devices.iter().map(|&(id, at)| (Tap::Device(id), at));
        for (tap, at) in taps.into_iter().chain(devices) {
            if let Some(at) = at {
                points.insert(tap, at);
            }
        }
        ports.extend(self.switches.iter().copied());
        note_ports.extend(self.note_ports.iter().copied());
        let makers = self
            .makers
            .iter()
            .map(|&(id, node)| (Tap::Device(id), node));
        made_points.extend(makers);
    }
}

impl Strip<'_> {
    fn push(&mut self, key: NodeKey, node: Box<dyn Node>) -> NodeId {
        let id = self.graph.add_boxed(key, node);
        if let Some(tail) = self.tail {
            connect(self.graph, tail, id);
        }
        self.tail = Some(id);
        id
    }

    /// Adds `channel`'s devices, each in its [`Switch`], its fader and the
    /// stages `stages` puts in its slots, up to its output stage. A device
    /// without a node leaves the signal as it is, with the slot in front of
    /// it still there, and so does a deactivated one, whose switch is in
    /// the graph but not in the strip. An instrument without a node, or
    /// deactivated, still takes the notes, so they don't go to an
    /// instrument after it. A MIDI effect's [`NoteSwitch`] is in the graph
    /// but out of the signal, and in the notes' way on a MIDI track, before
    /// the instrument: notes pass by one without a node, or deactivated.
    /// One that no notes reach, after the instrument or on a channel
    /// without notes, is left out of their way, as if deactivated. A device
    /// in the signal that plays notes out as it renders, such as a plugin
    /// with a note output, is a maker, for what takes those notes. A track
    /// resampling the master has its `cut` after its devices.
    fn channel(
        &mut self,
        channel: Channel<'_>,
        devices: &Devices,
        media: &Media,
        stages: &mut impl FnMut(Channel<'_>, Slot) -> Option<Box<dyn Node>>,
        cut: Option<ResamplingCut>,
    ) -> Taps {
        let mut slot = |strip: &mut Self, at: Slot| {
            if let Some(stage) = stages(channel, at) {
                strip.push(channel.slot_key(at), stage);
            }
        };
        let mut instrument = None;
        let mut midi_effects = Vec::new();
        let mut note_taps = Vec::new();
        let mut note_ports = Vec::new();
        let mut makers = Vec::new();
        let notes = matches!(channel, Channel::Track(track) if track.kind == TrackKind::Midi);
        let (mut device_taps, mut switches) = (Vec::new(), Vec::new());
        for device in channel.devices() {
            slot(self, Slot::BeforeDevice(device.id));
            let key = keys::device(device.id);
            if device.kind.is_midi_effect() {
                // No notes reach one after the instrument, or where there
                // are none.
                let reached = notes && instrument.is_none();
                match device_node(device, devices, media) {
                    Some(node) if !device.active || !reached => {
                        self.graph.add(key, NoteSwitch::deactivated(node));
                    }
                    Some(node) => {
                        let node = self.graph.add(key, NoteSwitch::new(node, device.bypassed));
                        (self.graph.plays_notes(node)).expect("the node was just added");
                        if devices.renders_live(device.id) {
                            (self.graph.set_live(node)).expect("the node was just added");
                        }
                        midi_effects.push(node);
                        note_ports.push((device.id, node));
                    }
                    None => {}
                }
                if reached {
                    note_taps.push((device.id, midi_effects.last().copied()));
                }
                device_taps.push((device.id, self.tail));
                continue;
            }
            let node = match device_node(device, devices, media) {
                Some(node) if !device.active => {
                    self.graph.add(key, Switch::deactivated(node));
                    None
                }
                Some(node) => Some(self.push(key, Box::new(Switch::new(node, device.bypassed)))),
                None => None,
            };
            if let Some(node) = node {
                if devices.renders_live(device.id) {
                    (self.graph.set_live(node)).expect("the node was just added");
                }
                switches.push((device.id, node));
                if device.kind.is_instrument() {
                    note_ports.push((device.id, node));
                }
                if device.kind.makes_notes() {
                    makers.push((device.id, node));
                }
            }
            device_taps.push((device.id, self.tail));
            if device.kind.is_instrument() && instrument.is_none() {
                instrument = Some(node);
            }
        }
        if let (Some(cut), Channel::Track(track)) = (cut, channel) {
            self.push(keys::track_cut(track.id), Box::new(cut));
        }
        slot(self, Slot::PreFader);
        let pre_fader = self.tail;
        self.push(channel.fader_key(), Box::new(channel.fader()));
        slot(self, Slot::PostFader);
        let post_fader = self.tail;
        slot(self, Slot::BusFeed);
        Taps {
            pre_fader,
            post_fader,
            instrument: instrument.flatten(),
            midi_effects,
            note_taps,
            note_ports,
            makers,
            devices: device_taps,
            switches,
        }
    }

    /// Feeds the strip into `bus`.
    fn feed(self, bus: NodeId) {
        if let Some(tail) = self.tail {
            connect(self.graph, tail, bus);
        }
    }
}

fn connect(graph: &mut Graph, from: NodeId, to: NodeId) {
    graph
        .connect(from, to)
        .expect("the model's routing rules leave no loops");
}

/// Builds and prepares graphs on a thread of its own, so a node that's slow
/// to [prepare](Node::prepare), such as a plugin loading, never holds up the
/// thread that asked for it.
///
/// Each job is a closure that builds a graph, usually [`compile`] on a
/// snapshot of the project, and the [`EngineConfig`] to prepare it for. When
/// several jobs are waiting, the compiler runs only the newest, so results
/// can lag edits but never pile up.
pub struct Compiler {
    jobs: Option<Sender<Job>>,
    results: Receiver<Compiled>,
    thread: Option<JoinHandle<()>>,
    /// The number of the last job queued.
    last_job: u64,
    /// The number of the last job done.
    last_done: u64,
}

struct Job {
    number: u64,
    config: EngineConfig,
    /// Prepare the graph whole ([`Graph::prepare_whole`]).
    whole: bool,
    build: Box<dyn FnOnce() -> Graph + Send>,
}

/// A job the [`Compiler`] has done.
#[derive(Debug)]
pub struct Compiled {
    /// The job's number, from [`Compiler::compile`]. The jobs before it that
    /// it overtook have no results.
    pub job: u64,
    /// The graph, prepared for the job's config.
    pub graph: Result<PreparedGraph, GraphError>,
}

impl Compiler {
    /// Starts the compiler's thread, `musicable-compiler`.
    ///
    /// # Panics
    ///
    /// If the operating system can't start a thread.
    pub fn spawn() -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (result_tx, results) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("musicable-compiler".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    let graph = (job.build)();
                    let graph = match job.whole {
                        true => graph.prepare_whole(&job.config),
                        false => graph.prepare(&job.config),
                    };
                    let compiled = Compiled {
                        job: job.number,
                        graph,
                    };
                    if result_tx.send(compiled).is_err() {
                        break;
                    }
                }
            })
            .expect("failed to spawn the compiler thread");
        Self {
            jobs: Some(jobs),
            results,
            thread: Some(thread),
            last_job: 0,
            last_done: 0,
        }
    }

    /// Queues a job: `build` runs on the compiler's thread, and the graph it
    /// builds is prepared for `config`. Returns the job's number. Jobs are
    /// numbered from 1, in the order they're queued.
    pub fn compile(
        &mut self,
        config: EngineConfig,
        build: impl FnOnce() -> Graph + Send + 'static,
    ) -> u64 {
        self.queue(config, false, build)
    }

    /// Like [`compile`](Self::compile), preparing the graph whole
    /// ([`Graph::prepare_whole`]): for an engine that renders nothing
    /// ahead.
    pub fn compile_whole(
        &mut self,
        config: EngineConfig,
        build: impl FnOnce() -> Graph + Send + 'static,
    ) -> u64 {
        self.queue(config, true, build)
    }

    fn queue(
        &mut self,
        config: EngineConfig,
        whole: bool,
        build: impl FnOnce() -> Graph + Send + 'static,
    ) -> u64 {
        self.last_job += 1;
        let job = Job {
            number: self.last_job,
            config,
            whole,
            build: Box::new(build),
        };
        if let Some(jobs) = &self.jobs {
            // Only fails if the thread is gone, which `try_recv` and `recv`
            // report.
            let _ = jobs.send(job);
        }
        self.last_job
    }

    /// Queues a job to compile `project`, as it is now, for `config`.
    pub fn compile_project(&mut self, config: EngineConfig, project: &Project) -> u64 {
        let project = project.clone();
        self.compile(config, move || compile(&project))
    }

    /// The next job done, if there is one.
    ///
    /// # Panics
    ///
    /// If a job panicked on the compiler's thread, with that panic.
    pub fn try_recv(&mut self) -> Option<Compiled> {
        match self.results.try_recv() {
            Ok(compiled) => Some(self.done(compiled)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => self.failed(),
        }
    }

    /// Waits for the next job to be done.
    ///
    /// # Panics
    ///
    /// If a job panicked on the compiler's thread, with that panic, or if
    /// there's no job to wait for.
    pub fn recv(&mut self) -> Compiled {
        // The newest job always runs, so once it's done, nothing more comes.
        assert!(self.last_done < self.last_job, "there's no job to wait for");
        match self.results.recv() {
            Ok(compiled) => self.done(compiled),
            Err(_) => self.failed(),
        }
    }

    fn done(&mut self, compiled: Compiled) -> Compiled {
        self.last_done = compiled.job;
        compiled
    }

    /// Hands on the panic that stopped the compiler's thread.
    #[cold]
    fn failed(&mut self) -> ! {
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            std::panic::resume_unwind(panic);
        }
        panic!("the compiler's thread has stopped");
    }
}

impl Drop for Compiler {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            // A panic in a job nobody waited for goes with it.
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for Compiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compiler")
            .field("last_job", &self.last_job)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::any::{Any, TypeId};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU8, AtomicU16, AtomicUsize, Ordering};

    use musicable_model::automation::{Lane, Point};
    use musicable_model::clip::ClipContent;
    use musicable_model::modulation::{ModulatorId, Output};
    use musicable_model::native::{NativeDevice, NativeKind, compressor};
    use musicable_model::plugin::PluginKind;
    use musicable_model::routing::{Port, WireId};
    use musicable_model::time::{BeatRange, Beats};
    use musicable_model::{DeviceId, Monitor, SendId, TestTone, TrackId, TrackKind};

    use super::*;
    use crate::modulation;
    use crate::nodes::{Delay, Impulse, Latent};
    use crate::{
        AudioMut, Block, Engine, Event, Inputs, MidiMessage, NodeEvent, NodeEventKind, NoteEvent,
        ProcessContext, ReferenceRenderer, Renderer, TransportCommand, TransportState,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 128,
    };

    fn project() -> Project {
        let mut p = Project::new();
        let drums = p.add_track("Drums", TrackKind::Audio);
        p.track_mut(drums).unwrap().tone = Some(TestTone {
            frequency: 220.0,
            level: 0.5,
        });
        let bass = p.add_track("Bass", TrackKind::Midi);
        p.track_mut(bass).unwrap().tone = Some(TestTone {
            frequency: 55.0,
            level: 0.5,
        });
        p.add_track("Reverb", TrackKind::Return);
        p
    }

    /// Renders `frames` more frames of interleaved stereo, in uneven device
    /// callbacks.
    fn play(engine: &mut Engine, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; 2 * frames];
        let mut done = 0;
        for size in [441, 64, 7].into_iter().cycle() {
            let size = size.min(frames - done);
            engine.process_interleaved(&mut out[2 * done..2 * (done + size)], 2);
            done += size;
            if done == frames {
                break;
            }
        }
        out
    }

    /// Plays `graph` from the start.
    fn render(graph: PreparedGraph, frames: usize) -> Vec<f32> {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        play(&mut engine, frames)
    }

    /// Each node's key, type and input keys, in key order, and the output
    /// and cue nodes' keys.
    #[allow(clippy::type_complexity)]
    fn shape(
        graph: &PreparedGraph,
    ) -> (
        Vec<(NodeKey, TypeId, Vec<NodeKey>)>,
        NodeKey,
        Option<NodeKey>,
    ) {
        let key = |id: NodeId| graph.by_key.iter().find(|&&(_, n)| n == id).unwrap().0;
        let nodes = graph
            .by_key
            .iter()
            .map(|&(k, id)| {
                let node: &dyn Node = graph.nodes[id.0].as_ref();
                let node: &dyn Any = node;
                let inputs = graph.inputs[id.0].iter().map(|&i| key(i)).collect();
                (k, node.type_id(), inputs)
            })
            .collect();
        (nodes, graph.output.map(key).unwrap(), graph.cue.map(key))
    }

    fn no_stages(_: Channel<'_>, _: Slot) -> Option<Box<dyn Node>> {
        None
    }

    /// Halves the drums after their fader: a test stage.
    fn half_drums(channel: Channel<'_>, slot: Slot) -> Option<Box<dyn Node>> {
        let drums = matches!(channel, Channel::Track(track) if track.name == "Drums");
        (drums && slot == Slot::PostFader).then(|| Box::new(Gain::new(0.5)) as Box<dyn Node>)
    }

    /// Counts the frames it has rendered: state a swap must carry over.
    struct Ramp(f32);

    impl Node for Ramp {
        fn channels(&self) -> usize {
            1
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<Ramp>() {
                self.0 = previous.0;
            }
        }

        fn process(
            &mut self,
            _ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            for sample in output.channel_mut(0) {
                *sample = self.0;
                self.0 += 1.0;
            }
        }
    }

    #[test]
    fn tracks_get_stable_keys() {
        let p = project();
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        // The master bus, the cue bus, the metronome, the reverb's bus, a
        // fader and an output stage for each track, clip players, tones and
        // input monitors for the drums and the bass, and the master's fader
        // and output stage.
        assert_eq!(graph.len(), 18);
        for key in [
            keys::MASTER,
            keys::CUE,
            keys::METRONOME,
            keys::master_gain(),
            keys::master_output(),
        ] {
            assert!(graph.contains(key), "{key:?}");
        }
        for track in &p.tracks {
            assert!(graph.contains(keys::track_gain(track.id)));
            assert!(graph.contains(keys::track_output(track.id)));
            assert_eq!(
                graph.contains(keys::track_tone(track.id)),
                track.tone.is_some()
            );
            assert_eq!(
                graph.contains(keys::track_bus(track.id)),
                track.kind.is_bus()
            );
            assert_eq!(
                graph.contains(keys::track_clips(track.id)),
                !track.kind.is_bus()
            );
            assert_eq!(
                graph.contains(keys::track_input(track.id)),
                !track.kind.is_bus()
            );
        }
        assert_eq!(shape(&graph).1, keys::master_output());
    }

    #[test]
    fn every_node_has_its_own_key() {
        let slots = [Slot::PreFader, Slot::PostFader, Slot::BusFeed];
        let mut seen = BTreeSet::from([
            keys::MASTER,
            keys::CUE,
            keys::METRONOME,
            keys::master_gain(),
            keys::master_output(),
        ]);
        seen.extend(slots.map(keys::master_slot));
        assert_eq!(seen.len(), 8);
        // The last id a project hands out is one below `u32::MAX`.
        for id in [0, 1, u32::MAX - 1] {
            let track = TrackId(id);
            let strip = slots.map(|slot| keys::track_slot(track, slot));
            let roles = [
                keys::track_gain(track),
                keys::track_tone(track),
                keys::track_output(track),
                keys::track_bus(track),
                keys::track_clips(track),
                keys::track_input(track),
            ];
            // The slot in front of a device is the device's, whichever
            // chain it's on.
            let before = Slot::BeforeDevice(DeviceId(id));
            let in_front = keys::track_slot(track, before);
            assert_eq!(keys::track_slot(TrackId(7), before), in_front);
            assert_eq!(keys::master_slot(before), in_front);
            let others = [keys::send(SendId(id)), keys::device(DeviceId(id)), in_front];
            for key in strip.into_iter().chain(roles).chain(others) {
                assert!(seen.insert(key), "{key:?} is taken");
            }
        }
    }

    #[test]
    fn keys_say_what_their_nodes_belong_to() {
        use keys::Owner;
        let (track, device) = (TrackId(3), DeviceId(4));
        let on = Chain::Track(track);
        let owned = [
            (keys::MASTER, Owner::Strip(Chain::Master)),
            (keys::METRONOME, Owner::Metronome),
            (keys::CUE, Owner::Metronome),
            (keys::master_gain(), Owner::Strip(Chain::Master)),
            (keys::master_output(), Owner::Strip(Chain::Master)),
            (
                keys::master_slot(Slot::PostFader),
                Owner::Strip(Chain::Master),
            ),
            (
                keys::modulation(Chain::Master),
                Owner::Modulators(Chain::Master),
            ),
            (keys::track_gain(track), Owner::Strip(on)),
            (keys::track_slot(track, Slot::PreFader), Owner::Strip(on)),
            (keys::track_slot(track, Slot::BusFeed), Owner::Strip(on)),
            (keys::track_output(track), Owner::Strip(on)),
            (keys::track_bus(track), Owner::Strip(on)),
            (keys::track_cut(track), Owner::Strip(on)),
            (keys::track_tone(track), Owner::Clips(track)),
            (keys::track_clips(track), Owner::Clips(track)),
            (keys::track_input(track), Owner::Clips(track)),
            (keys::modulation(on), Owner::Modulators(on)),
            (keys::device(device), Owner::Device(device)),
            (
                keys::track_slot(track, Slot::BeforeDevice(device)),
                Owner::BeforeDevice(device),
            ),
            (keys::send(SendId(5)), Owner::Send(SendId(5))),
            (keys::wire(WireId(6)), Owner::Wire(WireId(6))),
        ];
        for (key, owner) in owned {
            assert_eq!(keys::owner(key), Some(owner), "{key:?}");
        }
        // Record taps have keys no node has.
        let tap = musicable_model::routing::Tap::PreFx(track);
        assert_eq!(keys::owner(keys::record(tap)), None);
        assert_eq!(keys::owner(keys::record_notes(tap)), None);
        assert_ne!(keys::record(tap), keys::record_notes(tap));
    }

    #[test]
    fn tier_0_projects_sound_as_they_did() {
        let mut p = project();
        p.metronome = true;
        p.tracks[0].gain = 0.8;
        p.tracks[1].muted = true;
        p.tracks[2].tone = Some(TestTone {
            frequency: 330.0,
            level: 0.25,
        });
        // What compile() built before the mixer: each track's tone feeds its
        // gain stage, which feeds the master, and the metronome feeds the
        // cue bus.
        let mut before = Graph::new();
        let master = before.add(keys::MASTER, Sum::default());
        let cue = before.add(keys::CUE, Sum::default());
        let metronome = before.add(keys::METRONOME, Metronome::new());
        before.connect(metronome, cue).unwrap();
        before.set_cue(cue).unwrap();
        for track in &p.tracks {
            let gain = Gain::new(track.gain).with_muted(track.muted);
            let gain = before.add(keys::track_gain(track.id), gain);
            if let Some(tone) = track.tone {
                let sine = Sine::new(tone.frequency, tone.level);
                let source = before.add(keys::track_tone(track.id), sine);
                before.connect(source, gain).unwrap();
            }
            before.connect(gain, master).unwrap();
        }
        before.set_output(master).unwrap();
        let before = before.prepare(&CONFIG).unwrap();

        // With the mixer at its defaults, every stage it adds passes the
        // signal through unchanged, so the samples are the same.
        let now = compile(&p).prepare(&CONFIG).unwrap();
        assert!(now.len() > before.len());
        assert!(render(now, 30_000) == render(before, 30_000));
    }

    #[test]
    fn empty_slots_add_no_nodes() {
        let p = project();
        let shape_with = |stages| {
            shape(
                &compile_with(
                    &p,
                    &Meters::new(),
                    &Media::new(),
                    &Devices::new(),
                    &InputLayout::new(),
                    stages,
                )
                .prepare(&CONFIG)
                .unwrap(),
            )
        };
        let all_off = shape(&compile(&p).prepare(&CONFIG).unwrap());
        assert_eq!(shape_with(no_stages), all_off);
    }

    #[test]
    fn a_slot_switches_on_and_off_during_playback_without_a_glitch() {
        let p = project();
        let meters = Meters::new();
        let off = || {
            compile_with(
                &p,
                &meters,
                &Media::new(),
                &Devices::new(),
                &InputLayout::new(),
                no_stages,
            )
            .prepare(&CONFIG)
            .unwrap()
        };
        let on = || {
            compile_with(
                &p,
                &meters,
                &Media::new(),
                &Devices::new(),
                &InputLayout::new(),
                half_drums,
            )
            .prepare(&CONFIG)
            .unwrap()
        };
        assert_eq!(on().len(), off().len() + 1);
        assert!(on().contains(keys::track_slot(p.tracks[0].id, Slot::PostFader)));

        // Off, on and off again, swapping between device callbacks.
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(off()).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        let mut out = play(&mut engine, 1000);
        handle.swap_graph(on()).unwrap();
        out.extend(play(&mut engine, 1000));
        handle.swap_graph(off()).unwrap();
        out.extend(play(&mut engine, 1000));

        // Each part is exactly what its graph would have played there had it
        // been running all along. Frame 1000 starts at sample 2000.
        let always_off = render(off(), 3000);
        let always_on = render(on(), 3000);
        assert!(out[..2000] == always_off[..2000]);
        assert!(out[2000..4000] == always_on[2000..4000]);
        assert!(out[4000..] == always_off[4000..]);
        assert!(always_on[2000..4000] != always_off[2000..4000]);
    }

    #[test]
    fn stages_keep_their_state_across_swaps() {
        let mut p = Project::new();
        p.add_track("Ramp", TrackKind::Audio);
        let ramp_in = |at: Slot| {
            move |channel: Channel<'_>, slot: Slot| {
                let track = matches!(channel, Channel::Track(_));
                (track && slot == at).then(|| Box::new(Ramp(0.0)) as Box<dyn Node>)
            }
        };
        let meters = Meters::new();
        let graph = |at: Slot| {
            compile_with(
                &p,
                &meters,
                &Media::new(),
                &Devices::new(),
                &InputLayout::new(),
                ramp_in(at),
            )
            .prepare(&CONFIG)
            .unwrap()
        };
        let left = |out: Vec<f32>| -> Vec<f32> { out.into_iter().step_by(2).collect() };
        let count = |from: u16| -> Vec<f32> { (from..from + 100).map(f32::from).collect() };

        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph(Slot::BusFeed)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(0));
        // Recompiled, the stage in the same slot carries on.
        handle.swap_graph(graph(Slot::BusFeed)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(100));
        // In another slot it's another node, so it starts again.
        handle.swap_graph(graph(Slot::PreFader)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(0));
    }

    #[test]
    fn the_stage_in_front_of_a_device_goes_with_it() {
        let mut p = Project::new();
        let first = p.add_track("First", TrackKind::Audio);
        let second = p.add_track("Second", TrackKind::Audio);
        let gain = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let device = gain.id;
        p.track_mut(first).unwrap().devices.push(gain);
        let meters = Meters::new();
        let graph = |p: &Project| {
            let ramp = |_: Channel<'_>, slot: Slot| {
                (slot == Slot::BeforeDevice(device)).then(|| Box::new(Ramp(0.0)) as Box<dyn Node>)
            };
            let devices = Devices::new();
            compile_with(
                p,
                &meters,
                &Media::new(),
                &devices,
                &InputLayout::new(),
                ramp,
            )
            .prepare(&CONFIG)
            .unwrap()
        };
        let left = |out: Vec<f32>| -> Vec<f32> { out.into_iter().step_by(2).collect() };
        let count = |from: u16| -> Vec<f32> { (from..from + 100).map(f32::from).collect() };

        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph(&p)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(0));
        // Moved to the other track, the device takes the stage in front of
        // it along, and it carries on, as a console stage would.
        let moved = p.track_mut(first).unwrap().devices.remove(0);
        p.track_mut(second).unwrap().devices.push(moved);
        handle.swap_graph(graph(&p)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(100));
    }

    #[test]
    fn tracks_line_up_at_the_master_whatever_their_latency() {
        // Two tracks play an impulse on frame 100, one of them through a
        // stage with 37 frames of latency and then a group. Both send to a
        // return track.
        let mut p = Project::new();
        let late = p.add_track("Late", TrackKind::Audio);
        let on_time = p.add_track("On time", TrackKind::Audio);
        let group = p.add_track("Group", TrackKind::Group);
        let reverb = p.add_track("Reverb", TrackKind::Return);
        p.track_mut(late).unwrap().output = Bus::Group(group);
        for (track, level) in [(late, 0.25), (on_time, 0.5)] {
            let mut send = p.new_send(reverb);
            send.level = level;
            p.track_mut(track).unwrap().sends.push(send);
        }
        let stages = |channel: Channel<'_>, slot: Slot| {
            let Channel::Track(track) = channel else {
                return None;
            };
            let stage: Box<dyn Node> = match (track.name.as_str(), slot) {
                ("Late" | "On time", Slot::PreFader) => Box::new(Impulse { at: 100 }),
                ("Late", Slot::PostFader) => Box::new(Latent(Delay::new(37))),
                _ => return None,
            };
            Some(stage)
        };
        let graph = compile_with(
            &p,
            &Meters::new(),
            &Media::new(),
            &Devices::new(),
            &InputLayout::new(),
            stages,
        )
        .prepare(&CONFIG)
        .unwrap();
        assert_eq!(graph.latency(), 37);
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        let out = play(&mut engine, 300);
        assert_eq!(handle.stats().latency, 37);
        // Each track once, and the sends at their levels through the
        // return, all on the same frame, in both channels.
        let loud: Vec<_> = out.iter().enumerate().filter(|(_, s)| **s != 0.0).collect();
        assert_eq!(loud, [(2 * 137, &2.75), (2 * 137 + 1, &2.75)]);
    }

    /// Makes nodes that halve the signal, standing in for a plugin.
    struct Halves;

    impl NodeMaker for Halves {
        fn make(&self) -> Box<dyn Node> {
            Box::new(Gain::new(0.5))
        }
    }

    /// Makes test instruments, standing in for a plugin that plays notes.
    struct Synth;

    impl NodeMaker for Synth {
        fn make(&self) -> Box<dyn Node> {
            Box::new(TestInstrument::new(0.5))
        }
    }

    fn plugin(instrument: bool) -> DeviceKind {
        DeviceKind::Plugin(musicable_model::plugin::PluginDevice {
            format: musicable_model::PluginFormat::Clap,
            id: "org.example.plugin".into(),
            name: "Plugin".into(),
            kind: match instrument {
                true => PluginKind::Instrument,
                false => PluginKind::AudioEffect,
            },
            inputs: Vec::new(),
            note_output: false,
            settings: Default::default(),
        })
    }

    #[test]
    fn plugins_play_through_the_nodes_the_app_makes_and_missing_ones_pass_through() {
        let mut p = Project::new();
        let track = p.add_track("Tone", TrackKind::Audio);
        let device = p.new_device(plugin(false));
        let id = device.id;
        let t = p.track_mut(track).unwrap();
        t.tone = Some(TestTone {
            frequency: 440.0,
            level: 0.5,
        });
        t.devices.push(device);
        let mut devices = Devices::new();
        devices.insert(id, Arc::new(Halves));
        let with = |devices: &Devices| {
            compile_with_devices(&p, &Meters::new(), &Media::new(), devices)
                .prepare(&CONFIG)
                .unwrap()
        };
        assert!(with(&devices).contains(keys::device(id)));
        assert!(!with(&Devices::new()).contains(keys::device(id)));

        let missing = render(with(&Devices::new()), 256);
        let halved = render(with(&devices), 256);
        assert!(missing.iter().any(|&s| s != 0.0));
        let half: Vec<f32> = missing.iter().map(|s| s * 0.5).collect();
        assert!(halved == half);
    }

    #[test]
    fn a_midi_tracks_notes_go_to_a_plugin_that_plays_them() {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 69, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Note", range, ClipContent::Midi(notes));
        // An effect first, which notes pass by.
        let effect = p.new_device(plugin(false));
        let synth = p.new_device(plugin(true));
        let mut devices = Devices::new();
        devices.insert(effect.id, Arc::new(Halves));
        devices.insert(synth.id, Arc::new(Synth));
        let t = p.track_mut(keys).unwrap();
        t.clips.push(clip);
        t.devices = vec![effect, synth];
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices)
            .prepare(&CONFIG)
            .unwrap();
        assert!(render(graph, 1000).iter().any(|&s| s != 0.0));

        // Without the synth's node, nothing plays the notes: not even an
        // instrument after it.
        devices = Devices::new();
        devices.insert(p.tracks[0].devices[0].id, Arc::new(Halves));
        let after = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        p.tracks[0].devices.push(after);
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices)
            .prepare(&CONFIG)
            .unwrap();
        assert!(render(graph, 1000).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_deactivated_instrument_still_takes_the_notes() {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 69, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Note", range, ClipContent::Midi(notes));
        let mut synth = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        synth.active = false;
        let after = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let t = p.track_mut(keys).unwrap();
        t.clips.push(clip);
        t.devices = vec![synth, after];
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(render(graph, 1000).iter().all(|&s| s == 0.0));
    }

    /// Stands in for a MIDI effect plugin: makes nodes that play the notes
    /// they get on, `up` keys up, or none of them, and keep the keys of
    /// those they get.
    struct Transposes {
        up: Option<u8>,
        heard: Arc<Heard>,
    }

    impl Transposes {
        fn new(up: Option<u8>) -> Arc<Self> {
            let heard = Arc::new(Heard::default());
            Arc::new(Self { up, heard })
        }

        fn heard(&self) -> Vec<u8> {
            let count = self.heard.count.load(Ordering::Relaxed);
            let keys = &self.heard.keys[..count.min(self.heard.keys.len())];
            keys.iter().map(|key| key.load(Ordering::Relaxed)).collect()
        }
    }

    /// The first keys a [`Transposer`] heard, and how many it heard.
    #[derive(Default)]
    struct Heard {
        keys: [AtomicU8; 8],
        count: AtomicUsize,
    }

    impl NodeMaker for Transposes {
        fn make(&self) -> Box<dyn Node> {
            Box::new(Transposer {
                up: self.up,
                heard: Arc::clone(&self.heard),
            })
        }
    }

    struct Transposer {
        up: Option<u8>,
        heard: Arc<Heard>,
    }

    impl Node for Transposer {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut crate::Notes<'_>) {
            for event in ctx.events {
                let NodeEventKind::Note(mut note) = event.kind else {
                    continue;
                };
                if let NoteEvent::On { key, .. } = &mut note {
                    let heard = self.heard.count.fetch_add(1, Ordering::Relaxed);
                    if let Some(slot) = self.heard.keys.get(heard) {
                        slot.store(*key, Ordering::Relaxed);
                    }
                    *key += self.up.unwrap_or(0);
                }
                if self.up.is_some() {
                    notes.send(event.offset, note);
                }
            }
        }
    }

    fn midi_effect() -> DeviceKind {
        DeviceKind::Plugin(musicable_model::plugin::PluginDevice {
            format: musicable_model::PluginFormat::Clap,
            id: "org.example.arpeggiator".into(),
            name: "Arpeggiator".into(),
            kind: PluginKind::MidiEffect,
            inputs: Vec::new(),
            note_output: false,
            settings: Default::default(),
        })
    }

    /// A MIDI track playing a beat-long note, key 69, through devices of
    /// `kinds`, in order.
    fn note_through(kinds: Vec<DeviceKind>) -> Project {
        let mut p = Project::new();
        let keys = p.add_track("Keys", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 69, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Note", range, ClipContent::Midi(notes));
        let devices = kinds.into_iter().map(|kind| p.new_device(kind)).collect();
        let t = p.track_mut(keys).unwrap();
        t.clips.push(clip);
        t.devices = devices;
        p
    }

    /// Renders 1000 frames of `p`, with the first track's device at `at`
    /// played by `maker`, and says whether they're silent.
    fn silent_with(p: &Project, at: usize, maker: Arc<Transposes>) -> bool {
        let mut devices = Devices::new();
        devices.insert(p.tracks[0].devices[at].id, maker);
        let graph = compile_with_devices(p, &Meters::new(), &Media::new(), &devices);
        let out = render(graph.prepare(&CONFIG).unwrap(), 1000);
        out.iter().all(|&s| s == 0.0)
    }

    #[test]
    fn midi_effects_play_the_notes_on_to_the_instrument_each_to_the_next() {
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let gain = DeviceKind::TestGain { gain: 0.5 };
        let p = note_through(vec![midi_effect(), gain, midi_effect(), synth]);
        let (octave, fifth) = (Transposes::new(Some(12)), Transposes::new(Some(7)));
        let mut devices = Devices::new();
        devices.insert(p.tracks[0].devices[0].id, octave.clone());
        devices.insert(p.tracks[0].devices[2].id, fifth.clone());
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        let graph = graph.prepare(&CONFIG).unwrap();
        // Each is its own node, out of the signal.
        for device in &p.tracks[0].devices[..3] {
            assert!(graph.contains(keys::device(device.id)));
        }
        assert!(render(graph, 1000).iter().any(|&s| s != 0.0));
        assert_eq!((octave.heard(), fifth.heard()), (vec![69], vec![81]));

        // One that plays none on leaves the synth silent.
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let mut p = note_through(vec![midi_effect(), synth]);
        let swallows = Transposes::new(None);
        assert!(silent_with(&p, 0, swallows.clone()));
        assert_eq!(swallows.heard(), [69]);
        // Bypassed, it passes the notes by, and deactivated, they go round
        // it: either way, it hears none.
        p.tracks[0].devices[0].bypassed = true;
        let swallows = Transposes::new(None);
        assert!(!silent_with(&p, 0, swallows.clone()));
        p.tracks[0].devices[0].bypassed = false;
        p.tracks[0].devices[0].active = false;
        assert!(!silent_with(&p, 0, swallows.clone()));
        assert!(swallows.heard().is_empty());
        // Without its plugin, the notes pass it by too.
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(render(graph, 1000).iter().any(|&s| s != 0.0));
    }

    #[test]
    fn a_midi_effect_no_notes_reach_plays_nothing() {
        // After the instrument.
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let p = note_through(vec![synth, midi_effect()]);
        let swallows = Transposes::new(None);
        assert!(!silent_with(&p, 1, swallows.clone()));
        // On an audio track, whose sound passes it by.
        let mut p = Project::new();
        let tone = p.add_track("Tone", TrackKind::Audio);
        let effect = p.new_device(midi_effect());
        let t = p.track_mut(tone).unwrap();
        t.tone = Some(TestTone {
            frequency: 440.0,
            level: 0.5,
        });
        t.devices.push(effect);
        assert!(!silent_with(&p, 0, swallows.clone()));
        assert!(swallows.heard().is_empty());
        // On a MIDI track without an instrument, it plays the notes, for
        // nothing to hear.
        let p = note_through(vec![midi_effect()]);
        let plays = Transposes::new(Some(12));
        assert!(silent_with(&p, 0, plays.clone()));
        assert_eq!(plays.heard(), [69]);
    }

    #[test]
    fn a_midi_tracks_modulators_get_the_notes_its_instrument_does() {
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let mut p = note_through(vec![midi_effect(), synth]);
        let envelope = p.new_modulator(ModulatorKind::Envelope, "Envelope 1");
        let id = envelope.id;
        p.tracks[0].modulation.modulators.push(envelope);
        let opened = |p: &Project, up| {
            let mut meters = Meters::new();
            meters.follow(p);
            let mut devices = Devices::new();
            devices.insert(p.tracks[0].devices[0].id, Transposes::new(up));
            let graph = compile_with_devices(p, &meters, &Media::new(), &devices);
            render(graph.prepare(&CONFIG).unwrap(), 1000);
            meters.output(Output::new(id, 0)).unwrap().get() > 0.0
        };
        assert!(opened(&p, Some(12)));
        // No note gets past an effect that plays none on.
        assert!(!opened(&p, None));
        p.tracks[0].devices[0].bypassed = true;
        assert!(opened(&p, None));
    }

    /// Stands in for a multi-timbral plugin: makes instruments that keep the
    /// key and the channel of the first notes they get, and play nothing.
    #[derive(Default)]
    struct Parts {
        /// Each note's key, and its channel plus one in the byte above, or
        /// 0 for none.
        heard: [AtomicU16; 8],
        count: AtomicUsize,
    }

    impl Parts {
        /// The keys and channels heard, lowest key first.
        fn heard(&self) -> Vec<(u8, Option<u8>)> {
            let count = self.count.swap(0, Ordering::Relaxed).min(self.heard.len());
            let mut heard: Vec<_> = (self.heard[..count].iter())
                .map(|note| {
                    let [key, channel] = note.load(Ordering::Relaxed).to_le_bytes();
                    (key, channel.checked_sub(1))
                })
                .collect();
            heard.sort_unstable();
            heard
        }
    }

    struct Part(Arc<Parts>);

    impl NodeMaker for Arc<Parts> {
        fn make(&self) -> Box<dyn Node> {
            Box::new(Part(Arc::clone(self)))
        }
    }

    impl Node for Part {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(NoteEvent::On { key, channel, .. }) = event.kind {
                    let n = self.0.count.fetch_add(1, Ordering::Relaxed);
                    let channel = channel.map_or(0, |c| c + 1);
                    if let Some(slot) = self.0.heard.get(n) {
                        slot.store(u16::from_le_bytes([key, channel]), Ordering::Relaxed);
                    }
                }
            }
            output.clear();
        }
    }

    /// Keys, a MIDI track playing a beat-long note, key 69, without an
    /// instrument, and Strings, one playing key 60 on a multi-timbral
    /// plugin, with a note wire from the Keys' Post FX into the plugin on
    /// channel 4 (3 here).
    fn parts() -> (Project, Devices, Arc<Parts>) {
        let mut p = note_through(Vec::new());
        let keys = p.tracks[0].id;
        let strings = p.add_track("Strings", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 60, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Note", range, ClipContent::Midi(notes));
        let synth = p.new_device(plugin(true));
        let mut wire = p.new_wire(Tap::PostFx(Chain::Track(keys)), Port::Notes(synth.id));
        wire.channel = Some(3);
        p.wires.push(wire);
        let parts = Arc::new(Parts::default());
        let mut devices = Devices::new();
        devices.insert(synth.id, Arc::new(Arc::clone(&parts)));
        let t = p.track_mut(strings).unwrap();
        t.clips.push(clip);
        t.devices = vec![synth];
        (p, devices, parts)
    }

    fn heard(p: &Project, devices: &Devices, parts: &Parts) -> Vec<(u8, Option<u8>)> {
        let graph = compile_with_devices(p, &Meters::new(), &Media::new(), devices);
        render(graph.prepare(&CONFIG).unwrap(), 1000);
        parts.heard()
    }

    #[test]
    fn a_note_wire_plays_another_tracks_notes_on_an_instrument_on_its_channel() {
        let (mut p, devices, parts) = parts();
        let (keys, strings) = (p.tracks[0].id, p.tracks[1].id);
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        let wire = p.wires[0].id;
        assert!(graph.find(keys::wire(wire)).is_some(), "the wire's relay");
        assert_eq!(heard(&p, &devices, &parts), [(60, None), (69, Some(3))]);
        // Without a channel, on none.
        p.wires[0].channel = None;
        assert_eq!(heard(&p, &devices, &parts), [(60, None), (69, None)]);

        // A muted track's notes don't play on another's instrument, and a
        // track soloed lets through what plays on its own.
        p.track_mut(keys).unwrap().muted = true;
        assert_eq!(heard(&p, &devices, &parts), [(60, None)]);
        p.track_mut(keys).unwrap().muted = false;
        p.track_mut(strings).unwrap().solo = true;
        assert_eq!(heard(&p, &devices, &parts), [(60, None), (69, None)]);
        p.track_mut(strings).unwrap().solo = false;
        p.track_mut(keys).unwrap().solo = true;
        assert_eq!(heard(&p, &devices, &parts), [(60, None), (69, None)]);

        // A deactivated instrument has nothing to play them.
        p.track_mut(keys).unwrap().solo = false;
        p.track_mut(strings).unwrap().devices[0].active = false;
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        assert!(graph.find(keys::wire(wire)).is_none());
    }

    #[test]
    fn a_note_wire_plays_into_a_midi_effect_and_an_instruments_modulators_hear_it() {
        let (mut p, mut devices, parts) = parts();
        let strings = p.tracks[1].id;
        let envelope = p.new_modulator(ModulatorKind::Envelope, "Envelope 1");
        let id = envelope.id;
        p.track_mut(strings)
            .unwrap()
            .modulation
            .modulators
            .push(envelope);
        // Strings plays nothing of its own.
        p.track_mut(strings).unwrap().clips.clear();
        let opened = |p: &Project, devices: &Devices| {
            let mut meters = Meters::new();
            meters.follow(p);
            let graph = compile_with_devices(p, &meters, &Media::new(), devices);
            render(graph.prepare(&CONFIG).unwrap(), 1000);
            meters.output(Output::new(id, 0)).unwrap().get() > 0.0
        };
        assert!(opened(&p, &devices), "the wire's notes open the envelope");
        p.wires[0].channel = None;
        parts.heard();

        // Into an arpeggiator in front of the instrument, the notes go
        // through it, an octave up.
        let arp = p.new_device(midi_effect());
        let octave = Transposes::new(Some(12));
        devices.insert(arp.id, octave.clone());
        p.wires[0].to = Port::Notes(arp.id);
        p.track_mut(strings).unwrap().devices.insert(0, arp);
        assert_eq!(heard(&p, &devices, &parts), [(81, None)]);
        assert_eq!(octave.heard(), [69]);
        assert!(opened(&p, &devices), "the effect's notes open the envelope");
    }

    #[test]
    fn a_midi_track_takes_the_notes_after_a_midi_effect_or_at_post_fx() {
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let mut p = note_through(vec![midi_effect(), midi_effect(), synth]);
        let keys = p.tracks[0].id;
        let [octave, fifth] = [0, 1].map(|i| p.tracks[0].devices[i].id);
        // The Double hears what it takes through a MIDI effect of its own.
        let double = p.add_track("Double", TrackKind::Midi);
        let ear = p.new_device(midi_effect());
        let ear_id = ear.id;
        let t = p.track_mut(double).unwrap();
        t.devices.push(ear);
        t.monitor = Monitor::In;
        let heard = |p: &mut Project, tap| {
            p.track_mut(double).unwrap().input = Some(Input::Tap(tap));
            let ear = Transposes::new(Some(0));
            let mut devices = Devices::new();
            devices.insert(octave, Transposes::new(Some(12)));
            devices.insert(fifth, Transposes::new(Some(7)));
            devices.insert(ear_id, ear.clone());
            let graph = compile_with_devices(p, &Meters::new(), &Media::new(), &devices);
            render(graph.prepare(&CONFIG).unwrap(), 1000);
            ear.heard()
        };
        let post_fx = Tap::PostFx(Chain::Track(keys));
        assert_eq!(heard(&mut p, Tap::PreFx(keys)), [69]);
        assert_eq!(heard(&mut p, Tap::Device(octave)), [81]);
        assert_eq!(heard(&mut p, Tap::Device(fifth)), [88]);
        // What reaches the synth.
        assert_eq!(heard(&mut p, post_fx), [88]);
        // After a deactivated one, what comes into it, and after a bypassed
        // one, the same.
        p.tracks[0].devices[1].active = false;
        assert_eq!(heard(&mut p, Tap::Device(fifth)), [81]);
        assert_eq!(heard(&mut p, post_fx), [81]);
        p.tracks[0].devices[0].bypassed = true;
        assert_eq!(heard(&mut p, Tap::Device(octave)), [69]);
        assert_eq!(heard(&mut p, post_fx), [69]);
    }

    /// Stands in for an instrument plugin that plays its notes out as it
    /// renders: makes nodes that play nothing, and play the notes they get
    /// out again, `up` keys up, on the same frames, and counts the times
    /// they're told their notes go somewhere.
    struct Echoes {
        up: u8,
        told: Arc<AtomicUsize>,
    }

    impl Echoes {
        fn new(up: u8) -> Arc<Self> {
            let told = Arc::new(AtomicUsize::new(0));
            Arc::new(Self { up, told })
        }

        fn told(&self) -> usize {
            self.told.load(Ordering::Relaxed)
        }
    }

    impl NodeMaker for Echoes {
        fn make(&self) -> Box<dyn Node> {
            Box::new(Echo {
                up: self.up,
                made: Vec::with_capacity(8),
                told: Arc::clone(&self.told),
            })
        }
    }

    struct Echo {
        up: u8,
        made: Vec<(usize, NoteEvent)>,
        told: Arc<AtomicUsize>,
    }

    impl Node for Echo {
        fn makes_notes(&mut self) {
            self.told.fetch_add(1, Ordering::Relaxed);
        }

        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(mut note) = event.kind
                    && self.made.len() < self.made.capacity()
                {
                    if let NoteEvent::On { key, .. } = &mut note {
                        *key += self.up;
                    }
                    self.made.push((event.offset, note));
                }
            }
            output.clear();
        }

        fn made_notes(&mut self, _: &ProcessContext<'_>, notes: &mut crate::Notes<'_>) {
            for (offset, note) in self.made.drain(..) {
                notes.send(offset, note);
            }
        }
    }

    /// An instrument plugin with a note output, which it plays its notes
    /// out on as it renders.
    fn echoing() -> DeviceKind {
        let mut kind = plugin(true);
        if let DeviceKind::Plugin(plugin) = &mut kind {
            plugin.note_output = true;
        }
        kind
    }

    #[test]
    fn a_midi_track_takes_the_notes_an_instrument_plays_out_in_the_same_block() {
        // The Keys play their note on an instrument that plays it out an
        // octave up. The Double plays what it takes on an instrument of its
        // own, through a MIDI effect.
        let mut p = note_through(vec![echoing()]);
        let keys = p.tracks[0].id;
        let echo = p.tracks[0].devices[0].id;
        let double = p.add_track("Double", TrackKind::Midi);
        let ear = p.new_device(midi_effect());
        let ear_id = ear.id;
        let synth = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let t = p.track_mut(double).unwrap();
        t.devices = vec![ear, synth];
        t.monitor = Monitor::In;
        let play = |p: &mut Project, tap, up| {
            p.track_mut(double).unwrap().input = Some(Input::Tap(tap));
            let echoes = Echoes::new(12);
            let mut devices = Devices::new();
            devices.insert(echo, echoes.clone());
            devices.insert(ear_id, Transposes::new(Some(up)));
            let graph = compile_with_devices(p, &Meters::new(), &Media::new(), &devices);
            let out = render(graph.prepare(&CONFIG).unwrap(), 1000);
            (out, echoes.told())
        };
        // What it plays out sounds on the Double just as the Keys' own note
        // would an octave up, on the same frames: never a block late.
        let (taken, told) = play(&mut p, Tap::Device(echo), 0);
        let (before, untold) = play(&mut p, Tap::PreFx(keys), 12);
        assert!(taken.iter().any(|&s| s != 0.0));
        assert_eq!(taken, before);
        // Its node keeps them only while they go somewhere.
        assert_eq!((told, untold), (1, 0));
    }

    #[test]
    fn note_wires_and_records_take_the_notes_an_instrument_plays_out() {
        // The Keys' note wire takes what an instrument of theirs plays out,
        // on channel 4, into the Strings' multi-timbral plugin.
        let (mut p, mut devices, parts) = parts();
        let keys = p.tracks[0].id;
        let echo = p.new_device(echoing());
        let echo_id = echo.id;
        p.track_mut(keys).unwrap().devices.push(echo);
        devices.insert(echo_id, Echoes::new(12));
        p.wires[0].from = Tap::Device(echo_id);
        assert_eq!(heard(&p, &devices, &parts), [(60, None), (81, Some(3))]);

        // A MIDI track recording them has the instrument's notes recorded.
        let print = p.add_track("Print", TrackKind::Midi);
        let t = p.track_mut(print).unwrap();
        t.input = Some(Input::Tap(Tap::Device(echo_id)));
        t.armed = true;
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        let graph = graph.prepare(&CONFIG).unwrap();
        let tap = Tap::Device(echo_id);
        assert!(graph.note_records().eq([keys::record_notes(tap)]));

        // Without a note output, the point after it carries no notes.
        if let DeviceKind::Plugin(plugin) = &mut p.track_mut(keys).unwrap().devices[0].kind {
            plugin.note_output = false;
        }
        assert_eq!(heard(&p, &devices, &parts), [(60, None)]);
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        assert_eq!(graph.prepare(&CONFIG).unwrap().note_records().count(), 0);
    }

    #[test]
    fn modulators_dont_hear_notes_made_by_what_they_move() {
        // The Keys play their note on an instrument that plays it out, which
        // a note wire takes to an instrument on the master, whose Envelope
        // hears it.
        let mut p = note_through(vec![echoing()]);
        let echo = p.tracks[0].devices[0].id;
        let synth = p.new_device(plugin(true));
        let synth_id = synth.id;
        p.master.devices.push(synth);
        let wire = p.new_wire(Tap::Device(echo), Port::Notes(synth_id));
        p.wires.push(wire);
        let envelope = p.new_modulator(ModulatorKind::Envelope, "Envelope 1");
        let id = envelope.id;
        let routing = p.new_routing(Output::new(id, 0), ParamAddress::Device(echo, 3), 0.5);
        p.master.modulation.modulators.push(envelope);
        let opened = |p: &Project| {
            let mut meters = Meters::new();
            meters.follow(p);
            let mut devices = Devices::new();
            devices.insert(echo, Echoes::new(12));
            devices.insert(synth_id, Arc::new(Arc::new(Parts::default())));
            let graph = compile_with_devices(p, &meters, &Media::new(), &devices);
            render(graph.prepare(&CONFIG).unwrap(), 1000);
            meters.output(Output::new(id, 0)).unwrap().get() > 0.0
        };
        assert!(opened(&p));
        assert_eq!(p.unheard_notes(), []);
        // It hears them moving the instrument they play.
        let plays = p.new_routing(Output::new(id, 0), ParamAddress::Device(synth_id, 3), 0.5);
        p.master.modulation.routings.push(plays);
        assert!(opened(&p));
        assert_eq!(p.unheard_notes(), []);
        // Moving the instrument that makes them, it can't: it would have to
        // move it before it made them. The project says so too.
        p.master.modulation.routings = vec![routing];
        assert!(!opened(&p));
        let unheard = musicable_model::routing::UnheardNotes {
            chain: Chain::Master,
            wire: Some(wire.id),
            maker: echo,
        };
        assert_eq!(p.unheard_notes(), [unheard]);
        // Nor what it moves before it, the Keys' track's fader aside.
        let keys = p.tracks[0].id;
        let compressor = DeviceKind::Native(NativeDevice::new(NativeKind::Compressor));
        let before = p.new_device(compressor);
        let before_id = before.id;
        p.track_mut(keys).unwrap().devices.insert(0, before);
        let threshold = ParamAddress::Device(before_id, compressor::THRESHOLD as u32);
        let routing = p.new_routing(Output::new(id, 0), threshold, 0.5);
        p.master.modulation.routings = vec![routing];
        assert!(!opened(&p));
        assert_eq!(p.unheard_notes(), [unheard]);
        let fader = ParamAddress::Volume(Chain::Track(keys));
        p.master.modulation.routings = vec![p.new_routing(Output::new(id, 0), fader, 0.5)];
        assert!(opened(&p));
        assert_eq!(p.unheard_notes(), []);
    }

    #[test]
    fn modulators_dont_hear_a_way_that_brings_notes_made_by_what_they_move() {
        // The Keys' instrument plays out notes, which a MIDI track with a
        // clip of its own takes as its input, and wires on to an instrument
        // on the master, whose Envelope hears them.
        let mut p = note_through(vec![echoing()]);
        let echo = p.tracks[0].devices[0].id;
        let double = p.add_track("Double", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 72, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Own", range, ClipContent::Midi(notes));
        let t = p.track_mut(double).unwrap();
        t.input = Some(musicable_model::Input::Tap(Tap::Device(echo)));
        t.clips.push(clip);
        let synth = p.new_device(plugin(true));
        let synth_id = synth.id;
        p.master.devices.push(synth);
        let wire = p.new_wire(Tap::PostFx(Chain::Track(double)), Port::Notes(synth_id));
        p.wires.push(wire);
        let envelope = p.new_modulator(ModulatorKind::Envelope, "Envelope 1");
        let id = envelope.id;
        p.master.modulation.modulators.push(envelope);
        let moves_echo = p.new_routing(Output::new(id, 0), ParamAddress::Device(echo, 3), 0.5);
        p.master.modulation.routings.push(moves_echo);
        let devices = || {
            let mut devices = Devices::new();
            devices.insert(echo, Echoes::new(12));
            devices.insert(synth_id, Arc::new(Arc::new(Parts::default())));
            devices
        };
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices());
        let (from, to) = (
            graph.find(keys::wire(wire.id)),
            graph.find(keys::modulation(Chain::Master)),
        );
        assert!(!graph.sends_notes(from.unwrap(), to.unwrap()));
        let unheard = musicable_model::routing::UnheardNotes {
            chain: Chain::Master,
            wire: Some(wire.id),
            maker: echo,
        };
        assert_eq!(p.unheard_notes(), [unheard]);
        // An Envelope that notes don't gate hears nothing to miss.
        let never = musicable_model::modulation::NoteTrigger::Never;
        let trigger = envelope::NOTE_TRIGGER as u32;
        let modulator = p.modulator_mut(id).unwrap();
        modulator.set(trigger, never as u8 as f64).unwrap();
        assert_eq!(p.unheard_notes(), []);
    }

    #[test]
    fn two_midi_tracks_record_the_same_notes_from_different_taps() {
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let mut p = note_through(vec![midi_effect(), synth]);
        let keys = p.tracks[0].id;
        let arp = p.tracks[0].devices[0].id;
        let taps = [Tap::Device(arp), Tap::PostFx(Chain::Track(keys))];
        for (name, tap) in ["After Arp", "Post FX"].into_iter().zip(taps) {
            let track = p.add_track(name, TrackKind::Midi);
            let t = p.track_mut(track).unwrap();
            t.input = Some(Input::Tap(tap));
            t.armed = true;
        }
        // An audio track takes the sound at Post FX, with a writer of its
        // own.
        let print = p.add_track("Print", TrackKind::Audio);
        p.track_mut(print).unwrap().input = Some(Input::Tap(taps[1]));
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        // The arpeggiator's switch plays both MIDI tracks' notes.
        assert!(graph.note_records().eq(taps.map(keys::record_notes)));
        assert!(graph.records().eq([keys::record(taps[1])]));
    }

    /// Halves the signal 37 frames late, standing in for a plugin that
    /// looks ahead.
    struct LateHalf(Delay);

    impl Node for LateHalf {
        fn prepare(&mut self, config: &EngineConfig) {
            self.0.prepare(config);
        }

        fn latency(&self) -> usize {
            self.0.frames()
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<LateHalf>() {
                self.0.adopt(&mut previous.0);
            }
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            self.0.process(ctx, inputs, output);
            for channel in output.channels_mut() {
                channel.iter_mut().for_each(|s| *s *= 0.5);
            }
        }

        fn reset(&mut self) {
            self.0.reset();
        }
    }

    struct LateHalves;

    impl NodeMaker for LateHalves {
        fn make(&self) -> Box<dyn Node> {
            Box::new(LateHalf(Delay::new(37)))
        }
    }

    /// A tone through a plugin that halves it 37 frames late, and the
    /// plugin's device.
    fn late_halves() -> (Project, DeviceId, Devices) {
        let mut p = Project::new();
        let track = p.add_track("Tone", TrackKind::Audio);
        let device = p.new_device(plugin(false));
        let id = device.id;
        let t = p.track_mut(track).unwrap();
        t.tone = Some(TestTone {
            frequency: 440.0,
            level: 0.5,
        });
        t.devices.push(device);
        let mut devices = Devices::new();
        devices.insert(id, Arc::new(LateHalves));
        (p, id, devices)
    }

    #[test]
    fn bypassed_devices_keep_their_latency_and_deactivated_ones_lose_it() {
        let (mut p, id, devices) = late_halves();
        let graph = |p: &Project| {
            compile_with_devices(p, &Meters::new(), &Media::new(), &devices)
                .prepare(&CONFIG)
                .unwrap()
        };
        let playing = graph(&p);
        assert_eq!(playing.latency(), 37);
        let playing = render(playing, 256);
        p.tracks[0].devices[0].bypassed = true;
        let bypassed = graph(&p);
        assert_eq!(bypassed.latency(), 37);
        let bypassed = render(bypassed, 256);
        p.tracks[0].devices[0].bypassed = false;
        p.tracks[0].devices[0].active = false;
        let deactivated = graph(&p);
        assert_eq!(deactivated.latency(), 0);
        assert!(deactivated.contains(keys::device(id)));
        let deactivated = render(deactivated, 256);

        // Deactivated, the tone plays as if there were no device; bypassed,
        // as it comes in, 37 frames late; playing, halved as well.
        p.tracks[0].devices.clear();
        let without = render(graph(&p), 256);
        assert!(without.iter().any(|&s| s != 0.0));
        assert_eq!(deactivated, without);
        let mut late = vec![0.0; 2 * 37];
        late.extend_from_slice(&without[..without.len() - 2 * 37]);
        assert_eq!(bypassed, late);
        let half: Vec<f32> = late.iter().map(|s| s * 0.5).collect();
        assert_eq!(playing, half);
    }

    #[test]
    fn a_bypass_lane_switches_on_the_frame_it_asks_for() {
        use musicable_model::automation::{Lane, Point};
        use musicable_model::param::ParamAddress;

        let (mut p, id, devices) = late_halves();
        let graph = |p: &Project| {
            compile_with_devices(p, &Meters::new(), &Media::new(), &devices)
                .prepare(&CONFIG)
                .unwrap()
        };
        let beat = p.tempo.frame_at(Beats::new(1), CONFIG.sample_rate) as usize;
        let frames = beat + 1000;
        let playing = render(graph(&p), frames);
        p.tracks[0].devices[0].bypassed = true;
        let bypassed = render(graph(&p), frames);
        // Bypassed from the first beat on. The device is set bypassed, which
        // the lane overrides from the first frame, without a fade.
        let point = |at: i64, value: f64| Point::new(Beats::new(at), value);
        let lane = Lane::new(
            ParamAddress::Bypass(id),
            vec![point(0, 0.0), point(1, 0.0), point(1, 1.0)],
        );
        p.automation.push(lane);
        let switched = render(graph(&p), frames);
        // The fade starts where the first beat comes out, 37 frames late,
        // and takes 10 ms.
        let start = beat + 37;
        let fade = 480;
        let expected: Vec<f32> = (0..2 * frames)
            .map(|i| {
                let m = match (i / 2).checked_sub(start) {
                    None => 0.0,
                    Some(k) => ((k + 1).min(fade)) as f32 / fade as f32,
                };
                playing[i] * (1.0 - m) + bypassed[i] * m
            })
            .collect();
        // Frame by frame, since the whole lot would make a long message.
        if let Some(i) = (0..2 * frames).find(|&i| switched[i] != expected[i]) {
            let (got, due) = (switched[i], expected[i]);
            panic!("{got} on frame {} where {due} was due", i / 2);
        }
    }

    #[test]
    fn tracks_monitor_their_inputs_where_the_layout_puts_them() {
        let mut p = Project::new();
        let vocal = p.add_track("Vocal", TrackKind::Audio);
        p.add_track("Guitar", TrackKind::Audio);
        let mut layout = InputLayout::new();
        layout.add("mic", 0, 1);
        layout.add("rme", 1, 4);
        layout.set_default("mic");
        // The interface's second channel, alone, and the default input in
        // stereo, which the microphone hasn't the channels for.
        let second = AudioInput {
            device: "rme".into(),
            channel: 1,
            stereo: false,
            ..AudioInput::default()
        };
        assert_eq!(
            layout.find(&second),
            Some(InputChannels {
                index: 1,
                channel: 1,
                first: 2,
                count: 1
            })
        );
        assert_eq!(layout.find(&AudioInput::default()), None);
        let mono = AudioInput {
            stereo: false,
            ..AudioInput::default()
        };
        assert_eq!(layout.find(&mono).map(|c| c.first), Some(0));
        p.track_mut(vocal).unwrap().input = Some(Input::Audio(second));
        for track in &mut p.tracks {
            track.monitor = Monitor::In;
        }
        let graph =
            compile_with_inputs(&p, &Meters::new(), &Media::new(), &Devices::new(), &layout);
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph.prepare(&CONFIG).unwrap()).unwrap();
        let channels = [0.25, 0.0, 0.5, 0.0, 0.0];
        let input: Vec<f32> = channels.iter().flat_map(|&c| [c; 64]).collect();
        let mut out = vec![0.0; 2 * 64];
        engine.process_with_input(
            crate::AudioRef::from_planar(&input, 5),
            &[],
            &mut AudioMut::from_planar(&mut out, 2),
        );
        // The vocal's input in both channels; the guitar's is silent.
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn midi_tracks_play_their_input_on_their_instrument() {
        let mut layout = InputLayout::new();
        layout.add_midi("pads");
        layout.add_midi("keyboard");
        assert_eq!(layout.midi_ids().collect::<Vec<_>>(), ["pads", "keyboard"]);
        let keyboard = MidiInput {
            device: "keyboard".into(),
            channel: Some(0),
            ..MidiInput::default()
        };
        let source = |input, channel| Some(MidiSource { input, channel });
        assert_eq!(layout.find_midi(&keyboard), source(Some(1), Some(0)));
        assert_eq!(layout.find_midi(&MidiInput::default()), source(None, None));
        let missing = MidiInput {
            device: "drum machine".into(),
            ..MidiInput::default()
        };
        assert_eq!(layout.find_midi(&missing), None);

        let mut p = Project::new();
        let piano = p.add_track("Piano", TrackKind::Midi);
        let synth = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let track = p.track_mut(piano).unwrap();
        track.devices = vec![synth];
        track.input = Some(Input::Midi(keyboard));
        track.armed = true;
        let graph =
            compile_with_inputs(&p, &Meters::new(), &Media::new(), &Devices::new(), &layout)
                .prepare(&CONFIG)
                .unwrap();
        assert!(graph.contains(keys::track_input(piano)));
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        let message = |time, input, bytes| MidiMessage { time, input, bytes };
        let mut out = vec![0.0; 2 * 256];
        // A note from the pads, and one from the keyboard on another
        // channel, go unheard.
        let others = [
            message(0, 0, [0x90, 69, 127]),
            message(0, 1, [0x91, 69, 127]),
        ];
        let input = crate::AudioRef::empty(256);
        engine.process_interleaved_with_input(input, &others, &mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
        // The keyboard's plays from its frame.
        let note = [message(300, 1, [0x90, 69, 127])];
        engine.process_interleaved_with_input(input, &note, &mut out, 2);
        assert!(out[..2 * 44].iter().all(|&s| s == 0.0));
        assert!(out[2 * 44..].iter().any(|&s| s != 0.0));
    }

    #[test]
    fn muted_tracks_are_silent() {
        let mut p = project();
        let all = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        let bass = p.tracks[1].id;
        p.track_mut(bass).unwrap().muted = true;
        let drums_only = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        p.tracks[0].muted = true;
        let none = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert!(all.iter().any(|&s| s != 0.0));
        assert_ne!(all, drums_only);
        assert!(drums_only.iter().any(|&s| s != 0.0));
        assert!(none.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stems_are_tracks_as_they_feed_the_mix_before_mute_and_solo() {
        // Muting the bass, or soloing the drums, leaves the drums alone in
        // the mix, and the bass in its stem.
        for solo in [false, true] {
            let mut p = project();
            let (drums, bass) = (p.tracks[0].id, p.tracks[1].id);
            match solo {
                true => p.track_mut(drums).unwrap().solo = true,
                false => p.track_mut(bass).unwrap().muted = true,
            }
            let stems = [bass, TrackId(999), drums];
            let graph = compile_with_stems(
                &p,
                &Meters::new(),
                &Media::new(),
                &Devices::new(),
                &InputLayout::new(),
                &stems,
            )
            .prepare(&CONFIG)
            .unwrap();
            assert_eq!(graph.taps(), 2, "no tap for a track the project hasn't got");
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(graph).unwrap();
            handle
                .send(Event::Transport(TransportCommand::Play))
                .unwrap();
            let frames = 500;
            let mut mix = vec![0.0; 2 * frames];
            let mut taps = [vec![0.0; 2 * frames], vec![0.0; 2 * frames]];
            let [bass_stem, drums_stem] = &mut taps;
            engine.process_with_taps(
                &mut AudioMut::from_planar(&mut mix, 2),
                &mut [
                    AudioMut::from_planar(bass_stem, 2),
                    AudioMut::from_planar(drums_stem, 2),
                ],
            );
            assert!(bass_stem.iter().any(|&s| s != 0.0), "{solo}");
            assert_ne!(bass_stem, drums_stem);
            assert_eq!(mix, *drums_stem, "{solo}");
        }
    }

    #[test]
    fn the_compiler_returns_the_newest_project() {
        let mut compiler = Compiler::spawn();
        let mut p = Project::new();
        for i in 0..5 {
            p.add_track(format!("Track {i}"), TrackKind::Audio);
            assert_eq!(compiler.compile_project(CONFIG, &p), i + 1);
        }
        // Snapshots that queued up while it was busy are skipped.
        let mut jobs = Vec::new();
        let last = loop {
            let compiled = compiler.recv();
            jobs.push(compiled.job);
            if compiled.job == 5 {
                break compiled.graph.unwrap();
            }
        };
        // Five nodes for the master, the cue and the metronome, and four
        // per track: a clip player, an input monitor, a fader and an output
        // stage.
        assert_eq!(last.len(), 25);
        assert!(jobs.is_sorted());
        assert!(compiler.try_recv().is_none());
    }

    #[test]
    #[should_panic = "there's no job to wait for"]
    fn waiting_when_every_job_is_done_panics_instead_of_hanging() {
        let mut compiler = Compiler::spawn();
        compiler.compile_project(CONFIG, &Project::new());
        compiler.recv();
        compiler.recv();
    }

    #[test]
    fn each_job_has_its_own_config() {
        let mut compiler = Compiler::spawn();
        let other = EngineConfig {
            sample_rate: 44_100,
            max_block: 32,
        };
        let p = project();
        compiler.compile_project(CONFIG, &p);
        let first = compiler.recv();
        compiler.compile_project(other, &p);
        let second = compiler.recv();
        assert_eq!((first.job, second.job), (1, 2));
        assert_eq!(first.graph.unwrap().config(), CONFIG);
        assert_eq!(second.graph.unwrap().config(), other);
    }

    #[test]
    fn a_job_that_panics_panics_where_its_result_is_taken() {
        let mut compiler = Compiler::spawn();
        compiler.compile(CONFIG, || panic!("a plugin failed to load"));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compiler.recv()));
        let payload = caught.expect_err("the job panicked");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"a plugin failed to load")
        );
    }

    #[test]
    fn the_metronome_plays_alongside_the_master_not_through_it() {
        let mut p = project();
        p.metronome = true;
        p.tracks.iter_mut().for_each(|t| t.muted = true);
        let mut graph = compile(&p).prepare(&CONFIG).unwrap();
        let block = Block {
            frames: 64,
            time: 0,
            transport: TransportState {
                playing: true,
                ..TransportState::default()
            },
            tempo: &p.tempo,
            input: crate::AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        ReferenceRenderer.render(&mut graph, &block);
        // The downbeat's click is on the cue output, and not in the mix.
        assert_eq!(graph.cue(64).unwrap().channel(0)[0], 0.5);
        assert!(graph.output(64).iter().flatten().all(|&s| s == 0.0));
    }

    #[test]
    fn the_metronome_follows_the_project() {
        let mut p = project();
        p.tracks.iter_mut().for_each(|t| t.muted = true);
        let clicks = |p: &Project| render(compile(p).prepare(&CONFIG).unwrap(), 1000);
        assert!(clicks(&p).iter().all(|&s| s == 0.0));
        p.metronome = true;
        // The downbeat, at the first frame, at the metronome's level.
        assert_eq!(clicks(&p)[..2], [0.5, 0.5]);

        // Switched off and on again while playing.
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle
            .swap_graph(compile(&p).prepare(&CONFIG).unwrap())
            .unwrap();
        let enabled = |value| Event::Param {
            node: keys::METRONOME,
            param: Metronome::ENABLED,
            value,
        };
        handle.send(enabled(0.0)).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        assert!(play(&mut engine, 24_000).iter().all(|&s| s == 0.0));
        handle.send(enabled(1.0)).unwrap();
        // The second beat.
        assert_eq!(play(&mut engine, 1)[..2], [0.25, 0.25]);
    }

    /// A project with two tones, each on a track.
    fn two_tones() -> (Project, TrackId, TrackId) {
        let mut p = Project::new();
        let [drums, bass] = ["Drums", "Bass"].map(|name| {
            let track = p.add_track(name, TrackKind::Audio);
            p.track_mut(track).unwrap().tone = Some(TestTone {
                frequency: 220.0,
                level: 0.5,
            });
            track
        });
        (p, drums, bass)
    }

    /// Puts a Macro on `owner` with its knob at `knob`, pulling the fader
    /// of the track with id `track` all the way down as it turns.
    fn knob_on(p: &mut Project, owner: Chain, track: TrackId, knob: f64) -> ModulatorId {
        let mut macros = p.new_modulator(ModulatorKind::Macro, "Macro 1");
        macros.set(0, knob);
        let volume = ParamAddress::Volume(Chain::Track(track));
        let routing = p.new_routing(Output::new(macros.id, 0), volume, -1.0);
        let id = macros.id;
        let modulation = p.modulation_mut(owner).unwrap();
        modulation.modulators.push(macros);
        modulation.routings.push(routing);
        id
    }

    /// Whether the project is silent from frame 32 on, once a fader has had
    /// time to ramp.
    fn silent(p: &Project) -> bool {
        let out = render(compile(p).prepare(&CONFIG).unwrap(), 2048);
        out[64..].iter().all(|&s| s == 0.0)
    }

    #[test]
    fn modulators_move_parameters_through_their_channels_nodes() {
        let (mut p, drums, bass) = two_tones();
        assert!(!silent(&p));
        // One on the drums, and one on the master that goes through the
        // bass's node, which runs after the master's.
        knob_on(&mut p, Chain::Track(drums), drums, 1.0);
        knob_on(&mut p, Chain::Master, bass, 1.0);
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        for chain in [Chain::Track(drums), Chain::Track(bass), Chain::Master] {
            assert!(graph.contains(keys::modulation(chain)), "{chain:?}");
        }
        assert!(silent(&p));
        // A knob at 0 moves nothing.
        let (mut p, drums, bass) = two_tones();
        knob_on(&mut p, Chain::Track(drums), drums, 1.0);
        knob_on(&mut p, Chain::Master, bass, 0.0);
        assert!(!silent(&p));
    }

    #[test]
    fn the_nodes_read_out_their_outputs_and_what_they_move() {
        let (mut p, drums, bass) = two_tones();
        let on_drums = knob_on(&mut p, Chain::Track(drums), drums, 1.0);
        // Through the bass's node, which reads out the sum there.
        let on_master = knob_on(&mut p, Chain::Master, bass, 0.5);
        let mut meters = Meters::new();
        meters.follow(&p);
        render(compile_metered(&p, &meters).prepare(&CONFIG).unwrap(), 128);
        let output = |id| meters.output(Output::new(id, 0)).unwrap().get();
        assert_eq!((output(on_drums), output(on_master)), (1.0, 0.5));
        let moved = |track| {
            (meters.moved(ParamAddress::Volume(Chain::Track(track))))
                .unwrap()
                .get()
        };
        assert_eq!((moved(drums), moved(bass)), (-1.0, -0.5));
    }

    #[test]
    fn routings_bypassed_or_out_of_reach_move_nothing() {
        let (mut p, drums, bass) = two_tones();
        p.track_mut(bass).unwrap().muted = true;
        let group = p.add_track("Group", TrackKind::Group);
        p.track_mut(drums).unwrap().output = Bus::Group(group);
        knob_on(&mut p, Chain::Track(group), drums, 1.0);
        assert!(silent(&p));
        let bypassed = |p: &mut Project, bypassed: bool| {
            let routings = &mut p.track_mut(group).unwrap().modulation.routings;
            routings[0].bypassed = bypassed;
        };
        bypassed(&mut p, true);
        assert!(!silent(&p));
        // Once the drums leave the group, its knob no longer reaches them.
        bypassed(&mut p, false);
        p.track_mut(drums).unwrap().output = Bus::Master;
        assert!(!silent(&p));
        // The group keeps its node for its knob, and the drums, with
        // nothing moving them, have none.
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(graph.contains(keys::modulation(Chain::Track(group))));
        assert!(!graph.contains(keys::modulation(Chain::Track(drums))));
    }

    #[test]
    fn lanes_and_parameter_events_move_modulators() {
        let (mut p, drums, bass) = two_tones();
        p.track_mut(bass).unwrap().muted = true;
        let knob = knob_on(&mut p, Chain::Track(drums), drums, 0.0);
        let param = ParamAddress::Modulator(knob, 0);
        assert_eq!(
            keys::param(&p, param),
            Some((
                keys::modulation(Chain::Track(drums)),
                modulation::param_id(knob, 0)
            ))
        );
        assert!(!silent(&p));
        // A lane turns the knob all the way from the start of the song.
        let points = vec![Point::new(Beats::ZERO, 1.0)];
        p.automation.push(Lane::new(param, points));
        assert!(silent(&p));
        // So does a parameter event, to a knob without a lane.
        p.automation.clear();
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle
            .swap_graph(compile(&p).prepare(&CONFIG).unwrap())
            .unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        assert!(play(&mut engine, 1000).iter().any(|&s| s != 0.0));
        let (node, param) = keys::param(&p, param).unwrap();
        handle
            .send(Event::Param {
                node,
                param,
                value: 1.0,
            })
            .unwrap();
        play(&mut engine, 64);
        assert!(play(&mut engine, 1000).iter().all(|&s| s == 0.0));
    }

    /// A kick, an impulse on frame 100, and a bass whose compressor is keyed
    /// by the kick's Post FX through a wire and plays its key. The kick's
    /// device is there for its slot, which [`latencies`] puts the impulse
    /// in. The compressor's id, and the wire's.
    fn keyed() -> (Project, DeviceId, WireId) {
        let mut p = Project::new();
        let kick = p.add_track("Kick", TrackKind::Audio);
        let bass = p.add_track("Bass", TrackKind::Audio);
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        p.track_mut(kick).unwrap().devices.push(device);
        let mut native = NativeDevice::new(NativeKind::Compressor);
        native.set(compressor::SIDECHAIN, 1.0);
        native.set(compressor::LISTEN, 1.0);
        let device = p.new_device(DeviceKind::Native(native));
        let id = device.id;
        p.track_mut(bass).unwrap().devices.push(device);
        let wire = p.new_wire(Tap::PostFx(Chain::Track(kick)), Port::Sidechain(id));
        let wire_id = wire.id;
        p.wires.push(wire);
        (p, id, wire_id)
    }

    /// The stages for a [`keyed`] project: the kick's impulse, with `kick`
    /// frames of latency after it, and `bass` frames of latency in front of
    /// the bass's compressor.
    fn latencies(
        kick: usize,
        bass: usize,
    ) -> impl FnMut(Channel<'_>, Slot) -> Option<Box<dyn Node>> {
        move |channel, slot| {
            let Channel::Track(track) = channel else {
                return None;
            };
            let late = |frames: usize| {
                (frames > 0).then(|| Box::new(Latent(Delay::new(frames))) as Box<dyn Node>)
            };
            match (track.name.as_str(), slot) {
                ("Kick", Slot::BeforeDevice(_)) => Some(Box::new(Impulse { at: 100 })),
                ("Kick", Slot::PreFader) => late(kick),
                ("Bass", Slot::BeforeDevice(_)) => late(bass),
                _ => None,
            }
        }
    }

    /// `p` compiled with the stages [`latencies`] gives it.
    fn keyed_graph(p: &Project, kick: usize, bass: usize) -> PreparedGraph {
        compile_with(
            p,
            &Meters::new(),
            &Media::new(),
            &Devices::new(),
            &InputLayout::new(),
            latencies(kick, bass),
        )
        .prepare(&CONFIG)
        .unwrap()
    }

    /// The frames of `out`, interleaved stereo, that aren't silent, and
    /// what's on them, which is the same on both channels.
    fn loud(out: &[f32]) -> Vec<(usize, f32)> {
        out.chunks(2)
            .enumerate()
            .filter(|(_, frame)| frame.iter().any(|&s| s != 0.0))
            .map(|(i, frame)| {
                assert_eq!(frame[0], frame[1], "frame {i}");
                (i, frame[0])
            })
            .collect()
    }

    #[test]
    fn a_wire_keys_a_device_on_another_track_lined_up_whatever_the_latency() {
        let (p, ..) = keyed();
        for (kick, bass) in [(0, 0), (37, 0), (0, 37), (20, 37)] {
            let graph = keyed_graph(&p, kick, bass);
            assert_eq!(graph.latency(), kick.max(bass));
            // The kick, and the bass playing it as its key, on the same
            // frame.
            let out = render(graph, 400);
            assert_eq!(loud(&out), [(100 + kick.max(bass), 2.0)], "{kick}, {bass}");
        }
    }

    #[test]
    fn a_muted_or_soloed_out_track_still_keys() {
        let (mut p, ..) = keyed();
        p.tracks[0].muted = true;
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.0)]);
        p.tracks[0].muted = false;
        p.tracks[1].solo = true;
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.0)]);
    }

    #[test]
    fn a_bypassed_or_deactivated_device_plays_nothing_it_is_wired() {
        let (mut p, ..) = keyed();
        p.tracks[1].devices[0].bypassed = true;
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.0)]);
        p.tracks[1].devices[0].bypassed = false;
        p.tracks[1].devices[0].active = false;
        let graph = keyed_graph(&p, 0, 0);
        assert!(!graph.contains(keys::wire(p.wires[0].id)));
        assert_eq!(loud(&render(graph, 400)), [(100, 1.0)]);
    }

    #[test]
    fn wires_into_a_port_sum_at_their_gains_and_follow_their_lanes() {
        let (mut p, id, wire) = keyed();
        p.wire_mut(wire).unwrap().gain = 0.5;
        // From after the kick's device as well, where its impulse is too.
        let from = Tap::Device(p.tracks[0].devices[0].id);
        let mut second = p.new_wire(from, Port::Sidechain(id));
        second.gain = 0.25;
        p.wires.push(second);
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.75)]);
        let lane = Lane::new(
            ParamAddress::Wire(wire),
            vec![Point::new(Beats::ZERO, 0.125)],
        );
        p.automation.push(lane);
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.375)]);
    }

    #[test]
    fn modulators_on_the_keyed_track_move_a_wires_gain_through_its_range() {
        let (mut p, _, wire) = keyed();
        let (kick, bass) = (Chain::Track(p.tracks[0].id), Chain::Track(p.tracks[1].id));
        let gain = ParamAddress::Wire(wire);
        // A wire's gain is on the track of the device it feeds.
        assert!(p.in_reach(bass, gain));
        assert!(!p.in_reach(kick, gain));
        let mut macros = p.new_modulator(ModulatorKind::Macro, "Macro 1");
        macros.set(0, 1.0);
        let routing = p.new_routing(Output::new(macros.id, 0), gain, -1.0);
        let modulation = p.modulation_mut(bass).unwrap();
        modulation.modulators.push(macros);
        modulation.routings.push(routing);
        // All the way down, the compressor listens to nothing.
        assert_eq!(loud(&render(keyed_graph(&p, 0, 0), 400)), [(100, 1.0)]);
        // All the way up, to the wire's +24 dB rather than a fader's +6.
        p.tracks[1].modulation.routings[0].amount = 1.0;
        let out = loud(&render(keyed_graph(&p, 0, 0), 400));
        let [(100, level)] = out[..] else {
            panic!("{out:?}");
        };
        assert!(
            (level - 1.0 - routing::Wire::GAIN_MAX).abs() < 1e-3,
            "{level}"
        );
    }

    /// With the compressor compressing, a kick turns a bass on another
    /// track down from the very frame it comes out of the mix, whatever
    /// the latency on either, muted or not.
    #[test]
    fn a_kick_ducks_a_bass_on_another_track_on_its_own_frame() {
        for (kick, bass) in [(0, 0), (37, 0), (0, 37)] {
            for muted in [false, true] {
                let (mut p, ..) = keyed();
                // A bass under the threshold, so only the kick turns it
                // down.
                p.tracks[1].tone = Some(TestTone {
                    frequency: 55.0,
                    level: 0.005,
                });
                let mut native = NativeDevice::new(NativeKind::Compressor);
                native.set(compressor::SIDECHAIN, 1.0);
                native.set(compressor::THRESHOLD, -40.0);
                native.set(compressor::KNEE, 0.0);
                native.set(compressor::ATTACK, 0.1);
                p.tracks[1].devices[0].kind = DeviceKind::Native(native);
                p.tracks[0].muted = muted;
                let mut plain = p.clone();
                plain.wires.clear();
                let at = 100 + kick.max(bass);
                if !muted {
                    let mut kick_alone = plain.clone();
                    kick_alone.tracks[1].muted = true;
                    let out = render(keyed_graph(&kick_alone, kick, bass), 400);
                    assert_eq!(loud(&out), [(at, 1.0)]);
                }
                let ducked = render(keyed_graph(&p, kick, bass), 400);
                let plain = render(keyed_graph(&plain, kick, bass), 400);
                assert_eq!(ducked[..2 * at], plain[..2 * at], "{kick}, {bass}, {muted}");
                for c in 0..2 {
                    let (ducked, plain) = (ducked[2 * at + c], plain[2 * at + c]);
                    let kick = if muted { 0.0 } else { 1.0 };
                    let (ducked, plain) = ((ducked - kick).abs(), (plain - kick).abs());
                    assert!(ducked < plain * 0.9, "{ducked} {plain}");
                }
            }
        }
    }

    /// An export renders on the parallel scheduler with the tracks'
    /// stems: it plays the wires just as playback does.
    #[test]
    fn an_export_plays_wires_as_playback_does() {
        let (p, ..) = keyed();
        let (kick, bass) = (p.tracks[0].id, p.tracks[1].id);
        let played = render(keyed_graph(&p, 37, 0), 400);
        let graph = build(
            &p,
            &Meters::new(),
            &Media::new(),
            &Devices::new(),
            &InputLayout::new(),
            &[kick, bass],
            latencies(37, 0),
        )
        .prepare(&CONFIG)
        .unwrap();
        let renderer = crate::ParallelRenderer::new(crate::ParallelOptions {
            realtime: false,
            inline_below: None,
            ..crate::ParallelOptions::default()
        });
        let (mut engine, mut handle) = Engine::with_renderer(CONFIG, Box::new(renderer));
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        let frames = 400;
        let mut mix = vec![0.0; 2 * frames];
        let mut stems = [vec![0.0; 2 * frames], vec![0.0; 2 * frames]];
        let [kick_stem, bass_stem] = &mut stems;
        engine.process_with_taps(
            &mut AudioMut::from_planar(&mut mix, 2),
            &mut [
                AudioMut::from_planar(kick_stem, 2),
                AudioMut::from_planar(bass_stem, 2),
            ],
        );
        let interleave = |planar: &[f32]| -> Vec<f32> {
            let (left, right) = planar.split_at(frames);
            left.iter().zip(right).flat_map(|(&l, &r)| [l, r]).collect()
        };
        assert_eq!(interleave(&mix), played);
        assert_eq!(loud(&interleave(kick_stem)), [(137, 1.0)]);
        assert_eq!(loud(&interleave(bass_stem)), [(137, 1.0)]);
    }

    /// A kick, an impulse on frame 100, and a print of it: an audio track
    /// monitoring its input from the tap on the kick that `tap` gives, from
    /// the kick's id and its device's. The device is there for its slot,
    /// which [`printed_graph`] puts the impulse in.
    fn printed(tap: impl FnOnce(TrackId, DeviceId) -> Tap) -> Project {
        let mut p = Project::new();
        let kick = p.add_track("Kick", TrackKind::Audio);
        let print = p.add_track("Print", TrackKind::Audio);
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let id = device.id;
        p.track_mut(kick).unwrap().devices.push(device);
        let track = p.track_mut(print).unwrap();
        track.input = Some(Input::Tap(tap(kick, id)));
        track.monitor = Monitor::In;
        p
    }

    /// `p` compiled with the kick's impulse, `kick` frames of latency after
    /// it, and `print` frames of latency in front of the print's fader.
    fn printed_graph(p: &Project, kick: usize, print: usize) -> PreparedGraph {
        let stages = move |channel: Channel<'_>, slot: Slot| {
            let Channel::Track(track) = channel else {
                return None;
            };
            let late = |frames: usize| {
                (frames > 0).then(|| Box::new(Latent(Delay::new(frames))) as Box<dyn Node>)
            };
            match (track.name.as_str(), slot) {
                ("Kick", Slot::BeforeDevice(_)) => Some(Box::new(Impulse { at: 100 }) as _),
                ("Kick", Slot::PreFader) => late(kick),
                ("Print", Slot::PreFader) => late(print),
                _ => None,
            }
        };
        let inputs = InputLayout::new();
        compile_with(
            p,
            &Meters::new(),
            &Media::new(),
            &Devices::new(),
            &inputs,
            stages,
        )
        .prepare(&CONFIG)
        .unwrap()
    }

    /// Hands on what a record tap carries, its first channel, with silence
    /// as zeros.
    struct Kept(rtrb::Producer<f32>);

    impl crate::TapWriter for Kept {
        fn write(&mut self, _: u64, audio: crate::AudioRef<'_>) {
            for i in 0..audio.frames() {
                let sample = if audio.channels() > 0 {
                    audio.channel(0)[i]
                } else {
                    0.0
                };
                self.0.push(sample).unwrap();
            }
        }
    }

    /// A [`Kept`] with room for 400 frames, and what it hands on.
    fn kept() -> (Box<dyn crate::TapWriter>, rtrb::Consumer<f32>) {
        let (samples, kept) = rtrb::RingBuffer::new(400);
        (Box::new(Kept(samples)), kept)
    }

    fn drain(kept: &mut rtrb::Consumer<f32>) -> Vec<f32> {
        std::iter::from_fn(|| kept.pop().ok()).collect()
    }

    #[test]
    fn a_track_plays_another_tracks_tap_lined_up_whatever_the_latency() {
        for (kick, print) in [(0, 0), (37, 0), (0, 37), (20, 37)] {
            let p = printed(|kick, _| Tap::PostFader(Chain::Track(kick)));
            let graph = printed_graph(&p, kick, print);
            assert_eq!(graph.latency(), kick + print);
            // The kick, and the print playing it, on the same frame.
            let at = 100 + kick + print;
            assert_eq!(loud(&render(graph, 400)), [(at, 2.0)], "{kick}, {print}");
            // From after the device, before the kick's latency.
            let p = printed(|_, device| Tap::Device(device));
            let at = 100 + kick.max(print);
            let out = render(printed_graph(&p, kick, print), 400);
            assert_eq!(loud(&out), [(at, 2.0)], "{kick}, {print}");
        }
    }

    #[test]
    fn a_muted_track_carries_on_into_the_tracks_that_take_its_tap() {
        let mut p = printed(|kick, _| Tap::PostFader(Chain::Track(kick)));
        p.tracks[0].muted = true;
        assert_eq!(loud(&render(printed_graph(&p, 0, 0), 400)), [(100, 1.0)]);
        // Unless the print plays its clips, which are silent.
        p.tracks[1].monitor = Monitor::Off;
        assert!(loud(&render(printed_graph(&p, 0, 0), 400)).is_empty());
        // Nor does it play a tap the project hasn't.
        p.tracks[1].monitor = Monitor::In;
        let gone = Tap::PostFader(Chain::Track(TrackId(99)));
        p.tracks[1].input = Some(Input::Tap(gone));
        let graph = printed_graph(&p, 0, 0);
        assert!(graph.contains(keys::track_input(p.tracks[1].id)));
        assert_eq!(graph.records().count(), 0);
        assert!(loud(&render(graph, 400)).is_empty());
    }

    #[test]
    fn a_track_records_the_tap_it_takes_as_the_mix_plays_it() {
        let p = printed(|kick, _| Tap::PostFader(Chain::Track(kick)));
        let key = keys::record(Tap::PostFader(Chain::Track(p.tracks[0].id)));
        for (kick, print) in [(0, 0), (37, 0), (0, 37)] {
            let graph = printed_graph(&p, kick, print);
            assert!(graph.records().eq([key]));
            let (mut engine, mut handle) = Engine::new(CONFIG);
            let (writer, mut kept) = kept();
            assert!(handle.add_writer(key, writer).is_ok());
            handle.swap_graph(graph).unwrap();
            play(&mut engine, 400);
            // Lined up with the output, as the kick plays in the mix.
            let at = 100 + kick + print;
            let loud: Vec<(usize, f32)> = (drain(&mut kept).into_iter().enumerate())
                .filter(|&(_, s)| s != 0.0)
                .collect();
            assert_eq!(loud, [(at, 1.0)], "{kick}, {print}");
        }
    }

    #[test]
    fn a_track_resampling_the_master_plays_nothing_while_it_records() {
        let mut p = printed(|_, _| Input::RESAMPLING.tap().unwrap());
        // The print's tone, sent on too.
        let print = p.tracks[1].id;
        let reverb = p.add_track("Reverb", TrackKind::Return);
        let mut send = p.new_send(reverb);
        send.level = 1.0;
        let track = p.track_mut(print).unwrap();
        track.tone = Some(TestTone {
            frequency: 220.0,
            level: 0.5,
        });
        track.armed = true;
        track.sends.push(send);
        // And taken by another track.
        let copy = p.add_track("Copy", TrackKind::Audio);
        let track = p.track_mut(copy).unwrap();
        track.input = Some(Input::Tap(Tap::PreFx(print)));
        track.monitor = Monitor::In;
        let key = keys::record(Tap::PostFader(Chain::Master));
        let copied = keys::record(Tap::PreFx(print));
        assert!(printed_graph(&p, 0, 0).records().eq([key, copied]));
        // Playing, it plays its tone, monitoring or not.
        for monitor in [Monitor::In, Monitor::Auto, Monitor::Off] {
            p.track_mut(print).unwrap().monitor = monitor;
            let out = render(printed_graph(&p, 0, 0), 400);
            assert!(loud(&out).len() > 300, "{monitor:?}");
        }
        // Recording, the master it records has the kick alone in it.
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let (writer, mut kept) = kept();
        assert!(handle.add_writer(key, writer).is_ok());
        handle.swap_graph(printed_graph(&p, 0, 0)).unwrap();
        for command in [TransportCommand::Record(true), TransportCommand::Play] {
            handle.send(Event::Transport(command)).unwrap();
        }
        let out = play(&mut engine, 400);
        assert_eq!(loud(&out), [(100, 1.0)]);
        let left: Vec<f32> = out.chunks(2).map(|frame| frame[0]).collect();
        assert_eq!(drain(&mut kept), left);
    }

    /// Plays 0.25 on every channel from the first frame anything comes in
    /// on, whatever comes in after, as a reverb's tail rings on.
    struct Ringing(bool);

    impl Node for Ringing {
        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            out: &mut AudioMut<'_>,
        ) {
            out.clear();
            let heard = |i: usize| inputs.iter().any(|input| input.iter().any(|c| c[i] != 0.0));
            let from = if self.0 {
                Some(0)
            } else {
                (0..ctx.frames).find(|&i| heard(i))
            };
            if let Some(from) = from {
                self.0 = true;
                for channel in out.channels_mut() {
                    channel[from..].fill(0.25);
                }
            }
        }
    }

    #[test]
    fn a_track_resampling_the_master_cuts_what_its_devices_ring_on_with() {
        let mut p = printed(|_, _| Input::RESAMPLING.tap().unwrap());
        let print = p.tracks[1].id;
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let track = p.track_mut(print).unwrap();
        track.tone = Some(TestTone {
            frequency: 220.0,
            level: 0.5,
        });
        track.armed = true;
        track.devices.push(device);
        // The print's tone sets off a device that rings on.
        let stages = |channel: Channel<'_>, slot: Slot| {
            let Channel::Track(track) = channel else {
                return None;
            };
            match (track.name.as_str(), slot) {
                ("Print", Slot::BeforeDevice(_)) => Some(Box::new(Ringing(false)) as _),
                _ => None,
            }
        };
        let inputs = InputLayout::new();
        let graph = compile_with(
            &p,
            &Meters::new(),
            &Media::new(),
            &Devices::new(),
            &inputs,
            stages,
        );
        let key = keys::record(Tap::PostFader(Chain::Master));
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let (writer, mut kept) = kept();
        assert!(handle.add_writer(key, writer).is_ok());
        handle.swap_graph(graph.prepare(&CONFIG).unwrap()).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        play(&mut engine, 200);
        let playing = drain(&mut kept);
        assert_eq!((playing[0], playing[1], playing[199]), (0.0, 0.25, 0.25));
        // From the frame it starts recording, nothing of it is in the master.
        handle
            .send(Event::Transport(TransportCommand::Record(true)))
            .unwrap();
        play(&mut engine, 200);
        assert_eq!(drain(&mut kept), [0.0; 200]);
    }

    #[test]
    fn a_track_set_always_live_has_its_modulators_render_live() {
        // Its macro moves its own volume, which nothing live would reach
        // the macro from.
        let (mut p, drums, bass) = two_tones();
        knob_on(&mut p, Chain::Track(drums), drums, 0.5);
        knob_on(&mut p, Chain::Track(bass), bass, 0.5);
        p.track_mut(drums).unwrap().always_live = true;
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(graph.renders_live(keys::modulation(Chain::Track(drums))));
        assert!(!graph.renders_live(keys::modulation(Chain::Track(bass))));
    }

    #[test]
    fn a_job_can_keep_its_graph_whole() {
        let mut p = project();
        p.tracks[0].always_live = true;
        let key = keys::track_clips(p.tracks[0].id);
        let mut compiler = Compiler::spawn();
        let split = p.clone();
        compiler.compile(CONFIG, move || compile(&split));
        assert!(compiler.recv().graph.unwrap().renders_live(key));
        compiler.compile_whole(CONFIG, move || compile(&p));
        let whole = compiler.recv().graph.unwrap();
        assert!(!whole.renders_live(key));
        assert!(whole.contains(key));
    }

    #[test]
    fn a_track_set_always_live_renders_live_with_what_it_plays_into() {
        let mut p = project();
        let (drums, bass, reverb) = (p.tracks[0].id, p.tracks[1].id, p.tracks[2].id);
        let group = p.add_track("Group", TrackKind::Group);
        let keys = p.add_track("Keys", TrackKind::Midi);
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let gain = device.id;
        let send = p.new_send(reverb);
        let track = p.track_mut(bass).unwrap();
        track.always_live = true;
        track.devices.push(device);
        track.sends.push(send);
        p.track_mut(keys).unwrap().output = Bus::Group(group);
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        // The bass from its clips on, what it sends to and the master, but
        // not the drums, which only play into the master too.
        for key in [
            keys::track_clips(bass),
            keys::track_input(bass),
            keys::device(gain),
            keys::track_output(bass),
            keys::track_bus(reverb),
            keys::master_output(),
        ] {
            assert!(graph.renders_live(key), "{key:?}");
        }
        for key in [keys::track_clips(drums), keys::track_output(drums)] {
            assert!(!graph.renders_live(key), "{key:?}");
        }
        assert!(!graph.renders_live(keys::track_bus(group)));
        // A group set Always Live renders live from its bus on, but what
        // feeds it renders ahead.
        p.track_mut(bass).unwrap().always_live = false;
        p.track_mut(group).unwrap().always_live = true;
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        for key in [keys::track_bus(group), keys::track_output(group)] {
            assert!(graph.renders_live(key), "{key:?}");
        }
        for key in [keys::track_clips(keys), keys::track_clips(bass)] {
            assert!(!graph.renders_live(key), "{key:?}");
        }
    }

    #[test]
    fn a_plugin_that_renders_live_has_what_it_plays_into_render_live() {
        let mut p = project();
        let (drums, bass, reverb) = (p.tracks[0].id, p.tracks[1].id, p.tracks[2].id);
        let device = p.new_device(plugin(false));
        let id = device.id;
        let send = p.new_send(reverb);
        let track = p.track_mut(bass).unwrap();
        track.devices.push(device);
        track.sends.push(send);
        let mut devices = Devices::new();
        devices.insert(id, Arc::new(Halves));
        devices.set_live(id);
        let prepared = |p: &Project| {
            compile_with_devices(p, &Meters::new(), &Media::new(), &devices)
                .prepare(&CONFIG)
                .unwrap()
        };
        let graph = prepared(&p);
        // From the plugin on, with what it sends to and the master, while
        // what plays into it renders ahead, as the drums do.
        for key in [
            keys::device(id),
            keys::track_output(bass),
            keys::track_bus(reverb),
            keys::master_output(),
        ] {
            assert!(graph.renders_live(key), "{key:?}");
        }
        for key in [keys::track_clips(bass), keys::track_clips(drums)] {
            assert!(graph.contains(key) && !graph.renders_live(key), "{key:?}");
        }
        // Out of the signal, it has nothing render live.
        p.device_mut(id).unwrap().active = false;
        assert!(!prepared(&p).renders_live(keys::master_output()));

        // A MIDI effect that renders live has the instrument it plays into
        // render live, and the clips that play into it ahead.
        let synth = DeviceKind::TestInstrument { level: 0.5 };
        let p = note_through(vec![midi_effect(), synth]);
        let (track, effect) = (p.tracks[0].id, p.tracks[0].devices[0].id);
        let instrument = p.tracks[0].devices[1].id;
        let mut devices = Devices::new();
        devices.insert(effect, Transposes::new(Some(12)));
        devices.set_live(effect);
        let graph = compile_with_devices(&p, &Meters::new(), &Media::new(), &devices);
        let graph = graph.prepare(&CONFIG).unwrap();
        for key in [keys::device(effect), keys::device(instrument)] {
            assert!(graph.renders_live(key), "{key:?}");
        }
        let clips = keys::track_clips(track);
        assert!(graph.contains(clips) && !graph.renders_live(clips));
    }

    #[test]
    fn a_track_resampling_the_master_renders_on_one_side_of_the_engine() {
        let mut p = printed(|_, _| Input::RESAMPLING.tap().unwrap());
        let print = p.tracks[1].id;
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let id = device.id;
        p.track_mut(print).unwrap().devices.push(device);
        // A device after the monitor that renders live, as a plugin that
        // has to: the monitor renders live too, and the cut after it.
        let mut graph = compile(&p);
        let switch = graph.find(keys::device(id)).unwrap();
        graph.set_live(switch).unwrap();
        let graph = graph.prepare(&CONFIG).unwrap();
        for key in [keys::track_input(print), keys::track_cut(print)] {
            assert!(graph.renders_live(key), "{key:?}");
        }
    }

    #[test]
    fn a_track_that_goes_to_sends_only_is_heard_through_its_sends_alone() {
        let mut p = project();
        let (drums, bass, reverb) = (p.tracks[0].id, p.tracks[1].id, p.tracks[2].id);
        p.track_mut(bass).unwrap().muted = true;
        let direct = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert!(direct.iter().any(|&s| s != 0.0));
        p.track_mut(drums).unwrap().output = Bus::SendsOnly;
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(graph.contains(keys::track_output(drums)), "its meter");
        assert!(render(graph, 256).iter().all(|&s| s == 0.0));
        let mut send = p.new_send(reverb);
        send.level = 1.0;
        p.track_mut(drums).unwrap().sends.push(send);
        assert_eq!(render(compile(&p).prepare(&CONFIG).unwrap(), 256), direct);
    }

    /// A Lead MIDI track playing a note from its clip, with no instrument,
    /// and a Double MIDI track taking the Lead's notes, monitoring In, with
    /// an instrument to play them.
    fn doubled() -> Project {
        let mut p = Project::new();
        let lead = p.add_track("Lead", TrackKind::Midi);
        let mut notes = musicable_model::clip::MidiClip::default();
        notes.add_note(Beats::ZERO, Beats::new(1), 69, 1.0);
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let clip = p.new_clip("Note", range, ClipContent::Midi(notes));
        p.track_mut(lead).unwrap().clips.push(clip);
        let double = p.add_track("Double", TrackKind::Midi);
        let synth = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let track = p.track_mut(double).unwrap();
        track.devices = vec![synth];
        track.input = Some(Input::Tap(Tap::PreFx(lead)));
        track.monitor = Monitor::In;
        p
    }

    #[test]
    fn a_midi_track_plays_another_tracks_notes_on_its_instrument() {
        let mut p = doubled();
        let (lead, double) = (p.tracks[0].id, p.tracks[1].id);
        let played = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert!(
            played[..8].iter().any(|&s| s != 0.0),
            "from the note's start"
        );
        // Taken before mute and solo, as a key is.
        p.track_mut(lead).unwrap().muted = true;
        let muted = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert_eq!(muted, played);
        // Off, the Double plays its own clips, which are silent.
        p.track_mut(double).unwrap().monitor = Monitor::Off;
        let off = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert!(off.iter().all(|&s| s == 0.0));
        // A Double taking a tap the project hasn't plays none.
        let track = p.track_mut(double).unwrap();
        track.monitor = Monitor::In;
        track.input = Some(Input::Tap(Tap::PreFx(TrackId(99))));
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert_eq!(graph.note_records().count(), 0);
        assert!(render(graph, 256).iter().all(|&s| s == 0.0));
    }

    /// Hands on the notes a note record carries, at their engine times.
    struct KeptNotes(rtrb::Producer<(u64, NodeEventKind)>);

    impl crate::TapWriter for KeptNotes {
        fn write(&mut self, _: u64, audio: crate::AudioRef<'_>) {
            assert_eq!(audio.channels(), 0, "a note record has no sound");
        }

        fn write_notes(&mut self, time: u64, frames: usize, notes: &[NodeEvent]) {
            for event in notes {
                assert!(event.offset < frames);
                let at = time + event.offset as u64;
                self.0.push((at, event.kind)).unwrap();
            }
        }
    }

    #[test]
    fn a_midi_track_records_the_notes_it_takes_as_they_play() {
        let p = doubled();
        let lead = p.tracks[0].id;
        let key = keys::record_notes(Tap::PreFx(lead));
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        assert!(graph.note_records().eq([key]));
        assert_eq!(graph.records().count(), 0);
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let (notes, mut kept) = rtrb::RingBuffer::new(8);
        assert!(handle.add_writer(key, Box::new(KeptNotes(notes))).is_ok());
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        // Half a second at 120 beats a minute: the beat-long note ends.
        play(&mut engine, 24_100);
        let clip = &p.tracks[0].clips[0];
        let ClipContent::Midi(midi) = &clip.content else {
            unreachable!("the lead's clip is a MIDI clip");
        };
        let note = crate::clips::note_id(clip.id, midi.notes[0].id);
        let kept: Vec<_> = std::iter::from_fn(|| kept.pop().ok()).collect();
        assert_eq!(
            kept,
            [
                (
                    0,
                    NodeEventKind::Note(NoteEvent::On {
                        note,
                        key: 69,
                        velocity: 1.0,
                        channel: None,
                    })
                ),
                (
                    24_000,
                    NodeEventKind::Note(NoteEvent::Off {
                        note,
                        velocity: musicable_model::clip::Note::RELEASE_VELOCITY
                    })
                )
            ]
        );
    }
}

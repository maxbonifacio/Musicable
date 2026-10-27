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
//! it: the layout says which MIDI inputs it takes. Group and return tracks
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
//! Bypass says. A deactivated device's switch stays in the graph, so its
//! plugin stops in step with it, but out of the signal, latency and all.
//!
//! Each lane of automation that isn't overridden goes to the node with its
//! parameter ([`keys::param`], [`Graph::automate`]), which follows it as the
//! song plays.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use musicable_model::mixer::Bus;
use musicable_model::time::TempoMap;
use musicable_model::{
    AudioInput, ClipType, ConsoleSettings, Device, DeviceId, DeviceKind, Input, Master, MidiInput,
    Project, Track, TrackId, TrackKind,
};

use crate::clips::{AudioClips, MidiClips};
use crate::media::Media;
use crate::meter::Meters;
use crate::midi::MidiMonitor;
use crate::nodes::{Clipper, Gain, InputMonitor, Metronome, Sine, Sum, TestInstrument};
use crate::switch::Switch;
use crate::{Automation, EngineConfig, Graph, GraphError, Node, NodeId, NodeKey, PreparedGraph};

/// The keys [`compile`] gives model objects' nodes.
///
/// A track's nodes have its id in the low 32 bits of the key and their role
/// in the 16 bits above: 1 for the fader, 2 for the test tone, 3 to 6 for
/// the built-in slots, 7 for the output stage, 8 for a group's or a
/// return's bus, 11 for an audio or MIDI track's clip player and 12 for a
/// track's input monitor. The slot in
/// front of a device also has the device's position in the top 16 bits. The
/// master's strip has the keys of a track whose id is `u32::MAX`, which no
/// track has. Sends and devices have their own ids in the low 32 bits, with
/// role 9 for sends and 10 for devices. Keys with role 0 belong to no
/// channel.
pub mod keys {
    use musicable_model::param::ParamAddress;
    use musicable_model::{Chain, DeviceId, SendId, TrackId};

    use super::Slot;
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

    /// The native stage in one of a track's built-in slots.
    pub fn track_slot(track: TrackId, slot: Slot) -> NodeKey {
        let role = match slot {
            Slot::PreFader => 3,
            Slot::PostFader => 4,
            Slot::BusFeed => 5,
            Slot::BeforeDevice(device) => 6 | u64::from(device) << 16,
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

    /// The node with the parameter at `address`, and the parameter: a
    /// channel's fader's [`Gain::GAIN`] for its volume and [`Gain::PAN`] for
    /// its pan, a send's [`Gain::GAIN`], a device's parameter `n` as
    /// `ParamId(n)`, and its Bypass as its switch's [`Switch::BYPASS`]. A
    /// clip's parameters have none: its clip player follows the curves the
    /// clip keeps.
    pub fn param(address: ParamAddress) -> Option<(NodeKey, ParamId)> {
        let fader = |chain| match chain {
            Chain::Track(track) => track_gain(track),
            Chain::Master => master_gain(),
        };
        Some(match address {
            ParamAddress::Volume(chain) => (fader(chain), Gain::GAIN),
            ParamAddress::Pan(chain) => (fader(chain), Gain::PAN),
            ParamAddress::Send(send) => (self::send(send), Gain::GAIN),
            // The switch's own id, which no plugin has.
            ParamAddress::Device(_, n) if ParamId(n) == Switch::BYPASS => return None,
            ParamAddress::Device(device, n) => (self::device(device), ParamId(n)),
            ParamAddress::Bypass(device) => (self::device(device), Switch::BYPASS),
            ParamAddress::Clip(..) => return None,
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
    /// In front of the channel's device `n`, counting from 0.
    BeforeDevice(u16),
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
/// device: plugins now, extension devices later. The app fills it from the
/// plugin instances it holds.
#[derive(Clone, Default)]
pub struct Devices {
    makers: HashMap<DeviceId, Arc<dyn NodeMaker>>,
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
        f.debug_struct("Devices")
            .field("devices", &devices)
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
        let monitor = match (&track.input, player) {
            (Some(Input::Audio(input)), Some(player)) if track.kind == TrackKind::Audio => {
                let mut monitor = InputMonitor::new(track.monitor, track.armed);
                if let Some(channels) = inputs.find(input) {
                    monitor = monitor.with_input(channels.first, channels.count);
                }
                let monitor = graph.add(keys::track_input(track.id), monitor);
                connect(&mut graph, player, monitor);
                Some(monitor)
            }
            _ => None,
        };
        // A MIDI track's input joins its clips' notes on their way to its
        // instrument. A track without one has a monitor too, so the notes
        // held on an input it had end when the input goes.
        let midi_monitor = match (track.kind, player) {
            (TrackKind::Midi, Some(_)) => {
                let mut monitor = MidiMonitor::new(track.monitor, track.armed);
                if let Some(Input::Midi(input)) = &track.input
                    && let Some(source) = inputs.find_midi(input)
                {
                    monitor = monitor.with_source(source);
                }
                Some(graph.add(keys::track_input(track.id), monitor))
            }
            _ => None,
        };
        let head = buses.get(&track.id).copied().or(monitor).or(player);
        let mut strip = Strip {
            graph: &mut graph,
            tail: head,
        };
        let taps = strip.channel(Channel::Track(track), devices, media, &mut stages);
        if track.kind == TrackKind::Midi
            && let (Some(player), Some(instrument)) = (player, taps.instrument)
        {
            let routes = match midi_monitor {
                Some(monitor) => vec![(player, monitor), (monitor, instrument)],
                None => vec![(player, instrument)],
            };
            for (from, to) in routes {
                strip
                    .graph
                    .send_notes(from, to)
                    .expect("the nodes were just added");
            }
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
        let bus = match project.output_of(track) {
            Bus::Master => master,
            Bus::Group(group) => buses[&group],
        };
        strip.feed(bus);
        for send in project.sends_of(track) {
            let node = Gain::new(send.level).with_muted(silenced.send(send.id));
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
    strip.channel(
        Channel::Master(&project.master),
        devices,
        media,
        &mut stages,
    );
    let output = Gain::new(1.0).with_meter(meters.master().clone());
    strip.push(keys::master_output(), Box::new(output));
    let output = strip.tail.expect("the master's strip has its output stage");
    graph.set_output(output).expect("the output was just added");
    graph.set_cue(cue).expect("the cue bus was just added");
    for node in stems.iter().filter_map(|track| tapped.get(track)) {
        graph
            .add_tap(*node)
            .expect("the stems' nodes were just added");
    }

    // A parameter whose node isn't in the graph, such as a missing
    // plugin's, has nothing to automate.
    for lane in project.automation.iter().filter(|lane| !lane.overridden) {
        let Some((key, param)) = keys::param(lane.param) else {
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
/// fader, and the last one after the post-fader slot. `None` where the
/// channel has no node yet. And the channel's first instrument, if it has
/// one.
struct Taps {
    pre_fader: Option<NodeId>,
    post_fader: Option<NodeId>,
    instrument: Option<NodeId>,
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
    /// instrument after it.
    fn channel(
        &mut self,
        channel: Channel<'_>,
        devices: &Devices,
        media: &Media,
        stages: &mut impl FnMut(Channel<'_>, Slot) -> Option<Box<dyn Node>>,
    ) -> Taps {
        let mut slot = |strip: &mut Self, at: Slot| {
            if let Some(stage) = stages(channel, at) {
                strip.push(channel.slot_key(at), stage);
            }
        };
        let mut instrument = None;
        for (position, device) in channel.devices().iter().enumerate() {
            // A key has room for 65,536 slots in front of devices.
            if let Ok(position) = u16::try_from(position) {
                slot(self, Slot::BeforeDevice(position));
            }
            let key = keys::device(device.id);
            let node = match device_node(device, devices, media) {
                Some(node) if !device.active => {
                    self.graph.add(key, Switch::deactivated(node));
                    None
                }
                Some(node) => Some(self.push(key, Box::new(Switch::new(node, device.bypassed)))),
                None => None,
            };
            if device.kind.is_instrument() && instrument.is_none() {
                instrument = Some(node);
            }
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
                    let graph = (job.build)().prepare(&job.config);
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
        self.last_job += 1;
        let job = Job {
            number: self.last_job,
            config,
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

    use musicable_model::clip::ClipContent;
    use musicable_model::time::{BeatRange, Beats};
    use musicable_model::{DeviceId, Monitor, SendId, TestTone, TrackId, TrackKind};

    use super::*;
    use crate::nodes::{Delay, Impulse, Latent};
    use crate::{
        AudioMut, Block, Engine, Event, Inputs, MidiMessage, ProcessContext, ReferenceRenderer,
        Renderer, TransportCommand, TransportState,
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
        (nodes, key(graph.output), graph.cue.map(key))
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
        let slots = [
            Slot::BeforeDevice(0),
            Slot::BeforeDevice(1),
            Slot::BeforeDevice(u16::MAX),
            Slot::PreFader,
            Slot::PostFader,
            Slot::BusFeed,
        ];
        let mut seen = BTreeSet::from([
            keys::MASTER,
            keys::CUE,
            keys::METRONOME,
            keys::master_gain(),
            keys::master_output(),
        ]);
        seen.extend(slots.map(keys::master_slot));
        assert_eq!(seen.len(), 11);
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
            let others = [keys::send(SendId(id)), keys::device(DeviceId(id))];
            for key in strip.into_iter().chain(roles).chain(others) {
                assert!(seen.insert(key), "{key:?} is taken");
            }
        }
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
            instrument,
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
}

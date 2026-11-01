//! Real-time audio graph engine.
//!
//! The project model is compiled into a [`Graph`] of [`Node`]s off the audio
//! thread ([`compile()`], [`Compiler`]), prepared for the device's sample rate
//! and block size ([`Graph::prepare`]), and handed to the running [`Engine`]
//! through its [`EngineHandle`]. The engine swaps graphs between blocks. Each
//! node in the new graph takes over the state of the old node with the same
//! [`NodeKey`] ([`Node::adopt`]), and old graphs go back to the control thread
//! to be dropped.
//!
//! Graphs are run by a [`Renderer`]. The [`ReferenceRenderer`] processes one
//! node at a time in topological order: it is the oracle the
//! [`ParallelRenderer`] must match sample for sample. The parallel renderer
//! runs each node as soon as its inputs are done, on a pool of worker threads
//! and the audio thread.
//!
//! With a [`Lead`] set ([`EngineHandle::set_lead`]), the engine renders the
//! graph ahead of the audio device while nothing in it is played live, on
//! the parallel renderer's workers between device callbacks, and a callback
//! plays what was rendered.
//!
//! `docs/engine.md` describes the threads, the message flow and the real-time
//! rules.

use std::any::Any;

use musicable_model::time::TempoMap;

mod ahead;
pub mod automation;
pub mod buffer;
pub mod clips;
pub mod compile;
pub mod devices;
mod engine;
pub mod event;
pub mod graph;
pub mod load;
pub mod media;
pub mod meter;
pub mod midi;
pub mod modulation;
pub mod nodes;
pub mod output_recording;
mod parallel;
pub mod preview;
mod queue;
mod render;
pub mod resample;
pub mod rt;
pub mod switch;
pub mod tasks;
pub mod timeline;
mod transport;

pub use ahead::{AHEAD_BLOCK, AheadWork, Lead};
pub use automation::Automation;
pub use buffer::{AudioBuffer, AudioMut, AudioRef, InputPort, Inputs};
pub use compile::{
    Compiled, Compiler, Devices, InputChannels, InputLayout, MidiSource, NodeMaker, compile,
    compile_metered, compile_with_devices, compile_with_inputs, compile_with_media,
    compile_with_stems,
};
pub use engine::{
    Engine, EngineHandle, MAX_WRITERS, PreviewAt, QueueFull, Sent, Stats, SwapError, TapWriter,
};
pub use event::{
    Event, NodeEvent, NodeEventKind, NoteEvent, NoteId, ParamId, TransportCommand, TransportState,
};
pub use graph::{Controls, Graph, GraphError, Joint, NodeId, NodeKey, Notes, PreparedGraph};
pub use load::{Load, LoadKey, NodeLoads};
pub use meter::{Level, Meter, Meters};
pub use midi::{MidiMessage, MidiMonitor, MidiNotes, NoteWire, Played, played_notes};
pub use output_recording::OutputRecording;
pub use parallel::{
    AheadWaker, PARTS, ParallelMonitor, ParallelOptions, ParallelRenderer, ParallelStats, Pipeline,
    SHORTEST_PART,
};
pub use preview::Preview;
pub use render::{Block, ReferenceRenderer, Renderer};
pub use timeline::{Segment, Timeline};

#[cfg(test)]
#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

/// What a graph is prepared for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub sample_rate: u32,
    /// The most frames the engine renders in one block. Device callbacks can
    /// be any length; longer ones are rendered in several blocks.
    pub max_block: usize,
}

/// One processing unit: an instrument, an effect, a track's gain stage, a mix
/// bus.
///
/// [`process`](Node::process) and [`adopt`](Node::adopt) run on the audio
/// thread, or on one of the [`ParallelRenderer`]'s real-time workers, so they
/// must be real-time safe: no allocating or freeing memory, no locks, no
/// blocking and no I/O. Debug builds check for allocation (see [`rt`]). A
/// node can run on a different thread each block, though never on two at
/// once.
pub trait Node: Any + Send {
    /// How many channels the node outputs, fixed once it is in a graph.
    fn channels(&self) -> usize {
        2
    }

    /// Called off the audio thread when the graph is prepared, before the node
    /// renders. Allocate here.
    fn prepare(&mut self, _config: &EngineConfig) {}

    /// How many frames late the node's output is, against what comes in: the
    /// latency of a limiter that looks ahead or a linear-phase filter, say.
    /// Delay compensation delays the paths that meet this node's path, so
    /// they line up ([`Graph::prepare`]).
    ///
    /// Read off the audio thread, once the node is prepared. A node whose
    /// latency changes needs a new graph to change it.
    fn latency(&self) -> usize {
        0
    }

    /// Has the node follow `automation` for its parameter `param`, read at
    /// the song position, in place of the value set, for as long as it's in
    /// this graph. Called off the audio thread as the graph is built
    /// ([`Graph::automate`]), before [`prepare`](Node::prepare).
    ///
    /// A node that reads its automation itself, frame by frame with an
    /// [`automation::Param`], keeps it and returns `None`. By default the
    /// node hands it back, and the graph sends the node the automation's
    /// values as [`Param`](NodeEventKind::Param) events instead: on the
    /// frame of each point, every 32 frames of engine time in between, and
    /// on the first frame after the playhead jumps, whenever the value
    /// changed. Plugins get theirs that way, and smooth their parameters
    /// themselves.
    fn automate(&mut self, _param: ParamId, automation: Automation) -> Option<Automation> {
        Some(automation)
    }

    /// Called on the audio thread when a new graph replaces the running one,
    /// and this node replaces `previous`, the old node with the same key.
    ///
    /// Take over whatever keeps the output continuous, such as oscillator
    /// phase, filter memory or a plugin instance. Swap resources with
    /// `previous` rather than dropping them: `previous` is dropped later, on
    /// the control thread.
    fn adopt(&mut self, _previous: &mut dyn Node) {}

    /// Renders one block into `output`, applying `ctx.events` at their
    /// offsets.
    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>);

    /// Sends the notes the node plays in the block about to render, for a
    /// node whose notes go somewhere ([`Graph::send_notes`]), such as a MIDI
    /// clip player. The engine calls it on the audio thread before any node
    /// renders the block, so it must be real-time safe too.
    ///
    /// `ctx.events` holds the notes and controllers sent to the node for the
    /// block, in order of offset, from the nodes whose notes come to it and
    /// from the engine, so a MIDI effect can play them on, changed, and the
    /// node's parameter changes, so it plays them as its parameters are on
    /// each frame. The notes come here instead of with
    /// [`process`](Node::process), which gets the node's other events: its
    /// parameter changes again, and the modulation, which comes once the
    /// notes have played.
    fn play_notes(&mut self, _ctx: &ProcessContext<'_>, _notes: &mut Notes<'_>) {}

    /// Sends the notes the node made as it rendered the block `ctx` is for,
    /// for a node whose notes made while rendering go somewhere
    /// ([`Graph::send_made_notes`]), such as an instrument plugin that plays
    /// its arpeggios out, or one that hears notes in what comes into it.
    /// The engine calls it on the audio thread, or the worker rendering
    /// ahead, once the node's stage has rendered, so it must be real-time
    /// safe too. `ctx` has no events: the node had them in
    /// [`process`](Node::process), and keeps what it made there for this,
    /// over every part of the block it rendered as a block of its own.
    fn made_notes(&mut self, _ctx: &ProcessContext<'_>, _notes: &mut Notes<'_>) {}

    /// Tells the node that the notes it makes as it renders go somewhere in
    /// its graph ([`Graph::send_made_notes`]), so it keeps them for
    /// [`made_notes`](Node::made_notes) as it renders. Called off the audio
    /// thread as the graph is built, before [`prepare`](Node::prepare).
    fn makes_notes(&mut self) {}

    /// Sends the modulation the node makes for the block about to render,
    /// for a node that modulates other nodes' parameters
    /// ([`Graph::modulate`]), such as a channel's
    /// [`Modulation`](modulation::Modulation). The engine calls it on the
    /// audio thread once the notes have played and before any node renders
    /// the block, each node after the nodes that modulate it, so it must be
    /// real-time safe too.
    ///
    /// `ctx.events` holds the node's events for the block, in order of
    /// offset: its parameter changes, the notes sent to it, and the
    /// modulation sent to it by the nodes that modulate it. They come here
    /// instead of with [`process`](Node::process), which gets none.
    fn control(&mut self, _ctx: &ProcessContext<'_>, _controls: &mut Controls<'_>) {}

    /// Tells the node that something modulates its parameter `param` in its
    /// graph ([`Graph::modulate`]), so the parameter keeps the modulation
    /// the node it takes over from had for it ([`adopt`](Node::adopt)) until
    /// the next comes. A parameter that nothing modulates any more ramps
    /// its modulation away to nothing as the node takes over. Called off
    /// the audio thread as the graph is built, before
    /// [`prepare`](Node::prepare).
    fn modulated(&mut self, _param: ParamId) {}

    /// The notes whose voices the node ended in the block it last rendered.
    /// An instrument's voice can ring on well after its note is let go, so
    /// this is for modulators that run a copy for each voice, which stop it
    /// when the voice ends. Read on the audio thread, after the node
    /// renders. CLAP plugins and the built-in instruments say when their
    /// voices end, and nothing reads it yet.
    fn ended_voices(&self) -> &[NoteId] {
        &[]
    }

    /// Stops the node rendering, as when its device is bypassed: called on
    /// the audio thread after the last block it renders for a while. Until
    /// [`reset`](Node::reset) starts it again, it gets its events through
    /// [`flush`](Node::flush) instead of [`process`](Node::process). A
    /// plugin stops processing.
    fn stop(&mut self) {}

    /// Takes the parameter changes in `ctx.events`, whatever their offsets,
    /// while the node is stopped ([`stop`](Node::stop)), without rendering,
    /// so it starts again as they leave it. Its other events, such as
    /// notes, are dropped. Called on the audio thread for every block it's
    /// stopped for, events or not, so a plugin can take the changes made in
    /// its window too.
    fn flush(&mut self, _ctx: &ProcessContext<'_>) {}

    /// Starts the node again after [`stop`](Node::stop), as if it had
    /// rendered nothing yet: its delay lines, filters, envelopes and voices
    /// empty, and its parameters at their values, without a ramp. Called on
    /// the audio thread before the first block it renders again. A plugin
    /// resets its processing state.
    fn reset(&mut self) {}

    /// Whether the node is a live source now: it plays what comes in from
    /// outside the graph as it comes, from the audio devices' inputs
    /// ([`ProcessContext::input`]) or the MIDI inputs
    /// ([`ProcessContext::midi`]), as a track monitoring its input does.
    /// The engine renders it, and what it reaches, as the device asks for
    /// each block, rather than ahead ([`EngineHandle::set_lead`]): blocks it
    /// renders ahead have no inputs.
    ///
    /// `None`, the default, for a node that never is; `Some(false)` for
    /// one that can be but isn't now, such as a track's monitor that isn't
    /// monitoring. The graph asks each node once as it's prepared, and puts
    /// the ones that are in its live half ([`Graph::prepare`]), and it asks
    /// the ones that can be before each block, on the audio thread or a
    /// worker, so it must be quick and real-time safe. The engine renders
    /// an event for such a node in a block it renders as the device asks,
    /// so a parameter that makes it live, such as arming, applies in time,
    /// and while one outside the live half is live, it renders every block
    /// as the device asks.
    fn live(&self) -> Option<bool> {
        None
    }
}

impl dyn Node {
    /// The node as a `T`, if it is one. Meant for [`Node::adopt`].
    pub fn downcast_mut<T: Node>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut()
    }
}

/// What a node knows about the block it is rendering.
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext<'a> {
    /// The block's length. Every buffer the node sees is this long.
    pub frames: usize,
    pub sample_rate: u32,
    /// Engine time of the first frame: frames rendered since the engine
    /// started. Unlike the transport position, it never stops or jumps.
    pub time: u64,
    pub transport: TransportState,
    /// Where beats and bars fall in song positions. A musical position plays
    /// on the frame [`TempoMap::frame_at`] gives.
    pub tempo: &'a TempoMap,
    /// This node's events for the block, in order, each with an offset below
    /// `frames`.
    pub events: &'a [NodeEvent],
    /// The audio devices' inputs for the block, `frames` long: each input
    /// device's channels, one device after another, as the audio backend
    /// lays them out. No channels when the stream has no inputs. Frame `i`
    /// is what the inputs captured a fixed time before the block's frame
    /// `i` is heard: the input delay that monitoring adds, which the audio
    /// backend reports.
    pub input: AudioRef<'a>,
    /// The messages from the stream's MIDI inputs that play in the block,
    /// in order of [`time`](MidiMessage::time). A message plays at its
    /// time's offset from the block's [`time`](Self::time), or at the
    /// block's first frame if it came too late for its time.
    pub midi: &'a [MidiMessage],
    /// Whether the block renders ahead of the audio device
    /// ([`EngineHandle::set_lead`]), to be heard up to the lead later,
    /// rather than as the device asks for it. A plugin host passes it on,
    /// as VST3's prefetch mode.
    pub ahead: bool,
}

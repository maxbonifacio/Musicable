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
//! `docs/engine.md` describes the threads, the message flow and the real-time
//! rules.

use std::any::Any;

use musicable_model::time::TempoMap;

pub mod automation;
pub mod buffer;
pub mod clips;
pub mod compile;
pub mod devices;
mod engine;
pub mod event;
pub mod graph;
pub mod media;
pub mod meter;
pub mod midi;
pub mod nodes;
mod parallel;
pub mod preview;
mod render;
pub mod resample;
pub mod rt;
pub mod tasks;
pub mod timeline;
mod transport;

pub use automation::Automation;
pub use buffer::{AudioBuffer, AudioMut, AudioRef, Inputs};
pub use compile::{
    Compiled, Compiler, Devices, InputChannels, InputLayout, MidiSource, NodeMaker, compile,
    compile_metered, compile_with_devices, compile_with_inputs, compile_with_media,
    compile_with_stems,
};
pub use engine::{Engine, EngineHandle, PreviewAt, QueueFull, Stats, SwapError};
pub use event::{
    Event, NodeEvent, NodeEventKind, NoteEvent, NoteId, ParamId, TransportCommand, TransportState,
};
pub use graph::{Graph, GraphError, NodeId, NodeKey, Notes, PreparedGraph};
pub use meter::{Level, Meter, Meters};
pub use midi::{MidiMessage, MidiMonitor, MidiNotes, Played};
pub use parallel::{ParallelMonitor, ParallelOptions, ParallelRenderer, ParallelStats};
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
    /// from the engine, so a MIDI effect can play them on, changed. They come
    /// here instead of with [`process`](Node::process), which gets the
    /// node's other events.
    fn play_notes(&mut self, _ctx: &ProcessContext<'_>, _notes: &mut Notes<'_>) {}

    /// The notes whose voices the node ended in the block it last rendered.
    /// An instrument's voice can ring on well after its note is let go, so
    /// this is for modulators that run a copy for each voice, which stop it
    /// when the voice ends. Read on the audio thread, after the node
    /// renders. CLAP plugins and the built-in instruments say when their
    /// voices end, and nothing reads it yet.
    fn ended_voices(&self) -> &[NoteId] {
        &[]
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
}

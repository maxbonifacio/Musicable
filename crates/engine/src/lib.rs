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

pub mod buffer;
pub mod clips;
pub mod compile;
mod engine;
pub mod event;
pub mod graph;
pub mod media;
pub mod meter;
pub mod nodes;
mod parallel;
mod render;
mod resample;
pub mod rt;
mod transport;

pub use buffer::{AudioBuffer, AudioMut, AudioRef, Inputs};
pub use compile::{
    Compiled, Compiler, Devices, NodeMaker, compile, compile_metered, compile_with_devices,
    compile_with_media,
};
pub use engine::{Engine, EngineHandle, QueueFull, Stats, SwapError};
pub use event::{
    Event, NodeEvent, NodeEventKind, NoteEvent, NoteId, ParamId, TransportCommand, TransportState,
};
pub use graph::{Graph, GraphError, NodeId, NodeKey, Notes, PreparedGraph};
pub use meter::{Level, Meter, Meters};
pub use parallel::{ParallelMonitor, ParallelOptions, ParallelRenderer, ParallelStats};
pub use render::{Block, ReferenceRenderer, Renderer};

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
    /// `ctx.events` holds the notes sent to the node for the block, in order
    /// of offset, from the nodes whose notes come to it and from the engine,
    /// so a MIDI effect can play them on, changed. They come here instead of
    /// with [`process`](Node::process), which gets the node's other events.
    fn play_notes(&mut self, _ctx: &ProcessContext<'_>, _notes: &mut Notes<'_>) {}
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
}

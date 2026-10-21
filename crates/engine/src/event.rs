//! What the control thread sends the engine, and what nodes receive.

use musicable_model::time::{BeatRange, Beats};

use crate::graph::NodeKey;

/// A node parameter, numbered by the node type. Each node documents its
/// parameters as constants, such as [`Gain::GAIN`](crate::nodes::Gain::GAIN).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

/// An event for the engine, sent through
/// [`EngineHandle::send`](crate::EngineHandle::send) or
/// [`EngineHandle::send_at`](crate::EngineHandle::send_at).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Sets a node's parameter. Counted and dropped if no node in the running
    /// graph has the key.
    Param {
        node: NodeKey,
        param: ParamId,
        value: f64,
    },
    Transport(TransportCommand),
}

/// Transport changes. Musical positions are placed with the engine's tempo
/// map when the command applies, so they hold whatever the tempo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCommand {
    Play,
    /// Stops playback, keeping the position.
    Stop,
    /// Moves the playhead to a musical position: the frame it falls on (see
    /// [`TempoMap::frame_at`](musicable_model::time::TempoMap::frame_at)).
    /// Positions before the start of the song go to the start.
    Locate(Beats),
    /// Loops playback over a region, or stops looping with `None`.
    ///
    /// When the playhead reaches the end of the region, it jumps back to the
    /// start on the next frame. A playhead before the region plays into it;
    /// one already past the end plays on. A region shorter than a frame
    /// doesn't loop.
    Loop(Option<BeatRange>),
}

/// The transport during one block. The engine ends a block wherever the
/// transport changes and where a loop jumps back, so this holds for every
/// frame of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportState {
    pub playing: bool,
    /// Song position of the block's first frame, in samples. While playing,
    /// frame `i` of the block is at `position + i`.
    pub position: u64,
    /// The loop region, while looping is on.
    pub looping: Option<BeatRange>,
}

/// An event for one node, `offset` frames into the block being rendered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeEvent {
    pub offset: usize,
    pub kind: NodeEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum NodeEventKind {
    /// Takes effect from `offset` on. Nodes ignore parameters they don't have.
    Param { param: ParamId, value: f64 },
}

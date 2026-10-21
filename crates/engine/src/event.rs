//! What the control thread sends the engine, and what nodes receive.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCommand {
    Play,
    /// Stops playback, keeping the position.
    Stop,
    /// Moves the playhead to a song position, in samples.
    Locate(u64),
}

/// The transport during one block. The engine ends a block wherever the
/// transport changes, so this holds for every frame of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportState {
    pub playing: bool,
    /// Song position of the block's first frame, in samples. While playing,
    /// frame `i` of the block is at `position + i`.
    pub position: u64,
}

impl TransportState {
    pub(crate) fn apply(&mut self, command: TransportCommand) {
        match command {
            TransportCommand::Play => self.playing = true,
            TransportCommand::Stop => self.playing = false,
            TransportCommand::Locate(position) => self.position = position,
        }
    }
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

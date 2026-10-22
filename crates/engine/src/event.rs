//! What the control thread sends the engine, and what nodes receive.

use musicable_model::clip::Dimension;
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
    /// Plays a note on a node, such as an instrument, from outside the
    /// graph. Counted and dropped like a parameter event if no node has the
    /// key. Unlike parameter values, notes aren't sent again after a swap.
    Note {
        node: NodeKey,
        note: NoteEvent,
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
    /// How many times the playhead has jumped since the engine started: each
    /// locate counts, and each time a loop jumps back. A node that keeps
    /// track of what's playing, such as a clip player's sounding notes,
    /// compares it with the last block's to tell a jump from a tempo change,
    /// which moves the position in samples but not in beats.
    pub jumps: u64,
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
    /// Offsets a parameter from its value by `amount`, in the parameter's
    /// units, from `offset` on: what a modulator does, as CLAP's parameter
    /// modulation does. The value itself stays as it was, so the offset never
    /// ends up in the project, and the next modulation of the parameter
    /// replaces this one rather than adding to it. Nodes ignore it for
    /// parameters they don't have or can't modulate.
    Modulation { param: ParamId, amount: f64 },
    /// A note starts, changes or ends. Nodes that don't play notes ignore
    /// it.
    Note(NoteEvent),
}

/// Identifies a sounding note to the instrument playing it, from the note's
/// start to its end. Notes on the same key can sound at once, each with its
/// own expression, so instruments tell them apart by id, not by key, as MPE
/// does with channels and VST3 and CLAP with note ids.
///
/// A note from a clip has its clip's id and its own
/// ([`clips::note_id`](crate::clips::note_id)). A node that makes notes of
/// its own, such as an arpeggiator, picks ids that don't clash with the
/// notes it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NoteId(pub u64);

/// What happens to a note.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteEvent {
    /// The note starts. `velocity` is from 0.0 to 1.0. The note's expression
    /// starts at rest until an [`Expression`](Self::Expression) event says
    /// otherwise, which can come on the same frame, just after.
    On {
        note: NoteId,
        /// The MIDI note number, from 0 to 127.
        key: u8,
        velocity: f32,
    },
    /// One dimension of the note's expression changes, and holds until it
    /// changes again: pitch bend in semitones, pressure and timbre from 0.0
    /// to 1.0.
    Expression {
        note: NoteId,
        dimension: Dimension,
        value: f32,
    },
    /// The note is let go, at `velocity` from 0.0 to 1.0. Instruments let it
    /// ring out as they like. A note they don't know is ignored.
    Off { note: NoteId, velocity: f32 },
}

impl NoteEvent {
    pub fn note(&self) -> NoteId {
        match *self {
            Self::On { note, .. } | Self::Expression { note, .. } | Self::Off { note, .. } => note,
        }
    }
}

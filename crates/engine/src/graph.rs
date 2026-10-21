//! Building processing graphs and preparing them to run.

use std::cmp::Ordering;
use std::fmt;

use crate::buffer::{AudioBuffer, AudioRef};
use crate::engine::EVENT_CAPACITY;
use crate::parallel::Schedule;
use crate::{Block, EngineConfig, Node, NodeEvent, NodeEventKind, NoteEvent};

/// A node's identity across graph rebuilds, chosen by whoever builds the
/// graph. [`compile`](crate::compile()) derives keys from model ids.
///
/// When a new graph replaces the running one, each node takes over the state
/// of the old node with the same key, and parameter events find their node by
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(pub u64);

/// A node's index in one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    UnknownNode(NodeId),
    /// Connecting these nodes would create a feedback loop.
    Cycle,
    /// Two nodes have the same key.
    DuplicateKey(NodeKey),
    /// No output node was set.
    NoOutput,
    /// The node reports zero channels.
    NoChannels(NodeId),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "no node {}", id.0),
            Self::Cycle => write!(f, "the connection would create a feedback loop"),
            Self::DuplicateKey(key) => write!(f, "two nodes have key {}", key.0),
            Self::NoOutput => write!(f, "the graph has no output node"),
            Self::NoChannels(id) => write!(f, "node {} has no channels", id.0),
        }
    }
}

impl std::error::Error for GraphError {}

/// A processing graph being built: keyed nodes connected into a directed
/// acyclic graph. [`prepare`](Graph::prepare) turns it into something an
/// engine can run. Build graphs off the audio thread.
#[derive(Default)]
pub struct Graph {
    nodes: Vec<Box<dyn Node>>,
    keys: Vec<NodeKey>,
    /// `inputs[n]` lists the nodes feeding node `n`, in connection order.
    inputs: Vec<Vec<NodeId>>,
    /// `outputs[n]` lists the nodes that node `n` feeds.
    outputs: Vec<Vec<NodeId>>,
    output: Option<NodeId>,
    cue: Option<NodeId>,
    /// Which nodes' notes go to which, from [`send_notes`](Graph::send_notes).
    note_routes: Vec<(NodeId, NodeId)>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node. Keys must be unique, which [`prepare`](Graph::prepare)
    /// checks.
    pub fn add(&mut self, key: NodeKey, node: impl Node) -> NodeId {
        self.add_boxed(key, Box::new(node))
    }

    /// [`add`](Graph::add), for a node whose type is chosen at run time.
    pub fn add_boxed(&mut self, key: NodeKey, node: Box<dyn Node>) -> NodeId {
        self.nodes.push(node);
        self.keys.push(key);
        self.inputs.push(Vec::new());
        self.outputs.push(Vec::new());
        NodeId(self.nodes.len() - 1)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Feeds `from`'s output into `to`.
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.reaches(to, from) {
            return Err(GraphError::Cycle);
        }
        self.inputs[to.0].push(from);
        self.outputs[from.0].push(to);
        Ok(())
    }

    /// Sends the notes `from` plays to `to`, such as a MIDI clip player's to
    /// an instrument. Notes don't need an audio connection, and can go
    /// against the flow of audio: before any node renders a block, the
    /// engine has each node whose notes go somewhere send the block's notes
    /// ([`Node::play_notes`]), after the nodes whose notes it gets. So notes
    /// can pass through a chain of nodes, such as a MIDI effect between a
    /// clip player and an instrument, but not around a loop: a route that
    /// would close one is refused.
    pub fn send_notes(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.notes_reach(to, from) {
            return Err(GraphError::Cycle);
        }
        if !self.note_routes.contains(&(from, to)) {
            self.note_routes.push((from, to));
        }
        Ok(())
    }

    /// Sets the node whose output the engine plays.
    pub fn set_output(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        self.output = Some(node);
        Ok(())
    }

    /// Sets the cue output: a node the engine plays alongside the output,
    /// not through it, such as the metronome. The engine adds it to the
    /// device's buffer after the output, so it's in what you hear but not
    /// in the mix.
    pub fn set_cue(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        self.cue = Some(node);
        Ok(())
    }

    /// Orders the nodes, calls [`Node::prepare`] on each and allocates their
    /// buffers.
    ///
    /// # Panics
    ///
    /// If the sample rate or the block size in `config` is zero.
    pub fn prepare(mut self, config: &EngineConfig) -> Result<PreparedGraph, GraphError> {
        assert!(
            config.sample_rate > 0 && config.max_block > 0,
            "invalid engine config {config:?}"
        );
        let output = self.output.ok_or(GraphError::NoOutput)?;
        let mut by_key: Vec<(NodeKey, NodeId)> =
            self.keys.iter().copied().zip((0..).map(NodeId)).collect();
        by_key.sort_unstable();
        if let Some(pair) = by_key.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(GraphError::DuplicateKey(pair[0].0));
        }
        let mut buffers = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.prepare(config);
            let channels = node.channels();
            if channels == 0 {
                return Err(GraphError::NoChannels(NodeId(i)));
            }
            buffers.push(AudioBuffer::new(channels, config.max_block));
        }
        let order = self.topological_order();
        let schedule = Schedule::new(&self.inputs, &self.outputs, &order);
        self.note_routes.sort_unstable();
        let mut players: Vec<Player> = Vec::new();
        for &(from, to) in &self.note_routes {
            match players.last_mut() {
                Some(player) if player.node == from => player.targets.push(to),
                _ => players.push(Player {
                    node: from,
                    targets: vec![to],
                }),
            }
        }
        // Each player after the players whose notes it gets. Notes don't go
        // around loops, so one is always ready.
        let routes = &self.note_routes;
        let mut ordered: Vec<Player> = Vec::with_capacity(players.len());
        while !players.is_empty() {
            let waiting = |p: &Player| {
                let from_waiting = |from: NodeId| players.iter().any(|q| q.node == from);
                routes
                    .iter()
                    .any(|&(from, to)| to == p.node && from_waiting(from))
            };
            let ready = players.iter().position(|p| !waiting(p)).expect("no loops");
            ordered.push(players.remove(ready));
        }
        let mut plays_notes = vec![false; self.nodes.len()];
        let mut inbox = 0;
        for player in &ordered {
            let routes_in = routes.iter().filter(|&&(_, to)| to == player.node).count();
            plays_notes[player.node.0] = true;
            inbox = inbox.max(EVENT_CAPACITY + routes_in * Notes::CAPACITY);
        }
        // Room for every parameter event the engine can hold, and for each
        // player's notes to each of its targets.
        let notes: usize = ordered.iter().map(|p| p.targets.len()).sum();
        let capacity = EVENT_CAPACITY + notes * Notes::CAPACITY;
        Ok(PreparedGraph {
            config: *config,
            event_ranges: vec![(0, 0); self.nodes.len()],
            nodes: self.nodes,
            inputs: self.inputs,
            order,
            output,
            cue: self.cue,
            buffers,
            by_key,
            events: Vec::with_capacity(capacity),
            routed: Vec::with_capacity(capacity),
            players: ordered,
            plays_notes,
            inbox: Vec::with_capacity(inbox),
            inbox_events: Vec::with_capacity(inbox),
            schedule,
        })
    }

    fn check(&self, id: NodeId) -> Result<(), GraphError> {
        if id.0 < self.nodes.len() {
            Ok(())
        } else {
            Err(GraphError::UnknownNode(id))
        }
    }

    /// Whether notes can pass from `start` to `target`.
    fn notes_reach(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                let next = self.note_routes.iter().filter(|&&(from, _)| from == n);
                stack.extend(next.map(|&(_, to)| to));
            }
        }
        false
    }

    /// Whether signal can flow from `start` to `target`.
    fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                stack.extend(&self.outputs[n.0]);
            }
        }
        false
    }

    /// Nodes in an order where every node comes after all of its inputs.
    fn topological_order(&self) -> Vec<NodeId> {
        let mut remaining: Vec<usize> = self.inputs.iter().map(Vec::len).collect();
        let mut ready: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&n| remaining[n] == 0)
            .map(NodeId)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(node) = ready.pop() {
            order.push(node);
            for &next in &self.outputs[node.0] {
                remaining[next.0] -= 1;
                if remaining[next.0] == 0 {
                    ready.push(next);
                }
            }
        }
        debug_assert_eq!(order.len(), self.nodes.len(), "connect() rejects cycles");
        order
    }
}

/// A graph ready to run, for one [`EngineConfig`]: nodes in order and their
/// buffers allocated. Hand it to the engine with
/// [`EngineHandle::swap_graph`](crate::EngineHandle::swap_graph).
pub struct PreparedGraph {
    pub(crate) config: EngineConfig,
    pub(crate) nodes: Vec<Box<dyn Node>>,
    pub(crate) inputs: Vec<Vec<NodeId>>,
    /// Every node comes after all of its inputs.
    pub(crate) order: Vec<NodeId>,
    pub(crate) output: NodeId,
    pub(crate) cue: Option<NodeId>,
    /// Each node's output for the current block.
    pub(crate) buffers: Vec<AudioBuffer>,
    /// Sorted by key.
    pub(crate) by_key: Vec<(NodeKey, NodeId)>,
    /// The current block's node events, grouped by node.
    pub(crate) events: Vec<NodeEvent>,
    /// Where each node's events are in `events`.
    pub(crate) event_ranges: Vec<(u32, u32)>,
    /// The next block's node events, as they arrive. It has room for as many
    /// as `events`.
    routed: Vec<Routed>,
    /// The nodes that play notes, each after those whose notes it gets, and
    /// where their notes go.
    players: Vec<Player>,
    /// Whether each node is a player: it gets the notes sent to it with
    /// [`Node::play_notes`] rather than [`Node::process`].
    plays_notes: Vec<bool>,
    /// A player's notes as they're gathered for it, and then in order.
    inbox: Vec<Routed>,
    inbox_events: Vec<NodeEvent>,
    /// How the parallel renderer runs the graph.
    pub(crate) schedule: Schedule,
}

/// A node event on its way into a graph, with its arrival order.
#[derive(Clone, Copy)]
pub(crate) struct Routed {
    pub node: NodeId,
    pub order: u32,
    pub event: NodeEvent,
}

/// A node that plays notes, and the nodes its notes go to.
struct Player {
    node: NodeId,
    targets: Vec<NodeId>,
}

/// Where a node puts the notes it plays in a block: see
/// [`Node::play_notes`].
pub struct Notes<'a> {
    routed: &'a mut Vec<Routed>,
    targets: &'a [NodeId],
    frames: usize,
    /// How many more notes the node can send this block.
    room: usize,
    /// Notes there was no room for.
    dropped: u64,
}

impl Notes<'_> {
    /// The most note events a node can send in one block. Two seconds of
    /// dense MPE playing, ten fingers at once, comes to a few hundred in a
    /// block of 2,048 frames.
    pub const CAPACITY: usize = 512;

    /// Sends `note` to the nodes this node's notes go to, `offset` frames
    /// into the block. Send a block's notes in order of offset. Returns
    /// whether there was room for it: notes beyond a block's
    /// [`CAPACITY`](Self::CAPACITY) are dropped, and counted in
    /// [`Stats::events_dropped`](crate::Stats::events_dropped).
    pub fn send(&mut self, offset: usize, note: NoteEvent) -> bool {
        debug_assert!(offset < self.frames, "notes are sent within the block");
        if self.room == 0 {
            self.dropped += 1;
            return false;
        }
        self.room -= 1;
        for &node in self.targets {
            let order = self.routed.len() as u32;
            self.routed.push(Routed {
                node,
                order,
                event: NodeEvent {
                    offset,
                    kind: NodeEventKind::Note(note),
                },
            });
        }
        true
    }

    /// The block's length: notes are sent at offsets below it.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

impl PreparedGraph {
    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether a node has `key`.
    pub fn contains(&self, key: NodeKey) -> bool {
        self.find(key).is_some()
    }

    /// The output node's most recent `frames` frames.
    pub fn output(&self, frames: usize) -> AudioRef<'_> {
        self.buffers[self.output.0].view(frames)
    }

    /// The cue node's most recent `frames` frames, if the graph has one (see
    /// [`Graph::set_cue`]).
    pub fn cue(&self, frames: usize) -> Option<AudioRef<'_>> {
        self.cue.map(|cue| self.buffers[cue.0].view(frames))
    }

    pub(crate) fn find(&self, key: NodeKey) -> Option<NodeId> {
        self.by_key
            .binary_search_by_key(&key, |&(k, _)| k)
            .ok()
            .map(|i| self.by_key[i].1)
    }

    /// Lets each node take over from the node with the same key in `old`.
    /// Runs on the audio thread.
    pub(crate) fn adopt_from(&mut self, old: &mut PreparedGraph) {
        let (mut i, mut j) = (0, 0);
        while let (Some(&(key, id)), Some(&(old_key, old_id))) =
            (self.by_key.get(i), old.by_key.get(j))
        {
            match key.cmp(&old_key) {
                Ordering::Less => i += 1,
                Ordering::Greater => j += 1,
                Ordering::Equal => {
                    self.nodes[id.0].adopt(old.nodes[old_id.0].as_mut());
                    i += 1;
                    j += 1;
                }
            }
        }
    }

    /// Queues an event for the next block. Returns whether there was room:
    /// there always is for the events the engine holds.
    pub(crate) fn route(&mut self, node: NodeId, event: NodeEvent) -> bool {
        if self.routed.len() == self.routed.capacity() {
            return false;
        }
        let order = self.routed.len() as u32;
        self.routed.push(Routed { node, order, event });
        true
    }

    /// Has each node that plays notes send its notes for `block`, and
    /// returns how many there was no room for. Runs on the audio thread,
    /// before the block renders.
    pub(crate) fn play_notes(&mut self, block: &Block<'_>) -> u64 {
        let Self {
            config,
            nodes,
            routed,
            players,
            plays_notes,
            inbox,
            inbox_events,
            ..
        } = self;
        let is_note = |r: &Routed| matches!(r.event.kind, NodeEventKind::Note(_));
        let mut dropped = 0;
        for player in players.iter() {
            // The notes sent to it so far, which is all of them: the engine
            // routed its own before the block, and the players they come
            // from have played.
            inbox.clear();
            let sent = routed
                .iter()
                .filter(|r| r.node == player.node && is_note(r));
            inbox.extend(sent);
            inbox.sort_unstable_by_key(|r| (r.event.offset, r.order));
            inbox_events.clear();
            inbox_events.extend(inbox.iter().map(|r| r.event));
            let ctx = block.context(config.sample_rate, inbox_events);
            let mut notes = Notes {
                routed,
                targets: &player.targets,
                frames: block.frames,
                room: Notes::CAPACITY,
                dropped: 0,
            };
            nodes[player.node.0].play_notes(&ctx, &mut notes);
            dropped += notes.dropped;
        }
        // The players have had theirs.
        if !players.is_empty() {
            routed.retain(|r| !(plays_notes[r.node.0] && is_note(r)));
        }
        dropped
    }

    /// Stores the events queued for the block about to render, and empties
    /// the queue.
    pub(crate) fn load_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        self.load_events(&mut routed);
        self.routed = routed;
    }

    /// Clears what [`load_routed`](Self::load_routed) stored, once the block
    /// has rendered.
    pub(crate) fn clear_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        self.clear_events(&routed);
        routed.clear();
        self.routed = routed;
    }

    /// Stores a block's node events, grouped by node, in order of offset and
    /// otherwise in arrival order. `routed` must fit in `events`.
    pub(crate) fn load_events(&mut self, routed: &mut [Routed]) {
        routed.sort_unstable_by_key(|r| (r.node, r.event.offset, r.order));
        for r in routed.iter() {
            let index = self.events.len() as u32;
            self.events.push(r.event);
            let range = &mut self.event_ranges[r.node.0];
            if range.0 == range.1 {
                *range = (index, index + 1);
            } else {
                range.1 = index + 1;
            }
        }
    }

    pub(crate) fn clear_events(&mut self, routed: &[Routed]) {
        for r in routed {
            self.event_ranges[r.node.0] = (0, 0);
        }
        self.events.clear();
    }
}

impl fmt::Debug for PreparedGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedGraph")
            .field("config", &self.config)
            .field("nodes", &self.nodes.len())
            .field("output", &self.output)
            .field("cue", &self.cue)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::TempoMap;

    use super::*;
    use crate::nodes::{Constant, Sum};
    use crate::{
        AudioMut, Inputs, NoteId, ProcessContext, ReferenceRenderer, Renderer, TransportState, rt,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    #[test]
    fn feedback_loops_are_rejected() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), Sum::default());
        let b = g.add(NodeKey(1), Sum::default());
        let c = g.add(NodeKey(2), Sum::default());
        g.connect(a, b).unwrap();
        g.connect(b, c).unwrap();
        assert_eq!(g.connect(c, a), Err(GraphError::Cycle));
        assert_eq!(g.connect(a, a), Err(GraphError::Cycle));
        assert_eq!(
            g.connect(a, NodeId(7)),
            Err(GraphError::UnknownNode(NodeId(7)))
        );
    }

    #[test]
    fn keys_must_be_unique() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(3), Constant::new(1.0));
        g.add(NodeKey(3), Constant::new(2.0));
        g.set_output(a).unwrap();
        assert_eq!(
            g.prepare(&CONFIG).unwrap_err(),
            GraphError::DuplicateKey(NodeKey(3))
        );
    }

    #[test]
    fn a_graph_needs_an_output() {
        let mut g = Graph::new();
        g.add(NodeKey(0), Constant::new(1.0));
        assert_eq!(g.prepare(&CONFIG).unwrap_err(), GraphError::NoOutput);
    }

    #[test]
    fn nodes_come_after_their_inputs() {
        // A layered graph where every node feeds several later ones.
        let mut g = Graph::new();
        let ids: Vec<NodeId> = (0..40).map(|i| g.add(NodeKey(i), Sum::default())).collect();
        for (i, &to) in ids.iter().enumerate() {
            for &from in ids[..i].iter().rev().step_by(3).take(4) {
                g.connect(from, to).unwrap();
            }
        }
        g.set_output(ids[39]).unwrap();
        let prepared = g.prepare(&CONFIG).unwrap();
        let mut position = vec![0; prepared.len()];
        for (p, node) in prepared.order.iter().enumerate() {
            position[node.0] = p;
        }
        for (node, inputs) in prepared.inputs.iter().enumerate() {
            for input in inputs {
                assert!(position[input.0] < position[node]);
            }
        }
    }

    /// Plays a note through each block, and counts the notes sent to it.
    #[derive(Default)]
    struct Pulse(usize);

    impl Node for Pulse {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            self.0 += ctx.events.len();
            let note = NoteId(ctx.transport.position);
            notes.send(
                0,
                NoteEvent::On {
                    note,
                    key: 60,
                    velocity: 1.0,
                },
            );
            notes.send(
                ctx.frames - 1,
                NoteEvent::Off {
                    note,
                    velocity: 0.5,
                },
            );
        }
    }

    /// Plays the notes it gets an octave up, and counts the events
    /// `process` gets.
    #[derive(Default)]
    struct OctaveUp(usize);

    impl Node for OctaveUp {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.0 += ctx.events.len();
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(mut note) = event.kind {
                    if let NoteEvent::On { key, .. } = &mut note {
                        *key += 12;
                    }
                    notes.send(event.offset, note);
                }
            }
        }
    }

    /// Keeps the notes it gets, with their offsets.
    struct Keep(Vec<(usize, NoteEvent)>);

    impl Node for Keep {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(note) = event.kind {
                    self.0.push((event.offset, note));
                }
            }
            output.clear();
        }
    }

    #[test]
    fn notes_pass_through_a_chain_of_nodes() {
        let mut g = Graph::new();
        // Added in another order than the notes flow.
        let octave = g.add(NodeKey(0), OctaveUp::default());
        let pulse = g.add(NodeKey(1), Pulse::default());
        let keep = g.add(NodeKey(2), Keep(Vec::with_capacity(64)));
        g.send_notes(octave, keep).unwrap();
        g.send_notes(pulse, octave).unwrap();
        assert_eq!(g.send_notes(keep, pulse), Err(GraphError::Cycle));
        assert_eq!(g.send_notes(keep, keep), Err(GraphError::Cycle));
        g.set_output(keep).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let tempo = TempoMap::default();
        for position in [0, 64] {
            let block = Block {
                frames: 64,
                time: 0,
                transport: TransportState {
                    playing: true,
                    position,
                    ..TransportState::default()
                },
                tempo: &tempo,
            };
            // Notes from the engine, as `Event::Note` sends them, come to a
            // player with the notes of the players before it.
            let from_engine = |key| NodeEvent {
                offset: 10,
                kind: NodeEventKind::Note(NoteEvent::On {
                    note: NoteId(1000 + position),
                    key,
                    velocity: 1.0,
                }),
            };
            if position == 64 {
                assert!(graph.route(octave, from_engine(50)));
                assert!(graph.route(pulse, from_engine(0)));
            }
            rt::scope(|| {
                assert_eq!(graph.play_notes(&block), 0);
                graph.load_routed();
                ReferenceRenderer.render(&mut graph, &block);
                graph.clear_routed();
            });
        }
        let kept = &graph.nodes[keep.0]
            .as_mut()
            .downcast_mut::<Keep>()
            .unwrap()
            .0;
        let on = |note, key| NoteEvent::On {
            note: NoteId(note),
            key,
            velocity: 1.0,
        };
        let off = |note| NoteEvent::Off {
            note: NoteId(note),
            velocity: 0.5,
        };
        assert_eq!(
            *kept,
            [
                (0, on(0, 72)),
                (63, off(0)),
                (0, on(64, 72)),
                (10, on(1064, 62)),
                (63, off(64))
            ]
        );
        // The notes came to the players with `play_notes`, not `process`.
        let octave = graph.nodes[octave.0]
            .as_mut()
            .downcast_mut::<OctaveUp>()
            .unwrap();
        assert_eq!(octave.0, 0);
        let pulse = graph.nodes[pulse.0]
            .as_mut()
            .downcast_mut::<Pulse>()
            .unwrap();
        assert_eq!(pulse.0, 1);
    }

    #[test]
    fn nodes_are_found_by_key() {
        let mut g = Graph::new();
        g.add(NodeKey(30), Constant::new(1.0));
        let b = g.add(NodeKey(10), Constant::new(1.0));
        g.add(NodeKey(20), Constant::new(1.0));
        g.set_output(b).unwrap();
        let prepared = g.prepare(&CONFIG).unwrap();
        assert_eq!(prepared.find(NodeKey(10)), Some(b));
        assert_eq!(prepared.find(NodeKey(20)), Some(NodeId(2)));
        assert!(!prepared.contains(NodeKey(15)));
    }
}

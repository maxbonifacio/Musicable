//! Building processing graphs and preparing them to run.

use std::cmp::Ordering;
use std::fmt;

use crate::buffer::{AudioBuffer, AudioRef};
use crate::engine::EVENT_CAPACITY;
use crate::parallel::Schedule;
use crate::{EngineConfig, Node, NodeEvent};

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
            events: Vec::with_capacity(EVENT_CAPACITY),
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
    /// How the parallel renderer runs the graph.
    pub(crate) schedule: Schedule,
}

/// A node event on its way into a graph, with its arrival order.
pub(crate) struct Routed {
    pub node: NodeId,
    pub order: u32,
    pub event: NodeEvent,
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

    /// Stores a block's node events, grouped by node and otherwise in arrival
    /// order. `routed` must fit in [`EVENT_CAPACITY`].
    pub(crate) fn load_events(&mut self, routed: &mut [Routed]) {
        routed.sort_unstable_by_key(|r| (r.node, r.order));
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
    use super::*;
    use crate::nodes::{Constant, Sum};

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

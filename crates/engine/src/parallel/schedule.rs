//! What the parallel renderer needs to know about a graph, worked out when the
//! graph is prepared so the audio thread doesn't have to.

use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::{AcqRel, Relaxed};

use crate::NodeId;
use crate::buffer::{AudioBuffer, RawBuffer};

/// A graph's dependencies, in the form the parallel renderer runs them.
///
/// Each node with several inputs has a counter of inputs still to finish in
/// the current block. The thread that brings it to zero runs the node or
/// queues it, and resets the counter for the next block. A node with one
/// input is ready as soon as that input finishes, and nodes without inputs
/// start each block.
///
/// A graph with notes made while rendering renders in stages, one after
/// another ([`Graph::send_made_notes`](crate::Graph::send_made_notes)), so
/// each stage is run as a graph of its own: a node's inputs in a stage
/// before its own have finished by the time it starts, and only those in
/// its own stage count, as do only the nodes it feeds there.
#[derive(Debug)]
pub(crate) struct Schedule {
    /// `dependents[offsets[n]..offsets[n + 1]]` are the nodes node `n` feeds
    /// in its stage, once per connection, the ones with the longest way to
    /// go first.
    offsets: Box<[u32]>,
    dependents: Box<[u32]>,
    /// How many connections feed each node in its stage.
    in_degree: Box<[u32]>,
    /// Connections into each node whose source hasn't finished this block.
    /// Unused for nodes with one input.
    pending: Box<[AtomicU32]>,
    /// Each stage's nodes without inputs in it, the ones with the longest
    /// way to go last: they are taken from the end.
    sources: Box<[Box<[u32]>]>,
    /// The most nodes of each stage that can run at once: the widest level
    /// of the stage. An estimate of how many threads it can use.
    widths: Box<[usize]>,
    /// Each node's output buffer, set before each block the parallel renderer
    /// runs.
    pub(super) buffers: Box<[RawBuffer]>,
}

impl Schedule {
    /// `inputs` and `outputs` list each node's connections, every node in
    /// `order` comes after all of its inputs, and `stage_of` says which of
    /// `stages` stages each node renders in: none before any of its inputs'.
    pub(crate) fn new(
        inputs: &[Vec<NodeId>],
        outputs: &[Vec<NodeId>],
        order: &[NodeId],
        stage_of: &[u32],
        stages: usize,
    ) -> Self {
        let len = inputs.len();
        let same = |a: NodeId, b: usize| stage_of[a.0] == stage_of[b];
        // The most nodes on any path from each node to the end of its stage,
        // counting itself: what's left to do once it starts.
        let mut height = vec![0u32; len];
        for &node in order.iter().rev() {
            let fed = outputs[node.0].iter().filter(|&&d| same(d, node.0));
            let longest = fed.map(|d| height[d.0]).max();
            height[node.0] = 1 + longest.unwrap_or(0);
        }
        // The most nodes on any path into each node in its stage: its level.
        let mut level = vec![0usize; len];
        let mut per_level = vec![vec![0usize; len]; stages];
        for &node in order {
            let feeding = inputs[node.0].iter().filter(|&&i| same(i, node.0));
            let deepest = feeding.map(|i| level[i.0] + 1).max();
            level[node.0] = deepest.unwrap_or(0);
            per_level[stage_of[node.0] as usize][level[node.0]] += 1;
        }

        let mut offsets = Vec::with_capacity(len + 1);
        let mut dependents = Vec::with_capacity(outputs.iter().map(Vec::len).sum());
        offsets.push(0);
        for (node, fed) in outputs.iter().enumerate() {
            let start = dependents.len();
            let fed = fed.iter().filter(|&&d| same(d, node));
            dependents.extend(fed.map(|d| index(d.0)));
            dependents[start..].sort_by_key(|&d| std::cmp::Reverse(height[d as usize]));
            offsets.push(index(dependents.len()));
        }
        let in_degree: Box<[u32]> = (inputs.iter().enumerate())
            .map(|(node, i)| index(i.iter().filter(|&&i| same(i, node)).count()))
            .collect();
        let sources = (0..stages)
            .map(|stage| {
                let mut sources: Vec<u32> = (0..len)
                    .filter(|&n| in_degree[n] == 0 && stage_of[n] as usize == stage)
                    .map(index)
                    .collect();
                sources.sort_by_key(|&n| height[n as usize]);
                sources.into_boxed_slice()
            })
            .collect();
        let widths = (per_level.into_iter())
            .map(|levels| levels.into_iter().max().unwrap_or(0))
            .collect();
        Self {
            pending: in_degree.iter().map(|&d| AtomicU32::new(d)).collect(),
            in_degree,
            offsets: offsets.into(),
            dependents: dependents.into(),
            sources,
            widths,
            buffers: vec![RawBuffer::NULL; len].into(),
        }
    }

    /// Stage `stage`'s nodes without inputs in it, the ones with the longest
    /// way to go last.
    pub(super) fn sources(&self, stage: usize) -> &[u32] {
        &self.sources[stage]
    }

    /// The most nodes of stage `stage` that can run at once.
    pub(super) fn width(&self, stage: usize) -> usize {
        self.widths[stage]
    }

    /// The nodes `node` feeds in its stage, once per connection.
    pub(super) fn dependents(&self, node: u32) -> &[u32] {
        let n = node as usize;
        &self.dependents[self.offsets[n] as usize..self.offsets[n + 1] as usize]
    }

    /// Counts one of `node`'s inputs as finished in the current block, and
    /// returns whether that was the last. A node with one input needs no
    /// counting, which saves most nodes in a channel strip an atomic
    /// operation.
    pub(super) fn input_done(&self, node: u32) -> bool {
        let n = node as usize;
        let in_degree = self.in_degree[n];
        if in_degree == 1 {
            return true;
        }
        let pending = &self.pending[n];
        if pending.fetch_sub(1, AcqRel) != 1 {
            return false;
        }
        // That was the last. Nothing else counts this node down this block,
        // so reset it for the next one.
        pending.store(in_degree, Relaxed);
        true
    }

    /// How many connections feed `node`.
    #[cfg(test)]
    pub(super) fn in_degree(&self, node: u32) -> u32 {
        self.in_degree[node as usize]
    }

    /// Points [`Schedule::buffers`] at `buffers`, the graph's output buffers.
    /// They don't move, so this only writes the first time: every thread
    /// reads these each block, and rewriting them would take them out of
    /// every other core's cache.
    pub(super) fn point_at(&mut self, buffers: &mut [AudioBuffer]) {
        for (raw, buffer) in self.buffers.iter_mut().zip(buffers) {
            let new = buffer.raw();
            if *raw != new {
                *raw = new;
            }
        }
    }
}

fn index(n: usize) -> u32 {
    u32::try_from(n).expect("graphs have fewer than 2^32 nodes and connections")
}

#[cfg(test)]
mod tests {
    use crate::nodes::Sum;
    use crate::{EngineConfig, Graph, NodeKey};

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 16,
    };

    #[test]
    fn longer_paths_start_first() {
        // a → b → c → out, d → out, e → b, e → out
        let mut g = Graph::new();
        let [a, b, c, d, e, out] = [0, 1, 2, 3, 4, 5].map(|k| g.add(NodeKey(k), Sum::default()));
        for (from, to) in [(a, b), (b, c), (c, out), (d, out), (e, out), (e, b)] {
            g.connect(from, to).unwrap();
        }
        g.set_output(out).unwrap();
        let s = g.prepare(&CONFIG).unwrap().schedule;
        // Sources: d has two nodes to go, a and e four each. They're taken
        // from the end: e, a, then d.
        assert_eq!(s.sources(0), [d.0 as u32, a.0 as u32, e.0 as u32]);
        // e feeds b, which has three to go, before the output.
        assert_eq!(s.dependents(e.0 as u32), [b.0 as u32, out.0 as u32]);
        assert_eq!(s.in_degree(out.0 as u32), 3);
        assert_eq!(s.in_degree(b.0 as u32), 2);
        // Levels: {a, d, e}, {b}, {c}, {out}.
        assert_eq!(s.width(0), 3);
    }

    #[test]
    fn repeated_connections_count_twice() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), Sum::default());
        let b = g.add(NodeKey(1), Sum::default());
        g.connect(a, b).unwrap();
        g.connect(a, b).unwrap();
        g.set_output(b).unwrap();
        let s = g.prepare(&CONFIG).unwrap().schedule;
        assert_eq!(s.dependents(0), [1, 1]);
        assert_eq!(s.in_degree(1), 2);
    }
}

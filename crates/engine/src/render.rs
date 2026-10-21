//! Renderers run every node of a graph once per block.

use crate::{Inputs, PreparedGraph, ProcessContext, TransportState};

/// One block, as a renderer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub frames: usize,
    /// Engine time of the first frame.
    pub time: u64,
    pub transport: TransportState,
}

/// Runs a graph for one block, on the audio thread.
///
/// The engine owns one renderer. [`ReferenceRenderer`] is the default; the
/// parallel scheduler will be another, and must match it sample for sample.
pub trait Renderer: Send {
    /// Processes every node of `graph` once, each after all of its inputs.
    /// The engine has already put each node's events for the block into
    /// `graph`.
    fn render(&mut self, graph: &mut PreparedGraph, block: &Block);
}

/// Processes one node at a time in topological order, on the calling thread.
///
/// Deliberately simple: it is the oracle the parallel scheduler is tested
/// against, not the production path.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceRenderer;

impl Renderer for ReferenceRenderer {
    fn render(&mut self, graph: &mut PreparedGraph, block: &Block) {
        let PreparedGraph {
            config,
            nodes,
            inputs,
            order,
            buffers,
            events,
            event_ranges,
            ..
        } = graph;
        for &node in order.iter() {
            // Take the node's buffer out so its inputs can be read alongside
            // it. No node feeds itself, and taking leaves an empty buffer, so
            // nothing is allocated or freed.
            let mut output = std::mem::take(&mut buffers[node.0]);
            let (start, end) = event_ranges[node.0];
            let ctx = ProcessContext {
                frames: block.frames,
                sample_rate: config.sample_rate,
                time: block.time,
                transport: block.transport,
                events: &events[start as usize..end as usize],
            };
            let inputs = Inputs::graph(buffers, &inputs[node.0], block.frames);
            nodes[node.0].process(&ctx, inputs, &mut output.view_mut(block.frames));
            buffers[node.0] = output;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::{Constant, Gain, Sum};
    use crate::{EngineConfig, Graph, NodeKey};

    fn render(graph: Graph, max_block: usize, frames: usize) -> PreparedGraph {
        let config = EngineConfig {
            sample_rate: 48_000,
            max_block,
        };
        let mut prepared = graph.prepare(&config).unwrap();
        let block = Block {
            frames,
            time: 0,
            transport: TransportState::default(),
        };
        ReferenceRenderer.render(&mut prepared, &block);
        prepared
    }

    #[test]
    fn two_tracks_into_master() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), Constant::new(0.25));
        let b = g.add(NodeKey(1), Constant::new(0.5).with_channels(1));
        let track_a = g.add(NodeKey(2), Gain::new(2.0));
        let track_b = g.add(NodeKey(3), Gain::new(0.5));
        let master = g.add(NodeKey(4), Sum::default());
        g.connect(a, track_a).unwrap();
        g.connect(b, track_b).unwrap();
        g.connect(track_a, master).unwrap();
        g.connect(track_b, master).unwrap();
        g.set_output(master).unwrap();

        let graph = render(g, 64, 64);
        let out = graph.output(64);
        assert_eq!(out.channels(), 2);
        assert!(out.iter().flatten().all(|&s| s == 0.75));
    }

    #[test]
    fn blocks_can_be_shorter_than_the_maximum() {
        let mut g = Graph::new();
        let c = g.add(NodeKey(0), Constant::new(1.0));
        g.set_output(c).unwrap();
        let graph = render(g, 64, 5);
        assert_eq!(graph.output(5).channel(1), [1.0; 5]);
        // The rest of the buffer was never written.
        assert_eq!(graph.output(64).channel(1)[5..], [0.0; 59]);
    }
}

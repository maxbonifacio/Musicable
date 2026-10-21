use crate::{Graph, NodeId};

/// Single-threaded renderer that processes nodes in topological order.
///
/// Deliberately simple: it is the oracle the parallel scheduler is tested
/// against, not the production path.
pub struct ReferenceRenderer {
    graph: Graph,
    order: Vec<NodeId>,
    buffers: Vec<Vec<f32>>,
    output: NodeId,
}

impl ReferenceRenderer {
    pub fn new(graph: Graph, output: NodeId, block_size: usize) -> Self {
        let order = graph.topological_order();
        let buffers = vec![vec![0.0; block_size]; graph.len()];
        Self {
            graph,
            order,
            buffers,
            output,
        }
    }

    /// Renders one block and returns the output node's buffer.
    pub fn render_block(&mut self) -> &[f32] {
        for &node in &self.order {
            let mut out = std::mem::take(&mut self.buffers[node.0]);
            let inputs: Vec<&[f32]> = self.graph.inputs[node.0]
                .iter()
                .map(|i| self.buffers[i.0].as_slice())
                .collect();
            self.graph.nodes[node.0].process(&inputs, &mut out);
            self.buffers[node.0] = out;
        }
        &self.buffers[self.output.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Constant, Gain, GraphError, Sum};

    #[test]
    fn two_tracks_into_master() {
        let mut g = Graph::new();
        let a = g.add(Constant(0.25));
        let b = g.add(Constant(0.5));
        let track_a = g.add(Gain(2.0));
        let track_b = g.add(Gain(0.5));
        let master = g.add(Sum);
        g.connect(a, track_a).unwrap();
        g.connect(b, track_b).unwrap();
        g.connect(track_a, master).unwrap();
        g.connect(track_b, master).unwrap();

        let mut r = ReferenceRenderer::new(g, master, 64);
        assert!(r.render_block().iter().all(|&s| s == 0.75));
    }

    #[test]
    fn feedback_loops_are_rejected() {
        let mut g = Graph::new();
        let a = g.add(Sum);
        let b = g.add(Sum);
        g.connect(a, b).unwrap();
        assert_eq!(g.connect(b, a), Err(GraphError::Cycle));
        assert_eq!(g.connect(a, a), Err(GraphError::Cycle));
    }
}

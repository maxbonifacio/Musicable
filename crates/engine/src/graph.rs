use crate::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    UnknownNode(NodeId),
    /// Connecting these nodes would create a feedback loop.
    Cycle,
}

/// Processing graph. Built and validated off the audio thread; renderers take
/// it as-is.
#[derive(Default)]
pub struct Graph {
    pub(crate) nodes: Vec<Box<dyn Node>>,
    /// `inputs[n]` lists the nodes feeding node `n`.
    pub(crate) inputs: Vec<Vec<NodeId>>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, node: impl Node + 'static) -> NodeId {
        self.nodes.push(Box::new(node));
        self.inputs.push(Vec::new());
        NodeId(self.nodes.len() - 1)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        for id in [from, to] {
            if id.0 >= self.nodes.len() {
                return Err(GraphError::UnknownNode(id));
            }
        }
        if from == to || self.reaches(to, from) {
            return Err(GraphError::Cycle);
        }
        self.inputs[to.0].push(from);
        Ok(())
    }

    /// Whether signal can flow from `start` to `target`.
    fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        // Walk downstream from `start` by scanning input lists.
        let mut stack = vec![start];
        let mut seen = vec![false; self.nodes.len()];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if std::mem::replace(&mut seen[n.0], true) {
                continue;
            }
            for (i, ins) in self.inputs.iter().enumerate() {
                if ins.contains(&n) {
                    stack.push(NodeId(i));
                }
            }
        }
        false
    }

    /// Nodes in an order where every node comes after all of its inputs.
    pub fn topological_order(&self) -> Vec<NodeId> {
        let n = self.nodes.len();
        let mut remaining: Vec<usize> = self.inputs.iter().map(Vec::len).collect();
        let mut ready: Vec<NodeId> = (0..n).filter(|&i| remaining[i] == 0).map(NodeId).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(node) = ready.pop() {
            order.push(node);
            for (i, ins) in self.inputs.iter().enumerate() {
                for _ in ins.iter().filter(|&&x| x == node) {
                    remaining[i] -= 1;
                    if remaining[i] == 0 {
                        ready.push(NodeId(i));
                    }
                }
            }
        }
        debug_assert_eq!(order.len(), n, "connect() rejects cycles");
        order
    }
}

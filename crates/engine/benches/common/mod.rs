//! Synthetic graphs for the scheduler benchmarks.

// Each benchmark uses some of these.
#![allow(dead_code)]

use musicable_engine::{
    AudioMut, EngineConfig, Graph, Inputs, Node, NodeId, NodeKey, ParallelOptions,
    ParallelRenderer, PreparedGraph, ProcessContext,
};

pub const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// For graphs rendered ahead of the device, in blocks up to 1024 frames.
pub const AHEAD: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 1024,
};

/// For graphs rendered as an export renders them, in blocks of 4096.
pub const EXPORT: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 4096,
};

/// What nodes cost, in rounds of [`Burn`] work per sample. One round is a
/// multiply-add that has to wait for the one before, a few nanoseconds per
/// sample, so a stereo node with 10 rounds takes about 2 µs per 64 frames.
pub mod cost {
    pub const INSTRUMENT: u32 = 24;
    pub const DEVICE: u32 = 6;
    /// A native stage in a channel strip slot, such as a clip type.
    pub const SLOT: u32 = 2;
    pub const FADER: u32 = 1;
    pub const BUS: u32 = 2;
    /// A return track's effect, such as a reverb.
    pub const RETURN: u32 = 24;
}

/// Stands in for a plugin or a native stage: sums its inputs, then does
/// `work` rounds of arithmetic on every sample, each round depending on the
/// one before, so the work can't be skipped or vectorised away. Each
/// channel carries on from its own last sample, so it plays the same
/// whatever blocks it renders.
pub struct Burn {
    work: u32,
    state: [f32; 2],
}

impl Burn {
    pub fn new(work: u32) -> Self {
        Self {
            work,
            state: [0.0; 2],
        }
    }
}

impl Node for Burn {
    fn process(&mut self, _: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        for (c, channel) in output.channels_mut().enumerate() {
            let state = &mut self.state[c % 2];
            for sample in channel {
                let mut x = *sample + *state;
                for _ in 0..self.work {
                    x = x.mul_add(0.999, 0.001);
                }
                *state = x * 1e-3;
                *sample = x;
            }
        }
    }
}

/// Builds a graph of [`Burn`] nodes, prepared for `config`.
pub struct Builder {
    graph: Graph,
    config: EngineConfig,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new(CONFIG)
    }
}

impl Builder {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            graph: Graph::default(),
            config,
        }
    }

    pub fn node(&mut self, work: u32, inputs: &[NodeId]) -> NodeId {
        let key = NodeKey(self.graph.len() as u64);
        let node = self.graph.add(key, Burn::new(work));
        for &input in inputs {
            self.graph.connect(input, node).unwrap();
        }
        node
    }

    /// A chain of nodes costing `work` each, fed by `input`. Returns its
    /// last node.
    pub fn chain(&mut self, input: Option<NodeId>, work: &[u32]) -> NodeId {
        let mut last = input;
        for &w in work {
            let inputs: &[NodeId] = match &last {
                Some(node) => std::slice::from_ref(node),
                None => &[],
            };
            last = Some(self.node(w, inputs));
        }
        last.expect("a chain has nodes")
    }

    pub fn finish(mut self, output: NodeId) -> PreparedGraph {
        self.graph.set_output(output).unwrap();
        self.graph.prepare(&self.config).unwrap()
    }
}

/// `width` nodes costing `work` each, all into one output.
pub fn wide(width: usize, work: u32) -> PreparedGraph {
    let mut b = Builder::default();
    let sources: Vec<_> = (0..width).map(|_| b.node(work, &[])).collect();
    let out = b.node(cost::BUS, &sources);
    b.finish(out)
}

/// A chain of `depth` nodes costing `work` each.
pub fn deep(depth: usize, work: u32) -> PreparedGraph {
    deep_for(CONFIG, depth, work)
}

/// The same, prepared for `config`.
pub fn deep_for(config: EngineConfig, depth: usize, work: u32) -> PreparedGraph {
    let mut b = Builder::new(config);
    let out = b.chain(None, &vec![work; depth]);
    b.finish(out)
}

/// How a synthetic project's tracks are routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// Every track straight into the master.
    Direct,
    /// Tracks in groups of eight, each group into the master.
    Groups,
    /// Tracks in groups of eight, and every track also sends to two return
    /// tracks.
    Sends,
}

impl Routing {
    pub const ALL: [Routing; 3] = [Routing::Direct, Routing::Groups, Routing::Sends];

    pub fn name(self) -> &'static str {
        match self {
            Routing::Direct => "direct",
            Routing::Groups => "groups",
            Routing::Sends => "sends",
        }
    }
}

/// A synthetic project: `tracks` tracks, each an instrument and two devices
/// followed by a channel strip, routed by `routing`. With `slots`, every
/// strip has its pre-fader, post-fader and bus feed slots filled, like a
/// project with clip types or console mode on everywhere.
#[derive(Debug, Clone, Copy)]
pub struct Project {
    pub tracks: usize,
    pub routing: Routing,
    pub slots: bool,
}

impl Project {
    /// Each track's nodes, in signal order: their costs.
    fn track(self) -> Vec<u32> {
        use cost::*;
        match self.slots {
            true => vec![INSTRUMENT, DEVICE, DEVICE, SLOT, FADER, SLOT, SLOT],
            false => vec![INSTRUMENT, DEVICE, DEVICE, FADER],
        }
    }

    /// The project's graph, one node per instrument, device and stage: how
    /// the engine schedules it.
    pub fn per_node(self) -> PreparedGraph {
        self.per_node_for(CONFIG)
    }

    /// The same, prepared for `config`.
    pub fn per_node_for(self, config: EngineConfig) -> PreparedGraph {
        let mut b = Builder::new(config);
        let track = self.track();
        let tracks: Vec<_> = (0..self.tracks).map(|_| b.chain(None, &track)).collect();
        let mut into_master = Vec::new();
        match self.routing {
            Routing::Direct => into_master.extend(&tracks),
            Routing::Groups | Routing::Sends => {
                for group in tracks.chunks(8) {
                    into_master.push(b.node(cost::BUS, group));
                }
            }
        }
        if self.routing == Routing::Sends {
            for _ in 0..2 {
                let send = b.node(cost::BUS, &tracks);
                into_master.push(b.chain(Some(send), &[cost::RETURN, cost::FADER]));
            }
        }
        let master = b.node(cost::BUS, &into_master);
        b.finish(master)
    }

    /// The same work, scheduled the way Ableton's documentation describes
    /// Live doing it: one thread per signal path, where tracks connected by
    /// routing or sends are one path, processed in series. Each path is one
    /// node here, costing as much as all of its nodes together.
    pub fn per_path(self) -> PreparedGraph {
        let track: u32 = self.track().iter().sum();
        let returns = 2 * (cost::BUS + cost::RETURN + cost::FADER);
        let paths: Vec<u32> = match self.routing {
            Routing::Direct => vec![track; self.tracks],
            Routing::Groups => self
                .groups()
                .map(|tracks| tracks as u32 * track + cost::BUS)
                .collect(),
            // Every track sends to the returns, so they're all one path.
            Routing::Sends => {
                let groups = self.groups().count() as u32 * cost::BUS;
                vec![self.tracks as u32 * track + groups + returns]
            }
        };
        let mut b = Builder::default();
        let paths: Vec<_> = paths.iter().map(|&work| b.node(work, &[])).collect();
        let master = b.node(cost::BUS, &paths);
        b.finish(master)
    }

    /// How many tracks are in each group.
    fn groups(self) -> impl Iterator<Item = usize> {
        (0..self.tracks)
            .step_by(8)
            .map(move |t| (self.tracks - t).min(8))
    }
}

/// A renderer with `lanes` threads in all, counting the audio thread, that
/// shares every block.
pub fn renderer(lanes: usize) -> ParallelRenderer {
    ParallelRenderer::new(ParallelOptions {
        threads: lanes - 1,
        inline_below: None,
        ..ParallelOptions::default()
    })
}

/// One thread per physical core, the default, or as many as the
/// `MUSICABLE_BENCH_THREADS` environment variable says. CI sets it to its
/// runners' logical processors: they have few physical cores, and a private
/// repository's runner has one, which would leave the parallel renderer no
/// workers.
pub fn cores() -> usize {
    std::env::var("MUSICABLE_BENCH_THREADS")
        .ok()
        .and_then(|threads| threads.parse().ok())
        .filter(|&threads: &usize| threads > 0)
        .unwrap_or(ParallelRenderer::default_threads() + 1)
}

/// Thread counts to measure: 1, 2, 4, ... up to one per physical core.
pub fn lane_counts() -> Vec<usize> {
    let mut lanes: Vec<_> = (0..).map(|i| 1 << i).take_while(|&n| n < cores()).collect();
    lanes.push(cores());
    lanes
}

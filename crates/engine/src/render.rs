//! Renderers run every node of a graph once per block.

use std::ops::Range;

use musicable_model::time::TempoMap;

use crate::{
    AheadWaker, AheadWork, AudioRef, Inputs, MidiMessage, NodeEvent, PreparedGraph, ProcessContext,
    TransportState, load, rt,
};

/// One block, as a renderer sees it.
#[derive(Debug, Clone, Copy)]
pub struct Block<'a> {
    pub frames: usize,
    /// Engine time of the first frame.
    pub time: u64,
    pub transport: TransportState,
    pub tempo: &'a TempoMap,
    /// The audio devices' inputs for the block: [`ProcessContext::input`].
    pub input: AudioRef<'a>,
    /// The MIDI inputs' messages for the block: [`ProcessContext::midi`].
    pub midi: &'a [MidiMessage],
    /// Whether the block renders ahead of the device:
    /// [`ProcessContext::ahead`].
    pub ahead: bool,
}

impl<'a> Block<'a> {
    /// Frames `frames` of the block, as a block of their own: from its
    /// frame `frames.start` on, with its inputs there and the MIDI messages
    /// for those frames. The first part has those that came late too, and
    /// the last any for after the block, as the block has.
    pub(crate) fn part(&self, frames: Range<usize>) -> Block<'a> {
        let (start, end) = (
            self.time + frames.start as u64,
            self.time + frames.end as u64,
        );
        let from = match frames.start {
            0 => 0,
            _ => self.midi.partition_point(|m| m.time < start),
        };
        let to = match frames.end == self.frames {
            true => self.midi.len(),
            false => self.midi.partition_point(|m| m.time < end),
        };
        Block {
            frames: frames.len(),
            time: start,
            transport: self.transport.after(frames.start as u64),
            tempo: self.tempo,
            input: self.input.range(frames),
            midi: &self.midi[from..to],
            ahead: self.ahead,
        }
    }

    /// What a node with `events` knows about this block. Every renderer
    /// builds node contexts here, so they all give nodes the same ones.
    pub(crate) fn context(&self, sample_rate: u32, events: &'a [NodeEvent]) -> ProcessContext<'a> {
        ProcessContext {
            frames: self.frames,
            sample_rate,
            time: self.time,
            transport: self.transport,
            tempo: self.tempo,
            events,
            input: self.input,
            midi: self.midi,
            ahead: self.ahead,
        }
    }
}

/// Runs a graph for one block, on the audio thread.
///
/// The engine owns one renderer. [`ReferenceRenderer`] is the default and the
/// oracle; [`ParallelRenderer`](crate::ParallelRenderer) runs nodes on several
/// threads and matches it sample for sample.
pub trait Renderer: Send {
    /// Processes every node of `graph`'s current stage once, each after all
    /// of its inputs: the whole graph, for one without notes made while
    /// rendering ([`Graph::send_made_notes`](crate::Graph::send_made_notes)).
    /// The engine has already put each node's events for the block into
    /// `graph`, and rendered the stages before.
    ///
    /// Nodes run with denormals flushed to zero
    /// ([`rt::flush_denormals`]), whichever thread runs them, and each one's
    /// time goes into its load ([`PreparedGraph::loads`]).
    fn render(&mut self, graph: &mut PreparedGraph, block: &Block<'_>);

    /// Has the renderer's own threads render the engine ahead of the audio
    /// device with `work`, whenever the engine asks through the waker this
    /// returns ([`EngineHandle::set_lead`](crate::EngineHandle::set_lead)).
    /// The engine calls it once, as it takes the renderer. `None`, the
    /// default, for a renderer without threads of its own, such as the
    /// reference renderer: the audio thread renders ahead itself then, at
    /// the end of each device callback.
    fn render_ahead(&mut self, work: AheadWork) -> Option<AheadWaker> {
        let _ = work;
        None
    }

    /// What renders the live half of a graph that has one, on the audio
    /// thread, while this renders the rest, ahead of the device when it can
    /// ([`Graph::prepare`](crate::Graph::prepare)). The engine calls it
    /// once, as it takes the renderer. A [`ReferenceRenderer`], the
    /// default.
    fn live_renderer(&mut self) -> Box<dyn Renderer> {
        Box::new(ReferenceRenderer)
    }
}

/// Processes one node at a time in topological order, on the calling thread.
///
/// Deliberately simple: it is the oracle the parallel scheduler is tested
/// against, not the production path.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceRenderer;

impl Renderer for ReferenceRenderer {
    fn render(&mut self, graph: &mut PreparedGraph, block: &Block<'_>) {
        rt::flush_denormals(|| render_in_order(graph, block));
    }
}

fn render_in_order(graph: &mut PreparedGraph, block: &Block<'_>) {
    let stage = graph.stage_nodes();
    let PreparedGraph {
        config,
        nodes,
        inputs,
        ports,
        order,
        buffers,
        events,
        event_ranges,
        loads,
        load_base,
        ..
    } = graph;
    // Each node's time runs from the end of the one before, so the clock is
    // read once a node.
    let mut last = load::now();
    for &node in &order[stage] {
        // Take the node's buffer out so its inputs can be read alongside it.
        // No node feeds itself, and taking leaves an empty buffer, so nothing
        // is allocated or freed.
        let mut output = std::mem::take(&mut buffers[node.0]);
        let (start, end) = event_ranges[node.0];
        let ctx = block.context(config.sample_rate, &events[start as usize..end as usize]);
        let inputs = Inputs::graph(buffers, &inputs[node.0], &ports[node.0], block.frames);
        nodes[node.0].process(&ctx, inputs, &mut output.view_mut(block.frames));
        buffers[node.0] = output;
        let now = load::now();
        // Its tasks ran here, if it had any.
        let load = &loads.loads[*load_base + node.0];
        load.add(now.wrapping_sub(last), 0, block.frames, block.ahead);
        last = now;
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
            tempo: &TempoMap::default(),
            input: AudioRef::empty(frames),
            midi: &[],
            ahead: false,
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
    fn a_part_of_a_block_is_a_block_of_its_own() {
        let tempo = TempoMap::default();
        let input = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let message = |time| MidiMessage {
            time,
            input: 0,
            bytes: [0x90, 60, 100],
        };
        // One late, and one for after the block.
        let midi = [9, 10, 12, 13, 20].map(message);
        let block = Block {
            frames: 4,
            time: 10,
            transport: TransportState {
                playing: true,
                position: 100,
                ..TransportState::default()
            },
            tempo: &tempo,
            input: AudioRef::from_planar(&input, 2),
            midi: &midi,
            ahead: true,
        };
        let times = |part: &Block<'_>| part.midi.iter().map(|m| m.time).collect::<Vec<_>>();
        let first = block.part(0..2);
        assert_eq!(
            (first.frames, first.time, first.transport.position),
            (2, 10, 100)
        );
        assert_eq!(first.input.channel(1), [5.0, 6.0]);
        assert_eq!(times(&first), [9, 10]);
        let middle = block.part(1..3);
        assert_eq!((middle.time, middle.transport.position), (11, 101));
        assert_eq!(times(&middle), [12]);
        let last = block.part(2..4);
        assert_eq!(
            (last.frames, last.time, last.transport.position),
            (2, 12, 102)
        );
        assert_eq!(last.input.channel(0), [3.0, 4.0]);
        assert_eq!(times(&last), [12, 13, 20]);
        assert!(last.ahead);
        // Stopped, the song doesn't move on.
        let stopped = Block {
            transport: TransportState {
                position: 100,
                ..TransportState::default()
            },
            ..block
        };
        assert_eq!(stopped.part(2..4).transport.position, 100);
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

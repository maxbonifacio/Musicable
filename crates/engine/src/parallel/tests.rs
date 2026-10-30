//! The parallel renderer against the reference renderer on random graphs, and
//! what it does when nodes break the rules.

use std::cell::RefCell;
use std::sync::atomic::AtomicUsize;

use musicable_model::time::{Beats, TempoMap};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::{Config, TestRunner};

use super::*;
use crate::graph::Routed;
use crate::load::tests::{play, play_ahead, reads, spin, timed};
use crate::nodes::{Constant, Delay, Gain, Latent, Sine, Spin, Sum};
use crate::{
    AudioBuffer, AudioMut, EngineConfig, Graph, NodeEventKind, NodeKey, ParamId, ProcessContext,
    TransportState,
};

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 32,
};

/// A node whose output depends on everything it is given: its inputs and
/// their order, its events, the block and its own history. If a node saw a
/// different block than under the reference renderer, or ran twice or not at
/// all, the difference would carry on through its output.
///
/// It only uses arithmetic that IEEE 754 rounds exactly, unlike `sin` or a
/// fused multiply-add: Miri varies the results of the others from call to
/// call, as a platform may.
struct Mix {
    channels: usize,
    seed: f32,
    state: f32,
}

impl Mix {
    fn new(seed: u8, channels: usize) -> Self {
        Self {
            channels,
            seed: f32::from(seed) / 64.0,
            state: 0.0,
        }
    }
}

impl Node for Mix {
    fn channels(&self) -> usize {
        self.channels
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let block = self.seed
            + (ctx.time % 1000) as f32 * 1e-3
            + (ctx.transport.position % 1000) as f32 * 1e-4
            + f32::from(u8::from(ctx.transport.playing))
            + ctx.tempo.bpm_at(Beats::ZERO) as f32 * 1e-3
            + ctx.sample_rate as f32 * 1e-6;
        let mut events = ctx.events.iter().peekable();
        for frame in 0..ctx.frames {
            while let Some(event) = events.next_if(|e| e.offset == frame) {
                if let NodeEventKind::Param { param, value } = event.kind {
                    self.state += (param.0 as f32 + 1.0) * value as f32;
                }
            }
            for c in 0..self.channels {
                let mut acc = self.state + block;
                for (i, input) in inputs.iter().enumerate() {
                    let x = input.channel(c % input.channels())[frame];
                    acc = acc * 0.5 + x * (i as f32 + 1.5);
                }
                let y = acc / (1.0 + acc.abs());
                output.channel_mut(c)[frame] = y;
                self.state = 0.9 * self.state + 0.1 * y;
            }
        }
    }
}

/// What a random graph's node is.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Mix {
        seed: u8,
        channels: usize,
    },
    Sum {
        channels: usize,
    },
    Gain {
        gain: f32,
        channels: usize,
    },
    Constant {
        value: f32,
    },
    Sine {
        frequency: f64,
    },
    /// So delay compensation adds delays wherever its path meets another.
    Latent {
        frames: usize,
        channels: usize,
    },
}

impl Kind {
    fn node(self) -> Box<dyn Node> {
        match self {
            Kind::Mix { seed, channels } => Box::new(Mix::new(seed, channels)),
            Kind::Sum { channels } => Box::new(Sum::new(channels)),
            Kind::Gain { gain, channels } => Box::new(Gain::new(gain).with_channels(channels)),
            Kind::Constant { value } => Box::new(Constant::new(value)),
            Kind::Sine { frequency } => Box::new(Sine::new(frequency, 0.5)),
            Kind::Latent { frames, channels } => {
                Box::new(Latent(Delay::new(frames).with_channels(channels)))
            }
        }
    }
}

/// A random graph, as data, so it can be built more than once and shrunk.
#[derive(Debug, Clone)]
struct Spec {
    /// Each node's kind and inputs. Inputs come earlier in the list.
    nodes: Vec<(Kind, Vec<usize>)>,
    /// The order the nodes are added to the graph, so that their ids aren't
    /// in the order they run.
    added: Vec<usize>,
    output: usize,
}

/// A block to render.
#[derive(Debug, Clone)]
struct BlockSpec {
    frames: usize,
    playing: bool,
    /// Node events, in order: node, offset, parameter and value.
    events: Vec<(usize, usize, u64, f64)>,
}

fn kind() -> impl Strategy<Value = Kind> {
    let channels = 1..=2usize;
    prop_oneof![
        4 => (any::<u8>(), channels.clone()).prop_map(|(seed, channels)| mix(seed, channels)),
        1 => channels.clone().prop_map(|channels| Kind::Sum { channels }),
        1 => (0.0f32..2.0, channels.clone()).prop_map(|(gain, channels)| Kind::Gain { gain, channels }),
        1 => (-1.0f32..1.0).prop_map(|value| Kind::Constant { value }),
        1 => (20.0f64..2000.0).prop_map(|frequency| Kind::Sine { frequency }),
        1 => (0..80usize, channels).prop_map(|(frames, channels)| Kind::Latent { frames, channels }),
    ]
}

/// Graphs of up to `max_nodes` nodes, each fed by up to four earlier ones,
/// some more than once.
fn spec(max_nodes: usize) -> impl Strategy<Value = Spec> {
    (1..=max_nodes)
        .prop_flat_map(|n| {
            let inputs = prop::collection::vec(any::<Index>(), 0..=4);
            let nodes = prop::collection::vec((kind(), inputs), n);
            let added = Just((0..n).collect::<Vec<_>>()).prop_shuffle();
            (nodes, added, any::<Index>())
        })
        .prop_map(|(nodes, added, output)| {
            let n = nodes.len();
            let nodes = nodes
                .into_iter()
                .enumerate()
                .map(|(i, (kind, inputs))| {
                    let inputs = match i {
                        0 => Vec::new(),
                        _ => inputs.iter().map(|input| input.index(i)).collect(),
                    };
                    (kind, inputs)
                })
                .collect();
            Spec {
                nodes,
                added,
                output: output.index(n),
            }
        })
}

/// A few blocks of any length, with events for any of `nodes` nodes, some
/// for parameters they don't have.
fn blocks(nodes: usize) -> impl Strategy<Value = Vec<BlockSpec>> {
    let event = (0..nodes, 0.0f64..1.0, 0..3u64, -1.0f64..1.0);
    let block = (
        1..=CONFIG.max_block,
        any::<bool>(),
        prop::collection::vec(event, 0..=8),
    )
        .prop_map(|(frames, playing, events)| {
            let mut events: Vec<_> = events
                .into_iter()
                .map(|(node, at, param, value)| (node, (at * frames as f64) as usize, param, value))
                .collect();
            events.sort_by_key(|&(_, offset, _, _)| offset);
            BlockSpec {
                frames,
                playing,
                events,
            }
        });
    prop::collection::vec(block, 1..=6)
}

/// Builds and prepares `spec`'s graph. Also returns the id of each node.
fn build(spec: &Spec) -> (PreparedGraph, Vec<NodeId>) {
    let mut g = Graph::new();
    let mut ids = vec![NodeId(0); spec.nodes.len()];
    for &i in &spec.added {
        ids[i] = g.add_boxed(NodeKey(i as u64), spec.nodes[i].0.node());
    }
    for (i, (_, inputs)) in spec.nodes.iter().enumerate() {
        for &input in inputs {
            g.connect(ids[input], ids[i]).unwrap();
        }
    }
    g.set_output(ids[spec.output]).unwrap();
    (g.prepare(&CONFIG).unwrap(), ids)
}

/// Renders one block the way the engine does: with the block's events loaded
/// and inside `rt::scope`.
fn render(
    renderer: &mut dyn Renderer,
    graph: &mut PreparedGraph,
    ids: &[NodeId],
    block: &BlockSpec,
    time: u64,
    tempo: &TempoMap,
) {
    render_at(renderer, graph, ids, block, time, time / 2, tempo);
}

/// The same, with the song at `position`.
fn render_at(
    renderer: &mut dyn Renderer,
    graph: &mut PreparedGraph,
    ids: &[NodeId],
    block: &BlockSpec,
    time: u64,
    position: u64,
    tempo: &TempoMap,
) {
    let mut routed: Vec<Routed> = block
        .events
        .iter()
        .enumerate()
        .map(|(order, &(node, offset, param, value))| Routed {
            node: ids[node],
            rank: 0,
            order: order as u32,
            event: NodeEvent {
                offset,
                kind: NodeEventKind::Param {
                    param: ParamId(param),
                    value,
                },
            },
        })
        .collect();
    graph.load_events(&mut routed);
    let transport = TransportState {
        playing: block.playing,
        position,
        ..TransportState::default()
    };
    let block = Block {
        frames: block.frames,
        time,
        transport,
        tempo,
        input: crate::AudioRef::empty(block.frames),
        midi: &[],
        ahead: false,
    };
    rt::scope(|| renderer.render(graph, &block));
    graph.clear_events(&routed);
}

/// Whether two buffers hold the same samples, bit for bit.
fn same(a: &AudioBuffer, b: &AudioBuffer, frames: usize) -> bool {
    let (a, b) = (a.view(frames), b.view(frames));
    let bits = |channel: &[f32]| channel.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
    a.channels() == b.channels() && a.iter().zip(b.iter()).all(|(a, b)| bits(a) == bits(b))
}

/// Renders `blocks` of `spec`'s graph with each of `renderers` and with the
/// reference renderer, and checks that after each block every node's output
/// is the same as the reference's, bit for bit.
fn check(
    spec: &Spec,
    blocks: &[BlockSpec],
    renderers: &mut [ParallelRenderer],
) -> Result<(), TestCaseError> {
    let tempo = TempoMap::default();
    let (mut expected, ids) = build(spec);
    let mut graphs: Vec<_> = renderers.iter().map(|_| build(spec).0).collect();
    let mut time = 0;
    for (b, block) in blocks.iter().enumerate() {
        render(
            &mut ReferenceRenderer,
            &mut expected,
            &ids,
            block,
            time,
            &tempo,
        );
        for (renderer, graph) in renderers.iter_mut().zip(&mut graphs) {
            render(renderer, graph, &ids, block, time, &tempo);
            for (node, id) in ids.iter().enumerate() {
                prop_assert!(
                    same(&graph.buffers[id.0], &expected.buffers[id.0], block.frames),
                    "node {node} differs in block {b} with {} workers",
                    renderer.threads(),
                );
            }
            // And the delays that delay compensation added after them.
            for delay in ids.len()..expected.buffers.len() {
                prop_assert!(
                    same(
                        &graph.buffers[delay],
                        &expected.buffers[delay],
                        block.frames
                    ),
                    "delay {delay} differs in block {b} with {} workers",
                    renderer.threads(),
                );
            }
        }
        time += block.frames as u64;
    }
    Ok(())
}

/// Renders `block` of a graph with the reference renderer as the parallel
/// renderer does pipelining it in `parts` parts: each part as a block of
/// its own, from its frame on, with the events in its frames, the last
/// with those after them too. Each node's output for a part goes where the
/// part is in its buffer in `expected`.
fn render_parts(
    graph: &mut PreparedGraph,
    expected: &mut [AudioBuffer],
    ids: &[NodeId],
    block: &BlockSpec,
    time: u64,
    tempo: &TempoMap,
    parts: usize,
) {
    let parts = parts.min(block.frames);
    for part in 0..parts {
        let frames = part_frames(block.frames, parts, part);
        let last = part + 1 == parts;
        let events = (block.events.iter())
            .filter(|&&(_, offset, ..)| offset >= frames.start && (offset < frames.end || last))
            .map(|&(node, offset, param, value)| (node, offset - frames.start, param, value))
            .collect();
        let spec = BlockSpec {
            frames: frames.len(),
            playing: block.playing,
            events,
        };
        let moved = if block.playing {
            frames.start as u64
        } else {
            0
        };
        let start = time + frames.start as u64;
        let mut reference = ReferenceRenderer;
        render_at(
            &mut reference,
            graph,
            ids,
            &spec,
            start,
            time / 2 + moved,
            tempo,
        );
        for (expected, buffer) in expected.iter_mut().zip(&graph.buffers) {
            let mut whole = expected.view_mut(block.frames);
            whole
                .range_mut(frames.clone())
                .copy_from(buffer.view(frames.len()));
        }
    }
}

/// Renders `blocks` of `spec`'s graph with each of `renderers`, which
/// pipeline every block in `parts` parts, and checks that after each block
/// every node's output is what the reference renderer renders part by part,
/// bit for bit.
fn check_parts(
    spec: &Spec,
    blocks: &[BlockSpec],
    parts: usize,
    renderers: &mut [ParallelRenderer],
) -> Result<(), TestCaseError> {
    let tempo = TempoMap::default();
    let (mut reference, ids) = build(spec);
    let mut expected = reference.buffers.clone();
    let mut graphs: Vec<_> = renderers.iter().map(|_| build(spec).0).collect();
    let mut time = 0;
    for (b, block) in blocks.iter().enumerate() {
        render_parts(
            &mut reference,
            &mut expected,
            &ids,
            block,
            time,
            &tempo,
            parts,
        );
        for (renderer, graph) in renderers.iter_mut().zip(&mut graphs) {
            renderer.force_parts(parts);
            render(renderer, graph, &ids, block, time, &tempo);
            // Every node's, and the delays' that delay compensation added.
            for (n, (buffer, expected)) in graph.buffers.iter().zip(&expected).enumerate() {
                prop_assert!(
                    same(buffer, expected, block.frames),
                    "node {n} differs in block {b} in {parts} parts with {} workers",
                    renderer.threads(),
                );
            }
        }
        time += block.frames as u64;
    }
    Ok(())
}

/// Renderers that pipeline every block, as for an export, and share any
/// graph a test makes between threads.
fn pipelining(threads: usize, spin: Duration) -> ParallelRenderer {
    ParallelRenderer::new(ParallelOptions {
        pipeline: Pipeline::Always,
        max_nodes: 1 << 10,
        ..options(threads, spin)
    })
}

fn options(threads: usize, spin: Duration) -> ParallelOptions {
    ParallelOptions {
        threads,
        spin,
        inline_below: None,
        max_nodes: 64,
        realtime: false,
        ..ParallelOptions::default()
    }
}

fn block(frames: usize, tempo: &TempoMap) -> Block<'_> {
    Block {
        frames,
        time: 0,
        transport: TransportState::default(),
        tempo,
        input: crate::AudioRef::empty(frames),
        midi: &[],
        ahead: false,
    }
}

/// `width` nodes made by `node`, all feeding a sum, which is the output.
fn fan_in(width: usize, mut node: impl FnMut(usize) -> Box<dyn Node>) -> PreparedGraph {
    let mut g = Graph::new();
    let sum = g.add(NodeKey(0), Sum::default());
    for i in 0..width {
        let source = g.add_boxed(NodeKey(i as u64 + 1), node(i));
        g.connect(source, sum).unwrap();
    }
    g.set_output(sum).unwrap();
    g.prepare(&CONFIG).unwrap()
}

#[test]
#[cfg_attr(miri, ignore)]
fn random_graphs_render_like_the_reference() {
    // Thousands of graphs, each rendered for a few blocks by renderers with
    // too few and too many workers, whose workers sleep or spin between
    // blocks. Set PROPTEST_CASES to try more.
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(2000);
    let mut runner = TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    });
    let renderers = RefCell::new(
        [
            (1, Duration::ZERO),
            (2, Duration::from_micros(50)),
            (3, Duration::ZERO),
            (7, Duration::from_millis(1)),
        ]
        .map(|(threads, spin)| ParallelRenderer::new(options(threads, spin))),
    );
    let strategy = spec(48).prop_flat_map(|spec| {
        let nodes = spec.nodes.len();
        (Just(spec), blocks(nodes))
    });
    let result = runner.run(&strategy, |(spec, blocks)| {
        check(&spec, &blocks, &mut *renderers.borrow_mut())
    });
    if let Err(error) = result {
        panic!("{error}");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn random_graphs_render_in_parts_as_the_reference_renders_the_parts() {
    // As above, each block pipelined in any number of parts, some of a
    // frame each. Set PROPTEST_CASES to try more.
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(1000);
    let mut runner = TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    });
    let renderers = RefCell::new(
        [
            (1, Duration::ZERO),
            (2, Duration::from_micros(50)),
            (3, Duration::ZERO),
            (7, Duration::from_millis(1)),
        ]
        .map(|(threads, spin)| pipelining(threads, spin)),
    );
    let strategy = spec(48).prop_flat_map(|spec| {
        let nodes = spec.nodes.len();
        (Just(spec), blocks(nodes), 2..=PARTS)
    });
    let result = runner.run(&strategy, |(spec, blocks, parts)| {
        check_parts(&spec, &blocks, parts, &mut *renderers.borrow_mut())
    });
    if let Err(error) = result {
        panic!("{error}");
    }
}

fn mix(seed: u8, channels: usize) -> Kind {
    Kind::Mix { seed, channels }
}

/// A graph small enough for Miri, which checks the renderer for data races
/// and undefined behaviour, and a few blocks to render.
fn small() -> (Spec, Vec<BlockSpec>) {
    let gain = Kind::Gain {
        gain: 0.5,
        channels: 2,
    };
    let spec = Spec {
        nodes: vec![
            (mix(1, 2), vec![]),
            (Kind::Constant { value: 0.5 }, vec![]),
            (mix(2, 1), vec![0, 1]),
            (gain, vec![0, 0]),
            (mix(3, 2), vec![2, 3, 1]),
        ],
        added: vec![3, 0, 4, 1, 2],
        output: 4,
    };
    let blocks = [(4, false), (1, true), (3, true), (2, true)]
        .into_iter()
        .map(|(frames, playing)| BlockSpec {
            frames,
            playing,
            events: vec![(2, 0, 0, 0.25), (3, frames - 1, Gain::GAIN.0, 2.0)],
        })
        .collect();
    (spec, blocks)
}

#[test]
fn a_small_graph_renders_like_the_reference() {
    let (spec, blocks) = small();
    let mut renderers = [
        ParallelRenderer::new(options(1, Duration::ZERO)),
        ParallelRenderer::new(options(2, Duration::from_micros(100))),
    ];
    check(&spec, &blocks, &mut renderers).unwrap();
}

#[test]
fn a_small_graph_renders_in_parts_as_the_reference_renders_the_parts() {
    let (spec, blocks) = small();
    let mut renderers = [
        pipelining(1, Duration::ZERO),
        pipelining(2, Duration::from_micros(100)),
    ];
    for parts in [2, 3] {
        check_parts(&spec, &blocks, parts, &mut renderers).unwrap();
    }
}

#[test]
fn renderers_can_take_turns_with_a_graph() {
    // The parallel renderer renders some blocks on the audio thread alone,
    // with the reference renderer, and keeps pointers to the graph's buffers
    // in between.
    let (spec, blocks) = small();
    let tempo = TempoMap::default();
    let (mut expected, ids) = build(&spec);
    let (mut graph, _) = build(&spec);
    let mut parallel = ParallelRenderer::new(options(2, Duration::ZERO));
    let mut time = 0;
    for (b, block) in blocks.iter().enumerate() {
        render(
            &mut ReferenceRenderer,
            &mut expected,
            &ids,
            block,
            time,
            &tempo,
        );
        match b % 2 {
            0 => render(&mut parallel, &mut graph, &ids, block, time, &tempo),
            _ => render(
                &mut ReferenceRenderer,
                &mut graph,
                &ids,
                block,
                time,
                &tempo,
            ),
        }
        for id in &ids {
            assert!(same(
                &graph.buffers[id.0],
                &expected.buffers[id.0],
                block.frames
            ));
        }
        time += block.frames as u64;
    }
}

#[test]
fn a_node_that_makes_many_ready_fits_the_queue() {
    // One source feeding every other node but one, which they all feed:
    // finishing the source queues all of them on one thread.
    let max_nodes = 16;
    let fanned = (1..max_nodes - 1).collect::<Vec<_>>();
    let mut nodes = vec![(mix(0, 2), vec![])];
    nodes.extend(fanned.iter().map(|&i| (mix(i as u8, 1), vec![0])));
    nodes.push((mix(99, 2), fanned));
    let spec = Spec {
        added: (0..nodes.len()).collect(),
        output: nodes.len() - 1,
        nodes,
    };
    let blocks = vec![
        BlockSpec {
            frames: 8,
            playing: true,
            events: vec![],
        };
        3
    ];
    let options = ParallelOptions {
        max_nodes,
        ..options(2, Duration::ZERO)
    };
    let mut renderers = [ParallelRenderer::new(options)];
    check(&spec, &blocks, &mut renderers).unwrap();
    let stats = renderers[0].monitor().stats();
    assert_eq!(stats.parallel_blocks, 3);
}

/// Waits until `count` nodes like it have started in the current block, or
/// until a deadline long past.
struct Rendezvous {
    count: usize,
    arrived: Arc<AtomicUsize>,
    missed: Arc<AtomicUsize>,
    blocks: usize,
}

impl Node for Rendezvous {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        self.blocks += 1;
        let all = self.count * self.blocks;
        self.arrived.fetch_add(1, AcqRel);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.arrived.load(Acquire) < all {
            if Instant::now() > deadline {
                self.missed.fetch_add(1, Relaxed);
                break;
            }
            rt::allowed(thread::yield_now);
        }
        output.clear();
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn nodes_run_on_several_threads_at_once() {
    // Each node waits for all the others to start, so a block only finishes
    // in time if every thread runs one at the same time.
    let threads = 4;
    let arrived = Arc::new(AtomicUsize::new(0));
    let missed = Arc::new(AtomicUsize::new(0));
    let mut graph = fan_in(threads + 1, |_| {
        Box::new(Rendezvous {
            count: threads + 1,
            arrived: arrived.clone(),
            missed: missed.clone(),
            blocks: 0,
        })
    });
    let tempo = TempoMap::default();
    // Workers that sleep between blocks have to be woken for each one.
    for spin in [Duration::ZERO, Duration::from_millis(5)] {
        let mut renderer = ParallelRenderer::new(options(threads, spin));
        for _ in 0..10 {
            renderer.render(&mut graph, &block(16, &tempo));
        }
    }
    assert_eq!(missed.load(Relaxed), 0);
}

/// Waits like a [`Rendezvous`], so each thread runs one, then outputs a
/// number too small for a normal float.
struct Denormal(Rendezvous);

impl Node for Denormal {
    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        self.0.process(ctx, inputs, output);
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        output.fill(tiny * std::hint::black_box(0.25));
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn every_thread_flushes_denormals_to_zero() {
    // Like the audio thread in the reference renderer, so a node gives the
    // same samples on any thread.
    let flushes = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));
    let threads = 3;
    let arrived = Arc::new(AtomicUsize::new(0));
    let missed = Arc::new(AtomicUsize::new(0));
    let mut graph = fan_in(threads + 1, |_| {
        Box::new(Denormal(Rendezvous {
            count: threads + 1,
            arrived: arrived.clone(),
            missed: missed.clone(),
            blocks: 0,
        }))
    });
    let tempo = TempoMap::default();
    let mut renderer = ParallelRenderer::new(options(threads, Duration::ZERO));
    for _ in 0..5 {
        renderer.render(&mut graph, &block(16, &tempo));
        for buffer in &graph.buffers {
            let zero = buffer.view(16).iter().flatten().all(|&s| s == 0.0);
            assert_eq!(zero, flushes);
        }
    }
    assert_eq!(missed.load(Relaxed), 0, "each thread ran one");
}

/// Panics in its first block.
#[derive(Default)]
struct PanicsOnce {
    panicked: bool,
}

impl Node for PanicsOnce {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        if !self.panicked {
            self.panicked = true;
            rt::allowed(|| panic!("a node failed"));
        }
        output.clear();
    }
}

#[test]
fn a_panic_on_any_thread_reaches_the_audio_thread() {
    let tempo = TempoMap::default();
    let mut renderer = ParallelRenderer::new(options(3, Duration::ZERO));
    for broken in [0, 3, 7] {
        let mut graph = fan_in(8, |i| match i == broken {
            true => Box::new(PanicsOnce::default()),
            false => Box::new(Mix::new(i as u8, 2)),
        });
        let failed = panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.render(&mut graph, &block(16, &tempo));
        }));
        let payload = failed.expect_err("the block should have failed");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"a node failed"));
        // The rest of the block ran, and the renderer carries on.
        renderer.render(&mut graph, &block(16, &tempo));
    }
}

/// Allocates in every block, which nodes must not do.
#[cfg(debug_assertions)]
struct Allocates;

#[cfg(debug_assertions)]
impl Node for Allocates {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        drop(std::hint::black_box(vec![0u8; 16]));
        output.clear();
    }
}

#[test]
#[cfg(debug_assertions)]
#[cfg_attr(miri, ignore)]
fn allocating_on_any_thread_fails_the_block() {
    let tempo = TempoMap::default();
    let mut renderer = ParallelRenderer::new(options(3, Duration::ZERO));
    let mut graph = fan_in(8, |i| match i {
        5 => Box::new(Allocates),
        _ => Box::new(Mix::new(i as u8, 2)),
    });
    // The node runs on whichever thread gets to it first.
    for _ in 0..20 {
        let failed = panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.render(&mut graph, &block(16, &tempo));
        }));
        let payload = failed.expect_err("the block should have failed");
        let message = payload.downcast::<String>().unwrap();
        assert!(message.contains("real-time violation"), "{message}");
    }
}

#[test]
fn blocks_without_enough_to_share_run_on_the_audio_thread() {
    let tempo = TempoMap::default();
    let wide = || fan_in(4, |i| Box::new(Mix::new(i as u8, 2)));
    let blocks = |renderer: &mut ParallelRenderer, graph: &mut PreparedGraph| {
        let before = renderer.monitor().stats();
        for _ in 0..5 {
            renderer.render(graph, &block(16, &tempo));
        }
        let after = renderer.monitor().stats();
        (
            after.parallel_blocks - before.parallel_blocks,
            after.inline_blocks - before.inline_blocks,
        )
    };

    // No workers.
    let mut renderer = ParallelRenderer::new(options(0, Duration::ZERO));
    assert_eq!(blocks(&mut renderer, &mut wide()), (0, 5));

    // A graph too big for the queues.
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        max_nodes: 4,
        ..options(2, Duration::ZERO)
    });
    assert_eq!(blocks(&mut renderer, &mut wide()), (0, 5));

    // A chain: nothing can run alongside anything else.
    let mut chain = Graph::new();
    let a = chain.add(NodeKey(0), Mix::new(0, 2));
    let b = chain.add(NodeKey(1), Mix::new(1, 2));
    chain.connect(a, b).unwrap();
    chain.set_output(b).unwrap();
    let mut chain = chain.prepare(&CONFIG).unwrap();
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        inline_below: Some(Duration::ZERO),
        ..options(2, Duration::ZERO)
    });
    assert_eq!(blocks(&mut renderer, &mut chain), (0, 5));
    // Unless every block is to be shared.
    let mut renderer = ParallelRenderer::new(options(2, Duration::ZERO));
    assert_eq!(blocks(&mut renderer, &mut chain), (5, 0));

    // Too little work: the first block of a graph is shared, to find out.
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        inline_below: Some(Duration::from_secs(3600)),
        ..options(2, Duration::ZERO)
    });
    let (mut first, mut second) = (wide(), wide());
    assert_eq!(blocks(&mut renderer, &mut first), (1, 4));
    assert_eq!(blocks(&mut renderer, &mut second), (1, 4));

    // Enough work.
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        inline_below: Some(Duration::ZERO),
        ..options(2, Duration::ZERO)
    });
    assert_eq!(blocks(&mut renderer, &mut wide()), (5, 0));
}

#[test]
fn the_monitor_counts_blocks_and_time() {
    let tempo = TempoMap::default();
    let renderer = ParallelRenderer::new(options(2, Duration::ZERO));
    let monitor = renderer.monitor();
    let mut renderer = renderer;
    let mut graph = fan_in(6, |i| Box::new(Mix::new(i as u8, 2)));
    for _ in 0..10 {
        renderer.render(&mut graph, &block(32, &tempo));
    }
    let stats = monitor.stats();
    assert_eq!(stats.threads, 2);
    assert_eq!((stats.parallel_blocks, stats.inline_blocks), (10, 0));
    assert!(stats.busy > Duration::ZERO && stats.wall > Duration::ZERO);
    assert!(stats.cores_used() > 0.0);
    drop(renderer);
    // Workers add up their time awake as they go to sleep and when they stop.
    assert!(monitor.stats().awake > Duration::ZERO);
}

#[test]
fn dropping_the_renderer_stops_its_workers() {
    let tempo = TempoMap::default();
    let mut graph = fan_in(4, |i| Box::new(Mix::new(i as u8, 2)));
    // Workers asleep, and workers spinning for an hour.
    for spin in [Duration::ZERO, Duration::from_secs(3600)] {
        // Before the workers have even started.
        drop(ParallelRenderer::new(options(3, spin)));
        let mut renderer = ParallelRenderer::new(options(3, spin));
        renderer.render(&mut graph, &block(8, &tempo));
        drop(renderer);
    }
}

/// What a [`Splits`] node's tasks did.
#[derive(Default)]
struct Seen {
    /// How often each task ran.
    runs: [AtomicUsize; 16],
    /// Which lanes ran tasks: the test's own thread, which plays the audio
    /// thread, then each worker.
    lanes: [AtomicBool; 8],
    arrived: AtomicUsize,
    missed: AtomicUsize,
}

impl Seen {
    fn runs(&self, tasks: usize) -> Vec<usize> {
        self.runs[..tasks].iter().map(|r| r.load(Relaxed)).collect()
    }

    fn lanes(&self, lanes: usize) -> Vec<bool> {
        self.lanes[..lanes]
            .iter()
            .map(|l| l.load(Relaxed))
            .collect()
    }
}

/// The lane of the thread running this.
fn lane() -> usize {
    let thread = thread::current();
    let worker = thread
        .name()
        .and_then(|n| n.strip_prefix("musicable-worker-"));
    worker.map_or(0, |n| n.parse().unwrap())
}

/// Splits its work into `tasks` tasks a block. The first `together` to start
/// wait for each other, or until a deadline long past, so a block only
/// finishes in time if that many threads run its tasks at once.
struct Splits {
    tasks: u32,
    together: usize,
    seen: Arc<Seen>,
    blocks: usize,
}

impl Splits {
    fn new(tasks: u32, together: usize) -> (Self, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let splits = Self {
            tasks,
            together,
            seen: seen.clone(),
            blocks: 0,
        };
        (splits, seen)
    }
}

impl Node for Splits {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        let all = self.tasks as usize * self.blocks + self.together;
        self.blocks += 1;
        let seen = &*self.seen;
        crate::tasks::run(self.tasks, &|index| {
            seen.runs[index as usize].fetch_add(1, Relaxed);
            seen.lanes[lane()].store(true, Relaxed);
            seen.arrived.fetch_add(1, AcqRel);
            let deadline = Instant::now() + Duration::from_secs(10);
            while seen.arrived.load(Acquire) < all {
                if Instant::now() > deadline {
                    seen.missed.fetch_add(1, Relaxed);
                    break;
                }
                rt::allowed(thread::yield_now);
            }
        });
        output.clear();
    }
}

/// A graph of `node` alone, which nothing can run alongside.
fn alone(node: impl Node) -> PreparedGraph {
    let mut g = Graph::new();
    let node = g.add(NodeKey(0), node);
    g.set_output(node).unwrap();
    g.prepare(&CONFIG).unwrap()
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_nodes_tasks_run_on_several_threads_at_once() {
    // The workers have no node to run, only the tasks, and each task waits
    // for one on every other thread. They sleep between blocks, so the tasks
    // have to wake them, or spin.
    let threads = 3;
    let tempo = TempoMap::default();
    for spin in [Duration::ZERO, Duration::from_millis(5)] {
        let (splits, seen) = Splits::new(8, threads + 1);
        let mut graph = alone(splits);
        let mut renderer = ParallelRenderer::new(options(threads, spin));
        for _ in 0..10 {
            renderer.render(&mut graph, &block(16, &tempo));
        }
        assert_eq!(seen.missed.load(Relaxed), 0);
        assert_eq!(seen.runs(8), [10; 8], "each task ran once a block");
        assert_eq!(seen.lanes(threads + 1), [true; 4]);
    }
}

#[test]
fn elsewhere_a_nodes_tasks_run_one_after_another() {
    let tempo = TempoMap::default();
    let (splits, seen) = Splits::new(8, 1);
    let mut graph = alone(splits);
    ReferenceRenderer.render(&mut graph, &block(16, &tempo));
    // A parallel renderer with no workers, and one running the block on the
    // audio thread alone.
    let mut renderer = ParallelRenderer::new(options(0, Duration::ZERO));
    renderer.render(&mut graph, &block(16, &tempo));
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        inline_below: Some(Duration::from_secs(3600)),
        ..options(3, Duration::ZERO)
    });
    renderer.render(&mut graph, &block(16, &tempo));
    assert_eq!(renderer.monitor().stats().inline_blocks, 1);
    assert_eq!(seen.runs(8), [3; 8]);
    assert_eq!(seen.lanes(4), [true, false, false, false]);
}

#[test]
fn blocks_after_one_with_tasks_are_shared() {
    // Nothing can run alongside the node, so the renderer would run its
    // blocks on the audio thread alone, but for its tasks. The first block
    // shows them, and the rest are shared, while there's enough work.
    let tempo = TempoMap::default();
    let blocks = |inline_below| {
        let (splits, _) = Splits::new(4, 1);
        let mut graph = alone(splits);
        let mut renderer = ParallelRenderer::new(ParallelOptions {
            inline_below: Some(inline_below),
            ..options(2, Duration::ZERO)
        });
        for _ in 0..5 {
            renderer.render(&mut graph, &block(16, &tempo));
        }
        let stats = renderer.monitor().stats();
        (stats.parallel_blocks, stats.inline_blocks)
    };
    assert_eq!(blocks(Duration::ZERO), (4, 1));
    assert_eq!(blocks(Duration::from_secs(3600)), (0, 5));
}

/// Splits its work into tasks that call [`tasks::run`](crate::tasks::run)
/// themselves, and checks that those run on the same thread.
struct SplitsAgain(Arc<AtomicUsize>);

impl Node for SplitsAgain {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        let inner = &*self.0;
        crate::tasks::run(4, &|_| {
            let outer = lane();
            crate::tasks::run(3, &|_| {
                assert_eq!(lane(), outer);
                inner.fetch_add(1, Relaxed);
            });
        });
        output.clear();
    }
}

#[test]
fn a_task_runs_its_own_tasks_itself() {
    let tempo = TempoMap::default();
    let inner = Arc::new(AtomicUsize::new(0));
    let mut graph = alone(SplitsAgain(inner.clone()));
    let mut renderer = ParallelRenderer::new(options(3, Duration::from_millis(5)));
    for _ in 0..20 {
        renderer.render(&mut graph, &block(16, &tempo));
    }
    assert_eq!(inner.load(Relaxed), 20 * 4 * 3);
}

/// Splits its work into tasks that panic in its first block.
#[derive(Default)]
struct TasksPanicOnce {
    seen: Arc<Seen>,
    panicked: bool,
}

impl Node for TasksPanicOnce {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        let panic = !std::mem::replace(&mut self.panicked, true);
        let seen = &*self.seen;
        crate::tasks::run(8, &|index| {
            seen.runs[index as usize].fetch_add(1, Relaxed);
            if panic {
                rt::allowed(|| panic!("a task failed"));
            }
        });
        output.clear();
    }
}

#[test]
fn a_panic_in_a_task_reaches_the_audio_thread_once_every_task_has_run() {
    let tempo = TempoMap::default();
    let fails = |renderer: &mut dyn Renderer, graph: &mut PreparedGraph| {
        let failed = panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.render(graph, &block(16, &tempo));
        }));
        let payload = failed.expect_err("the block should have failed");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"a task failed"));
    };
    // One after another.
    let node = TasksPanicOnce::default();
    let seen = node.seen.clone();
    fails(&mut ReferenceRenderer, &mut alone(node));
    assert_eq!(seen.runs(8), [1; 8]);
    // On several threads at once.
    let node = TasksPanicOnce::default();
    let seen = node.seen.clone();
    let mut graph = alone(node);
    let mut renderer = ParallelRenderer::new(options(3, Duration::ZERO));
    fails(&mut renderer, &mut graph);
    assert_eq!(seen.runs(8), [1; 8]);
    // The renderer carries on.
    renderer.render(&mut graph, &block(16, &tempo));
    assert_eq!(seen.runs(8), [2; 8]);
}

/// Splits its work into tasks, one of which allocates, which tasks must not
/// do.
#[cfg(debug_assertions)]
struct TaskAllocates;

#[cfg(debug_assertions)]
impl Node for TaskAllocates {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        crate::tasks::run(8, &|index| {
            if index == 5 {
                drop(std::hint::black_box(vec![0u8; 16]));
            }
        });
        output.clear();
    }
}

#[test]
#[cfg(debug_assertions)]
#[cfg_attr(miri, ignore)]
fn allocating_in_a_task_on_any_thread_fails_the_block() {
    let tempo = TempoMap::default();
    let mut renderer = ParallelRenderer::new(options(3, Duration::ZERO));
    let mut graph = alone(TaskAllocates);
    // The task runs on whichever thread takes it.
    for _ in 0..20 {
        let failed = panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.render(&mut graph, &block(16, &tempo));
        }));
        let payload = failed.expect_err("the block should have failed");
        let message = payload.downcast::<String>().unwrap();
        assert!(message.contains("real-time violation"), "{message}");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn nodes_read_the_time_they_take_on_any_thread() {
    timed(|| {
        let mut spun = Vec::new();
        let mut graph = fan_in(4, |i| {
            let (node, time) = spin(Duration::from_micros(100 * (i as u64 + 2)));
            spun.push(time);
            Box::new(node)
        });
        let loads = graph.loads();
        let mut renderer = ParallelRenderer::new(options(3, Duration::from_millis(5)));
        play(&mut renderer, &mut graph, 0, 30);
        assert_eq!(renderer.monitor().stats().parallel_blocks, 30);
        for (key, spun) in (1..).zip(&spun) {
            let load = loads.get(NodeKey(key)).unwrap();
            reads(load, spun)?;
            assert_eq!(load.frames(), 30 * 32);
        }
        Ok(())
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_nodes_tasks_count_towards_it_on_any_thread() {
    // Four tasks a block, which the workers take as they spin between
    // blocks. The node's own thread waits for theirs to finish, which
    // counts towards its longest block, but not its load: the threads
    // that ran them count them.
    timed(|| {
        let node = Spin {
            render: Duration::from_micros(800),
            tasks: 4,
            ..Spin::default()
        };
        let spun = node.spun.clone();
        let mut graph = alone(node);
        let loads = graph.loads();
        let mut renderer = ParallelRenderer::new(options(3, Duration::from_millis(5)));
        play(&mut renderer, &mut graph, 0, 20);
        let load = loads.get(NodeKey(0)).unwrap();
        reads(load, &spun)?;
        let longest = load.take_longest();
        assert!(longest >= Duration::from_micros(200), "{longest:?}");
        // Blocks rendered ahead take their time, but none is the longest.
        play_ahead(&mut renderer, &mut graph, 20 * 64, 10);
        assert_eq!(load.take_longest(), Duration::ZERO);
        Ok(())
    });
}

/// For graphs rendered in blocks long enough to pipeline.
const LONG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 1024,
};

/// A chain of nodes made by `node`, prepared for `config`: the last is the
/// output.
fn chain(
    length: usize,
    config: &EngineConfig,
    mut node: impl FnMut(usize) -> Box<dyn Node>,
) -> PreparedGraph {
    let mut g = Graph::new();
    let mut last = None;
    for i in 0..length {
        let next = g.add_boxed(NodeKey(i as u64), node(i));
        if let Some(last) = last {
            g.connect(last, next).unwrap();
        }
        last = Some(next);
    }
    g.set_output(last.unwrap()).unwrap();
    g.prepare(config).unwrap()
}

/// A block of `frames` from engine time 0, rendered ahead or not.
fn ahead_block(frames: usize, tempo: &TempoMap, ahead: bool) -> Block<'_> {
    Block {
        ahead,
        ..block(frames, tempo)
    }
}

/// Passes its input on after spinning for a while, and keeps the most nodes
/// like it that were rendering at once.
struct Overlaps {
    running: Arc<AtomicUsize>,
    most: Arc<AtomicUsize>,
}

impl Node for Overlaps {
    fn process(&mut self, _: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let now = self.running.fetch_add(1, AcqRel) + 1;
        self.most.fetch_max(now, AcqRel);
        let until = Instant::now() + Duration::from_micros(500);
        while Instant::now() < until {
            spin_loop();
        }
        self.running.fetch_sub(1, AcqRel);
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_pipelined_chain_renders_on_several_threads_at_once() {
    let tempo = TempoMap::default();
    let most = |pipeline: Pipeline| {
        let (running, most) = (Arc::default(), Arc::<AtomicUsize>::default());
        let mut graph = chain(4, &LONG, |_| {
            Box::new(Overlaps {
                running: Arc::clone(&running),
                most: Arc::clone(&most),
            })
        });
        let mut renderer = ParallelRenderer::new(ParallelOptions {
            pipeline,
            ..options(3, Duration::ZERO)
        });
        for _ in 0..5 {
            renderer.render(&mut graph, &ahead_block(1024, &tempo, true));
        }
        most.load(Acquire)
    };
    // Whole, each node of a chain waits for the one before.
    assert_eq!(most(Pipeline::Never), 1);
    // In parts, each renders one part as the next renders the part before.
    assert!(most(Pipeline::Ahead) >= 2);
}

#[test]
fn which_blocks_are_pipelined_follows_the_option() {
    let tempo = TempoMap::default();
    let pipelined = |pipeline, graph: &mut PreparedGraph, frames, ahead| {
        let mut renderer = ParallelRenderer::new(ParallelOptions {
            pipeline,
            ..options(3, Duration::ZERO)
        });
        for _ in 0..3 {
            renderer.render(graph, &ahead_block(frames, &tempo, ahead));
        }
        let stats = renderer.monitor().stats();
        assert_eq!(stats.parallel_blocks, 3, "every block is shared");
        stats.pipelined_blocks
    };
    let mut long = chain(4, &LONG, |i| Box::new(Mix::new(i as u8, 2)));
    assert_eq!(pipelined(Pipeline::Never, &mut long, 1024, true), 0);
    assert_eq!(pipelined(Pipeline::Ahead, &mut long, 1024, false), 0);
    assert_eq!(pipelined(Pipeline::Ahead, &mut long, 1024, true), 3);
    assert_eq!(pipelined(Pipeline::Always, &mut long, 1024, false), 3);
    // Too short for two parts of the shortest.
    let short = 2 * SHORTEST_PART - 1;
    assert_eq!(pipelined(Pipeline::Always, &mut long, short, true), 0);
    // A wide graph keeps the threads busy whole.
    let mut g = Graph::new();
    let sum = g.add(NodeKey(0), Sum::default());
    for i in 1..=16 {
        let source = g.add(NodeKey(i), Mix::new(i as u8, 2));
        g.connect(source, sum).unwrap();
    }
    g.set_output(sum).unwrap();
    let mut wide = g.prepare(&LONG).unwrap();
    assert_eq!(pipelined(Pipeline::Always, &mut wide, 1024, true), 0);

    // The renderer of a graph's live half never pipelines.
    let mut renderer = ParallelRenderer::new(ParallelOptions {
        pipeline: Pipeline::Always,
        ..options(3, Duration::ZERO)
    });
    let mut live = renderer.live_renderer();
    for _ in 0..3 {
        live.render(&mut long, &ahead_block(1024, &tempo, true));
    }
    assert_eq!(renderer.monitor().stats().pipelined_blocks, 0);
}

#[test]
fn a_stage_with_a_node_that_makes_notes_renders_whole() {
    let mut g = Graph::new();
    let maker = g.add(NodeKey(0), Mix::new(0, 2));
    let after = g.add(NodeKey(1), Mix::new(1, 2));
    let played = g.add(NodeKey(2), Mix::new(2, 2));
    g.connect(maker, after).unwrap();
    g.connect(after, played).unwrap();
    g.send_made_notes(maker, played).unwrap();
    g.set_output(played).unwrap();
    let mut graph = g.prepare(&LONG).unwrap();
    let mut renderer = pipelining(3, Duration::ZERO);
    renderer.force_parts(4);
    let tempo = TempoMap::default();
    for n in 0..3 {
        let block = Block {
            time: n * 1024,
            ..block(1024, &tempo)
        };
        rt::scope(|| graph.run(&mut renderer, &block));
    }
    // Two stages each block: the maker's whole, and the one it plays into
    // in parts.
    let stats = renderer.monitor().stats();
    assert_eq!((stats.parallel_blocks, stats.pipelined_blocks), (6, 3));
}

#[test]
#[cfg_attr(miri, ignore)]
fn each_part_of_a_pipelined_node_counts_towards_its_load() {
    // A part left out would read an eighth short. Each part spins long
    // enough that the lane's time between parts, which counts towards the
    // part it goes on to, is next to nothing.
    timed(|| {
        let mut spun = Vec::new();
        let mut graph = chain(3, &LONG, |_| {
            let (node, time) = spin(Duration::from_millis(1));
            spun.push(time);
            Box::new(node)
        });
        let loads = graph.loads();
        let mut renderer = ParallelRenderer::new(options(3, Duration::from_millis(5)));
        play_ahead(&mut renderer, &mut graph, 0, 10);
        assert_eq!(renderer.monitor().stats().pipelined_blocks, 10);
        for (key, spun) in (0..).zip(&spun) {
            let load = loads.get(NodeKey(key)).unwrap();
            reads(load, spun)?;
            assert_eq!(load.frames(), 10 * 1024);
        }
        Ok(())
    });
}

#[test]
fn a_panic_in_a_part_reaches_the_audio_thread() {
    let tempo = TempoMap::default();
    let mut renderer = pipelining(3, Duration::ZERO);
    renderer.force_parts(4);
    for broken in [0, 2, 4] {
        let mut graph = chain(5, &LONG, |i| match i == broken {
            true => Box::new(PanicsOnce::default()),
            false => Box::new(Mix::new(i as u8, 2)),
        });
        let failed = panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.render(&mut graph, &block(1024, &tempo));
        }));
        let payload = failed.expect_err("the block should have failed");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"a node failed"));
        // The rest of the block ran, and the renderer carries on.
        renderer.render(&mut graph, &block(1024, &tempo));
    }
}

//! CPU loads: how long each node takes to render, for any thread to read.
//!
//! Each node of a [`PreparedGraph`](crate::PreparedGraph) has a [`Load`].
//! The renderers add each block's time to it as they run the node, and so
//! does the engine as the node plays its notes ([`Node::play_notes`]) and
//! modulates ([`Node::control`]) before the block renders. A node's tasks
//! count towards it, on whichever threads they run ([`tasks`](crate::tasks)).
//! The readings live in atomics, so reading them never waits on the audio
//! thread, and a node's load carries on across graph swaps, as the node does
//! ([`Node::adopt`]). [`NodeLoads`] holds a graph's loads, for the control
//! thread to read while the graph runs
//! ([`PreparedGraph::loads`](crate::PreparedGraph::loads)).
//!
//! The clock is the cheapest the CPU has: the time-stamp counter on x86-64
//! and the virtual counter on ARM64, which take a few nanoseconds to read,
//! where the system's monotonic clock can take tens. Elsewhere it's the
//! monotonic clock. Ticks become time as they're read, at the rate
//! [`ticks_per_second`] gives.
//!
//! [`Node::play_notes`]: crate::Node::play_notes
//! [`Node::control`]: crate::Node::control
//! [`Node::adopt`]: crate::Node::adopt

use std::sync::OnceLock;
use std::sync::atomic::{
    AtomicBool, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};
use std::time::{Duration, Instant};

use crate::NodeKey;
use crate::graph::Joint;

/// What a node has cost since it started: the time it took, summed over the
/// threads it ran on, the frames it rendered, and its longest block lately.
///
/// A node never runs on two threads at once, so one thread at a time adds
/// its blocks. The threads that run its tasks add theirs before the node's
/// own thread is done with the block.
#[derive(Debug, Default)]
// A cache line of its own, as `CachePadded` has, so threads timing nodes
// side by side don't share one.
#[repr(align(128))]
pub struct Load {
    /// Clock ticks spent on the node, on every thread.
    ticks: AtomicU64,
    /// Frames rendered.
    frames: AtomicU64,
    /// The most ticks a block took, start to end, since the control thread
    /// last took it. Taking it races the audio thread raising it, so it only
    /// goes up with `fetch_max`.
    longest: AtomicU64,
    /// Ticks spent on the block about to render before it renders: playing
    /// notes and modulating.
    before: AtomicU64,
}

impl Load {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a block of `frames` the node rendered, which took `ticks` on the
    /// thread that rendered it, `waited` of them waiting for other threads
    /// to finish the node's tasks: those threads add the tasks' time
    /// themselves ([`add_task`](Self::add_task)). Real-time safe.
    pub(crate) fn add(&self, ticks: u64, waited: u64, frames: usize) {
        let before = self.before.load(Relaxed);
        if before != 0 {
            self.before.store(0, Relaxed);
        }
        let block = ticks.wrapping_add(before);
        // Only one thread adds blocks, and those adding tasks are done, so
        // nothing else writes these now.
        let work = block.saturating_sub(waited);
        let total = self.ticks.load(Relaxed).wrapping_add(work);
        self.ticks.store(total, Relaxed);
        let frames = self.frames.load(Relaxed).wrapping_add(frames as u64);
        self.frames.store(frames, Relaxed);
        if block > self.longest.load(Relaxed) {
            self.longest.fetch_max(block, Relaxed);
        }
    }

    /// Adds `ticks` the node spent playing notes or modulating for the block
    /// about to render, which counts towards that block. Real-time safe.
    pub(crate) fn add_before(&self, ticks: u64) {
        let before = self.before.load(Relaxed).wrapping_add(ticks);
        self.before.store(before, Relaxed);
    }

    /// Adds `ticks` that one of the node's tasks took on another thread,
    /// while the node's own thread renders it. Real-time safe.
    pub(crate) fn add_task(&self, ticks: u64) {
        // Several threads can run the node's tasks at once.
        self.ticks.fetch_add(ticks, Relaxed);
    }

    /// Carries on from `old`, the load of the node this one's node takes
    /// over from. Real-time safe.
    pub(crate) fn carry_on(&self, old: &Load) {
        self.ticks.store(old.ticks.load(Relaxed), Relaxed);
        self.frames.store(old.frames.load(Relaxed), Relaxed);
        self.longest.fetch_max(old.longest.load(Relaxed), Relaxed);
    }

    /// The time the node has taken since it started, summed over the threads
    /// it ran on.
    pub fn busy(&self) -> Duration {
        ticks_to_time(self.ticks.load(Relaxed))
    }

    /// The frames the node has rendered since it started.
    pub fn frames(&self) -> u64 {
        self.frames.load(Relaxed)
    }

    /// The longest any block took since the last call, from the node's first
    /// note played to its last frame rendered, on the thread that rendered
    /// it, waiting for its tasks included: how much of the time the block
    /// lasts the node takes up.
    pub fn take_longest(&self) -> Duration {
        ticks_to_time(self.longest.swap(0, Relaxed))
    }
}

/// Whose time a [`Load`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoadKey {
    /// The node with this key's.
    Node(NodeKey),
    /// A delay's, which delay compensation added at `at`, after the node
    /// with the key `after`, to line its output up with others
    /// ([`Graph::prepare`]). A node can have several delays after it, each
    /// at its own joint.
    ///
    /// [`Graph::prepare`]: crate::Graph::prepare
    Delay { after: NodeKey, at: Joint },
}

impl LoadKey {
    /// The node's key, or the key of the node the delay comes after.
    pub fn node(self) -> NodeKey {
        match self {
            Self::Node(key) | Self::Delay { after: key, .. } => key,
        }
    }
}

/// The loads of a prepared graph's nodes, for the control thread to read
/// while the graph runs.
#[derive(Debug)]
pub struct NodeLoads {
    /// Whose each load is, by node.
    keys: Vec<LoadKey>,
    /// By node.
    pub(crate) loads: Vec<Load>,
    /// Whether the engine has installed the graph, each load having carried
    /// on from the graph before.
    installed: AtomicBool,
}

impl NodeLoads {
    /// Loads for the nodes `keys` say, in order.
    pub(crate) fn new(keys: Vec<LoadKey>) -> Self {
        // Off the audio thread, so the clock is ready before it's read there.
        ticks_per_second();
        let loads = keys.iter().map(|_| Load::new()).collect();
        Self {
            keys,
            loads,
            installed: AtomicBool::new(false),
        }
    }

    /// Whether the engine has installed the graph. Until it has, the loads
    /// read nothing, or only part of what they carry on from the graph
    /// before, as the engine carries them on one by one; once it has, each
    /// reads at least what it carried on.
    pub fn installed(&self) -> bool {
        self.installed.load(Acquire)
    }

    /// Says that the engine has installed the graph, once every load has
    /// carried on from the graph before. Real-time safe.
    pub(crate) fn set_installed(&self) {
        self.installed.store(true, Release);
    }

    /// Carries the `n`th load on from `old`'s `old_n`th, the load of the
    /// node the `n`th takes over from, if they're both whose they are: a
    /// delay can take over from one at the same joint after another node,
    /// such as the delay after a tap when the taps change. Real-time safe.
    pub(crate) fn carry_on(&self, n: usize, old: &NodeLoads, old_n: usize) {
        if self.keys[n] == old.keys[old_n] {
            self.loads[n].carry_on(&old.loads[old_n]);
        }
    }

    /// Each node's load, with whose it is.
    pub fn iter(&self) -> impl Iterator<Item = (LoadKey, &Load)> {
        self.keys.iter().copied().zip(&self.loads)
    }

    /// The load of the node with `key`, if the graph has one.
    pub fn get(&self, key: NodeKey) -> Option<&Load> {
        self.iter()
            .find(|&(k, _)| k == LoadKey::Node(key))
            .map(|(_, load)| load)
    }

    pub fn len(&self) -> usize {
        self.loads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.loads.is_empty()
    }
}

/// Reads the clock, in ticks. Real-time safe: it reads a register, or the
/// monotonic clock, which the operating system serves without a system call
/// on the platforms Musicable runs on.
#[inline]
pub(crate) fn now() -> u64 {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        // The time-stamp counter, which every x86-64 CPU of the last decade
        // or so runs at one rate on every core, whatever the core's clock.
        // SAFETY: RDTSC is in every x86-64 CPU, and reads a counter.
        unsafe { std::arch::x86_64::_rdtsc() }
    }
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    {
        let ticks: u64;
        // SAFETY: CNTVCT_EL0 is readable at EL0 on every ARM64 system
        // Musicable runs on, macOS's and Windows' included, and reading it
        // touches nothing else.
        unsafe {
            std::arch::asm!("mrs {}, cntvct_el0", out(reg) ticks, options(nomem, nostack));
        }
        ticks
    }
    #[cfg(any(not(any(target_arch = "x86_64", target_arch = "aarch64")), miri))]
    {
        EPOCH.get().map_or(0, |at| at.elapsed().as_nanos() as u64)
    }
}

/// Where the monotonic clock's ticks count from, where [`now`] reads it.
#[cfg(any(not(any(target_arch = "x86_64", target_arch = "aarch64")), miri))]
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// The clock's rate, once known.
static RATE: OnceLock<f64> = OnceLock::new();

/// How many ticks of the clock make a second. It stays the same once known,
/// so differences between readings are exact. Not on the audio thread: on
/// x86-64 the first call measures the counter for 10 ms, which preparing
/// the first graph does ([`NodeLoads`]).
pub fn ticks_per_second() -> f64 {
    *RATE.get_or_init(rate)
}

/// The monotonic clock's rate, in nanoseconds, from when it's first read.
#[cfg(any(not(any(target_arch = "x86_64", target_arch = "aarch64")), miri))]
fn rate() -> f64 {
    EPOCH.get_or_init(Instant::now);
    1e9
}

/// The virtual counter's rate, which the system sets as it starts and
/// keeps in CNTFRQ_EL0, or else as it measures.
#[cfg(all(target_arch = "aarch64", not(miri)))]
fn rate() -> f64 {
    let hertz: u64;
    // SAFETY: CNTFRQ_EL0 is readable at EL0 wherever CNTVCT_EL0 is, and
    // reading it touches nothing else.
    unsafe {
        std::arch::asm!("mrs {}, cntfrq_el0", out(reg) hertz, options(nomem, nostack));
    }
    if hertz > 0 { hertz as f64 } else { measure() }
}

/// The time-stamp counter's rate, as it measures.
#[cfg(all(target_arch = "x86_64", not(miri)))]
fn rate() -> f64 {
    measure()
}

/// Measures the counter against the monotonic clock over 10 ms, long enough
/// that the monotonic clock's own grain doesn't count, and short enough that
/// nothing, such as the computer sleeping, comes between.
#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), not(miri)))]
fn measure() -> f64 {
    const SHORTEST: Duration = Duration::from_millis(10);
    let (start, ticks) = reading();
    while start.elapsed() < SHORTEST {
        std::hint::spin_loop();
    }
    let (end, end_ticks) = reading();
    end_ticks.wrapping_sub(ticks) as f64 / (end - start).as_secs_f64()
}

/// The counter, with the monotonic clock's time as it was read: halfway
/// between two of the monotonic clock's readings around it, the closest
/// together of a few tries, so that the thread being preempted between
/// them can't skew the rate.
#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), not(miri)))]
fn reading() -> (Instant, u64) {
    let mut closest: Option<(Duration, Instant, u64)> = None;
    for _ in 0..8 {
        let before = Instant::now();
        let ticks = now();
        let between = before.elapsed();
        if closest.is_none_or(|(apart, ..)| between < apart) {
            closest = Some((between, before + between / 2, ticks));
        }
        if between < Duration::from_micros(2) {
            break;
        }
    }
    let (_, at, ticks) = closest.expect("the clock was read");
    (at, ticks)
}

fn ticks_to_time(ticks: u64) -> Duration {
    Duration::from_secs_f64(ticks as f64 / ticks_per_second())
}

/// The clock's ticks in `time`, for tests.
#[cfg(test)]
pub(crate) fn ticks(time: Duration) -> u64 {
    (time.as_secs_f64() * ticks_per_second()) as u64
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use musicable_model::time::TempoMap;

    use super::*;
    use crate::nodes::{Delay, Latent, Spin, Sum};
    use crate::{
        AudioRef, Block, Engine, EngineConfig, Graph, ParamId, PreparedGraph, ReferenceRenderer,
        Renderer, TransportState, rt,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    #[test]
    fn ticks_become_time() {
        ticks_per_second();
        let started = Instant::now();
        let first = now();
        while started.elapsed() < Duration::from_millis(30) {
            std::hint::spin_loop();
        }
        let ticks = now() - first;
        let measured = ticks_to_time(ticks).as_secs_f64();
        let real = started.elapsed().as_secs_f64();
        // On x86-64 the rate was measured over 10 ms, so it's close, not exact.
        assert!(
            (measured / real - 1.0).abs() < 0.05,
            "{measured} s measured over {real} s"
        );
    }

    #[test]
    fn a_load_adds_up_blocks_and_keeps_the_longest() {
        let load = Load::new();
        let long = ticks(Duration::from_micros(500));
        load.add(long, 0, 128);
        load.add(long / 10, 0, 128);
        assert_eq!(load.frames(), 256);
        let busy = load.busy().as_secs_f64();
        assert!((busy / 550e-6 - 1.0).abs() < 0.01, "{busy}");
        let longest = load.take_longest().as_secs_f64();
        assert!((longest / 500e-6 - 1.0).abs() < 0.01, "{longest}");
        assert_eq!(load.take_longest(), Duration::ZERO, "taking it clears it");
    }

    #[test]
    fn notes_and_modulation_count_towards_the_next_block() {
        let load = Load::new();
        load.add_before(300);
        load.add_before(200);
        load.add(1000, 0, 64);
        assert_eq!(load.ticks.load(Relaxed), 1500);
        assert_eq!(load.longest.load(Relaxed), 1500);
        load.add(100, 0, 64);
        assert_eq!(load.ticks.load(Relaxed), 1600, "the block after has none");
    }

    #[test]
    fn waiting_for_tasks_counts_towards_the_longest_block_only() {
        let load = Load::new();
        // Tasks on other threads took 700 ticks, while the node's own thread
        // waited 400 of the 1000 it took.
        load.add_task(300);
        load.add_task(400);
        load.add(1000, 400, 64);
        assert_eq!(load.ticks.load(Relaxed), 1300);
        assert_eq!(load.longest.load(Relaxed), 1000);
    }

    #[test]
    fn a_load_carries_on_from_the_old_one() {
        let old = Load::new();
        old.add(1000, 0, 64);
        old.add_task(500);
        let new = Load::new();
        new.carry_on(&old);
        new.add(100, 0, 64);
        assert_eq!(new.ticks.load(Relaxed), 1600);
        assert_eq!(new.frames(), 128);
        assert_eq!(new.longest.load(Relaxed), 1000);
    }

    /// A spinning node and the time it spins.
    pub(crate) fn spin(render: Duration) -> (Spin, Arc<AtomicU64>) {
        let spin = Spin {
            render,
            ..Spin::default()
        };
        let spun = spin.spun.clone();
        (spin, spun)
    }

    /// Plays `blocks` blocks of `graph`, as long as it takes, from engine
    /// time `from`, as the engine does: the notes and the modulation, then
    /// the block.
    pub(crate) fn play(
        renderer: &mut dyn Renderer,
        graph: &mut PreparedGraph,
        from: u64,
        blocks: usize,
    ) {
        let tempo = TempoMap::default();
        let frames = graph.config().max_block;
        for n in 0..blocks {
            let block = Block {
                frames,
                time: from + (n * frames) as u64,
                transport: TransportState::default(),
                tempo: &tempo,
                input: AudioRef::empty(frames),
                midi: &[],
            };
            rt::scope(|| {
                graph.play_notes(&block);
                graph.control(&block);
                graph.load_routed();
                renderer.render(graph, &block);
                graph.clear_routed();
            });
        }
    }

    /// Asserts that `load` reads `spun` nanoseconds, within 5%.
    #[track_caller]
    pub(crate) fn assert_reads(load: &Load, spun: &AtomicU64) {
        let spun = spun.load(Relaxed) as f64 * 1e-9;
        let busy = load.busy().as_secs_f64();
        assert!(spun > 0.0, "the node spun");
        assert!(
            (busy / spun - 1.0).abs() < 0.05,
            "{busy} s read for {spun} s spun"
        );
    }

    #[test]
    fn each_node_reads_the_time_it_takes() {
        let mut g = Graph::new();
        let sum = g.add(NodeKey(0), Sum::default());
        let (short, short_spun) = spin(Duration::from_micros(200));
        let (long, long_spun) = spin(Duration::from_micros(500));
        for (key, node) in [(1, short), (2, long)] {
            let node = g.add(NodeKey(key), node);
            g.connect(node, sum).unwrap();
        }
        g.set_output(sum).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let loads = graph.loads();
        play(&mut ReferenceRenderer, &mut graph, 0, 40);
        let short = loads.get(NodeKey(1)).unwrap();
        let long = loads.get(NodeKey(2)).unwrap();
        assert_reads(short, &short_spun);
        assert_reads(long, &long_spun);
        assert_eq!(long.frames(), 40 * 64);
        let longest = long.take_longest();
        assert!(longest >= Duration::from_micros(500), "{longest:?}");
        // Summing silence takes next to nothing.
        let sum = loads.get(NodeKey(0)).unwrap();
        assert!(sum.busy() * 20 < short.busy(), "{:?}", sum.busy());
        assert_eq!(sum.frames(), 40 * 64);
    }

    #[test]
    fn playing_notes_and_modulating_count_towards_a_node() {
        let mut g = Graph::new();
        let sum = g.add(NodeKey(0), Sum::default());
        let before = Duration::from_micros(150);
        let player = Spin {
            render: Duration::from_micros(50),
            before,
            ..Spin::default()
        };
        let modulator = Spin {
            before,
            ..Spin::default()
        };
        let (played, modulated) = (player.spun.clone(), modulator.spun.clone());
        let player = g.add(NodeKey(1), player);
        let modulator = g.add(NodeKey(2), modulator);
        g.send_notes(player, sum).unwrap();
        g.modulate(modulator, sum, ParamId(0)).unwrap();
        g.set_output(sum).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let loads = graph.loads();
        play(&mut ReferenceRenderer, &mut graph, 0, 40);
        let player = loads.get(NodeKey(1)).unwrap();
        assert_reads(player, &played);
        assert_reads(loads.get(NodeKey(2)).unwrap(), &modulated);
        // A block's time counts from the notes it plays.
        let longest = player.take_longest();
        assert!(longest >= Duration::from_micros(200), "{longest:?}");
    }

    #[test]
    fn loads_carry_on_across_swaps() {
        // A spinning node on a latent path and a dry one, so delay
        // compensation adds a delay after the dry one.
        let graph = |latency: usize| {
            let mut g = Graph::new();
            let sum = g.add(NodeKey(0), Sum::default());
            let (node, spun) = spin(Duration::from_micros(100));
            let source = g.add(NodeKey(1), node);
            let wet = g.add(NodeKey(2), Latent(Delay::new(latency)));
            g.connect(source, wet).unwrap();
            g.connect(wet, sum).unwrap();
            g.connect(source, sum).unwrap();
            g.set_output(sum).unwrap();
            (g.prepare(&CONFIG).unwrap(), spun)
        };
        let (mut first, spun_first) = graph(10);
        play(&mut ReferenceRenderer, &mut first, 0, 10);
        let (mut second, spun_second) = graph(20);
        let loads = second.loads();
        let delay = |loads: &NodeLoads| {
            let after = |key| {
                matches!(
                    key,
                    LoadKey::Delay {
                        after: NodeKey(1),
                        ..
                    }
                )
            };
            let mut delays = loads.iter().filter(|&(key, _)| after(key));
            let (_, delay) = delays.next().expect("a delay after the dry path");
            assert!(delays.next().is_none());
            delay.frames()
        };
        assert_eq!(delay(&first.loads()), 640);
        // Until the engine installs it, a graph's loads read nothing.
        assert_eq!(loads.get(NodeKey(1)).unwrap().frames(), 0);
        rt::scope(|| second.adopt_from(&mut first));
        play(&mut ReferenceRenderer, &mut second, 640, 10);
        let spinner = loads.get(NodeKey(1)).unwrap();
        assert_eq!(spinner.frames(), 1280);
        spun_second.fetch_add(spun_first.load(Relaxed), Relaxed);
        assert_reads(spinner, &spun_second);
        assert_eq!(delay(&loads), 1280, "delays carry on from theirs too");
    }

    #[test]
    fn each_delay_has_a_key_of_its_own() {
        // A source into two sums, each with a latent path from the source
        // beside it, so delay compensation adds a delay after the source
        // into each sum.
        let mut g = Graph::new();
        let output = g.add(NodeKey(0), Sum::default());
        let source = g.add(NodeKey(1), Spin::default());
        for (key, latency) in [(2, 10), (4, 20)] {
            let wet = g.add(NodeKey(key), Latent(Delay::new(latency)));
            let sum = g.add(NodeKey(key + 1), Sum::default());
            g.connect(source, wet).unwrap();
            g.connect(wet, sum).unwrap();
            g.connect(source, sum).unwrap();
            g.connect(sum, output).unwrap();
        }
        g.set_output(output).unwrap();
        let graph = g.prepare(&CONFIG).unwrap();
        let mut keys: Vec<LoadKey> = graph.loads().iter().map(|(key, _)| key).collect();
        let into = |to| LoadKey::Delay {
            after: NodeKey(1),
            at: Joint::Input {
                to: NodeKey(to),
                from: NodeKey(1),
                nth: 0,
            },
        };
        assert!(keys.contains(&into(3)), "{keys:?}");
        assert!(keys.contains(&into(5)), "{keys:?}");
        let len = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), len, "no two loads are the same one's");
    }

    #[test]
    fn a_delay_carries_on_only_from_one_after_the_same_node() {
        // Taps lined up with a latent output. When another tap comes first,
        // the delay after the first tap is after another node, and its load
        // starts again, as does the delay after the tap that's now second.
        let graph = |side_first: bool| {
            let mut g = Graph::new();
            let source = g.add(NodeKey(0), Spin::default());
            let side = g.add(NodeKey(1), Spin::default());
            let wet = g.add(NodeKey(2), Latent(Delay::new(20)));
            g.connect(source, wet).unwrap();
            g.set_output(wet).unwrap();
            if side_first {
                g.add_tap(side).unwrap();
            }
            g.add_tap(source).unwrap();
            g.prepare(&CONFIG).unwrap()
        };
        let tap = |n, after| LoadKey::Delay {
            after: NodeKey(after),
            at: Joint::Tap(n),
        };
        let frames = |loads: &NodeLoads, key| {
            let (_, load) = loads.iter().find(|&(k, _)| k == key).expect("a load");
            load.frames()
        };
        let mut first = graph(false);
        play(&mut ReferenceRenderer, &mut first, 0, 10);
        assert_eq!(frames(&first.loads(), tap(0, 0)), 640);
        let mut second = graph(true);
        rt::scope(|| second.adopt_from(&mut first));
        play(&mut ReferenceRenderer, &mut second, 640, 10);
        let loads = second.loads();
        assert_eq!(frames(&loads, tap(0, 1)), 640, "after another node");
        assert_eq!(frames(&loads, tap(1, 0)), 640, "at another joint");
        assert_eq!(frames(&loads, LoadKey::Node(NodeKey(0))), 1280);
    }

    #[test]
    fn loads_are_whole_once_the_engine_installs_their_graph() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let mut buffer = vec![0.0; 2 * CONFIG.max_block];
        for blocks in 1..=2 {
            let mut g = Graph::new();
            let source = g.add(NodeKey(0), Spin::default());
            g.set_output(source).unwrap();
            let graph = g.prepare(&CONFIG).unwrap();
            let loads = graph.loads();
            handle.swap_graph(graph).unwrap();
            assert!(!loads.installed());
            engine.process_interleaved(&mut buffer, 2);
            assert!(loads.installed());
            // The second carried on from the first.
            let frames = loads.get(NodeKey(0)).unwrap().frames();
            assert_eq!(frames, blocks * CONFIG.max_block as u64);
        }
    }
}

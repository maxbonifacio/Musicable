//! The parallel renderer: runs a graph's nodes on several real-time threads
//! at once, as many as the graph allows.
//!
//! The audio thread works too, as lane 0; each worker thread is another lane.
//! A block goes like this:
//!
//! 1. The audio thread describes the block in a [`Job`] and publishes it by
//!    bumping the epoch. Workers that are spinning see it at once; sleeping
//!    ones are woken in a tree, each waking two more, so the audio thread
//!    makes at most two wake-up calls.
//! 2. Lanes take nodes without inputs (sources) from a shared counter. When a
//!    lane finishes a node it counts it off each node it feeds. Of the nodes
//!    that makes ready, the one with the longest path to the end of the graph
//!    runs next on the same lane, and the rest go on the lane's own deque.
//!    Lanes with nothing to do steal from other lanes' deques.
//! 3. The block ends when every node has run. The audio thread waits for
//!    that, working all the while, and returns.
//!
//! Workers only touch the graph and the job after taking one of the block's
//! nodes, and count their nodes as finished only when they are done with the
//! graph. So the block can't end, and the graph can't be swapped out, while
//! any worker is still using it. A worker still looking for work when its
//! block ends can't take a node on the strength of what it saw before: the
//! sources counter carries the block's epoch, and the index thieves steal at
//! only grows, so a stale attempt fails.
//!
//! Between blocks, idle workers spin for [`ParallelOptions::spin`], then
//! sleep. Blocks with little work run on the audio thread alone
//! ([`ParallelOptions::inline_below`]), since waking workers would cost more
//! than they save.
//!
//! When the engine renders ahead of the device
//! ([`EngineHandle::set_lead`](crate::EngineHandle::set_lead)), the workers
//! render its ahead side between callbacks, one block at a time, and the
//! worker that takes it starts each block from its own lane, as the audio
//! thread does from lane 0: see [`AheadWaker`].
//!
//! The pool has two boards, each with a block of its own, so a graph's live
//! half and the rest can render at once ([`Graph::prepare`]): the main board
//! for what the engine's ahead side renders, ahead or as the device asks,
//! and the live board for the live half, which the audio thread renders
//! with the renderer [`Renderer::live_renderer`] gives. A block rendered
//! ahead while the live side renders a live half leaves two workers out,
//! the last two, which never render ahead either, so the live half has the
//! audio thread and them. The live board wakes them first. A lane with nothing to do on
//! the main board helps with the live board's block, and a lane helps only
//! with the tasks of nodes on its own board.
//!
//! [`Graph::prepare`]: crate::Graph::prepare
//!
//! A block rendered ahead can be pipelined ([`Pipeline`]): split into
//! parts that run down the graph at once, each node rendering its parts in
//! order, so a long chain of nodes renders on several threads, one node on
//! one part as the node after it renders the part before. Each part of a
//! node is a piece of work of its own, ready once that part of each of its
//! inputs and its own part before have run: the lane that finishes a part
//! goes on with the node's next part, whose state it has warm, and leaves
//! the parts it made ready for the nodes after for other lanes to take.
//! Each part is a block of its own to the node ([`Block::part`]), with the
//! events for its frames, and a node's output for it is those frames of its
//! buffer, so nothing outside the renderer changes. Nodes that play the same
//! whatever blocks they render, as rendering ahead already needs, play the
//! same pipelined, sample for sample. How many parts a block takes depends
//! on how many of the graph's nodes can run at once on average: a wide graph
//! keeps the lanes busy whole, and a chain takes two parts for each lane,
//! so a lane that falls behind holds up none of the others.
//!
//! A node can split its own work into tasks ([`tasks::run`](crate::tasks)),
//! as a plugin using CLAP's thread pool does. Its lane puts them in its
//! [`Tasks`], wakes up to two sleeping workers, and takes tasks itself until
//! none are left; each worker that takes one wakes up to two more while
//! tasks are left. Lanes with no node to run take tasks too. A block the
//! audio thread renders alone runs a node's tasks there, one after another,
//! and the blocks after it are shared, if they take enough work, even when
//! nothing in them can run alongside anything else.

mod deque;
mod priority;
mod schedule;
#[cfg(test)]
mod tests;

use std::any::Any;
use std::cell::{Cell, UnsafeCell};
use std::hint::spin_loop;
use std::ops::{Deref, Range};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

pub(crate) use schedule::Schedule;

use self::deque::{Deque, Steal};
use crate::load::{self, Load};
use crate::{
    AheadWork, Block, InputPort, Inputs, Node, NodeEvent, NodeId, PreparedGraph, ReferenceRenderer,
    Renderer, rt,
};

/// How a [`ParallelRenderer`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelOptions {
    /// Worker threads, besides the audio thread, which works too. The default
    /// is one per physical core, minus one for the audio thread, and no more
    /// than 24 on Windows ([`ParallelRenderer::default_threads`]).
    pub threads: usize,
    /// How long a worker with nothing to do keeps checking for the next block
    /// before it goes to sleep. Spinning burns a core; waking a sleeping
    /// thread takes the operating system tens of microseconds. The default,
    /// 20 µs, covers blocks that follow each other within one device
    /// callback, not the gap between callbacks.
    pub spin: Duration,
    /// Render a block on the audio thread alone when the block before it took
    /// less work than this, summed over threads: waking workers would cost
    /// more than they save. `None` shares every block between the threads.
    /// The default is 100 µs.
    pub inline_below: Option<Duration>,
    /// The most nodes a graph can have for the renderer to share it between
    /// threads. Each thread's work queue holds this many nodes. Bigger graphs
    /// render on the audio thread alone. The default is 16,384.
    pub max_nodes: usize,
    /// Run the workers at real-time priority: in the MMCSS "Pro Audio" class
    /// on Windows. Other platforms don't raise the priority yet.
    pub realtime: bool,
    /// Which blocks to pipeline: split into parts that run down the graph
    /// at once, so a long chain renders on several threads. The default
    /// pipelines the blocks rendered ahead of the device.
    pub pipeline: Pipeline,
}

/// Which blocks a [`ParallelRenderer`] pipelines: splits into parts, up to
/// [`PARTS`] and none shorter than [`SHORTEST_PART`] frames, that run down
/// the graph at once, each node rendering its parts in order. It never
/// pipelines a graph's live half, nor a stage of a graph with a node that
/// makes notes as it renders, which hands them on after the whole block
/// ([`Graph::send_made_notes`](crate::Graph::send_made_notes)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pipeline {
    /// None: each block renders whole.
    Never,
    /// Those rendered ahead of the device ([`Block::ahead`]), which it
    /// waits for only once the lead runs out.
    #[default]
    Ahead,
    /// Every block, as for an export, where no device waits for any.
    Always,
}

/// The most parts a block is pipelined in.
pub const PARTS: usize = 16;

/// The fewest frames in a part of a pipelined block: a shorter part would
/// cost more in handing it over than its node saves.
pub const SHORTEST_PART: usize = 128;

/// How a pipelined block's pieces of work are numbered: node `n`'s part
/// `p` is `n << PART_BITS | p`.
const PART_BITS: u32 = PARTS.trailing_zeros();
const _: () = assert!(PARTS.is_power_of_two(), "a part's number takes its bits");

impl Default for ParallelOptions {
    fn default() -> Self {
        Self {
            threads: ParallelRenderer::default_threads(),
            spin: Duration::from_micros(20),
            inline_below: Some(Duration::from_micros(100)),
            max_nodes: 1 << 14,
            realtime: true,
            pipeline: Pipeline::default(),
        }
    }
}

/// Runs each block's nodes on a pool of worker threads and the audio thread,
/// as many at once as the graph allows. Its output matches
/// [`ReferenceRenderer`]'s sample for sample.
///
/// Create it off the audio thread, since it starts the workers, and drop it
/// off the audio thread too, since that stops them. Pass it to
/// [`Engine::with_renderer`](crate::Engine::with_renderer).
pub struct ParallelRenderer {
    shared: Arc<Shared>,
    /// The workers, which only the renderer that started them has: not the
    /// one for the live half it gives ([`Renderer::live_renderer`]).
    workers: Vec<JoinHandle<()>>,
    /// The board it renders on: [`MAIN`], or [`LIVE`] for the live half.
    board: usize,
    inline_below: Option<Duration>,
    max_nodes: usize,
    pipeline: Pipeline,
    /// Parallel blocks rendered on its board.
    epoch: u64,
    /// Whether the last block ran on the audio thread alone, for each of
    /// the first [`STAGES`] stages of a graph, the last for the rest.
    inline: [bool; STAGES],
    /// The work the last block took, summed over threads, for each stage
    /// the same way.
    last_work: [Duration; STAGES],
    /// The address of the graph the last block ran, to notice a new one.
    last_graph: usize,
    /// How many parts to pipeline each block in, for a test.
    #[cfg(test)]
    forced_parts: Option<usize>,
}

impl ParallelRenderer {
    /// One worker per physical core, minus one for the audio thread. On
    /// Windows, no more than 24: Windows gives a process 32 real-time (MMCSS)
    /// threads, and the audio thread, audio drivers and plugins need some of
    /// them.
    pub fn default_threads() -> usize {
        priority::cap(num_cpus::get_physical().saturating_sub(1))
    }

    /// Starts the worker threads.
    ///
    /// # Panics
    ///
    /// If the operating system can't start a thread.
    pub fn new(options: ParallelOptions) -> Self {
        let lanes = (0..=options.threads)
            .map(|_| Lane {
                deques: [MAIN, LIVE].map(|_| Deque::new(options.max_nodes)),
                sleeping: CachePadded(AtomicBool::new(false)),
                thread: OnceLock::new(),
                tasks: Tasks::default(),
            })
            .collect();
        let shared = Arc::new(Shared {
            lanes,
            boards: [MAIN, LIVE].map(|_| Board::default()),
            kept: KEPT.min(options.threads.saturating_sub(1)),
            shutdown: AtomicBool::new(false),
            spin: options.spin,
            counters: Counters::default(),
            ahead: OnceLock::new(),
            ahead_wanted: AtomicBool::new(false),
            ahead_inside: AtomicUsize::new(0),
            ahead_closed: AtomicBool::new(false),
            live_half: AtomicBool::new(false),
            lane_zero: AtomicBool::new(false),
        });
        let workers = (1..=options.threads)
            .map(|lane| {
                let shared = shared.clone();
                let realtime = options.realtime;
                thread::Builder::new()
                    .name(format!("musicable-worker-{lane}"))
                    // Like the audio thread while it renders, so the nodes
                    // a worker runs give the same samples there.
                    .spawn(move || rt::flush_denormals(|| shared.worker(lane, realtime)))
                    .expect("failed to start a worker thread")
            })
            .collect();
        Self {
            shared,
            workers,
            board: MAIN,
            inline_below: options.inline_below,
            max_nodes: options.max_nodes,
            pipeline: options.pipeline,
            epoch: 0,
            inline: [false; STAGES],
            last_work: [Duration::MAX; STAGES],
            last_graph: 0,
            #[cfg(test)]
            forced_parts: None,
        }
    }

    /// Worker threads, not counting the audio thread.
    pub fn threads(&self) -> usize {
        self.shared.lanes.len() - 1
    }

    /// A handle for reading the renderer's counters from another thread, such
    /// as the control thread, after the engine has taken the renderer.
    pub fn monitor(&self) -> ParallelMonitor {
        ParallelMonitor(self.shared.clone())
    }

    /// Has the renderer pipeline every block in `parts` parts, or as many
    /// as it has frames, whatever the graph, as long as it may pipeline it.
    #[cfg(test)]
    pub(crate) fn force_parts(&mut self, parts: usize) {
        self.forced_parts = Some(parts.clamp(1, PARTS));
    }

    /// How many parts to render `block` of `graph`'s current stage in: one,
    /// unless it pipelines the block, on `lanes` lanes.
    fn parts(&self, graph: &PreparedGraph, block: &Block<'_>, lanes: usize) -> usize {
        let pipelines = match self.pipeline {
            Pipeline::Never => false,
            Pipeline::Ahead => block.ahead,
            Pipeline::Always => true,
        };
        // Its pieces of work have to fit their numbers.
        let fits = graph.len() < 1 << (u32::BITS - PART_BITS);
        if !pipelines || !fits || graph.stage_makes_notes() {
            return 1;
        }
        // Each piece of work is pushed onto a deque at most once a block,
        // and a deque holds `max_nodes`: no more pieces than that.
        let room = (self.max_nodes / graph.len().max(1)).max(1);
        #[cfg(test)]
        if let Some(parts) = self.forced_parts {
            return parts.min(block.frames).min(room).max(1);
        }
        let most = (block.frames / SHORTEST_PART).min(PARTS).min(room);
        graph
            .schedule
            .parts(graph.stage, lanes)
            .clamp(1, most.max(1))
    }

    /// Whether to share this block, in `parts` parts, between threads.
    fn share(&mut self, graph: &PreparedGraph, parts: usize) -> bool {
        let tasks = self.shared.boards[self.board]
            .tasks_asked
            .swap(false, Relaxed);
        if self.shared.lanes.len() == 1 || graph.len() > self.max_nodes {
            return false;
        }
        let address = graph as *const PreparedGraph as usize;
        if address != self.last_graph {
            // A new graph: assume it's worth sharing until it shows otherwise.
            self.last_graph = address;
            self.last_work = [Duration::MAX; STAGES];
        }
        let Some(below) = self.inline_below else {
            return true;
        };
        // Nothing can run alongside anything else, and no node splits its
        // work into tasks, or there's too little to do. Once inline, stay
        // there until there's clearly enough work, so a graph near the line
        // doesn't flip every block.
        let stage = graph.stage.min(STAGES - 1);
        let below = if self.inline[stage] { below * 2 } else { below };
        let wide = graph.schedule.width(graph.stage) > 1 || parts > 1;
        (wide || tasks) && self.last_work[stage] >= below
    }

    fn render_inline(&mut self, graph: &mut PreparedGraph, block: &Block<'_>) {
        let started = Instant::now();
        // Nodes that ask to run tasks run them here, and get the next block
        // shared.
        let _lane = WorkingFor::enter(&self.shared, INLINE, self.board);
        ReferenceRenderer.render(graph, block);
        let stage = graph.stage.min(STAGES - 1);
        self.last_work[stage] = started.elapsed();
        self.inline[stage] = true;
        self.shared.counters.inline_blocks.fetch_add(1, Relaxed);
    }

    /// Shares the block, in `parts` parts, between `open` lanes: all, or
    /// all but the last [`Shared::kept`].
    fn render_shared(
        &mut self,
        graph: &mut PreparedGraph,
        block: &Block<'_>,
        open: usize,
        parts: usize,
    ) {
        let shared = &*self.shared;
        let board = &shared.boards[self.board];
        // The audio thread's lane, or the worker's that renders ahead.
        let lane = DRIVER.get();
        graph.schedule.point_at(&mut graph.buffers);
        let job = Job {
            nodes: graph.nodes.as_mut_ptr(),
            inputs: graph.inputs.as_ptr(),
            ports: graph.ports.as_ptr(),
            events: graph.events.as_mut_ptr(),
            event_ranges: graph.event_ranges.as_ptr(),
            schedule: &graph.schedule,
            stage: graph.stage,
            block: (block as *const Block<'_>).cast(),
            parts,
            sample_rate: graph.config.sample_rate,
            loads: graph.loads.loads[graph.load_base..].as_ptr(),
        };
        let started = Instant::now();
        // From the board's, as another renderer may have used it before.
        self.epoch = board.epoch.load(Relaxed) + 1;
        // Nothing reads these until it takes a node of the new block, which
        // the store to `sources` makes possible.
        debug_assert!(board.job.load(Relaxed).is_null(), "one block at a time");
        board.job.store((&job as *const Job).cast_mut(), Relaxed);
        board
            .remaining
            .store(graph.stage_nodes().len() * parts, Relaxed);
        board.open.store(open, Relaxed);
        // A worker starting the block works in place of lane 0, which is
        // the audio thread's and idle unless it comes to help: one lane
        // more keeps as many threads busy.
        let width = graph.schedule.width(graph.stage) * parts;
        let wanted = width + usize::from(lane != 0 && lane < width);
        board.wanted.store(wanted.min(open), Relaxed);
        let sources = graph.schedule.sources(graph.stage).len() as u64;
        board.sources.store(tag(self.epoch) | sources, Release);
        // Wake the workers. SeqCst pairs with the check sleeping workers make
        // before they park: either they see the new epoch, or we see them
        // asleep.
        board.epoch.store(self.epoch, SeqCst);
        // The tree starts at lane 0. A worker starting the block wakes the
        // lanes it would have woken too.
        shared.wake_children(self.board, 0);
        if lane != 0 {
            shared.wake_children(self.board, lane);
        }
        shared.work(self.board, lane, self.epoch);
        // Every node has run, and every thread is done with the graph and the
        // job.
        let wall = started.elapsed();
        let busy = Duration::from_nanos(board.block_busy.swap(0, Relaxed));
        board.job.store(std::ptr::null_mut(), Relaxed);
        let stage = graph.stage.min(STAGES - 1);
        self.last_work[stage] = busy;
        self.inline[stage] = false;
        let counters = &shared.counters;
        counters.parallel_blocks.fetch_add(1, Relaxed);
        if parts > 1 {
            counters.pipelined_blocks.fetch_add(1, Relaxed);
        }
        counters.busy.fetch_add(nanos(busy), Relaxed);
        counters.wall.fetch_add(nanos(wall), Relaxed);
        if let Some(payload) = board.failure.take() {
            panic::resume_unwind(payload);
        }
    }
}

impl Default for ParallelRenderer {
    fn default() -> Self {
        Self::new(ParallelOptions::default())
    }
}

impl Renderer for ParallelRenderer {
    fn render(&mut self, graph: &mut PreparedGraph, block: &Block<'_>) {
        // A block rendered ahead leaves the live half its workers, while
        // the live side renders one.
        let capped = self.board == MAIN && block.ahead && self.shared.live_half.load(Relaxed);
        let lanes = self.shared.lanes.len();
        let open = if capped {
            lanes - self.shared.kept
        } else {
            lanes
        };
        let parts = self.parts(graph, block, open);
        let shared = Arc::clone(&self.shared);
        // The workers flush denormals for as long as they run.
        rt::flush_denormals(|| {
            let claim = (self.share(graph, parts))
                .then(|| Claim::take(&shared, Some(self.board), DRIVER.get()))
                .flatten();
            match claim {
                Some(_claim) => self.render_shared(graph, block, open, parts),
                None => self.render_inline(graph, block),
            }
        });
    }

    fn set_live_half(&mut self, has: bool) {
        if self.board == LIVE {
            self.shared.live_half.store(has, Relaxed);
        }
    }

    fn render_ahead(&mut self, work: AheadWork) -> Option<AheadWaker> {
        if self.board != MAIN || self.workers.is_empty() || self.shared.ahead.set(work).is_err() {
            return None;
        }
        Some(AheadWaker(self.shared.clone()))
    }

    /// A renderer on the same threads, on a board of its own, so the live
    /// half renders while the workers render the rest ahead. Whatever
    /// renderers there are, one block at a time shares each board between
    /// the threads, and one thread at a time starts one from lane 0, the
    /// audio thread's: a renderer that would start another renders its
    /// block alone.
    fn live_renderer(&mut self) -> Box<dyn Renderer> {
        Box::new(Self {
            shared: self.shared.clone(),
            workers: Vec::new(),
            board: LIVE,
            inline_below: self.inline_below,
            max_nodes: self.max_nodes,
            pipeline: Pipeline::Never,
            epoch: 0,
            inline: [false; STAGES],
            last_work: [Duration::MAX; STAGES],
            last_graph: 0,
            #[cfg(test)]
            forced_parts: None,
        })
    }
}

impl Drop for ParallelRenderer {
    fn drop(&mut self) {
        // The workers are the renderer's that started them.
        if self.board != MAIN {
            return;
        }
        // The same handshake as a new block: either a worker sees the flag
        // before it sleeps, or we see it asleep and wake it.
        self.shared.shutdown.store(true, SeqCst);
        for lane in &self.shared.lanes[1..] {
            lane.wake();
        }
        for worker in self.workers.drain(..) {
            // A worker that panicked outside a node has nothing left to say.
            let _ = worker.join();
        }
    }
}

impl std::fmt::Debug for ParallelRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelRenderer")
            .field("threads", &self.threads())
            .field("board", &self.board)
            .field("inline_below", &self.inline_below)
            .field("max_nodes", &self.max_nodes)
            .finish_non_exhaustive()
    }
}

/// Reads a [`ParallelRenderer`]'s counters from any thread.
#[derive(Clone)]
pub struct ParallelMonitor(Arc<Shared>);

impl ParallelMonitor {
    pub fn stats(&self) -> ParallelStats {
        let c = &self.0.counters;
        let time = |counter: &AtomicU64| Duration::from_nanos(counter.load(Relaxed));
        ParallelStats {
            threads: self.0.lanes.len() - 1,
            parallel_blocks: c.parallel_blocks.load(Relaxed),
            pipelined_blocks: c.pipelined_blocks.load(Relaxed),
            inline_blocks: c.inline_blocks.load(Relaxed),
            busy: time(&c.busy),
            wall: time(&c.wall),
            awake: time(&c.awake),
        }
    }
}

impl std::fmt::Debug for ParallelMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ParallelMonitor")
            .field(&self.stats())
            .finish()
    }
}

/// What a [`ParallelRenderer`] has done since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParallelStats {
    /// Worker threads, not counting the audio thread.
    pub threads: usize,
    /// Blocks shared between threads.
    pub parallel_blocks: u64,
    /// Of those, the ones pipelined in parts ([`Pipeline`]), counting each
    /// stage of a graph with notes made while rendering.
    pub pipelined_blocks: u64,
    /// Blocks the audio thread rendered alone.
    pub inline_blocks: u64,
    /// Time spent running nodes in shared blocks, summed over threads.
    pub busy: Duration,
    /// How long shared blocks took on the audio thread.
    pub wall: Duration,
    /// Time the workers spent awake, whether running nodes, looking for work
    /// or waiting for the next block: the CPU time they cost.
    pub awake: Duration,
}

impl ParallelStats {
    /// How many threads were running nodes, on average, during shared
    /// blocks.
    pub fn cores_used(&self) -> f64 {
        self.busy.as_secs_f64() / self.wall.as_secs_f64().max(f64::MIN_POSITIVE)
    }
}

/// The board for what the engine's ahead side renders, ahead of the device
/// or as it asks.
const MAIN: usize = 0;
/// The board for a graph's live half.
const LIVE: usize = 1;

/// How many workers a block rendered ahead leaves for the live half, while
/// there's one: the last ones, which never render ahead.
const KEPT: usize = 2;

/// What the audio thread and the workers share.
struct Shared {
    /// Lane 0 is the audio thread's.
    lanes: Box<[Lane]>,
    /// The main board and the live board.
    boards: [Board; 2],
    /// How many of the last lanes a block rendered ahead leaves out while
    /// the graph has a live half: [`KEPT`], or fewer, so a worker renders
    /// ahead.
    kept: usize,
    shutdown: AtomicBool,
    spin: Duration,
    counters: Counters,
    /// The engine's ahead side, for the workers to render when it asks.
    ahead: OnceLock<AheadWork>,
    /// Set when the engine asks a worker to render ahead, until one takes
    /// it on.
    ahead_wanted: AtomicBool,
    /// Workers rendering the engine's ahead side now.
    ahead_inside: AtomicUsize,
    /// Set once the engine stops, after which no worker renders ahead.
    ahead_closed: AtomicBool,
    /// Set while the live side renders a live half, which a block rendered
    /// ahead leaves its workers.
    live_half: AtomicBool,
    /// Set while a thread works as lane 0: starting a block from it, or
    /// helping as it with one a worker renders ahead.
    lane_zero: AtomicBool,
}

/// One block at a time, as the lanes working on it see it.
#[derive(Default)]
struct Board {
    /// Shared blocks started. Workers wait for it to change.
    epoch: CachePadded<AtomicU64>,
    /// How many of the current block's sources are left to take, with the
    /// block's epoch in the high 32 bits: lanes only take sources of the
    /// block they are working on.
    sources: CachePadded<AtomicU64>,
    /// Nodes of the current block not yet finished and counted.
    remaining: CachePadded<AtomicUsize>,
    /// The current block. Only dereferenced by a thread that has taken one of
    /// its nodes, as the block can't end before that node is counted.
    job: AtomicPtr<Job>,
    /// How many lanes the current block can keep busy. Sleeping workers
    /// beyond that aren't woken, unless a node has tasks for them.
    wanted: AtomicUsize,
    /// The lanes below this one may work on the current block: all but the
    /// ones a block rendered ahead leaves the live half.
    open: AtomicUsize,
    /// Set when a node asks to run tasks, so the next block is shared even
    /// when nothing in it can run alongside anything else.
    tasks_asked: AtomicBool,
    /// Nanoseconds spent running nodes in the current block, summed over
    /// lanes.
    block_busy: AtomicU64,
    /// Set while a renderer shares a block on the board.
    driven: AtomicBool,
    failure: Failure,
}

/// What wakes a waiting worker.
enum Wake {
    /// A block to work on: its board and epoch.
    Block(usize, u64),
    /// The engine's ahead side to render.
    Ahead,
    Shutdown,
}

/// One thread's place in the pool.
struct Lane {
    /// Nodes this lane made ready and hasn't run yet, on each board.
    deques: [Deque; 2],
    /// Set while the worker sleeps, or is about to.
    sleeping: CachePadded<AtomicBool>,
    /// The worker, to wake it. Set when it starts; lane 0 never has one.
    thread: OnceLock<Thread>,
    /// The tasks of the node this lane is running, for other lanes to help
    /// with.
    tasks: Tasks,
}

impl Lane {
    /// Wakes the worker if it sleeps, and says whether it did.
    fn wake(&self) -> bool {
        if self.sleeping.swap(false, SeqCst)
            && let Some(thread) = self.thread.get()
        {
            rt::allowed(|| thread.unpark());
            return true;
        }
        false
    }
}

/// One block's work, as the threads that run it see it. It lives on the audio
/// thread's stack while the block runs.
struct Job {
    nodes: *mut Box<dyn Node>,
    inputs: *const Vec<NodeId>,
    ports: *const Vec<InputPort>,
    /// A pipelined block moves each node's events to the frames of the
    /// part they're in as the node gets them.
    events: *mut NodeEvent,
    event_ranges: *const (u32, u32),
    schedule: *const Schedule,
    /// The graph's stage the block renders.
    stage: usize,
    block: *const Block<'static>,
    /// How many parts the block is pipelined in: 1 for none. Each piece of
    /// work is a node then, and otherwise a part of one, as [`piece`]
    /// numbers them.
    parts: usize,
    sample_rate: u32,
    /// Each node's load.
    loads: *const Load,
}

#[derive(Default)]
struct Counters {
    parallel_blocks: AtomicU64,
    pipelined_blocks: AtomicU64,
    inline_blocks: AtomicU64,
    busy: AtomicU64,
    wall: AtomicU64,
    awake: AtomicU64,
}

impl Shared {
    /// A worker thread's life.
    fn worker(&self, lane: usize, realtime: bool) {
        let _priority = realtime.then(priority::raise);
        let _ = self.lanes[lane].thread.set(thread::current());
        let mut seen = [0; 2];
        let mut awake = Awake::new(&self.counters.awake);
        loop {
            match self.wait(lane, seen, &mut awake) {
                // The block may be over: the last it started rendering
                // ahead, or one it slept through. Then there's nothing in
                // it for this lane, or for the lanes it would wake.
                Wake::Block(board, epoch) => {
                    seen[board] = epoch;
                    let b = &self.boards[board];
                    if b.remaining.load(Acquire) > 0 && lane < b.open.load(Relaxed) {
                        self.wake_children(board, lane);
                        self.work(board, lane, epoch);
                    }
                }
                Wake::Ahead => self.render_ahead(lane),
                Wake::Shutdown => break,
            }
            awake.count();
        }
        awake.count();
    }

    /// Renders the engine's ahead side, unless the engine has stopped.
    fn render_ahead(&self, lane: usize) {
        // Pairs with `AheadWaker::close`: either this sees the engine
        // closed, or the engine sees this inside and waits.
        self.ahead_inside.fetch_add(1, SeqCst);
        if !self.ahead_closed.load(SeqCst)
            && let Some(work) = self.ahead.get()
        {
            let _driving = Driving::from(lane);
            work.run();
        }
        self.ahead_inside.fetch_sub(1, Release);
    }

    /// Whether `lane` may render the engine's ahead side: all but the last
    /// lanes, which a block rendered ahead may leave the live half.
    fn drives(&self, lane: usize) -> bool {
        lane < self.lanes.len() - self.kept
    }

    /// Takes the engine's request to render ahead, if it made one and
    /// `lane` may take it on.
    fn ahead_asked(&self, lane: usize) -> bool {
        self.drives(lane)
            && self.ahead_wanted.load(Relaxed)
            && self.ahead_wanted.swap(false, AcqRel)
    }

    /// The board on which a block other than the one in `seen` has
    /// started, live first, and its epoch.
    fn started(&self, seen: [u64; 2], order: Ordering) -> Option<(usize, u64)> {
        [LIVE, MAIN].into_iter().find_map(|board| {
            let epoch = self.boards[board].epoch.load(order);
            (epoch != seen[board]).then_some((board, epoch))
        })
    }

    /// Waits for a block other than those in `seen`, by board, to start,
    /// or for the engine to ask for its ahead side to be rendered: spins
    /// for a while, then sleeps.
    fn wait(&self, lane: usize, seen: [u64; 2], awake: &mut Awake<'_>) -> Wake {
        let started = Instant::now();
        let mut spins = 0u32;
        while !self.spin.is_zero() {
            if self.shutdown.load(Acquire) {
                return Wake::Shutdown;
            }
            if let Some((board, epoch)) = self.started(seen, Acquire) {
                return Wake::Block(board, epoch);
            }
            if self.ahead_asked(lane) {
                return Wake::Ahead;
            }
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(64) {
                if started.elapsed() >= self.spin {
                    break;
                }
                // Let other threads on this core run.
                thread::yield_now();
            }
            spin_loop();
        }
        let drives = self.drives(lane);
        let lane = &self.lanes[lane];
        loop {
            lane.sleeping.store(true, SeqCst);
            // Pairs with the epoch store in `render_shared` and the request
            // in `AheadWaker::wake`: either we see the new epoch or the
            // request, or the thread making it sees us asleep and wakes us.
            let block = self.started(seen, SeqCst);
            let wake = if self.shutdown.load(SeqCst) {
                Some(Wake::Shutdown)
            } else if let Some((board, epoch)) = block {
                Some(Wake::Block(board, epoch))
            } else if drives && self.ahead_wanted.swap(false, SeqCst) {
                Some(Wake::Ahead)
            } else {
                None
            };
            if let Some(wake) = wake {
                lane.sleeping.store(false, Relaxed);
                return wake;
            }
            awake.count();
            thread::park();
            awake.restart();
        }
    }

    /// Wakes the two lanes after `lane` in `board`'s wake-up tree, if the
    /// block can use them and they sleep. The main board's tree starts from
    /// lane 0 and goes up. The live board's goes down from the last lane,
    /// so it wakes the lanes a block rendered ahead leaves it first.
    fn wake_children(&self, board: usize, lane: usize) {
        let wanted = self.boards[board].wanted.load(Relaxed);
        let lanes = self.lanes.len();
        // Where a lane is in the tree, and the lane in a place: the same
        // either way.
        let turn = |n: usize| match (board, n) {
            (LIVE, 1..) => lanes - n,
            _ => n,
        };
        let place = turn(lane);
        for child in [2 * place + 1, 2 * place + 2] {
            if child < wanted {
                self.lanes[turn(child)].wake();
            }
        }
    }

    /// Runs nodes of `board`'s block `epoch` until all of them have run, or
    /// until the next block starts, and helps with the tasks of other
    /// lanes on the board when there's no node to run. On the main board,
    /// it helps with the live board's block first, which the device waits
    /// for. A worker only starts on a block through [`Shared::wait`], so it
    /// never misses waking the lanes below it.
    fn work(&self, board: usize, lane: usize, epoch: u64) {
        let _lane = WorkingFor::enter(self, lane, board);
        let b = &self.boards[board];
        let mut backoff = Backoff::default();
        loop {
            // A block that's started since, rendered ahead, may leave this
            // lane out: then it takes none of its nodes or tasks.
            let open = lane < b.open.load(Relaxed);
            let stolen = || open.then(|| self.steal(board, lane)).flatten();
            match self.take_source(board, epoch).or_else(stolen) {
                Some(first) => {
                    backoff = Backoff::default();
                    self.session(board, lane, first);
                }
                None if b.remaining.load(Acquire) == 0 => return,
                None if b.epoch.load(Acquire) != epoch => return,
                None if board == MAIN && self.help_live(lane) => backoff = Backoff::default(),
                None if open && self.help(board, lane) => backoff = Backoff::default(),
                None => backoff.snooze(),
            }
        }
    }

    /// Works on the live board's block, if one is running, for `lane`,
    /// which holds no node on the main board: runs a node of it, or a
    /// task. Says whether it did.
    fn help_live(&self, lane: usize) -> bool {
        let live = &self.boards[LIVE];
        // The audio thread starts the live board's blocks, and is never on
        // both boards.
        if lane == 0 || live.remaining.load(Acquire) == 0 {
            return false;
        }
        // A node and what it goes on to at a time, so the lane is back on
        // the main board as soon as that has work again.
        let epoch = live.epoch.load(Acquire);
        let _lane = WorkingFor::enter(self, lane, LIVE);
        match self
            .take_source(LIVE, epoch)
            .or_else(|| self.steal(LIVE, lane))
        {
            Some(first) => {
                self.session(LIVE, lane, first);
                true
            }
            None => self.help(LIVE, lane),
        }
    }

    /// Runs `task` for each of `0..tasks`, for the node `lane` is running
    /// on `board`, with whichever lanes on the board can help, and returns
    /// once every task is done. Every task runs, even after one panics; the
    /// first panic here comes out once they're done, and one on another
    /// lane at the end of the block.
    fn run_tasks(&self, board: usize, lane: usize, tasks: u32, task: &(dyn Fn(u32) + Sync)) {
        let slot = &self.lanes[lane].tasks;
        // Only this lane gives out the slot's jobs.
        let job = ((slot.left.load(Relaxed) >> 32) as u32).wrapping_add(1);
        // SAFETY: no lane reads the slot's task between jobs, as there's
        // none left to take, and this function waits for every task of this
        // job to finish, so the task outlives every use.
        unsafe { *slot.task.get() = Some(erase(task)) };
        slot.done.store(0, Relaxed);
        slot.board.store(board, Relaxed);
        // Publishes the task: a lane reads it only after taking a task.
        slot.left
            .store(u64::from(job) << 32 | u64::from(tasks), Release);
        self.wake_helpers(board, tasks.saturating_sub(1));
        let mut failure = None;
        while let Some(index) = slot.take(job) {
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| task(index))) {
                failure.get_or_insert(payload);
            }
            slot.done.fetch_add(1, Release);
        }
        // Every task is taken. Wait for the ones other lanes took. They add
        // their time to the node's load, so the node's own leaves the wait
        // out.
        if slot.done.load(Acquire) < tasks {
            let waiting = load::now();
            let mut backoff = Backoff::default();
            while slot.done.load(Acquire) < tasks {
                backoff.snooze();
            }
            let waited = slot.waited.load(Relaxed);
            let waited = waited.wrapping_add(load::now().wrapping_sub(waiting));
            slot.waited.store(waited, Relaxed);
        }
        if let Some(payload) = failure {
            panic::resume_unwind(payload);
        }
    }

    /// Wakes up to two sleeping workers that may work on `board`'s block,
    /// if `left` tasks are still to take. Each helper wakes up to two more
    /// as it takes a task, so the wake-up calls spread out over the
    /// threads, as they do when a block starts. Workers already awake help
    /// when they have no node to run.
    fn wake_helpers(&self, board: usize, left: u32) {
        let mut wanted = left.min(2);
        let open = self.boards[board].open.load(Relaxed);
        for lane in self.lanes.iter().take(open).skip(1) {
            if wanted == 0 {
                return;
            }
            if lane.sleeping.load(Relaxed) && lane.wake() {
                wanted -= 1;
            }
        }
    }

    /// Runs a task of a node another lane is running on `board`, if
    /// there's one to take. Says whether it ran one.
    fn help(&self, board: usize, lane: usize) -> bool {
        let lanes = self.lanes.len();
        for offset in 1..lanes {
            let slot = &self.lanes[(lane + offset) % lanes].tasks;
            let word = slot.left.load(Acquire);
            // The board is the job's, which `left` published.
            if word as u32 == 0 || slot.board.load(Relaxed) != board {
                continue;
            }
            let Some(index) = slot.take((word >> 32) as u32) else {
                continue;
            };
            self.wake_helpers(board, index);
            let started = Instant::now();
            // SAFETY: taking a task keeps its job from ending until the task
            // is counted done, so the task is set, and alive, and so is the
            // load of the node whose task it is, which its lane set before
            // giving the job out.
            let (task, load) = unsafe {
                let task = &*(*slot.task.get()).expect("a job has its task");
                (task, &*slot.load.load(Relaxed))
            };
            let ticks = load::now();
            let ran = {
                let _tasks = WorkingFor::none();
                panic::catch_unwind(AssertUnwindSafe(|| rt::scope(|| task(index))))
            };
            load.add_task(load::now().wrapping_sub(ticks));
            let b = &self.boards[board];
            if let Err(payload) = ran {
                b.failure.record(payload);
            }
            b.block_busy.fetch_add(nanos(started.elapsed()), Relaxed);
            slot.done.fetch_add(1, Release);
            return true;
        }
        false
    }

    /// Takes one of the sources of `board`'s block `epoch`: its first part,
    /// in a pipelined block.
    fn take_source(&self, board: usize, epoch: u64) -> Option<u32> {
        let b = &self.boards[board];
        let mut word = b.sources.load(Acquire);
        loop {
            let left = word as u32;
            if left == 0 || word & !u64::from(u32::MAX) != tag(epoch) {
                return None;
            }
            match (b.sources).compare_exchange_weak(word, word - 1, AcqRel, Acquire) {
                Ok(_) => {
                    // SAFETY: we took a source of the running block, which
                    // can't end before we count it, so its job is valid.
                    let job = unsafe { &*b.job.load(Acquire) };
                    let schedule = unsafe { &*job.schedule };
                    let source = schedule.sources(job.stage)[left as usize - 1];
                    return Some(match job.parts {
                        1 => source,
                        _ => piece(source, 0),
                    });
                }
                Err(actual) => word = actual,
            }
        }
    }

    /// Steals a node from another lane's deque on `board`.
    fn steal(&self, board: usize, thief: usize) -> Option<u32> {
        let lanes = self.lanes.len();
        loop {
            let mut contended = false;
            for offset in 1..lanes {
                match self.lanes[(thief + offset) % lanes].deques[board].steal() {
                    Steal::Item(node) => return Some(node),
                    Steal::Retry => contended = true,
                    Steal::Empty => {}
                }
            }
            if !contended {
                return None;
            }
        }
    }

    /// Runs `first`, which this lane has taken on `board`, and then whatever
    /// the lane can keep going with. Then counts the nodes it ran as
    /// finished.
    fn session(&self, board: usize, lane: usize, first: u32) {
        let b = &self.boards[board];
        let started = Instant::now();
        let mut finished = 0;
        // Workers check their own real-time safety here. On the audio thread
        // the engine's check covers this one.
        let checked = panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: this lane took `first` in the board's running block.
            rt::scope(|| unsafe { self.run(board, lane, first, &mut finished) })
        }));
        if let Err(payload) = checked {
            b.failure.record(payload);
        }
        b.block_busy.fetch_add(nanos(started.elapsed()), Relaxed);
        // Only now that this lane is done with the graph may the block end.
        b.remaining.fetch_sub(finished, AcqRel);
    }

    /// Runs `first`, then each node this lane makes ready that it doesn't
    /// push, then whatever is on its own deque on `board`. Counts each in
    /// `finished`.
    ///
    /// # Safety
    ///
    /// This lane has taken `first` in `board`'s running block.
    unsafe fn run(&self, board: usize, lane: usize, first: u32, finished: &mut usize) {
        let b = &self.boards[board];
        // SAFETY: the block can't end while this lane holds nodes it hasn't
        // counted, so the job and everything it points to stay valid.
        let job = unsafe { &*b.job.load(Acquire) };
        if job.parts > 1 {
            // SAFETY: the caller's promise.
            return unsafe { self.run_parts(board, job, lane, first, finished) };
        }
        let schedule = unsafe { &*job.schedule };
        let (frames, ahead) = unsafe { ((*job.block).frames, (*job.block).ahead) };
        let Lane { deques, tasks, .. } = &self.lanes[lane];
        let deque = &deques[board];
        let mut next = Some(first);
        // Each node's time runs from the end of the one before, so the clock
        // is read once a node.
        let mut last = load::now();
        while let Some(node) = next.or_else(|| deque.pop()) {
            // SAFETY: the job has a load for each of its nodes.
            let load = unsafe { &*job.loads.add(node as usize) };
            // The time of any tasks it gives out goes there too.
            tasks
                .load
                .store(std::ptr::from_ref(load).cast_mut(), Relaxed);
            // SAFETY: this lane took `node`, from the counter, a deque or by
            // making it ready, so no other thread runs it this block.
            unsafe { self.run_node(b, job, node) };
            let now = load::now();
            load.add(now.wrapping_sub(last), tasks.take_waited(), frames, ahead);
            last = now;
            next = finish(schedule, node, deque);
            *finished += 1;
        }
    }

    /// [`run`](Self::run) for a pipelined block: runs `first`, a part of a
    /// node, then each part this lane makes ready that it doesn't push,
    /// then whatever is on its own deque. Counts each in `finished`.
    ///
    /// # Safety
    ///
    /// This lane has taken `first` in `board`'s running block, which `job`
    /// describes.
    unsafe fn run_parts(
        &self,
        board: usize,
        job: &Job,
        lane: usize,
        first: u32,
        finished: &mut usize,
    ) {
        let b = &self.boards[board];
        // SAFETY: as in `run`.
        let schedule = unsafe { &*job.schedule };
        let ahead = unsafe { (*job.block).ahead };
        let Lane { deques, tasks, .. } = &self.lanes[lane];
        let deque = &deques[board];
        let mut next = Some(first);
        let mut last = load::now();
        while let Some(piece) = next.or_else(|| deque.pop()) {
            let (node, part) = (piece >> PART_BITS, piece as usize % PARTS);
            // SAFETY: the job has a load for each of its nodes.
            let load = unsafe { &*job.loads.add(node as usize) };
            tasks
                .load
                .store(std::ptr::from_ref(load).cast_mut(), Relaxed);
            // SAFETY: this lane took the part, from the counter, a deque or
            // by making it ready, so no other thread runs any part of the
            // node meanwhile.
            let frames = unsafe { self.run_part(b, job, node, part) };
            let now = load::now();
            load.add(now.wrapping_sub(last), tasks.take_waited(), frames, ahead);
            last = now;
            next = finish_part(schedule, node, part, job.parts, deque);
            *finished += 1;
        }
    }

    /// Processes part `part` of `node`, as a block of its own, and returns
    /// how many frames it was. A panic is caught and recorded on `board`,
    /// as in [`run_node`](Self::run_node).
    ///
    /// # Safety
    ///
    /// This lane has taken the part in the pipelined block `job`
    /// describes, which is running: the same part of each of the node's
    /// inputs has run, and its own part before.
    unsafe fn run_part(&self, board: &Board, job: &Job, node: u32, part: usize) -> usize {
        let n = node as usize;
        // SAFETY: the job's pointers are valid while its block runs. This
        // lane is the only one using node `n`, its events and its output's
        // frames for the part; the inputs' parts it reads have run, and
        // nothing writes to those frames of their buffers until the next
        // block. Each part's frames are the block's, within the capacity.
        unsafe {
            let block = &*job.block;
            let schedule = &*job.schedule;
            let frames = part_frames(block.frames, job.parts, part);
            // Its events for the part, from where those for its part before
            // ended, which are in order of offset, moved to the part's
            // frames. The last part has any after the block too.
            let (start, end) = *job.event_ranges.add(n);
            let cursor = &schedule.cursors[n];
            let from = if part == 0 {
                start
            } else {
                cursor.load(Relaxed)
            };
            let mut to = from;
            while to < end {
                let event = &mut *job.events.add(to as usize);
                if event.offset >= frames.end && frames.end < block.frames {
                    break;
                }
                event.offset -= frames.start;
                to += 1;
            }
            cursor.store(to, Relaxed);
            let events =
                std::slice::from_raw_parts(job.events.add(from as usize), (to - from) as usize);
            let part_block = block.part(frames.clone());
            let ctx = part_block.context(job.sample_rate, events);
            let inputs = Inputs::shared(
                &schedule.buffers,
                &*job.inputs.add(n),
                &*job.ports.add(n),
                block.frames,
            )
            .range(frames.clone());
            let mut output = schedule.buffers[n].part_mut(frames.clone());
            let node = &mut **job.nodes.add(n);
            let ran = panic::catch_unwind(AssertUnwindSafe(|| {
                node.process(&ctx, inputs, &mut output);
            }));
            if let Err(payload) = ran {
                board.failure.record(payload);
            }
            frames.len()
        }
    }

    /// Processes `node`. A panic is caught and recorded on `board`, so the
    /// block still ends; the thread that started it raises it again then.
    ///
    /// # Safety
    ///
    /// This lane has taken `node` in the block `job` describes, which is
    /// running, and all of the node's inputs have run.
    unsafe fn run_node(&self, board: &Board, job: &Job, node: u32) {
        let n = node as usize;
        // SAFETY: the job's pointers are valid while its block runs. This
        // lane is the only one using node `n` and its output buffer; its
        // inputs have run, and nothing writes to their buffers until the
        // next block.
        unsafe {
            let block = &*job.block;
            let schedule = &*job.schedule;
            let (start, end) = *job.event_ranges.add(n);
            let events =
                std::slice::from_raw_parts(job.events.add(start as usize), (end - start) as usize);
            let ctx = block.context(job.sample_rate, events);
            let inputs = Inputs::shared(
                &schedule.buffers,
                &*job.inputs.add(n),
                &*job.ports.add(n),
                block.frames,
            );
            let mut output = schedule.buffers[n].view_mut(block.frames);
            let node = &mut **job.nodes.add(n);
            let ran = panic::catch_unwind(AssertUnwindSafe(|| {
                node.process(&ctx, inputs, &mut output);
            }));
            if let Err(payload) = ran {
                board.failure.record(payload);
            }
        }
    }
}

/// A node's tasks, which any lane can take while the node waits for them.
/// Each call to [`tasks::run`](crate::tasks::run) is a job.
#[derive(Default)]
struct Tasks {
    /// The job's number in the high half, which only the slot's own lane
    /// changes, and how many of its tasks are left to take in the low half.
    /// Tasks are taken from the last down.
    left: CachePadded<AtomicU64>,
    /// The job's tasks done.
    done: AtomicU32,
    /// What runs each task. Set before `left` gives out a new job, and read
    /// only by a lane that has taken one of its tasks.
    task: UnsafeCell<Option<*const (dyn Fn(u32) + Sync)>>,
    /// The load of the node the slot's lane is running, which the time of
    /// each of its tasks goes into. Set before the node runs, and read only
    /// by a lane that has taken one of its tasks.
    load: AtomicPtr<Load>,
    /// Ticks the slot's own lane has spent waiting for other lanes to finish
    /// the tasks of the node it runs, since the node started. Only that lane
    /// uses it.
    waited: AtomicU64,
    /// The board of the node the slot's lane is running, whose lanes help
    /// with its tasks. Set before `left` gives out a new job.
    board: AtomicUsize,
}

// SAFETY: `task` is written by the slot's own lane before it publishes a
// job through `left`, and read by others only after they take one of the
// job's tasks, which the job can't end without; it runs on any thread at
// once, as its `Sync` bound allows.
unsafe impl Sync for Tasks {}
// SAFETY: as above; the pointer is only a borrowed task.
unsafe impl Send for Tasks {}

impl Tasks {
    /// Takes one of job `job`'s tasks, if any are left: its index, which is
    /// also how many are left after it.
    fn take(&self, job: u32) -> Option<u32> {
        let mut word = self.left.load(Acquire);
        loop {
            let left = word as u32;
            if left == 0 || (word >> 32) as u32 != job {
                return None;
            }
            match self
                .left
                .compare_exchange_weak(word, word - 1, AcqRel, Acquire)
            {
                Ok(_) => return Some(left - 1),
                Err(actual) => word = actual,
            }
        }
    }

    /// Takes the ticks the slot's lane has waited ([`Tasks::waited`]). Only
    /// that lane calls this.
    fn take_waited(&self) -> u64 {
        let waited = self.waited.load(Relaxed);
        if waited != 0 {
            self.waited.store(0, Relaxed);
        }
        waited
    }
}

/// `task`, with its lifetime erased, for a lane's [`Tasks`].
fn erase<'a>(task: &'a (dyn Fn(u32) + Sync + 'a)) -> *const (dyn Fn(u32) + Sync) {
    // SAFETY: the same pointer, which `run_tasks` uses only while `task`
    // lives.
    unsafe {
        std::mem::transmute::<&'a (dyn Fn(u32) + Sync + 'a), &'static (dyn Fn(u32) + Sync)>(task)
    }
}

thread_local! {
    /// The renderer, lane and board this thread is working for while it
    /// renders a block: where the tasks of the nodes it runs go.
    static LANE: Cell<(*const Shared, usize, usize)> =
        const { Cell::new((std::ptr::null(), 0, MAIN)) };
    /// The lane this thread starts blocks from: 0 on the audio thread, and
    /// a worker's own while it renders the engine's ahead side.
    static DRIVER: Cell<usize> = const { Cell::new(0) };
}

/// Has this thread start blocks from a lane until dropped.
struct Driving(usize);

impl Driving {
    fn from(lane: usize) -> Self {
        Self(DRIVER.replace(lane))
    }
}

impl Drop for Driving {
    fn drop(&mut self) {
        DRIVER.set(self.0);
    }
}

/// Wakes a [`ParallelRenderer`]'s workers to render the engine's ahead
/// side between device callbacks: see [`Renderer::render_ahead`]. The
/// engine keeps it.
pub struct AheadWaker(Arc<Shared>);

impl AheadWaker {
    /// Asks a worker to render ahead: one that's awake takes it on when it
    /// next looks for work, or else this wakes one. Only the workers a
    /// block rendered ahead may use take it on. Real-time safe.
    pub(crate) fn wake(&self) {
        let shared = &*self.0;
        shared.ahead_wanted.store(true, SeqCst);
        // Pairs with the check in `Shared::wait`: a worker we see awake
        // sees the request before it sleeps. Of the workers that may take
        // it on, there's at least one, as there's a waker only with workers.
        let workers = &shared.lanes[1..shared.lanes.len() - shared.kept];
        if workers.iter().all(|lane| lane.sleeping.load(SeqCst)) {
            workers[0].wake();
        }
    }

    /// Works on the block a worker is rendering ahead, if there is one, for
    /// the audio thread while it waits for that worker to finish: as lane
    /// 0, which is idle while a worker starts the blocks. Says whether
    /// there was a block to work on. Real-time safe.
    pub(crate) fn help(&self) -> bool {
        let shared = &*self.0;
        let main = &shared.boards[MAIN];
        if main.remaining.load(Acquire) == 0 {
            return false;
        }
        let Some(_claim) = Claim::take(shared, None, 0) else {
            return false;
        };
        // A block that ends meanwhile gives nothing more to take: its
        // sources carry its epoch, and steals see only the running one's
        // nodes.
        shared.work(MAIN, 0, main.epoch.load(Acquire));
        true
    }

    /// Stops the workers rendering ahead, and waits for any doing so to
    /// finish: for the engine, as it stops, off the audio thread.
    pub(crate) fn close(&self) {
        let shared = &*self.0;
        shared.ahead_closed.store(true, SeqCst);
        while shared.ahead_inside.load(SeqCst) > 0 {
            thread::yield_now();
        }
    }
}

impl std::fmt::Debug for AheadWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AheadWaker").finish_non_exhaustive()
    }
}

/// A board a renderer shares a block on, and lane 0 if a thread works
/// from it, which one thread at a time has, until dropped.
struct Claim<'a> {
    shared: &'a Shared,
    board: Option<usize>,
    lane_zero: bool,
}

impl<'a> Claim<'a> {
    /// Claims `board`, if any, and lane 0 if `lane` is it, unless another
    /// thread has either.
    fn take(shared: &'a Shared, board: Option<usize>, lane: usize) -> Option<Self> {
        let claim = |flag: &AtomicBool| !flag.swap(true, Acquire);
        let driven = board.map(|board| &shared.boards[board].driven);
        if driven.is_some_and(|driven| !claim(driven)) {
            return None;
        }
        let lane_zero = lane == 0;
        if lane_zero && !claim(&shared.lane_zero) {
            if let Some(driven) = driven {
                driven.store(false, Release);
            }
            return None;
        }
        Some(Self {
            shared,
            board,
            lane_zero,
        })
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if let Some(board) = self.board {
            self.shared.boards[board].driven.store(false, Release);
        }
        if self.lane_zero {
            self.shared.lane_zero.store(false, Release);
        }
    }
}

/// The lane of a block the audio thread renders alone.
const INLINE: usize = usize::MAX;

/// How many of a graph's stages the renderer judges apart, whether to share
/// each between threads: a graph without notes made while rendering has
/// one, and the stages beyond these share the last's judgement.
const STAGES: usize = 4;

/// Marks this thread as working for a lane of a renderer on a board, or for
/// none, until dropped.
struct WorkingFor((*const Shared, usize, usize));

impl WorkingFor {
    fn enter(shared: &Shared, lane: usize, board: usize) -> Self {
        Self(LANE.replace((shared, lane, board)))
    }

    /// For none, while this thread runs a task: tasks it asks for run here.
    fn none() -> Self {
        Self(LANE.replace((std::ptr::null(), 0, MAIN)))
    }
}

impl Drop for WorkingFor {
    fn drop(&mut self) {
        LANE.set(self.0);
    }
}

/// Runs `task` for each of `0..tasks` with the lanes of the renderer whose
/// shared block this thread is working on, and says whether it did: false,
/// having run nothing, in any other block or task.
pub(crate) fn run_tasks(tasks: u32, task: &(dyn Fn(u32) + Sync)) -> bool {
    let (shared, lane, board) = LANE.get();
    if shared.is_null() {
        return false;
    }
    // SAFETY: set only while this thread renders a block for the renderer,
    // which keeps `Shared` alive meanwhile.
    let shared = unsafe { &*shared };
    shared.boards[board].tasks_asked.store(true, Relaxed);
    if lane == INLINE {
        return false;
    }
    let _tasks = WorkingFor::none();
    shared.run_tasks(board, lane, tasks, task);
    true
}

/// Counts `node` as finished for each node it feeds. Of the nodes that makes
/// ready, returns the first, which has the longest way to go, for this lane
/// to run next, and pushes the others onto `deque`.
fn finish(schedule: &Schedule, node: u32, deque: &Deque) -> Option<u32> {
    let mut next = None;
    for &fed in schedule.dependents(node) {
        if schedule.input_done(fed) {
            match next {
                None => next = Some(fed),
                Some(_) => deque.push(fed),
            }
        }
    }
    next
}

/// Node `node`'s part `part`, as a piece of work of a pipelined block.
fn piece(node: u32, part: usize) -> u32 {
    node << PART_BITS | part as u32
}

/// The frames of part `part` of a block of `frames` pipelined in `parts`:
/// each about as long as the others, and none empty, as a block has at
/// least as many frames as parts.
fn part_frames(frames: usize, parts: usize, part: usize) -> Range<usize> {
    part * frames / parts..(part + 1) * frames / parts
}

/// Counts part `part` of `node` as finished for what waits for it, in a
/// block pipelined in `parts`: the same part of each node it feeds, and
/// its own next part. Of the parts that makes ready, returns the node's
/// next, whose state is warm on this lane, or else the first, of the node
/// with the longest way to go, for this lane to run next, and pushes the
/// others onto `deque`.
fn finish_part(
    schedule: &Schedule,
    node: u32,
    part: usize,
    parts: usize,
    deque: &Deque,
) -> Option<u32> {
    let own = part + 1 < parts && schedule.part_done(node, part + 1);
    let mut next = own.then(|| piece(node, part + 1));
    for &fed in schedule.dependents(node) {
        if schedule.part_done(fed, part) {
            match next {
                None => next = Some(piece(fed, part)),
                Some(_) => deque.push(piece(fed, part)),
            }
        }
    }
    next
}

/// The first panic in a block, kept for the thread that started it to raise
/// again, or the first on a worker rendering the engine's ahead side, kept
/// for the audio thread.
#[derive(Default)]
pub(crate) struct Failure {
    state: AtomicU8,
    payload: UnsafeCell<Option<Box<dyn Any + Send>>>,
}

const EMPTY: u8 = 0;
const WRITING: u8 = 1;
const FULL: u8 = 2;

// SAFETY: `payload` is written by the one thread that moves `state` from
// EMPTY to WRITING, and read by the one thread that takes it only once it
// sees the state FULL, which the writer stores after the write.
unsafe impl Sync for Failure {}

impl Failure {
    pub(crate) fn record(&self, payload: Box<dyn Any + Send>) {
        if self
            .state
            .compare_exchange(EMPTY, WRITING, Acquire, Relaxed)
            .is_ok()
        {
            // SAFETY: winning the exchange gives this thread the slot.
            unsafe { *self.payload.get() = Some(payload) };
            self.state.store(FULL, Release);
        }
    }

    /// Whether it holds a panic.
    #[cfg(test)]
    pub(crate) fn is_set(&self) -> bool {
        self.state.load(Acquire) == FULL
    }

    /// Takes the first panic, if there was one. Only one thread calls this:
    /// the one that started the block, after the block, or the audio thread.
    pub(crate) fn take(&self) -> Option<Box<dyn Any + Send>> {
        if self.state.load(Acquire) != FULL {
            return None;
        }
        // SAFETY: the writer is done, and nothing writes again until this
        // resets the state.
        let payload = unsafe { (*self.payload.get()).take() };
        self.state.store(EMPTY, Release);
        payload
    }
}

/// Adds up the time a worker spends awake.
struct Awake<'a> {
    total: &'a AtomicU64,
    since: Instant,
}

impl<'a> Awake<'a> {
    fn new(total: &'a AtomicU64) -> Self {
        Self {
            total,
            since: Instant::now(),
        }
    }

    /// Adds the time since the last count.
    fn count(&mut self) {
        let now = Instant::now();
        self.total.fetch_add(nanos(now - self.since), Relaxed);
        self.since = now;
    }

    /// Starts counting again after sleeping.
    fn restart(&mut self) {
        self.since = Instant::now();
    }
}

/// Spins with growing pauses, then yields to other threads.
#[derive(Default)]
struct Backoff(u32);

impl Backoff {
    fn snooze(&mut self) {
        if self.0 <= 6 {
            for _ in 0..1 << self.0 {
                spin_loop();
            }
            self.0 += 1;
        } else {
            rt::allowed(thread::yield_now);
        }
    }
}

/// Keeps a value on its own cache lines, so threads writing neighbouring
/// values don't slow each other down. 128 bytes covers the line pairs that
/// x86 prefetches together and Apple's larger lines.
#[derive(Debug, Default)]
#[repr(align(128))]
pub(crate) struct CachePadded<T>(pub T);

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Block `epoch`'s tag in the high half of [`Shared::sources`].
fn tag(epoch: u64) -> u64 {
    epoch << 32
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap_or(u64::MAX)
}

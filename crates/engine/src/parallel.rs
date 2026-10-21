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

mod deque;
mod priority;
mod schedule;
#[cfg(test)]
mod tests;

use std::any::Any;
use std::cell::UnsafeCell;
use std::hint::spin_loop;
use std::ops::Deref;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

pub(crate) use schedule::Schedule;

use self::deque::{Deque, Steal};
use crate::{
    Block, Inputs, Node, NodeEvent, NodeId, PreparedGraph, ReferenceRenderer, Renderer, rt,
};

/// How a [`ParallelRenderer`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelOptions {
    /// Worker threads, besides the audio thread, which works too. The default
    /// is one per physical core, minus one for the audio thread.
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
}

impl Default for ParallelOptions {
    fn default() -> Self {
        Self {
            threads: ParallelRenderer::default_threads(),
            spin: Duration::from_micros(20),
            inline_below: Some(Duration::from_micros(100)),
            max_nodes: 1 << 14,
            realtime: true,
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
    workers: Vec<JoinHandle<()>>,
    inline_below: Option<Duration>,
    max_nodes: usize,
    /// Parallel blocks rendered.
    epoch: u64,
    /// Whether the last block ran on the audio thread alone.
    inline: bool,
    /// The work the last block took, summed over threads.
    last_work: Duration,
    /// The address of the graph the last block ran, to notice a new one.
    last_graph: usize,
}

impl ParallelRenderer {
    /// One worker per physical core, minus one for the audio thread.
    pub fn default_threads() -> usize {
        num_cpus::get_physical().saturating_sub(1)
    }

    /// Starts the worker threads.
    ///
    /// # Panics
    ///
    /// If the operating system can't start a thread.
    pub fn new(options: ParallelOptions) -> Self {
        let lanes = (0..=options.threads)
            .map(|_| Lane {
                deque: Deque::new(options.max_nodes),
                sleeping: CachePadded(AtomicBool::new(false)),
                thread: OnceLock::new(),
            })
            .collect();
        let shared = Arc::new(Shared {
            lanes,
            epoch: CachePadded(AtomicU64::new(0)),
            sources: CachePadded(AtomicU64::new(0)),
            remaining: CachePadded(AtomicUsize::new(0)),
            job: AtomicPtr::new(std::ptr::null_mut()),
            wanted: AtomicUsize::new(0),
            failure: Failure::default(),
            shutdown: AtomicBool::new(false),
            spin: options.spin,
            counters: Counters::default(),
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
            inline_below: options.inline_below,
            max_nodes: options.max_nodes,
            epoch: 0,
            inline: false,
            last_work: Duration::MAX,
            last_graph: 0,
        }
    }

    /// Worker threads, not counting the audio thread.
    pub fn threads(&self) -> usize {
        self.workers.len()
    }

    /// A handle for reading the renderer's counters from another thread, such
    /// as the control thread, after the engine has taken the renderer.
    pub fn monitor(&self) -> ParallelMonitor {
        ParallelMonitor(self.shared.clone())
    }

    /// Whether to share this block between threads.
    fn share(&mut self, graph: &PreparedGraph) -> bool {
        if self.workers.is_empty() || graph.len() > self.max_nodes {
            return false;
        }
        let address = graph as *const PreparedGraph as usize;
        if address != self.last_graph {
            // A new graph: assume it's worth sharing until it shows otherwise.
            self.last_graph = address;
            self.last_work = Duration::MAX;
        }
        let Some(below) = self.inline_below else {
            return true;
        };
        // Nothing can run alongside anything else, or there's too little to
        // do. Once inline, stay there until there's clearly enough work, so a
        // graph near the line doesn't flip every block.
        let below = if self.inline { below * 2 } else { below };
        graph.schedule.width > 1 && self.last_work >= below
    }

    fn render_inline(&mut self, graph: &mut PreparedGraph, block: &Block<'_>) {
        let started = Instant::now();
        ReferenceRenderer.render(graph, block);
        self.last_work = started.elapsed();
        self.inline = true;
        self.shared.counters.inline_blocks.fetch_add(1, Relaxed);
    }

    fn render_shared(&mut self, graph: &mut PreparedGraph, block: &Block<'_>) {
        let shared = &*self.shared;
        graph.schedule.point_at(&mut graph.buffers);
        let job = Job {
            nodes: graph.nodes.as_mut_ptr(),
            inputs: graph.inputs.as_ptr(),
            events: graph.events.as_ptr(),
            event_ranges: graph.event_ranges.as_ptr(),
            schedule: &graph.schedule,
            block: (block as *const Block<'_>).cast(),
            sample_rate: graph.config.sample_rate,
        };
        let started = Instant::now();
        self.epoch += 1;
        // Nothing reads these until it takes a node of the new block, which
        // the store to `sources` makes possible.
        shared.job.store((&job as *const Job).cast_mut(), Relaxed);
        shared.remaining.store(graph.len(), Relaxed);
        shared
            .wanted
            .store(graph.schedule.width.min(shared.lanes.len()), Relaxed);
        let sources = graph.schedule.sources.len() as u64;
        shared.sources.store(tag(self.epoch) | sources, Release);
        // Wake the workers. SeqCst pairs with the check sleeping workers make
        // before they park: either they see the new epoch, or we see them
        // asleep.
        shared.epoch.store(self.epoch, SeqCst);
        shared.wake_children(0);
        shared.work(0, self.epoch);
        // Every node has run, and every thread is done with the graph and the
        // job.
        let wall = started.elapsed();
        let busy = Duration::from_nanos(shared.counters.block_busy.swap(0, Relaxed));
        shared.job.store(std::ptr::null_mut(), Relaxed);
        self.last_work = busy;
        self.inline = false;
        let counters = &shared.counters;
        counters.parallel_blocks.fetch_add(1, Relaxed);
        counters.busy.fetch_add(nanos(busy), Relaxed);
        counters.wall.fetch_add(nanos(wall), Relaxed);
        if let Some(payload) = shared.failure.take() {
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
        // The workers flush denormals for as long as they run.
        rt::flush_denormals(|| {
            if self.share(graph) {
                self.render_shared(graph, block);
            } else {
                self.render_inline(graph, block);
            }
        });
    }
}

impl Drop for ParallelRenderer {
    fn drop(&mut self) {
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
            .field("threads", &self.workers.len())
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

/// What the audio thread and the workers share.
struct Shared {
    /// Lane 0 is the audio thread's.
    lanes: Box<[Lane]>,
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
    /// beyond that aren't woken.
    wanted: AtomicUsize,
    failure: Failure,
    shutdown: AtomicBool,
    spin: Duration,
    counters: Counters,
}

/// One thread's place in the pool.
struct Lane {
    /// Nodes this lane made ready and hasn't run yet.
    deque: Deque,
    /// Set while the worker sleeps, or is about to.
    sleeping: CachePadded<AtomicBool>,
    /// The worker, to wake it. Set when it starts; lane 0 never has one.
    thread: OnceLock<Thread>,
}

impl Lane {
    fn wake(&self) {
        if self.sleeping.swap(false, SeqCst)
            && let Some(thread) = self.thread.get()
        {
            thread.unpark();
        }
    }
}

/// One block's work, as the threads that run it see it. It lives on the audio
/// thread's stack while the block runs.
struct Job {
    nodes: *mut Box<dyn Node>,
    inputs: *const Vec<NodeId>,
    events: *const NodeEvent,
    event_ranges: *const (u32, u32),
    schedule: *const Schedule,
    block: *const Block<'static>,
    sample_rate: u32,
}

#[derive(Default)]
struct Counters {
    parallel_blocks: AtomicU64,
    inline_blocks: AtomicU64,
    /// Nanoseconds spent running nodes in the current block, summed over
    /// lanes.
    block_busy: AtomicU64,
    busy: AtomicU64,
    wall: AtomicU64,
    awake: AtomicU64,
}

impl Shared {
    /// A worker thread's life.
    fn worker(&self, lane: usize, realtime: bool) {
        let _priority = realtime.then(priority::raise);
        let _ = self.lanes[lane].thread.set(thread::current());
        let mut seen = 0;
        let mut awake = Awake::new(&self.counters.awake);
        while let Some(epoch) = self.wait(lane, seen, &mut awake) {
            seen = epoch;
            self.wake_children(lane);
            self.work(lane, epoch);
            awake.count();
        }
        awake.count();
    }

    /// Waits for a block other than `seen` to start, and returns its epoch:
    /// spins for a while, then sleeps. `None` when the renderer shuts down.
    fn wait(&self, lane: usize, seen: u64, awake: &mut Awake<'_>) -> Option<u64> {
        let started = Instant::now();
        let mut spins = 0u32;
        while !self.spin.is_zero() {
            if self.shutdown.load(Acquire) {
                return None;
            }
            let epoch = self.epoch.load(Acquire);
            if epoch != seen {
                return Some(epoch);
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
        let lane = &self.lanes[lane];
        loop {
            lane.sleeping.store(true, SeqCst);
            // Pairs with the epoch store in `render_shared`: either we see the
            // new epoch, or the audio thread sees us asleep and wakes us.
            let epoch = self.epoch.load(SeqCst);
            let shutdown = self.shutdown.load(SeqCst);
            if epoch != seen || shutdown {
                lane.sleeping.store(false, Relaxed);
                return (!shutdown).then_some(epoch);
            }
            awake.count();
            thread::park();
            awake.restart();
        }
    }

    /// Wakes the two lanes after `lane` in the wake-up tree, if the block can
    /// use them and they sleep.
    fn wake_children(&self, lane: usize) {
        let wanted = self.wanted.load(Relaxed);
        for child in [2 * lane + 1, 2 * lane + 2] {
            if child < wanted {
                self.lanes[child].wake();
            }
        }
    }

    /// Runs nodes of block `epoch` until all of them have run, or until the
    /// next block starts. A worker only starts on a block through
    /// [`Shared::wait`], so it never misses waking the lanes below it.
    fn work(&self, lane: usize, epoch: u64) {
        let mut backoff = Backoff::default();
        loop {
            match self.take_source(epoch).or_else(|| self.steal(lane)) {
                Some(first) => {
                    backoff = Backoff::default();
                    self.session(lane, first);
                }
                None if self.remaining.load(Acquire) == 0 => return,
                None if self.epoch.load(Acquire) != epoch => return,
                None => backoff.snooze(),
            }
        }
    }

    /// Takes one of block `epoch`'s sources.
    fn take_source(&self, epoch: u64) -> Option<u32> {
        let mut word = self.sources.load(Acquire);
        loop {
            let left = word as u32;
            if left == 0 || word & !u64::from(u32::MAX) != tag(epoch) {
                return None;
            }
            match self
                .sources
                .compare_exchange_weak(word, word - 1, AcqRel, Acquire)
            {
                Ok(_) => {
                    // SAFETY: we took a source of the running block, which
                    // can't end before we count it, so its job is valid.
                    let schedule = unsafe { &*(*self.job.load(Acquire)).schedule };
                    return Some(schedule.sources[left as usize - 1]);
                }
                Err(actual) => word = actual,
            }
        }
    }

    /// Steals a node from another lane's deque.
    fn steal(&self, thief: usize) -> Option<u32> {
        let lanes = self.lanes.len();
        loop {
            let mut contended = false;
            for offset in 1..lanes {
                match self.lanes[(thief + offset) % lanes].deque.steal() {
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

    /// Runs `first`, which this lane has taken, and then whatever the lane can
    /// keep going with. Then counts the nodes it ran as finished.
    fn session(&self, lane: usize, first: u32) {
        let started = Instant::now();
        let mut finished = 0;
        // Workers check their own real-time safety here. On the audio thread
        // the engine's check covers this one.
        let checked = panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: this lane took `first` in the running block.
            rt::scope(|| unsafe { self.run(lane, first, &mut finished) })
        }));
        if let Err(payload) = checked {
            self.failure.record(payload);
        }
        self.counters
            .block_busy
            .fetch_add(nanos(started.elapsed()), Relaxed);
        // Only now that this lane is done with the graph may the block end.
        self.remaining.fetch_sub(finished, AcqRel);
    }

    /// Runs `first`, then each node this lane makes ready that it doesn't
    /// push, then whatever is on its own deque. Counts each in `finished`.
    ///
    /// # Safety
    ///
    /// This lane has taken `first` in the running block.
    unsafe fn run(&self, lane: usize, first: u32, finished: &mut usize) {
        // SAFETY: the block can't end while this lane holds nodes it hasn't
        // counted, so the job and everything it points to stay valid.
        let job = unsafe { &*self.job.load(Acquire) };
        let schedule = unsafe { &*job.schedule };
        let deque = &self.lanes[lane].deque;
        let mut next = Some(first);
        while let Some(node) = next.or_else(|| deque.pop()) {
            // SAFETY: this lane took `node`, from the counter, a deque or by
            // making it ready, so no other thread runs it this block.
            unsafe { self.run_node(job, node) };
            next = finish(schedule, node, deque);
            *finished += 1;
        }
    }

    /// Processes `node`. A panic is caught and recorded, so the block still
    /// ends; the audio thread raises it again then.
    ///
    /// # Safety
    ///
    /// This lane has taken `node` in the block `job` describes, which is
    /// running, and all of the node's inputs have run.
    unsafe fn run_node(&self, job: &Job, node: u32) {
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
            let inputs = Inputs::shared(&schedule.buffers, &*job.inputs.add(n), block.frames);
            let mut output = schedule.buffers[n].view_mut(block.frames);
            let node = &mut **job.nodes.add(n);
            let ran = panic::catch_unwind(AssertUnwindSafe(|| {
                node.process(&ctx, inputs, &mut output);
            }));
            if let Err(payload) = ran {
                self.failure.record(payload);
            }
        }
    }
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

/// The first panic in a block, kept for the audio thread to raise again.
#[derive(Default)]
struct Failure {
    state: AtomicU8,
    payload: UnsafeCell<Option<Box<dyn Any + Send>>>,
}

const EMPTY: u8 = 0;
const WRITING: u8 = 1;
const FULL: u8 = 2;

// SAFETY: `payload` is written by the one thread that moves `state` from
// EMPTY to WRITING, and read by the audio thread only after the block, which
// can't end before the write is done.
unsafe impl Sync for Failure {}

impl Failure {
    fn record(&self, payload: Box<dyn Any + Send>) {
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

    /// Takes the block's first panic, if there was one. Only the audio thread
    /// calls this, after the block.
    fn take(&self) -> Option<Box<dyn Any + Send>> {
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
            thread::yield_now();
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

//! The scheduler at the pace of a device, printed as Markdown tables:
//!
//! 1. **Per node against per signal path.** A 64-track project scheduled per
//!    node by the parallel renderer, and per signal path: one thread per
//!    path, where tracks connected by routing or sends are one path,
//!    processed in series, as Ableton's documentation describes Live doing
//!    it.
//! 2. **Waiting between blocks.** Block times, and the CPU the workers burn,
//!    for different `ParallelOptions::spin` settings.
//! 3. **Small graphs.** The audio thread alone against sharing, for projects
//!    of a few tracks: where `ParallelOptions::inline_below` belongs.
//!
//! ```sh
//! cargo bench -p musicable-engine --bench realtime
//! cargo bench -p musicable-engine --bench realtime -- small   # one table
//! ```

// A report, not engine code: it prints, and it sleeps between blocks like a
// device does.
#![allow(clippy::disallowed_macros, clippy::disallowed_methods)]

mod common;

use std::time::{Duration, Instant};

use common::{CONFIG, Project, Routing, cores, renderer};
use musicable_engine::{
    AudioRef, Block, ParallelOptions, ParallelRenderer, ParallelStats, PreparedGraph,
    ReferenceRenderer, Renderer, TransportState,
};
use musicable_model::time::TempoMap;

/// Frames per block: the small buffer that makes scheduling hard.
const FRAMES: usize = 64;

/// How long each block took to render, sorted.
struct Times(Vec<Duration>);

impl Times {
    fn percentile(&self, p: f64) -> Duration {
        self.0[((self.0.len() - 1) as f64 * p).round() as usize]
    }

    fn median(&self) -> Duration {
        self.percentile(0.5)
    }

    fn mean(&self) -> Duration {
        self.0.iter().sum::<Duration>() / self.0.len() as u32
    }

    fn max(&self) -> Duration {
        self.percentile(1.0)
    }
}

fn render(
    renderer: &mut dyn Renderer,
    graph: &mut PreparedGraph,
    blocks: usize,
    mut wait: impl FnMut(usize),
) -> Times {
    let tempo = TempoMap::default();
    let mut times = Vec::with_capacity(blocks);
    for i in 0..blocks {
        wait(i);
        let block = Block {
            frames: FRAMES,
            time: (i * FRAMES) as u64,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(FRAMES),
            midi: &[],
        };
        let started = Instant::now();
        renderer.render(graph, &block);
        times.push(started.elapsed());
    }
    times.sort();
    Times(times)
}

/// Renders blocks one after another.
fn back_to_back(renderer: &mut dyn Renderer, graph: &mut PreparedGraph, blocks: usize) -> Times {
    render(renderer, graph, blocks, |_| {})
}

/// Renders each block when a device playing at 48 kHz would ask for it.
fn paced(renderer: &mut dyn Renderer, graph: &mut PreparedGraph, blocks: usize) -> Times {
    let period = Duration::from_secs_f64(FRAMES as f64 / f64::from(CONFIG.sample_rate));
    let start = Instant::now();
    render(renderer, graph, blocks, |i| {
        wait_until(start + period * i as u32);
    })
}

/// Sleeps until shortly before `deadline`, then spins: sleeping alone can
/// overshoot by more than a block.
fn wait_until(deadline: Instant) {
    let early = Duration::from_micros(300);
    let now = Instant::now();
    if deadline > now + early {
        std::thread::sleep(deadline - now - early);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

/// What a renderer did between two readings of its counters.
fn since(before: ParallelStats, after: ParallelStats) -> ParallelStats {
    ParallelStats {
        threads: after.threads,
        parallel_blocks: after.parallel_blocks - before.parallel_blocks,
        inline_blocks: after.inline_blocks - before.inline_blocks,
        busy: after.busy - before.busy,
        wall: after.wall - before.wall,
        awake: after.awake - before.awake,
    }
}

fn micros(duration: Duration) -> String {
    format!("{:.0} µs", duration.as_secs_f64() * 1e6)
}

fn per_node_against_per_path() {
    println!("## 64 tracks: per node against per signal path\n");
    println!(
        "Every track has an instrument, two devices and a channel strip with its slots \
         filled. {} threads, {FRAMES}-frame blocks, back to back unless it says device \
         pace. One thread is the reference renderer on the same graph. Cores used is how \
         many threads were running nodes, on average, while a block rendered. Speed-up is \
         against one thread scheduled per node.\n",
        cores()
    );
    println!(
        "| Routing | Scheduled | Nodes | One thread | Shared | Cores used | Speed-up | \
         p99 at device pace |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for routing in Routing::ALL {
        let project = Project {
            tracks: 64,
            routing,
            slots: true,
        };
        // Against the same work on one thread, scheduled per node.
        let one_thread = back_to_back(&mut ReferenceRenderer, &mut project.per_node(), 2000).mean();
        for (scheduled, mut graph) in [
            ("per node", project.per_node()),
            ("per path", project.per_path()),
        ] {
            let alone = back_to_back(&mut ReferenceRenderer, &mut graph, 2000).mean();
            let mut renderer = renderer(cores());
            let monitor = renderer.monitor();
            back_to_back(&mut renderer, &mut graph, 200);
            let before = monitor.stats();
            let times = back_to_back(&mut renderer, &mut graph, 2000);
            let used = since(before, monitor.stats()).cores_used();
            let paced = paced(&mut renderer, &mut graph, 2000);
            println!(
                "| {} | {scheduled} | {} | {} | {} | {used:.1} | {:.1}× | {} |",
                routing.name(),
                graph.len(),
                micros(alone),
                micros(times.mean()),
                one_thread.as_secs_f64() / times.mean().as_secs_f64(),
                micros(paced.percentile(0.99)),
            );
        }
    }
    println!();
}

fn waiting_between_blocks() {
    println!("## Waiting between blocks\n");
    println!(
        "Projects routed with sends, slots filled, {} threads, a {FRAMES}-frame block every \
         {:.2} ms. Worker CPU is the time the workers spent awake, running nodes, looking \
         for work or spinning, in cores.\n",
        cores(),
        FRAMES as f64 * 1e3 / f64::from(CONFIG.sample_rate)
    );
    println!("| Tracks | Spin | Median | p99 | Max | Worker CPU |");
    println!("|---|---|---|---|---|---|");
    for tracks in [16, 64] {
        let project = Project {
            tracks,
            routing: Routing::Sends,
            slots: true,
        };
        for spin in [0, 20, 50, 200, 1000].map(Duration::from_micros) {
            let mut renderer = ParallelRenderer::new(ParallelOptions {
                threads: cores() - 1,
                spin,
                inline_below: None,
                ..ParallelOptions::default()
            });
            let monitor = renderer.monitor();
            let mut graph = project.per_node();
            paced(&mut renderer, &mut graph, 200);
            let before = monitor.stats();
            let started = Instant::now();
            let times = paced(&mut renderer, &mut graph, 3000);
            let cpu = since(before, monitor.stats()).awake.as_secs_f64()
                / started.elapsed().as_secs_f64();
            println!(
                "| {tracks} | {} | {} | {} | {} | {cpu:.2} |",
                micros(spin),
                micros(times.median()),
                micros(times.percentile(0.99)),
                micros(times.max()),
            );
        }
    }
    println!();
}

fn small_graphs() {
    println!("## Small graphs: the audio thread alone against sharing\n");
    println!(
        "Tracks straight into the master, no slots, {} threads, at device pace. The \
         audio thread alone is the reference renderer. Shared shares every block; \
         default options decide block by block.\n",
        cores()
    );
    println!(
        "| Tracks | Alone, median | Alone, p99 | Shared, median | Shared, p99 | \
         Default options, median | Default options, p99 | Blocks shared |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for tracks in [1, 2, 4, 8, 16, 32] {
        let project = Project {
            tracks,
            routing: Routing::Direct,
            slots: false,
        };
        let alone = paced(&mut ReferenceRenderer, &mut project.per_node(), 2000);
        // Its workers stop before the next renderer's start: Windows gives a
        // process 32 threads in MMCSS.
        let shared = {
            let mut renderer = renderer(cores());
            let mut graph = project.per_node();
            paced(&mut renderer, &mut graph, 200);
            paced(&mut renderer, &mut graph, 2000)
        };
        // Deciding block by block.
        let mut renderer = ParallelRenderer::new(ParallelOptions {
            threads: cores() - 1,
            ..ParallelOptions::default()
        });
        let monitor = renderer.monitor();
        let mut graph = project.per_node();
        paced(&mut renderer, &mut graph, 200);
        let before = monitor.stats();
        let default = paced(&mut renderer, &mut graph, 2000);
        let blocks = since(before, monitor.stats());
        let fraction =
            blocks.parallel_blocks as f64 / (blocks.parallel_blocks + blocks.inline_blocks) as f64;
        println!(
            "| {tracks} | {} | {} | {} | {} | {} | {} | {:.0}% |",
            micros(alone.median()),
            micros(alone.percentile(0.99)),
            micros(shared.median()),
            micros(shared.percentile(0.99)),
            micros(default.median()),
            micros(default.percentile(0.99)),
            fraction * 100.0,
        );
    }
    println!();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Only under `cargo bench`, not when tests build every target.
    if !args.iter().any(|arg| arg == "--bench") {
        return;
    }
    // `cargo bench --bench realtime -- small` runs the tables with "small"
    // in their names.
    let filters: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    let tables: [(&str, fn()); 3] = [
        ("paths", per_node_against_per_path),
        ("waiting", waiting_between_blocks),
        ("small", small_graphs),
    ];
    println!("# The parallel scheduler at device pace\n");
    for (name, table) in tables {
        if filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())) {
            table();
        }
    }
}

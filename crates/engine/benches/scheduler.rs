//! How long the parallel renderer takes per block, for graphs that are easy
//! and hard to share between threads, with 1, 2, 4, ... threads up to one per
//! physical core, against the reference renderer. Blocks run back to back, so
//! the workers never sleep: see the `realtime` benchmark for blocks at the
//! pace of a device.
//!
//! ```sh
//! cargo bench -p musicable-engine --bench scheduler
//! ```

mod common;

use std::time::Duration;

use common::{Project, Routing, cost, deep, lane_counts, renderer, wide};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use musicable_engine::{
    AudioRef, Block, PreparedGraph, ReferenceRenderer, Renderer, TransportState,
};
use musicable_model::time::TempoMap;

/// Builds a graph to measure.
type Build = fn() -> PreparedGraph;

/// The graphs, by name.
fn graphs() -> [(&'static str, Build); 5] {
    [
        // Everything can run at once.
        ("wide", || wide(64, cost::INSTRUMENT)),
        // Nothing can: what scheduling costs.
        ("deep", || deep(64, cost::DEVICE)),
        // A 64-track project in groups, with two return tracks every track
        // sends to.
        ("sends", || {
            let project = Project {
                tracks: 64,
                routing: Routing::Sends,
                slots: false,
            };
            project.per_node()
        }),
        // The same with every channel strip slot filled: more, smaller nodes.
        ("slots", || {
            let project = Project {
                tracks: 64,
                routing: Routing::Sends,
                slots: true,
            };
            project.per_node()
        }),
        // Four tracks: little to share.
        ("small", || {
            let project = Project {
                tracks: 4,
                routing: Routing::Direct,
                slots: false,
            };
            project.per_node()
        }),
    ]
}

fn scheduler(c: &mut Criterion) {
    let tempo = TempoMap::default();
    for frames in [64, 256] {
        let block = Block {
            frames,
            time: 0,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(frames),
        };
        let mut group = c.benchmark_group(format!("{frames} frames"));
        group
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(3));
        for (name, build) in graphs() {
            let mut graph = build();
            group.bench_function(BenchmarkId::new(name, "reference"), |b| {
                b.iter(|| ReferenceRenderer.render(&mut graph, &block));
            });
            for lanes in lane_counts() {
                let mut renderer = renderer(lanes);
                let mut graph = build();
                group.bench_function(BenchmarkId::new(name, format!("{lanes} threads")), |b| {
                    b.iter(|| renderer.render(&mut graph, &block));
                });
            }
        }
        group.finish();
    }
}

criterion_group!(benches, scheduler);
criterion_main!(benches);

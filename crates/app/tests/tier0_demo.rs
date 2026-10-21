//! The Tier 0 demo from the plan (M8), offline: open the saved demo, press
//! Space, and it plays and loops with the metronome, rendered by the
//! parallel scheduler with output that matches the reference renderer
//! sample for sample. Saved and opened again, it plays the same.

use std::fs;
use std::path::{Path, PathBuf};

use musicable_app::null_test::{self, Player, Render};
use musicable_engine::{EngineConfig, ParallelOptions, rt};
use musicable_model::Project;

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

/// The demo, as File > New Demo Project made it and saved it in Tier 0, in
/// version 1 of the project format.
const DEMO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../io/tests/fixtures/v1/demo.musicable"
);

/// The demo as this version of Musicable saves it.
fn saved_demo() -> PathBuf {
    let version = musicable_io::project::VERSION;
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../io/tests/fixtures/v{version}/demo.musicable"))
}

/// A low buffer size, as the demo plays on Windows.
const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 128,
};

/// Half a second past the end of the loop, so it loops back once.
const PAST_THE_LOOP: u64 = 24_000;

fn demo() -> Project {
    musicable_io::project::load(DEMO).unwrap()
}

/// Where the demo's loop ends: four bars at 124 BPM.
fn loop_end(project: &Project) -> u64 {
    let end = project
        .tempo
        .frame_at(project.loop_region.end, CONFIG.sample_rate);
    assert_eq!(end, 371_613);
    end as u64
}

/// The parallel scheduler with three workers, sharing every block, however
/// many cores the machine has.
fn shared() -> Render {
    Render::Parallel(ParallelOptions {
        threads: 3,
        inline_below: None,
        realtime: false,
        ..ParallelOptions::default()
    })
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn the_demo_plays_the_same_with_the_parallel_scheduler() {
    let project = demo();
    let frames = loop_end(&project) + PAST_THE_LOOP;
    let renders = [Render::Reference, Render::App, shared()];
    let mut players: Vec<Player> = renders
        .iter()
        .map(|render| Player::new(project.clone(), None, render, CONFIG))
        .collect();
    let differences = null_test::compare(&mut players, frames);
    assert_eq!(differences, [None, None, None]);

    // It played, and looped back to the start of the loop.
    let reference = &players[0];
    assert!(reference.peak() > 0.1, "{}", reference.peak());
    let stats = reference.stats();
    assert!(stats.playing);
    assert_eq!(stats.position, PAST_THE_LOOP);
    assert_eq!(stats.events_late, 0);

    // The scheduler shared the blocks it was told to.
    let shared = players[2].parallel().unwrap();
    assert!(shared.parallel_blocks > 0);
    assert_eq!(shared.inline_blocks, 0);
}

#[test]
fn saved_and_opened_again_the_demo_plays_the_same() {
    let project = demo();
    let path = scratch("saved_and_opened_again").join("demo.musicable");
    musicable_io::project::save(&path, &project).unwrap();
    assert_eq!(fs::read(&path).unwrap(), fs::read(saved_demo()).unwrap());
    let reopened = musicable_io::project::load(&path).unwrap();
    assert_eq!(reopened, project);

    let mut players = [
        Player::new(project, None, &Render::Reference, CONFIG),
        Player::new(reopened, None, &Render::App, CONFIG),
    ];
    let differences = null_test::compare(&mut players, 2 * CONFIG.sample_rate as u64);
    assert_eq!(differences, [None, None]);
}

/// The null test notices a difference of the smallest step a sample can
/// take.
#[test]
fn the_null_test_finds_the_smallest_difference() {
    let project = demo();
    let mut louder = project.clone();
    let gain = &mut louder.tracks[0].gain;
    *gain = f32::from_bits(gain.to_bits() + 1);
    let mut players = [
        Player::new(project, None, &Render::Reference, CONFIG),
        Player::new(louder, None, &shared(), CONFIG),
    ];
    let differences = null_test::compare(&mut players, CONFIG.sample_rate.into());
    let difference = differences[1].expect("a difference");
    assert_ne!(difference.got.to_bits(), difference.expected.to_bits());
}

/// An empty folder for one test, in Cargo's scratch folder for tests.
fn scratch(test: &str) -> PathBuf {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

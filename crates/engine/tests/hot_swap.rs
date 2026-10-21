//! A graph compiled on the compiler thread replaces the running one while
//! another thread renders, without a glitch.

use std::thread;

use musicable_engine::{
    AudioMut, Compiler, Engine, EngineConfig, Event, TransportCommand, compile, rt,
};
use musicable_model::{Project, TestTone, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// Device callback sizes, in turn. Drivers don't always deliver the period
/// that was asked for, so the engine must take whatever comes.
const CALLBACKS: [usize; 5] = [441, 64, 970, 128, 7];

/// Tracks with a test tone each: frequency and whether it's muted.
fn project(tracks: &[(f64, bool)]) -> Project {
    let mut p = Project::new();
    for &(frequency, muted) in tracks {
        let id = p.add_track(format!("{frequency} Hz"), TrackKind::Midi);
        let track = p.track_mut(id).unwrap();
        track.gain = 0.5;
        track.muted = muted;
        track.tone = Some(TestTone {
            frequency,
            level: 0.5,
        });
    }
    p
}

/// Plays `project` from the start, offline, as stereo frames.
fn offline(project: &Project, frames: usize) -> Vec<[f32; 2]> {
    let (mut engine, mut handle) = Engine::new(CONFIG);
    handle
        .swap_graph(compile(project).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut out = vec![0.0; 2 * frames];
    engine.process(&mut AudioMut::from_planar(&mut out, 2));
    let (left, right) = out.split_at(frames);
    left.iter().zip(right).map(|(&l, &r)| [l, r]).collect()
}

struct Live {
    out: Vec<[f32; 2]>,
    /// The frame where the second graph took over.
    swapped_at: usize,
    /// Where each device callback started.
    callbacks: Vec<usize>,
}

/// Plays `before` on an audio thread, then compiles `after` on the compiler
/// thread and swaps it in mid-playback.
fn swap_while_playing(before: &Project, after: &Project) -> Live {
    let (mut engine, mut handle) = Engine::new(CONFIG);
    let mut compiler = Compiler::spawn();
    compiler.compile_project(CONFIG, before);
    handle.swap_graph(compiler.recv().graph.unwrap()).unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();

    let audio = thread::spawn(move || {
        let mut live = Live {
            out: Vec::new(),
            swapped_at: 0,
            callbacks: Vec::new(),
        };
        let mut device = [0.0; 2 * 970];
        for &size in CALLBACKS.iter().cycle() {
            let start = live.out.len();
            let swaps = engine.stats().swaps;
            let block = &mut device[..2 * size];
            engine.process_interleaved(block, 2);
            live.callbacks.push(start);
            live.out.extend(block.chunks(2).map(|f| [f[0], f[1]]));
            if swaps == 1 && engine.stats().swaps == 2 {
                live.swapped_at = start;
            }
            // Play on for a while after the swap.
            if live.swapped_at > 0 && live.out.len() > live.swapped_at + 20_000 {
                return live;
            }
            assert!(live.out.len() < 60 * 48_000, "the new graph never arrived");
        }
        unreachable!()
    });

    // Swap in the middle of playback, not before it starts.
    while handle.stats().time == 0 {
        thread::yield_now();
    }
    compiler.compile_project(CONFIG, after);
    handle.swap_graph(compiler.recv().graph.unwrap()).unwrap();
    let live = audio.join().expect("the audio thread failed");
    assert_eq!(handle.stats().swaps, 2);
    assert_eq!(
        handle.collect_garbage(),
        1,
        "the old graph comes back to be dropped"
    );
    live
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn swapping_in_an_unchanged_mix_is_seamless() {
    let before = project(&[(220.0, false), (330.0, false)]);
    // A new track that's muted leaves the mix as it was.
    let after = project(&[(220.0, false), (330.0, false), (440.0, true)]);
    let live = swap_while_playing(&before, &after);
    assert!(live.swapped_at > 0);
    let expected = offline(&before, live.out.len());
    assert!(live.out == expected, "the swap changed the output");
}

#[test]
fn a_new_track_comes_in_at_a_block_boundary() {
    let before = project(&[(220.0, false), (330.0, false)]);
    let after = project(&[(220.0, false), (330.0, false), (440.0, false)]);
    let live = swap_while_playing(&before, &after);
    let at = live.swapped_at;
    assert!(at > 0 && live.callbacks.contains(&at));
    let old_mix = offline(&before, live.out.len());
    let new_mix = offline(&after, live.out.len());
    // Up to the swap the old graph plays, and from then on the new one, with
    // its tracks exactly where they would have been all along.
    assert!(live.out[..at] == old_mix[..at]);
    assert!(live.out[at..] == new_mix[at..]);
    assert!(old_mix[at..] != new_mix[at..]);
}

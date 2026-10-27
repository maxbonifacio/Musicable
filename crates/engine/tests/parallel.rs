//! The engine sounds the same with the parallel renderer as with the
//! reference renderer, sample for sample, through parameter changes,
//! modulation, transport and tempo changes, and graph swaps.

use musicable_engine::compile::keys;
use musicable_engine::nodes::{Gain, Sine};
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Event, ParallelOptions, ParallelRenderer, ReferenceRenderer,
    Renderer, TransportCommand, compile, rt,
};
use musicable_model::modulation::{ModulatorKind, Output, lfo};
use musicable_model::param::ParamAddress;
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::{Chain, Project, TestTone, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 64,
};

/// Device callback sizes, in turn.
const CALLBACKS: [usize; 5] = [441, 64, 970, 128, 7];

/// `tracks` tracks with a test tone each, the metronome on, and an LFO on
/// the master panning every other track.
fn project(tracks: u32) -> Project {
    let mut p = Project::new();
    p.metronome = true;
    for i in 0..tracks {
        let id = p.add_track(format!("Track {i}"), TrackKind::Midi);
        let track = p.track_mut(id).unwrap();
        track.gain = 0.25;
        track.tone = Some(TestTone {
            frequency: 110.0 * f64::from(i + 1),
            level: 0.5,
        });
    }
    let mut lfo = p.new_modulator(ModulatorKind::Lfo, "LFO 1");
    lfo.set(lfo::RATE as u32, 5.0);
    let from = Output::new(lfo.id, 0);
    p.master.modulation.modulators.push(lfo);
    let panned: Vec<Chain> = p
        .tracks
        .iter()
        .step_by(2)
        .map(|t| Chain::Track(t.id))
        .collect();
    for track in panned {
        let routing = p.new_routing(from, ParamAddress::Pan(track), 0.5);
        p.master.modulation.routings.push(routing);
    }
    p
}

/// Plays a session through `renderer` and returns everything it played: gain
/// and pitch changes on exact frames, a loop, a tempo change, a recompile
/// that adds tracks and mutes one, a locate, and stopping and starting.
fn session(renderer: Box<dyn Renderer>) -> Vec<f32> {
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, renderer);
    let mut project = project(12);
    handle
        .swap_graph(compile(&project).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut played = Vec::new();
    let mut device = [0.0; 2 * 970];
    for (callback, &frames) in CALLBACKS.iter().cycle().take(60).enumerate() {
        let now = handle.stats().time;
        let transport = |command| Event::Transport(command);
        match callback {
            3 => {
                for (i, track) in project.tracks.iter().enumerate() {
                    let gain = Event::Param {
                        node: keys::track_gain(track.id),
                        param: Gain::GAIN,
                        value: 0.05 * i as f64,
                    };
                    handle.send_at(now + 17 * i as u64, gain).unwrap();
                }
            }
            8 => {
                let region = BeatRange::new(Beats::new(1), Beats::new(2));
                let looping = transport(TransportCommand::Loop(Some(region)));
                handle.send(looping).unwrap();
            }
            12 => {
                let tempo = TempoMap::new(97.0, TimeSignature::FOUR_FOUR);
                handle.set_tempo_map(tempo).unwrap();
            }
            20 => {
                for i in 12..20 {
                    let id = project.add_track(format!("Track {i}"), TrackKind::Audio);
                    let track = project.track_mut(id).unwrap();
                    track.tone = Some(TestTone {
                        frequency: 55.0 * f64::from(i),
                        level: 0.25,
                    });
                }
                project.tracks[3].muted = true;
                let graph = compile(&project).prepare(&CONFIG).unwrap();
                handle.swap_graph(graph).unwrap();
            }
            26 => {
                for (i, track) in project.tracks.iter().enumerate().step_by(3) {
                    let pitch = Event::Param {
                        node: keys::track_tone(track.id),
                        param: Sine::FREQUENCY,
                        value: 300.0 + i as f64,
                    };
                    handle.send_at(now + 5 * i as u64, pitch).unwrap();
                }
            }
            30 => {
                let locate = transport(TransportCommand::Locate(Beats::from_ratio(7, 3)));
                handle.send(locate).unwrap();
            }
            35 => handle.send(transport(TransportCommand::Stop)).unwrap(),
            38 => handle.send(transport(TransportCommand::Play)).unwrap(),
            _ => {}
        }
        let device = &mut device[..2 * frames];
        engine.process(&mut AudioMut::from_planar(device, 2));
        played.extend_from_slice(device);
        handle.collect_garbage();
    }
    played
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn the_engine_sounds_the_same_with_the_parallel_renderer() {
    let expected = session(Box::new(ReferenceRenderer));
    assert!(expected.iter().any(|&s| s != 0.0));
    for threads in [1, 3, 8] {
        let renderer = ParallelRenderer::new(ParallelOptions {
            threads,
            inline_below: None,
            realtime: false,
            ..ParallelOptions::default()
        });
        let monitor = renderer.monitor();
        let played = session(Box::new(renderer));
        assert!(same(&played, &expected), "{threads} workers");
        assert!(monitor.stats().parallel_blocks > 0);
    }
    // With the defaults, small blocks may run on the audio thread alone.
    let played = session(Box::new(ParallelRenderer::default()));
    assert!(same(&played, &expected), "default options");
}

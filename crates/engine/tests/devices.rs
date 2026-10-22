//! Musicable's own devices in a project: they process where the chain puts
//! them, a kind without DSP yet passes the signal through, parameter
//! changes land on their frames through the engine, and every session
//! sounds the same with the parallel renderer as with the reference
//! renderer.

use musicable_engine::compile::keys;
use musicable_engine::devices::BUILT;
use musicable_engine::event::ParamId;
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Event, ParallelOptions, ParallelRenderer, ReferenceRenderer,
    Renderer, TransportCommand, compile, rt,
};
use musicable_model::native::{NativeDevice, NativeKind, delay, eq, utility};
use musicable_model::time::{TempoMap, TimeSignature};
use musicable_model::{Device, DeviceKind, Project, TestTone, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 64,
};

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

/// A project with a track playing a test tone, and `devices` made from
/// it on the track.
fn project(devices: impl FnOnce(&mut Project) -> Vec<Device>) -> Project {
    let mut p = Project::new();
    let track = p.add_track("Tone", TrackKind::Audio);
    p.track_mut(track).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.5,
    });
    let devices = devices(&mut p);
    p.track_mut(track).unwrap().devices = devices;
    p
}

fn native(p: &mut Project, kind: NativeKind, values: &[(usize, f64)]) -> Device {
    let mut device = NativeDevice::new(kind);
    for &(param, value) in values {
        device.set(param, value);
    }
    p.new_device(DeviceKind::Native(device))
}

/// Plays `project` from the start through `renderer`, sending each of
/// `events` at its engine time, and returns the left channel.
fn play(project: &Project, renderer: Box<dyn Renderer>, events: &[(u64, Event)]) -> Vec<f32> {
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, renderer);
    handle
        .swap_graph(compile(project).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    for &(at, event) in events {
        handle.send_at(at, event).unwrap();
    }
    let mut out = vec![0.0; 2 * 4800];
    engine.process(&mut AudioMut::from_planar(&mut out, 2));
    out.truncate(4800);
    out
}

fn reference(project: &Project) -> Vec<f32> {
    play(project, Box::new(ReferenceRenderer), &[])
}

/// The built-in effects that leave the signal as it is at their defaults.
/// The others, such as the reverb, are heard as soon as they're added.
const TRANSPARENT: [NativeKind; 3] = [NativeKind::Eq, NativeKind::Gate, NativeKind::Utility];

#[test]
fn devices_without_dsp_yet_pass_the_signal_through() {
    let dry = reference(&project(|_| vec![]));
    for kind in NativeKind::ALL {
        let wet = reference(&project(|p| vec![native(p, kind, &[])]));
        if !BUILT.contains(&kind) || TRANSPARENT.contains(&kind) {
            assert_eq!(wet, dry, "{kind:?}");
        }
    }
}

#[test]
fn devices_process_in_chain_order() {
    let dry = reference(&project(|_| vec![]));
    let half = reference(&project(|p| {
        vec![
            native(p, NativeKind::Utility, &[(utility::GAIN, 0.5)]),
            native(p, NativeKind::Utility, &[(utility::INVERT_LEFT, 1.0)]),
        ]
    }));
    for (wet, dry) in half.iter().zip(&dry) {
        assert_eq!(*wet, -dry * 0.5);
    }
    // A notch on the tone takes it out, once it has settled.
    let notched = reference(&project(|p| {
        let notch = [
            (eq::ON, 1.0),
            (eq::SHAPE, 5.0),
            (eq::FREQ, 440.0),
            (eq::Q, 1.0),
        ];
        vec![native(p, NativeKind::Eq, &notch)]
    }));
    let tail = &notched[2400..];
    assert!(
        tail.iter().all(|s| s.abs() < 1e-3),
        "a notch lets 440 Hz through"
    );
}

#[test]
fn parameter_changes_land_on_their_frames() {
    let mut id = None;
    let p = project(|p| {
        let device = native(p, NativeKind::Utility, &[]);
        id = Some(device.id);
        vec![device]
    });
    let dry = reference(&project(|_| vec![]));
    let mute = Event::Param {
        node: keys::device(id.unwrap()),
        param: ParamId(utility::MUTE as u32),
        value: 1.0,
    };
    let wet = play(&p, Box::new(ReferenceRenderer), &[(1000, mute)]);
    assert_eq!(wet[..1000], dry[..1000]);
    // Faded out over 10 ms from frame 1000.
    assert_ne!(wet[1000], dry[1000]);
    assert!(wet[1480..].iter().all(|&s| s == 0.0));
}

#[test]
fn the_delay_keeps_time_with_the_song() {
    let p = project(|p| {
        let sixteenth = [
            (delay::NOTE, 2.0),
            (delay::FEEDBACK, 0.0),
            (delay::MIX, 100.0),
        ];
        vec![native(p, NativeKind::Delay, &sixteenth)]
    });
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, Box::new(ReferenceRenderer));
    handle
        .set_tempo_map(TempoMap::new(90.0, TimeSignature::FOUR_FOUR))
        .unwrap();
    handle
        .swap_graph(compile(&p).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut out = vec![0.0; 2 * 9000];
    engine.process(&mut AudioMut::from_planar(&mut out, 2));
    // Only the echoes, a sixteenth at 90 bpm behind the tone: 8000 frames.
    let first = out[..9000].iter().position(|&s| s != 0.0).unwrap();
    assert!((8000..8002).contains(&first), "{first}");
}

/// SplitMix64, seeded so a failure happens again on the next run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// From 0.0 to 1.0.
    fn unit(&mut self) -> f64 {
        (self.next() % 1001) as f64 / 1000.0
    }
}

/// A project of tracks with built-in devices set at random, and events
/// that move their parameters at random times.
fn random_session(random: &mut Random) -> (Project, Vec<(u64, Event)>) {
    let mut p = Project::new();
    let mut events = Vec::new();
    for t in 0..4 {
        let track = p.add_track(t.to_string(), TrackKind::Audio);
        p.track_mut(track).unwrap().tone = Some(TestTone {
            frequency: 100.0 + (random.next() % 3000) as f64,
            level: 0.5,
        });
        let mut devices = Vec::new();
        for _ in 0..1 + random.next() % 3 {
            let kind = BUILT[(random.next() % BUILT.len() as u64) as usize];
            let mut device = NativeDevice::new(kind);
            for (i, param) in kind.params().iter().enumerate() {
                if random.next().is_multiple_of(3) {
                    device.set(i, param.from_unit(random.unit()));
                }
            }
            let device = p.new_device(DeviceKind::Native(device));
            for _ in 0..4 {
                let i = (random.next() % kind.params().len() as u64) as usize;
                let value = kind.params()[i].from_unit(random.unit());
                let event = Event::Param {
                    node: keys::device(device.id),
                    param: ParamId(i as u32),
                    value,
                };
                events.push((random.next() % 4800, event));
            }
            devices.push(device);
        }
        p.track_mut(track).unwrap().devices = devices;
    }
    (p, events)
}

#[test]
fn every_session_sounds_the_same_with_the_parallel_renderer() {
    let mut random = Random(11);
    for i in 0..12 {
        let (project, events) = random_session(&mut random);
        let expected = play(&project, Box::new(ReferenceRenderer), &events);
        assert!(expected.iter().any(|&s| s != 0.0));
        for threads in [1, 4] {
            let renderer = ParallelRenderer::new(ParallelOptions {
                threads,
                inline_below: None,
                realtime: false,
                ..ParallelOptions::default()
            });
            let played = play(&project, Box::new(renderer), &events);
            let same = played
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "session {i}, {threads} workers");
        }
    }
}

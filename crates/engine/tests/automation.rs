//! Automation: parameters follow their lanes at the song position, so they
//! loop and locate as clips do, glide over steps and jumps, and play the
//! same with the parallel renderer as with the reference renderer, and in
//! blocks of any size. Nodes that read no automation of their own get it
//! as parameter events, which win over the values set.

use musicable_engine::compile::keys;
use musicable_engine::nodes::{Constant, Gain};
use musicable_engine::{
    AudioMut, Automation, Engine, EngineConfig, Event, Graph, NodeKey, ParallelOptions,
    ParallelRenderer, ReferenceRenderer, Renderer, TransportCommand, compile, rt,
};
use musicable_model::automation::{Lane, Point};
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::native::{NativeDevice, NativeKind, compressor, eq, gate, utility};
use musicable_model::param::{ParamAddress, Scale};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::{Chain, DeviceKind, Project, TestTone, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

/// A slow rate, so a session covers many beats: 4,000 frames to the beat at
/// 120 BPM, and 80 to a glide.
const RATE: u32 = 8_000;

const BEAT: usize = 4_000;

fn config(max_block: usize) -> EngineConfig {
    EngineConfig {
        sample_rate: RATE,
        max_block,
    }
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

fn points(points: &[(Beats, f64)]) -> Vec<Point> {
    points
        .iter()
        .map(|&(at, value)| Point::new(at, value))
        .collect()
}

fn transport(command: TransportCommand) -> Event {
    Event::Transport(command)
}

/// Plays `engine` for `frames` frames, in callbacks of `callback`, and
/// returns the left channel.
fn left(engine: &mut Engine, frames: usize, callback: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut device = vec![0.0; 2 * callback];
    while out.len() < frames {
        let size = callback.min(frames - out.len());
        let device = &mut device[..2 * size];
        engine.process(&mut AudioMut::from_planar(device, 2));
        out.extend_from_slice(&device[..size]);
    }
    out
}

#[track_caller]
fn assert_near(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

/// A constant 1.0 through a fader following `automation`, which shows the
/// fader's gain as it plays.
fn fader(automation: Automation) -> Graph {
    let mut graph = Graph::new();
    let one = graph.add(NodeKey(0), Constant::new(1.0));
    let fader = graph.add(NodeKey(1), Gain::new(0.5));
    graph.connect(one, fader).unwrap();
    graph.set_output(fader).unwrap();
    graph.automate(fader, Gain::GAIN, automation).unwrap();
    graph
}

/// Up from silence over a beat, a step down to a quarter, and holding
/// there.
fn rise_and_step() -> Automation {
    let lane = points(&[
        (beats(0, 1), 0.0),
        (beats(1, 1), 1.0),
        (beats(1, 1), 0.25),
        (beats(2, 1), 0.25),
    ]);
    Automation::new(&lane, Scale::Linear).unwrap()
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn a_fader_follows_its_lane_and_glides_over_steps() {
    let config = config(64);
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(fader(rise_and_step()).prepare(&config).unwrap())
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    let out = left(&mut engine, 3 * BEAT, 100);
    assert_eq!(out[0], 0.0);
    assert_near(out[BEAT / 2], 0.5);
    assert_near(out[BEAT - 1], 1.0 - 1.0 / BEAT as f32);
    // The step glides down from where the rise got to over 10 ms, 80
    // frames.
    let top = out[BEAT - 1];
    let glide = |frame: usize| 0.25 + (top - 0.25) * (1.0 - frame as f32 / 80.0).max(0.0);
    for frame in [0, 1, 40, 78, 79, 80] {
        assert_near(out[BEAT + frame], glide(frame + 1));
    }
    assert!(out[BEAT + 79..].iter().all(|&gain| gain == 0.25));
}

#[test]
fn automation_loops_and_locates_as_clips_do() {
    let config = config(64);
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(fader(rise_and_step()).prepare(&config).unwrap())
        .unwrap();
    let rise = BeatRange::new(beats(0, 1), beats(1, 1));
    handle
        .send(transport(TransportCommand::Loop(Some(rise))))
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    let out = left(&mut engine, 2 * BEAT, 333);
    // Back round the loop, it glides down from the top of the rise to the
    // bottom, and then plays the rise again as it did.
    assert_near(out[BEAT], out[BEAT - 1] * (1.0 - 80.0_f32.recip()));
    assert_eq!(out[BEAT + 80..], out[80..BEAT]);
    // A locate glides to the value where the playhead lands.
    let landing = beats(3, 2);
    handle
        .send(transport(TransportCommand::Locate(landing)))
        .unwrap();
    handle
        .send(transport(TransportCommand::Loop(None)))
        .unwrap();
    let out = left(&mut engine, BEAT, 333);
    assert!(out[0] > 0.25 && out[0] < 1.0, "{}", out[0]);
    assert!(out[80..].iter().all(|&gain| gain == 0.25));
    // Stopped, it holds the value at the playhead.
    handle.send(transport(TransportCommand::Stop)).unwrap();
    handle
        .send(transport(TransportCommand::Locate(beats(1, 2))))
        .unwrap();
    let out = left(&mut engine, 200, 200);
    assert_near(out[199], 0.5);
}

/// Bass and chords on tones and a test instrument, sending to a reverb,
/// with lanes on volumes, pans, a send, a test device's gain and the
/// instrument's level, which it gets as events, and an overridden lane.
/// Then lanes on built-in devices, which read their own: an EQ on the bass
/// whose shelf rises and moves, whose 24 dB cut (two sections) comes in and
/// goes out and whose bell turns to a notch, a utility on the master
/// whose gain, balance and DC filter move, a compressor on the chords
/// whose threshold, knee, attack, release and mix move and whose detector
/// switches to RMS, and a gate on the return that closes and opens again
/// as its threshold steps, while its hold, release and floor move.
fn project() -> Project {
    let mut p = Project::new();
    let bass = p.add_track("Bass", TrackKind::Audio);
    let chords = p.add_track("Chords", TrackKind::Midi);
    let keys = p.add_track("Keys", TrackKind::Midi);
    let reverb = p.add_track("Reverb", TrackKind::Return);
    for (track, frequency) in [(bass, 55.0), (chords, 220.0), (reverb, 440.0)] {
        p.track_mut(track).unwrap().tone = Some(TestTone {
            frequency,
            level: 0.25,
        });
    }
    let send = p.new_send(reverb);
    let send_id = send.id;
    p.track_mut(bass).unwrap().sends.push(send);
    let gain = p.new_device(DeviceKind::TestGain { gain: 0.5 });
    let gain_id = gain.id;
    p.track_mut(chords).unwrap().devices.push(gain);
    let mut notes = MidiClip::default();
    notes.add_note(beats(0, 1), beats(6, 1), 60, 1.0);
    notes.add_note(beats(2, 1), beats(3, 1), 67, 0.5);
    let clip = p.new_clip(
        "Keys",
        BeatRange::new(beats(0, 1), beats(8, 1)),
        ClipContent::Midi(notes),
    );
    let instrument = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let instrument_id = instrument.id;
    let track = p.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    let tone = p.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
    let tone_id = tone.id;
    p.track_mut(bass).unwrap().devices.push(tone);
    let utility = p.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Utility)));
    let utility_id = utility.id;
    p.master.devices.push(utility);
    let compressor = p.new_device(DeviceKind::Native(NativeDevice::new(
        NativeKind::Compressor,
    )));
    let compressor_id = compressor.id;
    p.track_mut(chords).unwrap().devices.push(compressor);
    let gate = p.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Gate)));
    let gate_id = gate.id;
    p.track_mut(reverb).unwrap().devices.push(gate);

    let lane = |param, at: &[(Beats, f64)]| Lane::new(param, points(at));
    let mut overridden = lane(ParamAddress::Pan(Chain::Master), &[(beats(0, 1), 1.0)]);
    overridden.overridden = true;
    p.automation = vec![
        lane(
            ParamAddress::Volume(Chain::Track(bass)),
            &[
                (beats(0, 1), 0.0),
                (beats(2, 1), 1.0),
                (beats(2, 1), 0.5),
                (beats(4, 1), 0.0),
            ],
        ),
        lane(
            ParamAddress::Pan(Chain::Track(chords)),
            &[(beats(0, 1), -1.0), (beats(3, 1), 1.0)],
        ),
        lane(
            ParamAddress::Send(send_id),
            &[(beats(1, 2), 0.0), (beats(5, 2), 1.0)],
        ),
        lane(
            ParamAddress::Volume(Chain::Master),
            &[(beats(1, 1), 1.0), (beats(7, 3), 0.75), (beats(7, 3), 0.5)],
        ),
        lane(
            ParamAddress::Device(gain_id, 0),
            &[(beats(0, 1), 0.25), (beats(5, 1), 1.5)],
        ),
        lane(
            ParamAddress::Device(instrument_id, 0),
            &[(beats(0, 1), 0.0), (beats(2, 1), 1.0), (beats(9, 4), 0.2)],
        ),
        overridden,
    ];
    let band = |band: usize, param: usize, at: &[(Beats, f64)]| {
        let param = band * eq::PER_BAND + param;
        lane(ParamAddress::Device(tone_id, param as u32), at)
    };
    let utility = |param: usize, at: &[(Beats, f64)]| {
        lane(ParamAddress::Device(utility_id, param as u32), at)
    };
    let (two, five) = (beats(2, 1), beats(5, 1));
    p.automation.extend([
        band(1, eq::GAIN, &[(beats(0, 1), -12.0), (beats(4, 1), 12.0)]),
        band(1, eq::FREQ, &[(beats(0, 1), 50.0), (beats(6, 1), 400.0)]),
        band(
            0,
            eq::ON,
            &[(two, 0.0), (two, 1.0), (five, 1.0), (five, 0.0)],
        ),
        band(0, eq::FREQ, &[(beats(0, 1), 40.0), (beats(4, 1), 120.0)]),
        band(2, eq::SHAPE, &[(beats(3, 1), 4.0), (beats(3, 1), 5.0)]),
        utility(utility::GAIN, &[(beats(0, 1), 1.0), (beats(4, 1), 0.5)]),
        utility(utility::PAN, &[(beats(2, 1), 0.0), (beats(6, 1), -0.5)]),
        utility(utility::DC, &[(beats(1, 1), 0.0), (beats(1, 1), 1.0)]),
    ]);
    let compressor = |param: usize, at: &[(Beats, f64)]| {
        lane(ParamAddress::Device(compressor_id, param as u32), at)
    };
    let gate =
        |param: usize, at: &[(Beats, f64)]| lane(ParamAddress::Device(gate_id, param as u32), at);
    p.automation.extend([
        compressor(
            compressor::THRESHOLD,
            &[(beats(0, 1), -30.0), (beats(4, 1), -6.0)],
        ),
        compressor(compressor::DETECTOR, &[(two, 0.0), (two, 1.0)]),
        compressor(
            compressor::ATTACK,
            &[(beats(0, 1), 0.1), (beats(3, 1), 50.0)],
        ),
        compressor(
            compressor::RELEASE,
            &[(beats(1, 1), 5.0), (beats(5, 1), 500.0)],
        ),
        compressor(compressor::KNEE, &[(beats(0, 1), 0.0), (beats(3, 1), 24.0)]),
        compressor(
            compressor::MIX,
            &[(beats(0, 1), 100.0), (beats(6, 1), 20.0)],
        ),
        gate(
            gate::THRESHOLD,
            &[
                (beats(1, 1), -40.0),
                (beats(1, 1), -6.0),
                (beats(3, 1), -6.0),
                (beats(3, 1), -40.0),
            ],
        ),
        gate(gate::HOLD, &[(beats(0, 1), 0.0), (beats(4, 1), 50.0)]),
        gate(gate::RELEASE, &[(beats(0, 1), 1.0), (beats(5, 1), 200.0)]),
        gate(gate::FLOOR, &[(beats(0, 1), -80.0), (beats(5, 1), -20.0)]),
    ]);
    p
}

/// Plays `project` through `renderer` in blocks of up to `max_block`, in
/// device callbacks of uneven sizes, with a loop, a tempo change, a swap
/// to edited lanes, a locate, a stop and a start, and a swap to a lane
/// overridden, and returns what it played.
fn session(renderer: Box<dyn Renderer>, max_block: usize) -> Vec<f32> {
    let config = config(max_block);
    let mut project = project();
    let (mut engine, mut handle) = Engine::with_renderer(config, renderer);
    handle
        .swap_graph(compile(&project).prepare(&config).unwrap())
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    let mut played = Vec::new();
    let mut device = [0.0; 2 * 970];
    let sizes = [441, 64, 970, 128, 7, 300];
    for (callback, &frames) in sizes.iter().cycle().take(150).enumerate() {
        match callback {
            5 => {
                let region = BeatRange::new(beats(1, 1), beats(3, 1));
                handle
                    .send(transport(TransportCommand::Loop(Some(region))))
                    .unwrap();
            }
            25 => {
                let tempo = TempoMap::new(97.0, TimeSignature::FOUR_FOUR);
                handle.set_tempo_map(tempo).unwrap();
            }
            40 => {
                project.automation[0].points[1].value = 0.75;
                project.automation[1].points.pop();
                let graph = compile(&project).prepare(&config).unwrap();
                handle.swap_graph(graph).unwrap();
            }
            55 => handle
                .send(transport(TransportCommand::Locate(beats(1, 3))))
                .unwrap(),
            70 => handle.send(transport(TransportCommand::Stop)).unwrap(),
            75 => handle
                .send(transport(TransportCommand::Locate(beats(5, 2))))
                .unwrap(),
            80 => handle.send(transport(TransportCommand::Play)).unwrap(),
            100 => {
                project.automation[3].overridden = true;
                project.master.gain = 0.25;
                let graph = compile(&project).prepare(&config).unwrap();
                handle.swap_graph(graph).unwrap();
            }
            120 => handle
                .send(transport(TransportCommand::Loop(None)))
                .unwrap(),
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
fn automation_plays_the_same_with_either_renderer_and_any_blocks() {
    let expected = session(Box::new(ReferenceRenderer), 64);
    assert!(expected.iter().any(|&s| s.abs() > 0.05), "a quiet test");
    for threads in [1, 3] {
        let renderer = ParallelRenderer::new(ParallelOptions {
            threads,
            inline_below: None,
            realtime: false,
            ..ParallelOptions::default()
        });
        assert!(
            same(&session(Box::new(renderer), 64), &expected),
            "{threads} workers"
        );
    }
    for max_block in [1, 37, 1024] {
        let played = session(Box::new(ReferenceRenderer), max_block);
        assert!(same(&played, &expected), "blocks of {max_block}");
    }
}

/// Plays `project` from the start for `frames` frames.
fn play(project: &Project, frames: usize) -> Vec<f32> {
    let config = config(64);
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(compile(project).prepare(&config).unwrap())
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    left(&mut engine, frames, 256)
}

#[test]
fn overridden_lanes_leave_the_value_set() {
    let mut automated = project();
    automated.automation.retain(|lane| lane.overridden);
    let mut plain = automated.clone();
    plain.automation.clear();
    assert!(same(&play(&automated, BEAT), &play(&plain, BEAT)));
    automated.automation[0].overridden = false;
    assert!(!same(&play(&automated, BEAT), &play(&plain, BEAT)));
}

#[test]
fn nodes_that_read_no_automation_get_events_that_win() {
    // The instrument's level rises from silence over two beats.
    let mut project = project();
    let instrument = ParamAddress::Device(project.tracks[2].devices[0].id, 0);
    project.automation.retain(|lane| lane.param == instrument);
    project.tracks.truncate(3);
    project.tracks[0].tone = None;
    project.tracks[0].sends.clear();
    project.tracks[1].tone = None;
    let config = config(64);
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(compile(&project).prepare(&config).unwrap())
        .unwrap();
    // A level set before the automation's first comes, on the same frame.
    let (key, param) = keys::param(instrument);
    handle
        .send(Event::Param {
            node: key,
            param,
            value: 1.0,
        })
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    let out = left(&mut engine, 2 * BEAT, 500);
    let peak = |frames: &[f32]| frames.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    let (start, end) = (peak(&out[..100]), peak(&out[2 * BEAT - 400..]));
    assert!(start < 0.01, "{start}");
    assert!(end > 0.4, "{end}");
    assert!(peak(&out[BEAT - 200..BEAT]) < end * 0.6);
}

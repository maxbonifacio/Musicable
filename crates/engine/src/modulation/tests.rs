//! A channel's modulators as its node plays them, a block at a time as the
//! engine runs it: their shapes, their routings and their ticks.

use std::f64::consts::TAU;

use musicable_model::Chain;
use musicable_model::modulation::{ControlKind, Curve, Output};
use musicable_model::param::ParamAddress;
use musicable_model::time::TimeSignature;

use super::*;
use crate::graph::Routed;
use crate::{AudioRef, NodeId, TransportState, rt};

const RATE: u32 = 48_000;
const BLOCK: usize = 512;
/// A tick's length in seconds: 32 frames.
const TICK: f64 = 32.0 / 48_000.0;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: RATE,
        max_block: BLOCK,
    }
}

/// A modulator of `kind` with id `id`, its parameters at their defaults
/// but for `values`, by number.
fn modulator(id: u32, kind: ModulatorKind, values: &[(usize, f64)]) -> model::Modulator {
    let mut modulator = model::Modulator::new(ModulatorId(id), kind, kind.name());
    for &(n, value) in values {
        modulator
            .set(n as u32, value)
            .expect("it has the parameter");
    }
    modulator
}

/// A routing from output `n` of the modulator with id `from`, by `amount`.
/// The node doesn't look at the parameter: the target it moves says.
fn routing(id: u32, from: u32, n: u32, amount: f64) -> Routing {
    let to = ParamAddress::Volume(Chain::Master);
    Routing::new(RoutingId(id), Output::new(ModulatorId(from), n), to, amount)
}

/// Moves `target` by `routing`, with its amount a share of a range 1 long.
fn route(node: &mut Modulation, routing: Routing, target: usize) {
    assert!(node.add_routing(&routing, 1.0, target));
}

fn note_on(offset: usize, note: u64) -> NodeEvent {
    let note = NoteEvent::On {
        note: NoteId(note),
        key: 60,
        velocity: 1.0,
    };
    NodeEvent {
        offset,
        kind: NodeEventKind::Note(note),
    }
}

fn note_off(offset: usize, note: u64) -> NodeEvent {
    let note = NoteEvent::Off {
        note: NoteId(note),
        velocity: 0.0,
    };
    NodeEvent {
        offset,
        kind: NodeEventKind::Note(note),
    }
}

/// Sets parameter `n` of the modulator with id `modulator` to `value`.
fn set(offset: usize, modulator: u32, n: usize, value: f64) -> NodeEvent {
    let param = param_id(ModulatorId(modulator), n as u32);
    NodeEvent {
        offset,
        kind: NodeEventKind::Param { param, value },
    }
}

/// What the ports sent: each amount's frame of engine time, its port and
/// the amount.
type Sent = Vec<(u64, usize, f64)>;

/// What port `port` held on frame `frame`: the amount it sent last by then.
fn held(sent: &[(u64, usize, f64)], port: usize, frame: u64) -> f64 {
    let last = sent.iter().rev().find(|s| s.1 == port && s.0 <= frame);
    last.expect("each port sends on the first tick").2
}

#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

/// A node run a block at a time, as the engine runs it, checking that it
/// doesn't allocate. Its port `p` goes to parameter `p` of node `p`.
struct Run {
    node: Modulation,
    ports: Vec<(NodeId, ParamId)>,
    routed: Vec<Routed>,
    tempo: TempoMap,
    /// Engine time of the next block's first frame.
    time: u64,
    transport: TransportState,
}

impl Run {
    /// Runs `node`, prepared, from engine time 0 with the transport
    /// stopped at the start of the song, at 120 bpm.
    fn new(mut node: Modulation) -> Self {
        node.prepare(&config());
        let ports = (node.targets.iter())
            .filter(|t| matches!(t.to, To::Port(_)))
            .count();
        Self {
            node,
            ports: (0..ports).map(|p| (NodeId(p), ParamId(p as u64))).collect(),
            routed: Vec::with_capacity(ports * Feed::room(BLOCK)),
            tempo: TempoMap::new(120.0, TimeSignature::new(4, 4).unwrap()),
            time: 0,
            transport: TransportState::default(),
        }
    }

    /// Runs a block of `frames` with `events`, and returns what it sent.
    fn block(&mut self, frames: usize, events: &[NodeEvent]) -> Sent {
        let Self {
            node,
            ports,
            routed,
            tempo,
            time,
            transport,
        } = self;
        routed.clear();
        let ctx = ProcessContext {
            frames,
            sample_rate: RATE,
            time: *time,
            transport: *transport,
            tempo: &*tempo,
            events,
            input: AudioRef::empty(frames),
            midi: &[],
            ahead: false,
        };
        let room = ports.len() * Feed::room(BLOCK);
        let mut controls = Controls::for_tests(routed, ports, frames, room);
        rt::scope(|| node.control(&ctx, &mut controls));
        assert_eq!(controls.dropped(), 0, "the ports have room");
        let sent = (routed.iter())
            .map(|r| match r.event.kind {
                NodeEventKind::Modulation {
                    param,
                    amount,
                    note: None,
                } => (*time + r.event.offset as u64, param.0 as usize, amount),
                kind => panic!("{kind:?} isn't modulation"),
            })
            .collect();
        *time += frames as u64;
        if transport.playing {
            transport.position += frames as u64;
        }
        sent
    }

    /// Runs `frames` frames in blocks, with no events.
    fn frames(&mut self, frames: usize) -> Sent {
        let mut sent = Vec::new();
        let mut done = 0;
        while done < frames {
            let n = BLOCK.min(frames - done);
            sent.extend(self.block(n, &[]));
            done += n;
        }
        sent
    }

    /// How far through its cycle the LFO at index `m` is.
    fn phase(&self, m: usize) -> f64 {
        match &self.node.modulators[m].kind {
            Kind::Lfo(lfo) => lfo.phase,
            _ => panic!("modulator {m} isn't an LFO"),
        }
    }
}

#[test]
fn an_lfo_plays_its_shape_at_its_rate() {
    // At 15 Hz a cycle is 3,200 frames long: each tick, a hundredth.
    let sine = modulator(0, ModulatorKind::Lfo, &[(lfo::RATE, 15.0)]);
    let triangle = modulator(
        1,
        ModulatorKind::Lfo,
        &[
            (lfo::RATE, 15.0),
            (lfo::SHAPE, 1.0),
            (lfo::PHASE, 90.0),
            (lfo::UNIPOLAR, 1.0),
        ],
    );
    let mut node = Modulation::new(&[sine, triangle]);
    for (port, from) in [(0, 0), (1, 1)] {
        let target = node.add_port(port);
        route(&mut node, routing(port as u32, from, 0, 1.0), target);
    }
    // Only the sine's top half, at half the amount.
    let target = node.add_port(2);
    let positive = Routing {
        curve: Curve::Positive,
        ..routing(2, 0, 0, 0.5)
    };
    route(&mut node, positive, target);
    let sent = Run::new(node).frames(3200);
    for k in 0..100 {
        let (frame, phase) = (32 * k, k as f64 / 100.0);
        let sine = (TAU * phase).sin();
        assert_close(held(&sent, 0, frame), sine);
        // A quarter of a cycle on, from 0 to 1.
        let triangle = lfo::Shape::Triangle.value(phase + 0.25, 0.5);
        assert_close(held(&sent, 1, frame), 0.5 * (triangle + 1.0));
        assert_close(held(&sent, 2, frame), 0.5 * sine.max(0.0));
    }
    // A port sends whenever what it moves changes: on every tick here, and
    // on the first, which sends every port, though it's 0.
    assert_eq!(sent.iter().filter(|s| s.1 == 0).count(), 100);
    assert_eq!(sent[0], (0, 0, 0.0));
}

#[test]
fn a_synced_lfo_with_the_song_start_follows_the_beats() {
    // A saw a beat long, counted from the start of the song: 24,000 frames
    // at 120 bpm.
    let saw = modulator(
        0,
        ModulatorKind::Lfo,
        &[(lfo::SHAPE, 2.0), (lfo::SYNC, 1.0), (lfo::START, 1.0)],
    );
    let mut node = Modulation::new(&[saw]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let mut run = Run::new(node);
    // From a quarter of the way through beat 6.
    run.transport.playing = true;
    run.transport.position = 126_000;
    let sent = run.block(64, &[]);
    assert_eq!(sent.len(), 2);
    assert_close(sent[0].2, 0.5);
    assert_close(sent[1].2, 0.5 + 2.0 * 32.0 / 24_000.0);
    // It holds still while the transport stands still.
    run.transport.playing = false;
    let sent = run.frames(1024);
    assert_eq!(sent.len(), 1);
    assert_close(sent[0].2, 0.5 + 2.0 * 64.0 / 24_000.0);
}

#[test]
fn a_note_starts_an_lfo_again_after_its_delay_and_fade_in() {
    // A square, high for half a second, that waits 10 ms after each start
    // and fades in over 10 ms more.
    let square = modulator(
        0,
        ModulatorKind::Lfo,
        &[
            (lfo::SHAPE, 3.0),
            (lfo::START, 2.0),
            (lfo::DELAY, 10.0),
            (lfo::FADE_IN, 10.0),
        ],
    );
    let mut node = Modulation::new(&[square]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let mut run = Run::new(node);
    let mut sent = run.block(BLOCK, &[note_on(100, 1)]);
    sent.extend(run.frames(1536));
    // The note starts it on its own frame, a tick of its own, so it waits
    // until frame 580 and is in by 1060.
    let fade = |frame: u64| ((frame - 100) as f64 / f64::from(RATE) - 0.01) / 0.01;
    assert!(sent.iter().all(|s| s.0 >= 608 || s.2 == 0.0), "{sent:?}");
    assert_close(held(&sent, 0, 608), fade(608));
    assert_close(held(&sent, 0, 1056), fade(1056));
    assert_eq!(held(&sent, 0, 1088), 1.0);
    assert_eq!(sent.last().unwrap().0, 1088);
}

#[test]
fn note_triggers_say_which_notes_start_an_lfo_again() {
    let on_notes = |trigger: f64| (lfo::NOTE_TRIGGER, trigger);
    let start = (lfo::START, 2.0);
    let node = Modulation::new(&[
        modulator(0, ModulatorKind::Lfo, &[start, on_notes(1.0)]),
        modulator(1, ModulatorKind::Lfo, &[start, on_notes(2.0)]),
        modulator(2, ModulatorKind::Lfo, &[start, on_notes(3.0)]),
    ]);
    let mut run = Run::new(node);
    // At 1 Hz, the cycle's share is the seconds since the LFO started, at
    // the last tick of each block.
    let since = |frames: f64| frames / f64::from(RATE);
    run.block(BLOCK, &[note_on(0, 1)]);
    for m in 0..3 {
        assert_close(run.phase(m), since(480.0));
    }
    // Always starts again on a note while another is held, and Legato
    // doesn't.
    run.block(BLOCK, &[note_on(0, 2)]);
    assert_close(run.phase(0), since(480.0));
    assert_close(run.phase(1), since(992.0));
    // Legato starts again on a note once they've all ended, and Never
    // doesn't start on notes.
    run.block(BLOCK, &[note_off(0, 1), note_off(10, 2), note_on(20, 3)]);
    assert_close(run.phase(0), since(1504.0 - 1044.0));
    assert_close(run.phase(1), since(1504.0 - 1044.0));
    assert_close(run.phase(2), since(1504.0));
}

#[test]
fn an_envelope_runs_its_stages_while_notes_hold_its_gate() {
    // Up over 10 ms, down to half over 20 ms, and down to nothing over 10
    // ms once let go: 480, 960 and 480 frames.
    let envelope = modulator(
        0,
        ModulatorKind::Envelope,
        &[
            (envelope::ATTACK, 10.0),
            (envelope::DECAY, 20.0),
            (envelope::SUSTAIN, 50.0),
            (envelope::RELEASE, 10.0),
        ],
    );
    let mut node = Modulation::new(&[envelope]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let mut run = Run::new(node);
    let mut sent = run.block(BLOCK, &[note_on(0, 1)]);
    sent.extend(run.frames(1536));
    sent.extend(run.block(BLOCK, &[note_off(0, 1)]));
    sent.extend(run.frames(BLOCK));
    let level = |frame| held(&sent, 0, frame);
    assert_eq!(level(0), 0.0);
    assert_close(level(256), 256.0 / 480.0);
    assert_close(level(480), 1.0);
    assert_close(level(480 + 480), 0.75);
    assert_close(level(1440), 0.5);
    assert_close(level(2047), 0.5);
    assert_close(level(2048 + 256), 0.5 * (1.0 - 256.0 / 480.0));
    assert_close(level(2048 + 480), 0.0);
    assert_eq!(level(2048 + 512), 0.0);
    assert!(sent.last().unwrap().0 <= 2048 + 512);
}

#[test]
fn a_one_shot_envelope_runs_on_whatever_the_gate_does() {
    let stages = [
        (envelope::ATTACK, 10.0),
        (envelope::DECAY, 10.0),
        (envelope::ONE_SHOT, 1.0),
    ];
    let straight = modulator(0, ModulatorKind::Envelope, &stages);
    let mut bent = modulator(1, ModulatorKind::Envelope, &stages);
    bent.set(envelope::ATTACK_CURVE as u32, 100.0);
    let mut node = Modulation::new(&[straight, bent]);
    for port in [0, 1] {
        let target = node.add_port(port);
        route(&mut node, routing(port as u32, port as u32, 0, 1.0), target);
    }
    let mut run = Run::new(node);
    let mut sent = run.block(BLOCK, &[note_on(0, 1), note_off(64, 1)]);
    sent.extend(run.frames(1024));
    let level = |frame| held(&sent, 0, frame);
    assert_close(level(256), 256.0 / 480.0);
    assert_close(level(480), 1.0);
    assert_close(level(480 + 256), 1.0 - 256.0 / 480.0);
    assert_close(level(960), 0.0);
    assert_eq!(level(992), 0.0);
    // A curve of 100% rises fast at first, then slowly.
    let bent = envelope::bend(256.0 / 480.0, 1.0);
    assert!(bent > 256.0 / 480.0);
    assert_close(held(&sent, 1, 256), bent);
}

#[test]
fn an_envelope_with_its_transport_switch_plays_while_the_song_does() {
    let envelope = modulator(
        0,
        ModulatorKind::Envelope,
        &[
            (envelope::ATTACK, 0.0),
            (envelope::DECAY, 0.0),
            (envelope::SUSTAIN, 100.0),
            (envelope::RELEASE, 0.0),
            (envelope::TRANSPORT, 1.0),
        ],
    );
    let mut node = Modulation::new(&[envelope]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let mut run = Run::new(node);
    assert_eq!(run.block(64, &[]), [(0, 0, 0.0)]);
    run.transport.playing = true;
    assert_eq!(run.block(64, &[]), [(64, 0, 1.0)]);
    run.transport.playing = false;
    assert_eq!(run.block(64, &[]), [(128, 0, 0.0)]);
}

#[test]
fn modulators_move_each_other_from_the_tick_after() {
    // A knob opens an envelope's gate, which jumps to full at once.
    let knob = modulator(0, ModulatorKind::Macro, &[]);
    let envelope = modulator(
        1,
        ModulatorKind::Envelope,
        &[
            (envelope::ATTACK, 0.0),
            (envelope::DECAY, 0.0),
            (envelope::SUSTAIN, 100.0),
        ],
    );
    let mut node = Modulation::new(&[knob, envelope]);
    let gate = node
        .add_param(ModulatorId(1), envelope::GATE as u32)
        .unwrap();
    route(&mut node, routing(0, 0, 0, 1.0), gate);
    let target = node.add_port(0);
    route(&mut node, routing(1, 1, 0, 1.0), target);
    // With the knob on its gate, notes don't start it: Auto acts as Never.
    node.move_trigger(ModulatorId(1));
    let mut run = Run::new(node);
    let sent = run.block(BLOCK, &[note_on(0, 1), set(64, 0, 0, 1.0)]);
    assert_eq!(sent, [(0, 0, 0.0), (96, 0, 1.0)]);
}

#[test]
fn a_rising_trigger_starts_an_lfo_again() {
    let knob = modulator(0, ModulatorKind::Macro, &[]);
    let lfo = modulator(1, ModulatorKind::Lfo, &[(lfo::START, 2.0)]);
    let mut node = Modulation::new(&[knob, lfo]);
    let trigger = node.add_param(ModulatorId(1), lfo::TRIGGER as u32).unwrap();
    route(&mut node, routing(0, 0, 0, 1.0), trigger);
    node.move_trigger(ModulatorId(1));
    let mut run = Run::new(node);
    // Notes don't start it, so it runs on from when the node started.
    run.block(BLOCK, &[note_on(0, 1)]);
    assert_close(run.phase(1), 480.0 / f64::from(RATE));
    // The knob rises on frame 612, and the trigger with it on the next
    // tick, 640. The block's last tick is on 992.
    run.block(BLOCK, &[set(100, 0, 0, 1.0)]);
    assert_close(run.phase(1), (992.0 - 640.0) / f64::from(RATE));
}

#[test]
fn routings_bend_scale_and_smooth_what_they_take() {
    let mut knobs = modulator(0, ModulatorKind::Macro, &[]);
    let second = knobs.new_control(ControlKind::Knob);
    knobs.controls.push(second);
    let mut node = Modulation::new(&[knobs]);
    let routings = [
        // Half the first knob, squared.
        Routing {
            curve: Curve::Exponential,
            ..routing(0, 0, 0, 0.5)
        },
        // The first knob, times the second.
        Routing {
            scale: Some(Output::new(ModulatorId(0), 1)),
            ..routing(1, 0, 0, 1.0)
        },
        // The first knob, over 10 ms.
        Routing {
            lag: 10.0,
            ..routing(2, 0, 0, 1.0)
        },
    ];
    for (port, routing) in routings.into_iter().enumerate() {
        let target = node.add_port(port);
        route(&mut node, routing, target);
    }
    // Half the first knob, of a range 4 long.
    let target = node.add_port(3);
    assert!(node.add_routing(&routing(3, 0, 0, 0.5), 4.0, target));
    let mut run = Run::new(node);
    let amount = NodeEvent {
        offset: 128,
        kind: NodeEventKind::Param {
            param: amount_id(RoutingId(1)),
            value: 0.25,
        },
    };
    let events = [
        set(0, 0, 0, 0.5),
        set(0, 0, 1, 0.5),
        set(64, 0, 0, 1.0),
        amount,
    ];
    let mut sent = run.block(BLOCK, &events);
    sent.extend(run.block(BLOCK, &[]));
    let at = |port, frame| held(&sent, port, frame);
    assert_eq!(
        [at(0, 0), at(1, 0), at(2, 0), at(3, 0)],
        [0.125, 0.25, 0.5, 1.0]
    );
    assert_eq!([at(0, 64), at(1, 64), at(3, 64)], [0.5, 0.5, 2.0]);
    // A new amount applies without a new node.
    assert_eq!(at(1, 128), 0.125);
    // The lag starts where the output was, and follows it about two thirds
    // of the way in its time.
    let follow = 1.0 - (-TICK / 0.01).exp();
    let mut lagged = 0.5;
    for k in 2..=16 {
        lagged += (1.0 - lagged) * follow;
        assert_close(at(2, 32 * k), lagged);
    }
    assert_close(at(2, 512), 1.0 - 0.5 * (-1.0_f64).exp());
}

#[test]
fn ticks_fall_on_the_grid_of_engine_time_and_on_events() {
    let mut node = Modulation::new(&[modulator(0, ModulatorKind::Macro, &[])]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let mut run = Run::new(node);
    // A block that starts off the grid, the knob moving on each of the 20
    // frames after its first.
    run.time = 10;
    let value = |k: usize| k as f64 / 100.0;
    let events: Vec<NodeEvent> = (1..=20).map(|k| set(k, 0, 0, value(k))).collect();
    let sent = run.block(BLOCK, &events);
    // A tick on the block's first frame, one on each of its first 16
    // events, and one for the rest on the grid, on frame 32.
    let mut ticks = vec![(10, 0, 0.0)];
    ticks.extend((1..=16).map(|k| (10 + k as u64, 0, value(k))));
    ticks.push((32, 0, value(20)));
    assert_eq!(sent, ticks);
    assert!(run.frames(BLOCK).is_empty());
}

#[test]
fn relays_add_what_routings_on_other_channels_move() {
    let knob = modulator(0, ModulatorKind::Macro, &[(0, 0.5)]);
    let mut node = Modulation::new(&[knob]);
    let target = node.add_port(0);
    route(&mut node, routing(0, 0, 0, 1.0), target);
    let relay = node.add_relay(target);
    let mut run = Run::new(node);
    let master = NodeEvent {
        offset: 40,
        kind: NodeEventKind::Modulation {
            param: relay,
            amount: 0.25,
            note: None,
        },
    };
    assert_eq!(run.block(BLOCK, &[master]), [(0, 0, 0.5), (40, 0, 0.75)]);
}

#[test]
fn a_new_node_carries_on_where_the_one_it_replaces_left_off() {
    let node = || {
        let lfo = modulator(0, ModulatorKind::Lfo, &[(lfo::RATE, 15.0)]);
        let mut node = Modulation::new(&[lfo]);
        let target = node.add_port(0);
        let lagged = Routing {
            lag: 5.0,
            ..routing(0, 0, 0, 1.0)
        };
        route(&mut node, lagged, target);
        node
    };
    let mut all_along = Run::new(node());
    let mut swapped = Run::new(node());
    assert_eq!(swapped.frames(1600), all_along.frames(1600));
    // As a new graph does: its shape and its lag go on as they were.
    let mut new = node();
    new.prepare(&config());
    new.adopt(&mut swapped.node);
    swapped.node = new;
    let after = all_along.frames(1600);
    assert!(after[0].0 == 1600 && after.len() == 50);
    assert_eq!(swapped.frames(1600), after);
}

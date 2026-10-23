use musicable_model::Monitor;
use musicable_model::clip::{Controller, Dimension};
use musicable_model::time::TempoMap;

use super::*;
use crate::{
    AudioRef, Block, EngineConfig, Graph, NodeEvent, NodeId, NodeKey, PreparedGraph,
    ReferenceRenderer, Renderer, rt,
};

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 64,
};

/// What `bytes` does to notes, for a message that moves no controller.
fn take(notes: &mut MidiNotes, bytes: [u8; 3]) -> Vec<NoteEvent> {
    play(notes, bytes)
        .into_iter()
        .map(|played| match played {
            Played::Note(note) => note,
            Played::Controller { .. } => panic!("{bytes:?} moves a controller"),
        })
        .collect()
}

/// What `bytes` plays.
fn play(notes: &mut MidiNotes, bytes: [u8; 3]) -> Vec<Played> {
    let mut played = Vec::new();
    notes.take(bytes, |p| played.push(p));
    played
}

fn moves(controller: Controller, value: f32) -> Played {
    Played::Controller { controller, value }
}

fn on(note: u64, key: u8, velocity: f32) -> NoteEvent {
    NoteEvent::On {
        note: NoteId(note),
        key,
        velocity,
    }
}

fn off(note: u64, velocity: f32) -> NoteEvent {
    NoteEvent::Off {
        note: NoteId(note),
        velocity,
    }
}

fn expression(note: u64, dimension: Dimension, value: f32) -> NoteEvent {
    NoteEvent::Expression {
        note: NoteId(note),
        dimension,
        value,
    }
}

fn bend(note: u64, semitones: f32) -> NoteEvent {
    expression(note, Dimension::PitchBend, semitones)
}

/// The messages that set registered parameter `rpn` on `channel` to
/// `value`.
fn rpn(channel: u8, rpn: u8, value: u8) -> [[u8; 3]; 3] {
    let status = 0xB0 | channel;
    [[status, 101, 0], [status, 100, rpn], [status, 6, value]]
}

#[test]
fn notes_start_and_end_with_ids_of_their_own() {
    let mut notes = MidiNotes::new(Some(0), 100);
    assert_eq!(take(&mut notes, [0x90, 60, 127]), [on(100, 60, 1.0)]);
    assert_eq!(take(&mut notes, [0x90, 64, 127]), [on(101, 64, 1.0)]);
    assert_eq!(take(&mut notes, [0x80, 60, 0]), [off(100, 0.0)]);
    // A note on at velocity 0 lets a note go at no particular speed.
    assert_eq!(take(&mut notes, [0x90, 64, 0]), [off(101, RELEASE)]);
    // Letting go of a key that isn't held does nothing.
    assert_eq!(take(&mut notes, [0x80, 64, 0]), []);
    // A key struck again before it's let go ends its note first.
    assert_eq!(take(&mut notes, [0x90, 62, 127]), [on(102, 62, 1.0)]);
    assert_eq!(
        take(&mut notes, [0x90, 62, 127]),
        [off(102, RELEASE), on(103, 62, 1.0)]
    );
}

#[test]
fn it_takes_one_channel_or_every_channel() {
    let mut notes = MidiNotes::new(Some(3), 0);
    assert_eq!(notes.channel(), Some(3));
    assert_eq!(take(&mut notes, [0x94, 60, 127]), []);
    assert_eq!(take(&mut notes, [0x93, 60, 127]), [on(0, 60, 1.0)]);
    // Anything but channel messages does nothing: a program change, the
    // clock and the start of a system exclusive message.
    for bytes in [[0xC3, 5, 0], [0xF8, 0, 0], [0xF0, 0x7E, 0x7F]] {
        assert_eq!(take(&mut notes, bytes), [], "{bytes:?}");
    }

    let mut every = MidiNotes::new(None, 0);
    assert_eq!(every.channel(), None);
    assert_eq!(take(&mut every, [0x94, 60, 127]), [on(0, 60, 1.0)]);
    assert_eq!(take(&mut every, [0x9F, 60, 127]), [on(1, 60, 1.0)]);
}

#[test]
fn pitch_bend_goes_as_far_as_its_range() {
    let mut notes = MidiNotes::new(Some(0), 0);
    take(&mut notes, [0x90, 60, 127]);
    // Two semitones either way until it's told otherwise.
    assert_eq!(take(&mut notes, [0xE0, 0x7F, 0x7F]), [bend(0, 2.0)]);
    assert_eq!(take(&mut notes, [0xE0, 0, 0]), [bend(0, -2.0)]);
    assert_eq!(take(&mut notes, [0xE0, 0, 0x40]), [bend(0, 0.0)]);

    // RPN 0 sets the range, in semitones and cents.
    let [select_msb, select_lsb, coarse] = rpn(0, 0, 12);
    for bytes in [select_msb, select_lsb, coarse, [0xB0, 38, 50]] {
        assert_eq!(take(&mut notes, bytes), [], "{bytes:?}");
    }
    assert_eq!(take(&mut notes, [0xE0, 0x7F, 0x7F]), [bend(0, 12.5)]);
    assert_eq!(take(&mut notes, [0xE0, 0, 0]), [bend(0, -12.5)]);

    // Data entry for a non-registered parameter leaves it alone.
    for bytes in [[0xB0, 99, 0], [0xB0, 98, 0], [0xB0, 6, 48]] {
        assert_eq!(take(&mut notes, bytes), [], "{bytes:?}");
    }
    assert_eq!(take(&mut notes, [0xE0, 0x7F, 0x7F]), [bend(0, 12.5)]);
}

#[test]
fn mpe_members_bend_their_own_notes_and_the_master_bends_them_all() {
    let mut notes = MidiNotes::new(None, 0);
    assert_eq!(take(&mut notes, [0x91, 60, 127]), [on(0, 60, 1.0)]);
    assert_eq!(take(&mut notes, [0x92, 64, 127]), [on(1, 64, 1.0)]);
    // A member channel bends its note 48 semitones at most.
    assert_eq!(take(&mut notes, [0xE1, 0x7F, 0x7F]), [bend(0, 48.0)]);
    assert_eq!(take(&mut notes, [0xE1, 0, 0]), [bend(0, -48.0)]);
    // The master channel bends every note in the zone 2 semitones at most,
    // on top of their own bend.
    assert_eq!(
        take(&mut notes, [0xE0, 0x7F, 0x7F]),
        [bend(0, -46.0), bend(1, 2.0)]
    );
    // A note starts with the bend its channel has.
    assert_eq!(
        take(&mut notes, [0x91, 67, 127]),
        [on(2, 67, 1.0), bend(2, -46.0)]
    );
}

#[test]
fn mpe_configuration_messages_set_the_zones() {
    let mut notes = MidiNotes::new(None, 0);
    // An upper zone of three channels: 14, 13 and 12 leave the lower zone
    // for it.
    for bytes in rpn(15, 6, 3) {
        assert_eq!(take(&mut notes, bytes), [], "{bytes:?}");
    }
    take(&mut notes, [0x9E, 60, 127]);
    take(&mut notes, [0x9B, 62, 127]);
    // Each master bends the notes in its own zone.
    assert_eq!(take(&mut notes, [0xEF, 0x7F, 0x7F]), [bend(0, 2.0)]);
    assert_eq!(take(&mut notes, [0xE0, 0x7F, 0x7F]), [bend(1, 2.0)]);
    assert_eq!(take(&mut notes, [0xEE, 0x7F, 0x7F]), [bend(0, 50.0)]);

    // A zone of no channels ends the zone: its channels bend 2 semitones,
    // on their own.
    for bytes in rpn(0, 6, 0) {
        take(&mut notes, bytes);
    }
    assert_eq!(take(&mut notes, [0xEB, 0x7F, 0x7F]), [bend(1, 2.0)]);

    // A zone of all 15 channels takes them from the other zone, whose
    // master is then a channel like any other.
    for bytes in rpn(0, 6, 15) {
        take(&mut notes, bytes);
    }
    assert_eq!(take(&mut notes, [0xEE, 0, 0x40]), [bend(0, 2.0)]);
    assert_eq!(take(&mut notes, [0xEF, 0, 0x40]), []);

    // Taking one channel, it reads that channel as a keyboard plays it, and
    // configuration messages change nothing.
    let mut one = MidiNotes::new(Some(15), 0);
    for bytes in rpn(15, 6, 3) {
        take(&mut one, bytes);
    }
    take(&mut one, [0x9F, 60, 127]);
    assert_eq!(take(&mut one, [0xEF, 0x7F, 0x7F]), [bend(0, 2.0)]);
}

#[test]
fn pressure_and_slide_go_to_the_notes_on_their_channel() {
    let mut notes = MidiNotes::new(None, 0);
    // A note starts with the expression its channel has.
    for bytes in [[0xD3, 127, 0], [0xB3, 74, 0], [0xE3, 0x7F, 0x7F]] {
        assert_eq!(take(&mut notes, bytes), [], "{bytes:?}");
    }
    assert_eq!(
        take(&mut notes, [0x93, 60, 127]),
        [
            on(0, 60, 1.0),
            bend(0, 48.0),
            expression(0, Dimension::Pressure, 1.0),
            expression(0, Dimension::Timbre, 0.0),
        ]
    );
    // Then it follows it.
    assert_eq!(
        take(&mut notes, [0xD3, 0, 0]),
        [expression(0, Dimension::Pressure, 0.0)]
    );
    assert_eq!(
        take(&mut notes, [0xB3, 74, 127]),
        [expression(0, Dimension::Timbre, 1.0)]
    );
    // A key's own pressure goes to its note alone.
    assert_eq!(
        take(&mut notes, [0xA3, 60, 127]),
        [expression(0, Dimension::Pressure, 1.0)]
    );
    assert_eq!(take(&mut notes, [0xA3, 61, 127]), []);
    // Another channel's go to its own notes.
    assert_eq!(take(&mut notes, [0xD4, 127, 0]), []);
}

#[test]
fn channel_mode_messages_end_notes_and_reset_expression() {
    let mut notes = MidiNotes::new(None, 0);
    take(&mut notes, [0x91, 60, 127]);
    take(&mut notes, [0x91, 62, 127]);
    take(&mut notes, [0x92, 64, 127]);
    // All Notes Off ends a channel's notes.
    assert_eq!(
        take(&mut notes, [0xB1, 123, 0]),
        [off(0, RELEASE), off(1, RELEASE)]
    );
    assert_eq!(take(&mut notes, [0x81, 60, 0]), []);
    // Reset All Controllers takes a channel's expression back to rest.
    take(&mut notes, [0xD2, 127, 0]);
    take(&mut notes, [0xB2, 74, 127]);
    take(&mut notes, [0xE2, 0x7F, 0x7F]);
    assert_eq!(
        take(&mut notes, [0xB2, 121, 0]),
        [
            bend(2, 0.0),
            expression(2, Dimension::Pressure, 0.0),
            expression(2, Dimension::Timbre, 0.0),
        ]
    );
    // All Sound Off ends notes too.
    assert_eq!(take(&mut notes, [0xB2, 120, 0]), [off(2, RELEASE)]);
}

#[test]
fn controllers_move_for_every_note_from_any_channel() {
    let mut notes = MidiNotes::new(None, 0);
    // The mod wheel on the master channel, the sustain pedal on a member.
    let wheel = 64.0 / 127.0;
    assert_eq!(
        play(&mut notes, [0xB0, 1, 64]),
        [moves(Controller::MOD_WHEEL, wheel)]
    );
    assert_eq!(
        play(&mut notes, [0xB3, 64, 127]),
        [moves(Controller::SUSTAIN, 1.0)]
    );
    // Slide is a note's expression, data entry and parameter numbers set
    // parameters, and channel mode messages aren't controllers.
    for bytes in [
        [0xB1, 74, 10],
        [0xB0, 101, 0],
        [0xB0, 100, 0],
        [0xB0, 6, 48],
        [0xB0, 38, 0],
        [0xB0, 96, 1],
        [0xB0, 99, 1],
        [0xB0, 123, 0],
        [0xB0, 127, 0],
    ] {
        assert_eq!(play(&mut notes, bytes), [], "{bytes:?}");
    }
    let held = |notes: &MidiNotes| notes.controllers().collect::<Vec<_>>();
    assert_eq!(
        held(&notes),
        [(Controller::MOD_WHEEL, wheel), (Controller::SUSTAIN, 1.0)]
    );
    // Reset All Controllers lets go of the switches held on, on any
    // channel, and leaves the rest where they are.
    assert_eq!(
        play(&mut notes, [0xB5, 121, 0]),
        [moves(Controller::SUSTAIN, 0.0)]
    );
    assert_eq!(play(&mut notes, [0xB5, 121, 0]), []);
    assert_eq!(
        held(&notes),
        [(Controller::MOD_WHEEL, wheel), (Controller::SUSTAIN, 0.0)]
    );

    // Taking one channel, it takes that channel's alone.
    let mut one = MidiNotes::new(Some(2), 0);
    assert_eq!(play(&mut one, [0xB3, 64, 127]), []);
    assert_eq!(
        play(&mut one, [0xB2, 64, 127]),
        [moves(Controller::SUSTAIN, 1.0)]
    );
}

#[test]
fn notes_held_can_be_let_go_or_forgotten() {
    let mut notes = MidiNotes::new(None, 0);
    take(&mut notes, [0x90, 60, 127]);
    take(&mut notes, [0x95, 61, 127]);
    play(&mut notes, [0xB0, 64, 127]);
    // Letting go lets go of the sustain pedal too, so nothing rings on.
    let mut released = Vec::new();
    notes.release_all(|event| released.push(event));
    assert_eq!(
        released,
        [
            Played::Note(off(0, RELEASE)),
            Played::Note(off(1, RELEASE)),
            moves(Controller::SUSTAIN, 0.0)
        ]
    );
    assert_eq!(take(&mut notes, [0x80, 60, 0]), []);
    // It still knows the pedal is down, as it is.
    assert_eq!(
        notes.controllers().collect::<Vec<_>>(),
        [(Controller::SUSTAIN, 1.0)]
    );

    take(&mut notes, [0x90, 60, 127]);
    notes.forget_notes();
    assert_eq!(take(&mut notes, [0x80, 60, 0]), []);
}

/// Plays notes and moves controllers at set engine times, as a clip player
/// does.
struct Clips(Vec<(u64, NodeEventKind)>);

impl Node for Clips {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let block = ctx.time..ctx.time + ctx.frames as u64;
        for &(time, kind) in &self.0 {
            if !block.contains(&time) {
                continue;
            }
            let offset = (time - ctx.time) as usize;
            match kind {
                NodeEventKind::Note(note) => notes.send(offset, note),
                NodeEventKind::Controller { controller, value } => {
                    notes.send_controller(offset, controller, value)
                }
                _ => continue,
            };
        }
    }
}

/// Keeps the notes it gets and the controllers' moves, at their engine
/// times, as an instrument would play them.
struct Keep {
    notes: Vec<(u64, NoteEvent)>,
    moves: Vec<(u64, Controller, f32)>,
}

impl Keep {
    fn new() -> Self {
        Self {
            notes: Vec::with_capacity(64),
            moves: Vec::with_capacity(64),
        }
    }
}

impl Node for Keep {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        for event in ctx.events {
            let time = ctx.time + event.offset as u64;
            match event.kind {
                NodeEventKind::Note(note) => self.notes.push((time, note)),
                NodeEventKind::Controller { controller, value } => {
                    self.moves.push((time, controller, value));
                }
                _ => {}
            }
        }
        output.clear();
    }
}

/// A clip player, a monitor and an instrument, rendered a block of 64
/// frames at a time.
struct Rig {
    graph: PreparedGraph,
    time: u64,
    transport: TransportState,
    tempo: TempoMap,
}

const MONITOR: NodeId = NodeId(1);
const INSTRUMENT: NodeId = NodeId(2);

fn graph(monitor: MidiMonitor, clips: Vec<(u64, NodeEventKind)>) -> PreparedGraph {
    let mut g = Graph::new();
    let player = g.add(NodeKey(0), Clips(clips));
    let monitor = g.add(NodeKey(1), monitor);
    let instrument = g.add(NodeKey(2), Keep::new());
    assert_eq!((monitor, instrument), (MONITOR, INSTRUMENT));
    g.send_notes(player, monitor).unwrap();
    g.send_notes(monitor, instrument).unwrap();
    g.set_output(instrument).unwrap();
    g.prepare(&CONFIG).unwrap()
}

impl Rig {
    /// With clips that play `clips`' notes.
    fn new(monitor: MidiMonitor, clips: Vec<(u64, NoteEvent)>) -> Self {
        let clips = clips.into_iter().map(|(t, n)| (t, NodeEventKind::Note(n)));
        Self::with_clips(monitor, clips.collect())
    }

    /// With clips that play `clips`, notes and controllers.
    fn with_clips(monitor: MidiMonitor, clips: Vec<(u64, NodeEventKind)>) -> Self {
        Self {
            graph: graph(monitor, clips),
            time: 0,
            transport: TransportState::default(),
            tempo: TempoMap::default(),
        }
    }

    /// Renders the next block, with `midi`.
    fn block(&mut self, midi: &[MidiMessage]) {
        let block = Block {
            frames: 64,
            time: self.time,
            transport: self.transport,
            tempo: &self.tempo,
            input: AudioRef::empty(64),
            midi,
        };
        let graph = &mut self.graph;
        rt::scope(|| {
            assert_eq!(graph.play_notes(&block), 0);
            graph.load_routed();
            ReferenceRenderer.render(graph, &block);
            graph.clear_routed();
        });
        self.time += 64;
    }

    /// Sets one of the monitor's parameters as the next block renders.
    fn set(&mut self, param: ParamId, value: f64) {
        let kind = NodeEventKind::Param { param, value };
        assert!(self.graph.route(MONITOR, NodeEvent { offset: 0, kind }));
    }

    /// Puts in a graph with `monitor` in place of the monitor, without
    /// clips, as a swap does.
    fn swap(&mut self, monitor: MidiMonitor) {
        let mut new = graph(monitor, Vec::new());
        rt::scope(|| new.adopt_from(&mut self.graph));
        self.graph = new;
    }

    fn instrument(&mut self) -> &mut Keep {
        let node = self.graph.nodes[INSTRUMENT.0].as_mut();
        node.downcast_mut::<Keep>().unwrap()
    }

    /// The notes the instrument got since this was last asked.
    fn kept(&mut self) -> Vec<(u64, NoteEvent)> {
        std::mem::replace(&mut self.instrument().notes, Vec::with_capacity(64))
    }

    /// The controllers' moves the instrument got since this was last asked.
    fn moved(&mut self) -> Vec<(u64, Controller, f32)> {
        std::mem::replace(&mut self.instrument().moves, Vec::with_capacity(64))
    }
}

fn message(time: u64, input: usize, bytes: [u8; 3]) -> MidiMessage {
    MidiMessage { time, input, bytes }
}

/// A monitor taking every channel of every input.
fn monitor(monitor: Monitor, armed: bool) -> MidiMonitor {
    let source = MidiSource {
        input: None,
        channel: None,
    };
    MidiMonitor::new(monitor, armed).with_source(source)
}

fn playing(recording: bool) -> TransportState {
    TransportState {
        playing: true,
        record: recording,
        recording,
        ..TransportState::default()
    }
}

#[test]
fn an_armed_track_plays_its_input() {
    let mut rig = Rig::new(monitor(Monitor::Auto, true), Vec::new());
    rig.block(&[message(10, 0, [0x90, 60, 127])]);
    rig.block(&[message(70, 3, [0x80, 60, 0])]);
    assert_eq!(
        rig.kept(),
        [(10, on(LIVE_NOTES, 60, 1.0)), (70, off(LIVE_NOTES, 0.0))]
    );
}

#[test]
fn monitoring_says_whether_the_input_or_the_clips_play() {
    let clips = vec![(5, on(1, 50, 1.0)), (20, off(1, 1.0))];
    let midi = [
        message(10, 0, [0x90, 60, 127]),
        message(30, 0, [0x80, 60, 127]),
    ];
    // Monitoring, armed, recording: whether the input plays, and whether
    // the clips do.
    for (mode, armed, recording, input, clip) in [
        (Monitor::Off, true, false, false, true),
        (Monitor::In, false, false, true, false),
        (Monitor::Auto, false, false, false, true),
        (Monitor::Auto, false, true, false, true),
        (Monitor::Auto, true, false, true, true),
        (Monitor::Auto, true, true, true, false),
    ] {
        let mut rig = Rig::new(monitor(mode, armed), clips.clone());
        rig.transport = playing(recording);
        rig.block(&midi);
        let mut expected = Vec::new();
        if clip {
            expected.push((5, on(1, 50, 1.0)));
        }
        if input {
            expected.push((10, on(LIVE_NOTES, 60, 1.0)));
        }
        // Ends always pass: instruments pass over notes they don't know.
        expected.push((20, off(1, 1.0)));
        if input {
            expected.push((30, off(LIVE_NOTES, 1.0)));
        }
        assert_eq!(
            rig.kept(),
            expected,
            "{mode:?}, armed {armed}, recording {recording}"
        );
    }
}

#[test]
fn notes_held_end_when_the_input_stops_playing() {
    let mut rig = Rig::new(monitor(Monitor::Auto, true), Vec::new());
    rig.block(&[message(10, 0, [0x90, 60, 127])]);
    // Disarming takes effect from the next block.
    rig.set(MidiMonitor::ARMED, 0.0);
    rig.block(&[]);
    rig.block(&[message(130, 0, [0x80, 60, 127])]);
    assert_eq!(
        rig.kept(),
        [
            (10, on(LIVE_NOTES, 60, 1.0)),
            (128, off(LIVE_NOTES, RELEASE))
        ]
    );

    // What's held while it doesn't play stays silent once it plays again.
    rig.block(&[message(200, 0, [0x90, 62, 127])]);
    rig.set(MidiMonitor::ARMED, 1.0);
    rig.block(&[]);
    rig.block(&[
        message(330, 0, [0x80, 62, 127]),
        message(340, 0, [0x90, 64, 127]),
    ]);
    assert_eq!(rig.kept(), [(340, on(LIVE_NOTES + 2, 64, 1.0))]);
}

#[test]
fn recording_over_the_clips_ends_their_notes() {
    let pressure = expression(1, Dimension::Pressure, 0.5);
    let clips = vec![
        (10, on(1, 50, 1.0)),
        (100, pressure),
        (150, off(1, 1.0)),
        (170, on(2, 52, 1.0)),
    ];
    let mut rig = Rig::new(monitor(Monitor::Auto, true), clips);
    rig.transport = playing(false);
    rig.block(&[]);
    rig.transport = playing(true);
    rig.block(&[]);
    rig.block(&[]);
    assert_eq!(
        rig.kept(),
        [
            (10, on(1, 50, 1.0)),
            (64, off(1, RELEASE)),
            (100, pressure),
            (150, off(1, 1.0)),
        ]
    );
}

#[test]
fn a_track_takes_its_own_input_and_channel() {
    let source = MidiSource {
        input: Some(1),
        channel: Some(2),
    };
    let mut rig = Rig::new(
        MidiMonitor::new(Monitor::In, false).with_source(source),
        Vec::new(),
    );
    rig.block(&[
        message(1, 0, [0x92, 60, 127]),
        message(2, 1, [0x93, 61, 127]),
        message(3, 1, [0x92, 62, 127]),
    ]);
    assert_eq!(rig.kept(), [(3, on(LIVE_NOTES, 62, 1.0))]);

    // A track whose input isn't open plays none.
    let mut rig = Rig::new(MidiMonitor::new(Monitor::In, false), Vec::new());
    rig.block(&[message(3, 0, [0x90, 62, 127])]);
    assert_eq!(rig.kept(), []);
}

#[test]
fn input_and_clips_play_in_order_and_late_messages_play_at_once() {
    let clips = vec![(70, on(1, 50, 1.0)), (90, off(1, 1.0))];
    let mut rig = Rig::new(monitor(Monitor::Auto, true), clips);
    rig.transport = playing(false);
    rig.block(&[]);
    // Messages timed before the block play at its start, and those timed
    // after it at its end.
    rig.block(&[
        message(20, 0, [0x90, 60, 127]),
        message(80, 0, [0x80, 60, 127]),
        message(500, 0, [0x90, 61, 127]),
    ]);
    assert_eq!(
        rig.kept(),
        [
            (64, on(LIVE_NOTES, 60, 1.0)),
            (70, on(1, 50, 1.0)),
            (80, off(LIVE_NOTES, 1.0)),
            (90, off(1, 1.0)),
            (127, on(LIVE_NOTES + 1, 61, 1.0)),
        ]
    );
}

#[test]
fn a_swap_keeps_notes_held_on_the_same_input_and_ends_the_rest() {
    let from = |input| {
        let source = MidiSource {
            input,
            channel: None,
        };
        MidiMonitor::new(Monitor::In, false).with_source(source)
    };
    let mut rig = Rig::new(from(Some(0)), Vec::new());
    rig.block(&[message(10, 0, [0x90, 60, 127])]);
    assert_eq!(rig.kept(), [(10, on(LIVE_NOTES, 60, 1.0))]);
    // On the same input, the note plays on until it's let go.
    rig.swap(from(Some(0)));
    rig.block(&[message(70, 0, [0x80, 60, 127])]);
    assert_eq!(rig.kept(), [(70, off(LIVE_NOTES, 1.0))]);

    // On another, the notes held end as the new graph starts, and new
    // notes take new ids.
    rig.block(&[message(130, 0, [0x90, 62, 127])]);
    assert_eq!(rig.kept(), [(130, on(LIVE_NOTES + 1, 62, 1.0))]);
    rig.swap(from(Some(1)));
    rig.block(&[
        message(200, 0, [0x80, 62, 127]),
        message(210, 1, [0x90, 64, 127]),
    ]);
    assert_eq!(
        rig.kept(),
        [
            (192, off(LIVE_NOTES + 1, RELEASE)),
            (210, on(LIVE_NOTES + 2, 64, 1.0))
        ]
    );

    // Two swaps before a block still end them.
    rig.block(&[message(260, 1, [0x90, 65, 127])]);
    assert_eq!(rig.kept(), [(260, on(LIVE_NOTES + 3, 65, 1.0))]);
    rig.swap(from(Some(0)));
    rig.swap(from(None));
    rig.block(&[message(330, 2, [0x90, 67, 127])]);
    assert_eq!(
        rig.kept(),
        [
            (320, off(LIVE_NOTES + 2, RELEASE)),
            (320, off(LIVE_NOTES + 3, RELEASE)),
            (330, on(LIVE_NOTES + 4, 67, 1.0))
        ]
    );
}

/// A controller's move, as a clip player sends it.
fn controller(controller: Controller, value: f32) -> NodeEventKind {
    NodeEventKind::Controller { controller, value }
}

#[test]
fn controllers_go_on_from_the_input_and_the_clips_as_their_notes_do() {
    let (wheel, pedal) = (Controller::MOD_WHEEL, Controller::SUSTAIN);
    let clips = vec![(5, controller(wheel, 0.5)), (30, controller(pedal, 1.0))];
    let midi = [
        message(10, 0, [0xB0, 64, 127]),
        message(40, 0, [0xB0, 1, 0]),
    ];
    // The input with In, the clips with Off, and both armed with Auto.
    for (mode, armed, input, clip) in [
        (Monitor::In, false, true, false),
        (Monitor::Off, true, false, true),
        (Monitor::Auto, true, true, true),
    ] {
        let mut rig = Rig::with_clips(monitor(mode, armed), clips.clone());
        rig.transport = playing(false);
        rig.block(&midi);
        let mut expected = Vec::new();
        if clip {
            expected.push((5, wheel, 0.5));
        }
        if input {
            expected.push((10, pedal, 1.0));
        }
        if clip {
            expected.push((30, pedal, 1.0));
        }
        if input {
            expected.push((40, wheel, 0.0));
        }
        assert_eq!(rig.moved(), expected, "{mode:?}");
    }
}

#[test]
fn switches_held_are_let_go_when_what_holds_them_stops_playing() {
    let (wheel, pedal) = (Controller::MOD_WHEEL, Controller::SUSTAIN);
    // The input's pedal, down as the track is disarmed, which takes effect
    // from the next block. Its mod wheel stays where it was.
    let mut rig = Rig::new(monitor(Monitor::Auto, true), Vec::new());
    rig.block(&[
        message(10, 0, [0xB0, 64, 127]),
        message(20, 0, [0xB0, 1, 127]),
    ]);
    rig.set(MidiMonitor::ARMED, 0.0);
    rig.block(&[]);
    rig.block(&[]);
    assert_eq!(
        rig.moved(),
        [(10, pedal, 1.0), (20, wheel, 1.0), (128, pedal, 0.0)]
    );

    // The clips' pedal, down as recording starts over them. Once it stops,
    // the clips' controllers go back to what the clips hold, through a
    // swap too.
    let clips = vec![
        (10, controller(pedal, 1.0)),
        (20, controller(wheel, 0.25)),
        (100, controller(wheel, 0.75)),
    ];
    let mut rig = Rig::with_clips(monitor(Monitor::Auto, true), clips);
    rig.transport = playing(false);
    rig.block(&[]);
    rig.transport = playing(true);
    rig.block(&[]);
    assert_eq!(
        rig.moved(),
        [(10, pedal, 1.0), (20, wheel, 0.25), (64, pedal, 0.0)]
    );
    rig.swap(monitor(Monitor::Auto, true));
    rig.transport = playing(false);
    rig.block(&[]);
    assert_eq!(rig.moved(), [(128, wheel, 0.75), (128, pedal, 1.0)]);

    // A pedal held on an input the track no longer takes is let go as the
    // new graph starts, as its notes end.
    let from = |input| {
        let source = MidiSource {
            input,
            channel: None,
        };
        MidiMonitor::new(Monitor::In, false).with_source(source)
    };
    let mut rig = Rig::new(from(Some(0)), Vec::new());
    rig.block(&[message(10, 0, [0xB0, 64, 127])]);
    assert_eq!(rig.moved(), [(10, pedal, 1.0)]);
    rig.swap(from(Some(1)));
    rig.block(&[]);
    assert_eq!(rig.moved(), [(64, pedal, 0.0)]);
}

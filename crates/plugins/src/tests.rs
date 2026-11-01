//! CLAP hosting, tested against the test plugins (`crates/test-plugins`),
//! which count every time the host breaks one of CLAP's rules, and what the
//! VST3 tests share with these.

use std::f64::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use clap_sys::events::{
    CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_IS_LOOP_ACTIVE,
    CLAP_TRANSPORT_IS_PLAYING, CLAP_TRANSPORT_IS_RECORDING,
};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use musicable_engine::nodes::Sum;
use musicable_engine::switch::{NoteSwitch, Switch};
use musicable_engine::{
    AudioMut, AudioRef, Engine, EngineConfig, Event, Graph, InputPort, Inputs, Node, NodeEvent,
    NodeEventKind, NodeKey, NoteEvent, NoteId, ParallelOptions, ParallelRenderer, ParamId,
    ProcessContext, TransportState, rt,
};
use musicable_model::clip::{Controller, Dimension, Note};
use musicable_model::plugin::{PluginInput, PluginKind};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_test_plugins::testing::{self, Library, Lifecycle};
use musicable_test_plugins::{
    ARP_STEP, ARPEGGIATOR_ID, BYPASS, COST, GAIN, GAIN_ID, JITTER, LATENCY, LEVEL, LOAD_ID,
    POSITION, SIDECHAIN, SYNTH_ID, THREADS, TRANSPOSE, gui,
};

use crate::clap::Instance;
use crate::scan::{self, Outcome};
use crate::{ParentWindow, PluginEvent, PluginInfo};

pub(crate) const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// A directory of its own for a test to write in.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "musicable-plugins-{}-{name}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of the test plugins, with the hooks tests drive them with.
struct TestPlugins {
    path: PathBuf,
    library: Library,
}

impl TestPlugins {
    /// A copy of the library named `name.clap`, so the test gets the
    /// library's statics to itself.
    fn new(name: &str) -> Self {
        let library = Library::copy(&temp_dir(name), name);
        Self {
            path: library.path().to_owned(),
            library,
        }
    }

    fn assert_alright(&self) {
        self.library.assert_alright();
    }

    // SAFETY, for the hooks below: each takes the instance's plugin, which
    // lives as long as the instance, on its main thread.

    fn gui(&self, instance: &Instance, action: u32, param: u32, value: f64) {
        // SAFETY: as above.
        unsafe { self.library.gui(instance.raw(), action, param, value) };
    }

    fn set_label(&self, instance: &Instance, label: &str) {
        // SAFETY: as above.
        unsafe { self.library.set_label(instance.raw(), label) };
    }

    fn label(&self, instance: &Instance) -> String {
        // SAFETY: as above.
        unsafe { self.library.label(instance.raw()) }
    }

    fn request_callback(&self, instance: &Instance) {
        // SAFETY: as above.
        unsafe { self.library.request_callback(instance.raw()) };
    }

    fn main_thread_calls(&self, instance: &Instance) -> u32 {
        // SAFETY: as above.
        unsafe { self.library.main_thread_calls(instance.raw()) }
    }

    /// The pretend window: -1 for none, 0 hidden, 1 showing, and its
    /// parent.
    fn window(&self, instance: &Instance) -> (i32, usize) {
        // SAFETY: as above.
        unsafe { self.library.window(instance.raw()) }
    }

    fn request_resize(&self, instance: &Instance, width: u32, height: u32) -> bool {
        // SAFETY: as above.
        unsafe { self.library.request_resize(instance.raw(), width, height) }
    }

    /// How many times the plugin had the host run its tasks, and on how
    /// many threads they ran.
    fn pool_use(&self, instance: &Instance) -> (u32, u32) {
        // SAFETY: as above.
        unsafe { self.library.pool_use(instance.raw()) }
    }

    /// The transport's flags, position in beats and tempo, as the plugin
    /// last saw them.
    fn transport(&self, instance: &Instance) -> Option<(u32, i64, f64)> {
        // SAFETY: as above.
        unsafe { self.library.transport(instance.raw()) }
    }

    /// The MIDI messages the synth got since this was last asked.
    fn midi(&self, instance: &Instance) -> Vec<[u8; 3]> {
        // SAFETY: as above, between blocks.
        unsafe { self.library.midi(instance.raw()) }
    }

    fn lifecycle(&self, instance: &Instance) -> Lifecycle {
        // SAFETY: as above.
        unsafe { self.library.lifecycle(instance.raw()) }
    }

    /// Whether the gain's sidechain is on, as the plugin has it.
    fn sidechain(&self, instance: &Instance) -> bool {
        // SAFETY: as above.
        unsafe { self.library.sidechain(instance.raw()) }
    }

    fn gain(&self) -> Instance {
        Instance::new(&self.path, GAIN_ID.to_str().unwrap(), None).unwrap()
    }

    fn synth(&self) -> Instance {
        Instance::new(&self.path, SYNTH_ID.to_str().unwrap(), None).unwrap()
    }

    fn arpeggiator(&self) -> Instance {
        Instance::new(&self.path, ARPEGGIATOR_ID.to_str().unwrap(), None).unwrap()
    }

    fn load(&self) -> Instance {
        Instance::new(&self.path, LOAD_ID.to_str().unwrap(), None).unwrap()
    }
}

/// What a block is played with.
#[derive(Default)]
pub(crate) struct Block<'a> {
    /// The engine time of its first frame.
    pub(crate) time: u64,
    pub(crate) transport: TransportState,
    pub(crate) tempo: TempoMap,
    pub(crate) events: &'a [NodeEvent],
    /// Whether it renders ahead of the device.
    pub(crate) ahead: bool,
}

/// Plays one block of `node`, with `input` on the left and its negative on
/// the right, and returns both channels. It mustn't allocate.
pub(crate) fn play(node: &mut impl Node, input: &[f32], block: &Block<'_>) -> [Vec<f32>; 2] {
    play_keyed(node, input, None, block)
}

/// Plays one block of `node` as [`play`] does, with `key`, if there is one,
/// into side input `side` too, in mono.
pub(crate) fn play_keyed(
    node: &mut impl Node,
    input: &[f32],
    key: Option<(u32, &[f32])>,
    block: &Block<'_>,
) -> [Vec<f32>; 2] {
    let frames = input.len();
    let planar_in: Vec<f32> = input
        .iter()
        .copied()
        .chain(input.iter().map(|s| -s))
        .collect();
    let (side, key) = key.unwrap_or((0, &[]));
    assert!(
        key.is_empty() || key.len() == frames,
        "a key as long as the block"
    );
    let mut planar_out = vec![f32::NAN; frames * 2];
    let ctx = ProcessContext {
        frames,
        sample_rate: CONFIG.sample_rate,
        time: block.time,
        transport: block.transport,
        tempo: &block.tempo,
        events: block.events,
        input: AudioRef::empty(frames),
        midi: &[],
        ahead: block.ahead,
    };
    {
        let views = [
            AudioRef::from_planar(&planar_in, 2),
            AudioRef::from_planar(key, 1),
        ];
        let ports = [InputPort::Main, InputPort::Side(side)];
        let inputs = if key.is_empty() {
            Inputs::from_views(&views[..1])
        } else {
            Inputs::from_views_on(&views, &ports)
        };
        let mut output = AudioMut::from_planar(&mut planar_out, 2);
        rt::scope(|| node.process(&ctx, inputs, &mut output));
    }
    let right = planar_out.split_off(frames);
    [planar_out, right]
}

pub(crate) fn ramp(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (i + 1) as f32 / frames as f32)
        .collect()
}

pub(crate) fn scaled(signal: &[f32], by: f32) -> Vec<f32> {
    signal.iter().map(|s| s * by).collect()
}

pub(crate) fn param(offset: usize, param: u32, value: f64) -> NodeEvent {
    NodeEvent {
        offset,
        kind: NodeEventKind::Param {
            param: ParamId(param.into()),
            value,
        },
    }
}

pub(crate) fn modulation(
    offset: usize,
    param: u32,
    amount: f64,
    note: Option<NoteId>,
) -> NodeEvent {
    NodeEvent {
        offset,
        kind: NodeEventKind::Modulation {
            param: ParamId(param.into()),
            amount,
            note,
        },
    }
}

pub(crate) fn note(offset: usize, event: NoteEvent) -> NodeEvent {
    NodeEvent {
        offset,
        kind: NodeEventKind::Note(event),
    }
}

pub(crate) fn controller(offset: usize, controller: Controller, value: f32) -> NodeEvent {
    controller_on(offset, controller, value, None)
}

/// A controller's move on `channel`, as a note wire with one plays it.
pub(crate) fn controller_on(
    offset: usize,
    controller: Controller,
    value: f32,
    channel: Option<u8>,
) -> NodeEvent {
    NodeEvent {
        offset,
        kind: NodeEventKind::Controller {
            controller,
            value,
            channel,
        },
    }
}

pub(crate) fn assert_close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((a - e).abs() < 1e-4, "frame {i}: {a} isn't {e}");
    }
}

#[test]
fn the_gain_plugin_describes_its_parameters() {
    let plugins = TestPlugins::new("params");
    let instance = plugins.gain();
    assert_eq!(instance.info().name, "Test Gain");
    assert_eq!(instance.info().kind(), PluginKind::AudioEffect);
    let params = instance.params();
    assert_eq!(
        params.iter().map(|p| p.id).collect::<Vec<_>>(),
        [GAIN, LATENCY, BYPASS]
    );
    let gain = &params[0];
    assert_eq!(
        (
            gain.name.as_str(),
            gain.module.as_str(),
            gain.min,
            gain.max,
            gain.default
        ),
        ("Gain", "Test", 0.0, 2.0, 1.0)
    );
    assert!(gain.automatable && gain.modulatable && gain.steps == 0);
    assert!(!gain.modulatable_per_note);
    assert!(params[1].steps == 48_000 && !params[1].automatable);
    assert!(!params[0].bypass && !params[1].bypass);
    assert!(params[2].bypass && params[2].steps == 1 && params[2].automatable);
    assert_eq!(instance.param_value(GAIN), Some(1.0));
    assert_eq!(instance.param_value(99), None);
    assert_eq!(
        instance.value_to_text(LATENCY, 64.0).as_deref(),
        Some("64 samples")
    );
    assert_eq!(instance.text_to_value(GAIN, "0.25"), Some(0.25));
    assert_eq!(instance.text_to_value(GAIN, "loud"), None);
    assert_eq!(instance.value_to_text(BYPASS, 1.0).as_deref(), Some("On"));
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn the_gain_plugin_plays_its_input() {
    let plugins = TestPlugins::new("gain");
    let mut instance = plugins.gain();
    let mut node = instance.node_source().node();
    let input = ramp(64);
    let silence = vec![0.0; 64];
    assert_eq!(
        play(&mut node, &input, &Block::default()),
        [silence.clone(), silence]
    );

    instance.activate(CONFIG).unwrap();
    assert!(instance.is_active());
    let [left, right] = play(&mut node, &input, &Block::default());
    assert_eq!((left, right), (input.clone(), scaled(&input, -1.0)));

    // Changes from the main thread apply from the next block's start, and
    // the node's events from their offsets.
    instance.set_param(GAIN, 0.5);
    let [left, _] = play(&mut node, &input, &Block::default());
    assert_eq!(left, scaled(&input, 0.5));
    assert_eq!(instance.param_value(GAIN), Some(0.5));
    let events = [param(10, GAIN, 2.0)];
    let block = Block {
        events: &events,
        ..Block::default()
    };
    let [left, _] = play(&mut node, &input, &block);
    assert_eq!(left[..10], scaled(&input[..10], 0.5));
    assert_eq!(left[10..], scaled(&input[10..], 2.0));

    // Blocks longer than the plugin was activated for play silence.
    assert_eq!(
        play(&mut node, &ramp(257), &Block::default())[0],
        vec![0.0; 257]
    );
    instance.deactivate().unwrap();
    assert_eq!(play(&mut node, &input, &Block::default())[0], vec![0.0; 64]);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn modulation_offsets_a_parameter_and_leaves_its_value() {
    let plugins = TestPlugins::new("modulation");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let input = ramp(64);
    let block = |events| Block {
        events,
        ..Block::default()
    };
    let events = [modulation(16, GAIN, 0.5, None)];
    let [left, _] = play(&mut node, &input, &block(&events));
    assert_eq!(left[..16], input[..16]);
    assert_eq!(left[16..], scaled(&input[16..], 1.5));

    // It holds until the next modulation of the parameter, which replaces
    // it, and the value stays as it was.
    assert_eq!(play(&mut node, &input, &block(&[]))[0], scaled(&input, 1.5));
    let events = [modulation(0, GAIN, -0.25, None)];
    assert_eq!(
        play(&mut node, &input, &block(&events))[0],
        scaled(&input, 0.75)
    );
    assert_eq!(instance.param_value(GAIN), Some(1.0));

    // An effect plays no notes, so modulation for a note goes nowhere.
    let events = [modulation(0, GAIN, 1.0, Some(NoteId(1)))];
    assert_eq!(
        play(&mut node, &input, &block(&events))[0],
        scaled(&input, 0.75)
    );
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn changes_to_an_inactive_plugin_apply_at_once() {
    let plugins = TestPlugins::new("inactive");
    let mut instance = plugins.gain();
    instance.set_param(GAIN, 0.25);
    instance.set_param(LATENCY, 32.0);
    assert_eq!(instance.param_value(GAIN), Some(0.25));
    instance.activate(CONFIG).unwrap();
    assert_eq!(instance.latency(), 32);
    // Changes waiting for a node when the plugin deactivates get to it.
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(16), &Block::default());
    instance.set_param(GAIN, 0.75);
    instance.deactivate().unwrap();
    assert_eq!(instance.param_value(GAIN), Some(0.75));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn an_active_plugin_no_node_processes_trades_changes_at_once() {
    // As when there's no audio device: active, and never processed.
    let plugins = TestPlugins::new("unprocessed");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    instance.set_param(GAIN, 0.5);
    assert_eq!(instance.param_value(GAIN), Some(0.5));
    plugins.gui(&instance, gui::BEGIN, GAIN, 0.0);
    plugins.gui(&instance, gui::VALUE, GAIN, 0.25);
    plugins.gui(&instance, gui::END, GAIN, 0.0);
    assert_eq!(
        instance.poll(),
        [
            PluginEvent::GestureBegin { param: GAIN },
            PluginEvent::ParamChanged {
                param: GAIN,
                value: 0.25
            },
            PluginEvent::GestureEnd { param: GAIN },
        ]
    );
    // A node that starts it later plays it as it is.
    let mut node = instance.node_source().node();
    let input = ramp(16);
    assert_eq!(
        play(&mut node, &input, &Block::default())[0],
        scaled(&input, 0.25)
    );
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn a_latency_change_restarts_the_plugin() {
    let plugins = TestPlugins::new("latency");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    assert_eq!((instance.latency(), node.latency()), (0, 0));
    instance.set_param(LATENCY, 100.0);
    play(&mut node, &ramp(64), &Block::default());
    assert_eq!(
        instance.poll(),
        [PluginEvent::Restarted {
            latency_changed: true
        }]
    );
    assert_eq!((instance.latency(), node.latency()), (100, 100));
    let mut impulse = vec![0.0; 256];
    impulse[0] = 1.0;
    let [left, _] = play(&mut node, &impulse, &Block::default());
    let peaks: Vec<_> = left
        .iter()
        .enumerate()
        .filter(|(_, s)| **s != 0.0)
        .collect();
    assert_eq!(peaks, [(100, &1.0)]);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn a_latency_set_before_the_plugin_activates_needs_no_restart() {
    // The test gain asks for a restart when it's given another latency,
    // even while it's inactive, which activating it does anyway.
    let plugins = TestPlugins::new("latency-first");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, 100.0);
    instance.activate(CONFIG).unwrap();
    assert_eq!(instance.latency(), 100);
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(64), &Block::default());
    assert_eq!(instance.poll(), []);
    drop((node, instance));
    plugins.assert_alright();
}

/// `input` ducked by `key`, as the test gains duck: each sample times one
/// less the key's size.
pub(crate) fn ducked(input: &[f32], key: &[f32]) -> Vec<f32> {
    input.iter().zip(key).map(|(x, k)| x * (1.0 - k)).collect()
}

#[test]
fn a_side_input_switches_on_with_a_restart_as_it_is_wired() {
    let plugins = TestPlugins::new("sidechain");
    let mut instance = plugins.gain();
    let sidechain = PluginInput {
        id: SIDECHAIN,
        name: "Sidechain".into(),
    };
    assert_eq!(instance.side_inputs(), [sidechain]);
    // Inactive, it takes the side inputs as it activates.
    assert_eq!(instance.set_side_inputs(&[SIDECHAIN]), Ok(false));
    assert!(plugins.sidechain(&instance), "on, as CLAP has it at first");
    assert_eq!(instance.set_side_inputs(&[]), Ok(false));
    instance.activate(CONFIG).unwrap();
    assert!(!plugins.sidechain(&instance), "off with nothing wired");
    let mut node = instance.node_source().node();
    let (input, key) = (vec![0.5; 64], ramp(64));
    let keyed = Some((SIDECHAIN, key.as_slice()));
    // Off, it hears nothing of its side input.
    assert_eq!(
        play_keyed(&mut node, &input, keyed, &Block::default())[0],
        input
    );
    assert_eq!(plugins.lifecycle(&instance).starts, 1);

    // Wired, it restarts with the side input on, which ducks it.
    assert_eq!(instance.set_side_inputs(&[SIDECHAIN, 99]), Ok(true));
    assert!(plugins.sidechain(&instance));
    assert_eq!(instance.set_side_inputs(&[99, SIDECHAIN]), Ok(false));
    let [left, right] = play_keyed(&mut node, &input, keyed, &Block::default());
    assert_close(&left, &ducked(&input, &key));
    assert_close(&right, &scaled(&ducked(&input, &key), -1.0));
    assert_eq!(plugins.lifecycle(&instance).starts, 2, "it restarted");
    // Its main input still plays without a key.
    assert_eq!(play(&mut node, &input, &Block::default())[0], input);

    // Unwired, it restarts with it off again.
    assert_eq!(instance.set_side_inputs(&[]), Ok(true));
    assert!(!plugins.sidechain(&instance));
    assert_eq!(
        play_keyed(&mut node, &input, keyed, &Block::default())[0],
        input
    );
    assert_eq!(plugins.lifecycle(&instance).starts, 3);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn a_side_input_the_plugin_cant_switch_gets_only_what_is_wired() {
    // This copy's gain has no audio-ports-activation: its sidechain is on
    // all along, as many plugins' are.
    let plugins = TestPlugins::new("fixed-ports");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let (input, key) = (vec![0.5; 64], ramp(64));
    let keyed = Some((SIDECHAIN, key.as_slice()));
    assert_eq!(
        play_keyed(&mut node, &input, keyed, &Block::default())[0],
        input
    );
    assert_eq!(instance.set_side_inputs(&[SIDECHAIN]), Ok(true));
    let [left, _] = play_keyed(&mut node, &input, keyed, &Block::default());
    assert_close(&left, &ducked(&input, &key));
    assert_eq!(instance.set_side_inputs(&[]), Ok(true));
    assert_eq!(
        play_keyed(&mut node, &input, keyed, &Block::default())[0],
        input
    );
    assert!(plugins.sidechain(&instance), "never switched");
    // The synth has no side inputs.
    let mut synth = plugins.synth();
    assert_eq!(synth.side_inputs(), []);
    assert_eq!(synth.set_side_inputs(&[SIDECHAIN]), Ok(false));
    drop((node, instance, synth));
    plugins.assert_alright();
}

#[test]
fn a_latent_plugin_hears_its_side_input_as_late_as_its_input() {
    let plugins = TestPlugins::new("sidechain-latency");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, 37.0);
    instance.set_side_inputs(&[SIDECHAIN]).unwrap();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let input = vec![0.5; 256];
    let mut key = vec![0.0; 256];
    key[100..110].fill(1.0);
    let [left, _] = play_keyed(
        &mut node,
        &input,
        Some((SIDECHAIN, &key)),
        &Block::default(),
    );
    let mut expected = vec![0.5; 256];
    expected[..37].fill(0.0);
    expected[137..147].fill(0.0);
    assert_eq!(left, expected);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn delay_compensation_lines_a_latent_plugin_up_with_a_dry_path() {
    let plugins = TestPlugins::new("compensation");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, 123.0);
    instance.activate(CONFIG).unwrap();
    let mut g = Graph::new();
    let source = g.add(NodeKey(0), Impulse { at: 10 });
    let wet = g.add(NodeKey(1), instance.node_source().node());
    let sum = g.add(NodeKey(2), Sum::new(2));
    g.connect(source, wet).unwrap();
    g.connect(wet, sum).unwrap();
    g.connect(source, sum).unwrap();
    g.set_output(sum).unwrap();
    let graph = g.prepare(&CONFIG).unwrap();
    assert_eq!(graph.latency(), 123);
    let (mut engine, mut handle) = Engine::new(CONFIG);
    handle.swap_graph(graph).unwrap();
    let mut output = Vec::new();
    for _ in 0..4 {
        let mut block = vec![0.0; 2 * 256];
        engine.process(&mut AudioMut::from_planar(&mut block, 2));
        output.extend_from_slice(&block[..256]);
    }
    assert_eq!(engine.stats().latency, 123);
    let peaks: Vec<_> = output
        .iter()
        .enumerate()
        .filter(|(_, s)| **s != 0.0)
        .collect();
    assert_eq!(peaks, [(133, &2.0)], "both paths, to the sample");
    drop((engine, handle));
    drop(instance);
    plugins.assert_alright();
}

/// A single 1.0 at engine time `at`.
pub(crate) struct Impulse {
    pub(crate) at: u64,
}

impl Node for Impulse {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        if let Some(offset) = self.at.checked_sub(ctx.time)
            && offset < ctx.frames as u64
        {
            for channel in output.channels_mut() {
                channel[offset as usize] = 1.0;
            }
        }
    }
}

#[test]
fn state_saves_and_loads() {
    let plugins = TestPlugins::new("state");
    let mut first = plugins.gain();
    first.set_param(GAIN, 0.75);
    plugins.set_label(&first, "warm");
    assert_eq!(first.poll(), [PluginEvent::StateChanged]);
    let state = first.save_state().unwrap();
    assert!(String::from_utf8_lossy(&state).contains("label=warm"));
    let mut second = plugins.gain();
    second.load_state(&state).unwrap();
    assert_eq!(second.param_value(GAIN), Some(0.75));
    assert_eq!(plugins.label(&second), "warm");
    assert!(second.load_state(b"nonsense").is_err());
    drop((first, second));
    plugins.assert_alright();
}

#[test]
fn changes_in_the_plugins_window_come_back() {
    let plugins = TestPlugins::new("window-changes");
    let mut instance = plugins.gain();
    // Inactive, the plugin is flushed on the main thread.
    plugins.gui(&instance, gui::BEGIN, GAIN, 0.0);
    plugins.gui(&instance, gui::VALUE, GAIN, 0.3);
    plugins.gui(&instance, gui::END, GAIN, 0.0);
    assert_eq!(
        instance.poll(),
        [
            PluginEvent::GestureBegin { param: GAIN },
            PluginEvent::ParamChanged {
                param: GAIN,
                value: 0.3
            },
            PluginEvent::GestureEnd { param: GAIN },
        ]
    );
    // Processed, the changes come out of processing.
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(16), &Block::default());
    plugins.gui(&instance, gui::VALUE, GAIN, 1.5);
    assert_eq!(instance.poll(), []);
    play(&mut node, &ramp(16), &Block::default());
    assert_eq!(
        instance.poll(),
        [PluginEvent::ParamChanged {
            param: GAIN,
            value: 1.5
        }]
    );
    let input = ramp(16);
    assert_eq!(
        play(&mut node, &input, &Block::default())[0],
        scaled(&input, 1.5)
    );
    drop((node, instance));
    plugins.assert_alright();
}

/// Bypasses the device in a switch, as its lane or the project does, or
/// brings it back.
pub(crate) fn bypass(offset: usize, bypassed: bool) -> NodeEvent {
    let bypass = u32::try_from(Switch::BYPASS.0).expect("a plugin's ids are 32 bits");
    param(offset, bypass, if bypassed { 1.0 } else { 0.0 })
}

/// Plays `signal` through `node` in blocks of 256 frames, from engine time
/// 0, with the events `events` gives each block. After each block,
/// `between` gets its index. Returns the left channel.
pub(crate) fn play_blocks(
    node: &mut impl Node,
    signal: &[f32],
    mut events: impl FnMut(usize) -> Vec<NodeEvent>,
    mut between: impl FnMut(usize),
) -> Vec<f32> {
    let mut out = Vec::with_capacity(signal.len());
    for (i, input) in signal.chunks(256).enumerate() {
        let events = events(i);
        let block = Block {
            time: (i * 256) as u64,
            events: &events,
            ..Block::default()
        };
        let [left, _] = play(node, input, &block);
        out.extend(left);
        between(i);
    }
    out
}

/// What a switch round a device 32 frames late plays as the device is
/// bypassed at frame 256 and brought back at frame 1280: the device's
/// output at gain `before`, a crossfade from 288 to 768 to `signal` 32
/// frames late, and from 1312 to 1792 back to the device at gain `after`.
pub(crate) fn bypassed_at_256_and_back_at_1280(
    signal: &[f32],
    before: f32,
    after: f32,
) -> Vec<f32> {
    let late = |t: usize| if t < 32 { 0.0 } else { signal[t - 32] };
    (0..signal.len())
        .map(|t| {
            let dry = late(t);
            match t {
                ..288 => dry * before,
                288..768 => {
                    let m = (t - 287) as f32 / 480.0;
                    dry * before * (1.0 - m) + dry * m
                }
                768..1312 => dry,
                1312..1792 => {
                    let m = (1791 - t) as f32 / 480.0;
                    dry * after * (1.0 - m) + dry * m
                }
                _ => dry * after,
            }
        })
        .collect()
}

#[test]
fn a_bypassed_plugin_stops_and_starts_afresh_as_it_comes_back() {
    let plugins = TestPlugins::new("bypassed");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, 32.0);
    instance.activate(CONFIG).unwrap();
    instance.set_param(GAIN, 0.5);
    let mut switch = Switch::new(Box::new(instance.node_source().node()), false);
    switch.prepare(&CONFIG);
    assert_eq!(switch.latency(), 32, "a bypassed plugin keeps its latency");
    let signal = ramp(256 * 8);
    let mut lifecycle = Vec::new();
    let mut running = Vec::new();
    assert!(!instance.is_running());
    let events = |i| match i {
        1 => vec![bypass(0, true)],
        // Stopped, it takes the changes its parameters get.
        4 => vec![param(0, GAIN, 2.0)],
        5 => vec![bypass(0, false)],
        _ => Vec::new(),
    };
    let out = play_blocks(&mut switch, &signal, events, |i| {
        if i == 4 {
            assert_eq!(instance.param_value(GAIN), Some(2.0));
            // And the main thread's, and the changes in its window come
            // back.
            instance.set_param(GAIN, 0.25);
            assert_eq!(instance.param_value(GAIN), Some(0.25));
            plugins.gui(&instance, gui::VALUE, BYPASS, 1.0);
            assert_eq!(
                instance.poll(),
                [PluginEvent::ParamChanged {
                    param: BYPASS,
                    value: 1.0
                }]
            );
            plugins.gui(&instance, gui::VALUE, BYPASS, 0.0);
            instance.poll();
        }
        lifecycle.push(plugins.lifecycle(&instance));
        running.push(instance.is_running());
    });
    assert_close(&out, &bypassed_at_256_and_back_at_1280(&signal, 0.5, 0.25));
    assert_eq!(running, [true, true, false, false, false, true, true, true]);
    // It stops once the fade is over, and processes nothing until it comes
    // back, then it starts afresh.
    assert_eq!((lifecycle[1].stops, lifecycle[2].stops), (0, 1));
    assert_eq!(lifecycle[2].blocks, lifecycle[4].blocks);
    assert_eq!(
        (
            lifecycle[4].starts,
            lifecycle[4].resets,
            lifecycle[5].starts,
            lifecycle[5].resets
        ),
        (1, 0, 2, 1)
    );
    assert_eq!(lifecycle[7].stops, 1);
    drop((switch, instance));
    plugins.assert_alright();
}

#[test]
fn a_deactivated_plugin_stops_and_still_takes_its_parameters() {
    let plugins = TestPlugins::new("deactivated");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut playing = Switch::new(Box::new(instance.node_source().node()), false);
    playing.prepare(&CONFIG);
    let input = ramp(256);
    play(&mut playing, &input, &Block::default());
    // The graph for the project with it deactivated takes over.
    let mut deactivated = Switch::deactivated(Box::new(instance.node_source().node()));
    deactivated.prepare(&CONFIG);
    deactivated.adopt(&mut playing);
    assert_eq!(plugins.lifecycle(&instance).stops, 1);
    let events = [param(0, GAIN, 0.5)];
    let block = Block {
        time: 256,
        events: &events,
        ..Block::default()
    };
    assert_eq!(play(&mut deactivated, &input, &block)[0], vec![0.0; 256]);
    assert_eq!(instance.param_value(GAIN), Some(0.5));
    // Active again, it starts afresh, at once.
    let mut active = Switch::new(Box::new(instance.node_source().node()), false);
    active.prepare(&CONFIG);
    active.adopt(&mut deactivated);
    let block = Block {
        time: 512,
        ..Block::default()
    };
    assert_eq!(play(&mut active, &input, &block)[0], scaled(&input, 0.5));
    assert_eq!(
        plugins.lifecycle(&instance),
        Lifecycle {
            starts: 2,
            stops: 1,
            resets: 1,
            blocks: 2
        }
    );
    drop((playing, deactivated, active, instance));
    plugins.assert_alright();
}

/// What the test synth plays for a note at `frequency` and `amplitude`,
/// from `phase` in cycles, for `frames` frames, and the phase it ends on.
pub(crate) fn sine(frequency: f64, amplitude: f64, phase: f64, frames: usize) -> (Vec<f32>, f64) {
    let mut phase = phase;
    let samples = (0..frames)
        .map(|_| {
            let sample = (amplitude * (phase * TAU).sin()) as f32;
            phase = (phase + frequency / f64::from(CONFIG.sample_rate)).fract();
            sample
        })
        .collect();
    (samples, phase)
}

#[test]
fn the_synth_plays_notes_with_their_expression() {
    let plugins = TestPlugins::new("synth");
    let mut instance = plugins.synth();
    assert_eq!(instance.info().kind(), PluginKind::Instrument);
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let silence = vec![0.0; 128];
    let a = NoteId(7);
    let on = [note(
        0,
        NoteEvent::On {
            note: a,
            key: 69,
            velocity: 1.0,
            channel: None,
        },
    )];
    let block = |events| Block {
        events,
        ..Block::default()
    };
    // Level 0.5, velocity 1, A at 440 Hz.
    let [left, right] = play(&mut node, &silence, &block(&on));
    let (expected, phase) = sine(440.0, 0.5, 0.0, 128);
    assert_close(&left, &expected);
    assert_close(&right, &expected);

    // Bent up an octave, and pressed as hard as it goes.
    let expression = [
        note(
            0,
            NoteEvent::Expression {
                note: a,
                dimension: Dimension::PitchBend,
                value: 12.0,
            },
        ),
        note(
            0,
            NoteEvent::Expression {
                note: a,
                dimension: Dimension::Pressure,
                value: 1.0,
            },
        ),
    ];
    let [left, _] = play(&mut node, &silence, &block(&expression));
    assert_close(&left, &sine(880.0, 1.0, phase, 128).0);

    // A second note on the same key is a note of its own, which lets go
    // separately.
    let b = NoteId(8);
    let more = [
        note(
            0,
            NoteEvent::On {
                note: b,
                key: 69,
                velocity: 0.5,
                channel: None,
            },
        ),
        note(
            64,
            NoteEvent::Off {
                note: a,
                velocity: 0.0,
            },
        ),
    ];
    let [left, _] = play(&mut node, &silence, &block(&more));
    let (second, _) = sine(440.0, 0.25, 0.0, 128);
    assert_close(&left[64..], &second[64..]);
    let off = [note(
        0,
        NoteEvent::Off {
            note: b,
            velocity: 0.0,
        },
    )];
    assert_eq!(play(&mut node, &silence, &block(&off))[0], silence);
    drop((node, instance));
    plugins.assert_alright();
}

/// Two of the synth's notes on the same key, A at 440 Hz, from the block's
/// first frame: `a` at velocity 1 and `b` at velocity 0.5. Only their ids
/// tell them apart.
fn two_notes(a: NoteId, b: NoteId) -> [NodeEvent; 2] {
    let on = |note, velocity| {
        self::note(
            0,
            NoteEvent::On {
                note,
                key: 69,
                velocity,
                channel: None,
            },
        )
    };
    [on(a, 1.0), on(b, 0.5)]
}

#[test]
fn the_synth_modulates_each_notes_voice_on_its_own() {
    let plugins = TestPlugins::new("voice modulation");
    let mut instance = plugins.synth();
    let level = &instance.params()[0];
    assert_eq!(level.id, LEVEL);
    assert!(level.modulatable && level.modulatable_per_note);
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let silence = vec![0.0; 128];
    let block = |events| Block {
        events,
        ..Block::default()
    };
    // The voices play the same sine, each as loud as its velocity times the
    // level, 0.5, and whatever modulates it.
    let (a, b) = (NoteId(1), NoteId(2));
    let [on_a, on_b] = two_notes(a, b);
    let events = [on_a, on_b, modulation(0, LEVEL, 0.25, Some(a))];
    let [left, _] = play(&mut node, &silence, &block(&events));
    let (expected, phase) = sine(440.0, 0.75 + 0.25, 0.0, 128);
    assert_close(&left, &expected);

    // Modulation for every voice adds to each voice's own, and a note's
    // next modulation replaces its last. A note that isn't playing gets
    // none.
    let events = [
        modulation(0, LEVEL, 0.25, None),
        modulation(0, LEVEL, -0.25, Some(b)),
        modulation(0, LEVEL, 0.5, Some(NoteId(3))),
    ];
    let [left, _] = play(&mut node, &silence, &block(&events));
    assert_close(&left, &sine(440.0, 1.0 + 0.25, phase, 128).0);
    assert_eq!(instance.param_value(LEVEL), Some(0.5));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn the_synth_says_which_notes_voices_ended() {
    let plugins = TestPlugins::new("voices ending");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let silence = vec![0.0; 128];
    let block = |events| Block {
        events,
        ..Block::default()
    };
    let (a, b) = (NoteId(1), NoteId(2));
    let events = two_notes(a, b);
    play(&mut node, &silence, &block(&events));
    assert!(node.ended_voices().is_empty());

    // The synth ends a voice as soon as its note is let go, and says which
    // by the id the note got.
    let off = |offset, note| {
        self::note(
            offset,
            NoteEvent::Off {
                note,
                velocity: 0.0,
            },
        )
    };
    let events = [off(32, b)];
    let [left, _] = play(&mut node, &silence, &block(&events));
    assert_eq!(node.ended_voices(), [b]);
    let (a_alone, _) = sine(440.0, 0.5, 0.0, 384);
    assert_close(&left[32..], &a_alone[160..256]);

    // Each block says what ended in it, and modulation for a note whose
    // voice ended goes nowhere.
    let events = [modulation(0, LEVEL, 0.5, Some(b))];
    let [left, _] = play(&mut node, &silence, &block(&events));
    assert!(node.ended_voices().is_empty());
    assert_close(&left, &a_alone[256..]);
    let events = [off(0, a)];
    play(&mut node, &silence, &block(&events));
    assert_eq!(node.ended_voices(), [a]);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn the_synth_runs_its_voices_on_the_thread_pool() {
    let plugins = TestPlugins::new("pool");
    let mut instance = plugins.synth();
    instance.set_param(THREADS, 1.0);
    instance.set_param(LEVEL, 1.0);
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let on = [note(
        0,
        NoteEvent::On {
            note: NoteId(1),
            key: 60,
            velocity: 1.0,
            channel: None,
        },
    )];
    let block = Block {
        events: &on,
        ..Block::default()
    };
    let [left, _] = play(&mut node, &[0.0; 64], &block);
    assert!(left.iter().any(|s| *s != 0.0));
    // Outside a renderer, the tasks run one after another on the thread
    // processing the plugin.
    assert_eq!(plugins.pool_use(&instance), (1, 1));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn the_synths_voices_run_on_the_renderers_threads() {
    let plugins = TestPlugins::new("pool-threads");
    let mut instance = plugins.synth();
    instance.set_param(THREADS, 1.0);
    instance.activate(CONFIG).unwrap();
    let mut g = Graph::new();
    let synth = g.add(NodeKey(0), instance.node_source().node());
    g.set_output(synth).unwrap();
    // Nothing can run alongside the synth, so the workers only run its
    // voices. They spin between blocks, to be there for each one.
    let renderer = ParallelRenderer::new(ParallelOptions {
        threads: 3,
        spin: Duration::from_secs(1),
        inline_below: None,
        realtime: false,
        ..ParallelOptions::default()
    });
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, Box::new(renderer));
    handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
    for key in 0..16 {
        let note = NoteEvent::On {
            note: NoteId(key),
            key: 48 + key as u8,
            velocity: 1.0,
            channel: None,
        };
        handle
            .send(Event::Note {
                node: NodeKey(0),
                note,
            })
            .unwrap();
    }
    // Which thread takes which voice is up to how they run.
    let mut block = vec![0.0; 2 * 256];
    let deadline = Instant::now() + Duration::from_secs(10);
    while plugins.pool_use(&instance).1 < 2 && Instant::now() < deadline {
        engine.process(&mut AudioMut::from_planar(&mut block, 2));
    }
    let (requests, threads) = plugins.pool_use(&instance);
    assert!(threads > 1, "{threads} thread in {requests} requests");
    assert!(block.iter().any(|s| *s != 0.0));
    drop((engine, handle));
    drop(instance);
    plugins.assert_alright();
}

/// Note `note` on `key`, starting at `offset` with its expression: pitch
/// bend, pressure and timbre.
fn note_with(
    offset: usize,
    note: NoteId,
    key: u8,
    velocity: f32,
    [bend, pressure, timbre]: [f32; 3],
) -> [NodeEvent; 4] {
    let express = |dimension, value| {
        self::note(
            offset,
            NoteEvent::Expression {
                note,
                dimension,
                value,
            },
        )
    };
    [
        self::note(
            offset,
            NoteEvent::On {
                note,
                key,
                velocity,
                channel: None,
            },
        ),
        express(Dimension::PitchBend, bend),
        express(Dimension::Pressure, pressure),
        express(Dimension::Timbre, timbre),
    ]
}

fn express(offset: usize, note: NoteId, dimension: Dimension, value: f32) -> NodeEvent {
    self::note(
        offset,
        NoteEvent::Expression {
            note,
            dimension,
            value,
        },
    )
}

pub(crate) fn let_go(offset: usize, note: NoteId) -> NodeEvent {
    self::note(
        offset,
        NoteEvent::Off {
            note,
            velocity: 0.0,
        },
    )
}

#[test]
fn an_mpe_synth_gets_its_channels_set_up_and_each_note_on_a_channel_of_its_own() {
    let plugins = TestPlugins::new("mpe synth");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let block = |events| Block {
        events,
        ..Block::default()
    };
    let (a, b) = (NoteId(1), NoteId(2));
    let mut events = Vec::new();
    events.extend(note_with(0, a, 60, 1.0, [12.0, 0.0, 0.5]));
    events.extend(note_with(0, b, 60, 0.5, [0.0, 1.0, 0.0]));
    play(&mut node, &[0.0; 64], &block(&events));
    let midi = plugins.midi(&instance);
    // MPE's configuration for a lower zone of 15 channels first, and each
    // channel's bend range, 48 semitones.
    assert_eq!(midi[..3], [[0xB0, 101, 0], [0xB0, 100, 6], [0xB0, 6, 15]]);
    assert_eq!(midi[6..9], [[0xB1, 101, 0], [0xB1, 100, 0], [0xB1, 6, 48]]);
    // Then the notes, each with its expression before it, on channels of
    // their own though they share a key.
    assert_eq!(
        midi[96..],
        [
            [0xE1, 0, 80],
            [0xD1, 0, 0],
            [0xB1, 74, 64],
            [0x91, 60, 127],
            [0xE2, 0, 64],
            [0xD2, 127, 0],
            [0xB2, 74, 0],
            [0x92, 60, 64],
        ]
    );
    // The synth says which voice ended by channel and key, which the node
    // knows the note by.
    let off = [let_go(0, b)];
    play(&mut node, &[0.0; 64], &block(&off));
    assert_eq!(node.ended_voices(), [b]);
    assert_eq!(plugins.midi(&instance), [[0x82, 60, 0]]);
    let off = [let_go(0, a)];
    play(&mut node, &[0.0; 64], &block(&off));
    assert_eq!(node.ended_voices(), [a]);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn mpe_plays_each_notes_expression_as_note_expression_does() {
    // The same performance, on a synth that takes CLAP's note expression and
    // one that takes MPE, sounds the same: MPE carries each note's own
    // pitch bend, pressure and timbre. These values go through MIDI as they
    // are: bends in steps of 48 semitones over 8,192, the rest in 127ths.
    let clap = TestPlugins::new("note expression synth");
    let mpe = TestPlugins::new("mpe expression synth");
    let mut instances = [clap.synth(), mpe.synth()];
    let mut nodes = instances.each_mut().map(|instance| {
        instance.activate(CONFIG).unwrap();
        instance.node_source().node()
    });
    let (a, b) = (NoteId(1), NoteId(2));
    let k = |n: f32| n / 127.0;
    let mut first = Vec::new();
    first.extend(note_with(0, a, 60, 1.0, [12.0, 0.0, k(32.0)]));
    first.extend(note_with(40, b, 64, k(64.0), [-1.5, k(64.0), k(96.0)]));
    let blocks: [Vec<NodeEvent>; 4] = [
        first,
        vec![
            express(10, a, Dimension::PitchBend, -24.0),
            express(10, a, Dimension::Pressure, 1.0),
            express(20, b, Dimension::Timbre, 0.0),
            express(30, b, Dimension::PitchBend, 0.75),
        ],
        vec![let_go(32, a), express(50, b, Dimension::Pressure, k(1.0))],
        vec![let_go(64, b)],
    ];
    for (i, events) in blocks.iter().enumerate() {
        let block = Block {
            events,
            ..Block::default()
        };
        let [heard, expected] = nodes
            .each_mut()
            .map(|node| play(node, &[0.0; 128], &block)[0].clone());
        assert!(heard.iter().any(|s| *s != 0.0), "block {i}");
        assert_close(&heard, &expected);
        let [ended, expected] = nodes.each_ref().map(|node| node.ended_voices().to_vec());
        assert_eq!(ended, expected, "block {i}");
    }
    drop((nodes, instances));
    clap.assert_alright();
    mpe.assert_alright();
}

#[test]
fn the_sustain_pedal_holds_the_notes_let_go_until_it_comes_up() {
    for name in ["sustained synth", "mpe sustained synth"] {
        let plugins = TestPlugins::new(name);
        let mut instance = plugins.synth();
        instance.activate(CONFIG).unwrap();
        let mut node = instance.node_source().node();
        let silence = vec![0.0; 128];
        let block = |events| Block {
            events,
            ..Block::default()
        };
        let a = NoteId(1);
        // A timbre of none, as an MPE note at rest is half bright.
        let mut on = vec![controller(0, Controller::SUSTAIN, 1.0)];
        on.extend(note_with(0, a, 69, 1.0, [0.0, 0.0, 0.0]));
        on.push(let_go(64, a));
        // Let go, the note sounds on while the pedal is down.
        let [left, _] = play(&mut node, &silence, &block(&on));
        let (expected, _) = sine(440.0, 0.5, 0.0, 384);
        assert_close(&left, &expected[..128]);
        assert!(node.ended_voices().is_empty(), "{name}");
        let up = [controller(32, Controller::SUSTAIN, 0.0)];
        let [left, _] = play(&mut node, &silence, &block(&up));
        assert_close(&left[..32], &expected[128..160]);
        assert_eq!(left[32..], silence[32..], "{name}");
        assert_eq!(node.ended_voices(), [a], "{name}");
        // The pedal goes as MIDI's controller 64, on the first channel.
        let midi = plugins.midi(&instance);
        let pedal: Vec<_> = midi.iter().filter(|m| m[1] == 64 && m[0] == 0xB0).collect();
        assert_eq!(pedal, [&[0xB0, 64, 127], &[0xB0, 64, 0]], "{name}");
        drop((node, instance));
        plugins.assert_alright();
    }
}

/// A note starting on `key` at velocity 1, on `channel` if it's given one.
pub(crate) fn on_channel(offset: usize, id: u64, key: u8, channel: Option<u8>) -> NodeEvent {
    note(
        offset,
        NoteEvent::On {
            note: NoteId(id),
            key,
            velocity: 1.0,
            channel,
        },
    )
}

#[test]
fn a_multi_timbral_synth_plays_the_notes_a_note_wire_gives_a_channel_on_it() {
    let plugins = TestPlugins::new("parts synth");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let silence = vec![0.0; 128];
    fn block(events: &[NodeEvent]) -> Block<'_> {
        Block {
            events,
            ..Block::default()
        }
    }
    // A, 220 Hz, on no channel of its own: the first, its first part.
    let [left, _] = play(&mut node, &silence, &block(&[on_channel(0, 1, 57, None)]));
    let (first, _) = sine(220.0, 0.5, 0.0, 128);
    assert_close(&left, &first);
    let [left, _] = play(&mut node, &silence, &block(&[let_go(0, NoteId(1))]));
    assert_eq!(left, silence);
    // On the third, its part plays it two octaves up.
    let [left, _] = play(
        &mut node,
        &silence,
        &block(&[on_channel(0, 2, 57, Some(2))]),
    );
    assert_close(&left, &sine(880.0, 0.5, 0.0, 128).0);
    let [left, _] = play(&mut node, &silence, &block(&[let_go(0, NoteId(2))]));
    assert_eq!(left, silence);
    drop((node, instance));
    plugins.assert_alright();

    // An MPE synth hears a note on its channel, sharing it.
    let plugins = TestPlugins::new("mpe synth");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let events = [
        on_channel(0, 1, 60, Some(4)),
        on_channel(0, 2, 64, Some(4)),
        controller_on(0, Controller::SUSTAIN, 1.0, Some(4)),
    ];
    play(&mut node, &silence, &block(&events));
    let midi = plugins.midi(&instance);
    assert_eq!(
        midi[96..],
        [[0x94, 60, 127], [0x94, 64, 127], [0xB4, 64, 127]]
    );
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn controllers_go_as_midi_to_a_plugin_that_takes_notes_as_clap_s_own() {
    let plugins = TestPlugins::new("controllers");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let events = [
        controller(0, Controller::MOD_WHEEL, 64.0 / 127.0),
        controller(10, Controller::new(3).unwrap(), 1.0),
        note(
            20,
            NoteEvent::On {
                note: NoteId(1),
                key: 60,
                velocity: 1.0,
                channel: None,
            },
        ),
    ];
    let block = Block {
        events: &events,
        ..Block::default()
    };
    play(&mut node, &[0.0; 64], &block);
    // Notes go as CLAP's own, so the only MIDI is the controllers'.
    assert_eq!(plugins.midi(&instance), [[0xB0, 1, 64], [0xB0, 3, 127]]);
    drop((node, instance));
    plugins.assert_alright();
}

/// Hands on the notes it gets, by engine time, as many as there's room
/// for. Through a ring buffer, which never allocates, as a mutex can as it
/// first locks: macOS's does.
struct Kept(rtrb::Producer<(u64, NoteEvent)>);

impl Node for Kept {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for event in ctx.events {
            if let NodeEventKind::Note(note) = event.kind {
                // Past the room, the test finds notes missing.
                let _ = self.0.push((ctx.time + event.offset as u64, note));
            }
        }
    }
}

/// Plays `arpeggiator`, a MIDI effect's node, in its switch, as a MIDI
/// track has it, with keys 60 and 64 held from engine time 0 and let go
/// at frame 15,104, two and a half steps on, and returns the notes it
/// plays, by engine time.
pub(crate) fn arpeggiate(arpeggiator: impl Node) -> Vec<(u64, NoteEvent)> {
    let mut g = Graph::new();
    let switch = g.add(NodeKey(0), NoteSwitch::new(Box::new(arpeggiator), false));
    let (notes, mut played) = rtrb::RingBuffer::new(64);
    let kept = g.add(NodeKey(1), Kept(notes));
    g.send_notes(switch, kept).unwrap();
    g.set_output(kept).unwrap();
    let (mut engine, mut handle) = Engine::new(CONFIG);
    handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
    let mut send = |note| {
        let node = NodeKey(0);
        handle.send(Event::Note { node, note }).unwrap();
    };
    for (id, key) in [(1, 60), (2, 64)] {
        let (note, velocity) = (NoteId(id), 1.0);
        send(NoteEvent::On {
            note,
            key,
            velocity,
            channel: None,
        });
    }
    let mut block = vec![0.0; 2 * 256];
    for i in 0..61 {
        if i == 59 {
            for id in [1, 2] {
                let (note, velocity) = (NoteId(id), 0.0);
                send(NoteEvent::Off { note, velocity });
            }
        }
        engine.process(&mut AudioMut::from_planar(&mut block, 2));
    }
    drop((engine, handle));
    std::iter::from_fn(|| played.pop().ok()).collect()
}

/// Checks that an arpeggiator transposing an octave up played the keys
/// [`arpeggiate`] holds in turn, a step each, lowest first, each with a
/// fresh id from the host's own, and ended the last as they were let go.
pub(crate) fn assert_arpeggiated(played: &[(u64, NoteEvent)]) {
    let step = (ARP_STEP * f64::from(CONFIG.sample_rate)) as u64;
    assert_eq!(step, 6000);
    let ids: Vec<_> = played
        .iter()
        .filter_map(|(_, note)| match note {
            NoteEvent::On { note, .. } => Some(*note),
            _ => None,
        })
        .collect();
    let [a, b, c] = ids[..] else {
        panic!("three steps: {played:?}");
    };
    let on = |note, key| NoteEvent::On {
        note,
        key,
        velocity: 1.0,
        channel: None,
    };
    let off = |note| NoteEvent::Off {
        note,
        velocity: 0.0,
    };
    assert_eq!(
        played,
        [
            (0, on(a, 72)),
            (step, off(a)),
            (step, on(b, 76)),
            (2 * step, off(b)),
            (2 * step, on(c, 72)),
            (15_104, off(c)),
        ]
    );
    // Ids of their own, which no note played into a plugin has.
    assert!(ids.iter().all(|id| id.0 >= 1 << 63), "{ids:?}");
    assert!(a != b && b != c && a != c, "{ids:?}");
}

#[test]
fn an_arpeggiator_plays_notes_of_its_own_as_clap_notes_or_midi() {
    // A library named so plays MIDI.
    for name in ["arpeggiator", "midi-arpeggiator"] {
        let plugins = TestPlugins::new(name);
        let mut instance = plugins.arpeggiator();
        assert_eq!(instance.info().kind(), PluginKind::MidiEffect);
        instance.set_param(TRANSPOSE, 12.0);
        instance.activate(CONFIG).unwrap();
        assert_arpeggiated(&arpeggiate(instance.node_source().node()));
        drop(instance);
        plugins.assert_alright();
    }
}

/// Plays `synth`, the node of a synth that plays each note it gets out
/// again an octave up, in its switch, as a track has it, bypassed to begin
/// with, and returns the notes it plays out, by engine time. Brought back
/// at 1,000, partway through a block, it gets C from 1,010 to 1,100, in
/// the part of that block it renders and in the next, and E from 1,500.
/// Bypassed at 2,000, it stops as its fade ends, with E sounding, and D
/// from 3,000 finds it stopped.
pub(crate) fn echo(synth: impl Node) -> Vec<(u64, NoteEvent)> {
    let mut g = Graph::new();
    let switch = g.add(NodeKey(0), Switch::new(Box::new(synth), true));
    let (notes, mut played) = rtrb::RingBuffer::new(64);
    let kept = g.add(NodeKey(1), Kept(notes));
    g.send_made_notes(switch, kept).unwrap();
    g.set_output(kept).unwrap();
    let (mut engine, mut handle) = Engine::new(CONFIG);
    handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
    let node = NodeKey(0);
    let bypass = |value| Event::Param {
        node,
        param: Switch::BYPASS,
        value,
    };
    let note = |note| Event::Note { node, note };
    let on = |id, key| {
        note(NoteEvent::On {
            note: NoteId(id),
            key,
            velocity: 0.75,
            channel: None,
        })
    };
    let off = |id| {
        note(NoteEvent::Off {
            note: NoteId(id),
            velocity: 0.25,
        })
    };
    for (time, event) in [
        (1_000, bypass(0.0)),
        (1_010, on(1, 60)),
        (1_100, off(1)),
        (1_500, on(2, 64)),
        (2_000, bypass(1.0)),
        (3_000, on(3, 62)),
    ] {
        handle.send_at(time, event).unwrap();
    }
    let mut block = vec![0.0; 2 * 256];
    for _ in 0..16 {
        engine.process(&mut AudioMut::from_planar(&mut block, 2));
    }
    assert_eq!(engine.stats().events_late, 0);
    drop((engine, handle));
    std::iter::from_fn(|| played.pop().ok()).collect()
}

/// Checks that a synth [`echo`] played played C and E out an octave up,
/// on the frames it got them, with ids of their own, and that C ended
/// where it was let go, and E where the synth stopped, at the end of its
/// fade, 10 ms after it was bypassed.
pub(crate) fn assert_echoed(played: &[(u64, NoteEvent)]) {
    let ids: Vec<_> = played
        .iter()
        .filter_map(|(_, note)| match note {
            NoteEvent::On { note, .. } => Some(*note),
            _ => None,
        })
        .collect();
    let [c, e] = ids[..] else {
        panic!("two notes: {played:?}");
    };
    let on = |note, key| NoteEvent::On {
        note,
        key,
        velocity: 0.75,
        channel: None,
    };
    let off = |note, velocity| NoteEvent::Off { note, velocity };
    let stopped = 2_000 + 480;
    assert_eq!(
        played,
        [
            (1_010, on(c, 72)),
            (1_100, off(c, 0.25)),
            (1_500, on(e, 76)),
            (stopped, off(e, Note::RELEASE_VELOCITY)),
        ]
    );
    assert!(c != e && ![1, 2, 3].contains(&c.0), "{ids:?}");
}

#[test]
fn a_synth_plays_out_notes_as_it_renders() {
    // A library named so has the synth play out what it gets; without,
    // it has no note output.
    let plugins = TestPlugins::new("synth");
    assert!(!plugins.synth().note_output());
    drop(plugins);
    let plugins = TestPlugins::new("echo");
    let mut instance = plugins.synth();
    assert!(instance.note_output());
    instance.activate(CONFIG).unwrap();
    assert!(instance.note_output());
    assert_echoed(&echo(instance.node_source().node()));
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn a_plugin_with_a_hard_real_time_requirement_must_render_live() {
    // A library named so has its plugins say they have one, as a plugin
    // that talks to hardware does. They say it once, as they're made, and
    // on the main thread.
    let plugins = TestPlugins::new("gain");
    assert!(!plugins.gain().must_render_live());
    drop(plugins);
    let plugins = TestPlugins::new("live");
    let mut instance = plugins.gain();
    assert!(instance.must_render_live());
    instance.activate(CONFIG).unwrap();
    assert!(instance.must_render_live());
    assert!(plugins.synth().must_render_live());
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn plugins_see_the_transport() {
    let plugins = TestPlugins::new("transport");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    assert_eq!(plugins.transport(&instance), None);
    let block = Block {
        // One second in at 120 bpm: two beats.
        transport: TransportState {
            playing: true,
            position: 48_000,
            looping: Some(BeatRange::new(Beats::ZERO, Beats::new(4))),
            record: true,
            recording: true,
            jumps: 0,
        },
        tempo: TempoMap::new(120.0, TimeSignature::FOUR_FOUR),
        ..Block::default()
    };
    play(&mut node, &ramp(16), &block);
    let (flags, beats, tempo) = plugins.transport(&instance).unwrap();
    for flag in [
        CLAP_TRANSPORT_HAS_TEMPO,
        CLAP_TRANSPORT_HAS_BEATS_TIMELINE,
        CLAP_TRANSPORT_IS_PLAYING,
        CLAP_TRANSPORT_IS_RECORDING,
        CLAP_TRANSPORT_IS_LOOP_ACTIVE,
    ] {
        assert_ne!(flags & flag, 0, "{flag}");
    }
    assert_eq!((beats, tempo), (2 * CLAP_BEATTIME_FACTOR, 120.0));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn the_load_passes_its_input_through_takes_its_time_and_says_where_it_is() {
    let plugins = TestPlugins::new("load");
    let mut instance = plugins.load();
    let params: Vec<_> = instance.params().iter().map(|p| p.name.clone()).collect();
    assert_eq!(params, ["Cost", "Jitter", "Position"]);
    assert!(instance.params()[2].read_only);
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let input = ramp(256);
    let playing = |position| Block {
        transport: TransportState {
            playing: true,
            position,
            ..TransportState::default()
        },
        tempo: TempoMap::new(120.0, TimeSignature::FOUR_FOUR),
        ..Block::default()
    };
    let [left, right] = play(&mut node, &input, &playing(48_000));
    assert_eq!((left, right), (input.clone(), scaled(&input, -1.0)));
    // It says the position at the end of the block, a second and 256
    // frames in, as the host polls it.
    let end = 1.0 + 256.0 / 48_000.0;
    let said: Vec<f64> = (instance.poll().into_iter())
        .filter_map(|event| match event {
            PluginEvent::ParamChanged { param, value } if param == POSITION => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!((said[0] - end).abs() < 1e-6, "{said:?}");
    assert!((instance.param_value(POSITION).unwrap() - end).abs() < 1e-6);

    // 2 ms for each 64 frames: 8 ms for 256.
    instance.set_param(COST, 2000.0);
    let took = |node: &mut _| {
        let start = Instant::now();
        play(node, &input, &playing(96_000));
        start.elapsed()
    };
    took(&mut node);
    let time = took(&mut node);
    assert!(time >= Duration::from_millis(8), "{time:?}");
    assert!(time < Duration::from_millis(80), "{time:?}");

    // With Jitter, some blocks take longer: the same ones every run.
    instance.set_param(COST, 100.0);
    instance.set_param(JITTER, 1.0);
    let mut spikes = Vec::new();
    for block in 0..64 {
        if took(&mut node) >= Duration::from_micros(400 * 5) {
            spikes.push(block);
        }
    }
    assert!((1..16).contains(&spikes.len()), "{spikes:?}");
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn plugins_get_the_main_thread_when_they_ask() {
    let plugins = TestPlugins::new("callback");
    let woken = Arc::new(AtomicU32::new(0));
    let waker = {
        let woken = Arc::clone(&woken);
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
    };
    let mut instance =
        Instance::new(&plugins.path, GAIN_ID.to_str().unwrap(), Some(waker)).unwrap();
    plugins.request_callback(&instance);
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert_eq!(plugins.main_thread_calls(&instance), 0);
    assert_eq!(instance.poll(), []);
    assert_eq!(plugins.main_thread_calls(&instance), 1);
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn the_plugins_window_opens_inside_ours() {
    let plugins = TestPlugins::new("window");
    let mut instance = plugins.gain();
    assert!(instance.has_window());
    let parent = ParentWindow::Win32(ptr::without_provenance_mut(77));
    assert_eq!(instance.open_window(parent, 1.0), Ok((400, 300)));
    assert!(instance.is_window_open());
    assert_eq!(plugins.window(&instance), (1, 77));
    assert_eq!(instance.resize_window(500, 50), Some((500, 100)));
    assert!(plugins.request_resize(&instance, 640, 480));
    assert_eq!(
        instance.poll(),
        [PluginEvent::WindowResize {
            width: 640,
            height: 480
        }]
    );
    instance.close_window();
    assert_eq!(plugins.window(&instance).0, -1);
    instance.open_floating_window("Test Gain").unwrap();
    assert_eq!(plugins.window(&instance).0, 1);
    // Destroying the plugin closes its window first.
    drop(instance);
    plugins.assert_alright();
}

/// Is the scanning process, when a test runs this binary as one.
#[test]
fn scan_child() {
    if let Some(path) = std::env::var_os(SCAN_CHILD) {
        scan::child(Path::new(&path));
    }
}

const SCAN_CHILD: &str = "MUSICABLE_SCAN_CHILD";

/// Scans the library at `path` in a scanning process of its own.
pub(crate) fn scan_path(path: &Path, timeout: Duration) -> Outcome {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "tests::scan_child", "--test-threads", "1"])
        .env(SCAN_CHILD, path);
    scan::scan_isolated(command, timeout)
}

fn scan_copy(name: &str, timeout: Duration) -> Outcome {
    scan_path(&testing::copy(&temp_dir(name), name), timeout)
}

#[test]
fn the_scanner_finds_plugins_and_survives_bad_ones() {
    let Outcome::Found(found) = scan_copy("good", scan::TIMEOUT) else {
        panic!("the good library wasn't found");
    };
    let names: Vec<_> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["Test Gain", "Test Synth", "Test Arpeggiator", "Test Load"]
    );
    assert!(found[1].features.iter().any(|f| f == "instrument"));
    let kinds: Vec<_> = found.iter().map(PluginInfo::kind).collect();
    assert_eq!(
        kinds,
        [
            PluginKind::AudioEffect,
            PluginKind::Instrument,
            PluginKind::MidiEffect,
            PluginKind::AudioEffect
        ]
    );
    let crashed = scan_copy("crash", scan::TIMEOUT);
    assert!(matches!(&crashed, Outcome::Crashed(_)), "{crashed:?}");
    let hung = scan_copy("hang", Duration::from_secs(1));
    assert!(
        matches!(&hung, Outcome::Crashed(reason) if reason.contains("still loading")),
        "{hung:?}"
    );
    let refused = scan_copy("refuse", scan::TIMEOUT);
    assert!(
        matches!(&refused, Outcome::Failed(reason) if reason.contains("failed to initialize")),
        "{refused:?}"
    );
}

#[test]
fn playing_a_plugin_says_when_its_notes_come_against_its_latency() {
    use musicable_model::plugin::{NoteTiming, PluginKind};
    use musicable_test_plugins::HOLDS_BACK;

    use crate::timing::measure_instance;

    let plugins = TestPlugins::new("timing");
    // An arpeggiator that holds its steps back by its latency has them
    // come with its sound, and one that doesn't, as it plays them.
    for (holds_back, timing) in [(1.0, NoteTiming::WithSound), (0.0, NoteTiming::AsPlayed)] {
        let mut instance = crate::Instance::Clap(plugins.arpeggiator());
        instance.set_param(LATENCY, 960.0);
        instance.set_param(HOLDS_BACK, holds_back);
        let measured = measure_instance(&mut instance, PluginKind::MidiEffect).unwrap();
        assert_eq!(measured, Some(timing), "holds back {holds_back}");
    }
    // Too short a latency to tell, it says nothing.
    let mut instance = crate::Instance::Clap(plugins.arpeggiator());
    instance.set_param(LATENCY, 32.0);
    instance.set_param(HOLDS_BACK, 1.0);
    assert_eq!(
        measure_instance(&mut instance, PluginKind::MidiEffect).unwrap(),
        None
    );
    drop(instance);
    plugins.assert_alright();
}

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
use musicable_engine::{
    AudioMut, AudioRef, Engine, EngineConfig, Event, Graph, Inputs, Node, NodeEvent, NodeEventKind,
    NodeKey, NoteEvent, NoteId, ParallelOptions, ParallelRenderer, ParamId, ProcessContext,
    TransportState, rt,
};
use musicable_model::clip::Dimension;
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_test_plugins::testing::{self, Library};
use musicable_test_plugins::{GAIN, GAIN_ID, LATENCY, LEVEL, SYNTH_ID, THREADS, gui};

use crate::scan::{self, Outcome};
use crate::{Instance, ParentWindow, PluginEvent};

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

    fn gain(&self) -> Instance {
        Instance::new(&self.path, GAIN_ID.to_str().unwrap(), None).unwrap()
    }

    fn synth(&self) -> Instance {
        Instance::new(&self.path, SYNTH_ID.to_str().unwrap(), None).unwrap()
    }
}

/// What a block is played with.
#[derive(Default)]
pub(crate) struct Block<'a> {
    pub(crate) transport: TransportState,
    pub(crate) tempo: TempoMap,
    pub(crate) events: &'a [NodeEvent],
}

/// Plays one block of `node`, with `input` on the left and its negative on
/// the right, and returns both channels. It mustn't allocate.
pub(crate) fn play(node: &mut impl Node, input: &[f32], block: &Block<'_>) -> [Vec<f32>; 2] {
    let frames = input.len();
    let planar_in: Vec<f32> = input
        .iter()
        .copied()
        .chain(input.iter().map(|s| -s))
        .collect();
    let mut planar_out = vec![f32::NAN; frames * 2];
    let ctx = ProcessContext {
        frames,
        sample_rate: CONFIG.sample_rate,
        time: 0,
        transport: block.transport,
        tempo: &block.tempo,
        events: block.events,
        input: AudioRef::empty(frames),
    };
    {
        let views = [AudioRef::from_planar(&planar_in, 2)];
        let inputs = Inputs::from_views(&views);
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
            param: ParamId(param),
            value,
        },
    }
}

pub(crate) fn note(offset: usize, event: NoteEvent) -> NodeEvent {
    NodeEvent {
        offset,
        kind: NodeEventKind::Note(event),
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
    assert!(!instance.info().is_instrument());
    let params = instance.params();
    assert_eq!(
        params.iter().map(|p| p.id).collect::<Vec<_>>(),
        [GAIN, LATENCY]
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
    assert!(params[1].steps == 48_000 && !params[1].automatable);
    assert_eq!(instance.param_value(GAIN), Some(1.0));
    assert_eq!(instance.param_value(99), None);
    assert_eq!(
        instance.value_to_text(LATENCY, 64.0).as_deref(),
        Some("64 samples")
    );
    assert_eq!(instance.text_to_value(GAIN, "0.25"), Some(0.25));
    assert_eq!(instance.text_to_value(GAIN, "loud"), None);
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
    assert!(instance.info().is_instrument());
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
        events: &[],
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
    assert_eq!(names, ["Test Gain", "Test Synth"]);
    assert!(found[1].features.iter().any(|f| f == "instrument"));
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

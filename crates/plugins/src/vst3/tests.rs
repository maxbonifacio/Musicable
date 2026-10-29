//! VST3 hosting, tested against the test plugins' VST3 module, which counts
//! every time the host breaks one of VST3's rules.

use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use musicable_engine::nodes::Sum;
use musicable_engine::switch::Switch;
use musicable_engine::{AudioMut, Engine, Graph, Node, NodeKey, NoteEvent, NoteId, TransportState};
use musicable_model::PluginFormat;
use musicable_model::clip::{Controller, Dimension};
use musicable_model::plugin::{PluginInput, PluginKind};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_test_plugins::testing::{self, Library, Lifecycle};
use musicable_test_plugins::vst3::{
    PEAK, VST3_ARPEGGIATOR_ID, VST3_GAIN_ID, VST3_SIDECHAIN, VST3_SYNTH_ID, Vst3Transport,
};
use musicable_test_plugins::{GAIN, LATENCY, LEVEL, TRANSPOSE, gui};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kBarPositionValid, kContTimeValid, kCycleActive, kCycleValid, kPlaying, kProjectTimeMusicValid,
    kRecording, kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::ProcessModes_::{kPrefetch, kRealtime};
use vst3::Steinberg::Vst::RestartFlags_::{
    kIoChanged, kLatencyChanged, kParamTitlesChanged, kParamValuesChanged, kReloadComponent,
};
use vst3::Steinberg::{int32, uint32};

use super::Instance;
use super::preset::{self, Preset};
use crate::scan::{self, Outcome};
use crate::tests::{
    Block, CONFIG, Impulse, arpeggiate, assert_arpeggiated, assert_close, bypass,
    bypassed_at_256_and_back_at_1280, controller, ducked, note, param, play, play_blocks,
    play_keyed, ramp, scaled, scan_path, sine, temp_dir,
};
use crate::{Buses, ParentWindow, PluginEvent, PluginInfo, Waker};

/// A VST3 bundle of the test plugins, with the hooks tests drive them with.
struct TestPlugins {
    path: PathBuf,
    library: Library,
}

impl TestPlugins {
    /// A bundle named `name.vst3`, so the test gets the library's statics
    /// to itself.
    fn new(name: &str) -> Self {
        let library = Library::copy_vst3(&temp_dir(name), name);
        Self {
            path: library.path().to_owned(),
            library,
        }
    }

    fn instance(&self, id: &str, waker: Option<Waker>) -> Instance {
        Instance::new(&self.path, id, waker).unwrap()
    }

    fn gain(&self) -> Instance {
        self.instance(VST3_GAIN_ID, None)
    }

    fn synth(&self) -> Instance {
        self.instance(VST3_SYNTH_ID, None)
    }

    fn arpeggiator(&self) -> Instance {
        self.instance(VST3_ARPEGGIATOR_ID, None)
    }

    fn assert_alright(&self) {
        self.library.assert_alright();
    }

    // SAFETY, for the hooks below: each takes the instance's component or
    // controller, which live as long as the instance, on its UI thread.

    fn gui(&self, instance: &Instance, action: u32, param: u32, value: f64) {
        let controller = instance.raw_controller();
        // SAFETY: as above.
        let taken = unsafe { self.library.vst3_gui(controller, action, param, value) };
        assert!(taken, "the host didn't take the edit");
    }

    fn set_label(&self, instance: &Instance, label: &str) {
        // SAFETY: as above.
        let sent = unsafe {
            self.library
                .vst3_set_label(instance.raw_controller(), label)
        };
        assert!(sent, "the label didn't get to the component");
    }

    /// The label the component has.
    fn label(&self, instance: &Instance) -> String {
        // SAFETY: as above.
        unsafe { self.library.vst3_label(instance.raw_component()) }.unwrap()
    }

    fn set_zoom(&self, instance: &Instance, zoom: f64) {
        // SAFETY: as above.
        assert!(unsafe { self.library.vst3_set_zoom(instance.raw_controller(), zoom) });
    }

    fn zoom(&self, instance: &Instance) -> Option<f64> {
        // SAFETY: as above.
        unsafe { self.library.vst3_zoom(instance.raw_controller()) }
    }

    /// A parameter's value as the processor has it.
    fn processor_value(&self, instance: &Instance, param: u32) -> Option<f64> {
        // SAFETY: as above.
        unsafe {
            self.library
                .vst3_processor_value(instance.raw_component(), param)
        }
    }

    /// The pretend window: -1 for none, 1 in a window of the host's; that
    /// window; its platform (1 Win32, 2 Cocoa, 3 X11); and its scale.
    fn window(&self, instance: &Instance) -> (i32, usize, i32, f64) {
        // SAFETY: as above.
        unsafe { self.library.vst3_window(instance.raw_controller()) }
    }

    fn request_resize(&self, instance: &Instance, width: u32, height: u32) -> bool {
        let controller = instance.raw_controller();
        // SAFETY: as above.
        unsafe { self.library.vst3_request_resize(controller, width, height) }
    }

    fn transport(&self, instance: &Instance) -> Option<Vst3Transport> {
        // SAFETY: as above.
        unsafe { self.library.vst3_transport(instance.raw_component()) }
    }

    fn restart(&self, instance: &Instance, flags: i32) {
        // SAFETY: as above.
        assert!(unsafe { self.library.vst3_restart(instance.raw_controller(), flags) });
    }

    fn lifecycle(&self, instance: &Instance) -> Lifecycle {
        // SAFETY: as above.
        unsafe { self.library.vst3_lifecycle(instance.raw_component()) }.unwrap()
    }

    /// Whether the gain's sidechain bus is on, as the plugin has it.
    fn sidechain(&self, instance: &Instance) -> bool {
        // SAFETY: as above.
        unsafe { self.library.vst3_sidechain(instance.raw_component()) }.unwrap()
    }
}

/// Test Gain's Latency, normalized, for `frames` frames.
fn latency(frames: u32) -> f64 {
    f64::from(frames) / 48_000.0
}

#[test]
fn the_gain_plugin_describes_itself_and_its_parameters() {
    let plugins = TestPlugins::new("params");
    let instance = plugins.gain();
    let info = instance.info();
    assert_eq!(
        (info.format, info.id.as_str(), info.name.as_str()),
        (PluginFormat::Vst3, VST3_GAIN_ID, "Test Gain")
    );
    assert_eq!(
        (info.vendor.as_str(), info.version.as_str()),
        ("Musicable", "1.0.0")
    );
    assert_eq!(info.features, ["Fx"]);
    assert_eq!(info.kind(), PluginKind::AudioEffect);
    let params = instance.params();
    assert_eq!(
        params.iter().map(|p| p.id).collect::<Vec<_>>(),
        [GAIN, LATENCY, PEAK]
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
        ("Gain", "Levels", 0.0, 1.0, 0.5)
    );
    assert!(gain.automatable && !gain.modulatable && !gain.read_only && gain.steps == 0);
    assert!(!gain.modulatable_per_note);
    let latency_param = &params[1];
    assert_eq!(
        (
            latency_param.module.as_str(),
            latency_param.steps,
            latency_param.automatable
        ),
        ("", 48_000, false)
    );
    assert!(params[2].read_only);
    assert_eq!(instance.param_value(GAIN), Some(0.5));
    assert_eq!(instance.param_value(99), None);
    assert_eq!(
        instance.value_to_text(LATENCY, latency(64)).as_deref(),
        Some("64 samples")
    );
    assert_eq!(instance.value_to_text(GAIN, 0.25).as_deref(), Some("0.50"));
    assert_eq!(instance.text_to_value(GAIN, "0.5"), Some(0.25));
    assert_eq!(instance.text_to_value(GAIN, "loud"), None);
    // The controller's class is in the module, but isn't a plugin.
    let controller = "4D5553494341424C4554455354000002";
    assert!(Instance::new(&plugins.path, controller, None).is_err());
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
    // 0.5 is a gain of 1.
    let [left, right] = play(&mut node, &input, &Block::default());
    assert_eq!((left, right), (input.clone(), scaled(&input, -1.0)));

    // Changes from the main thread apply from the next block's start, and
    // the node's events from their offsets.
    instance.set_param(GAIN, 0.25);
    let [left, _] = play(&mut node, &input, &Block::default());
    assert_eq!(left, scaled(&input, 0.5));
    assert_eq!(instance.param_value(GAIN), Some(0.25));
    let events = [param(10, GAIN, 1.0)];
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
fn a_bypassed_plugin_takes_its_changes_and_has_its_processing_restarted_coming_back() {
    let plugins = TestPlugins::new("bypassed");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, latency(32));
    instance.activate(CONFIG).unwrap();
    // A gain of 0.5.
    instance.set_param(GAIN, 0.25);
    let mut switch = Switch::new(Box::new(instance.node_source().node()), false);
    switch.prepare(&CONFIG);
    assert_eq!(switch.latency(), 32, "a bypassed plugin keeps its latency");
    let activated = plugins.lifecycle(&instance);
    let signal = ramp(256 * 8);
    let mut lifecycle = Vec::new();
    let mut running = Vec::new();
    let events = |i| match i {
        1 => vec![bypass(0, true)],
        // Stopped, it takes the changes its parameters get, in a block of
        // no frames.
        4 => vec![param(0, GAIN, 1.0)],
        5 => vec![bypass(0, false)],
        _ => Vec::new(),
    };
    let out = play_blocks(&mut switch, &signal, events, |i| {
        if i == 4 {
            assert_eq!(plugins.processor_value(&instance, GAIN), Some(1.0));
            // A gain of 0.25, as it comes back.
            instance.set_param(GAIN, 0.125);
        }
        lifecycle.push(plugins.lifecycle(&instance));
        running.push(instance.is_running());
    });
    assert_eq!(running, [true, true, false, false, false, true, true, true]);
    assert_close(&out, &bypassed_at_256_and_back_at_1280(&signal, 0.5, 0.25));
    // It processes nothing while it's bypassed. VST3 has no reset: coming
    // back, its processing goes off and on again.
    assert_eq!(lifecycle[2].blocks, lifecycle[4].blocks);
    let switched = |l: &Lifecycle| (l.starts - activated.starts, l.stops - activated.stops);
    assert_eq!(switched(&lifecycle[4]), (0, 0));
    assert_eq!(switched(&lifecycle[5]), (1, 1));
    assert_eq!(switched(&lifecycle[7]), (1, 1));
    drop((switch, instance));
    plugins.assert_alright();
}

#[test]
fn a_deactivated_plugin_still_takes_its_changes() {
    let plugins = TestPlugins::new("deactivated");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut playing = Switch::new(Box::new(instance.node_source().node()), false);
    playing.prepare(&CONFIG);
    let input = ramp(256);
    play(&mut playing, &input, &Block::default());
    let mut deactivated = Switch::deactivated(Box::new(instance.node_source().node()));
    deactivated.prepare(&CONFIG);
    deactivated.adopt(&mut playing);
    let before = plugins.lifecycle(&instance);
    let events = [param(0, GAIN, 0.25)];
    let block = Block {
        time: 256,
        events: &events,
        ..Block::default()
    };
    assert_eq!(play(&mut deactivated, &input, &block)[0], vec![0.0; 256]);
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.25));
    assert_eq!(plugins.lifecycle(&instance), before);
    let mut active = Switch::new(Box::new(instance.node_source().node()), false);
    active.prepare(&CONFIG);
    active.adopt(&mut deactivated);
    let block = Block {
        time: 512,
        ..Block::default()
    };
    assert_eq!(play(&mut active, &input, &block)[0], scaled(&input, 0.5));
    let after = plugins.lifecycle(&instance);
    assert_eq!(
        (
            after.starts - before.starts,
            after.stops - before.stops,
            after.blocks - before.blocks
        ),
        (1, 1, 1)
    );
    drop((playing, deactivated, active, instance));
    plugins.assert_alright();
}

#[test]
fn changes_to_an_inactive_plugin_reach_it_as_it_activates() {
    let plugins = TestPlugins::new("inactive");
    let mut instance = plugins.gain();
    instance.set_param(GAIN, 0.25);
    instance.set_param(LATENCY, latency(32));
    // The controller has them at once, and the processor once it processes.
    assert_eq!(instance.param_value(GAIN), Some(0.25));
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.5));
    instance.activate(CONFIG).unwrap();
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.25));
    // The test gain takes its latency as it's activated, so it's activated
    // again for it: the restart it asked for.
    assert_eq!(instance.latency(), 32);
    assert_eq!(instance.poll(), []);
    // Changes waiting for a node when the plugin deactivates get to it.
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(16), &Block::default());
    instance.set_param(GAIN, 0.75);
    instance.deactivate().unwrap();
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.75));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn an_active_plugin_no_node_processes_gets_its_changes_as_its_state_saves() {
    // As when there's no audio device: active, and never processed.
    let plugins = TestPlugins::new("unprocessed");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    instance.set_param(GAIN, 0.25);
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.5));
    // Saving waits a few blocks for a node to hand them over, then hands
    // them over itself.
    let state = instance.save_state().unwrap();
    assert!(String::from_utf8_lossy(&state).contains("p10=0.25"));
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.25));
    // Edits in the window come back, and a node that starts later plays
    // them.
    plugins.gui(&instance, gui::BEGIN, GAIN, 0.0);
    plugins.gui(&instance, gui::VALUE, GAIN, 0.125);
    plugins.gui(&instance, gui::END, GAIN, 0.0);
    assert_eq!(
        instance.poll(),
        [
            PluginEvent::GestureBegin { param: GAIN },
            PluginEvent::ParamChanged {
                param: GAIN,
                value: 0.125
            },
            PluginEvent::GestureEnd { param: GAIN },
        ]
    );
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
fn saving_waits_for_changes_on_their_way_to_the_plugin() {
    let plugins = TestPlugins::new("settle");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let stop = Arc::new(AtomicBool::new(false));
    let audio = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let input = ramp(256);
            while !stop.load(Ordering::SeqCst) {
                play(&mut node, &input, &Block::default());
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    for value in [0.25, 0.75, 0.125] {
        instance.set_param(GAIN, value);
        let state = instance.save_state().unwrap();
        let state = String::from_utf8_lossy(&state);
        assert!(state.contains(&format!("p10={value}\n")), "{state}");
    }
    stop.store(true, Ordering::SeqCst);
    audio.join().unwrap();
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn a_latency_change_restarts_the_plugin() {
    let plugins = TestPlugins::new("latency");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    assert_eq!((instance.latency(), node.latency()), (0, 0));
    instance.set_param(LATENCY, latency(100));
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

    // As when someone turns it in the plugin's window.
    plugins.gui(&instance, gui::VALUE, LATENCY, latency(200));
    assert_eq!(
        instance.poll(),
        [
            PluginEvent::ParamChanged {
                param: LATENCY,
                value: latency(200)
            },
            PluginEvent::Restarted {
                latency_changed: true
            },
        ]
    );
    assert_eq!(instance.latency(), 200);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn a_latency_set_before_the_plugin_activates_needs_no_restart() {
    let plugins = TestPlugins::new("latency-first");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, latency(100));
    // The restart the plugin asked for waits for it to activate, which is
    // one.
    assert_eq!(instance.poll(), []);
    instance.activate(CONFIG).unwrap();
    assert_eq!(instance.latency(), 100);
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(64), &Block::default());
    assert_eq!(instance.poll(), []);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn a_side_input_turns_on_with_a_restart_as_it_is_wired() {
    let plugins = TestPlugins::new("sidechain");
    let mut instance = plugins.gain();
    let sidechain = PluginInput {
        id: VST3_SIDECHAIN,
        name: "Sidechain".into(),
    };
    assert_eq!(instance.side_inputs(), [sidechain]);
    assert_eq!(instance.set_side_inputs(&[VST3_SIDECHAIN]), Ok(false));
    instance.activate(CONFIG).unwrap();
    assert!(plugins.sidechain(&instance), "on as it activated");
    assert_eq!(instance.set_side_inputs(&[]), Ok(true));
    assert!(!plugins.sidechain(&instance));
    let mut node = instance.node_source().node();
    let (input, key) = (vec![0.5; 64], ramp(64));
    let keyed = Some((VST3_SIDECHAIN, key.as_slice()));
    // Off, it hears nothing of its side input.
    assert_eq!(
        play_keyed(&mut node, &input, keyed, &Block::default())[0],
        input
    );
    let starts = plugins.lifecycle(&instance).starts;

    // Wired, it restarts with the bus on, which ducks it.
    assert_eq!(instance.set_side_inputs(&[VST3_SIDECHAIN, 0, 7]), Ok(true));
    assert!(plugins.sidechain(&instance));
    assert_eq!(instance.set_side_inputs(&[VST3_SIDECHAIN]), Ok(false));
    let [left, right] = play_keyed(&mut node, &input, keyed, &Block::default());
    assert_close(&left, &ducked(&input, &key));
    assert_close(&right, &scaled(&ducked(&input, &key), -1.0));
    assert!(plugins.lifecycle(&instance).starts > starts, "it restarted");
    assert_eq!(play(&mut node, &input, &Block::default())[0], input);

    // A latent plugin hears it as late as its input.
    instance.set_param(LATENCY, latency(37));
    assert_eq!(
        instance.poll(),
        [PluginEvent::Restarted {
            latency_changed: true
        }]
    );
    assert!(plugins.sidechain(&instance), "still on");
    let input = vec![0.5; 256];
    let mut key = vec![0.0; 256];
    key[100..110].fill(1.0);
    let [left, _] = play_keyed(
        &mut node,
        &input,
        Some((VST3_SIDECHAIN, &key)),
        &Block::default(),
    );
    let mut expected = vec![0.5; 256];
    expected[..37].fill(0.0);
    expected[137..147].fill(0.0);
    assert_eq!(left, expected);
    // The synth has none.
    let mut synth = plugins.synth();
    assert_eq!(synth.side_inputs(), []);
    assert_eq!(synth.set_side_inputs(&[VST3_SIDECHAIN]), Ok(false));
    drop((node, instance, synth));
    plugins.assert_alright();
}

#[test]
fn delay_compensation_lines_a_latent_plugin_up_with_a_dry_path() {
    let plugins = TestPlugins::new("compensation");
    let mut instance = plugins.gain();
    instance.set_param(LATENCY, latency(123));
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

#[test]
fn state_saves_and_loads_as_a_vst3_preset() {
    let plugins = TestPlugins::new("state");
    let mut first = plugins.gain();
    first.activate(CONFIG).unwrap();
    first.set_param(GAIN, 0.375);
    // The controller tells the component in a message, and the host that
    // the state changed.
    plugins.set_label(&first, "warm");
    assert_eq!(plugins.label(&first), "warm");
    assert_eq!(first.poll(), [PluginEvent::StateChanged]);
    // The controller's state of its own.
    plugins.set_zoom(&first, 2.0);
    let state = first.save_state().unwrap();
    let saved = preset::read(&state).unwrap();
    assert_eq!(saved.class_id, VST3_GAIN_ID);
    let component = String::from_utf8_lossy(saved.component);
    assert!(component.contains("p10=0.375\n") && component.contains("label=warm\n"));
    assert_eq!(saved.controller, Some(&b"zoom=2"[..]));

    let mut second = plugins.gain();
    second.load_state(&state).unwrap();
    assert_eq!(second.param_value(GAIN), Some(0.375));
    assert_eq!(plugins.processor_value(&second, GAIN), Some(0.375));
    assert_eq!(plugins.label(&second), "warm");
    assert_eq!(plugins.zoom(&second), Some(2.0));

    // What isn't its state changes nothing.
    let err = second.load_state(b"nonsense").unwrap_err();
    assert!(err.to_string().contains("isn't a VST3 preset"), "{err}");
    let broken = preset::write(&Preset {
        class_id: VST3_GAIN_ID,
        component: b"nonsense",
        controller: None,
    });
    let err = second.load_state(&broken).unwrap_err();
    assert!(err.to_string().contains("couldn't load its state"), "{err}");
    let mut synth = plugins.synth();
    let err = second.load_state(&synth.save_state().unwrap()).unwrap_err();
    assert!(err.to_string().contains("another plugin's"), "{err}");
    assert_eq!(second.param_value(GAIN), Some(0.375));

    // The synth is its own controller, so it has only the one state.
    plugins.set_label(&synth, "bright");
    assert_eq!(synth.poll(), [PluginEvent::StateChanged]);
    let state = synth.save_state().unwrap();
    assert_eq!(preset::read(&state).unwrap().controller, None);
    let mut other = plugins.synth();
    other.load_state(&state).unwrap();
    assert_eq!(plugins.label(&other), "bright");
    drop((first, second, synth, other));
    plugins.assert_alright();
}

#[test]
fn changes_in_the_plugins_window_come_back() {
    let plugins = TestPlugins::new("window-changes");
    let mut instance = plugins.gain();
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
    // Inactive, the processor gets them as it activates.
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.5));
    instance.activate(CONFIG).unwrap();
    assert_eq!(plugins.processor_value(&instance, GAIN), Some(0.3));
    // Processed, it gets them with the next block.
    let mut node = instance.node_source().node();
    play(&mut node, &ramp(16), &Block::default());
    plugins.gui(&instance, gui::VALUE, GAIN, 0.75);
    assert_eq!(
        instance.poll(),
        [PluginEvent::ParamChanged {
            param: GAIN,
            value: 0.75
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

#[test]
fn meters_and_automation_reach_the_controller_without_coming_back() {
    let plugins = TestPlugins::new("meters");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let events = [param(0, GAIN, 0.25)];
    let block = Block {
        events: &events,
        ..Block::default()
    };
    // A gain of 0.5 on a ramp up to 1.
    play(&mut node, &ramp(64), &block);
    // Neither is an edit: the project has the automation already, and the
    // meter is the plugin's to show.
    assert_eq!(instance.poll(), []);
    assert_eq!(instance.param_value(GAIN), Some(0.25));
    assert_eq!(instance.param_value(PEAK), Some(0.5));
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn the_plugin_restarts_as_it_asks() {
    let plugins = TestPlugins::new("restarts");
    let mut instance = plugins.gain();
    // Inactive, the restart waits for it to activate, which is one.
    plugins.restart(&instance, kLatencyChanged);
    assert_eq!(instance.poll(), []);
    instance.activate(CONFIG).unwrap();
    assert_eq!(instance.poll(), []);
    for flags in [kLatencyChanged, kIoChanged, kReloadComponent] {
        plugins.restart(&instance, flags);
        assert_eq!(
            instance.poll(),
            [PluginEvent::Restarted {
                latency_changed: false
            }],
            "{flags}"
        );
        assert!(instance.is_active());
    }
    plugins.restart(&instance, kParamTitlesChanged);
    assert_eq!(instance.poll(), [PluginEvent::ParamsChanged]);
    assert_eq!(instance.params().len(), 3);
    plugins.restart(&instance, kParamValuesChanged);
    assert_eq!(instance.poll(), [PluginEvent::StateChanged]);
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn the_synth_plays_notes_with_their_expression() {
    let plugins = TestPlugins::new("synth");
    let mut instance = plugins.synth();
    assert_eq!(instance.info().kind(), PluginKind::Instrument);
    assert_eq!(instance.info().features, ["Instrument", "Synth"]);
    assert_eq!(
        instance.params().iter().map(|p| p.id).collect::<Vec<_>>(),
        [LEVEL]
    );
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
fn controllers_move_the_parameters_the_plugins_midi_mapping_says() {
    let plugins = TestPlugins::new("midi mapping");
    let mut instance = plugins.synth();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    // The synth maps the volume controller to its level, and nothing to
    // the mod wheel.
    let events = [
        controller(0, Controller::VOLUME, 0.25),
        controller(0, Controller::MOD_WHEEL, 1.0),
        note(
            0,
            NoteEvent::On {
                note: NoteId(1),
                key: 69,
                velocity: 1.0,
            },
        ),
    ];
    let block = Block {
        events: &events,
        ..Block::default()
    };
    let [left, _] = play(&mut node, &[0.0; 128], &block);
    assert_close(&left, &sine(440.0, 0.25, 0.0, 128).0);
    assert_eq!(plugins.processor_value(&instance, LEVEL), Some(0.25));
    // The controller hears of it, as of automation, and it's no edit.
    assert_eq!(instance.poll(), []);
    assert_eq!(instance.param_value(LEVEL), Some(0.25));
    drop((node, instance));
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
    play(&mut node, &ramp(16), &block);
    let transport = plugins.transport(&instance).unwrap();
    for flag in [
        kPlaying,
        kRecording,
        kTempoValid,
        kTimeSigValid,
        kProjectTimeMusicValid,
        kBarPositionValid,
        kCycleActive,
        kCycleValid,
        kContTimeValid,
    ] {
        assert_ne!(transport.state & flag as uint32, 0, "{flag}");
    }
    assert_eq!(
        (
            transport.samples,
            transport.beats,
            transport.bar_start,
            transport.tempo
        ),
        (48_000, 2.0, 0.0, 120.0)
    );
    assert_eq!((transport.cycle, transport.signature), ([0.0, 4.0], [4, 4]));
    // Frames since it activated, wherever the song is.
    assert_eq!(transport.steady, 16);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn blocks_rendered_ahead_come_in_prefetch_mode() {
    let plugins = TestPlugins::new("prefetch");
    let mut instance = plugins.gain();
    instance.activate(CONFIG).unwrap();
    let mut node = instance.node_source().node();
    let ahead = Block {
        ahead: true,
        ..Block::default()
    };
    play(&mut node, &ramp(16), &ahead);
    let mode = plugins.transport(&instance).unwrap().mode;
    assert_eq!(mode, kPrefetch as int32);
    // A block the device asks for, as in an export, comes in real time.
    play(&mut node, &ramp(16), &Block::default());
    let mode = plugins.transport(&instance).unwrap().mode;
    assert_eq!(mode, kRealtime as int32);
    drop((node, instance));
    plugins.assert_alright();
}

#[test]
fn plugins_wake_the_app_when_they_ask_for_something() {
    let plugins = TestPlugins::new("wake");
    let woken = Arc::new(AtomicU32::new(0));
    let waker = {
        let woken = Arc::clone(&woken);
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
    };
    let mut instance = plugins.instance(VST3_GAIN_ID, Some(waker));
    plugins.set_label(&instance, "warm");
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    plugins.gui(&instance, gui::VALUE, GAIN, 0.25);
    assert_eq!(woken.load(Ordering::SeqCst), 2);
    assert_eq!(
        instance.poll(),
        [
            PluginEvent::ParamChanged {
                param: GAIN,
                value: 0.25
            },
            PluginEvent::StateChanged,
        ]
    );
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn the_plugins_window_opens_inside_ours() {
    let plugins = TestPlugins::new("window");
    let mut instance = plugins.gain();
    assert!(instance.has_window());
    assert_eq!(plugins.window(&instance).0, -1);
    let parent = ParentWindow::Win32(ptr::without_provenance_mut(77));
    assert_eq!(instance.open_window(parent, 1.0), Ok((400, 300)));
    assert!(instance.is_window_open());
    assert_eq!(plugins.window(&instance), (1, 77, 1, 1.0));
    assert_eq!(instance.resize_window(500, 50), Some((500, 100)));
    // The window takes the size it asks for at once, and ours follows.
    assert!(plugins.request_resize(&instance, 640, 480));
    assert_eq!(instance.window_size(), Some((640, 480)));
    assert_eq!(
        instance.poll(),
        [PluginEvent::WindowResize {
            width: 640,
            height: 480
        }]
    );
    instance.close_window();
    assert!(!instance.is_window_open());
    assert_eq!(plugins.window(&instance).0, -1);

    // Scaled, it asks for the room as it opens, which it has.
    assert_eq!(
        instance.open_window(ParentWindow::X11(99), 2.0),
        Ok((800, 600))
    );
    assert_eq!(plugins.window(&instance), (1, 99, 3, 2.0));
    assert_eq!(instance.poll(), []);
    // macOS scales windows itself. An open window closes first.
    let parent = ParentWindow::Cocoa(ptr::without_provenance_mut(55));
    assert_eq!(instance.open_window(parent, 2.0), Ok((400, 300)));
    assert_eq!(plugins.window(&instance), (1, 55, 2, 1.0));
    // VST3's windows only go inside the host's.
    assert!(instance.open_floating_window("Test Gain").is_err());
    // Destroying the plugin closes its window first.
    drop(instance);
    plugins.assert_alright();
}

#[test]
fn an_arpeggiator_plays_notes_of_its_own() {
    let plugins = TestPlugins::new("arpeggiator");
    let mut instance = plugins.arpeggiator();
    assert_eq!(instance.info().kind(), PluginKind::MidiEffect);
    // An octave up, of two either way.
    instance.set_param(TRANSPOSE, 0.75);
    instance.activate(CONFIG).unwrap();
    assert_arpeggiated(&arpeggiate(instance.node_source().node()));
    drop(instance);
    plugins.assert_alright();
}

fn scan_bundle(name: &str, timeout: Duration) -> Outcome {
    let (bundle, _) = testing::copy_vst3(&temp_dir(name), name);
    scan_path(&bundle, timeout)
}

#[test]
fn the_scanner_finds_vst3_plugins_and_survives_bad_ones() {
    let Outcome::Found(found) = scan_bundle("good", scan::TIMEOUT) else {
        panic!("the good bundle wasn't found");
    };
    let names: Vec<_> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Test Gain", "Test Synth", "Test Arpeggiator"]);
    assert_eq!(
        (found[0].format, found[0].id.as_str()),
        (PluginFormat::Vst3, VST3_GAIN_ID)
    );
    let kinds: Vec<_> = found.iter().map(PluginInfo::kind).collect();
    assert_eq!(
        kinds,
        [
            PluginKind::AudioEffect,
            PluginKind::Instrument,
            PluginKind::MidiEffect
        ]
    );
    // The arpeggiator says nothing of what it is but its buses.
    assert_eq!(
        found[2].buses,
        Some(Buses {
            notes_in: 1,
            notes_out: 1,
            ..Buses::default()
        })
    );
    let crashed = scan_bundle("crash", scan::TIMEOUT);
    assert!(matches!(&crashed, Outcome::Crashed(_)), "{crashed:?}");
    let hung = scan_bundle("hang", Duration::from_secs(1));
    assert!(
        matches!(&hung, Outcome::Crashed(reason) if reason.contains("still loading")),
        "{hung:?}"
    );
    let refused = scan_bundle("refuse", scan::TIMEOUT);
    assert!(
        matches!(&refused, Outcome::Failed(reason) if reason.contains("failed to initialize")),
        "{refused:?}"
    );
}

//! Plugins on tracks, through the app's session, with the test plugins
//! (`crates/test-plugins`), CLAP and VST3 alike. A plugin loads once a scan
//! says it's installed, plays, and comes back from the project's file as it
//! was saved. Changes made in its window are steps in the undo history, and
//! the ones it makes on its own aren't, so redo carries on past them. A
//! plugin moved to another track plays on there, its window open, and a
//! copy loads a plugin of its own. Its parameters follow their automation
//! until someone turns them by hand. Its own Bypass is its device's. A
//! missing plugin lets the sound through and says why. Delay compensation
//! lines a latent plugin up with a dry track to the sample, also after the
//! plugin's latency changes. A plugin's side inputs go into its device as
//! it lists them, and a muted kick wired into one ducks a bass through the
//! plugin, lined up to the sample whichever track has latency. An
//! arpeggiator in front of a synth plays it the notes of its track's clips
//! in turn, and bypassed or deactivated lets the notes through. A second
//! track doubles what it plays, taking the notes after it or at its
//! track's Post FX, with the first track muted. Four tracks without an
//! instrument play four parts of a multi-timbral synth on another, through
//! their MIDI To, each on a MIDI channel of its own, and mute and solo cut a
//! part as they would its track. And the
//! app's binary scans each library in a process of its own, so a library
//! that crashes takes only that process down.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{ptr, thread};

use musicable_app::{PLUGIN_SETTLING, SCAN_PLUGIN, Session};
use musicable_commands::{Args, Keymap, Value, automation, device, edit, mixer, transport, wire};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent, MidiClip};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{
    Device, DeviceId, DeviceKind, PluginFormat, Project, TestTone, TrackId, TrackKind,
};
use musicable_plugins::scan::{self, Outcome};
use musicable_plugins::{Instance, ParentWindow, PluginInfo};
use musicable_test_plugins::testing::{self, Library};
use musicable_test_plugins::vst3::{
    VST3_ARPEGGIATOR_ID, VST3_GAIN_ID, VST3_SIDECHAIN, VST3_SYNTH_ID,
};
use musicable_test_plugins::{
    ARP_STEP, ARPEGGIATOR_ID, BYPASS, GAIN, GAIN_ID, LATENCY, SIDECHAIN, SYNTH_ID, TRANSPOSE, gui,
};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// A folder of its own for each test, made fresh.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "musicable-app-plugins-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The test plugins in one format, and what tests do to them in it.
struct TestPlugins {
    format: PluginFormat,
    /// The test's folder, which they're in.
    dir: PathBuf,
    library: Library,
}

impl TestPlugins {
    /// The test plugins in `format`, in a folder of their own for the test
    /// `test`, so it gets the library's statics to itself.
    fn new(format: PluginFormat, test: &str) -> Self {
        let name = match format {
            PluginFormat::Clap => format!("{test}-clap"),
            PluginFormat::Vst3 => format!("{test}-vst3"),
        };
        let dir = folder(&name);
        let library = match format {
            PluginFormat::Clap => Library::copy(&dir, &name),
            PluginFormat::Vst3 => Library::copy_vst3(&dir, &name),
        };
        Self {
            format,
            dir,
            library,
        }
    }

    /// What a scan finds: Test Gain, Test Synth and Test Arpeggiator.
    fn installed(&self) -> Vec<PluginInfo> {
        scan::scan_here(self.library.path()).unwrap()
    }

    /// A Test Gain device.
    fn gain(&self, project: &mut Project, settings: PluginSettings) -> Device {
        let id = match self.format {
            PluginFormat::Clap => GAIN_ID.to_str().unwrap(),
            PluginFormat::Vst3 => VST3_GAIN_ID,
        };
        let kind = PluginKind::AudioEffect;
        plugin(project, self.format, id, "Test Gain", kind, settings)
    }

    /// A Test Synth device.
    fn synth(&self, project: &mut Project) -> Device {
        let id = match self.format {
            PluginFormat::Clap => SYNTH_ID.to_str().unwrap(),
            PluginFormat::Vst3 => VST3_SYNTH_ID,
        };
        let (kind, settings) = (PluginKind::Instrument, PluginSettings::default());
        plugin(project, self.format, id, "Test Synth", kind, settings)
    }

    /// A Test Arpeggiator device.
    fn arpeggiator(&self, project: &mut Project, settings: PluginSettings) -> Device {
        let id = match self.format {
            PluginFormat::Clap => ARPEGGIATOR_ID.to_str().unwrap(),
            PluginFormat::Vst3 => VST3_ARPEGGIATOR_ID,
        };
        let kind = PluginKind::MidiEffect;
        plugin(project, self.format, id, "Test Arpeggiator", kind, settings)
    }

    /// Test Gain's Gain for `factor`: the factor itself in CLAP, and
    /// normalized from 0 to 2 in VST3.
    fn factor(&self, factor: f64) -> f64 {
        match self.format {
            PluginFormat::Clap => factor,
            PluginFormat::Vst3 => factor / 2.0,
        }
    }

    /// Test Gain's Latency for `frames` frames: the frames in CLAP, and
    /// normalized from 0 to 48,000 in VST3.
    fn latency(&self, frames: u32) -> f64 {
        match self.format {
            PluginFormat::Clap => f64::from(frames),
            PluginFormat::Vst3 => f64::from(frames) / 48_000.0,
        }
    }

    /// Test Arpeggiator's Transpose for `semitones`: the semitones in
    /// CLAP, and normalized from -24 to 24 in VST3.
    fn transpose(&self, semitones: f64) -> f64 {
        match self.format {
            PluginFormat::Clap => semitones,
            PluginFormat::Vst3 => (semitones + 24.0) / 48.0,
        }
    }

    /// Test Gain's side input, as its device lists it.
    fn sidechain(&self) -> PluginInput {
        let id = match self.format {
            PluginFormat::Clap => SIDECHAIN,
            PluginFormat::Vst3 => VST3_SIDECHAIN,
        };
        PluginInput {
            id,
            name: "Sidechain".into(),
        }
    }

    // SAFETY, for the hooks below: each is given the instance's plugin, of
    // this copy, on its main thread.

    /// Does what someone does in the plugin's window: see [`gui`].
    fn gui(&self, instance: &Instance, action: u32, param: u32, value: f64) {
        match instance {
            // SAFETY: as above.
            Instance::Clap(plugin) => unsafe {
                self.library.gui(plugin.raw(), action, param, value)
            },
            Instance::Vst3(plugin) => {
                let controller = plugin.raw_controller();
                // SAFETY: as above.
                let taken = unsafe { self.library.vst3_gui(controller, action, param, value) };
                assert!(taken, "the host didn't take the edit");
            }
        }
    }

    /// Sets the plugin's label, a state its parameters don't say, as
    /// someone would in its window.
    fn set_label(&self, instance: &Instance, label: &str) {
        match instance {
            // SAFETY: as above.
            Instance::Clap(plugin) => unsafe { self.library.set_label(plugin.raw(), label) },
            Instance::Vst3(plugin) => {
                let controller = plugin.raw_controller();
                // SAFETY: as above.
                let sent = unsafe { self.library.vst3_set_label(controller, label) };
                assert!(sent, "the label didn't get to the component");
            }
        }
    }

    /// The plugin's label, as its processor has it.
    fn label(&self, instance: &Instance) -> String {
        match instance {
            // SAFETY: as above.
            Instance::Clap(plugin) => unsafe { self.library.label(plugin.raw()) },
            // SAFETY: as above.
            Instance::Vst3(plugin) => unsafe { self.library.vst3_label(plugin.raw_component()) }
                .expect("Test Gain's component has a label"),
        }
    }

    fn assert_alright(&self) {
        self.library.assert_alright();
    }
}

/// A plugin device for the `format` plugin with id `id`, called `name`.
fn plugin(
    project: &mut Project,
    format: PluginFormat,
    id: &str,
    name: &str,
    kind: PluginKind,
    settings: PluginSettings,
) -> Device {
    project.new_device(DeviceKind::Plugin(PluginDevice {
        format,
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        inputs: Vec::new(),
        note_output: false,
        settings,
    }))
}

fn with_param(param: u32, value: f64) -> PluginSettings {
    let mut settings = PluginSettings::default();
    settings.set_param(param, Some(value));
    settings
}

/// The peaks of the test tone `tone_with` plays, straight through.
const TONE: [f32; 2] = [0.5, 0.5];

/// A track playing a test tone, with `device` on it.
fn tone_with(device: impl FnOnce(&mut Project) -> Device) -> (Project, Device) {
    let mut project = Project::new();
    let tone = project.add_track("Tone", TrackKind::Audio);
    let device = device(&mut project);
    let track = project.track_mut(tone).unwrap();
    track.tone = Some(TestTone {
        frequency: 440.0,
        level: 0.5,
    });
    track.devices.push(device.clone());
    (project, device)
}

/// A session, and an engine that renders when told to, standing in for the
/// audio device.
struct Rig {
    engine: Engine,
    session: Session,
}

impl Rig {
    /// Opens `project`, whose file is in `folder`, waits for its graph, as
    /// the app's first frames would, and plays: test tones sound only while
    /// the transport plays.
    fn new(project: Project, folder: Option<&Path>) -> Self {
        let (engine, handle) = Engine::new(CONFIG);
        let mut session = Session::new(project, folder, Keymap::default_keymap(), handle);
        session.wait_for_graph();
        session.run(&transport::PLAY).unwrap();
        Self { engine, session }
    }

    /// Finds the project's plugins among the test plugins, as a scan
    /// finishing would, and waits for the graph with them.
    fn install(&mut self, plugins: &TestPlugins) {
        self.session.set_installed_plugins(plugins.installed());
        self.session.wait_for_graph();
    }

    /// Renders `frames` frames as the audio thread would, ticking the
    /// session as the UI would, and returns them interleaved.
    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut output = vec![0.0; 2 * frames];
        for block in output.chunks_mut(2 * 240) {
            self.engine.process_interleaved(block, 2);
            self.session.tick();
        }
        let stats = self.session.stats();
        assert_eq!((stats.events_late, stats.events_dropped), (0, 0));
        output
    }

    /// Renders a fifth of a second and returns the peak of each channel over
    /// its last half, when any change has long settled. It waits first for
    /// any graph a command needs, since under load one can take longer to
    /// compile than the render takes, and after for any graph a change in a
    /// plugin needs.
    fn peaks(&mut self) -> [f32; 2] {
        self.session.wait_for_graph();
        let frames = 9_600;
        let output = self.render(frames);
        self.session.wait_for_graph();
        let mut peaks = [0.0_f32; 2];
        for frame in output[frames..].chunks(2) {
            for (peak, sample) in peaks.iter_mut().zip(frame) {
                *peak = peak.max(sample.abs());
            }
        }
        peaks
    }

    fn settings(&self, device: DeviceId) -> PluginSettings {
        let project = self.session.project();
        let (_, _, found) = project.find_device(device).unwrap();
        found.kind.plugin().unwrap().settings.clone()
    }

    fn inputs(&self, device: DeviceId) -> Vec<PluginInput> {
        let project = self.session.project();
        let (_, _, found) = project.find_device(device).unwrap();
        found.kind.plugin().unwrap().inputs.clone()
    }

    /// Plays `frames` frames from the start of the song, and returns its
    /// left channel and its right. It stops for a while first, so what
    /// delay compensation held back of what played before is gone.
    fn play_from_start(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        self.session.run(&transport::STOP).unwrap();
        self.render(4_800);
        self.session.run(&transport::GO_TO_START).unwrap();
        self.session.run(&transport::PLAY).unwrap();
        channels(&self.render(frames))
    }
}

#[track_caller]
fn near(peaks: [f32; 2], expected: [f32; 2]) {
    for (peak, expected) in peaks.into_iter().zip(expected) {
        assert!(
            (peak - expected).abs() < 0.01,
            "{peaks:?}, expected {expected:?}"
        );
    }
}

fn times(peaks: [f32; 2], by: f32) -> [f32; 2] {
    peaks.map(|peak| peak * by)
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn a_clap_plugin_plays_on_its_track_and_comes_back_as_it_was_saved() {
    plays_and_comes_back_as_it_was_saved(PluginFormat::Clap);
}

#[test]
fn a_vst3_plugin_plays_on_its_track_and_comes_back_as_it_was_saved() {
    plays_and_comes_back_as_it_was_saved(PluginFormat::Vst3);
}

fn plays_and_comes_back_as_it_was_saved(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "saved");
    let quarter = with_param(GAIN, plugins.factor(0.25));
    let (song, device) = tone_with(|p| plugins.gain(p, quarter));
    let mut rig = Rig::new(song, None);

    // Until a scan says what's installed, the device lets the sound
    // through.
    near(rig.peaks(), TONE);
    rig.install(&plugins);
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    let wet = rig.peaks();
    near(wet, times(TONE, 0.25));

    // State that isn't a parameter goes into the project.
    plugins.set_label(rig.session.plugin(device.id).unwrap(), "Warm");
    rig.session.tick();
    assert!(rig.settings(device.id).state.is_some());
    let path = plugins.dir.join("Song.musicable");
    project::save(&path, &rig.session.saved_project()).unwrap();
    drop(rig);

    // Opened again, it plays as it did, and has its label back.
    let opened = project::load(&path).unwrap();
    let mut rig = Rig::new(opened, Some(&plugins.dir));
    rig.install(&plugins);
    let instance = rig.session.plugin(device.id).unwrap();
    assert_eq!(plugins.label(instance), "Warm");
    near(rig.peaks(), wet);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn changes_in_a_clap_plugins_window_are_steps_in_the_undo_history() {
    changes_in_its_window_are_steps_in_the_undo_history(PluginFormat::Clap);
}

#[test]
fn changes_in_a_vst3_plugins_window_are_steps_in_the_undo_history() {
    changes_in_its_window_are_steps_in_the_undo_history(PluginFormat::Vst3);
}

fn changes_in_its_window_are_steps_in_the_undo_history(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "window");
    let (song, device) = tone_with(|p| plugins.gain(p, PluginSettings::default()));
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    near(rig.peaks(), TONE);

    // Someone drags the gain's knob down to a half, in two moves.
    let instance = rig.session.plugin(device.id).unwrap();
    plugins.gui(instance, gui::BEGIN, GAIN, 0.0);
    plugins.gui(instance, gui::VALUE, GAIN, plugins.factor(0.75));
    plugins.gui(instance, gui::VALUE, GAIN, plugins.factor(0.5));
    plugins.gui(instance, gui::END, GAIN, 0.0);
    let half = times(TONE, 0.5);
    near(rig.peaks(), half);
    let gain = rig.settings(device.id).param(GAIN);
    assert_eq!(gain, Some(plugins.factor(0.5)));
    assert_eq!(rig.session.undo_name(), Some("Change Test Gain"));

    // One undo takes the whole drag back, in the plugin too, and redo puts
    // it back.
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), TONE);
    assert_eq!(rig.settings(device.id).param(GAIN), None);
    let instance = rig.session.plugin(device.id).unwrap();
    assert_eq!(instance.param_value(GAIN), Some(plugins.factor(1.0)));
    assert_eq!(rig.session.undo_name(), None);
    rig.session.run(&edit::REDO).unwrap();
    near(rig.peaks(), half);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_clap_plugins_own_changes_are_no_steps_in_the_undo_history() {
    own_changes_are_no_steps_in_the_undo_history(PluginFormat::Clap);
}

#[test]
fn a_vst3_plugins_own_changes_are_no_steps_in_the_undo_history() {
    own_changes_are_no_steps_in_the_undo_history(PluginFormat::Vst3);
}

fn own_changes_are_no_steps_in_the_undo_history(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "own");
    let mut song = Project::new();
    let tone = song.add_track("Tone", TrackKind::Audio);
    song.track_mut(tone).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.5,
    });
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);

    // Test Gain goes on the track, and its gain to a half.
    let (kind, id) = match format {
        PluginFormat::Clap => ("clap", GAIN_ID.to_str().unwrap()),
        PluginFormat::Vst3 => ("vst3", VST3_GAIN_ID),
    };
    let args = Args::new()
        .with("format", kind)
        .with("plugin", id)
        .with("name", "Test Gain")
        .with("track", tone);
    let Ok(Value::Device(gain)) = rig.session.run_with(&device::ADD_PLUGIN, &args) else {
        panic!("Test Gain didn't go on the track");
    };
    let args = Args::new()
        .with("device", gain)
        .with("param", i64::from(GAIN))
        .with("value", plugins.factor(0.5));
    rig.session.run_with(&device::SET_PARAM, &args).unwrap();
    let half = times(TONE, 0.5);
    near(rig.peaks(), half);

    // Both undone, then the plugin redone: it loads afresh, and changes its
    // state as it starts, as some plugins do. The project follows, but
    // that's no step in the undo history, so the gain can still be redone.
    rig.session.run(&edit::UNDO).unwrap();
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), TONE);
    rig.session.run(&edit::REDO).unwrap();
    rig.session.wait_for_graph();
    plugins.set_label(rig.session.plugin(gain).unwrap(), "Warm");
    rig.session.tick();
    let warm = rig.settings(gain).state;
    assert!(warm.is_some());
    assert_eq!(rig.session.undo_name(), Some("Add Test Gain"));
    assert_eq!(rig.session.redo_name(), Some("Change Test Gain"));
    rig.session.run(&edit::REDO).unwrap();
    near(rig.peaks(), half);
    assert_eq!(rig.settings(gain).state, warm);

    // Once it has settled, a change in its window is a step: someone picked
    // a preset, say.
    let parent = ParentWindow::Win32(ptr::without_provenance_mut(77));
    let instance = rig.session.plugin_mut(gain).unwrap();
    instance.open_window(parent, 1.0).unwrap();
    thread::sleep(PLUGIN_SETTLING);
    plugins.set_label(rig.session.plugin(gain).unwrap(), "Bright");
    rig.session.tick();
    let bright = rig.settings(gain).state;
    assert_ne!(bright, warm);

    // Undo takes it back, and the plugin says its state changed as it loads
    // the one from before, as plugins do: that's its own doing too, with
    // its window open, so redo can still make the change again.
    rig.session.run(&edit::UNDO).unwrap();
    assert_eq!(rig.settings(gain).state, warm);
    let instance = rig.session.plugin(gain).unwrap();
    assert_eq!(plugins.label(instance), "Warm");
    plugins.set_label(instance, "Warm");
    rig.session.tick();
    assert_eq!(rig.session.redo_name(), Some("Change Test Gain"));
    rig.session.run(&edit::REDO).unwrap();
    assert_eq!(rig.settings(gain).state, bright);
    let instance = rig.session.plugin(gain).unwrap();
    assert_eq!(plugins.label(instance), "Bright");
    near(rig.peaks(), half);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_clap_plugin_moves_with_its_window_open_and_a_copy_loads_its_own() {
    moves_and_copies(PluginFormat::Clap);
}

#[test]
fn a_vst3_plugin_moves_with_its_window_open_and_a_copy_loads_its_own() {
    moves_and_copies(PluginFormat::Vst3);
}

fn moves_and_copies(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "moved");
    let quarter = with_param(GAIN, plugins.factor(0.25));
    let (mut song, gain) = tone_with(|p| plugins.gain(p, quarter));
    // A quieter tone on a track of its own, so each track's gain is heard.
    let quiet = song.add_track("Quiet", TrackKind::Audio);
    song.track_mut(quiet).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.25,
    });
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    let first = [0.5 * 0.25 + 0.25; 2];
    near(rig.peaks(), first);
    // What's alive of one plugin: a VST3 plugin's component and controller.
    let one = plugins.library.alive();
    let parent = ParentWindow::Win32(ptr::without_provenance_mut(77));
    let instance = rig.session.plugin_mut(gain.id).unwrap();
    instance.open_window(parent, 1.0).unwrap();
    let open = plugins.library.alive();

    // Moved to the quiet track, it plays there, the same plugin, with its
    // window still open, and undone, it goes back the same way.
    let to_quiet = Args::new()
        .with("devices", vec![gain.id])
        .with("track", quiet);
    rig.session.run_with(&device::MOVE, &to_quiet).unwrap();
    assert_eq!(rig.session.undo_name(), Some("Move Test Gain"));
    near(rig.peaks(), [0.5 + 0.25 * 0.25; 2]);
    assert!(rig.session.plugin(gain.id).unwrap().is_window_open());
    assert_eq!(plugins.library.alive(), open);
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), first);
    let instance = rig.session.plugin_mut(gain.id).unwrap();
    assert!(instance.is_window_open());
    assert_eq!(plugins.library.alive(), open);

    // A copy loads a plugin of its own, with the state the original's has,
    // though the project hasn't caught up with it yet.
    instance.close_window();
    let closed = plugins.library.alive();
    plugins.set_label(rig.session.plugin(gain.id).unwrap(), "Warm");
    let Ok(Value::List(copies)) = rig.session.run_with(&device::COPY, &to_quiet) else {
        panic!("no copies");
    };
    let [Value::Device(copy)] = copies[..] else {
        panic!("one copy, not {copies:?}");
    };
    near(rig.peaks(), [(0.5 + 0.25) * 0.25; 2]);
    assert_eq!(plugins.library.alive(), closed + one);
    assert_eq!(plugins.label(rig.session.plugin(copy).unwrap()), "Warm");
    assert_eq!(rig.session.undo_name(), Some("Copy Test Gain"));
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), first);
    assert_eq!(plugins.library.alive(), closed);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_clap_plugins_parameter_follows_its_automation() {
    follows_its_automation(PluginFormat::Clap);
}

#[test]
fn a_vst3_plugins_parameter_follows_its_automation() {
    follows_its_automation(PluginFormat::Vst3);
}

fn follows_its_automation(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "automation");
    let (song, device) = tone_with(|p| plugins.gain(p, PluginSettings::default()));
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    near(rig.peaks(), TONE);

    // The gain's lane holds it at a quarter. The parameter's own value
    // stays as it was, under the automation.
    let gain = ParamAddress::Device(device.id, GAIN);
    let args = Args::new()
        .with("param", gain)
        .with("at", vec![Beats::ZERO])
        .with("values", vec![plugins.factor(0.25)]);
    rig.session
        .run_with(&automation::SET_POINTS, &args)
        .unwrap();
    let quarter = times(TONE, 0.25);
    near(rig.peaks(), quarter);
    assert_eq!(rig.settings(device.id).param(GAIN), None);

    // Someone turns the knob in the plugin's window, which overrides the
    // automation: the knob's value plays.
    let instance = rig.session.plugin(device.id).unwrap();
    plugins.gui(instance, gui::BEGIN, GAIN, 0.0);
    plugins.gui(instance, gui::VALUE, GAIN, plugins.factor(0.5));
    plugins.gui(instance, gui::END, GAIN, 0.0);
    let half = times(TONE, 0.5);
    near(rig.peaks(), half);
    assert!(!rig.session.project().is_automated(gain));
    assert_eq!(
        rig.settings(device.id).param(GAIN),
        Some(plugins.factor(0.5))
    );

    // Undo takes the turn back, so the automation plays again. Redo turns
    // it again, and re-enabling the automation has it play once more.
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), quarter);
    assert!(rig.session.project().is_automated(gain));
    rig.session.run(&edit::REDO).unwrap();
    near(rig.peaks(), half);
    rig.session.run(&automation::RE_ENABLE).unwrap();
    near(rig.peaks(), quarter);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_plugins_own_bypass_is_its_devices() {
    // Test Gain's Bypass is CLAP's: the VST3 one has none.
    let plugins = TestPlugins::new(PluginFormat::Clap, "own-bypass");
    let (song, device) = tone_with(|p| plugins.gain(p, with_param(GAIN, 0.25)));
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    let wet = times(TONE, 0.25);
    near(rig.peaks(), wet);
    let bypassed = |rig: &Rig| {
        let (_, _, found) = rig.session.project().find_device(device.id).unwrap();
        found.bypassed
    };
    let own = |rig: &Rig| rig.session.plugin(device.id).unwrap().param_value(BYPASS);

    // Clicked in the plugin's window, its Bypass bypasses the device, as a
    // step in the undo history. It isn't one of the plugin's settings.
    let instance = rig.session.plugin(device.id).unwrap();
    plugins.gui(instance, gui::BEGIN, BYPASS, 0.0);
    plugins.gui(instance, gui::VALUE, BYPASS, 1.0);
    plugins.gui(instance, gui::END, BYPASS, 0.0);
    near(rig.peaks(), TONE);
    assert!(bypassed(&rig));
    assert_eq!(rig.session.undo_name(), Some("Bypass Test Gain"));
    assert_eq!(rig.settings(device.id).param(BYPASS), None);

    // Undone, the device comes back, and the plugin's Bypass turns off.
    rig.session.run(&edit::UNDO).unwrap();
    assert!(!bypassed(&rig));
    assert_eq!(own(&rig), Some(0.0));
    near(rig.peaks(), wet);

    // Bypassing the device turns the plugin's Bypass on, once the device
    // has faded out and the plugin has stopped.
    let set = |bypassed: bool| {
        Args::new()
            .with("devices", vec![device.id])
            .with("bypassed", bypassed)
    };
    rig.session
        .run_with(&device::SET_BYPASS, &set(true))
        .unwrap();
    assert_eq!(own(&rig), Some(0.0));
    near(rig.peaks(), TONE);
    assert_eq!(own(&rig), Some(1.0));

    // A project the device isn't bypassed in plays it, whatever the
    // plugin's state says.
    let mut saved = rig.session.saved_project();
    saved.device_mut(device.id).unwrap().bypassed = false;
    drop(rig);
    let mut rig = Rig::new(saved, None);
    rig.install(&plugins);
    assert_eq!(own(&rig), Some(0.0));
    near(rig.peaks(), wet);

    // While a lane moves the device's Bypass, the plugin's stays off.
    let args = Args::new()
        .with("param", ParamAddress::Bypass(device.id))
        .with("at", vec![Beats::ZERO])
        .with("values", vec![1.0]);
    rig.session
        .run_with(&automation::SET_POINTS, &args)
        .unwrap();
    near(rig.peaks(), TONE);
    assert_eq!(own(&rig), Some(0.0));
    rig.session
        .run_with(&device::SET_BYPASS, &set(false))
        .unwrap();
    near(rig.peaks(), wet);
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_missing_plugin_lets_the_sound_through_and_says_why() {
    let plugins = TestPlugins::new(PluginFormat::Clap, "missing");
    let settings = PluginSettings::default();
    let reverb = |p: &mut Project| {
        let (id, kind) = ("org.example.reverb", PluginKind::AudioEffect);
        plugin(p, PluginFormat::Clap, id, "Old Reverb", kind, settings)
    };
    let (song, device) = tone_with(reverb);
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    let why = "Old Reverb isn't installed, or Musicable hasn't found it: it plays nothing";
    assert_eq!(rig.session.take_problems(), [why]);
    assert_eq!(rig.session.plugin_problem(device.id), Some(why));
    assert!(rig.session.plugin(device.id).is_none());
    near(rig.peaks(), TONE);
    // It isn't tried again until the plugins installed change.
    rig.session.tick();
    assert!(rig.session.take_problems().is_empty());
    drop(rig);
    plugins.assert_alright();
}

/// The left channel and the right, of interleaved stereo.
fn channels(output: &[f32]) -> (Vec<f32>, Vec<f32>) {
    output.chunks(2).map(|frame| (frame[0], frame[1])).unzip()
}

/// Where the first sample louder than a half is.
fn first_click(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|sample| sample.abs() > 0.5)
}

#[test]
fn delay_compensation_lines_a_latent_clap_plugin_up_with_a_dry_track() {
    delay_compensation_lines_it_up_with_a_dry_track(PluginFormat::Clap);
}

#[test]
fn delay_compensation_lines_a_latent_vst3_plugin_up_with_a_dry_track() {
    delay_compensation_lines_it_up_with_a_dry_track(PluginFormat::Vst3);
}

fn delay_compensation_lines_it_up_with_a_dry_track(format: PluginFormat) {
    // Two tracks play the same clicks, one of them through Test Gain, which
    // reports its latency. Panned apart, the two come out the same, to the
    // sample.
    let plugins = TestPlugins::new(format, "compensation");
    let dir = &plugins.dir;
    let clicks: Vec<f32> = (0..48_000)
        .map(|i| match i % 4_800 {
            0 => 1.0,
            n => ((n * 7_919) % 101) as f32 / 1_000.0,
        })
        .collect();
    save_wav(
        &dir.join("Clicks.wav"),
        48_000,
        SampleFormat::Float32,
        &[&clicks],
    )
    .unwrap();
    let mut song = Project::new();
    let wet = song.add_track("Wet", TrackKind::Audio);
    let dry = song.add_track("Dry", TrackKind::Audio);
    for (track, pan) in [(wet, -1.0), (dry, 1.0)] {
        let range = BeatRange::new(Beats::ZERO, Beats::new(2));
        let clicks = ClipContent::Audio(AudioClip::new("Clicks.wav"));
        let clip = song.new_clip("Clicks", range, clicks);
        let track = song.track_mut(track).unwrap();
        track.clips.push(clip);
        track.pan = pan;
    }
    let device = plugins.gain(&mut song, with_param(LATENCY, plugins.latency(1_000)));
    song.track_mut(wet).unwrap().devices.push(device.clone());
    let mut rig = Rig::new(song, Some(dir));
    rig.install(&plugins);

    let play = |rig: &mut Rig| {
        rig.session.run(&transport::STOP).unwrap();
        rig.session.run(&transport::GO_TO_START).unwrap();
        rig.session.run(&transport::PLAY).unwrap();
        let (left, right) = channels(&rig.render(24_000));
        let differs = left.iter().zip(&right).position(|(l, r)| l != r);
        assert_eq!(differs, None, "the wet track and the dry one line up");
        first_click(&left)
    };
    // The whole mix waits for the plugin.
    assert_eq!(play(&mut rig), Some(1_000));
    assert_eq!(rig.session.stats().latency, 1_000);

    // Someone sets a longer latency in the plugin's window. The plugin
    // restarts with it, and the graph is built again around it, where the
    // dry track jumps in time to line up again. Once what played before
    // the jump has played out, the two line up as before.
    let instance = rig.session.plugin(device.id).unwrap();
    plugins.gui(instance, gui::BEGIN, LATENCY, 0.0);
    plugins.gui(instance, gui::VALUE, LATENCY, plugins.latency(3_000));
    plugins.gui(instance, gui::END, LATENCY, 0.0);
    rig.render(4_800);
    rig.session.wait_for_graph();
    rig.render(4_800);
    assert_eq!(rig.session.stats().latency, 3_000);
    let latency = rig.settings(device.id).param(LATENCY);
    assert_eq!(latency, Some(plugins.latency(3_000)));
    assert_eq!(play(&mut rig), Some(3_000));
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_muted_kick_ducks_a_bass_through_a_clap_plugins_side_input() {
    ducks_a_bass_through_a_side_input(PluginFormat::Clap);
}

#[test]
fn a_muted_kick_ducks_a_bass_through_a_vst3_plugins_side_input() {
    ducks_a_bass_through_a_side_input(PluginFormat::Vst3);
}

/// How many frames apart the kick's clicks are, and how long each is.
const BEAT: usize = 4_800;
const CLICK: usize = 10;

fn ducks_a_bass_through_a_side_input(format: PluginFormat) {
    // The kick clicks at full scale, muted. The bass holds a quarter
    // through Test Gain, its ducker, whose side input the kick keys: where
    // the kick clicks, the bass goes silent, to the sample, whichever
    // track has latency. Two more Test Gains give the tracks latency.
    let plugins = TestPlugins::new(format, "sidechain");
    let dir = &plugins.dir;
    let clicks: Vec<f32> = (0..48_000)
        .map(|i| if i % BEAT < CLICK { 1.0 } else { 0.0 })
        .collect();
    let float = SampleFormat::Float32;
    save_wav(&dir.join("Kick.wav"), 48_000, float, &[&clicks]).unwrap();
    save_wav(&dir.join("Bass.wav"), 48_000, float, &[&[0.25; 48_000]]).unwrap();
    let mut song = Project::new();
    let kick = song.add_track("Kick", TrackKind::Audio);
    let bass = song.add_track("Bass", TrackKind::Audio);
    for (track, file) in [(kick, "Kick.wav"), (bass, "Bass.wav")] {
        let range = BeatRange::new(Beats::ZERO, Beats::new(2));
        let clip = song.new_clip(file, range, ClipContent::Audio(AudioClip::new(file)));
        song.track_mut(track).unwrap().clips.push(clip);
    }
    let kick_latency = plugins.gain(&mut song, PluginSettings::default());
    let bass_latency = plugins.gain(&mut song, PluginSettings::default());
    let ducker = plugins.gain(&mut song, PluginSettings::default());
    let track = song.track_mut(kick).unwrap();
    track.muted = true;
    track.devices.push(kick_latency.clone());
    let track = song.track_mut(bass).unwrap();
    track.devices.extend([bass_latency.clone(), ducker.clone()]);
    let mut rig = Rig::new(song, Some(dir));
    rig.install(&plugins);

    // As it loads, the plugin lists its side input, which its device takes.
    assert_eq!(rig.inputs(ducker.id), []);
    rig.render(240);
    assert_eq!(rig.inputs(ducker.id), [plugins.sidechain()]);
    // What the bass plays, ducked or not, `latency` frames late.
    let bass_with = |latency: usize, ducked: bool| -> Vec<f32> {
        (0..24_000_usize)
            .map(|i| match i.checked_sub(latency) {
                Some(i) if !ducked || i % BEAT >= CLICK => 0.25,
                _ => 0.0,
            })
            .collect()
    };
    let (left, right) = rig.play_from_start(24_000);
    assert_eq!(
        (&left, &right),
        (&bass_with(0, false), &bass_with(0, false))
    );

    let from_kick = Args::new()
        .with("track", kick)
        .with("tap", "post_fader")
        .with("device", ducker.id);
    let Value::Wire(key) = rig.session.run_with(&wire::CONNECT, &from_kick).unwrap() else {
        panic!("a wire");
    };
    rig.session.wait_for_graph();
    let (left, right) = rig.play_from_start(24_000);
    assert_eq!((&left, &right), (&bass_with(0, true), &bass_with(0, true)));

    // Someone gives one track latency, then the other, in a Test Gain's
    // window: the ducker hears the kick lined up all the same.
    let set_latency = |rig: &mut Rig, device: DeviceId, frames: u32| {
        let instance = rig.session.plugin(device).unwrap();
        plugins.gui(instance, gui::BEGIN, LATENCY, 0.0);
        plugins.gui(instance, gui::VALUE, LATENCY, plugins.latency(frames));
        plugins.gui(instance, gui::END, LATENCY, 0.0);
        rig.render(4_800);
        rig.session.wait_for_graph();
        rig.render(4_800);
    };
    for (device, other) in [
        (kick_latency.id, bass_latency.id),
        (bass_latency.id, kick_latency.id),
    ] {
        set_latency(&mut rig, other, 0);
        set_latency(&mut rig, device, 37);
        assert_eq!(rig.session.stats().latency, 37);
        let (left, right) = rig.play_from_start(24_000);
        assert_eq!(
            (&left, &right),
            (&bass_with(37, true), &bass_with(37, true))
        );
    }

    // Disconnected, the side input is off again, and the bass plays on.
    rig.session
        .run_with(&wire::DISCONNECT, &Args::new().with("wire", key))
        .unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(24_000);
    assert_eq!(left, bass_with(37, false));
    // Undone, the wire's back, and the side input with it.
    rig.session.run(&edit::UNDO).unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(24_000);
    assert_eq!(left, bass_with(37, true));
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_clap_arpeggiator_plays_a_synth_in_its_track() {
    arpeggiator_plays_a_synth_in_its_track(PluginFormat::Clap);
}

#[test]
fn a_vst3_arpeggiator_plays_a_synth_in_its_track() {
    arpeggiator_plays_a_synth_in_its_track(PluginFormat::Vst3);
}

/// How many times `samples` cross zero.
fn crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count()
}

/// The peak of `samples`.
fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |peak, s| peak.max(s.abs()))
}

#[track_caller]
fn assert_plays_key(samples: &[f32], key: u8) {
    let frequency = 440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0);
    let expected = 2.0 * frequency * samples.len() as f64 / f64::from(CONFIG.sample_rate);
    let crossed = crossings(samples) as f64;
    assert!(
        (crossed - expected).abs() < 2.0,
        "{crossed} crossings, and key {key} makes {expected:.1}"
    );
    // Test Synth's Level, at velocity 1.
    let peak = peak(samples);
    assert!((peak - 0.5).abs() < 0.01, "a peak of {peak}");
}

fn arpeggiator_plays_a_synth_in_its_track(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "arpeggiator");
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    // C and E, held for a beat: half a second.
    let mut chord = MidiClip::default();
    for key in [60, 64] {
        chord.add_note(Beats::ZERO, Beats::new(1), key, 1.0);
    }
    let range = BeatRange::new(Beats::ZERO, Beats::new(1));
    let clip = song.new_clip("Chord", range, ClipContent::Midi(chord));
    let up = with_param(TRANSPOSE, plugins.transpose(12.0));
    let arpeggiator = plugins.arpeggiator(&mut song, up);
    let synth = plugins.synth(&mut song);
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices = vec![arpeggiator.clone(), synth];
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());

    // The chord's keys in turn, lowest first, an octave up, a step of an
    // eighth of a second each, until the chord ends.
    let beat = 24_000;
    let step = (ARP_STEP * f64::from(CONFIG.sample_rate)) as usize;
    let (left, _) = rig.play_from_start(beat + 4_800);
    let start = left.iter().position(|s| *s != 0.0).unwrap();
    assert!(start < 256, "the synth started at {start}");
    for (i, key) in [72, 76, 72, 76].into_iter().enumerate() {
        // Clear of where the steps meet.
        let from = start + i * step + 100;
        assert_plays_key(&left[from..from + step - 200], key);
    }
    assert!(left[start + beat + 1..].iter().all(|s| *s == 0.0));

    // Bypassed or deactivated, it lets the chord through as it is: two
    // notes at once.
    let device = Args::new().with("devices", vec![arpeggiator.id]);
    let bypass = device.clone().with("bypassed", true);
    rig.session.run_with(&device::SET_BYPASS, &bypass).unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(beat);
    assert!(peak(&left) > 0.9, "a peak of {}", peak(&left));
    let bypass = device.clone().with("bypassed", false);
    rig.session.run_with(&device::SET_BYPASS, &bypass).unwrap();
    let deactivate = device.with("active", false);
    rig.session
        .run_with(&device::SET_ACTIVE, &deactivate)
        .unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(beat);
    assert!(peak(&left) > 0.9, "a peak of {}", peak(&left));
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_second_track_doubles_a_clap_arpeggiator_through_midi_from() {
    doubles_an_arpeggiator_through_midi_from(PluginFormat::Clap);
}

#[test]
fn a_second_track_doubles_a_vst3_arpeggiator_through_midi_from() {
    doubles_an_arpeggiator_through_midi_from(PluginFormat::Vst3);
}

fn doubles_an_arpeggiator_through_midi_from(format: PluginFormat) {
    let plugins = TestPlugins::new(format, "double");
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let double = song.add_track("Double", TrackKind::Midi);
    let mut chord = MidiClip::default();
    for key in [60, 64] {
        chord.add_note(Beats::ZERO, Beats::new(1), key, 1.0);
    }
    let range = BeatRange::new(Beats::ZERO, Beats::new(1));
    let clip = song.new_clip("Chord", range, ClipContent::Midi(chord));
    let up = with_param(TRANSPOSE, plugins.transpose(12.0));
    let arpeggiator = plugins.arpeggiator(&mut song, up);
    let synth = plugins.synth(&mut song);
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices = vec![arpeggiator.clone(), synth];
    // Muted, so only the Double sounds, which plays what it takes all the
    // same.
    track.muted = true;
    let synth = plugins.synth(&mut song);
    song.track_mut(double).unwrap().devices.push(synth);
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);

    // Chosen in the mixer: the notes after the arpeggiator, and then at the
    // Keys' Post FX, those that reach its synth, which are the same.
    let beat = 24_000;
    let step = (ARP_STEP * f64::from(CONFIG.sample_rate)) as usize;
    for args in [
        Args::new().with("after", arpeggiator.id),
        Args::new().with("from", keys).with("tap", "post_fx"),
    ] {
        let args = args.with("track", double);
        rig.session.run_with(&mixer::SET_INPUT, &args).unwrap();
        rig.session.wait_for_graph();
        let (left, _) = rig.play_from_start(beat + 4_800);
        let start = left.iter().position(|s| *s != 0.0).unwrap();
        assert!(start < 256, "the Double started at {start}");
        for (i, key) in [72, 76, 72, 76].into_iter().enumerate() {
            let from = start + i * step + 100;
            assert_plays_key(&left[from..from + step - 200], key);
        }
        assert!(left[start + beat + 1..].iter().all(|s| *s == 0.0));
    }
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn a_second_track_doubles_what_a_clap_synth_plays_out_through_midi_from() {
    doubles_what_a_synth_plays_out_through_midi_from(PluginFormat::Clap);
}

#[test]
fn a_second_track_doubles_what_a_vst3_synth_plays_out_through_midi_from() {
    doubles_what_a_synth_plays_out_through_midi_from(PluginFormat::Vst3);
}

fn doubles_what_a_synth_plays_out_through_midi_from(format: PluginFormat) {
    // Named so, the library's synth plays each note it gets out again as
    // it renders, an octave up, on the same frame.
    let plugins = TestPlugins::new(format, "echo");
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let double = song.add_track("Double", TrackKind::Midi);
    let mut c = MidiClip::default();
    c.add_note(Beats::ZERO, Beats::new(1), 60, 1.0);
    let range = BeatRange::new(Beats::ZERO, Beats::new(1));
    let clip = song.new_clip("C", range, ClipContent::Midi(c));
    let synth = plugins.synth(&mut song);
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(synth.clone());
    let doubling = plugins.synth(&mut song);
    song.track_mut(double).unwrap().devices.push(doubling);
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);
    // It says it has a note output as it loads, and its device takes it,
    // as no step to undo.
    rig.render(240);
    let (_, _, device) = rig.session.project().find_device(synth.id).unwrap();
    assert!(device.kind.plugin().unwrap().note_output);
    assert_eq!(rig.session.undo_name(), None);

    // The Keys' synth alone, for where it starts.
    let beat = 24_000;
    let (left, _) = rig.play_from_start(beat + 4_800);
    let start = left.iter().position(|s| *s != 0.0).unwrap();
    assert!(start < 256, "the Keys started at {start}");
    assert_plays_key(&left[start + 100..start + beat - 100], 60);

    // Muted, and its notes taken after its synth, chosen in the mixer: the
    // Double plays them an octave up, from the same frame, a block late
    // never.
    let mute = Args::new().with("track", keys).with("muted", true);
    rig.session.run_with(&mixer::SET_MUTE, &mute).unwrap();
    let args = Args::new().with("track", double).with("after", synth.id);
    rig.session.run_with(&mixer::SET_INPUT, &args).unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(beat + 4_800);
    let doubled = left.iter().position(|s| *s != 0.0).unwrap();
    assert_eq!(doubled, start);
    assert_plays_key(&left[start + 100..start + beat - 100], 72);
    assert!(left[start + beat + 1..].iter().all(|s| *s == 0.0));

    // Bypassed, the synth plays none out, and the Double's silent.
    let bypass = Args::new().with("devices", vec![synth.id]);
    let bypass = bypass.with("bypassed", true);
    rig.session.run_with(&device::SET_BYPASS, &bypass).unwrap();
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(beat + 4_800);
    assert_eq!(peak(&left), 0.0);
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn four_tracks_play_four_parts_of_a_multi_timbral_clap_synth() {
    plays_four_parts_of_a_multi_timbral_synth(PluginFormat::Clap);
}

#[test]
fn four_tracks_play_four_parts_of_a_multi_timbral_vst3_synth() {
    plays_four_parts_of_a_multi_timbral_synth(PluginFormat::Vst3);
}

/// Plays the four beats of [`plays_four_parts_of_a_multi_timbral_synth`]'s
/// song, and checks the key each sounds, or that it's silent.
#[track_caller]
fn assert_parts(rig: &mut Rig, keys: [Option<u8>; 4]) {
    let beat = 24_000;
    rig.session.wait_for_graph();
    let (left, _) = rig.play_from_start(4 * beat);
    for (i, key) in keys.into_iter().enumerate() {
        // Clear of where the beats meet, however late the first starts.
        let heard = &left[i * beat + 300..(i + 1) * beat - 300];
        match key {
            Some(key) => assert_plays_key(heard, key),
            None => assert_eq!(peak(heard), 0.0, "beat {i} isn't silent"),
        }
    }
}

fn plays_four_parts_of_a_multi_timbral_synth(format: PluginFormat) {
    // Named so, the library's synth plays each MIDI channel an octave above
    // the one before, as a part of its own.
    let plugins = TestPlugins::new(format, "parts");
    let mut song = Project::new();
    let strings = song.add_track("Strings", TrackKind::Midi);
    let synth = plugins.synth(&mut song);
    song.track_mut(strings).unwrap().devices.push(synth.clone());
    // Four tracks without an instrument, each playing the A below middle C
    // for a beat, in turn.
    let parts: Vec<TrackId> = (0..4)
        .map(|i| {
            let part = song.add_track(format!("Part {}", i + 1), TrackKind::Midi);
            let mut a = MidiClip::default();
            a.add_note(Beats::ZERO, Beats::new(1), 57, 1.0);
            let range = BeatRange::new(Beats::new(i), Beats::new(i + 1));
            let clip = song.new_clip("A", range, ClipContent::Midi(a));
            song.track_mut(part).unwrap().clips.push(clip);
            part
        })
        .collect();
    let mut rig = Rig::new(song, None);
    rig.install(&plugins);

    // Chosen in each part's mixer strip: the synth, on a channel of its
    // own. The parts play in turn, each an octave above the one before.
    for (channel, &part) in (0_i64..).zip(&parts) {
        let args = (Args::new().with("track", part))
            .with("device", synth.id)
            .with("channel", channel);
        rig.session.run_with(&mixer::SET_MIDI_TO, &args).unwrap();
    }
    assert_parts(&mut rig, [Some(57), Some(69), Some(81), Some(93)]);

    // A muted part is cut, and a soloed one plays alone, through the synth
    // it plays.
    let part = |i: usize| Args::new().with("track", parts[i]);
    let mute = |muted: bool| part(1).with("muted", muted);
    rig.session.run_with(&mixer::SET_MUTE, &mute(true)).unwrap();
    assert_parts(&mut rig, [Some(57), None, Some(81), Some(93)]);
    rig.session
        .run_with(&mixer::SET_MUTE, &mute(false))
        .unwrap();
    let solo = |solo: bool| part(2).with("solo", solo);
    rig.session.run_with(&mixer::SET_SOLO, &solo(true)).unwrap();
    assert_parts(&mut rig, [None, None, Some(81), None]);
    rig.session
        .run_with(&mixer::SET_SOLO, &solo(false))
        .unwrap();

    // Another channel, picked in the Routing panel, plays another part, and
    // undone, the fourth part's back.
    let wire = rig.session.project().midi_to(parts[3]).next().unwrap().id;
    let args = Args::new().with("wire", wire).with("channel", 0_i64);
    rig.session.run_with(&wire::SET_CHANNEL, &args).unwrap();
    assert_parts(&mut rig, [Some(57), Some(69), Some(81), Some(57)]);
    rig.session.run(&edit::UNDO).unwrap();
    assert_parts(&mut rig, [Some(57), Some(69), Some(81), Some(93)]);
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    drop(rig);
    plugins.assert_alright();
}

#[test]
fn the_app_scans_each_library_in_a_process_of_its_own() {
    let dir = folder("scan");
    let scanned = |library: &Path| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_musicable"));
        command.arg(SCAN_PLUGIN).arg(library);
        scan::scan_isolated(command, scan::TIMEOUT)
    };
    // A CLAP library and a VST3 bundle.
    let copies = |name| [testing::copy(&dir, name), testing::copy_vst3(&dir, name).0];
    for good in copies("good") {
        let Outcome::Found(found) = scanned(&good) else {
            panic!("the test plugins weren't found in {}", good.display());
        };
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Test Gain", "Test Synth", "Test Arpeggiator"]);
        // A MIDI effect, which a VST3 plugin says only through its buses,
        // as the scanning process counts them.
        assert_eq!(found[2].kind(), PluginKind::MidiEffect);
    }
    for crash in copies("crash") {
        let crashed = scanned(&crash);
        assert!(matches!(crashed, Outcome::Crashed(_)), "{crashed:?}");
    }
}

//! Plugins on tracks, through the app's session, with the test plugins
//! (`crates/test-plugins`), CLAP and VST3 alike. A plugin loads once a scan
//! says it's installed, plays, and comes back from the project's file as it
//! was saved. Changes made in its window are steps in the undo history, and
//! the ones it makes on its own aren't, so redo carries on past them. Its
//! parameters follow their automation until someone turns them by hand. A
//! missing plugin lets the sound through and says why. Delay compensation
//! lines a latent plugin up with a dry track to the sample, also after the
//! plugin's latency changes. And the app's binary scans each library in a
//! process of its own, so a library that crashes takes only that process
//! down.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{ptr, thread};

use musicable_app::{PLUGIN_SETTLING, SCAN_PLUGIN, Session};
use musicable_commands::{Args, Keymap, Value, automation, device, edit, transport};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Device, DeviceId, DeviceKind, PluginFormat, Project, TestTone, TrackKind};
use musicable_plugins::scan::{self, Outcome};
use musicable_plugins::{Instance, ParentWindow, PluginInfo};
use musicable_test_plugins::testing::{self, Library};
use musicable_test_plugins::vst3::VST3_GAIN_ID;
use musicable_test_plugins::{GAIN, GAIN_ID, LATENCY, gui};

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

    /// What a scan finds: Test Gain and Test Synth.
    fn installed(&self) -> Vec<PluginInfo> {
        scan::scan_here(self.library.path()).unwrap()
    }

    /// A Test Gain device.
    fn gain(&self, project: &mut Project, settings: PluginSettings) -> Device {
        let id = match self.format {
            PluginFormat::Clap => GAIN_ID.to_str().unwrap(),
            PluginFormat::Vst3 => VST3_GAIN_ID,
        };
        plugin(project, self.format, id, "Test Gain", settings)
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
    settings: PluginSettings,
) -> Device {
    project.new_device(DeviceKind::Plugin(PluginDevice {
        format,
        id: id.to_owned(),
        name: name.to_owned(),
        instrument: false,
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
fn a_missing_plugin_lets_the_sound_through_and_says_why() {
    let plugins = TestPlugins::new(PluginFormat::Clap, "missing");
    let settings = PluginSettings::default();
    let reverb = |p: &mut Project| {
        let id = "org.example.reverb";
        plugin(p, PluginFormat::Clap, id, "Old Reverb", settings)
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
        assert_eq!(names, ["Test Gain", "Test Synth"]);
    }
    for crash in copies("crash") {
        let crashed = scanned(&crash);
        assert!(matches!(crashed, Outcome::Crashed(_)), "{crashed:?}");
    }
}

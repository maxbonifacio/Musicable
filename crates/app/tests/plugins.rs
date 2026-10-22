//! Plugins on tracks, through the app's session, with the test plugins
//! (`crates/test-plugins`). A plugin loads once a scan says it's installed,
//! plays, and comes back from the project's file as it was saved. Changes
//! made in its window are steps in the undo history. A missing plugin lets
//! the sound through and says why. Delay compensation lines a latent plugin
//! up with a dry track to the sample, also after the plugin's latency
//! changes. And the app's binary scans each library in a process of its own,
//! so a library that crashes takes only that process down.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use musicable_app::{SCAN_PLUGIN, Session};
use musicable_commands::{Keymap, edit, transport};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent};
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Device, DeviceId, DeviceKind, PluginFormat, Project, TestTone, TrackKind};
use musicable_plugins::PluginInfo;
use musicable_plugins::scan::{self, Outcome};
use musicable_test_plugins::testing::{self, Library};
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

/// What a scan finds in the test plugins' library: Test Gain and Test
/// Synth.
fn installed(library: &Library) -> Vec<PluginInfo> {
    scan::scan_here(library.path()).unwrap()
}

/// A plugin device for the plugin with id `id`, called `name`.
fn plugin(project: &mut Project, id: &str, name: &str, settings: PluginSettings) -> Device {
    project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: id.to_owned(),
        name: name.to_owned(),
        instrument: false,
        settings,
    }))
}

fn test_gain(project: &mut Project, settings: PluginSettings) -> Device {
    plugin(project, GAIN_ID.to_str().unwrap(), "Test Gain", settings)
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
    fn install(&mut self, library: &Library) {
        self.session.set_installed_plugins(installed(library));
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

    /// Renders a fifth of a second, waiting for any graph a change in a
    /// plugin needs, and returns the peak of each channel over its last
    /// half, when any change has long settled.
    fn peaks(&mut self) -> [f32; 2] {
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
fn a_plugin_plays_on_its_track_and_comes_back_as_it_was_saved() {
    let dir = folder("saved");
    let library = Library::copy(&dir, "saved");
    let (song, device) = tone_with(|p| test_gain(p, with_param(GAIN, 0.25)));
    let mut rig = Rig::new(song, None);

    // Until a scan says what's installed, the device lets the sound
    // through.
    near(rig.peaks(), TONE);
    rig.install(&library);
    assert_eq!(rig.session.take_problems(), Vec::<String>::new());
    let wet = rig.peaks();
    near(wet, times(TONE, 0.25));

    // State that isn't a parameter, changed in the plugin's window, goes
    // into the project, as a step in the undo history.
    let instance = rig.session.plugin(device.id).unwrap();
    // SAFETY: the instance's plugin, on its main thread.
    unsafe { library.set_label(instance.raw(), "Warm") };
    rig.session.tick();
    assert_eq!(rig.session.undo_name(), Some("Change Test Gain"));
    assert!(rig.settings(device.id).state.is_some());
    let path = dir.join("Song.musicable");
    project::save(&path, &rig.session.saved_project()).unwrap();
    drop(rig);

    // Opened again, it plays as it did, and has its label back.
    let opened = project::load(&path).unwrap();
    let mut rig = Rig::new(opened, Some(&dir));
    rig.install(&library);
    let instance = rig.session.plugin(device.id).unwrap();
    // SAFETY: as above.
    assert_eq!(unsafe { library.label(instance.raw()) }, "Warm");
    near(rig.peaks(), wet);
    drop(rig);
    library.assert_alright();
}

#[test]
fn changes_in_a_plugins_window_are_steps_in_the_undo_history() {
    let dir = folder("window");
    let library = Library::copy(&dir, "window");
    let (song, device) = tone_with(|p| test_gain(p, PluginSettings::default()));
    let mut rig = Rig::new(song, None);
    rig.install(&library);
    near(rig.peaks(), TONE);

    // Someone drags the gain's knob down to a half, in two moves.
    let plugin = rig.session.plugin(device.id).unwrap().raw();
    // SAFETY: the instance's plugin, on its main thread.
    unsafe {
        library.gui(plugin, gui::BEGIN, GAIN, 0.0);
        library.gui(plugin, gui::VALUE, GAIN, 0.75);
        library.gui(plugin, gui::VALUE, GAIN, 0.5);
        library.gui(plugin, gui::END, GAIN, 0.0);
    }
    let half = times(TONE, 0.5);
    near(rig.peaks(), half);
    assert_eq!(rig.settings(device.id).param(GAIN), Some(0.5));
    assert_eq!(rig.session.undo_name(), Some("Change Test Gain"));

    // One undo takes the whole drag back, in the plugin too, and redo puts
    // it back.
    rig.session.run(&edit::UNDO).unwrap();
    near(rig.peaks(), TONE);
    assert_eq!(rig.settings(device.id).param(GAIN), None);
    let instance = rig.session.plugin(device.id).unwrap();
    assert_eq!(instance.param_value(GAIN), Some(1.0));
    assert_eq!(rig.session.undo_name(), None);
    rig.session.run(&edit::REDO).unwrap();
    near(rig.peaks(), half);
    drop(rig);
    library.assert_alright();
}

#[test]
fn a_missing_plugin_lets_the_sound_through_and_says_why() {
    let dir = folder("missing");
    let library = Library::copy(&dir, "missing");
    let settings = PluginSettings::default();
    let (song, device) = tone_with(|p| plugin(p, "org.example.reverb", "Old Reverb", settings));
    let mut rig = Rig::new(song, None);
    rig.install(&library);
    let why = "Old Reverb isn't installed, or Musicable hasn't found it: it plays nothing";
    assert_eq!(rig.session.take_problems(), [why]);
    assert_eq!(rig.session.plugin_problem(device.id), Some(why));
    assert!(rig.session.plugin(device.id).is_none());
    near(rig.peaks(), TONE);
    // It isn't tried again until the plugins installed change.
    rig.session.tick();
    assert!(rig.session.take_problems().is_empty());
    drop(rig);
    library.assert_alright();
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
fn delay_compensation_lines_a_latent_plugin_up_with_a_dry_track() {
    // Two tracks play the same clicks, one of them through Test Gain, which
    // reports its latency. Panned apart, the two come out the same, to the
    // sample.
    let dir = folder("compensation");
    let library = Library::copy(&dir, "compensation");
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
    let device = test_gain(&mut song, with_param(LATENCY, 1_000.0));
    song.track_mut(wet).unwrap().devices.push(device.clone());
    let mut rig = Rig::new(song, Some(&dir));
    rig.install(&library);

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
    let plugin = rig.session.plugin(device.id).unwrap().raw();
    // SAFETY: the instance's plugin, on its main thread.
    unsafe {
        library.gui(plugin, gui::BEGIN, LATENCY, 0.0);
        library.gui(plugin, gui::VALUE, LATENCY, 3_000.0);
        library.gui(plugin, gui::END, LATENCY, 0.0);
    }
    rig.render(4_800);
    rig.session.wait_for_graph();
    rig.render(4_800);
    assert_eq!(rig.session.stats().latency, 3_000);
    assert_eq!(rig.settings(device.id).param(LATENCY), Some(3_000.0));
    assert_eq!(play(&mut rig), Some(3_000));
    drop(rig);
    library.assert_alright();
}

#[test]
fn the_app_scans_each_library_in_a_process_of_its_own() {
    let dir = folder("scan");
    let scanner = |library: &Path| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_musicable"));
        command.arg(SCAN_PLUGIN).arg(library);
        command
    };
    let good = testing::copy(&dir, "good");
    let Outcome::Found(found) = scan::scan_isolated(scanner(&good), scan::TIMEOUT) else {
        panic!("the test plugins weren't found");
    };
    let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Test Gain", "Test Synth"]);
    let crash = testing::copy(&dir, "crash");
    let crashed = scan::scan_isolated(scanner(&crash), scan::TIMEOUT);
    assert!(matches!(crashed, Outcome::Crashed(_)), "{crashed:?}");
}

//! Plugins on tracks in the app's window, with the test plugins
//! (`crates/test-plugins`): the plugin browser lists what the last scan
//! found, a double-click puts a plugin on the selected track, and the
//! devices panel opens the plugin's window and deletes the device. Changes
//! made in the plugin's window are steps in the undo history, and the
//! plugins come back from the project's file as they were saved. VST3
//! plugins go on tracks the same way, and their windows open inside
//! Musicable's on Windows. A device's Bypass button bypasses its plugin,
//! and deactivates it with Alt, and the devices panel's keys act on the
//! devices selected there. A MIDI effect is in the browser's folder of
//! them, can be marked as another kind, and goes in front of the track's
//! instrument.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::path::Path;

use eframe::egui::{Event, Modifiers, PointerButton, Pos2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_io::project;
use musicable_model::plugin::PluginSettings;
use musicable_model::{DeviceId, Project, TrackKind};
use musicable_plugins::scan::{self, Outcome, ScanCache};
use musicable_test_plugins::testing::Library;
use musicable_test_plugins::{BYPASS, GAIN, LEVEL, gui};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::plugin_scan::PLUGINS_FILE;
use crate::settings::SETTINGS_FILE;

/// Keeps what a scan of `library` finds in `config`, as if Musicable had
/// scanned before.
fn scanned(config: &Path, library: &Library) {
    let mut cache = ScanCache::default();
    cache.update(&[library.path().to_owned()], false, |path| {
        Outcome::Found(scan::scan_here(path).unwrap())
    });
    cache.write(&config.join(PLUGINS_FILE)).unwrap();
}

fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

/// Moves the pointer to `at`, waits a third of a second, so the clicks
/// before don't make these a double or triple click, and clicks `times`
/// times.
fn click_at(harness: &mut Harness<'static, App>, at: Pos2, times: usize) {
    harness.hover_at(at);
    for _ in 0..20 {
        harness.step();
    }
    for _ in 0..times {
        for pressed in [true, false] {
            harness.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
    }
    harness.run();
}

fn double_click(harness: &mut Harness<'static, App>, label: &str) {
    let at = harness.get_by_label(label).rect().center();
    click_at(harness, at, 2);
}

/// The names of the plugins on the track called `track`, in order, and
/// their ids.
fn plugins(harness: &Harness<'static, App>, track: &str) -> Vec<(String, DeviceId)> {
    let song = harness.state().session.project();
    let track = song.tracks.iter().find(|t| t.name == track).unwrap();
    (track.devices.iter())
        .filter_map(|d| Some((d.kind.plugin()?.name.clone(), d.id)))
        .collect()
}

fn names(harness: &Harness<'static, App>, track: &str) -> Vec<String> {
    plugins(harness, track)
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

fn settings(project: &Project, device: DeviceId) -> PluginSettings {
    let (_, _, device) = project.find_device(device).unwrap();
    device.kind.plugin().unwrap().settings.clone()
}

#[test]
fn plugins_go_on_tracks_from_the_browser_and_come_back_from_the_file() {
    let config = TempDir::new("plugins-config");
    let folder = TempDir::new("plugins-song");
    let library = Library::copy(&folder.0, "test-plugins");
    scanned(&config.0, &library);
    let mut song = Project::new();
    song.add_track("Keys", TrackKind::Midi);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path.clone()));

    // A click on the track's header selects it, and the devices panel
    // shows its devices, none yet. Add Device opens the plugin browser,
    // with what the last scan found.
    harness.get_by_label("Select a track, or the master track, to see its devices.");
    let header = harness.get_by_label("Track Keys").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Device...");
    harness.get_by_label("3 plugins");

    // A double-click puts a plugin on the track, after what's there.
    double_click(&mut harness, "Test Synth");
    double_click(&mut harness, "Test Gain");
    assert_eq!(names(&harness, "Keys"), ["Test Synth", "Test Gain"]);
    harness.get_by_label("Device Test Synth");
    harness.get_by_label("Device Test Gain");
    let synth = plugins(&harness, "Keys")[0].1;
    let instance = harness.state().session.plugin(synth).unwrap();
    let plugin = instance.as_clap().unwrap().raw();

    // The panel opens the plugin's window: inside a window of Musicable's
    // on Windows, a window of its own elsewhere. And it closes it again.
    click(&mut harness, "Test Synth Plugin Window");
    assert!(harness.state().plugin_windows.is_open(synth));
    // SAFETY: the instance's plugin, on its main thread.
    let (showing, parent) = unsafe { library.window(plugin) };
    assert_eq!((showing, parent != 0), (1, cfg!(windows)));
    click(&mut harness, "Test Synth Plugin Window");
    assert!(!harness.state().plugin_windows.is_open(synth));
    // SAFETY: as above.
    assert_eq!(unsafe { library.window(plugin) }.0, -1);

    // A change made in the plugin's window is a step in the undo history,
    // with no audio device playing it.
    // SAFETY: as above.
    unsafe {
        library.gui(plugin, gui::BEGIN, LEVEL, 0.0);
        library.gui(plugin, gui::VALUE, LEVEL, 0.25);
        library.gui(plugin, gui::END, LEVEL, 0.0);
    }
    harness.run();
    let state = harness.state();
    assert_eq!(state.session.undo_name(), Some("Change Test Synth"));
    assert_eq!(
        settings(state.session.project(), synth).param(LEVEL),
        Some(0.25)
    );

    // Ctrl+S saves the plugins with their settings, and each plugin's
    // state as it is.
    press(&mut harness, chord("Ctrl+S"));
    assert!(!harness.state().session.is_modified());
    let saved = settings(&project::load(&path).unwrap(), synth);
    assert_eq!(saved.param(LEVEL), Some(0.25));
    assert!(saved.state.is_some());

    // The panel deletes a device, which undoes and redoes.
    click(&mut harness, "Delete Test Gain");
    assert_eq!(names(&harness, "Keys"), ["Test Synth"]);
    assert!(harness.query_by_label("Device Test Gain").is_none());
    press(&mut harness, chord("Ctrl+Z"));
    assert_eq!(names(&harness, "Keys"), ["Test Synth", "Test Gain"]);
    harness.get_by_label("Device Test Gain");
    press(&mut harness, chord("Ctrl+Y"));
    assert_eq!(names(&harness, "Keys"), ["Test Synth"]);
    // The step before is the change made in the plugin's window, which
    // undo takes back in the plugin too.
    press(&mut harness, chord("Ctrl+Z"));
    press(&mut harness, chord("Ctrl+Z"));
    assert_eq!(names(&harness, "Keys"), ["Test Synth", "Test Gain"]);
    let state = harness.state();
    assert_eq!(settings(state.session.project(), synth).param(LEVEL), None);
    let instance = state.session.plugin(synth).unwrap();
    assert_eq!(instance.param_value(LEVEL), Some(0.5));
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);

    // Opened again, the song has its plugins as they were saved.
    let harness = window(&config.0, Some(path));
    assert_eq!(names(&harness, "Keys"), ["Test Synth", "Test Gain"]);
    let instance = harness.state().session.plugin(synth).unwrap();
    assert_eq!(instance.param_value(LEVEL), Some(0.25));
    drop(harness);
    library.assert_alright();
}

#[test]
fn the_bypass_button_bypasses_a_plugin_and_alt_click_deactivates_it() {
    let config = TempDir::new("bypass-config");
    let folder = TempDir::new("bypass-song");
    let library = Library::copy(&folder.0, "test-plugins");
    scanned(&config.0, &library);
    let mut song = Project::new();
    song.add_track("Bass", TrackKind::Audio);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));
    let header = harness.get_by_label("Track Bass").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Device...");
    double_click(&mut harness, "Test Gain");
    let gain = plugins(&harness, "Bass")[0].1;

    // The plugin's own bypass is its device's Bypass, so automation lists
    // only the device's.
    let listed = |harness: &Harness<'static, App>| -> Vec<u32> {
        let status = &harness.state().plugin_status[&gain];
        status.params.iter().map(|param| param.id).collect()
    };
    assert_eq!(listed(&harness), [GAIN]);
    assert_ne!(BYPASS, GAIN);

    // The Bypass button bypasses it, and Alt-click deactivates it, each a
    // step to undo.
    let switches = |harness: &Harness<'static, App>| {
        let project = harness.state().session.project();
        let (_, _, device) = project.find_device(gain).unwrap();
        (device.bypassed, device.active)
    };
    click(&mut harness, "Test Gain Bypass");
    assert_eq!(switches(&harness), (true, true));
    assert_eq!(
        harness.state().session.undo_name(),
        Some("Bypass Test Gain")
    );
    harness.get_by_label("Device Test Gain, bypassed");
    harness
        .get_by_label("Test Gain Bypass")
        .click_modifiers(Modifiers::ALT);
    harness.run();
    assert_eq!(switches(&harness), (true, false));
    let undo = harness.state().session.undo_name();
    assert_eq!(undo, Some("Deactivate Test Gain"));
    harness.get_by_label("Device Test Gain, bypassed, deactivated");
    press(&mut harness, chord("Ctrl+Z"));
    press(&mut harness, chord("Ctrl+Z"));
    assert_eq!(switches(&harness), (false, true));
    harness.get_by_label("Device Test Gain");
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);
    library.assert_alright();
}

#[test]
fn the_devices_panels_keys_act_on_the_devices_selected_there() {
    let config = TempDir::new("panel-keys-config");
    let folder = TempDir::new("panel-keys-song");
    let library = Library::copy(&folder.0, "test-plugins");
    scanned(&config.0, &library);
    let mut song = Project::new();
    song.add_track("Bass", TrackKind::Audio);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));
    let header = harness.get_by_label("Track Bass").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Device...");
    double_click(&mut harness, "Test Gain");
    double_click(&mut harness, "Test Synth");
    let gain = plugins(&harness, "Bass")[0].1;
    let undo =
        |harness: &Harness<'static, App>| harness.state().session.undo_name().map(str::to_owned);

    // A click on its title bar selects it, for the panel's keys: 0
    // bypasses it, and Ctrl+Right moves it past the synth.
    click(&mut harness, "Device Test Gain");
    harness.get_by_label("Device Test Gain, selected");
    press(&mut harness, chord("0"));
    let project = harness.state().session.project();
    assert!(project.find_device(gain).unwrap().2.bypassed);
    assert_eq!(undo(&harness).as_deref(), Some("Bypass Test Gain"));
    press(&mut harness, chord("Ctrl+Right"));
    assert_eq!(names(&harness, "Bass"), ["Test Synth", "Test Gain"]);
    assert_eq!(undo(&harness).as_deref(), Some("Move Test Gain"));

    // The edit keys take it too: Ctrl+D puts a copy after it, bypassed as
    // it is, and selects the copy, which Delete deletes.
    press(&mut harness, chord("Ctrl+D"));
    let on_bass = plugins(&harness, "Bass");
    assert_eq!(
        names(&harness, "Bass"),
        ["Test Synth", "Test Gain", "Test Gain"]
    );
    assert_eq!(undo(&harness).as_deref(), Some("Duplicate Test Gain"));
    let copy = on_bass[2].1;
    let project = harness.state().session.project();
    assert!(project.find_device(copy).unwrap().2.bypassed);
    let editor = harness.state().session.editor();
    assert_eq!(editor.selected_devices(project), [copy]);
    press(&mut harness, chord("Delete"));
    assert_eq!(
        plugins(&harness, "Bass")[1..],
        [("Test Gain".to_owned(), gain)]
    );
    assert_eq!(undo(&harness).as_deref(), Some("Delete Test Gain"));
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);
    library.assert_alright();
}

#[test]
fn a_vst3_plugin_goes_on_a_track_and_its_window_opens_inside_musicables() {
    let config = TempDir::new("vst3-config");
    let folder = TempDir::new("vst3-song");
    let library = Library::copy_vst3(&folder.0, "test-plugins");
    scanned(&config.0, &library);
    let mut song = Project::new();
    song.add_track("Keys", TrackKind::Midi);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));

    // The browser lists the VST3 module's plugins, and a double-click puts
    // one on the selected track, where it loads.
    let header = harness.get_by_label("Track Keys").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Device...");
    harness.get_by_label("3 plugins");
    double_click(&mut harness, "Test Gain");
    assert_eq!(names(&harness, "Keys"), ["Test Gain"]);
    let gain = plugins(&harness, "Keys")[0].1;
    let instance = harness.state().session.plugin(gain).unwrap();
    let controller = instance.as_vst3().unwrap().raw_controller();

    // Its window opens inside a window of Musicable's on Windows. A VST3
    // plugin's window only goes inside another, so elsewhere the app says
    // it can't open yet.
    click(&mut harness, "Test Gain Plugin Window");
    // SAFETY: the instance's controller, on its main thread.
    let (showing, parent, platform, _) = unsafe { library.vst3_window(controller) };
    if cfg!(windows) {
        assert!(harness.state().plugin_windows.is_open(gain));
        assert_eq!((showing, parent != 0, platform), (1, true, 1));
        click(&mut harness, "Test Gain Plugin Window");
        assert!(!harness.state().plugin_windows.is_open(gain));
        // SAFETY: as above.
        assert_eq!(unsafe { library.vst3_window(controller) }.0, -1);
    } else {
        assert!(!harness.state().plugin_windows.is_open(gain));
        assert_eq!(showing, -1);
        let why = "Test Gain's window can't open here yet: Musicable shows VST3 \
                   plugins' windows only on Windows so far.";
        assert_eq!(harness.state().messages, [why]);
    }
    drop(harness);
    library.assert_alright();
}

#[test]
fn a_midi_effect_from_the_browser_goes_in_front_of_the_instrument() {
    let config = TempDir::new("midi-effect-config");
    let folder = TempDir::new("midi-effect-song");
    // VST3's arpeggiator, which only its buses mark as a MIDI effect.
    let library = Library::copy_vst3(&folder.0, "test-plugins");
    scanned(&config.0, &library);
    let mut song = Project::new();
    song.add_track("Keys", TrackKind::Midi);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));
    let header = harness.get_by_label("Track Keys").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Device...");

    // It's alone in the MIDI Effects folder.
    click(&mut harness, "MIDI Effects");
    harness.get_by_label("Test Arpeggiator");
    assert!(harness.query_by_label("Test Synth").is_none());

    // Marked as an audio effect, it leaves the folder, and the settings
    // keep the mark. Marked as what the scan found, the mark goes.
    let settings = || std::fs::read_to_string(config.0.join(SETTINGS_FILE)).unwrap_or_default();
    let mark = "\"vst3:4D5553494341424C4554455354000004\" = \"audio_effect\"";
    harness.get_by_label("Test Arpeggiator").click_secondary();
    harness.run();
    click(&mut harness, "Audio Effect");
    assert!(harness.query_by_label("Test Arpeggiator").is_none());
    assert!(settings().contains(mark), "{}", settings());
    click(&mut harness, "Audio Effects");
    harness.get_by_label("Test Arpeggiator").click_secondary();
    harness.run();
    click(&mut harness, "MIDI Effect");
    assert!(harness.query_by_label("Test Arpeggiator").is_none());
    assert!(!settings().contains("plugins.kinds"), "{}", settings());

    // Added after the synth, it goes in front of it, where the notes are.
    click(&mut harness, "All");
    double_click(&mut harness, "Test Synth");
    double_click(&mut harness, "Test Arpeggiator");
    assert_eq!(names(&harness, "Keys"), ["Test Arpeggiator", "Test Synth"]);
    harness.get_by_label("MIDI Effect · VST3");
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);
    library.assert_alright();
}

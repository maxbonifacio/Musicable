//! Plugins on tracks in the app's window, with the test plugins
//! (`crates/test-plugins`): the plugin browser lists what the last scan
//! found, a double-click puts a plugin on the selected track, and the
//! devices panel opens the plugin's window and deletes the device. Changes
//! made in the plugin's window are steps in the undo history, and the
//! plugins come back from the project's file as they were saved.
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
use musicable_test_plugins::{LEVEL, gui};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::plugin_scan::PLUGINS_FILE;

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
    // shows its devices, none yet. Add Plugin opens the plugin browser,
    // with what the last scan found.
    harness.get_by_label("Select a track to see its devices, or pick Master.");
    let header = harness.get_by_label("Track Keys").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    click(&mut harness, "Add Plugin...");
    harness.get_by_label("2 plugins");

    // A double-click puts a plugin on the track, after what's there.
    double_click(&mut harness, "Test Synth");
    double_click(&mut harness, "Test Gain");
    assert_eq!(names(&harness, "Keys"), ["Test Synth", "Test Gain"]);
    harness.get_by_label("Device Test Synth");
    harness.get_by_label("Device Test Gain");
    let synth = plugins(&harness, "Keys")[0].1;
    let plugin = harness.state().session.plugin(synth).unwrap().raw();

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

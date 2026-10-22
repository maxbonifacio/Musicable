//! The browser in the app's window: a folder of samples added to the library
//! is scanned on the background queue, Ctrl+F puts the keys in the search
//! field, picking a sample previews it through the engine at the volume the
//! settings say, and a double-click puts it on a new track. The library
//! keeps its folders and tags, and the browser its settings, when the app
//! starts again.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::Path;

use eframe::egui::accesskit::Role;
use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::{Args, browser};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_model::TrackKind;

use super::App;
use super::testing::{TempDir, chord, press, send, window};
use crate::settings::{BrowserSettings, SETTINGS_FILE, Settings};

/// A tenth of a second at a quarter of full scale, in mono at 48 kHz: the
/// rate the engine runs at without a device, so it plays as it is.
fn wav(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let samples = vec![0.25f32; 4800];
    save_wav(path, 48_000, SampleFormat::Int16, &[&samples]).unwrap();
}

/// Lets the library's queue do what it was asked, and the window show it.
fn settle(harness: &mut Harness<'static, App>) {
    for _ in 0..10 {
        let library = harness.state_mut().session.library_mut();
        library.wait();
        if library.is_loaded() && library.scanning().is_empty() && !library.is_finding_presets() {
            harness.run();
            return;
        }
    }
    panic!("the library is still busy");
}

/// The text field whose hint is `hint`.
fn field<'a>(harness: &'a Harness<'static, App>, hint: &'static str) -> egui_kittest::Node<'a> {
    harness.get_by(move |node| node.data().placeholder() == Some(hint))
}

fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

/// Moves the pointer to `at`, waits a third of a second, so the clicks
/// before don't make these a double click, and clicks twice.
fn double_click_at(harness: &mut Harness<'static, App>, at: Pos2) {
    harness.hover_at(at);
    for _ in 0..20 {
        harness.step();
    }
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

/// The loudest sample in `played`.
fn peak(played: &[f32]) -> f32 {
    played
        .iter()
        .fold(0.0, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn samples_are_found_previewed_and_put_on_tracks_from_the_browser() {
    let dir = TempDir::new("browsing");
    let samples = dir.0.join("Samples");
    for name in ["Drums/Kick 01.wav", "Drums/Snare 01.wav", "Keys/Pad.wav"] {
        wav(&samples.join(name));
    }
    let settings = Settings {
        browser: BrowserSettings {
            preview_volume: -6,
            presets_folder: Some(dir.0.join("Presets")),
            ..BrowserSettings::default()
        },
        ..Settings::default()
    };
    settings.write(&dir.0.join(SETTINGS_FILE)).unwrap();
    let mut harness = window(&dir.0, None);
    settle(&mut harness);
    // The folder Add Folder's dialog would pick.
    let folder = Args::new().with("folder", samples.to_string_lossy().as_ref());
    let session = &mut harness.state_mut().session;
    session.run_with(&browser::ADD_FOLDER, &folder).unwrap();
    settle(&mut harness);

    // Ctrl+F shows the browser, with the keys in its search field, and
    // what's typed there searches the library.
    press(&mut harness, chord("Ctrl+F"));
    harness.get_by_label("3 samples in 1 folder");
    assert!(field(&harness, "Search samples").is_focused());
    send(&mut harness, vec![Event::Text("kick".into())]);
    harness.get_by_label("1 found");

    // Picking a sample plays it, at the volume the settings say: -6 dB.
    click(&mut harness, "Kick 01");
    settle(&mut harness);
    let played = harness.state_mut().output.play_idle(1024);
    let loudest = peak(&played);
    assert!((loudest - 0.25 * 0.501).abs() < 0.001, "{loudest}");
    harness.run();
    harness.get_by_label("Preview of Kick 01");
    let stop = harness.get_by_label("Stop Preview");
    assert!(!stop.accesskit_node().is_disabled());
    click(&mut harness, "Stop Preview");
    harness.state_mut().output.play_idle(1024);
    let played = harness.state_mut().output.play_idle(1024);
    assert_eq!(peak(&played), 0.0);

    // A double-click puts it on a new audio track named after it.
    let at = harness.get_by_label("Kick 01").rect().center();
    double_click_at(&mut harness, at);
    let song = harness.state().session.project();
    let track = song.tracks.iter().find(|t| t.name == "Kick 01").unwrap();
    assert_eq!(track.kind, TrackKind::Audio);
    let clips: Vec<&str> = track.clips.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(clips, ["Kick 01"]);

    // Its menu in the browser, as the clip is called Kick 01 too, gives it
    // a tag.
    let row = harness.get_by_role_and_label(Role::ListItem, "Kick 01");
    row.click_secondary();
    harness.run();
    field(&harness, "New tag").click();
    harness.run();
    field(&harness, "New tag").type_text("dusty");
    harness.run();
    harness.key_press(Key::Enter);
    harness.run();
    // Auto Preview off is kept in the settings.
    click(&mut harness, "Auto Preview");
    let kept = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap().browser;
    assert!(!kept.auto_preview);
    assert_eq!(kept.presets_folder, settings.browser.presets_folder);
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);

    // After a restart, the library has the folder and the tag, and picking
    // a sample doesn't play it.
    let mut harness = window(&dir.0, None);
    settle(&mut harness);
    press(&mut harness, chord("Ctrl+F"));
    harness.get_by_label("3 samples in 1 folder");
    send(&mut harness, vec![Event::Text("dusty".into())]);
    harness.get_by_label("1 found");
    harness
        .get_by_role_and_label(Role::ListItem, "Kick 01")
        .click();
    harness.run();
    settle(&mut harness);
    assert!(
        harness
            .state()
            .session
            .library()
            .preview_playing()
            .is_none()
    );
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

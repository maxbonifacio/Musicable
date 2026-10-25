//! The done test for the Settings window (milestone 6 of the Tier 2 UI
//! foundations): the audio settings and the keys are pages of one window,
//! the other settings have pages there too, and every option in the
//! settings file but the export window's has a control. Changes apply at
//! once, and go to the settings file, or to the mouse map's.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;

use eframe::egui::accesskit::Role;
use eframe::egui::{self, Event, PointerButton, Pos2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::{Behaviour, MiddleC, PlayFrom};
use musicable_commands::{Modifiers, options};
use musicable_io::project;
use musicable_model::time::Beats;
use musicable_model::{Project, TrackKind};
use musicable_ui::{Gesture, MouseAction, MouseMap, Target};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::library;
use crate::mouse_file::BACKUP_FILE;
use crate::settings::{MOUSE_FILE, SETTINGS_FILE, Settings};
use crate::settings_window::{APPEARANCE, AUDIO, BROWSER, KEYS, MOUSE, RECORDING, WORKSPACES};

/// The page the Settings window shows, if it's open.
fn page(harness: &Harness<'static, App>) -> Option<&'static str> {
    let window = &harness.state().settings_window;
    window.is_open().then(|| window.page())
}

/// Scrolls the widget called `label` into view, and clicks it.
fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    harness.run();
    harness.get_by_label(label).click();
    harness.run();
}

/// Turns to the page called `title`, from the list down the window's side.
fn turn_to(harness: &mut Harness<'static, App>, title: &str) {
    click(harness, &format!("{title} page"));
}

/// The settings as the app last saved them.
fn saved(config: &TempDir) -> Settings {
    Settings::read(&config.0.join(SETTINGS_FILE)).unwrap()
}

/// Picks `item` from the menu called `menu`.
fn pick(harness: &mut Harness<'static, App>, menu: &str, item: &str) {
    click(harness, menu);
    harness.get_by_role_and_label(Role::Button, item).click();
    harness.run();
}

/// What the menu called `menu` shows.
fn shown(harness: &Harness<'static, App>, menu: &str) -> String {
    let node = harness.get_by_role_and_label(Role::ComboBox, menu);
    node.accesskit_node().value().unwrap_or_default().to_owned()
}

/// What the mouse map in use does with `gesture` on `target`, with `mods`.
fn does(
    harness: &Harness<'static, App>,
    target: Target,
    gesture: Gesture,
    mods: Modifiers,
) -> Option<MouseAction> {
    harness
        .state()
        .shell
        .mouse_map()
        .action(target, gesture, mods)
}

/// The clips on the first track, by their starts and ends in beats.
fn clips(harness: &Harness<'static, App>) -> Vec<(f64, f64)> {
    let track = &harness.state().session.project().tracks[0];
    (track.clips.iter())
        .map(|clip| (clip.start.to_f64(), clip.end().to_f64()))
        .collect()
}

/// In the first track's lane, at `at` beats: in a clip's body if there's
/// one there.
fn lane(harness: &Harness<'static, App>, at: f64) -> Pos2 {
    let lanes = harness.get_by_label("Arrangement").rect();
    let row = harness.get_by_label("Track Keys").rect();
    let viewport = harness.state().session.editor().viewport;
    pos2(
        lanes.left() + viewport.x_of(Beats::from_f64(at)),
        row.bottom() - 6.0,
    )
}

fn button(harness: &mut Harness<'static, App>, at: Pos2, pressed: bool) {
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run();
}

/// Drags from `from` to `to`, in a few moves, or clicks if they're the same.
fn drag(harness: &mut Harness<'static, App>, from: Pos2, to: Pos2) {
    harness.hover_at(from);
    harness.run();
    button(harness, from, true);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    button(harness, to, false);
}

#[test]
fn the_audio_settings_and_the_keys_are_pages_of_one_window() {
    let config = TempDir::new("settings-pages");
    let mut harness = window(&config.0, None);
    harness.run();
    assert_eq!(page(&harness), None);

    // Ctrl+Comma opens it, at its first page, the audio settings.
    press(&mut harness, chord("Ctrl+Comma"));
    assert_eq!(page(&harness), Some(AUDIO));
    harness.get_by_label("Settings");
    harness.get_by_label("Now playing");

    // The commands that showed windows of their own show their pages.
    press(&mut harness, chord("Ctrl+K"));
    assert_eq!(page(&harness), Some(KEYS));
    harness.get_by_label("Add keys to Toggle Metronome");
    assert!(harness.query_by_label("Now playing").is_none());
    let session = &mut harness.state_mut().session;
    session.run(&options::APPEARANCE).unwrap();
    harness.run();
    assert_eq!(page(&harness), Some(APPEARANCE));
    harness.get_by_label("Light");

    // The list down its side turns to the others.
    turn_to(&mut harness, "Recording");
    assert_eq!(page(&harness), Some(RECORDING));
    harness.get_by_label("No device is playing.");

    // Escape closes it, and it opens again at the page it showed.
    press(&mut harness, chord("Escape"));
    assert_eq!(page(&harness), None);
    assert!(harness.query_by_label("Settings").is_none());
    press(&mut harness, chord("Ctrl+Comma"));
    assert_eq!(page(&harness), Some(RECORDING));
}

#[test]
fn the_keys_page_stops_giving_keys_before_the_window_closes() {
    let config = TempDir::new("settings-giving-keys");
    let mut harness = window(&config.0, None);
    press(&mut harness, chord("Ctrl+K"));

    // Escape is a key a command can have, so the page waits on with it,
    // and then Escape stops giving the command keys.
    click(&mut harness, "Add keys to Toggle Metronome");
    press(&mut harness, chord("Escape"));
    harness.get_by_label("isn't used yet.");
    press(&mut harness, chord("Escape"));
    assert!(!harness.state().shell.giving_keys());
    assert_eq!(page(&harness), Some(KEYS));

    // A click outside the window stops it too, and goes on to what it's on.
    click(&mut harness, "Add keys to Toggle Metronome");
    assert!(harness.state().shell.capturing());
    harness
        .get_by_role_and_label(Role::Button, "Metronome")
        .click();
    harness.run();
    assert!(!harness.state().shell.giving_keys());
    assert!(harness.state().session.project().metronome);
    assert_eq!(page(&harness), Some(KEYS));

    // So does turning to another page.
    click(&mut harness, "Add keys to Toggle Metronome");
    turn_to(&mut harness, "Audio");
    assert!(!harness.state().shell.giving_keys());
    assert!(!harness.state().shell.capturing());

    // The command palette over the window takes Escape first.
    press(&mut harness, chord("Ctrl+Shift+P"));
    assert!(harness.state().shell.asking());
    press(&mut harness, chord("Escape"));
    assert!(!harness.state().shell.asking());
    assert_eq!(page(&harness), Some(AUDIO));
    press(&mut harness, chord("Escape"));
    assert_eq!(page(&harness), None);
}

#[test]
fn every_setting_but_the_export_windows_has_a_control() {
    let config = TempDir::new("settings-controls");
    let mine = config.0.join("My Presets");
    let text = format!(
        "[browser]\npresets_folder = {:?}\n",
        mine.display().to_string()
    );
    std::fs::write(config.0.join(SETTINGS_FILE), text).unwrap();
    let mut harness = window(&config.0, None);
    harness.run();
    // The browser panel has controls of its own for previews, and it's
    // hidden at first, so those found are the page's.
    assert!(!harness.state().shell.panel_shown(musicable_ui::BROWSER));
    let session = &mut harness.state_mut().session;
    session.run(&options::SETTINGS).unwrap();
    harness.run();

    // How long Capture holds the inputs. The recording offset is the device
    // playing's, and there's none.
    turn_to(&mut harness, "Recording");
    harness
        .get_by_label("Capture Length")
        .set_value_accesskit(12.0);
    harness.run();
    assert_eq!(saved(&config).recording.capture_minutes, 12);
    harness.get_by_label("No device is playing.");

    // How editing behaves.
    turn_to(&mut harness, "Behaviour");
    click(&mut harness, "Where the playhead stopped");
    click(
        &mut harness,
        "C4, as in Reaper and scientific pitch notation",
    );
    click(&mut harness, "Files whose tempo drifts lock to the grid");
    let behaviour = Behaviour {
        play_from: PlayFrom::Playhead,
        middle_c: MiddleC::C4,
        warp_drifting: false,
        ..Behaviour::default()
    };
    assert_eq!(harness.state().session.editor().behaviour, behaviour);
    assert_eq!(saved(&config).behaviour, behaviour);
    // How analysis finds beats is the computer's.
    click(&mut harness, "Fast analysis");
    assert!(saved(&config).analysis.fast);

    // The theme.
    turn_to(&mut harness, "Appearance");
    click(&mut harness, "Light");
    assert_eq!(saved(&config).appearance.theme, "light");

    // The browser's previews, and where presets go.
    turn_to(&mut harness, "Browser");
    assert_eq!(page(&harness), Some(BROWSER));
    click(&mut harness, "Preview samples as they're picked");
    harness
        .get_by_role_and_label(Role::Slider, "Preview Volume")
        .set_value_accesskit(-6.0);
    harness.run();
    harness.get_by_label(&mine.display().to_string());
    click(&mut harness, "Use the Default");
    let browser = saved(&config).browser;
    assert!(!browser.auto_preview);
    assert_eq!(browser.preview_volume, -6);
    assert_eq!(browser.presets_folder, None);
    let library = harness.state().session.library();
    assert_eq!(
        library.presets_folder(),
        library::presets_folder().as_deref()
    );
    assert!(
        harness
            .get_by_label("Use the Default")
            .accesskit_node()
            .is_disabled()
    );
    // The workspace in use has a test of its own, below, and the audio
    // settings need a device.
}

#[test]
fn the_workspaces_page_switches_and_keeps_workspaces() {
    let config = TempDir::new("settings-workspaces");
    let mut harness = window(&config.0, None);
    harness.run();
    harness.state_mut().settings_window.open(Some(WORKSPACES));
    harness.run();
    // Default can't be renamed or deleted.
    assert!(
        harness
            .get_by_label("Rename...")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        harness
            .get_by_label("Delete...")
            .accesskit_node()
            .is_disabled()
    );

    // Save As asks for a name over the window, and Escape cancels that
    // without closing the window.
    click(&mut harness, "Save As...");
    let field = harness.get_by_role_and_label(Role::TextInput, "Save Workspace As");
    field.type_text("Mixing");
    harness.run();
    press(&mut harness, chord("Escape"));
    assert!(harness.query_by_label("Save Workspace As").is_none());
    assert_eq!(page(&harness), Some(WORKSPACES));
    click(&mut harness, "Save As...");
    let field = harness.get_by_role_and_label(Role::TextInput, "Save Workspace As");
    field.type_text("Mixing");
    harness.run();
    press(&mut harness, chord("Enter"));

    // The page lists the new workspace in use, with the keys that switch
    // to it, and the settings keep it.
    assert_eq!(harness.state().workspaces.current(), 1);
    harness.get_by_label("Alt+2");
    assert!(
        !harness
            .get_by_label("Rename...")
            .accesskit_node()
            .is_disabled()
    );
    assert_eq!(saved(&config).workspaces.current, "Mixing");
    click(&mut harness, "Default");
    assert_eq!(harness.state().workspaces.current(), 0);
    assert_eq!(saved(&config).workspaces.current, "Default");
    assert_eq!(page(&harness), Some(WORKSPACES));
}

#[test]
fn the_mouse_page_changes_what_gestures_do() {
    let config = TempDir::new("settings-mouse");
    let folder = TempDir::new("settings-mouse-song");
    let mut song = Project::new();
    song.add_track("Keys", TrackKind::Midi);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path.clone()));
    harness.run();
    // A click in the lane puts the start marker there, and Ctrl+Shift+M a
    // bar-long MIDI clip.
    let at = lane(&harness, 0.2);
    drag(&mut harness, at, at);
    press(&mut harness, chord("Ctrl+Shift+M"));
    assert_eq!(clips(&harness), [(0.0, 4.0)]);

    // Dragging a clip's body selects time, as in Live. On the Mouse page,
    // it moves the clip instead, at once, and the file keeps that.
    press(&mut harness, chord("Ctrl+Comma"));
    turn_to(&mut harness, "Mouse");
    assert_eq!(page(&harness), Some(MOUSE));
    harness.get_by_label("Create It");
    let menu = "A clip's body: Drag";
    assert_eq!(shown(&harness, menu), "Select time");
    pick(&mut harness, menu, "Move");
    assert_eq!(shown(&harness, menu), "Move");
    let drag_body = (Target::ClipBody, Gesture::Drag, Modifiers::NONE);
    let (target, gesture, none) = drag_body;
    assert_eq!(
        does(&harness, target, gesture, none),
        Some(MouseAction::Move)
    );
    let file = config.0.join(MOUSE_FILE);
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("[clip_body]\n"), "{text}");
    assert!(text.contains("\n\"Drag\" = \"move\"\n"), "{text}");
    harness.get_by_label("Reload");
    press(&mut harness, chord("Escape"));
    let (from, to) = (lane(&harness, 1.0), lane(&harness, 5.0));
    drag(&mut harness, from, to);
    assert_eq!(clips(&harness), [(4.0, 8.0)]);

    // A gesture gets an action with other modifiers held: it does what it
    // did until another's picked, and Reset takes the action away again.
    press(&mut harness, chord("Ctrl+Comma"));
    let strip = (Target::Strip, Gesture::Click, Modifiers::SHIFT);
    let (target, gesture, shift) = strip;
    pick(
        &mut harness,
        "The ruler's strip: Add Modifiers",
        "Shift+Click",
    );
    let menu = "The ruler's strip: Shift+Click";
    assert_eq!(shown(&harness, menu), "Put the playhead there");
    pick(&mut harness, menu, "Turn looping on or off");
    assert_eq!(
        does(&harness, target, gesture, shift),
        Some(MouseAction::ToggleLoop)
    );
    click(&mut harness, &format!("Reset {menu}"));
    assert!(harness.query_by_label(menu).is_none());
    assert_eq!(
        does(&harness, target, gesture, shift),
        Some(MouseAction::Locate)
    );

    // Ctrl-dragging a clip's title copies it, as in Live. Without an action
    // of its own, it moves the clip, as dragging does.
    let menu = "A clip's title bar: Ctrl+Drag";
    pick(&mut harness, menu, "Same as Drag");
    assert_eq!(shown(&harness, menu), "Same as Drag");
    let ctrl_drag = (Target::ClipTitle, Gesture::Drag, Modifiers::CTRL);
    let (target, gesture, ctrl) = ctrl_drag;
    assert_eq!(
        does(&harness, target, gesture, ctrl),
        Some(MouseAction::Move)
    );

    // The changes last, and Reset All keeps them in a copy of the file.
    drop(harness);
    let mut harness = window(&config.0, Some(path));
    harness.run();
    let (target, gesture, mods) = drag_body;
    assert_eq!(
        does(&harness, target, gesture, mods),
        Some(MouseAction::Move)
    );
    let (target, gesture, mods) = ctrl_drag;
    assert_eq!(
        does(&harness, target, gesture, mods),
        Some(MouseAction::Move)
    );
    harness.state_mut().settings_window.open(Some(MOUSE));
    harness.run();
    click(&mut harness, "Reset All");
    assert_eq!(*harness.state().shell.mouse_map(), MouseMap::live());
    let kept = fs::read_to_string(config.0.join(BACKUP_FILE)).unwrap();
    assert!(kept.contains("\n\"Ctrl+Drag\" = \"none\"\n"), "{kept}");
    harness.get_by_label(&format!(
        "Every gesture does what it does at first again. Your changes from before are in \
         {BACKUP_FILE}."
    ));
}

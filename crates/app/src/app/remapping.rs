//! The done test for key remapping (N3 in the Tier 1 plan): every command
//! can be found in the command palette and given new keys in the keyboard
//! shortcuts window, without editing a file. The app saves the keys to the
//! user's keymap file, and they work the next time it starts.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::App as _;
use eframe::egui::{self, Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::{CommandId, KeyChord, Keymap, transport};

use super::App;
use crate::Output;
use crate::settings::{KEYMAP_FILE, Settings};

/// A config folder of its own for each test, removed when it ends.
struct ConfigDir(PathBuf);

impl ConfigDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("musicable-remapping-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for ConfigDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The app's window, with its settings and keymap in `config_dir`.
fn window(config_dir: &Path) -> Harness<'static, App> {
    let config_dir = config_dir.to_owned();
    Harness::builder()
        .with_size([1280.0, 800.0])
        .build_eframe(move |_| {
            let (output, handle) = Output::none();
            let settings = Settings::default();
            App::with_output(None, Some(config_dir), settings, output, handle, Vec::new())
        })
}

/// Hands `events` to the app as eframe does: through its raw input hook,
/// then to egui.
fn send(harness: &mut Harness<'static, App>, events: Vec<Event>) {
    let mut input = egui::RawInput {
        events,
        ..Default::default()
    };
    let ctx = harness.ctx.clone();
    harness.state_mut().raw_input_hook(&ctx, &mut input);
    for event in input.events {
        harness.event(event);
    }
    harness.run();
}

/// Presses and lets go of `chord`.
fn press(harness: &mut Harness<'static, App>, chord: KeyChord) {
    let (key, modifiers) = egui_key(chord);
    let key = |pressed| Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    };
    let events = vec![
        Event::ModifiersChanged(modifiers),
        key(true),
        key(false),
        Event::ModifiersChanged(Modifiers::NONE),
    ];
    send(harness, events);
}

/// The egui key and modifiers that make `chord`, for the chords these tests
/// press.
fn egui_key(chord: KeyChord) -> (Key, Modifiers) {
    let text = chord.to_string();
    let name = text.rsplit('+').next().unwrap();
    let key = Key::from_name(name).unwrap_or_else(|| panic!("no egui key for {text}"));
    let mods = chord.mods;
    let modifiers = Modifiers {
        alt: mods.alt,
        ctrl: mods.ctrl,
        shift: mods.shift,
        mac_cmd: mods.cmd,
        command: mods.ctrl || mods.cmd,
    };
    (key, modifiers)
}

fn chord(text: &str) -> KeyChord {
    text.parse().unwrap()
}

/// Scrolls the widget called `label` into view, and clicks it.
fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    harness.run();
    harness.get_by_label(label).click();
    harness.run();
}

/// A command as the command palette lists it: its area and title, then its
/// keys.
fn palette_row(app: &App, id: &CommandId) -> String {
    let command = app.session.commands().get(id).unwrap();
    let area = id.as_str().split('.').next().unwrap();
    let area = area[..1].to_uppercase() + &area[1..];
    let keys: Vec<_> = app
        .session
        .keymap()
        .chords_for(id)
        .map(|chord| chord.to_string())
        .collect();
    format!("{area}: {} {}", command.title, keys.join(", "))
}

/// Chords no command has by default, one for each command.
fn new_chords(count: usize) -> Vec<KeyChord> {
    let defaults = Keymap::default_keymap();
    let letters = ('A'..='Z').map(|letter| format!("Ctrl+Alt+Shift+{letter}"));
    let functions = (1..=24).map(|n| format!("Ctrl+Alt+Shift+F{n}"));
    let chords: Vec<_> = letters
        .chain(functions)
        .map(|text| chord(&text))
        .filter(|&chord| defaults.lookup(chord).is_none())
        .take(count)
        .collect();
    assert_eq!(
        chords.len(),
        count,
        "more commands than chords to give them"
    );
    chords
}

#[test]
fn every_command_is_found_in_the_palette_and_rebound_from_the_app() {
    let config = ConfigDir::new("every-command");
    let mut harness = window(&config.0);
    let ids: Vec<CommandId> = harness
        .state()
        .session
        .commands()
        .iter()
        .map(|command| command.id.clone())
        .collect();
    let chords = new_chords(ids.len());

    // Ctrl+K opens the keyboard shortcuts window. Each command gets a new
    // chord: click its +, press the chord, and Enter assigns it.
    press(&mut harness, chord("Ctrl+K"));
    harness.get_by_label("Keyboard Shortcuts");
    for (id, &new) in ids.iter().zip(&chords) {
        let title = harness
            .state()
            .session
            .commands()
            .get(id)
            .unwrap()
            .title
            .clone();
        click(&mut harness, &format!("Add keys to {title}"));
        assert!(harness.state().shell.capturing(), "{id} waits for keys");
        // The keys go to the window, not to the command they run.
        press(&mut harness, new);
        harness.get_by_label("isn't used yet.");
        press(&mut harness, chord("Enter"));
        let app = harness.state();
        assert!(!app.shell.asking(), "{id} has its keys");
        assert_eq!(app.session.keymap().lookup(new), Some(id));
    }
    let mut expected = Keymap::default_keymap();
    for (id, &new) in ids.iter().zip(&chords) {
        expected.bind(new, id.clone());
    }
    assert_eq!(*harness.state().session.keymap(), expected);
    assert_eq!(harness.state().keymap_file.error(), None);

    // Ctrl+Shift+P opens the command palette, which lists every command with
    // its keys, new ones too.
    press(&mut harness, chord("Ctrl+Shift+P"));
    assert!(harness.state().shell.asking(), "the palette is open");
    for id in &ids {
        harness.get_by_label(&palette_row(harness.state(), id));
    }
    // Typing finds a command, and Enter runs it.
    send(&mut harness, vec![Event::Text("metronome".to_owned())]);
    press(&mut harness, chord("Enter"));
    assert!(harness.state().session.project().metronome);
    assert!(!harness.state().shell.asking(), "the palette closed");

    // The keys are in the user's keymap file, and work after a restart.
    drop(harness);
    let text = fs::read_to_string(config.0.join(KEYMAP_FILE)).unwrap();
    assert!(text.contains(&chords[0].to_string()), "{text}");
    let mut harness = window(&config.0);
    assert_eq!(*harness.state().session.keymap(), expected);
    let metronome = ids.iter().position(|id| *id == transport::TOGGLE_METRONOME);
    press(&mut harness, chords[metronome.unwrap()]);
    assert!(harness.state().session.project().metronome);
    // The old keys still do, as the commands kept them.
    press(&mut harness, chord("O"));
    assert!(!harness.state().session.project().metronome);
}

#[test]
fn keys_that_run_commands_can_be_given_to_others() {
    let config = ConfigDir::new("reassign");
    let mut harness = window(&config.0);
    press(&mut harness, chord("Ctrl+K"));
    // Escape and Enter can be pressed as keys too: the window waits for
    // them, and they run nothing.
    click(&mut harness, "Add keys to Toggle Metronome");
    press(&mut harness, chord("Escape"));
    harness.get_by_label("isn't used yet.");
    click(&mut harness, "Try Other Keys");
    // The loop's Ctrl+L goes to the metronome, on Reassign.
    press(&mut harness, chord("Ctrl+L"));
    harness.get_by_label("runs Toggle Loop now.");
    assert!(!harness.state().session.project().looping, "it ran nothing");
    click(&mut harness, "Reassign");

    let keymap = harness.state().session.keymap().clone();
    assert_eq!(
        keymap.lookup(chord("Ctrl+L")),
        Some(&transport::TOGGLE_METRONOME)
    );
    assert_eq!(keymap.chords_for(&transport::TOGGLE_LOOP).count(), 0);
    // The window's search field doesn't have the focus any more, so the keys
    // run the command.
    press(&mut harness, chord("Ctrl+L"));
    assert!(harness.state().session.project().metronome);
    assert!(!harness.state().session.project().looping);
}

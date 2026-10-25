//! The done test for key remapping (N3 in the Tier 1 plan): every command
//! can be found in the command palette and given new keys on the Settings
//! window's Keys page, without editing a file. The app saves the keys to the
//! user's keymap file, and they work the next time it starts.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;

use eframe::egui::Event;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::{CommandId, KeyChord, Keymap, transport, workspace};

use super::App;
use super::testing::{TempDir, chord, press, send, window};
use crate::settings::KEYMAP_FILE;

/// Scrolls the widget called `label` into view, and clicks it.
fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    harness.run();
    harness.get_by_label(label).click();
    harness.run();
}

/// A command as the command palette lists it: its area and title, then its
/// keys. The palette names a workspace switch by the workspace it switches
/// to, and leaves out those for slots without one.
fn palette_row(app: &App, id: &CommandId) -> Option<String> {
    let command = app.session.commands().get(id).unwrap();
    let area = id.as_str().split('.').next().unwrap();
    let area = area[..1].to_uppercase() + &area[1..];
    let title = match workspace::slot_of(id) {
        Some(slot) => format!("Switch to {}", app.workspaces.name(usize::from(slot) - 1)?),
        None => command.title.clone(),
    };
    let keys: Vec<_> = app
        .session
        .keymap()
        .chords_for(id)
        .map(|chord| chord.to_string())
        .collect();
    Some(format!("{area}: {title} {}", keys.join(", ")))
}

/// Chords no command has by default, one for each command.
fn new_chords(count: usize) -> Vec<KeyChord> {
    let defaults = Keymap::default_keymap();
    let letters = ('A'..='Z').map(|letter| format!("Ctrl+Alt+Shift+{letter}"));
    let digits = (0..=9).map(|n| format!("Ctrl+Alt+Shift+{n}"));
    let functions = ["Ctrl+Alt+Shift", "Ctrl+Shift", "Alt+Shift", "Ctrl+Alt"]
        .into_iter()
        .flat_map(|mods| (1..=24).map(move |n| format!("{mods}+F{n}")));
    let chords: Vec<_> = letters
        .chain(digits)
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
    let config = TempDir::new("remapping-every-command");
    let mut harness = window(&config.0, None);
    // Keys can't give a command arguments, so the commands that need them,
    // such as Move Clips, have none.
    let ids: Vec<CommandId> = harness
        .state()
        .session
        .commands()
        .iter()
        .filter(|command| !command.needs_arguments())
        .map(|command| command.id.clone())
        .collect();
    let chords = new_chords(ids.len());

    // Ctrl+K opens the Settings window at its Keys page. Each command gets a
    // new chord: click its +, press the chord, and Enter assigns it.
    press(&mut harness, chord("Ctrl+K"));
    harness.get_by_label("Settings");
    harness.get_by_label("Keys page");
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
        // The keys go to the page, not to the command they run.
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
        if let Some(row) = palette_row(harness.state(), id) {
            harness.get_by_label(&row);
        }
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
    let mut harness = window(&config.0, None);
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
    let config = TempDir::new("remapping-reassign");
    let mut harness = window(&config.0, None);
    press(&mut harness, chord("Ctrl+K"));
    // Escape and Enter can be pressed as keys too: the page waits for them,
    // and they run nothing.
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
    // The page's search field doesn't have the focus any more, so the keys
    // run the command.
    press(&mut harness, chord("Ctrl+L"));
    assert!(harness.state().session.project().metronome);
    assert!(!harness.state().session.project().looping);
}

//! The done test for the History panel (milestone 7 of the Tier 2 UI
//! foundations): it lists the steps of the undo history by name, clicking
//! an earlier step brings the project back to it, its arrow keys step
//! through the history while it has the focus, and it moves to a window of
//! its own and comes back with the workspace, as the other panels do.
//!
//! It drives the app's window as eframe would, without an audio device.

use eframe::egui::accesskit::{Role, Toggled};
use eframe::egui::{Event, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::{Args, Focus, track, view};
use musicable_ui::{HISTORY, Layout};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::workspaces::{self, WORKSPACES_FOLDER};

/// The rows the History panel lists, and the one the project is at.
fn rows(harness: &Harness<'static, App>) -> (Vec<String>, Vec<String>) {
    let rows: Vec<_> = harness.get_all_by_role(Role::ListItem).collect();
    let name = |row: &egui_kittest::Node<'_>| row.accesskit_node().label().unwrap_or_default();
    // egui tells a selected row by its being toggled on.
    let current = (rows.iter()).filter(|row| row.accesskit_node().toggled() == Some(Toggled::True));
    (rows.iter().map(name).collect(), current.map(name).collect())
}

/// The project's tracks, by name.
fn tracks(harness: &Harness<'static, App>) -> Vec<String> {
    let project = harness.state().session.project();
    project
        .tracks
        .iter()
        .map(|track| track.name.clone())
        .collect()
}

/// Clicks the History panel's row called `name`, as the user does.
fn click(harness: &mut Harness<'static, App>, name: &str) {
    let at = harness
        .get_by_role_and_label(Role::ListItem, name)
        .rect()
        .center();
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

#[test]
fn clicking_a_step_in_the_history_panel_goes_back_to_it() {
    let config = TempDir::new("undoing");
    let mut harness = window(&config.0, None);
    let session = &mut harness.state_mut().session;
    for command in [
        track::NEW_AUDIO_TRACK,
        track::NEW_MIDI_TRACK,
        track::NEW_RETURN_TRACK,
    ] {
        session.run(&command).unwrap();
    }
    // Ctrl+Alt+H shows the panel, with a row for each step under one for
    // the project as it opened.
    press(&mut harness, chord("Ctrl+Alt+H"));
    assert!(harness.state().shell.panel_shown(HISTORY));
    let listed = [
        "Untitled",
        "New Audio Track",
        "New MIDI Track",
        "New Return Track",
    ];
    let listed = listed.map(String::from).to_vec();
    assert_eq!(
        rows(&harness),
        (listed.clone(), vec!["New Return Track".into()])
    );

    // Clicking an earlier step takes the project back to it. The panel
    // takes the focus, and the steps after it stay listed, for redo.
    click(&mut harness, "New Audio Track");
    assert_eq!(tracks(&harness), ["Audio 1"]);
    assert_eq!(
        rows(&harness),
        (listed.clone(), vec!["New Audio Track".into()])
    );
    assert_eq!(harness.state().session.editor().focus, Focus::HISTORY);
    // Its arrow keys step through them, as redo and undo do.
    press(&mut harness, chord("Down"));
    assert_eq!(tracks(&harness), ["Audio 1", "MIDI 1"]);
    press(&mut harness, chord("Up"));
    press(&mut harness, chord("Up"));
    assert_eq!(tracks(&harness), Vec::<String>::new());
    assert_eq!(rows(&harness).1, ["Untitled"]);
    // Back where it opened, the project has no changes to save.
    assert!(!harness.state().session.is_modified());
    // The last step makes them all again.
    click(&mut harness, "New Return Track");
    assert_eq!(tracks(&harness), ["Audio 1", "MIDI 1", "Return 1"]);

    // A new change after going back drops the steps undone, as undo does.
    click(&mut harness, "New Audio Track");
    (harness.state_mut().session)
        .run(&track::NEW_GROUP_TRACK)
        .unwrap();
    harness.run();
    let listed = ["Untitled", "New Audio Track", "New Group Track"];
    assert_eq!(rows(&harness).0, listed);
    assert_eq!(tracks(&harness), ["Audio 1", "Group 1"]);
}

#[test]
fn the_history_panel_works_in_a_window_of_its_own_and_comes_back_with_it() {
    let config = TempDir::new("undoing-window");
    let mut harness = window(&config.0, None);
    let session = &mut harness.state_mut().session;
    for command in [track::NEW_AUDIO_TRACK, track::NEW_MIDI_TRACK] {
        session.run(&command).unwrap();
    }
    // The panel goes in a window of its own, as the others do.
    session.run(&view::TOGGLE_HISTORY).unwrap();
    let history = Args::new().with("panel", HISTORY);
    session.run_with(&view::SET_FOCUS, &history).unwrap();
    session.run(&view::MOVE_TO_NEW_WINDOW).unwrap();
    harness.run();
    assert_eq!(harness.state().shell.windows(), 1);
    // The tests show the window in the main one. A step clicked there is
    // gone to.
    harness.get_by_label("History - Untitled - Musicable");
    click(&mut harness, "New Audio Track");
    assert_eq!(tracks(&harness), ["Audio 1"]);

    // The workspace keeps the window, and it opens again with the app.
    let file = config.0.join(WORKSPACES_FOLDER).join("Default.toml");
    let default = workspaces::read(&file).unwrap().unwrap();
    assert_eq!(default.windows.len(), 2);
    assert_eq!(default.windows[1].layout, Layout::panel(HISTORY));
    drop(harness);
    let harness = window(&config.0, None);
    let shell = &harness.state().shell;
    assert_eq!(shell.windows(), 1);
    assert!(shell.panel_shown(HISTORY));
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

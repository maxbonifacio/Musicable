//! The panels' layout in the app's window, and the parts of its transport
//! bar, track headers and mixer strips: the app saves them to the default
//! workspace's file as the user changes them, and the window comes back as
//! it was left.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;

use egui_kittest::kittest::Queryable;
use musicable_commands::view;
use musicable_ui::{BROWSER, DEVICES, MIXER, TRANSPORT_BAR, Workspace};

use super::testing::{TempDir, window};
use crate::workspaces::default_file;

#[test]
fn the_window_comes_back_laid_out_as_it_was_left() {
    let config = TempDir::new("laying-out-comes-back");
    let mut harness = window(&config.0, None);
    harness.run();
    // Nothing's saved until the user changes something.
    assert!(!default_file(&config.0).exists());
    let session = &mut harness.state_mut().session;
    session.run(&view::TOGGLE_MIXER).unwrap();
    session.run(&view::TOGGLE_BROWSER).unwrap();
    harness.run();
    let text = fs::read_to_string(default_file(&config.0)).unwrap();
    let workspace = Workspace::parse(&text).unwrap();
    assert_eq!(workspace.windows.len(), 1);
    assert!(workspace.windows[0].hidden.contains(&MIXER.to_owned()));
    assert!(!workspace.windows[0].hidden.contains(&BROWSER.to_owned()));
    drop(harness);

    let harness = window(&config.0, None);
    let shell = &harness.state().shell;
    assert!(!shell.panel_shown(MIXER));
    assert!(shell.panel_shown(BROWSER) && shell.panel_shown(DEVICES));
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

#[test]
fn the_parts_come_back_as_they_were_left() {
    let config = TempDir::new("laying-out-parts");
    let mut harness = window(&config.0, None);
    harness.run();
    harness.get_by_label("Metronome").click_secondary();
    harness.run();
    harness.get_by_label("Hide Metronome").click();
    harness.run();
    assert!(harness.query_by_label("Metronome").is_none());
    let text = fs::read_to_string(default_file(&config.0)).unwrap();
    let workspace = Workspace::parse(&text).unwrap();
    assert_eq!(workspace.views.0[TRANSPORT_BAR].hidden, ["metronome"]);
    drop(harness);

    let mut harness = window(&config.0, None);
    harness.run();
    assert!(harness.query_by_label("Metronome").is_none());
    harness.get_by_label("Loop");
    // The panels are laid out as at first.
    assert!(harness.state().shell.panel_shown(MIXER));
}

#[test]
fn a_workspace_file_that_cant_be_read_leaves_the_first_layout() {
    let config = TempDir::new("laying-out-bad-file");
    let path = default_file(&config.0);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "format = \"musicable-workspace\"\nversion = 9\n").unwrap();
    let harness = window(&config.0, None);
    let shell = &harness.state().shell;
    assert!(shell.panel_shown(MIXER) && shell.panel_shown(DEVICES));
    assert!(!shell.panel_shown(BROWSER));
    assert_eq!(
        harness.state().messages,
        [
            "The workspace file is version 9, and this version of Musicable reads version 1; \
             Musicable lays the panels out as it does at first."
        ]
    );
}

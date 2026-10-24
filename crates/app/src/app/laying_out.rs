//! The panels' layout in the app's window: the app saves it to the file of
//! the workspace in use as the user changes it, switches between the
//! user's workspaces, and the window comes back laid out as it was left.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::{file, view, workspace};
use musicable_ui::{BROWSER, DEVICES, MIXER, Workspace};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::settings::SETTINGS_FILE;
use crate::workspaces::WORKSPACES_FOLDER;

/// The file of the workspace called `name`, in the config folder `config`.
fn workspace_file(config: &Path, name: &str) -> PathBuf {
    config.join(WORKSPACES_FOLDER).join(format!("{name}.toml"))
}

fn default_file(config: &Path) -> PathBuf {
    workspace_file(config, "Default")
}

/// Keeps the layout as a new workspace called `name`, as Save Workspace As
/// does once the user types the name and presses Enter.
fn save_as(harness: &mut Harness<'static, App>, name: &str) {
    harness
        .state_mut()
        .session
        .run(&workspace::SAVE_AS)
        .unwrap();
    harness.run();
    let field = harness.get_by_role_and_label(Role::TextInput, "Save Workspace As");
    field.type_text(name);
    harness.run();
    press(harness, chord("Enter"));
}

/// Which of the browser and the mixer are shown.
fn shown(harness: &Harness<'static, App>) -> [bool; 2] {
    let shell = &harness.state().shell;
    [shell.panel_shown(BROWSER), shell.panel_shown(MIXER)]
}

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

#[test]
fn workspaces_switch_with_a_key_and_the_app_opens_in_the_last_one() {
    let config = TempDir::new("laying-out-switching");
    let mut harness = window(&config.0, None);
    harness.run();
    save_as(&mut harness, "Mixing");
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    assert!(workspace_file(&config.0, "Mixing").exists());
    assert!(!default_file(&config.0).exists(), "Default is as at first");
    // Mixing shows the browser and hides the mixer.
    let session = &mut harness.state_mut().session;
    session.run(&view::TOGGLE_BROWSER).unwrap();
    session.run(&view::TOGGLE_MIXER).unwrap();
    harness.run();
    assert_eq!(shown(&harness), [true, false]);
    // Alt+1 switches to Default, laid out as at first, and Alt+2 back.
    press(&mut harness, chord("Alt+1"));
    assert_eq!(shown(&harness), [false, true]);
    assert_eq!(harness.state().workspaces.current_name(), "Default");
    press(&mut harness, chord("Alt+2"));
    assert_eq!(shown(&harness), [true, false]);
    // The third slot has no workspace.
    press(&mut harness, chord("Alt+3"));
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    assert!(
        !default_file(&config.0).exists(),
        "switching changes nothing"
    );
    drop(harness);

    // The app opens in the workspace in use when it closed.
    let harness = window(&config.0, None);
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    assert_eq!(shown(&harness), [true, false]);
    let settings = fs::read_to_string(config.0.join(SETTINGS_FILE)).unwrap();
    assert!(
        settings.contains("[workspaces]\ncurrent = \"Mixing\"\n"),
        "{settings}"
    );
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

#[test]
fn a_project_opens_in_the_workspace_it_was_saved_with() {
    let config = TempDir::new("laying-out-project");
    let mut harness = window(&config.0, None);
    harness.run();
    save_as(&mut harness, "Mixing");
    harness
        .state_mut()
        .session
        .run(&view::TOGGLE_BROWSER)
        .unwrap();
    harness.run();
    let path = config.0.join("Song.musicable");
    harness.state_mut().path = Some(path.clone());
    harness.state_mut().session.run(&file::SAVE).unwrap();
    harness.run();
    let saved = fs::read_to_string(&path).unwrap();
    assert!(
        saved.ends_with("  \"workspace\": \"Mixing\"\n}\n"),
        "{saved}"
    );
    press(&mut harness, chord("Alt+1"));
    assert_eq!(shown(&harness), [false, true]);
    // Opening it switches to Mixing.
    let project = musicable_io::project::load(&path).unwrap();
    harness
        .state_mut()
        .open_project(project, Some(path.clone()));
    harness.run();
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    assert_eq!(shown(&harness), [true, true]);
    press(&mut harness, chord("Alt+1"));
    drop(harness);

    // So does opening the app with it, though it closed in Default.
    let harness = window(&config.0, Some(path.clone()));
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    // A workspace the user doesn't have leaves the one in use.
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("\"Mixing\"", "\"Tracking\"")).unwrap();
    drop(harness);
    let harness = window(&config.0, Some(path));
    assert_eq!(harness.state().workspaces.current_name(), "Mixing");
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

#[test]
fn workspaces_are_renamed_deleted_and_reset() {
    let config = TempDir::new("laying-out-renaming");
    let mut harness = window(&config.0, None);
    harness.run();
    save_as(&mut harness, "Mixing");
    harness
        .state_mut()
        .session
        .run(&view::TOGGLE_BROWSER)
        .unwrap();
    harness.run();
    // Renaming renames the file.
    harness.state_mut().session.run(&workspace::RENAME).unwrap();
    harness.run();
    let field = harness.get_by_role_and_label(Role::TextInput, "Rename Mixing");
    field.type_text("Mix");
    harness.run();
    press(&mut harness, chord("Enter"));
    assert_eq!(harness.state().workspaces.current_name(), "Mix");
    assert!(workspace_file(&config.0, "Mix").exists());
    assert!(!workspace_file(&config.0, "Mixing").exists());
    assert_eq!(harness.state().settings.workspaces.current, "Mix");
    assert_eq!(shown(&harness), [true, true]);
    // Reset lays it out as at first, and saves that.
    harness.state_mut().session.run(&workspace::RESET).unwrap();
    harness.run();
    assert_eq!(shown(&harness), [false, true]);
    let text = fs::read_to_string(workspace_file(&config.0, "Mix")).unwrap();
    let reset = Workspace::parse(&text).unwrap();
    assert!(reset.windows[0].hidden.contains(&BROWSER.to_owned()));
    harness
        .state_mut()
        .session
        .run(&view::TOGGLE_BROWSER)
        .unwrap();
    harness.run();
    // Deleting asks first, then switches to Default.
    harness.state_mut().session.run(&workspace::DELETE).unwrap();
    harness.run();
    harness.get_by_label("Delete the workspace Mix?");
    harness.get_by_label("Delete").click();
    harness.run();
    assert_eq!(harness.state().workspaces.current_name(), "Default");
    assert!(!workspace_file(&config.0, "Mix").exists());
    assert_eq!(shown(&harness), [false, true]);
    // Default keeps its name and can't be deleted.
    let session = &mut harness.state_mut().session;
    session.run(&workspace::RENAME).unwrap();
    session.run(&workspace::DELETE).unwrap();
    harness.run();
    assert!(!harness.state().shell.asking());
    assert_eq!(
        harness.state().messages,
        [
            "Default keeps its name: Save Workspace As makes a copy to name.",
            "Default can't be deleted: Reset Workspace lays it out as at first."
        ]
    );
}

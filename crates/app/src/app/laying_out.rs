//! The panels' layout in the app's windows, and the parts of its transport
//! bar, track headers and mixer strips: the app saves them to the file of
//! the workspace in use as the user changes them, switches between the
//! user's workspaces, and the windows come back as they were left.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::{Args, file, view, workspace};
use musicable_ui::{BROWSER, DEVICES, Layout, MIXER, TRANSPORT_BAR, Workspace};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::settings::SETTINGS_FILE;
use crate::workspaces::{self, WORKSPACES_FOLDER};

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

/// Hides the transport bar's `part` from its menu, as the user does.
fn hide(harness: &mut Harness<'static, App>, part: &str) {
    harness.get_by_label(part).click_secondary();
    harness.run();
    harness.get_by_label(&format!("Hide {part}")).click();
    harness.run();
}

/// The transport bar's parts the file of the workspace called `name` hides.
fn hidden_parts(config: &Path, name: &str) -> Vec<String> {
    let text = fs::read_to_string(workspace_file(config, name)).unwrap();
    let mut workspace = Workspace::parse(&text).unwrap();
    let lineup = workspace.views.0.remove(TRANSPORT_BAR);
    lineup.map(|lineup| lineup.hidden).unwrap_or_default()
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
fn the_parts_come_back_as_they_were_left() {
    let config = TempDir::new("laying-out-parts");
    let mut harness = window(&config.0, None);
    harness.run();
    hide(&mut harness, "Metronome");
    assert!(harness.query_by_label("Metronome").is_none());
    assert_eq!(hidden_parts(&config.0, "Default"), ["metronome"]);
    drop(harness);

    let mut harness = window(&config.0, None);
    harness.run();
    assert!(harness.query_by_label("Metronome").is_none());
    harness.get_by_label("Loop");
    // The panels are laid out as at first.
    assert!(harness.state().shell.panel_shown(MIXER));
}

#[test]
fn each_workspace_has_parts_of_its_own() {
    let config = TempDir::new("laying-out-parts-each");
    let mut harness = window(&config.0, None);
    harness.run();
    save_as(&mut harness, "Mixing");
    hide(&mut harness, "Metronome");
    assert_eq!(hidden_parts(&config.0, "Mixing"), ["metronome"]);
    // Default shows it, and Mixing hides it again.
    press(&mut harness, chord("Alt+1"));
    harness.get_by_label("Metronome");
    assert!(!default_file(&config.0).exists());
    press(&mut harness, chord("Alt+2"));
    assert!(harness.query_by_label("Metronome").is_none());
    // A copy keeps them.
    save_as(&mut harness, "Tracking");
    assert_eq!(hidden_parts(&config.0, "Tracking"), ["metronome"]);
    // Reset shows them as at first, and saves that.
    harness.state_mut().session.run(&workspace::RESET).unwrap();
    harness.run();
    harness.get_by_label("Metronome");
    assert!(hidden_parts(&config.0, "Tracking").is_empty());
    assert_eq!(hidden_parts(&config.0, "Mixing"), ["metronome"]);
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
fn a_workspace_keeps_a_window_of_its_own_where_it_was_left() {
    let config = TempDir::new("laying-out-windows");
    let mut harness = window(&config.0, None);
    harness.run();
    save_as(&mut harness, "Mixing");
    // Mixing has the mixer in a window of its own.
    let session = &mut harness.state_mut().session;
    let mixer = Args::new().with("panel", MIXER);
    session.run_with(&view::SET_FOCUS, &mixer).unwrap();
    session.run(&view::MOVE_TO_NEW_WINDOW).unwrap();
    harness.run();
    assert_eq!(harness.state().shell.windows(), 1);
    let path = workspace_file(&config.0, "Mixing");
    let mut mixing = workspaces::read(&path).unwrap().unwrap();
    assert_eq!(mixing.windows.len(), 2);
    assert_eq!(mixing.windows[1].layout, Layout::panel(MIXER));
    // Default has it in the main window.
    press(&mut harness, chord("Alt+1"));
    assert_eq!(harness.state().shell.windows(), 0);
    assert_eq!(shown(&harness), [false, true]);
    // The user left it on the second monitor, right of the first, which
    // the tests' windows can't be moved to.
    mixing.windows[1].at = Some([4000, 200]);
    workspaces::write(&path, &mixing).unwrap();
    // Alt+2 opens it there again.
    press(&mut harness, chord("Alt+2"));
    let shell = &harness.state().shell;
    assert_eq!(shell.windows(), 1);
    assert_eq!(shell.workspace().windows[1].at, Some([4000, 200]));
    drop(harness);

    // So does opening the app in Mixing.
    let harness = window(&config.0, None);
    let shell = &harness.state().shell;
    assert_eq!(shell.windows(), 1);
    assert_eq!(shell.workspace().windows[1].at, Some([4000, 200]));
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
        saved.contains("\n  \"workspace\": \"Mixing\",\n"),
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

#[test]
fn a_project_opens_where_its_view_was_saved_and_navigating_needs_no_saving() {
    let config = TempDir::new("laying-out-view");
    let mut harness = window(&config.0, None);
    harness.run();
    let session = &mut harness.state_mut().session;
    session.run(&view::ZOOM_IN).unwrap();
    session.run(&view::SAVE_VIEW_2).unwrap();
    session.run(&view::ZOOM_IN).unwrap();
    session.run(&view::TOGGLE_FOLLOW).unwrap();
    harness.run();
    // Moving around isn't a change to the project.
    assert!(!harness.state().session.is_modified());
    let zoom = harness.state().session.editor().viewport.zoom;
    let path = config.0.join("Song.musicable");
    harness.state_mut().path = Some(path.clone());
    harness.state_mut().session.run(&file::SAVE).unwrap();
    harness.run();
    drop(harness);

    let mut harness = window(&config.0, Some(path));
    let editor = harness.state().session.editor();
    assert_eq!(editor.viewport.zoom, zoom);
    assert!(!editor.follows(musicable_commands::navigation::Panel::Arrangement));
    assert!(!harness.state().session.is_modified());
    harness
        .state_mut()
        .session
        .run(&view::RECALL_VIEW_2)
        .unwrap();
    assert_eq!(harness.state().session.editor().viewport.zoom, zoom / 2.0);
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

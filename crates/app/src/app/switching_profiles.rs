//! The done test for profiles (milestone 6 of the Tier 2 UI foundations,
//! with the Settings window): a second profile with its own theme, keys and
//! mouse switches in one step, from the Profiles page or with a command,
//! and the app opens with the profile it closed with. Profiles are made,
//! copied, renamed and deleted on the Profiles page, and a profile's
//! changes go to its own files.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::Color32;
use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::PlayFrom;
use musicable_commands::{Modifiers, options, transport};
use musicable_ui::{Gesture, MIXER, MouseAction, Target, Theme};

use super::App;
use super::testing::{TempDir, chord, press, window};
use crate::profiles::PROFILES_FOLDER;
use crate::settings::{KEYMAP_FILE, MOUSE_FILE, ProfileSettings, SETTINGS_FILE, Settings};
use crate::settings_window::PROFILES;
use crate::workspaces::{self, WORKSPACES_FOLDER};

/// The folder of the profile called `name`, in the config folder `config`.
fn profile_dir(config: &Path, name: &str) -> PathBuf {
    config.join(PROFILES_FOLDER).join(name)
}

/// A keymap file that gives Toggle Metronome Ctrl+Alt+Shift+M alone.
const METRONOME_KEYS: &str =
    "[bindings]\n\"transport.toggle_metronome\" = [\"Ctrl+Alt+Shift+M\"]\n";

/// The profile Bitwig habits, as it would be copied from another computer:
/// light, with its own keys for the metronome, a clip's body moved by
/// dragging it, Play starting from the playhead, and the workspace Mixing,
/// which hides the mixer, in use.
fn write_bitwig_habits(harness: &Harness<'static, App>, config: &Path) {
    let dir = profile_dir(config, "Bitwig habits");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(SETTINGS_FILE),
        "[behaviour]\nplay_from = \"playhead\"\n\n[appearance]\ntheme = \"light\"\n\n\
         [workspaces]\ncurrent = \"Mixing\"\n",
    )
    .unwrap();
    fs::write(dir.join(KEYMAP_FILE), METRONOME_KEYS).unwrap();
    fs::write(dir.join(MOUSE_FILE), "[clip_body]\n\"Drag\" = \"move\"\n").unwrap();
    let mut mixing = harness.state().shell.workspace();
    mixing.windows[0].hidden.push(MIXER.to_owned());
    let path = dir.join(WORKSPACES_FOLDER).join("Mixing.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    workspaces::write(&path, &mixing).unwrap();
}

/// What dragging a clip's body does.
fn clip_drag(harness: &Harness<'static, App>) -> Option<MouseAction> {
    let mouse = harness.state().shell.mouse_map();
    mouse.action(Target::ClipBody, Gesture::Drag, Modifiers::NONE)
}

/// The command Ctrl+Alt+Shift+M runs.
fn metronome_keys(harness: &Harness<'static, App>) -> Option<String> {
    let keymap = harness.state().session.keymap();
    let id = keymap.lookup(chord("Ctrl+Alt+Shift+M"));
    id.map(|id| id.as_str().to_owned())
}

/// The colour egui's widgets fill panels with.
fn panel_fill(harness: &Harness<'static, App>) -> Color32 {
    harness.ctx.global_style().visuals.panel_fill
}

/// Says the profile in use is as Bitwig habits has it.
#[track_caller]
fn assert_bitwig_habits(harness: &Harness<'static, App>) {
    let app = harness.state();
    assert_eq!(app.profiles.current_name(), "Bitwig habits");
    assert_eq!(app.shell.theme(), &Theme::light());
    assert_eq!(panel_fill(harness), Theme::light().widgets.panel);
    assert_eq!(
        metronome_keys(harness).as_deref(),
        Some(transport::TOGGLE_METRONOME.as_str())
    );
    assert_eq!(clip_drag(harness), Some(MouseAction::Move));
    assert_eq!(app.session.editor().behaviour.play_from, PlayFrom::Playhead);
    assert_eq!(app.workspaces.current_name(), "Mixing");
    assert!(!app.shell.panel_shown(MIXER));
}

/// Says the profile in use is Default, as Musicable starts.
#[track_caller]
fn assert_default(harness: &Harness<'static, App>) {
    let app = harness.state();
    assert_eq!(app.profiles.current_name(), "Default");
    assert_eq!(app.shell.theme(), &Theme::dark());
    assert_eq!(panel_fill(harness), Theme::dark().widgets.panel);
    assert_eq!(metronome_keys(harness), None);
    assert_eq!(clip_drag(harness), Some(MouseAction::SelectTime));
    assert_eq!(
        app.session.editor().behaviour.play_from,
        PlayFrom::StartMarker
    );
    assert_eq!(app.workspaces.current_name(), "Default");
    assert!(app.shell.panel_shown(MIXER));
}

/// The settings file's settings, as the app last saved them.
fn saved(config: &Path) -> Settings {
    Settings::read(&config.join(SETTINGS_FILE)).unwrap()
}

/// Opens the Settings window at the Profiles page.
fn open_profiles(harness: &mut Harness<'static, App>) {
    let session = &mut harness.state_mut().session;
    session.run(&options::PROFILES).unwrap();
    harness.run();
    let window = &harness.state().settings_window;
    assert!(window.is_open() && window.page() == PROFILES);
}

/// Picks the profile called `name` on the Profiles page.
fn pick(harness: &mut Harness<'static, App>, name: &str) {
    let radio = harness.get_by_role_and_label(Role::RadioButton, name);
    radio.click();
    harness.run();
}

/// Clicks the button called `label`.
fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

/// Types `name` in the box called `title`, the Profiles page's box for a
/// name, and presses Enter.
fn name(harness: &mut Harness<'static, App>, title: &str, name: &str) {
    let field = harness.get_by_role_and_label(Role::TextInput, title);
    field.type_text(name);
    harness.run();
    press(harness, chord("Enter"));
}

#[test]
fn a_second_profile_with_its_own_theme_keys_and_mouse_switches_in_one_step() {
    let config = TempDir::new("profiles-switching");
    let mut harness = window(&config.0, None);
    harness.run();
    assert_default(&harness);
    // The profile's folder is copied in while Musicable runs, and the
    // Profiles page finds it.
    write_bitwig_habits(&harness, &config.0);
    open_profiles(&mut harness);
    pick(&mut harness, "Bitwig habits");
    assert_bitwig_habits(&harness);
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    // Its keys work, once the window's closed.
    press(&mut harness, chord("Escape"));
    assert!(!harness.state().settings_window.is_open());
    press(&mut harness, chord("Ctrl+Alt+Shift+M"));
    assert!(harness.state().session.project().metronome);
    // The settings file keeps the profile in use, and Default's settings
    // as they were.
    let settings = saved(&config.0);
    assert_eq!(settings.profiles.current, "Bitwig habits");
    assert_eq!(settings.profile(), ProfileSettings::default());

    // Switch to Next Profile goes back to Default, as Musicable starts, and
    // on to Bitwig habits.
    let session = &mut harness.state_mut().session;
    session.run(&options::NEXT_PROFILE).unwrap();
    harness.run();
    assert_default(&harness);
    assert_eq!(
        harness.state().messages,
        ["Switched to the profile Default."]
    );
    let session = &mut harness.state_mut().session;
    session.run(&options::NEXT_PROFILE).unwrap();
    harness.run();
    assert_bitwig_habits(&harness);
    // The notices don't pile up.
    assert_eq!(
        harness.state().messages,
        ["Switched to the profile Bitwig habits."]
    );
    // The profile's files are as they were.
    let dir = profile_dir(&config.0, "Bitwig habits");
    let text = fs::read_to_string(dir.join(KEYMAP_FILE)).unwrap();
    assert_eq!(text, METRONOME_KEYS);
    let profile = ProfileSettings::read(&dir.join(SETTINGS_FILE)).unwrap();
    assert_eq!(profile.appearance.theme, "light");
    assert_eq!(profile.workspaces.current, "Mixing");
    drop(harness);

    // Musicable opens with the profile it closed with.
    let harness = window(&config.0, None);
    assert_bitwig_habits(&harness);
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
}

#[test]
fn profiles_are_made_copied_renamed_and_deleted_on_the_profiles_page() {
    let config = TempDir::new("profiles-page");
    // Default has keys of its own.
    fs::write(config.0.join(KEYMAP_FILE), METRONOME_KEYS).unwrap();
    let mut harness = window(&config.0, None);
    harness.run();
    open_profiles(&mut harness);
    for disabled in ["Rename...", "Delete..."] {
        let button = harness.get_by_role_and_label(Role::Button, disabled);
        assert!(button.accesskit_node().is_disabled(), "{disabled}");
    }

    // New makes a profile with Musicable's own keys, and switches to it.
    click(&mut harness, "New...");
    name(&mut harness, "New Profile", "Studio");
    let app = harness.state();
    assert_eq!(app.profiles.names(), ["Default", "Studio"]);
    assert_eq!(app.profiles.current_name(), "Studio");
    assert_eq!(metronome_keys(&harness), None);
    assert!(profile_dir(&config.0, "Studio").is_dir());
    assert_eq!(saved(&config.0).profiles.current, "Studio");
    // Its changes go to its own files.
    harness.get_by_label("Appearance page").click();
    harness.run();
    harness.get_by_label("Light").click();
    harness.run();
    assert_eq!(harness.state().shell.theme(), &Theme::light());
    let path = profile_dir(&config.0, "Studio").join(SETTINGS_FILE);
    let profile = ProfileSettings::read(&path).unwrap();
    assert_eq!(profile.appearance.theme, "light");
    assert_eq!(saved(&config.0).appearance.theme, "dark");

    // Duplicate copies the profile in use, Default here, with its keys.
    open_profiles(&mut harness);
    pick(&mut harness, "Default");
    assert_eq!(harness.state().shell.theme(), &Theme::dark());
    click(&mut harness, "Duplicate...");
    name(&mut harness, "Duplicate Default", "Live habits");
    assert_eq!(harness.state().profiles.current_name(), "Live habits");
    assert_eq!(
        metronome_keys(&harness).as_deref(),
        Some(transport::TOGGLE_METRONOME.as_str())
    );
    let copied = profile_dir(&config.0, "Live habits").join(KEYMAP_FILE);
    assert_eq!(fs::read_to_string(copied).unwrap(), METRONOME_KEYS);

    // A name that's taken can't be used, in other letters either. Escape
    // closes the box, and the window stays.
    click(&mut harness, "New...");
    let field = harness.get_by_role_and_label(Role::TextInput, "New Profile");
    field.type_text("studio");
    harness.run();
    harness.get_by_label("There's a profile called studio already.");
    press(&mut harness, chord("Enter"));
    assert_eq!(harness.state().profiles.names().len(), 3);
    press(&mut harness, chord("Escape"));
    assert!(
        (harness.query_by_role_and_label(Role::TextInput, "New Profile")).is_none(),
        "the box closed"
    );
    assert!(harness.state().settings_window.is_open());

    // Rename renames its folder, and the name it's kept by.
    click(&mut harness, "Rename...");
    name(&mut harness, "Rename Live habits", "Ableton");
    assert_eq!(harness.state().profiles.current_name(), "Ableton");
    assert!(
        profile_dir(&config.0, "Ableton")
            .join(KEYMAP_FILE)
            .is_file()
    );
    assert!(!profile_dir(&config.0, "Live habits").exists());
    assert_eq!(saved(&config.0).profiles.current, "Ableton");
    assert_eq!(
        metronome_keys(&harness).as_deref(),
        Some(transport::TOGGLE_METRONOME.as_str())
    );

    // Delete asks first, then takes its folder, and Default's in use.
    click(&mut harness, "Delete...");
    harness.get_by_label("Delete the profile Ableton?");
    click(&mut harness, "Delete");
    assert!(!profile_dir(&config.0, "Ableton").exists());
    let app = harness.state();
    assert_eq!(app.profiles.names(), ["Default", "Studio"]);
    assert_eq!(app.profiles.current_name(), "Default");
    assert_eq!(saved(&config.0).profiles.current, "Default");
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );

    // Musicable opens with Studio, in use when it closed.
    pick(&mut harness, "Studio");
    drop(harness);
    let harness = window(&config.0, None);
    assert_eq!(harness.state().profiles.names(), ["Default", "Studio"]);
    assert_eq!(harness.state().profiles.current_name(), "Studio");
    assert_eq!(harness.state().shell.theme(), &Theme::light());
}

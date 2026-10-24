//! The done test for themes (milestone 4 of the Tier 2 UI foundations): a
//! theme file changes the playhead and the buttons while Musicable runs,
//! the light theme reaches egui's widgets as well as Musicable's views, and
//! a broken file falls back to its base with a notice.
//!
//! It drives the app's window as eframe would, without an audio device.

use std::fs;
use std::path::Path;
use std::time::Duration;

use eframe::egui::{self, Color32};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::options;
use musicable_ui::Theme;

use super::App;
use super::testing::{TempDir, window};
use crate::settings::{SETTINGS_FILE, Settings, THEMES_FOLDER};

const ORANGE: Color32 = Color32::from_rgb(255, 136, 0);
const GREEN: Color32 = Color32::from_rgb(0, 255, 0);

/// Writes the theme file `Studio.toml` in `config_dir`'s themes folder.
fn write_studio(config_dir: &Path, text: &str) {
    let themes = config_dir.join(THEMES_FOLDER);
    fs::create_dir_all(&themes).unwrap();
    fs::write(themes.join("Studio.toml"), text).unwrap();
}

/// Gives the app time to look at its theme file, then a frame to see it.
fn look_again(harness: &mut Harness<'static, App>) {
    std::thread::sleep(Duration::from_millis(600));
    harness.run();
}

/// The colour egui fills buttons with at rest.
fn button_fill(harness: &Harness<'static, App>) -> Color32 {
    harness
        .ctx
        .global_style()
        .visuals
        .widgets
        .inactive
        .weak_bg_fill
}

#[test]
fn theme_files_change_the_window_as_they_are_saved() {
    let dir = TempDir::new("theme-files");
    fs::write(
        dir.0.join(SETTINGS_FILE),
        "[appearance]\ntheme = \"Studio\"\n",
    )
    .unwrap();
    let studio = |playhead: &str, button: &str| {
        format!(
            "base = \"dark\"\nplayhead = \"{playhead}\"\n[widgets.resting]\nbackground = \"{button}\"\n"
        )
    };
    write_studio(&dir.0, &studio("#ff8800", "#204060"));
    let mut harness = window(&dir.0, None);
    harness.run();
    assert_eq!(harness.state().shell.theme().playhead, ORANGE);
    assert_eq!(button_fill(&harness), Color32::from_rgb(32, 64, 96));
    assert!(harness.state().messages.is_empty());

    // Saved while Musicable runs, the changes show straight away.
    write_studio(&dir.0, &studio("#00ff00", "#40602a"));
    look_again(&mut harness);
    assert_eq!(harness.state().shell.theme().playhead, GREEN);
    assert_eq!(button_fill(&harness), Color32::from_rgb(64, 96, 42));

    // A file that can't be used leaves its base, and the status bar says so.
    write_studio(&dir.0, "base = \"light\"\nplayhead = \"#00ff00\n");
    look_again(&mut harness);
    assert_eq!(harness.state().shell.theme(), &Theme::light());
    assert_eq!(harness.ctx.theme(), egui::Theme::Light);
    let messages = &harness.state().messages;
    assert_eq!(
        messages,
        &[
            "Musicable couldn't use your theme file Studio.toml, so it uses the light theme: \
             see Options > Appearance."
        ]
    );

    // Mended, the notice goes, and the file's changes are back.
    write_studio(&dir.0, &studio("#ff8800", "#204060"));
    look_again(&mut harness);
    assert_eq!(harness.state().shell.theme().playhead, ORANGE);
    assert_eq!(harness.ctx.theme(), egui::Theme::Dark);
    assert!(harness.state().messages.is_empty());
}

#[test]
fn the_theme_picked_on_the_appearance_page_is_used_and_kept() {
    let dir = TempDir::new("appearance");
    let mut harness = window(&dir.0, None);
    harness.run();
    assert_eq!(harness.state().shell.theme(), &Theme::dark());
    assert_eq!(harness.ctx.theme(), egui::Theme::Dark);

    harness
        .state_mut()
        .session
        .run(&options::APPEARANCE)
        .unwrap();
    harness.run();
    harness.get_by_label("Light").click();
    harness.run();
    let light = Theme::light();
    assert_eq!(harness.state().shell.theme(), &light);
    // egui's widgets are the theme's whether the system is light or dark,
    // and so is the window's title bar.
    assert_eq!(harness.ctx.theme(), egui::Theme::Light);
    assert_eq!(
        harness.ctx.style_of(egui::Theme::Dark),
        harness.ctx.style_of(egui::Theme::Light)
    );
    assert_eq!(
        harness.ctx.global_style().visuals.panel_fill,
        light.widgets.panel
    );
    let settings = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap();
    assert_eq!(settings.appearance.theme, "light");

    // The next time Musicable starts, it's light.
    drop(harness);
    let mut harness = window(&dir.0, None);
    harness.run();
    assert_eq!(harness.state().shell.theme(), &light);
}

#[test]
fn a_missing_theme_file_leaves_the_dark_theme_with_a_notice() {
    let dir = TempDir::new("missing-theme");
    fs::write(
        dir.0.join(SETTINGS_FILE),
        "[appearance]\ntheme = \"Gone.toml\"\n",
    )
    .unwrap();
    let mut harness = window(&dir.0, None);
    harness.run();
    assert_eq!(harness.state().shell.theme(), &Theme::dark());
    let messages = &harness.state().messages;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Gone.toml"), "{messages:?}");

    // Written later, it's used once it's there.
    let themes = dir.0.join(THEMES_FOLDER);
    fs::create_dir_all(&themes).unwrap();
    fs::write(themes.join("Gone.toml"), "playhead = \"#ff8800\"\n").unwrap();
    look_again(&mut harness);
    assert_eq!(harness.state().shell.theme().playhead, ORANGE);
    assert!(harness.state().messages.is_empty());
}

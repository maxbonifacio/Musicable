//! The app's window as its tests drive it: as eframe would, without an audio
//! device.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::App as _;
use eframe::egui::{self, Event, Key, Modifiers};
use egui_kittest::Harness;
use musicable_commands::KeyChord;

use super::App;
use crate::Output;
use crate::settings::Settings;

/// A folder of its own for each test, empty to start with, and removed when
/// the test ends.
pub(super) struct TempDir(pub(super) PathBuf);

impl TempDir {
    pub(super) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("musicable-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temporary folder is writable");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The app's window, with its settings and keymap in `config_dir`, and the
/// project at `project` open, or a new one.
pub(super) fn window(config_dir: &Path, project: Option<PathBuf>) -> Harness<'static, App> {
    let config_dir = config_dir.to_owned();
    // Frames as quick as a screen's, so two clicks make a double click.
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40)
        .build_eframe(move |_| {
            let (output, handle) = Output::none();
            let settings = Settings::default();
            App::with_output(
                project,
                Some(config_dir),
                settings,
                output,
                handle,
                Vec::new(),
            )
        })
}

/// Hands `events` to the app as eframe does: through its raw input hook,
/// then to egui.
pub(super) fn send(harness: &mut Harness<'static, App>, events: Vec<Event>) {
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
pub(super) fn press(harness: &mut Harness<'static, App>, chord: KeyChord) {
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

/// The egui key and modifiers that make `chord`, for the chords the tests
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

pub(super) fn chord(text: &str) -> KeyChord {
    text.parse().unwrap()
}

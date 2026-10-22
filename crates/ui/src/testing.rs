//! The shell as the UI's tests drive it, standing in for the app.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui_kittest::{Harness, HarnessBuilder};
use musicable_commands::{Editor, Keymap, Problem, Registry, Transport, view};
use musicable_model::time::Beats;
use musicable_model::{DeviceId, Project};

use crate::{
    Action, Audio, KeymapStatus, Levels, Playback, PluginEntry, PluginStatus, Plugins, Shell, View,
    Waveforms,
};

/// What the shell shows in the tests, and what it hands back.
pub(crate) struct State {
    pub shell: Shell,
    pub project: Project,
    pub editor: Editor,
    pub waveforms: Waveforms,
    pub commands: Registry,
    /// The keys in use. A new keymap from the shell replaces them, as it does
    /// in the app.
    pub keymap: Keymap,
    pub default_keymap: Keymap,
    pub keymap_path: Option<PathBuf>,
    pub keymap_problems: Vec<Problem>,
    /// What the Edit menu's Undo and Redo would change.
    pub undo: Option<&'static str>,
    pub redo: Option<&'static str>,
    /// What the meters read.
    pub levels: Levels,
    /// The plugins installed, whether a scan is running, and how each
    /// plugin device's plugin is doing.
    pub plugins: Vec<PluginEntry>,
    pub scanning: bool,
    pub plugin_status: BTreeMap<DeviceId, PluginStatus>,
    pub actions: Vec<Action>,
}

/// A transport that plays along: locating moves its playhead.
#[derive(Debug, Default)]
pub(crate) struct Player {
    pub playing: bool,
    pub recording: bool,
    pub playhead: Beats,
}

impl Transport for Player {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
    }

    fn locate(&mut self, at: Beats) {
        self.playhead = at;
    }

    fn playhead(&self) -> Beats {
        self.playhead
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }
}

/// Every command the app has.
pub(crate) fn commands() -> Registry {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    commands
}

/// The shell in a window, showing `project` with the app's commands and the
/// default keymap.
pub(crate) fn harness(project: Project) -> Harness<'static, State> {
    build(Harness::builder(), project)
}

/// Like [`harness`], with frames as quick as a screen's, so two clicks make
/// a double click.
pub(crate) fn quick_harness(project: Project) -> Harness<'static, State> {
    let builder = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40);
    build(builder, project)
}

fn build(builder: HarnessBuilder<State>, project: Project) -> Harness<'static, State> {
    let state = State {
        shell: Shell::new(),
        project,
        editor: Editor::default(),
        waveforms: Waveforms::default(),
        commands: commands(),
        keymap: Keymap::default_keymap(),
        default_keymap: Keymap::default_keymap(),
        keymap_path: None,
        keymap_problems: Vec::new(),
        undo: None,
        redo: None,
        levels: Levels::default(),
        plugins: Vec::new(),
        scanning: false,
        plugin_status: BTreeMap::new(),
        actions: Vec::new(),
    };
    builder.with_size([1100.0, 720.0]).build_ui_state(
        |ui, state: &mut State| {
            let view = View {
                project: &state.project,
                editor: &state.editor,
                waveforms: &state.waveforms,
                name: "Untitled",
                modified: false,
                commands: &state.commands,
                keymap: &state.keymap,
                default_keymap: &state.default_keymap,
                undo: state.undo,
                redo: state.redo,
                playback: Playback {
                    playing: false,
                    record: false,
                    playhead: Beats::ZERO,
                    seconds: 0.0,
                },
                recordings: &[],
                levels: &state.levels,
                inputs: &[],
                midi_inputs: &[],
                audio: Audio {
                    summary: "48 kHz · 128 samples · 5.3 ms",
                    running: true,
                    load: Some(0.25),
                    dropouts: 0,
                    details: "",
                },
                keymap_file: KeymapStatus {
                    path: state.keymap_path.as_deref(),
                    problems: &state.keymap_problems,
                    error: None,
                    notice: None,
                },
                messages: &[],
                plugins: Plugins {
                    installed: &state.plugins,
                    scanning: state.scanning,
                    status: &state.plugin_status,
                },
            };
            let mut actions = state.shell.show(ui, &view);
            for action in &actions {
                if let Action::SetKeymap(keymap) = action {
                    state.keymap = keymap.clone();
                    state.keymap_problems.clear();
                }
            }
            // As the app would, keep the zoom and scrolling the
            // arrangement and the piano roll ask for, as the tests don't
            // look for them.
            actions.retain(|action| match action {
                Action::RunWith(id, args)
                    if *id == view::SET_VIEWPORT || *id == view::SET_PIANO_ROLL_VIEWPORT =>
                {
                    let number = |name| args.get::<f64>(name).unwrap_or_default();
                    let roll = &mut state.editor.piano_roll;
                    let viewport = match *id == view::SET_VIEWPORT {
                        true => &mut state.editor.viewport,
                        false => {
                            roll.height = number("height") as f32;
                            roll.key_height = number("key_height") as f32;
                            &mut roll.viewport
                        }
                    };
                    viewport.start = args.get("start").unwrap_or_default();
                    viewport.zoom = number("zoom");
                    viewport.top = number("top") as f32;
                    viewport.width = number("width") as f32;
                    false
                }
                _ => true,
            });
            state.actions.extend(actions);
        },
        state,
    )
}

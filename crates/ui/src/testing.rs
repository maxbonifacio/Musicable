//! The shell as the UI's tests drive it, standing in for the app.

use std::path::PathBuf;

use egui_kittest::Harness;
use musicable_commands::{Keymap, Problem, Registry, edit, file, options, track, transport, view};
use musicable_model::Project;
use musicable_model::time::Beats;

use crate::{Action, Audio, KeymapStatus, Levels, Playback, Shell, View};

/// What the shell shows in the tests, and what it hands back.
pub(crate) struct State {
    pub shell: Shell,
    pub project: Project,
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
    pub actions: Vec<Action>,
}

/// Every command the app has.
pub(crate) fn commands() -> Registry {
    let mut commands = Registry::new();
    file::register(&mut commands);
    edit::register(&mut commands);
    transport::register(&mut commands);
    track::register(&mut commands);
    options::register(&mut commands);
    view::register(&mut commands);
    commands
}

/// The shell in a window, showing `project` with the app's commands and the
/// default keymap.
pub(crate) fn harness(project: Project) -> Harness<'static, State> {
    let state = State {
        shell: Shell::new(),
        project,
        commands: commands(),
        keymap: Keymap::default_keymap(),
        default_keymap: Keymap::default_keymap(),
        keymap_path: None,
        keymap_problems: Vec::new(),
        undo: None,
        redo: None,
        levels: Levels::default(),
        actions: Vec::new(),
    };
    Harness::builder()
        .with_size([1100.0, 720.0])
        .build_ui_state(
            |ui, state: &mut State| {
                let view = View {
                    project: &state.project,
                    name: "Untitled",
                    modified: false,
                    commands: &state.commands,
                    keymap: &state.keymap,
                    default_keymap: &state.default_keymap,
                    undo: state.undo,
                    redo: state.redo,
                    playback: Playback {
                        playing: false,
                        playhead: Beats::ZERO,
                        seconds: 0.0,
                    },
                    levels: &state.levels,
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
                };
                let actions = state.shell.show(ui, &view);
                for action in &actions {
                    if let Action::SetKeymap(keymap) = action {
                        state.keymap = keymap.clone();
                        state.keymap_problems.clear();
                    }
                }
                state.actions.extend(actions);
            },
            state,
        )
}

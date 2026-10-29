//! The user interface, drawn with [egui](https://docs.rs/egui).
//!
//! The UI shows the project and runs commands. It never reaches the engine or
//! the audio device: the app hands it a [`View`] of what to show each frame,
//! and carries out the [`Action`]s it hands back. Key presses go to the
//! keymap through [`KeyPresses`] before egui sees them.
//!
//! The window has a menu bar and a transport bar over the panels, docked in
//! splits and tabs the user drags about: the arrangement in the middle,
//! with the mixer, the routing or the performance and the devices or the
//! piano roll under it and the browser or the history beside it. The
//! command palette goes over them, and so does the app's Settings window,
//! whose Keys page the shell draws. The transport bar, the track headers and
//! the mixer strips are made of parts the user can hide, move and resize.
//! The layout and the parts are one of the user's [`Workspace`]s, which the
//! app saves and switches between, and panels move out to windows of their
//! own, which it keeps too. Views draw from a [`Theme`], and look mouse
//! gestures up in a [`MouseMap`]. `docs/app.md` describes it all.

mod arrangement;
mod browser;
mod carry;
mod devices;
mod dock;
mod ghost;
mod history;
mod keys;
#[cfg(test)]
mod manual;
mod menus;
mod mixer;
mod modulators;
mod mouse;
mod native;
mod palette;
mod panels;
mod parts;
mod performance;
mod piano_roll;
mod sample_editor;
mod search;
mod shell;
mod shortcuts;
#[cfg(test)]
mod testing;
mod theme;
mod timeline;
mod transport_bar;
mod waveform;
mod widgets;
mod windows;
mod workspace;

pub use browser::{Library, Listing, MAX_PREVIEW_VOLUME, MIN_PREVIEW_VOLUME, Preview};
pub use devices::{PluginEntry, PluginParam, PluginStatus, Plugins};
pub use keys::{KeyPresses, chord, first_press};
pub use mixer::{InputChoice, Level, Levels, MidiChoice};
pub use mouse::{
    Gesture, MouseAction, MouseFileError, MouseMap, MouseMapFile, MouseProblem, Target,
    modifier_sets,
};
pub use panels::{
    ARRANGEMENT, BROWSER, DEVICES, HISTORY, MIXER, PERFORMANCE, PIANO_ROLL, ROUTING, SAMPLE_EDITOR,
};
pub use parts::{Lineup, MIXER_STRIP, TRACK_HEADER, TRANSPORT_BAR, Views};
pub use performance::{Cpu, Performance, Row};
pub use shell::{
    Action, Audio, DEFAULT_WORKSPACE, KeymapStatus, LeadGauge, Playback, Recording, SaveChoice,
    Shell, View, Workspaces, ask_to_save,
};
pub use theme::{
    Base, BrowserSizes, DeviceSizes, DockSizes, FontSizes, Fonts, MixerSizes, PianoRollSizes,
    SampleEditorSizes, Theme, ThemeFile, ThemeFileError, ThemeProblem, WidgetState, Widgets,
    apply as apply_theme,
};
pub use waveform::{Peak, Peaks, PeaksBuilder, Waveforms};
pub use widgets::{checked_name_box, clock, confirm_box, position};
pub use windows::{Monitor, on_a_monitor};
pub use workspace::{
    Layout, MAX_NAME, Split, WindowLayout, Workspace, WorkspaceError, check_file_name, check_name,
};

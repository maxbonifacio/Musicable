//! The user interface, drawn with [egui](https://docs.rs/egui).
//!
//! The UI shows the project and runs commands. It never reaches the engine or
//! the audio device: the app hands it a [`View`] of what to show each frame,
//! and carries out the [`Action`]s it hands back. Key presses go to the
//! keymap through [`KeyPresses`] before egui sees them.
//!
//! The window has a menu bar and a transport bar over the arrangement, with
//! the mixer and the devices in panels under it, the plugin browser beside
//! it, and the keyboard shortcuts window and the command palette over them. Views draw from a [`Theme`], and look mouse
//! gestures up in a [`MouseMap`]. `docs/app.md` describes it all.

mod arrangement;
mod devices;
mod keys;
mod menus;
mod mixer;
mod mouse;
mod palette;
mod search;
mod shell;
mod shortcuts;
#[cfg(test)]
mod testing;
mod theme;
mod waveform;
mod widgets;

pub use devices::{BROWSER, DEVICES, PluginEntry, PluginStatus, Plugins};
pub use keys::{KeyPresses, chord};
pub use mixer::{Level, Levels};
pub use mouse::{Gesture, MouseAction, MouseMap, Target};
pub use shell::{
    Action, Audio, KeymapStatus, MIXER, Playback, SaveChoice, Shell, View, ask_to_save,
};
pub use theme::{DeviceSizes, MixerSizes, Theme};
pub use waveform::{Peak, Peaks, PeaksBuilder, Waveforms};
pub use widgets::{clock, position};

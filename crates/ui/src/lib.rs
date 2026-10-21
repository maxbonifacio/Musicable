//! The user interface, drawn with [egui](https://docs.rs/egui).
//!
//! The UI shows the project and runs commands. It never reaches the engine or
//! the audio device: the app hands it a [`View`] of what to show each frame,
//! and carries out the [`Action`]s it hands back. Key presses go to the
//! keymap through [`KeyPresses`] before egui sees them.
//!
//! For now this is the app shell from the Tier 0 plan: a menu bar, a
//! transport bar with meters, the project's tracks, and the keyboard
//! shortcuts window and the command palette, which Tier 1 added.
//! `docs/app.md` describes it.

mod keys;
mod menus;
mod palette;
mod search;
mod shell;
mod shortcuts;
#[cfg(test)]
mod testing;
mod widgets;

pub use keys::{KeyPresses, chord};
pub use shell::{Action, Audio, KeymapStatus, Playback, SaveChoice, Shell, View, ask_to_save};
pub use widgets::{clock, position};

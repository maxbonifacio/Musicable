//! Named command registry and remappable keymaps.
//!
//! Every user action is a named command. Menus, hotkeys, the command palette,
//! scripting and agent control all go through this registry, so remapping a
//! hotkey is just editing a [`Keymap`]. The user's changes to the default
//! keymap live in a [`KeymapFile`]. `docs/commands.md` describes the file and
//! lists the commands.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

mod key;
mod keymap;
pub mod transport;

pub use key::{ChordError, Key, KeyChord, Modifiers};
pub use keymap::{KEYMAP_VERSION, Keymap, KeymapFile, KeymapFileError, Problem};

/// Dotted, stable command name, e.g. `"transport.play"`. Keymap files refer
/// to commands by it, so a command keeps its id once it has shipped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(Cow<'static, str>);

impl CommandId {
    pub const fn new(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CommandId {
    fn from(s: &str) -> Self {
        Self(Cow::Owned(s.to_owned()))
    }
}

impl From<String> for CommandId {
    fn from(s: String) -> Self {
        Self(Cow::Owned(s))
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    /// Human-readable title shown in menus and the command palette.
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Registry {
    commands: BTreeMap<CommandId, Command>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command. Returns `false` if the id was already taken.
    pub fn register(&mut self, id: impl Into<CommandId>, title: impl Into<String>) -> bool {
        let id = id.into();
        if self.commands.contains_key(&id) {
            return false;
        }
        self.commands.insert(
            id.clone(),
            Command {
                id,
                title: title.into(),
            },
        );
        true
    }

    pub fn get(&self, id: &CommandId) -> Option<&Command> {
        self.commands.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }
}

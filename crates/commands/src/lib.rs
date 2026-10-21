//! Named command registry and remappable keymaps.
//!
//! Every user action is a named command. Menus, hotkeys, the command palette,
//! scripting and agent control all go through this registry, so remapping a
//! hotkey is just editing a [`Keymap`].

use std::collections::BTreeMap;

/// Dotted, stable command name, e.g. `"transport.play"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(pub String);

impl From<&str> for CommandId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// A key plus modifiers, e.g. Ctrl+Shift+D. Keys are named strings for now
/// (`"Space"`, `"D"`, `"F5"`) until the UI layer settles on a key type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub key: String,
    pub mods: Modifiers,
}

impl KeyChord {
    pub fn new(key: impl Into<String>, mods: Modifiers) -> Self {
        Self {
            key: key.into(),
            mods,
        }
    }
}

/// Maps key chords to commands. Keymaps are plain data so they can be saved,
/// shared and swapped (an Ableton-style default, plus user presets).
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: BTreeMap<KeyChord, CommandId>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `chord` to `command`, returning the command it replaced, if any,
    /// so the UI can warn about conflicts.
    pub fn bind(&mut self, chord: KeyChord, command: impl Into<CommandId>) -> Option<CommandId> {
        self.bindings.insert(chord, command.into())
    }

    pub fn unbind(&mut self, chord: &KeyChord) -> Option<CommandId> {
        self.bindings.remove(chord)
    }

    pub fn lookup(&self, chord: &KeyChord) -> Option<&CommandId> {
        self.bindings.get(chord)
    }

    pub fn chords_for<'a>(&'a self, command: &'a CommandId) -> impl Iterator<Item = &'a KeyChord> {
        self.bindings
            .iter()
            .filter(move |(_, c)| *c == command)
            .map(|(k, _)| k)
    }

    /// Bindings that point at commands missing from `registry`.
    pub fn dangling<'a>(
        &'a self,
        registry: &'a Registry,
    ) -> impl Iterator<Item = (&'a KeyChord, &'a CommandId)> {
        self.bindings
            .iter()
            .filter(|(_, c)| registry.get(c).is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinding_reports_the_replaced_command() {
        let mut map = Keymap::new();
        let space = KeyChord::new("Space", Modifiers::default());
        assert_eq!(map.bind(space.clone(), "transport.play"), None);
        assert_eq!(
            map.bind(space.clone(), "transport.stop"),
            Some("transport.play".into())
        );
        assert_eq!(map.lookup(&space), Some(&"transport.stop".into()));
    }

    #[test]
    fn dangling_bindings_are_found() {
        let mut reg = Registry::new();
        assert!(reg.register("transport.play", "Play"));
        assert!(!reg.register("transport.play", "Play again"));
        let mut map = Keymap::new();
        map.bind(
            KeyChord::new("Space", Modifiers::default()),
            "transport.play",
        );
        map.bind(KeyChord::new("X", Modifiers::default()), "does.not.exist");
        let dangling: Vec<_> = map.dangling(&reg).map(|(_, c)| c.0.as_str()).collect();
        assert_eq!(dangling, ["does.not.exist"]);
    }
}

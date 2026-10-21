//! Named command registry, remappable keymaps, and the transactions commands
//! change the project through.
//!
//! Every user action is a named command. Menus, hotkeys, the command palette,
//! scripting and agent control all go through this registry, so remapping a
//! hotkey is just editing a [`Keymap`]. The user's changes to the default
//! keymap live in a [`KeymapFile`]. Commands change the project only through
//! transactions on its [`Document`], which keeps the undo history.
//! `docs/commands.md` describes all of it and lists the commands.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use musicable_model::time::Beats;

mod document;
pub mod edit;
pub mod file;
mod history;
mod key;
mod keymap;
pub mod mixer;
pub mod track;
pub mod transport;

pub use document::{Committed, Document, Edit, Transaction};
pub use file::Request;
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

/// What a running command can reach.
pub struct Context<'a> {
    /// The open project. Commands change it through
    /// [`Document::transact`].
    pub document: &'a mut Document,
    pub transport: &'a mut dyn Transport,
    /// What the command asks the app to do once it has run, such as showing
    /// a file dialog, which commands can't do themselves.
    pub requests: &'a mut Vec<Request>,
}

/// The engine's transport, as commands see it. The app implements it on top
/// of the engine.
///
/// The loop, the tempo and the metronome belong to the project, so commands
/// change them through transactions, and the app passes the changes on to
/// the engine.
pub trait Transport {
    /// Whether playback is running, counting what was asked for but hasn't
    /// reached the engine yet.
    fn is_playing(&self) -> bool;
    fn play(&mut self);
    /// Stops, leaving the playhead where it is.
    fn stop(&mut self);
    /// Moves the playhead to a musical position.
    fn locate(&mut self, at: Beats);
}

type Run = Box<dyn Fn(&mut Context<'_>)>;

#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<CommandId, (Command, Run)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command that runs `run`. Returns `false` if the id was
    /// already taken.
    pub fn register(
        &mut self,
        id: impl Into<CommandId>,
        title: impl Into<String>,
        run: impl Fn(&mut Context<'_>) + 'static,
    ) -> bool {
        let id = id.into();
        if self.commands.contains_key(&id) {
            return false;
        }
        let command = Command {
            id: id.clone(),
            title: title.into(),
        };
        self.commands.insert(id, (command, Box::new(run)));
        true
    }

    pub fn get(&self, id: &CommandId) -> Option<&Command> {
        self.commands.get(id).map(|(command, _)| command)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values().map(|(command, _)| command)
    }

    /// Runs the command `id`.
    pub fn run(&self, id: &CommandId, context: &mut Context<'_>) -> Result<(), UnknownCommand> {
        let (_, run) = self
            .commands
            .get(id)
            .ok_or_else(|| UnknownCommand(id.clone()))?;
        run(context);
        Ok(())
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// No command has this id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub CommandId);

impl fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "there's no command called {}", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

//! Keys as commands: [`Request::SetKeys`] and [`Request::ResetKeys`]
//! change a command's keys as the Keys page does, and save them to the
//! keymap file. Keys aren't steps in the undo history either, so each
//! change says what the keys were, for an agent to put them back.
//!
//! [`Request::SetKeys`]: musicable_commands::Request::SetKeys
//! [`Request::ResetKeys`]: musicable_commands::Request::ResetKeys

use musicable_commands::{CommandId, KeyChord, Keymap, Scope};
use serde_json::{Value as Json, json};

use super::App;

impl App {
    /// Gives `command` the keys `keys` in place of its own, as the Keys
    /// page does. Says what its keys were and are, and the keys taken
    /// from other commands, or why it can't.
    pub(super) fn set_keys(
        &mut self,
        command: &CommandId,
        keys: Vec<(Scope, KeyChord)>,
    ) -> Result<Json, String> {
        self.change_keys(command, |keymap| {
            keymap.unbind_command(command);
            (keys.into_iter())
                .filter_map(|(scope, chord)| {
                    let taken = keymap.bind_in(scope, chord, command.clone())?;
                    (taken != *command).then_some((scope, chord, taken))
                })
                .collect()
        })
    }

    /// Gives `command` its default keys again.
    pub(super) fn reset_keys(&mut self, command: &CommandId) -> Result<Json, String> {
        let defaults = self.keymap_file.defaults().clone();
        self.change_keys(command, |keymap| keymap.reset_command(command, &defaults))
    }

    /// Changes `command`'s keys with `change`, which gives back the keys it
    /// took from other commands, and uses and saves the keymap.
    fn change_keys(
        &mut self,
        command: &CommandId,
        change: impl FnOnce(&mut Keymap) -> Vec<(Scope, KeyChord, CommandId)>,
    ) -> Result<Json, String> {
        if self.session.commands().get(command).is_none() {
            return Err(format!("There's no command {command}."));
        }
        let mut keymap = self.session.keymap().clone();
        let was = keys_of(&keymap, command);
        let taken: Vec<Json> = (change(&mut keymap).into_iter())
            .map(|(scope, chord, from)| json!({ "key": key(scope, chord), "from": from.as_str() }))
            .collect();
        let now = keys_of(&keymap, command);
        self.keymap_file.save(&keymap);
        self.session.set_keymap(keymap);
        Ok(json!({ "command": command.as_str(), "was": was, "now": now, "taken": taken }))
    }
}

/// `command`'s keys in `keymap`, as `list_commands` gives them.
fn keys_of(keymap: &Keymap, command: &CommandId) -> Vec<String> {
    (keymap.bindings_for(command))
        .map(|(scope, chord)| key(scope, chord))
        .collect()
}

/// A key as `list_commands` gives it: "Ctrl+M", or "Delete in
/// arrangement" for one that works in a panel.
fn key(scope: Scope, chord: KeyChord) -> String {
    match scope {
        Scope::Everywhere => chord.to_string(),
        Scope::Panel(panel) => format!("{chord} in {panel}"),
    }
}

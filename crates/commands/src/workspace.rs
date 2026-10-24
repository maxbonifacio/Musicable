//! The workspace commands: switching between the user's layouts of the
//! panels, and saving, renaming, deleting and resetting them.
//!
//! A workspace keeps where every panel is, in each window, and the parts
//! the transport bar, the track headers and the mixer strips show. The
//! user has up to nine, in numbered slots: Default is always the first, and
//! a new one takes the lowest slot free, so its switch command, and the key
//! that runs it, stay the same as others come and go. Workspaces are the
//! app's, not the project's, so these ask the app, with a [`Request`].

use crate::{CommandId, Registry, Request};

/// Switches to the workspace in the first slot, Default.
pub const SWITCH_1: CommandId = CommandId::new("workspace.switch_1");
/// Switches to the workspace in the second slot, and so on.
pub const SWITCH_2: CommandId = CommandId::new("workspace.switch_2");
pub const SWITCH_3: CommandId = CommandId::new("workspace.switch_3");
pub const SWITCH_4: CommandId = CommandId::new("workspace.switch_4");
pub const SWITCH_5: CommandId = CommandId::new("workspace.switch_5");
pub const SWITCH_6: CommandId = CommandId::new("workspace.switch_6");
pub const SWITCH_7: CommandId = CommandId::new("workspace.switch_7");
pub const SWITCH_8: CommandId = CommandId::new("workspace.switch_8");
pub const SWITCH_9: CommandId = CommandId::new("workspace.switch_9");
/// The switch commands, by slot.
pub const SWITCH: [CommandId; SLOTS as usize] = [
    SWITCH_1, SWITCH_2, SWITCH_3, SWITCH_4, SWITCH_5, SWITCH_6, SWITCH_7, SWITCH_8, SWITCH_9,
];
/// Asks for a name, and keeps the panels' layout and the views' parts as a
/// new workspace under it, which is the one in use from then on.
pub const SAVE_AS: CommandId = CommandId::new("workspace.save_as");
/// Asks for a new name for the workspace in use. Default keeps its name.
pub const RENAME: CommandId = CommandId::new("workspace.rename");
/// Deletes the workspace in use, once the user says so, and switches to
/// Default, which can't be deleted.
pub const DELETE: CommandId = CommandId::new("workspace.delete");
/// Lays the panels of the workspace in use out as at first, all in the
/// main window, where they start, and shows the views' parts as at first.
pub const RESET: CommandId = CommandId::new("workspace.reset");

/// How many workspaces there can be.
pub const SLOTS: u8 = 9;

/// The workspace commands but for switching, with their titles and what
/// they ask for.
const COMMANDS: [(CommandId, &str, Request); 4] = [
    (SAVE_AS, "Save Workspace As...", Request::SaveWorkspaceAs),
    (RENAME, "Rename Workspace...", Request::RenameWorkspace),
    (DELETE, "Delete Workspace...", Request::DeleteWorkspace),
    (RESET, "Reset Workspace", Request::ResetWorkspace),
];

/// The titles of the switch commands, by slot, as the command palette and
/// the Keys page show them. The Workspaces menu shows the workspaces' names
/// instead.
const TITLES: [&str; SLOTS as usize] = [
    "Switch to Workspace 1",
    "Switch to Workspace 2",
    "Switch to Workspace 3",
    "Switch to Workspace 4",
    "Switch to Workspace 5",
    "Switch to Workspace 6",
    "Switch to Workspace 7",
    "Switch to Workspace 8",
    "Switch to Workspace 9",
];

/// The slot, from 1, that the switch command `id` switches to.
pub fn slot_of(id: &CommandId) -> Option<u8> {
    let index = SWITCH.iter().position(|switch| switch == id)?;
    u8::try_from(index + 1).ok()
}

/// Registers the workspace commands.
pub fn register(registry: &mut Registry) {
    let switches: Vec<_> = (SWITCH.iter().zip(TITLES).zip(1..))
        .map(|((id, title), slot)| (id.clone(), title, Request::SwitchWorkspace(slot)))
        .collect();
    crate::register_requests(registry, &switches);
    crate::register_requests(registry, &COMMANDS);
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;

    use super::*;
    use crate::testing::StubTransport;
    use crate::{Context, Document, Keymap, NoLibrary};

    fn run(registry: &Registry, id: &CommandId) -> Vec<Request> {
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        let mut ctx = Context {
            document: &mut document,
            transport: &mut StubTransport::default(),
            requests: &mut requests,
            library: &mut NoLibrary,
        };
        registry.run(id, &mut ctx).unwrap();
        assert!(!document.is_modified());
        requests
    }

    #[test]
    fn the_commands_ask_the_app_and_switching_has_keys() {
        let mut registry = Registry::new();
        register(&mut registry);
        for (slot, id) in (1..).zip(&SWITCH) {
            assert_eq!(run(&registry, id), [Request::SwitchWorkspace(slot)]);
            assert_eq!(slot_of(id), Some(slot));
        }
        assert_eq!(slot_of(&SAVE_AS), None);
        let asked = [SAVE_AS, RENAME, DELETE, RESET].map(|id| run(&registry, &id));
        let expected = [
            Request::SaveWorkspaceAs,
            Request::RenameWorkspace,
            Request::DeleteWorkspace,
            Request::ResetWorkspace,
        ];
        assert_eq!(asked, expected.map(|request| vec![request]));
        // Alt and the slot's number switch.
        let keymap = Keymap::default_keymap();
        let keys: Vec<String> = (SWITCH.iter())
            .flat_map(|id| keymap.chords_for(id).map(|chord| chord.to_string()))
            .collect();
        assert_eq!(
            keys,
            [
                "Alt+1", "Alt+2", "Alt+3", "Alt+4", "Alt+5", "Alt+6", "Alt+7", "Alt+8", "Alt+9"
            ]
        );
    }
}

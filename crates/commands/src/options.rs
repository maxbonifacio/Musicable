//! The options commands: the Settings window, with commands for some of
//! its pages, switching profiles, and scanning for plugins.
//!
//! Commands can't show a window, or reach the user's profiles, which are
//! the app's, so these ask the app, with a [`Request`]. Agents can't see the
//! Settings window, so the commands that show it are the user's.

use crate::{CommandId, Registry, Request};

/// Shows the Settings window, at the page it showed last.
pub const SETTINGS: CommandId = CommandId::new("options.settings");
/// Shows the Settings window's Audio page: the device, its mode and the
/// buffer size.
pub const AUDIO_SETTINGS: CommandId = CommandId::new("options.audio_settings");
/// Shows the Settings window's Keys page, where every command's keys can
/// be changed.
pub const KEYBOARD_SHORTCUTS: CommandId = CommandId::new("options.keyboard_shortcuts");
/// Shows the Settings window's Appearance page, where the theme is picked.
pub const APPEARANCE: CommandId = CommandId::new("options.appearance");
/// Shows the Settings window's Profiles page, where profiles are made and
/// switched between.
pub const PROFILES: CommandId = CommandId::new("options.profiles");
/// Shows the Settings window's Agents page, which lets agents such as
/// Claude Code control Musicable.
pub const AGENTS: CommandId = CommandId::new("options.agents");
/// Switches to the next of the user's profiles, in the order the Profiles
/// page lists them, and from the last back to the first: its keys, mouse
/// map, theme, workspaces and behaviour, all at once.
pub const NEXT_PROFILE: CommandId = CommandId::new("options.next_profile");
/// Scans for plugins again, loading the libraries that were left out as bad
/// too.
pub const RESCAN_PLUGINS: CommandId = CommandId::new("options.rescan_plugins");

/// Every options command, with its title and what it asks for.
const COMMANDS: [(CommandId, &str, Request); 8] = [
    (SETTINGS, "Settings...", Request::Settings),
    (AUDIO_SETTINGS, "Audio Settings...", Request::AudioSettings),
    (
        KEYBOARD_SHORTCUTS,
        "Keyboard Shortcuts...",
        Request::KeyboardShortcuts,
    ),
    (APPEARANCE, "Appearance...", Request::Appearance),
    (PROFILES, "Profiles...", Request::Profiles),
    (AGENTS, "Agents...", Request::Agents),
    (NEXT_PROFILE, "Switch to Next Profile", Request::NextProfile),
    (RESCAN_PLUGINS, "Rescan Plugins", Request::RescanPlugins),
];

/// Registers the options commands.
pub fn register(registry: &mut Registry) {
    crate::register_requests(registry, &COMMANDS);
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;

    use super::*;
    use crate::testing::StubTransport;
    use crate::{Context, Document, Keymap, view};

    #[test]
    fn the_windows_are_commands_with_keys() {
        let mut registry = Registry::new();
        register(&mut registry);
        view::register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        let ids = [
            SETTINGS,
            AUDIO_SETTINGS,
            KEYBOARD_SHORTCUTS,
            APPEARANCE,
            PROFILES,
            AGENTS,
            NEXT_PROFILE,
            RESCAN_PLUGINS,
            view::COMMAND_PALETTE,
        ];
        for id in &ids {
            let mut ctx = Context {
                document: &mut document,
                transport: &mut StubTransport::default(),
                requests: &mut requests,
                library: &mut crate::NoLibrary,
            };
            registry.run(id, &mut ctx).unwrap();
        }
        assert_eq!(
            requests,
            [
                Request::Settings,
                Request::AudioSettings,
                Request::KeyboardShortcuts,
                Request::Appearance,
                Request::Profiles,
                Request::Agents,
                Request::NextProfile,
                Request::RescanPlugins,
                Request::CommandPalette
            ]
        );
        assert_eq!(document.take_committed(), []);
        // Agents can't see the windows, but can switch profiles and rescan.
        let for_agents: Vec<_> = (ids.iter())
            .filter(|&id| registry.get(id).unwrap().agents)
            .collect();
        assert_eq!(for_agents, [&NEXT_PROFILE, &RESCAN_PLUGINS]);

        // Live's keys for its settings and its key map mode, and the usual
        // key for a command palette, which Live doesn't have.
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        assert_eq!(chords(SETTINGS), ["Ctrl+Comma"]);
        assert!(chords(AUDIO_SETTINGS).is_empty());
        assert_eq!(chords(KEYBOARD_SHORTCUTS), ["Ctrl+K"]);
        assert_eq!(chords(view::COMMAND_PALETTE), ["Ctrl+Shift+P"]);
    }
}

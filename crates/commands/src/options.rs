//! The options commands: the Settings window, with commands for some of
//! its pages, and scanning for plugins.
//!
//! Commands can't show a window, so these ask the app to, with a [`Request`].

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
/// Scans for plugins again, loading the libraries that were left out as bad
/// too.
pub const RESCAN_PLUGINS: CommandId = CommandId::new("options.rescan_plugins");

/// Every options command, with its title and what it asks for.
const COMMANDS: [(CommandId, &str, Request); 5] = [
    (SETTINGS, "Settings...", Request::Settings),
    (AUDIO_SETTINGS, "Audio Settings...", Request::AudioSettings),
    (
        KEYBOARD_SHORTCUTS,
        "Keyboard Shortcuts...",
        Request::KeyboardShortcuts,
    ),
    (APPEARANCE, "Appearance...", Request::Appearance),
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
            RESCAN_PLUGINS,
            view::COMMAND_PALETTE,
        ];
        for id in ids {
            let mut ctx = Context {
                document: &mut document,
                transport: &mut StubTransport::default(),
                requests: &mut requests,
                library: &mut crate::NoLibrary,
            };
            registry.run(&id, &mut ctx).unwrap();
        }
        assert_eq!(
            requests,
            [
                Request::Settings,
                Request::AudioSettings,
                Request::KeyboardShortcuts,
                Request::Appearance,
                Request::RescanPlugins,
                Request::CommandPalette
            ]
        );
        assert_eq!(document.take_committed(), []);

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

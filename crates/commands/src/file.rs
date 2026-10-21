//! The file commands: new, open, save and quit.
//!
//! Commands can't show a file dialog or close a window, so these ask the app
//! to, with a [`Request`] in [`Context::requests`](crate::Context::requests).
//! The app carries it out once the command has run, asking about unsaved
//! changes first where they would be lost.

use crate::{CommandId, Registry, Request};

/// Starts a new, empty project.
pub const NEW: CommandId = CommandId::new("file.new");
/// Starts the Tier 0 demo project: a dozen test tones, looping with the
/// metronome on.
pub const NEW_DEMO: CommandId = CommandId::new("file.new_demo");
/// Asks for a project file and opens it.
pub const OPEN: CommandId = CommandId::new("file.open");
/// Saves the project to its file, or asks where if it has none yet.
pub const SAVE: CommandId = CommandId::new("file.save");
/// Asks where to save the project, and saves it there.
pub const SAVE_AS: CommandId = CommandId::new("file.save_as");
/// Closes Musicable.
pub const QUIT: CommandId = CommandId::new("file.quit");

/// Every file command, with its title and what it asks for.
const COMMANDS: [(CommandId, &str, Request); 6] = [
    (NEW, "New Project", Request::New),
    (NEW_DEMO, "New Demo Project", Request::NewDemo),
    (OPEN, "Open Project...", Request::Open),
    (SAVE, "Save Project", Request::Save),
    (SAVE_AS, "Save Project As...", Request::SaveAs),
    (QUIT, "Quit", Request::Quit),
];

/// Registers the file commands.
pub fn register(registry: &mut Registry) {
    crate::register_requests(registry, &COMMANDS);
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;
    use musicable_model::time::Beats;

    use super::*;
    use crate::{Context, Document, Keymap, Transport};

    struct Stopped;

    impl Transport for Stopped {
        fn is_playing(&self) -> bool {
            false
        }

        fn play(&mut self) {}

        fn stop(&mut self) {}

        fn locate(&mut self, _: Beats) {}
    }

    #[test]
    fn file_commands_ask_the_app() {
        let mut registry = Registry::new();
        register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        for (id, title, _) in COMMANDS {
            assert_eq!(registry.get(&id).unwrap().title, title);
            let mut ctx = Context {
                document: &mut document,
                transport: &mut Stopped,
                requests: &mut requests,
            };
            registry.run(&id, &mut ctx).unwrap();
        }
        let expected: Vec<_> = COMMANDS.iter().map(|&(_, _, request)| request).collect();
        assert_eq!(requests, expected);
        assert_eq!(document.take_committed(), []);
    }

    #[test]
    fn file_commands_have_the_usual_keys() {
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        assert_eq!(chords(NEW), ["Ctrl+N"]);
        assert_eq!(chords(OPEN), ["Ctrl+O"]);
        assert_eq!(chords(SAVE), ["Ctrl+S"]);
        assert_eq!(chords(SAVE_AS), ["Ctrl+Shift+S"]);
        assert_eq!(chords(QUIT), ["Ctrl+Q"]);
        assert_eq!(chords(NEW_DEMO), Vec::<String>::new());
    }
}

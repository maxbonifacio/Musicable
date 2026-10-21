//! The edit commands: undo and redo.
//!
//! They step through the document's undo history (see
//! [`Document::undo`](crate::Document::undo)). Playing, stopping and moving
//! the playhead aren't transactions, so undo leaves the transport alone.

use crate::{CommandId, Context, Registry};

/// Takes back the last change to the project.
pub const UNDO: CommandId = CommandId::new("edit.undo");
/// Makes the last change undone again.
pub const REDO: CommandId = CommandId::new("edit.redo");

type Handler = fn(&mut Context<'_>);

/// Every edit command, with its title and what it runs.
const COMMANDS: [(CommandId, &str, Handler); 2] = [
    (UNDO, "Undo", |ctx| {
        ctx.document.undo();
    }),
    (REDO, "Redo", |ctx| {
        ctx.document.redo();
    }),
];

/// Registers the edit commands.
pub fn register(registry: &mut Registry) {
    for (id, title, run) in COMMANDS {
        registry.register(id, title, run);
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;
    use musicable_model::time::Beats;

    use super::*;
    use crate::{Document, Keymap, Transport};

    /// Counts what undo and redo ask of the transport, which is nothing.
    #[derive(Default)]
    struct Untouched(usize);

    impl Transport for Untouched {
        fn is_playing(&self) -> bool {
            false
        }

        fn play(&mut self) {
            self.0 += 1;
        }

        fn stop(&mut self) {
            self.0 += 1;
        }

        fn locate(&mut self, _: Beats) {
            self.0 += 1;
        }
    }

    #[test]
    fn undo_and_redo_step_through_the_history() {
        let mut registry = Registry::new();
        register(&mut registry);
        crate::transport::register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut transport = Untouched::default();
        let mut requests = Vec::new();
        let mut run = |id: &CommandId, document: &mut Document| {
            let mut ctx = Context {
                document,
                transport: &mut transport,
                requests: &mut requests,
            };
            registry.run(id, &mut ctx).unwrap();
        };

        run(&crate::transport::TOGGLE_METRONOME, &mut document);
        run(&crate::transport::GO_TO_START, &mut document);
        run(&UNDO, &mut document);
        assert!(!document.project().metronome);
        run(&UNDO, &mut document);
        run(&REDO, &mut document);
        assert!(document.project().metronome);
        run(&REDO, &mut document);
        assert!(document.project().metronome);

        let names: Vec<_> = document
            .take_committed()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "Toggle Metronome",
                "Undo Toggle Metronome",
                "Redo Toggle Metronome"
            ]
        );
        // Going to the start was the only call on the transport.
        assert_eq!(transport.0, 1);
        assert_eq!(requests, []);
    }

    #[test]
    fn undo_and_redo_have_live_s_keys() {
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        assert_eq!(chords(UNDO), ["Ctrl+Z"]);
        assert_eq!(chords(REDO), ["Ctrl+Y", "Ctrl+Shift+Z"]);
        let mut registry = Registry::new();
        register(&mut registry);
        for (id, title, _) in COMMANDS {
            assert_eq!(registry.get(&id).unwrap().title, title);
        }
    }
}

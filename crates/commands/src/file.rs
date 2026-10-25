//! The file commands: new, open, save and quit, importing audio files, and
//! exporting the song to them.
//!
//! Commands can't show a file dialog or close a window, so these ask the app
//! to, with a [`Request`] in [`Context::requests`](crate::Context::requests).
//! The app carries it out once the command has run, asking about unsaved
//! changes first where they would be lost. Given a file, Open, Save As,
//! Import Audio and Export Audio skip the dialog, as scripts and agents
//! need. Each runs on its own, never among other commands as one step.

use std::path::PathBuf;

use crate::{Args, Command, CommandError, CommandId, Param, Registry, Request, Type, Value};

/// Starts a new, empty project.
pub const NEW: CommandId = CommandId::new("file.new");
/// Starts the Tier 0 demo project: a dozen test tones, looping with the
/// metronome on.
pub const NEW_DEMO: CommandId = CommandId::new("file.new_demo");
/// Opens a project file, asking for one without it.
pub const OPEN: CommandId = CommandId::new("file.open");
/// Saves the project to its file, or asks where if it has none yet.
pub const SAVE: CommandId = CommandId::new("file.save");
/// Saves the project to a file, asking where without it.
pub const SAVE_AS: CommandId = CommandId::new("file.save_as");
/// Closes Musicable.
pub const QUIT: CommandId = CommandId::new("file.quit");
/// Inserts audio files, asking for them without them: see
/// [`arrange::INSERT_AUDIO`](crate::arrange::INSERT_AUDIO).
pub const IMPORT_AUDIO: CommandId = CommandId::new("file.import_audio");
/// Shows the export window, which renders the song, or a stretch of it, to
/// audio files: the mix, and each track on its own. Given a file, renders
/// the mix to it instead, with the window's last settings.
pub const EXPORT_AUDIO: CommandId = CommandId::new("file.export_audio");

/// The file commands without arguments, with their titles and what they
/// ask for.
const BARE: [(CommandId, &str, Request); 4] = [
    (NEW, "New Project", Request::New),
    (NEW_DEMO, "New Demo Project", Request::NewDemo),
    (SAVE, "Save Project", Request::Save),
    (QUIT, "Quit", Request::Quit),
];

/// Registers the file commands.
pub fn register(registry: &mut Registry) {
    for (id, title, request) in BARE {
        let mut command = Command::new(id, title).alone();
        command.agents = request.for_agents();
        registry.add(command, move |ctx, _| {
            ctx.requests.push(request.clone());
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(OPEN, "Open Project...")
            .with_params(&[Param::asked(
                "file",
                Type::Text,
                "The project file: the app asks for one without it.",
            )])
            .alone(),
        |ctx, args| {
            ctx.requests.push(Request::Open(file(args)?));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SAVE_AS, "Save Project As...")
            .with_params(&[Param::asked(
                "file",
                Type::Text,
                "The file to save the project to, which gets the project's extension if \
                 it has none: the app asks where without it.",
            )])
            .alone(),
        |ctx, args| {
            ctx.requests.push(Request::SaveAs(file(args)?));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(IMPORT_AUDIO, "Import Audio Files...")
            .with_params(&[
                Param::asked(
                    "files",
                    Type::List(&Type::Text),
                    "The audio files' paths: the app asks for them without it.",
                ),
                Param::optional(
                    "track",
                    Type::Track,
                    "An audio track. Without one, the clips go on the selected track if \
                     it's an audio track, or else on a new one.",
                ),
                Param::optional(
                    "at",
                    Type::Beats,
                    "Where the first clip starts: the time selection's start, or else the \
                     start marker.",
                ),
            ])
            .alone(),
        |ctx, args| {
            let files = match args.opt::<Vec<String>>("files")? {
                Some(files) if files.is_empty() => {
                    return Err(CommandError::invalid("there are no files to import"));
                }
                Some(files) if files.iter().any(String::is_empty) => {
                    return Err(CommandError::invalid("a file's path can't be empty"));
                }
                files => files.map(|files| files.into_iter().map(PathBuf::from).collect()),
            };
            ctx.requests.push(Request::ImportAudio {
                files,
                track: args.opt("track")?,
                at: args.opt("at")?,
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(EXPORT_AUDIO, "Export Audio...")
            .with_params(&[Param::asked(
                "file",
                Type::Text,
                "The audio file to render the mix to, with the export window's last \
                 settings, from what the window starts out with: the time selection, or \
                 else the song. Its extension picks the format: wav, aiff or flac. \
                 Without it, the app shows the export window.",
            )])
            .alone(),
        |ctx, args| {
            ctx.requests.push(Request::ExportAudio(file(args)?));
            Ok(Value::None)
        },
    );
}

/// The `file` argument, a path, if it was given.
fn file(args: &Args) -> Result<Option<PathBuf>, CommandError> {
    match args.opt::<String>("file")? {
        Some(path) if path.is_empty() => Err(CommandError::invalid("a file's path can't be empty")),
        path => Ok(path.map(PathBuf::from)),
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;
    use musicable_model::TrackId;
    use musicable_model::time::Beats;

    use super::*;
    use crate::testing::{Setup, StubTransport};
    use crate::{Context, Document, Keymap};

    /// Every file command, with its title.
    const COMMANDS: [(CommandId, &str); 8] = [
        (NEW, "New Project"),
        (NEW_DEMO, "New Demo Project"),
        (OPEN, "Open Project..."),
        (SAVE, "Save Project"),
        (SAVE_AS, "Save Project As..."),
        (IMPORT_AUDIO, "Import Audio Files..."),
        (EXPORT_AUDIO, "Export Audio..."),
        (QUIT, "Quit"),
    ];

    #[test]
    fn file_commands_ask_the_app() {
        let mut registry = Registry::new();
        register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        for (id, title) in COMMANDS {
            let command = registry.get(&id).unwrap();
            assert_eq!(command.title, title);
            assert!(command.alone, "{id} runs alone");
            assert_eq!(command.agents, id != QUIT, "{id}");
            let mut ctx = Context {
                document: &mut document,
                transport: &mut StubTransport::default(),
                requests: &mut requests,
                library: &mut crate::NoLibrary,
            };
            registry.run(&id, &mut ctx).unwrap();
        }
        let import = Request::ImportAudio {
            files: None,
            track: None,
            at: None,
        };
        let expected = [
            Request::New,
            Request::NewDemo,
            Request::Open(None),
            Request::Save,
            Request::SaveAs(None),
            import,
            Request::ExportAudio(None),
            Request::Quit,
        ];
        assert_eq!(requests, expected);
        assert_eq!(document.take_committed(), []);
    }

    #[test]
    fn given_files_they_skip_the_dialogs() {
        let mut setup = Setup::new(Project::new());
        let file = |path: &str| Args::new().with("file", path);
        setup.run_with(&OPEN, file("/songs/a.musicable")).unwrap();
        setup.run_with(&SAVE_AS, file("/songs/b")).unwrap();
        setup
            .run_with(&EXPORT_AUDIO, file("/out/mix.flac"))
            .unwrap();
        let files = Args::new()
            .with("files", vec!["/samples/kick.wav", "loop.wav"])
            .with("track", TrackId(3))
            .with("at", Beats::new(8));
        setup.run_with(&IMPORT_AUDIO, files).unwrap();
        assert_eq!(
            setup.requests,
            [
                Request::Open(Some("/songs/a.musicable".into())),
                Request::SaveAs(Some("/songs/b".into())),
                Request::ExportAudio(Some("/out/mix.flac".into())),
                Request::ImportAudio {
                    files: Some(vec!["/samples/kick.wav".into(), "loop.wav".into()]),
                    track: Some(TrackId(3)),
                    at: Some(Beats::new(8)),
                },
            ]
        );
        let invalid = |message: &str| Err(CommandError::invalid(message));
        assert_eq!(
            setup.run_with(&OPEN, file("")),
            invalid("a file's path can't be empty")
        );
        let none = Args::new().with("files", Vec::<String>::new());
        assert_eq!(
            setup.run_with(&IMPORT_AUDIO, none),
            invalid("there are no files to import")
        );
        let empty = Args::new().with("files", vec![""]);
        assert_eq!(
            setup.run_with(&IMPORT_AUDIO, empty),
            invalid("a file's path can't be empty")
        );
        assert_eq!(setup.requests.len(), 4);
        // Scripts and agents must give the files: the app would ask.
        let registry = &setup.registry;
        for id in [OPEN, SAVE_AS, EXPORT_AUDIO, IMPORT_AUDIO] {
            let params = &registry.get(&id).unwrap().params;
            assert!(params[0].asked && params[0].optional, "{id}");
        }
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
        assert_eq!(chords(EXPORT_AUDIO), ["Ctrl+Shift+R"]);
        assert_eq!(chords(NEW_DEMO), Vec::<String>::new());
        assert_eq!(chords(IMPORT_AUDIO), Vec::<String>::new());
    }
}

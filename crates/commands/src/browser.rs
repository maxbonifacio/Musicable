//! The browser commands: searching the library of samples, previewing and
//! tagging samples, finding ones alike, the folders the library holds, what
//! kind of plugin each plugin is, and whether it renders ahead.
//!
//! The library is the app's, as the transport is: the files are on its disk,
//! and its engine plays the previews. Commands reach it through
//! [`Library`]. What a search or [`FIND_SIMILAR`] finds is what the browser
//! shows, so an agent's search shows there too. The library's changes, such
//! as tags, aren't the project's, so they aren't in its undo history: a tag
//! is taken away with [`TAG`] too, a plugin marked as another kind is
//! marked back with [`SET_PLUGIN_KIND`], and a plugin's Render Ahead is set
//! back with [`SET_PLUGIN_RENDER_AHEAD`].

use std::path::{Path, PathBuf};

use musicable_model::plugin::{PluginKind, PluginState};
use musicable_model::{DeviceId, DeviceKind, PluginFormat};

use crate::device::{FORMAT, PLUGIN_KINDS, plugin_format};
use crate::{Command, CommandError, CommandId, Param, Registry, Request, Type, Value};

/// Finds samples by their names, folders and tags.
pub const SEARCH: CommandId = CommandId::new("browser.search");
/// Finds the samples most like one.
pub const FIND_SIMILAR: CommandId = CommandId::new("browser.find_similar");
/// Plays an audio file on its own, beside the song.
pub const PREVIEW: CommandId = CommandId::new("browser.preview");
/// Stops the preview.
pub const STOP_PREVIEW: CommandId = CommandId::new("browser.stop_preview");
/// Gives a sample a tag, or takes one away.
pub const TAG: CommandId = CommandId::new("browser.tag");
/// Adds a folder of samples to the library: one the user picks, without
/// arguments.
pub const ADD_FOLDER: CommandId = CommandId::new("browser.add_folder");
/// Takes a folder out of the library. The files stay where they are.
pub const REMOVE_FOLDER: CommandId = CommandId::new("browser.remove_folder");
/// Looks through the library's folders and the presets again.
pub const RESCAN: CommandId = CommandId::new("browser.rescan");
/// Shows the browser and puts the cursor in its search field.
pub const FOCUS_SEARCH: CommandId = CommandId::new("browser.focus_search");
/// Marks an installed plugin as an instrument, a MIDI effect or an audio
/// effect, for when the scan's rule gets it wrong: the browser lists it in
/// that kind's folder from then on, and devices added from it are of that
/// kind.
pub const SET_PLUGIN_KIND: CommandId = CommandId::new("browser.set_plugin_kind");
/// Sets an installed plugin's Render Ahead, for every project: at Auto, it
/// renders ahead of the audio device with the rest of the project, unless
/// it says it must render live, and at Never, it renders live wherever it
/// is, as the device asks for each buffer, with what it plays into.
pub const SET_PLUGIN_RENDER_AHEAD: CommandId = CommandId::new("browser.set_plugin_render_ahead");

/// What a plugin's Render Ahead can be: see [`SET_PLUGIN_RENDER_AHEAD`].
const RENDER_AHEAD: &[&str] = &["auto", "never"];

/// How many files a search gives back without a limit.
const LIMIT: usize = 100;

/// The browser's library of samples and presets, as commands see it, with
/// the plugins' states that presets and copies of devices take. The app
/// implements it.
pub trait Library {
    /// The audio files of the library whose names, folders or tags have
    /// every word of `text`, best first, at most `limit` of them. The
    /// browser shows them.
    fn search(&mut self, text: &str, limit: usize) -> Vec<PathBuf>;
    /// The files of the library most like `file`, most alike first, at most
    /// `limit` of them. The browser shows them.
    fn find_similar(&mut self, file: &Path, limit: usize) -> Result<Vec<PathBuf>, String>;
    /// Plays the audio file `file` on its own, beside the song, in place of
    /// what was previewing.
    fn preview(&mut self, file: &Path) -> Result<(), String>;
    fn stop_preview(&mut self);
    /// Gives `file`, a file of the library, the user's tag `tag`, or with
    /// `on` false takes it away. Returns whether that changed it.
    fn tag(&mut self, file: &Path, tag: &str, on: bool) -> Result<bool, String>;
    /// Adds `folder` to the library, and looks through it.
    fn add_folder(&mut self, folder: &Path) -> Result<(), String>;
    /// Takes `folder` out of the library.
    fn remove_folder(&mut self, folder: &Path) -> Result<(), String>;
    /// Looks through `folder` again, or every folder and the presets.
    fn rescan(&mut self, folder: Option<&Path>) -> Result<(), String>;
    /// The device the preset at `preset` holds, set as it was saved.
    fn load_preset(&mut self, preset: &Path) -> Result<DeviceKind, String>;
    /// Saves `device`, the project's device `id`, which is called
    /// `device_name`, as a preset called `name`, or named after the device,
    /// and returns where. A plugin's state is saved afresh first, as it is
    /// for a project.
    fn save_preset(
        &mut self,
        id: DeviceId,
        device: &DeviceKind,
        device_name: &str,
        name: Option<&str>,
    ) -> Result<PathBuf, String>;
    /// The state of the plugin of the project's device `id`, saved afresh,
    /// as it is for a preset, for a copy of the device: `None` if the device
    /// isn't a plugin whose plugin plays, or it couldn't save, when a copy
    /// takes the state the project has.
    fn plugin_state(&mut self, _id: DeviceId) -> Option<PluginState> {
        None
    }
    /// Marks the installed plugin `id` in `format` as `kind`, for good:
    /// marking it as the kind the scan found takes the mark away.
    fn set_plugin_kind(
        &mut self,
        _format: PluginFormat,
        _id: &str,
        _kind: PluginKind,
    ) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }
    /// Sets the installed plugin `id` in `format` to Render Ahead Never,
    /// which has it render live wherever it is, or with `never` false back
    /// to Auto, for good.
    fn set_plugin_render_ahead(
        &mut self,
        _format: PluginFormat,
        _id: &str,
        _never: bool,
    ) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }
}

/// No library: where the app has none, as when it exports. Every command
/// that needs one says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLibrary;

/// What commands that need a library say without one.
const NO_LIBRARY: &str = "there's no library here";

impl Library for NoLibrary {
    fn search(&mut self, _: &str, _: usize) -> Vec<PathBuf> {
        Vec::new()
    }

    fn find_similar(&mut self, _: &Path, _: usize) -> Result<Vec<PathBuf>, String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn preview(&mut self, _: &Path) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn stop_preview(&mut self) {}

    fn tag(&mut self, _: &Path, _: &str, _: bool) -> Result<bool, String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn add_folder(&mut self, _: &Path) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn remove_folder(&mut self, _: &Path) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn rescan(&mut self, _: Option<&Path>) -> Result<(), String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn load_preset(&mut self, _: &Path) -> Result<DeviceKind, String> {
        Err(NO_LIBRARY.to_owned())
    }

    fn save_preset(
        &mut self,
        _: DeviceId,
        _: &DeviceKind,
        _: &str,
        _: Option<&str>,
    ) -> Result<PathBuf, String> {
        Err(NO_LIBRARY.to_owned())
    }
}

const FILE: Param = Param::required("file", Type::Text, "The audio file's path.");
const LIMIT_PARAM: Param = Param::optional(
    "limit",
    Type::Int,
    "The most files to give back: 100 without it.",
);

/// Registers the browser commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(SEARCH, "Search the Library").with_params(&[
            Param::required(
                "text",
                Type::Text,
                "What to look for: every word has to be in a file's name, its folders or its \
                 tags.",
            ),
            LIMIT_PARAM,
        ]),
        |ctx, args| {
            let text: String = args.get("text")?;
            let found = ctx.library.search(&text, limit(args)?);
            Ok(paths(found))
        },
    );
    registry.add(
        Command::new(FIND_SIMILAR, "Find Similar Samples").with_params(&[FILE, LIMIT_PARAM]),
        |ctx, args| {
            let file = path(args, "file")?;
            let limit = limit(args)?;
            ctx.library
                .find_similar(&file, limit)
                .map(paths)
                .map_err(CommandError::invalid)
        },
    );
    registry.add(
        Command::new(PREVIEW, "Preview Sample").with_params(&[FILE]),
        |ctx, args| {
            let file = path(args, "file")?;
            ctx.library.preview(&file).map_err(CommandError::invalid)?;
            Ok(Value::None)
        },
    );
    registry.register(STOP_PREVIEW, "Stop Preview", |ctx| {
        ctx.library.stop_preview()
    });
    registry.add(
        Command::new(TAG, "Tag Sample").with_params(&[
            FILE,
            Param::required("tag", Type::Text, "The tag, such as kick or dusty."),
            Param::optional("on", Type::Bool, "Takes the tag away with false."),
        ]),
        |ctx, args| {
            let file = path(args, "file")?;
            let tag = tag(&args.get::<String>("tag")?)?;
            let on = args.opt("on")?.unwrap_or(true);
            let changed = ctx
                .library
                .tag(&file, &tag, on)
                .map_err(CommandError::invalid)?;
            Ok(Value::Bool(changed))
        },
    );
    registry.add(
        Command::new(ADD_FOLDER, "Add Folder to Library...").with_params(&[Param::asked(
            "folder",
            Type::Text,
            "The folder: the app asks which without it.",
        )]),
        |ctx, args| {
            match args.opt::<String>("folder")? {
                Some(folder) => ctx
                    .library
                    .add_folder(Path::new(&folder))
                    .map_err(CommandError::invalid)?,
                None => ctx.requests.push(Request::AddLibraryFolder),
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(REMOVE_FOLDER, "Remove Folder from Library").with_params(&[Param::required(
            "folder",
            Type::Text,
            "The folder, as the library has it.",
        )]),
        |ctx, args| {
            let folder = path(args, "folder")?;
            ctx.library
                .remove_folder(&folder)
                .map_err(CommandError::invalid)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RESCAN, "Rescan Library").with_params(&[Param::optional(
            "folder",
            Type::Text,
            "One of the library's folders: every folder and the presets without it.",
        )]),
        |ctx, args| {
            let folder: Option<String> = args.opt("folder")?;
            ctx.library
                .rescan(folder.as_deref().map(Path::new))
                .map_err(CommandError::invalid)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_PLUGIN_KIND, "Set Plugin Kind").with_params(&[
            FORMAT,
            Param::required(
                "plugin",
                Type::Text,
                "The plugin's id in its format, such as org.surge-synth-team.surge-xt.",
            ),
            Param::required(
                "kind",
                Type::Choice(PLUGIN_KINDS),
                "What it does with notes: an instrument plays them, a MIDI effect, such as an \
                 arpeggiator, plays them on changed, and an audio effect works on sound.",
            ),
        ]),
        |ctx, args| {
            let format = plugin_format(args)?;
            let id: String = args.get("plugin")?;
            let key: String = args.get("kind")?;
            // The choices are checked.
            let kind = PluginKind::from_key(&key).unwrap_or_default();
            ctx.library
                .set_plugin_kind(format, &id, kind)
                .map_err(CommandError::invalid)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_PLUGIN_RENDER_AHEAD, "Set Plugin Render Ahead").with_params(&[
            FORMAT,
            Param::required(
                "plugin",
                Type::Text,
                "The plugin's id in its format, such as org.surge-synth-team.surge-xt.",
            ),
            Param::required(
                "render_ahead",
                Type::Choice(RENDER_AHEAD),
                "Auto renders it ahead of the audio device with the rest of the project, unless \
                 it says it must render live, and Never renders it live wherever it is, for a \
                 plugin that misbehaves rendered ahead, such as one that talks to its other \
                 copies.",
            ),
        ]),
        |ctx, args| {
            let format = plugin_format(args)?;
            let id: String = args.get("plugin")?;
            let never = args.get::<String>("render_ahead")? == "never";
            ctx.library
                .set_plugin_render_ahead(format, &id, never)
                .map_err(CommandError::invalid)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(FOCUS_SEARCH, "Search in Browser").not_for_agents(),
        |ctx, _| {
            ctx.requests.push(Request::FocusBrowserSearch);
            Ok(Value::None)
        },
    );
}

/// The argument `name`, a path.
fn path(args: &crate::Args, name: &str) -> Result<PathBuf, CommandError> {
    let path: String = args.get(name)?;
    if path.is_empty() {
        return Err(CommandError::invalid(format!("the {name} can't be empty")));
    }
    Ok(PathBuf::from(path))
}

/// The `limit` argument, or the default.
fn limit(args: &crate::Args) -> Result<usize, CommandError> {
    match args.opt::<i64>("limit")? {
        None => Ok(LIMIT),
        Some(limit) => usize::try_from(limit)
            .map_err(|_| CommandError::invalid("a limit can't be less than 0")),
    }
}

/// A tag as the library keeps it: in lower case, with its spaces trimmed.
fn tag(tag: &str) -> Result<String, CommandError> {
    match tag.trim().to_lowercase() {
        tag if tag.is_empty() => Err(CommandError::invalid("a tag can't be empty")),
        tag => Ok(tag),
    }
}

/// Paths as a command's result: a list of text.
fn paths(paths: Vec<PathBuf>) -> Value {
    Value::List(
        paths
            .into_iter()
            .map(|path| Value::Text(path.to_string_lossy().into_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args;
    use crate::testing::Setup;

    fn text(paths: &[&str]) -> Value {
        Value::List(paths.iter().map(|p| Value::Text((*p).to_owned())).collect())
    }

    #[test]
    fn searches_and_previews_go_to_the_library() {
        let mut setup = Setup::new(musicable_model::Project::new());
        setup.library.files = vec!["/s/Kick.wav".into(), "/s/Kick 2.wav".into()];
        let found = setup.run_with(&SEARCH, Args::new().with("text", "kick"));
        assert_eq!(found, Ok(text(&["/s/Kick.wav", "/s/Kick 2.wav"])));
        let args = Args::new().with("text", "kick").with("limit", 1_i64);
        assert_eq!(setup.run_with(&SEARCH, args), Ok(text(&["/s/Kick.wav"])));
        let args = Args::new().with("file", "/s/Kick.wav");
        assert_eq!(
            setup.run_with(&FIND_SIMILAR, args.clone()),
            Ok(text(&["/s/Kick 2.wav"]))
        );
        assert_eq!(setup.run_with(&PREVIEW, args), Ok(Value::None));
        setup.run(&STOP_PREVIEW);
        assert_eq!(
            setup.library.calls,
            [
                "search kick 100",
                "search kick 1",
                "similar /s/Kick.wav 100",
                "preview /s/Kick.wav",
                "stop preview"
            ]
        );
        let empty = Args::new().with("file", "");
        assert_eq!(
            setup.run_with(&PREVIEW, empty),
            Err(CommandError::invalid("the file can't be empty"))
        );
        let missing = Args::new().with("file", "/s/Missing.wav");
        assert_eq!(
            setup.run_with(&PREVIEW, missing),
            Err(CommandError::invalid("/s/Missing.wav isn't there"))
        );
        // Nothing the library does is the project's.
        assert_eq!(setup.committed_names(), Vec::<String>::new());
    }

    #[test]
    fn tags_are_kept_in_lower_case() {
        let mut setup = Setup::new(musicable_model::Project::new());
        setup.library.files = vec!["/s/Kick.wav".into()];
        let args = Args::new()
            .with("file", "/s/Kick.wav")
            .with("tag", " Dusty ");
        assert_eq!(setup.run_with(&TAG, args.clone()), Ok(Value::Bool(true)));
        assert_eq!(setup.run_with(&TAG, args), Ok(Value::Bool(false)));
        let off = Args::new()
            .with("file", "/s/Kick.wav")
            .with("tag", "dusty")
            .with("on", false);
        assert_eq!(setup.run_with(&TAG, off), Ok(Value::Bool(true)));
        let blank = Args::new().with("file", "/s/Kick.wav").with("tag", "  ");
        assert_eq!(
            setup.run_with(&TAG, blank),
            Err(CommandError::invalid("a tag can't be empty"))
        );
    }

    #[test]
    fn plugins_are_marked_as_kinds_in_the_library() {
        let mut setup = Setup::new(musicable_model::Project::new());
        let args = Args::new()
            .with("format", "vst3")
            .with("plugin", "ABCD")
            .with("kind", "midi_effect");
        assert_eq!(setup.run_with(&SET_PLUGIN_KIND, args), Ok(Value::None));
        assert_eq!(setup.library.calls, ["kind Vst3 ABCD midi_effect"]);
        let wrong = Args::new()
            .with("format", "vst3")
            .with("plugin", "ABCD")
            .with("kind", "synth");
        assert!(setup.run_with(&SET_PLUGIN_KIND, wrong).is_err());
        let format = Args::new()
            .with("format", "au")
            .with("plugin", "ABCD")
            .with("kind", "instrument");
        assert_eq!(
            setup.run_with(&SET_PLUGIN_KIND, format),
            Err(CommandError::invalid(
                "there's no plugin format \"au\": it's clap or vst3"
            ))
        );
        // It's the app's, not the project's.
        assert_eq!(setup.committed_names(), Vec::<String>::new());
    }

    #[test]
    fn plugins_are_set_to_render_ahead_or_not_in_the_library() {
        let mut setup = Setup::new(musicable_model::Project::new());
        let args = |render_ahead| {
            Args::new()
                .with("format", "clap")
                .with("plugin", "org.example.hardware")
                .with("render_ahead", render_ahead)
        };
        let set = &SET_PLUGIN_RENDER_AHEAD;
        assert_eq!(setup.run_with(set, args("never")), Ok(Value::None));
        assert_eq!(setup.run_with(set, args("auto")), Ok(Value::None));
        assert!(setup.run_with(set, args("sometimes")).is_err());
        assert_eq!(
            setup.library.calls,
            [
                "render ahead Clap org.example.hardware never",
                "render ahead Clap org.example.hardware auto"
            ]
        );
        // It's the app's, not the project's.
        assert_eq!(setup.committed_names(), Vec::<String>::new());
    }

    #[test]
    fn folders_come_from_the_app_without_arguments() {
        let mut setup = Setup::new(musicable_model::Project::new());
        setup.run(&ADD_FOLDER);
        setup.run(&FOCUS_SEARCH);
        assert_eq!(
            setup.requests,
            [Request::AddLibraryFolder, Request::FocusBrowserSearch]
        );
        let folder = Args::new().with("folder", "/samples");
        setup.run_with(&ADD_FOLDER, folder.clone()).unwrap();
        setup.run_with(&RESCAN, folder.clone()).unwrap();
        setup.run(&RESCAN);
        setup.run_with(&REMOVE_FOLDER, folder).unwrap();
        assert_eq!(
            setup.library.calls,
            [
                "add /samples",
                "rescan Some(\"/samples\")",
                "rescan None",
                "remove /samples"
            ]
        );
    }

    #[test]
    fn without_a_library_commands_say_so() {
        let mut library = NoLibrary;
        assert_eq!(library.search("kick", 10), Vec::<PathBuf>::new());
        assert_eq!(
            library.preview(Path::new("/s/Kick.wav")),
            Err(NO_LIBRARY.to_owned())
        );
        assert_eq!(library.rescan(None), Err(NO_LIBRARY.to_owned()));
    }
}

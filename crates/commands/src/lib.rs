//! Named command registry, remappable keymaps, and the transactions commands
//! change the project through.
//!
//! Every user action is a named command. Menus, hotkeys, the command palette,
//! scripting and agent control all go through this registry, so remapping a
//! hotkey is just editing a [`Keymap`]. The user's changes to the default
//! keymap live in a [`KeymapFile`]. Commands take typed [`Args`] and give
//! back a [`Value`], and say [`When`] they apply. They change the project
//! only through transactions on its [`Document`], which keeps the undo
//! history and the arrangement's [`Editor`] state. `docs/commands.md`
//! describes all of it and lists the commands.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use musicable_model::clip::{ClipContent, ClipId};
use musicable_model::time::Beats;
use musicable_model::{DeviceId, LocatorId, Project, TakeId, TrackId};

mod args;
pub mod arrange;
pub mod automation;
pub mod browser;
pub mod controllers;
pub mod device;
mod document;
pub mod edit;
pub mod editor;
#[cfg(test)]
mod every_command;
pub mod file;
mod history;
mod key;
mod keymap;
pub mod locator;
pub mod mixer;
pub mod notes;
pub mod options;
pub mod take;
#[cfg(test)]
mod testing;
pub mod track;
pub mod transport;
pub mod view;

pub use args::{Args, CommandError, FromValue, Param, Type, Value, param_text};
pub use browser::{Library, NoLibrary};
pub use document::{Committed, Document, Edit, Transaction};
pub use editor::{Editor, Focus};
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
    /// The arguments it takes.
    pub params: Vec<Param>,
    /// When it applies without arguments. Menus grey it out, and keys skip
    /// it, when it doesn't.
    pub when: When,
}

impl Command {
    /// A command without arguments that always applies.
    pub fn new(id: impl Into<CommandId>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            params: Vec::new(),
            when: When::Always,
        }
    }

    pub fn with_params(mut self, params: &[Param]) -> Self {
        self.params = params.to_vec();
        self
    }

    pub fn when(mut self, when: When) -> Self {
        self.when = when;
        self
    }

    /// Whether it can only run with arguments: menus, keys and the command
    /// palette, which have none to give, leave it out.
    pub fn needs_arguments(&self) -> bool {
        self.params.iter().any(|param| !param.optional)
    }
}

/// When a command applies: a condition on the selection and the project
/// that menus, keys, context menus and agents can check before running it.
/// Arguments can name what a command acts on instead, so a command run with
/// arguments needn't meet it.
///
/// The edit commands act where the [focus](Editor::focus) is, so what's
/// selected means the piano roll's notes while it has the focus, and the
/// arrangement's clips, time or tracks while the arrangement has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum When {
    #[default]
    Always,
    /// Clips are selected.
    ClipsSelected,
    /// Clips are selected, or a stretch of time is, and the arrangement has
    /// the focus.
    ClipsOrTimeSelected,
    /// Something Copy can take is selected where the focus is: notes, or
    /// clips or time.
    CanCopy,
    /// Something is selected where the focus is: notes, or clips, time or
    /// tracks.
    SomethingSelected,
    /// Tracks are selected.
    TracksSelected,
    /// Cut or Copy took something to paste where the focus is.
    CanPaste,
    /// One clip is selected, or a track is: something Rename can name.
    CanRename,
    /// The project has locators.
    HasLocators,
    /// The piano roll has the focus, and notes selected.
    NotesSelected,
    /// Quantize has notes to move: the piano roll's, while it has the focus
    /// and shows a clip, or else those of the selected MIDI clips.
    CanQuantize,
    /// The piano roll's lane shows a controller its clip moves: a curve
    /// Clear Controller can take away.
    ControllerCurve,
    /// Someone overrode automation, which can be re-enabled.
    AutomationOverridden,
}

impl When {
    pub fn applies(self, project: &Project, editor: &Editor) -> bool {
        let selection = &editor.selection;
        let roll = &editor.piano_roll;
        let notes = editor.focus == Focus::PianoRoll;
        match self {
            When::Always => true,
            When::ClipsSelected => !selection.clips.is_empty(),
            When::ClipsOrTimeSelected => {
                !notes && (!selection.clips.is_empty() || selection.time.is_some())
            }
            When::CanCopy | When::SomethingSelected | When::NotesSelected if notes => {
                roll.clip.is_some() && !roll.notes.is_empty()
            }
            When::NotesSelected => false,
            When::CanCopy => !selection.clips.is_empty() || selection.time.is_some(),
            When::SomethingSelected => !selection.is_empty(),
            When::TracksSelected => !selection.tracks.is_empty(),
            When::CanPaste if notes => roll.clip.is_some() && roll.clipboard.is_some(),
            When::CanPaste => editor.clipboard.is_some(),
            When::CanRename => selection.clips.len() == 1 || editor.selected_track.is_some(),
            When::HasLocators => !project.locators.is_empty(),
            When::CanQuantize if notes => roll.clip.is_some(),
            When::CanQuantize => selection.clips.iter().any(|&id| {
                project
                    .find_clip(id)
                    .is_some_and(|(_, clip)| matches!(clip.content, ClipContent::Midi(_)))
            }),
            When::ControllerCurve => controllers::shown(project, editor).is_some(),
            When::AutomationOverridden => project.automation.iter().any(|l| l.overridden),
        }
    }
}

/// Something only the app can do, which a command asks for: commands can't
/// show a dialog or a window, or close one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    New,
    NewDemo,
    Open,
    Save,
    SaveAs,
    Quit,
    /// Ask for audio files, read them, and insert them with
    /// [`arrange::INSERT_AUDIO`].
    ImportAudio,
    /// Show the export window, to render the song to audio files.
    ExportAudio,
    /// Show the command palette.
    CommandPalette,
    /// Show the audio settings.
    AudioSettings,
    /// Show the keyboard shortcuts, where keys are changed.
    KeyboardShortcuts,
    /// Show or hide the panel with this id, such as the mixer.
    TogglePanel(&'static str),
    /// Show the panel with this id, such as the piano roll, if it's hidden.
    ShowPanel(&'static str),
    /// Show the piano roll in place of the devices, or the devices in place
    /// of the piano roll.
    SwitchDevicesPianoRoll,
    /// Let the user type a new name for this.
    Rename(Named),
    /// Show this plugin's own window, or close it.
    PluginWindow {
        device: DeviceId,
        show: bool,
    },
    /// Scan for plugins again, loading the libraries that were bad too.
    RescanPlugins,
    /// Make what the armed tracks, and those monitoring In, played over
    /// the transport's last run, where it didn't record, into takes, as if
    /// they had recorded it then, from what the recorder holds of their
    /// inputs.
    Capture,
    /// Ask for an audio file, and give it to this sampler with
    /// [`device::SET_SAMPLE`].
    LoadSample(DeviceId),
    /// Ask for a folder, and add it to the browser's library with
    /// [`browser::ADD_FOLDER`].
    AddLibraryFolder,
    /// Show the browser, and put the cursor in its search field.
    FocusBrowserSearch,
}

/// Something in the project with a name the user can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Track(TrackId),
    Clip(ClipId),
    Locator(LocatorId),
    Take(TakeId),
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
    /// The browser's library of samples and presets.
    pub library: &'a mut dyn Library,
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
    /// Where the playhead is, as of the engine's last block.
    fn playhead(&self) -> Beats;
    /// Whether recording is on, counting what was asked for but hasn't
    /// reached the engine yet. While it is, playing records the armed
    /// tracks, within the punch range if punching in or out is on.
    fn is_recording(&self) -> bool;
    /// Turns recording on or off. Stopping turns it off too.
    fn set_recording(&mut self, on: bool);
}

type Run = Box<dyn Fn(&mut Context<'_>, &Args) -> Result<Value, CommandError>>;

#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<CommandId, (Command, Run)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command without arguments that always applies, and
    /// runs `run`. Returns `false` if the id was already taken.
    pub fn register(
        &mut self,
        id: impl Into<CommandId>,
        title: impl Into<String>,
        run: impl Fn(&mut Context<'_>) + 'static,
    ) -> bool {
        self.add(Command::new(id, title), move |ctx, _| {
            run(ctx);
            Ok(Value::None)
        })
    }

    /// Registers `command`, which runs `run` with arguments that fit its
    /// parameters. Returns `false` if the id was already taken.
    pub fn add(
        &mut self,
        command: Command,
        run: impl Fn(&mut Context<'_>, &Args) -> Result<Value, CommandError> + 'static,
    ) -> bool {
        if self.commands.contains_key(&command.id) {
            return false;
        }
        self.commands
            .insert(command.id.clone(), (command, Box::new(run)));
        true
    }

    pub fn get(&self, id: &CommandId) -> Option<&Command> {
        self.commands.get(id).map(|(command, _)| command)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values().map(|(command, _)| command)
    }

    /// Runs the command `id` without arguments, as a menu or a key does. A
    /// command that needs arguments, or doesn't apply now, does nothing.
    pub fn run(&self, id: &CommandId, context: &mut Context<'_>) -> Result<(), UnknownCommand> {
        match self.run_with(id, &Args::new(), context) {
            Err(CommandError::Unknown(id)) => Err(UnknownCommand(id)),
            _ => Ok(()),
        }
    }

    /// Runs the command `id` with `args`, which must fit its parameters,
    /// and returns its result. Without arguments, it must apply now: see
    /// [`When`].
    pub fn run_with(
        &self,
        id: &CommandId,
        args: &Args,
        context: &mut Context<'_>,
    ) -> Result<Value, CommandError> {
        let (command, run) = self
            .commands
            .get(id)
            .ok_or_else(|| CommandError::Unknown(id.clone()))?;
        args.check(&command.params)?;
        let document = &context.document;
        if args.is_empty() && !command.when.applies(document.project(), document.editor()) {
            return Err(CommandError::NotNow(id.clone()));
        }
        run(context, args)
    }

    /// Whether the command `id` applies to `document` now, without
    /// arguments: see [`When`].
    pub fn applies(&self, id: &CommandId, document: &Document) -> bool {
        self.get(id)
            .is_some_and(|c| c.when.applies(document.project(), document.editor()))
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Registers every command.
pub fn register_all(registry: &mut Registry) {
    file::register(registry);
    edit::register(registry);
    transport::register(registry);
    track::register(registry);
    arrange::register(registry);
    locator::register(registry);
    device::register(registry);
    browser::register(registry);
    mixer::register(registry);
    take::register(registry);
    notes::register(registry);
    controllers::register(registry);
    automation::register(registry);
    view::register(registry);
    options::register(registry);
}

/// Registers commands that each ask the app for something.
fn register_requests(registry: &mut Registry, commands: &[(CommandId, &str, Request)]) {
    for (id, title, request) in commands {
        let request = *request;
        registry.register(id.clone(), *title, move |ctx: &mut Context<'_>| {
            ctx.requests.push(request);
        });
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

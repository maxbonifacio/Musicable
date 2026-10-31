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
use std::path::PathBuf;

use musicable_model::clip::{ClipContent, ClipId};
use musicable_model::modulation::ModulatorId;
use musicable_model::time::Beats;
use musicable_model::{Chain, DeviceId, LocatorId, Project, TakeId, TrackId};

mod args;
pub mod arrange;
pub mod auto_warp;
pub mod automation;
pub mod browser;
pub mod controllers;
pub mod debug;
pub mod device;
mod document;
pub mod edit;
pub mod editor;
#[cfg(test)]
mod every_command;
pub mod file;
pub mod history;
pub mod json;
mod key;
mod keymap;
pub mod locator;
pub mod mixer;
pub mod modulation;
pub mod notes;
pub mod options;
pub mod panel;
pub mod rewarp;
pub mod take;
#[cfg(test)]
mod testing;
pub mod track;
pub mod transient;
pub mod transport;
pub mod view;
pub mod warp;
pub mod warp_mode;
pub mod wire;
pub mod workspace;

pub use args::{Args, CommandError, FromValue, Param, Type, Value, param_text};
pub use browser::{Library, NoLibrary};
pub use document::{Committed, Document, Edit, Loop, Transaction};
pub use editor::{Editor, Edits, Focus};
pub use history::Steps;
pub use key::{ChordError, Key, KeyChord, Modifiers};
pub use keymap::{KEYMAP_VERSION, Keymap, KeymapFile, KeymapFileError, Problem, Scope};

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
    /// Whether agents can run it. Those that only show a window or ask the
    /// user for something, which an agent can't see or answer, are the
    /// user's, and so is Quit.
    pub agents: bool,
    /// Whether it runs on its own only, never among others as one step of
    /// the undo history ([`Registry::run_batch`]): it opens, saves or
    /// replaces the project, or steps through the undo history.
    pub alone: bool,
    /// Whether it needs at least one of its arguments, though each of them
    /// is optional: see [`needs_arguments`](Self::needs_arguments).
    pub some_arguments: bool,
}

impl Command {
    /// A command without arguments that always applies.
    pub fn new(id: impl Into<CommandId>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            params: Vec::new(),
            when: When::Always,
            agents: true,
            alone: false,
            some_arguments: false,
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

    /// Leaves it to the user: see [`agents`](Self::agents).
    pub fn not_for_agents(mut self) -> Self {
        self.agents = false;
        self
    }

    /// Runs it on its own only: see [`alone`](Self::alone).
    pub fn alone(mut self) -> Self {
        self.alone = true;
        self
    }

    /// Runs it with some of its arguments only: see
    /// [`some_arguments`](Self::some_arguments).
    pub fn with_some_arguments(mut self) -> Self {
        self.some_arguments = true;
        self
    }

    /// Whether it can only run with arguments: menus, keys and the command
    /// palette, which have none to give, leave it out.
    pub fn needs_arguments(&self) -> bool {
        self.some_arguments || self.params.iter().any(|param| !param.optional)
    }
}

/// When a command applies: a condition on the selection and the project
/// that menus, keys, context menus and agents can check before running it.
/// Arguments can name what a command acts on instead, so a command run with
/// arguments needn't meet it.
///
/// The edit commands act where the [focus](Editor::focus) is, so what's
/// selected means the piano roll's notes while it has the focus, the Sample
/// Editor's warp markers while it has it, the devices panel's devices while
/// it has it, and the arrangement's clips, time or tracks while the
/// arrangement has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum When {
    #[default]
    Always,
    /// Clips are selected.
    ClipsSelected,
    /// Clips are selected, or a stretch of time is, and the arrangement or
    /// the mixer has the focus.
    ClipsOrTimeSelected,
    /// Something Copy can take is selected where the focus is: notes,
    /// devices, or clips or time.
    CanCopy,
    /// Something is selected where the focus is: notes, warp markers,
    /// devices, or clips, time or tracks.
    SomethingSelected,
    /// Something Zoom to Selection can show is selected where the focus is:
    /// notes, warp markers, or clips or time.
    CanZoomToSelection,
    /// The panel with the focus has things to select: the arrangement's
    /// clips, the piano roll's notes, or the devices panel's devices.
    CanSelectAll,
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
    /// The devices panel has devices selected, wherever the focus is.
    DevicesSelected,
    /// Quantize has something to move: the piano roll's notes, while it has
    /// the focus and shows a clip, the transients of the Sample Editor's
    /// warped clip, while it has the focus, or else the notes of the
    /// selected MIDI clips, or the transients of the selected warped audio
    /// clips.
    CanQuantize,
    /// The piano roll's lane shows a controller its clip moves: a curve
    /// Clear Controller can take away.
    ControllerCurve,
    /// Someone overrode automation, which can be re-enabled.
    AutomationOverridden,
    /// The Sample Editor shows an audio clip.
    SampleEditorClip,
    /// The Sample Editor shows a warped audio clip.
    WarpedClip,
    /// The Sample Editor shows a warped audio clip, with a stretch of its
    /// time selected.
    TimeInSampleEditor,
    /// The Sample Editor has the focus, and warp markers selected.
    MarkersSelected,
    /// The Sample Editor shows a warped audio clip that has taken its
    /// transients from analysis.
    Transients,
    /// The Sample Editor has the focus, and transients in its time
    /// selection.
    TransientsSelected,
}

impl When {
    pub fn applies(self, project: &Project, editor: &Editor) -> bool {
        let selection = &editor.selection;
        let roll = &editor.piano_roll;
        let edits = editor.focus.edits();
        let notes = edits == Edits::Notes;
        let markers = edits == Edits::Markers;
        let devices = edits == Edits::Devices;
        let arranging = edits == Edits::Arrangement;
        let sample = &editor.sample_editor;
        match self {
            When::Always => true,
            When::ClipsSelected => !selection.clips.is_empty(),
            When::CanCopy
            | When::SomethingSelected
            | When::NotesSelected
            | When::CanZoomToSelection
                if notes =>
            {
                roll.clip.is_some() && !roll.notes.is_empty()
            }
            When::NotesSelected => false,
            When::SomethingSelected | When::MarkersSelected | When::CanZoomToSelection
                if markers =>
            {
                sample.clip.is_some() && !sample.markers.is_empty()
            }
            When::MarkersSelected => false,
            When::CanCopy | When::SomethingSelected if devices => {
                !editor.selected_devices(project).is_empty()
            }
            When::DevicesSelected => !editor.selected_devices(project).is_empty(),
            When::ClipsOrTimeSelected | When::CanCopy | When::CanZoomToSelection => {
                arranging && (!selection.clips.is_empty() || selection.time.is_some())
            }
            When::SomethingSelected => arranging && !selection.is_empty(),
            When::CanSelectAll if devices => (editor.selected_track)
                .and_then(|chain| project.devices(chain))
                .is_some_and(|shown| !shown.is_empty()),
            When::CanSelectAll => edits != Edits::Nothing,
            When::TracksSelected => !selection.tracks.is_empty(),
            When::CanPaste if notes => roll.clip.is_some() && roll.clipboard.is_some(),
            When::CanPaste if devices => {
                editor.selected_track.is_some() && editor.devices.clipboard.is_some()
            }
            When::CanPaste => arranging && editor.clipboard.is_some(),
            When::CanRename => {
                selection.clips.len() == 1 || matches!(editor.selected_track, Some(Chain::Track(_)))
            }
            When::HasLocators => !project.locators.is_empty(),
            When::CanQuantize if notes => roll.clip.is_some(),
            When::CanQuantize if markers => sample
                .clip
                .is_some_and(|id| transient::has_transients(project, id)),
            When::CanQuantize => selection.clips.iter().any(|&id| {
                (project.find_clip(id))
                    .is_some_and(|(_, clip)| matches!(clip.content, ClipContent::Midi(_)))
                    || transient::has_transients(project, id)
            }),
            When::ControllerCurve => controllers::shown(project, editor).is_some(),
            When::AutomationOverridden => project.automation.iter().any(|l| l.overridden),
            When::SampleEditorClip => sample.clip.is_some(),
            When::WarpedClip => sample
                .clip
                .is_some_and(|id| warp::warped(project, id).is_ok()),
            When::TimeInSampleEditor => {
                sample.time.is_some()
                    && (sample.clip).is_some_and(|id| warp::warped(project, id).is_ok())
            }
            When::Transients => sample.clip.is_some_and(|id| {
                warp::warped(project, id).is_ok_and(|(_, _, warp)| warp.onsets.is_some())
            }),
            When::TransientsSelected => {
                markers
                    && warp::selected_transients(project, editor)
                        .is_some_and(|(_, frames)| !frames.is_empty())
            }
        }
    }
}

/// Something only the app can do, which a command asks for: commands can't
/// show a dialog or a window, or close one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    New,
    NewDemo,
    /// Open the project file at this path, or else ask for one.
    Open(Option<PathBuf>),
    Save,
    /// Save the project to the file at this path, or else ask where.
    SaveAs(Option<PathBuf>),
    Quit,
    /// Read audio files, and insert them with [`arrange::INSERT_AUDIO`] on
    /// `track` from `at`, or where it puts them without those: these files,
    /// or else those the user picks.
    ImportAudio {
        files: Option<Vec<PathBuf>>,
        track: Option<TrackId>,
        at: Option<Beats>,
    },
    /// Render the song to the audio file at `file`, as the export window
    /// last did but for what `options` say, or else show the export window.
    ExportAudio {
        file: Option<PathBuf>,
        options: ExportOptions,
    },
    /// Show the command palette.
    CommandPalette,
    /// Show the Settings window, at the page it showed last.
    Settings,
    /// Show the Settings window's Audio page.
    AudioSettings,
    /// Show the Settings window's Keys page, where keys are changed.
    KeyboardShortcuts,
    /// Show the Settings window's Appearance page, where the theme is
    /// picked.
    Appearance,
    /// Show the Settings window's Profiles page.
    Profiles,
    /// Show the Settings window's Agents page, where agents are let in.
    Agents,
    /// Switch to the next of the user's profiles, or from the last back to
    /// the first.
    NextProfile,
    /// Show or hide the panel with this id, such as the mixer.
    TogglePanel(&'static str),
    /// Show the panel with this id, such as the piano roll, if it's hidden.
    ShowPanel(&'static str),
    /// Bring the panel with this id, the piano roll or the Sample Editor,
    /// in front of the devices, or the devices in front of it.
    SwitchDevicesClipEditor(&'static str),
    /// Bring the devices panel in front of the piano roll, unless it's
    /// hidden, with this device in sight: it was just added, moved or
    /// copied.
    ShowDevice(DeviceId),
    /// Move the panel with the focus to a new window of its own.
    MoveToNewWindow,
    /// Send the panel with the focus back to the main window.
    MoveToMainWindow,
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
    /// Switch to the workspace in this slot, from 1, the first being
    /// Default.
    SwitchWorkspace(u8),
    /// Keep the panels' layout as a new workspace under this name, or ask
    /// for one.
    SaveWorkspaceAs(Option<String>),
    /// Give the workspace in use this name, or ask for one.
    RenameWorkspace(Option<String>),
    /// Delete the workspace in use, once the user says so.
    DeleteWorkspace,
    /// Lay the panels of the workspace in use out as at first.
    ResetWorkspace,
    /// Change a setting, as the Settings window does: `key` names it as
    /// the settings file does, such as `audio.render_ahead`, and `value` is
    /// what to make it, as text, such as `long`.
    SetSetting {
        key: String,
        value: String,
    },
    /// Play through another audio device, or through the same one with other
    /// settings. What's `None` stays as it is.
    SetAudioDevice(AudioDevice),
    /// Count dropouts from zero again.
    ResetDropouts,
    /// Record what the audio device plays to this WAV file, a full path,
    /// with its timeline beside it: see [`debug`].
    RecordOutput(PathBuf),
    /// Stop recording what the audio device plays.
    StopRecordingOutput,
    /// Clear the highest peak of every channel's meter.
    ClearPeaks,
    /// Give `command` these keys, everywhere or in a panel, in place of
    /// the ones it has. A key another command has in the same place is
    /// taken from it.
    SetKeys {
        command: CommandId,
        keys: Vec<(Scope, KeyChord)>,
    },
    /// Give `command` its default keys again.
    ResetKeys(CommandId),
}

/// How [`file::EXPORT_AUDIO`] renders, where it doesn't as the export
/// window last did: what's `None` is as the window had it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportOptions {
    /// `song`, `loop` or `selection`. Without it, what the window starts
    /// out with: the time selection, or else the song.
    pub range: Option<String>,
    /// 16, 24, or 32 for floats.
    pub bits: Option<u8>,
    pub sample_rate: Option<u32>,
    pub dither: Option<bool>,
    pub normalize: Option<bool>,
    pub tail: Option<bool>,
    /// Write the mix. Without it, the mix is written.
    pub mix: Option<bool>,
    /// Write each of `tracks` on its own too.
    pub stems: Option<bool>,
    /// The tracks to write on their own: all of them without it.
    pub tracks: Option<Vec<TrackId>>,
}

/// The audio device to play through, and how, as
/// [`options::SET_AUDIO_DEVICE`] asks for it. What's `None` stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevice {
    /// `system` or `asio`.
    pub driver_type: Option<String>,
    /// The device's id or name. Without one, a new driver type plays
    /// through its first device, or the system's default output.
    pub device: Option<String>,
    /// Frames per buffer.
    pub buffer: Option<u32>,
    pub sample_rate: Option<u32>,
    /// Whether to take the device for Musicable alone, WASAPI's exclusive
    /// mode.
    pub exclusive: Option<bool>,
}

impl Request {
    /// Whether an agent can ask for it. Agents can't see a window, or
    /// answer a question in one, so the requests that only show a window
    /// or ask the user for something are the user's, and so is quitting.
    pub fn for_agents(&self) -> bool {
        !matches!(
            self,
            Request::Quit
                | Request::CommandPalette
                | Request::Settings
                | Request::AudioSettings
                | Request::KeyboardShortcuts
                | Request::Appearance
                | Request::Profiles
                | Request::Agents
                | Request::Rename(_)
                | Request::LoadSample(_)
                | Request::FocusBrowserSearch
                | Request::DeleteWorkspace
        )
    }
}

/// Something in the project with a name the user can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Track(TrackId),
    Clip(ClipId),
    Locator(LocatorId),
    Take(TakeId),
    Modulator(ModulatorId),
    /// A Macro's knob or button, by its number.
    Control(ModulatorId, u32),
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

    /// Runs `commands` in order, each with its arguments, as one step in
    /// the undo history called `name`, and returns their results. Each acts
    /// on what those before it left, as if run on its own.
    ///
    /// If one fails, the project and the editor's state go back to how they
    /// were before the first, and so does the undo history, and nothing
    /// they asked the app for is asked for. What they did to the transport
    /// or the library stays. Commands that run [alone](Command::alone) can't
    /// be among them.
    pub fn run_batch(
        &self,
        name: &str,
        commands: &[(CommandId, Args)],
        context: &mut Context<'_>,
    ) -> Result<Vec<Value>, BatchError> {
        for (index, (id, _)) in commands.iter().enumerate() {
            let failed = |error| BatchError { index, error };
            let command = self
                .get(id)
                .ok_or_else(|| failed(CommandError::Unknown(id.clone())))?;
            if command.alone {
                let problem = format!("{id} runs on its own, not among other commands");
                return Err(failed(CommandError::Invalid(problem)));
            }
        }
        let requests = context.requests.len();
        context.document.begin_batch(name);
        let mut values = Vec::with_capacity(commands.len());
        for (index, (id, args)) in commands.iter().enumerate() {
            match self.run_with(id, args, context) {
                Ok(value) => values.push(value),
                Err(error) => {
                    context.document.cancel_batch();
                    context.requests.truncate(requests);
                    return Err(BatchError { index, error });
                }
            }
        }
        context.document.end_batch();
        Ok(values)
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
    history::register(registry);
    transport::register(registry);
    track::register(registry);
    arrange::register(registry);
    locator::register(registry);
    device::register(registry);
    modulation::register(registry);
    browser::register(registry);
    mixer::register(registry);
    take::register(registry);
    notes::register(registry);
    controllers::register(registry);
    warp::register(registry);
    warp_mode::register(registry);
    rewarp::register(registry);
    transient::register(registry);
    automation::register(registry);
    wire::register(registry);
    view::register(registry);
    workspace::register(registry);
    options::register(registry);
    debug::register(registry);
}

/// Registers commands that each ask the app for something. Agents can run
/// those whose requests are [for agents](Request::for_agents).
fn register_requests(registry: &mut Registry, commands: &[(CommandId, &str, Request)]) {
    for (id, title, request) in commands {
        let mut command = Command::new(id.clone(), *title);
        command.agents = request.for_agents();
        let request = request.clone();
        registry.add(command, move |ctx, _| {
            ctx.requests.push(request.clone());
            Ok(Value::None)
        });
    }
}

/// Why [`Registry::run_batch`] stopped: the command at `index` failed.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchError {
    /// The failed command's place among those run, from 0.
    pub index: usize,
    pub error: CommandError,
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "command {} failed: {}", self.index + 1, self.error)
    }
}

impl std::error::Error for BatchError {}

/// No command has this id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub CommandId);

impl fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "there's no command called {}", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

//! Lets agents, such as Claude Code, control Musicable over MCP, the Model
//! Context Protocol: list and run its commands, and read the project, while
//! the user watches each change happen in the window and can undo it.
//!
//! While the user lets agents in, the app runs a [`Host`]. It listens on a
//! local pipe that only the user's account can open ([`pipe`]), and serves
//! each agent that connects the [tools](tools), on a thread of its own with
//! Tokio. A tool checks a call's arguments there, then hands the call to the
//! window and wakes it. The window answers on its own thread, with
//! [`answer`]: reads take their snapshot there, so an agent never sees a
//! change half made, and commands run there as a menu's would, each a step
//! in the undo history.
//!
//! Agents' clients start `musicable mcp`, the [`bridge`], which speaks MCP
//! over its standard input and output and passes each tool call on to the
//! Musicable that's running, through the pipe. While none is, or it doesn't
//! let agents in, the tools say so. The bridge runs [local tools](local)
//! itself, the app's audio commands and the log, which need no window.
//!
//! `docs/agents.md` describes the tools, and what they take and give back.

use std::path::{Path, PathBuf};

use musicable_commands::{
    Args, BatchError, CommandError, CommandId, Editor, Keymap, Registry, Steps, Value, edit,
    history, json,
};
use musicable_model::clip::ClipId;
use musicable_model::modulation::ModulatorId;
use musicable_model::param::{ParamAddress, ParamSpec};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Chain, DeviceId, Project, TrackId};
use serde_json::{Map, Value as Json};
use tokio::sync::oneshot;

pub mod bridge;
mod host;
pub mod local;
pub mod pipe;
mod reads;
#[cfg(test)]
mod tests;
pub mod tools;
mod waiting;

pub use host::Host;
pub use reads::catalogue;

/// A tool call for the window to answer on its own thread, with [`answer`].
#[derive(Debug)]
pub struct Call {
    asked: Asked,
    reply: Reply,
}

/// What a call asks the window for.
#[derive(Debug, Clone, PartialEq)]
enum Asked {
    /// The commands agents can run, or those whose ids or titles hold this
    /// text.
    Commands(Option<String>),
    /// One command, with its arguments' schema.
    Command(CommandId),
    Project,
    Clip(ClipId),
    /// A device, with only the parameters whose names hold the text, if
    /// there is one.
    Device(DeviceId, Option<String>),
    Modulator(ModulatorId),
    /// A parameter's automation.
    Automation(ParamAddress),
    Editor,
    History,
    /// What the project costs the CPU.
    Performance,
    /// What plays, and what's under way.
    Status,
    /// The settings, and what plays.
    Settings,
    /// The audio devices there are.
    AudioDevices,
    /// The plugins installed, or those whose names, vendors or ids hold
    /// the text.
    Plugins(Option<String>),
    /// How the panels and windows are laid out.
    Layout,
    /// The meters.
    Levels,
    /// The commands run this session: those after the entry numbered
    /// `since`, if it says, at most the `last`.
    CommandLog {
        since: Option<u64>,
        last: usize,
    },
    /// The windows.
    Windows,
    /// A picture of a window or a panel.
    Screenshot(Shot),
    /// The messages the window shows the user, and whether to clear them
    /// once read.
    Messages {
        clear: bool,
    },
    Run(Run),
}

/// Commands an agent asked to run, their arguments checked against their
/// parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// One command with its arguments, as a menu or a key runs it.
    One(CommandId, Args),
    /// Several commands in order, as one step in the undo history with this
    /// name: see [`Registry::run_batch`].
    Batch(String, Vec<(CommandId, Args)>),
}

impl Run {
    /// Whether it steps through the undo history, as Undo, Redo and going
    /// to a step do, rather than making a step in it.
    pub fn steps_through_history(&self) -> bool {
        let stepping = [edit::UNDO, edit::REDO, history::GO_TO];
        matches!(self, Run::One(id, _) if stepping.contains(id))
    }
}

/// A call's answer: what it gives back, as JSON, or why it failed, for the
/// agent to read.
type Answer = Result<Json, String>;

/// Where the answer to an agent's call goes. The window can keep it to
/// answer later, such as once the user has said whether to save their
/// changes, or once an export has finished. Dropping it unanswered tells the
/// agent that Musicable stopped before it answered.
#[derive(Debug)]
pub struct Reply {
    /// The agent, as its client names itself.
    agent: String,
    sender: oneshot::Sender<Answer>,
}

impl Reply {
    fn new(agent: String) -> (Self, oneshot::Receiver<Answer>) {
        let (sender, receiver) = oneshot::channel();
        (Self { agent, sender }, receiver)
    }

    /// The agent, as its client names itself, such as `claude-code`.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Whether the agent has stopped waiting for the answer, as when it
    /// cancels the call.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Answers with what a command gave back, or why it failed. `undo`
    /// names the step it made in the undo history, if it made one.
    pub fn ran(self, result: Result<Value, CommandError>, undo: Option<&str>) {
        let answer = match result {
            Ok(value) => Ok(with_undo(&[("result", json::to_json(&value))], undo)),
            Err(CommandError::NotNow(id)) => Err(format!(
                "{id} doesn't apply now. Without arguments it acts on the selection, which \
                 get_editor gives."
            )),
            Err(error) => Err(sentence(&error.to_string())),
        };
        self.send(answer);
    }

    /// Answers with what each of a batch's commands gave back, or which one
    /// failed and why. `undo` names the step they made, if they made one.
    pub fn ran_batch(self, result: Result<Vec<Value>, BatchError>, undo: Option<&str>) {
        let answer = match result {
            Ok(values) => {
                let results = values.iter().map(json::to_json).collect();
                Ok(with_undo(&[("results", Json::Array(results))], undo))
            }
            Err(error) => Err(format!("{} Nothing changed.", sentence(&error.to_string()))),
        };
        self.send(answer);
    }

    /// Answers with `json`.
    pub fn done(self, json: Json) {
        self.send(Ok(json));
    }

    /// Answers that the call failed, saying why in a sentence, such as
    /// "Couldn't save the project: the disk is full."
    pub fn failed(self, why: impl Into<String>) {
        self.send(Err(why.into()));
    }

    fn send(self, answer: Answer) {
        // The agent may have stopped waiting.
        let _ = self.sender.send(answer);
    }
}

/// An object of `fields`, and `undo` if there is one.
fn with_undo(fields: &[(&str, Json)], undo: Option<&str>) -> Json {
    let mut object: Map<String, Json> = (fields.iter())
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    if let Some(undo) = undo {
        object.insert("undo".into(), undo.into());
    }
    Json::Object(object)
}

/// `text` as a sentence: with a capital, unless it starts with a command's
/// id, and a full stop.
fn sentence(text: &str) -> String {
    let id = text
        .split_whitespace()
        .next()
        .is_some_and(|word| word.contains('.'));
    let mut chars = text.chars();
    let mut sentence: String = match chars.next() {
        Some(first) if !id => first.to_uppercase().chain(chars).collect(),
        Some(_) => text.to_owned(),
        None => return String::new(),
    };
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// What the project costs the CPU, as the Performance panel and the
/// transport bar show it: see [`Window::performance`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Performance {
    /// Whether an audio device plays. Without one, nothing renders, and
    /// there are no figures.
    pub playing: bool,
    /// What plays, as the status bar says it, such as "48 kHz · 128
    /// samples · 5.3 ms", or why nothing does.
    pub audio: String,
    /// The share of the time the audio thread was busy lately, as the
    /// transport bar's meter shows it: over 1.0, it can't keep up.
    pub load: Option<f32>,
    /// Dropouts since the device started or the count was reset.
    pub dropouts: u64,
    /// How far ahead of the device the engine rendered lately, while
    /// Render Ahead is on and a device plays.
    pub render_ahead: Option<RenderingAhead>,
    /// Each device's figures, and those of each channel's clips,
    /// modulators and mixer, and the metronome's, busiest first.
    pub rows: Vec<CpuRow>,
    /// The whole project's share of a core: 1.5 is a core and a half.
    pub total: f32,
}

/// How far ahead of the audio device the engine rendered lately, as the
/// gauge under the transport bar's CPU meter shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderingAhead {
    /// Render Ahead, as the Audio settings name it: "short", "normal" or
    /// "long".
    pub setting: &'static str,
    /// Whether the project renders ahead now, all but what plays live, such
    /// as a track monitoring its input: nothing does for a moment as a track
    /// arms.
    pub ahead: bool,
    /// How far ahead the engine was on average lately, in milliseconds,
    /// and how far it aimed to be: both none while nothing rendered ahead.
    pub lead_ms: f32,
    pub target_ms: f32,
    /// The share of a core of what rendered ahead, of the project's total.
    pub load: f32,
    /// The blocks the audio thread had to render itself since the device
    /// started, as the lead ran out.
    pub short_blocks: u64,
}

/// What something in the project costs the CPU, over the last second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuRow {
    pub part: CpuPart,
    /// The share of one core it takes to keep up. Shares add up.
    pub average: f32,
    /// Its longest block, as a share of the audio device's buffer. It
    /// matters only while it renders live.
    pub longest: f32,
    /// Whether it rendered ahead of the device, rather than live, as the
    /// device asked for each buffer.
    pub ahead: bool,
}

/// Something in the project that takes CPU time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuPart {
    Device(DeviceId),
    /// What a track plays: its clips, and the input it monitors.
    Clips(TrackId),
    /// A channel's modulators.
    Modulators(Chain),
    /// A channel's strip, but for its devices: its fader and pan, its
    /// sends, its bus and what lines its paths up.
    Mixer(Chain),
    Metronome,
}

/// What plays, and what's under way that lands later, as the transport
/// bar and the status bar show it: see [`Window::status`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Whether the transport plays, from the moment Play is pressed.
    pub playing: bool,
    /// Whether playing records the armed tracks.
    pub recording: bool,
    /// Where the playhead is in what's heard, in beats and in seconds from
    /// the start of the song.
    pub playhead: Beats,
    pub seconds: f64,
    /// The takes being recorded: each track's, and what it covers so far.
    pub takes: Vec<(TrackId, BeatRange)>,
    /// The question the window waits for the user to answer, if it asks
    /// one, such as whether to save their changes.
    pub question: Option<String>,
    /// The audio files being analysed for warping, by their names in the
    /// project.
    pub analysing: Vec<String>,
    /// The segments of clips still to render, such as Spectral's.
    pub rendering: usize,
    /// Whether a graph the window asked the engine for still compiles.
    pub compiling: bool,
    /// The export under way, if one is.
    pub export: Option<Exporting>,
    /// Whether the plugins are being scanned.
    pub scanning_plugins: bool,
    /// The library's folders being scanned, with the files found in each
    /// so far.
    pub scanning_folders: Vec<(String, usize)>,
    /// The library's folders whose files are being read, with the files
    /// left in each.
    pub reading_folders: Vec<(String, usize)>,
    /// Whether the library looks for presets.
    pub finding_presets: bool,
    /// Goes up with every change to the project, undone and redone ones
    /// too, and when another project opens.
    pub revision: u64,
}

/// What to take a picture of, and where to save it: see
/// [`Window::screenshot`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shot {
    /// The window: the main window with `None`, or else a window of its
    /// own, counted from 0.
    pub window: Option<usize>,
    /// The panel, by its id, in place of a whole window.
    pub panel: Option<String>,
    /// Where to save it as a PNG file too, if anywhere.
    pub file: Option<PathBuf>,
}

/// How a plugin device's plugin is doing: see [`Window::plugin_state`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginState {
    /// Its plugin loaded, and plays.
    pub loaded: bool,
    /// Why it plays nothing, if it doesn't, such as the plugin missing.
    pub problem: Option<String>,
    /// It has a window of its own, and whether that's open.
    pub has_window: bool,
    pub window_open: bool,
}

/// How an export is going, as its window's progress bar shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exporting {
    /// What it does: "preparing", "rendering", "normalising" or "finished".
    pub stage: &'static str,
    /// The pass it's on, from 1, of how many.
    pub pass: usize,
    pub passes: usize,
    /// How much of the whole export is done, from 0 to 1.
    pub done: f32,
    /// While it renders, how many times faster than real time.
    pub speed: Option<f64>,
}

/// What the tools read and run, on the window's thread.
pub trait Window {
    /// Every command, as the window runs them.
    fn commands(&self) -> &Registry;
    /// The keys in use.
    fn keymap(&self) -> &Keymap;
    fn project(&self) -> &Project;
    fn editor(&self) -> &Editor;
    fn history(&self) -> Steps<'_>;
    /// The project's file, once it has one.
    fn file(&self) -> Option<&Path>;
    /// Whether the project changed since it was opened or saved.
    fn is_modified(&self) -> bool;

    /// The parameters of the plugin device `device` that automation can
    /// move, by the plugin's id for each, as its plugin describes them, or
    /// `None` while it isn't loaded.
    fn plugin_params(&self, _device: DeviceId) -> Option<Vec<(u32, ParamSpec)>> {
        None
    }

    /// What the project costs the CPU, as last worked out.
    fn performance(&self) -> Performance {
        Performance::default()
    }

    /// What plays, and what's under way.
    fn status(&self) -> Status {
        Status::default()
    }

    /// How the plugin device `device`'s plugin is doing, or `None` if the
    /// window doesn't know.
    fn plugin_state(&self, _device: DeviceId) -> Option<PluginState> {
        None
    }

    /// The plugins installed, as the browser lists them, or those whose
    /// names, vendors or ids hold `filter`, as JSON.
    fn plugins(&self, _filter: Option<&str>) -> Json {
        Json::Object(Map::new())
    }

    /// How the panels and the windows of their own are laid out, as JSON.
    fn layout(&self) -> Json {
        Json::Object(Map::new())
    }

    /// What the meters read, as JSON.
    fn levels(&self) -> Json {
        Json::Object(Map::new())
    }

    /// The commands run this session, after the entry numbered `since`,
    /// if it says, at most the `last`, as JSON.
    fn command_log(&self, _since: Option<u64>, _last: usize) -> Json {
        serde_json::json!({ "entries": [], "count": 0, "file": null })
    }

    /// The windows, as JSON.
    fn windows(&self) -> Json {
        serde_json::json!({ "windows": [] })
    }

    /// Takes a picture of `shot`, and answers `reply` once it has: with
    /// what it took, and the picture as a PNG file in base64 as `png`,
    /// which the tool hands the agent as an image.
    fn screenshot(&mut self, _shot: Shot, reply: Reply) {
        reply.failed("This window can't take pictures.");
    }

    /// The settings, as the settings file has them, with the profile in
    /// use's, and what plays, as JSON.
    fn settings(&self) -> Json {
        Json::Object(Map::new())
    }

    /// The audio devices there are to play through and record from, listed
    /// again, as JSON.
    fn audio_devices(&mut self) -> Json {
        Json::Object(Map::new())
    }

    /// The messages the window shows the user, oldest first: errors and
    /// notices, such as a file that's missing.
    fn messages(&self) -> Vec<String> {
        Vec::new()
    }

    /// Clears the messages, as Dismiss does.
    fn clear_messages(&mut self) {}

    /// Runs `run`, as the agent `reply` names asked, and answers with
    /// [`Reply::ran`] or [`Reply::ran_batch`], at once or once it knows.
    fn run(&mut self, run: Run, reply: Reply);
}

/// Answers `call` on the window's thread: a read at once, from `window`,
/// and commands through [`Window::run`]. A call whose agent has stopped
/// waiting is dropped without running anything.
pub fn answer(call: Call, window: &mut impl Window) {
    let Call { asked, reply } = call;
    if reply.is_closed() {
        return;
    }
    let answer = match asked {
        Asked::Run(run) => return window.run(run, reply),
        Asked::Screenshot(shot) => return window.screenshot(shot, reply),
        Asked::Windows => Ok(window.windows()),
        Asked::Commands(filter) => Ok(reads::commands(window, filter.as_deref())),
        Asked::Command(id) => reads::command(window, &id),
        Asked::Project => Ok(reads::project(window)),
        Asked::Clip(clip) => reads::clip(window, clip),
        Asked::Device(device, filter) => reads::device(window, device, filter.as_deref()),
        Asked::Modulator(modulator) => reads::modulator(window, modulator),
        Asked::Automation(param) => reads::automation(window, param),
        Asked::Editor => Ok(reads::editor(window)),
        Asked::History => Ok(reads::history(window)),
        Asked::Performance => Ok(reads::performance(window)),
        Asked::Status => Ok(reads::status(window)),
        Asked::Settings => Ok(window.settings()),
        Asked::AudioDevices => Ok(window.audio_devices()),
        Asked::Plugins(filter) => Ok(window.plugins(filter.as_deref())),
        Asked::Layout => Ok(window.layout()),
        Asked::Levels => Ok(window.levels()),
        Asked::CommandLog { since, last } => Ok(window.command_log(since, last)),
        Asked::Messages { clear } => {
            let messages = window.messages();
            if clear {
                window.clear_messages();
            }
            Ok(serde_json::json!({ "messages": messages }))
        }
    };
    reply.send(answer);
}

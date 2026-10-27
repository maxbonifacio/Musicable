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
//! itself, the app's audio commands, which need no window.
//!
//! `docs/agents.md` describes the tools, and what they take and give back.

use std::path::Path;

use musicable_commands::{
    Args, BatchError, CommandError, CommandId, Editor, Keymap, Registry, Steps, Value, edit,
    history, json,
};
use musicable_model::clip::ClipId;
use musicable_model::modulation::ModulatorId;
use musicable_model::param::{ParamAddress, ParamSpec};
use musicable_model::{DeviceId, Project};
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
    Device(DeviceId),
    Modulator(ModulatorId),
    /// A parameter's automation.
    Automation(ParamAddress),
    Editor,
    History,
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
        Asked::Commands(filter) => Ok(reads::commands(window, filter.as_deref())),
        Asked::Command(id) => reads::command(window, &id),
        Asked::Project => Ok(reads::project(window)),
        Asked::Clip(clip) => reads::clip(window, clip),
        Asked::Device(device) => reads::device(window, device),
        Asked::Modulator(modulator) => reads::modulator(window, modulator),
        Asked::Automation(param) => reads::automation(window, param),
        Asked::Editor => Ok(reads::editor(window)),
        Asked::History => Ok(reads::history(window)),
    };
    reply.send(answer);
}

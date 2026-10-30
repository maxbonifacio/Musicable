//! The tools agents call: a few general ones that cover every command, so
//! a new command reaches agents with no change here, and reads of the
//! project, a clip, a device, a modulator, a lane of automation, the
//! editor's state, the undo history and what the project costs the CPU.
//!
//! A call's arguments are checked on the server's thread, against the
//! commands' parameters for the commands it runs. The window then answers
//! it on its own thread: see [`answer`](crate::answer).

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use musicable_commands::{Args, Command, CommandError, CommandId, Registry, json};
use musicable_model::DeviceId;
use musicable_model::clip::ClipId;
use musicable_model::modulation::ModulatorId;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value as Json, json};

use crate::{Asked, Call, Reply, Run, reads};

pub const LIST_COMMANDS: &str = "list_commands";
pub const DESCRIBE_COMMAND: &str = "describe_command";
pub const RUN_COMMAND: &str = "run_command";
pub const RUN_COMMANDS: &str = "run_commands";
pub const GET_PROJECT: &str = "get_project";
pub const GET_CLIP: &str = "get_clip";
pub const GET_DEVICE: &str = "get_device";
pub const GET_MODULATOR: &str = "get_modulator";
pub const GET_AUTOMATION: &str = "get_automation";
pub const GET_EDITOR: &str = "get_editor";
pub const GET_HISTORY: &str = "get_history";
pub const GET_PERFORMANCE: &str = "get_performance";

/// What the server tells agents as they connect: what Musicable is, and how
/// its tools go together.
const INSTRUCTIONS: &str = "\
Musicable is a digital audio workstation. These tools run the same commands \
as its menus and keys, in the window the user is watching, and each change \
is a named step in its undo history, which the user can undo.

Start with get_project, which has the tracks, clips, devices and locators \
with their ids. list_commands finds commands, with a filter such as \"notes\" \
or \"tempo\", and describe_command gives one's arguments. Positions and \
lengths are numbers of beats (quarter notes) from the start of the song, \
notes' positions are in their clip's own time, and gains are linear, 1 being \
unity. Without arguments, a command acts on the selection, as a key does, \
and fails if it doesn't apply now.

Run changes that belong together with run_commands, under a name such as \
\"Humanize\", so they're one step to undo.

The file commands take files by their full paths, with no dialogs. New and \
Open ask the user in the window whether to save unsaved changes first, and \
answer once they have.";

/// Who the server is, and that it has tools, as it says to each agent
/// that connects.
pub fn server_info() -> ServerConfig {
    server_info_with("")
}

/// Who the server is, as [`server_info`] says, with `more` instructions
/// after its own, if there are any.
pub(crate) fn server_info_with(more: &str) -> ServerConfig {
    let capabilities = ServerCapabilities::builder().enable_tools().build();
    let musicable =
        Implementation::new("musicable", env!("CARGO_PKG_VERSION")).with_title("Musicable");
    let instructions = match more.trim() {
        "" => INSTRUCTIONS.to_owned(),
        more => format!("{INSTRUCTIONS}\n\n{more}"),
    };
    InitializeResult::new(capabilities)
        .with_server_info(musicable)
        .with_instructions(instructions)
}

/// The tools, as `tools/list` gives them.
pub fn list() -> Vec<Tool> {
    let read = |name: &'static str, title: &str, about: &'static str, schema: Json| {
        tool(name, about, schema)
            .with_title(title)
            .annotate(ToolAnnotations::new().read_only(true).open_world(false))
    };
    let run = |name: &'static str, title: &str, about: &'static str, schema: Json| {
        tool(name, about, schema)
            .with_title(title)
            .annotate(ToolAnnotations::new().read_only(false).open_world(false))
    };
    let args = json!({
        "type": "object",
        "description": "The command's arguments by name, as describe_command's schema has them. \
                        Leave it out to run the command as a key does.",
    });
    vec![
        read(
            LIST_COMMANDS,
            "List Commands",
            "Lists the commands agents can run: each one's id, title and arguments with their \
             types, whether it applies now without arguments, if it needs none, and its keys. \
             A filter keeps those whose id or title holds it, such as \"notes\" or \"tempo\".",
            object(
                json!({
                    "filter": {
                        "type": "string",
                        "description": "Text that the id or the title holds, in any case.",
                    }
                }),
                &[],
            ),
        ),
        read(
            DESCRIBE_COMMAND,
            "Describe Command",
            "Describes one command: its arguments as a JSON Schema, saying what each is for, \
             whether it applies now without arguments, its keys, and whether it runs alone, \
             never with run_commands.",
            object(json!({ "id": id_schema() }), &["id"]),
        ),
        run(
            RUN_COMMAND,
            "Run Command",
            "Runs a command, as a menu or a key would, in the window the user is watching, and \
             gives back its result, such as the id of a new track, and the name of the step it \
             made in the undo history, which the user can undo. Without arguments, a command \
             acts on the selection, and fails if it doesn't apply now.",
            object(json!({ "id": id_schema(), "args": args }), &["id"]),
        ),
        run(
            RUN_COMMANDS,
            "Run Commands",
            "Runs several commands in order, as one step in the undo history under a name, \
             such as \"Humanize\", and gives back each one's result. Each acts on what those \
             before it left. If one fails, none of them changes anything, and the error says \
             which one failed and why.",
            object(
                json!({
                    "name": {
                        "type": "string",
                        "description": "What the History panel and the Edit menu call the step.",
                    },
                    "commands": {
                        "type": "array",
                        "minItems": 1,
                        "items": object(json!({ "id": id_schema(), "args": args }), &["id"]),
                    },
                }),
                &["name", "commands"],
            ),
        ),
        read(
            GET_PROJECT,
            "Get Project",
            "Reads the project: its file, the tempo map and time signatures, the loop, the \
             tracks with their mixer settings, devices and clips, and their modulators and \
             routings if they have any, the master, the wires, each taking a tap's signal into a \
             device's side input at a gain, or a MIDI track's notes into an instrument's or a \
             MIDI effect's note input, on a MIDI channel from 0 to 15 if it has one, the \
             locators, and which parameters are automated, each with its id. A wire's tap and \
             port are named as wire.connect takes them, and a track's \
             input from another track's tap, or Resampling, as mixer.set_input takes it. A \
             track whose output is \"sends_only\" feeds nothing but its sends. A device \
             that's bypassed says \"bypassed\": true, and one that's deactivated \"active\": \
             false. A clip's notes, a device's or a modulator's parameters and a lane's points \
             come from get_clip, get_device, get_modulator and get_automation.",
            object(json!({}), &[]),
        ),
        read(
            GET_CLIP,
            "Get Clip",
            "Reads one clip: where it is, and its notes, with their expression, and its \
             controllers' curves, in the clip's own time, or its audio file, with its warp \
             (its markers, mode, transpose, detune, and Slice's, Spectral's and Granular's \
             settings) and its curves, by parameter, if it has them.",
            object(json!({ "clip": id_number("The clip's id.") }), &["clip"]),
        ),
        read(
            GET_DEVICE,
            "Get Device",
            "Reads one device: what it is, where it is, whether it's bypassed or deactivated, \
             each of its parameters' number, name, range, unit and value, for \
             device.set_param and automation, which name a device's parameter as \"device 4 \
             param 2\" and its Bypass as \"device 4 bypass\", and its ports, with the wires \
             into each: a Compressor's or a Gate's Sidechain, or a plugin's side inputs, by the \
             plugin's ids and names for them, and an instrument's or a MIDI effect's note \
             input, last, as wire.connect takes them.",
            object(
                json!({ "device": id_number("The device's id.") }),
                &["device"],
            ),
        ),
        read(
            GET_MODULATOR,
            "Get Modulator",
            "Reads one modulator, a Macro, an LFO or an Envelope: where it is, each of its \
             parameters' number, name, range, unit and value, for modulation.set_param and \
             automation, which name a modulator's parameter as \"modulator 6 param 2\", and its \
             outputs, for modulation.add_routing. A Macro's parameters and outputs are its \
             knobs and buttons, by number.",
            object(
                json!({ "modulator": id_number("The modulator's id.") }),
                &["modulator"],
            ),
        ),
        read(
            GET_AUTOMATION,
            "Get Automation",
            "Reads one parameter's automation: its points, each a position in beats and a \
             value in the parameter's units, and whether someone overrode it by hand.",
            object(
                json!({
                    "param": {
                        "type": "string",
                        "description": "The parameter, as get_project names it: \"track 3 volume\", \
                                        \"master pan\", \"send 2\", \"device 4 param 17\", \
                                        \"wire 1\" or \"modulator 6 param 2\".",
                    }
                }),
                &["param"],
            ),
        ),
        read(
            GET_EDITOR,
            "Get Editor",
            "Reads the editor's state: what's selected, the selected track, which panel has \
             the focus, where the start marker is, the grid, what's in sight, the clip the \
             piano roll shows and its selected notes, and the devices selected in the devices \
             panel, which shows the selected track's, in the order they play.",
            object(json!({}), &[]),
        ),
        read(
            GET_HISTORY,
            "Get History",
            "Reads the undo history: each step's name, oldest first, and how many are done. \
             Those after them were undone, and redo makes them again.",
            object(json!({}), &[]),
        ),
        read(
            GET_PERFORMANCE,
            "Get Performance",
            "Reads what the project costs the CPU while an audio device plays, as the \
             Performance panel lists it, busiest first: each device's share of one core over \
             the last second, whether it renders ahead of the device or live, and while live \
             its longest block as a share of the device's buffer, and the same for each \
             channel's clips, modulators and mixer, and the metronome. Shares add up, to the \
             project's total: 1.5 is a core and a half. A device rendering live whose longest \
             block nears 1 can cause dropouts by itself, whatever its share. It also gives the \
             transport bar's figures: what plays, the audio thread's load, over 1 when it can't \
             keep up, and the dropouts, and while Render Ahead is on, how far ahead of the \
             device the engine rendered lately and aimed to, the share of a core of what \
             rendered ahead, and the blocks the audio thread had to render itself as the lead \
             ran out, which come before dropouts.",
            object(json!({}), &[]),
        ),
    ]
}

fn tool(name: &'static str, about: &'static str, schema: Json) -> Tool {
    let Json::Object(schema) = schema else {
        unreachable!("each tool's schema is an object");
    };
    Tool::new(name, about, Arc::new(schema))
}

/// The schema of an object with `properties`, of which `required` must be
/// there.
fn object(properties: Json, required: &[&str]) -> Json {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = required.into();
    }
    schema
}

fn id_schema() -> Json {
    json!({
        "type": "string",
        "description": "The command's id, such as \"notes.add\".",
    })
}

fn id_number(about: &str) -> Json {
    json!({ "type": "integer", "minimum": 0, "maximum": u32::MAX, "description": about })
}

/// The commands agents can run, as the server checks calls' arguments
/// against them.
pub(crate) type Commands = BTreeMap<CommandId, Command>;

/// The commands in `registry`, to check calls against.
pub(crate) fn commands(registry: &Registry) -> Commands {
    (registry.iter())
        .map(|command| (command.id.clone(), command.clone()))
        .collect()
}

/// Why a call can't go to the window.
#[derive(Debug, PartialEq)]
enum Refused {
    /// There's no tool with that name.
    NoTool(String),
    /// The arguments are wrong, as this sentence says.
    Arguments(String),
}

/// What the call to the tool `name` with `arguments` asks the window, its
/// arguments checked against `commands`.
fn asked(name: &str, arguments: &JsonObject, commands: &Commands) -> Result<Asked, Refused> {
    let allowed: &[&str] = match name {
        LIST_COMMANDS => &["filter"],
        DESCRIBE_COMMAND => &["id"],
        RUN_COMMAND => &["id", "args"],
        RUN_COMMANDS => &["name", "commands"],
        GET_CLIP => &["clip"],
        GET_DEVICE => &["device"],
        GET_MODULATOR => &["modulator"],
        GET_AUTOMATION => &["param"],
        GET_PROJECT | GET_EDITOR | GET_HISTORY | GET_PERFORMANCE => &[],
        _ => return Err(Refused::NoTool(name.to_owned())),
    };
    if let Some(unknown) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        let problem = format!("{name} takes no argument called {unknown}.");
        return Err(Refused::Arguments(problem));
    }
    let asked = match name {
        LIST_COMMANDS => Asked::Commands(optional_text(arguments, "filter")?),
        DESCRIBE_COMMAND => Asked::Command(CommandId::from(text(arguments, "id")?)),
        RUN_COMMAND => {
            let (id, args) = command(arguments, commands)?;
            Asked::Run(Run::One(id, args))
        }
        RUN_COMMANDS => {
            let name = text(arguments, "name")?;
            if name.trim().is_empty() {
                return Err(Refused::Arguments("The step needs a name.".into()));
            }
            let Some(Json::Array(list)) = arguments.get("commands") else {
                let problem = "commands must be a list of commands, each with its id and args.";
                return Err(Refused::Arguments(problem.into()));
            };
            if list.is_empty() {
                return Err(Refused::Arguments("There are no commands to run.".into()));
            }
            let mut batch = Vec::with_capacity(list.len());
            for (index, entry) in list.iter().enumerate() {
                let Json::Object(entry) = entry else {
                    let problem = format!("Command {} must be an object.", index + 1);
                    return Err(Refused::Arguments(problem));
                };
                let call = command(entry, commands).map_err(|refused| match refused {
                    Refused::Arguments(why) => {
                        Refused::Arguments(format!("Command {}: {why}", index + 1))
                    }
                    other => other,
                })?;
                batch.push(call);
            }
            Asked::Run(Run::Batch(name.to_owned(), batch))
        }
        GET_PROJECT => Asked::Project,
        GET_CLIP => Asked::Clip(ClipId(id(arguments, "clip")?)),
        GET_DEVICE => Asked::Device(DeviceId(id(arguments, "device")?)),
        GET_MODULATOR => Asked::Modulator(ModulatorId(id(arguments, "modulator")?)),
        GET_AUTOMATION => {
            let param = text(arguments, "param")?;
            let param = reads::parse_param(param).ok_or_else(|| {
                Refused::Arguments(format!(
                    "There's no parameter called \"{param}\": they're named as \"track 3 \
                     volume\", \"master pan\", \"send 2\", \"device 4 param 17\" or \
                     \"modulator 6 param 2\"."
                ))
            })?;
            Asked::Automation(param)
        }
        GET_EDITOR => Asked::Editor,
        GET_HISTORY => Asked::History,
        GET_PERFORMANCE => Asked::Performance,
        _ => unreachable!("every tool is matched above"),
    };
    Ok(asked)
}

/// The command `entry` names with its id, and its arguments from its args.
fn command(entry: &JsonObject, commands: &Commands) -> Result<(CommandId, Args), Refused> {
    if let Some(unknown) = entry
        .keys()
        .find(|key| !["id", "args"].contains(&key.as_str()))
    {
        let problem = format!("A command has its id and args, not {unknown}.");
        return Err(Refused::Arguments(problem));
    }
    let id = CommandId::from(text(entry, "id")?);
    let empty = Map::new();
    let args = match entry.get("args") {
        None | Some(Json::Null) => &empty,
        Some(Json::Object(args)) => args,
        Some(_) => {
            let problem = "args must be an object, of the command's arguments by name.";
            return Err(Refused::Arguments(problem.into()));
        }
    };
    let Some(command) = commands.get(&id) else {
        let problem = format!("There's no command called {id}: list_commands lists them.");
        return Err(Refused::Arguments(problem));
    };
    let args = json::agent_args(command, args).map_err(|error| {
        let why = match error {
            CommandError::NotForAgents(_) => format!("{error}."),
            error => format!("{error}: describe_command {id} gives its arguments."),
        };
        Refused::Arguments(crate::sentence(&why))
    })?;
    Ok((id, args))
}

fn text<'a>(arguments: &'a JsonObject, name: &str) -> Result<&'a str, Refused> {
    match arguments.get(name) {
        Some(Json::String(text)) => Ok(text),
        Some(_) => Err(Refused::Arguments(format!("{name} must be text."))),
        None => Err(Refused::Arguments(format!("{name} is missing."))),
    }
}

fn optional_text(arguments: &JsonObject, name: &str) -> Result<Option<String>, Refused> {
    match arguments.get(name) {
        None | Some(Json::Null) => Ok(None),
        Some(_) => text(arguments, name).map(|text| Some(text.to_owned())),
    }
}

fn id(arguments: &JsonObject, name: &str) -> Result<u32, Refused> {
    let id = (arguments.get(name))
        .and_then(Json::as_f64)
        .filter(|id| id.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(id));
    match id {
        Some(id) => Ok(id as u32),
        None => Err(Refused::Arguments(format!(
            "{name} must be an id: a whole number."
        ))),
    }
}

/// A failed call's result, saying why for the agent to read.
pub(crate) fn failed(why: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(why.into())])
}

/// What the server's thread shares with the window: where calls go, how
/// to wake the window for them, and the commands to check them against.
pub(crate) struct Link {
    pub calls: Sender<Call>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub commands: Commands,
}

/// The agents connected, by connection, as their clients name themselves.
pub(crate) type Agents = Arc<Mutex<BTreeMap<u64, String>>>;

/// The tools, served to one agent.
#[derive(Clone)]
pub(crate) struct Tools {
    pub link: Arc<Link>,
    /// The connection's place in `agents`.
    pub connection: u64,
    pub agents: Agents,
}

impl Tools {
    /// The agent's name, as its client gives it in the call `context`, or
    /// as it gave it when it connected, and noted for the status bar.
    fn agent(&self, context: &RequestContext<RoleServer>) -> String {
        let named = context.client_info().map(|client| client.name);
        let mut agents = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        let name = (agents.entry(self.connection)).or_insert_with(|| "an agent".to_owned());
        if let Some(named) = named.filter(|named| named != name) {
            *name = named;
            (self.link.wake)();
        }
        name.clone()
    }
}

impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        server_info()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.agent(&context);
        Ok(ListToolsResult::with_all_items(list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let agent = self.agent(&context);
        let arguments = request.arguments.unwrap_or_default();
        let asked = match asked(&request.name, &arguments, &self.link.commands) {
            Ok(asked) => asked,
            Err(Refused::NoTool(name)) => {
                let message = format!("there's no tool called {name}");
                return Err(ErrorData::invalid_params(message, None));
            }
            Err(Refused::Arguments(why)) => return Ok(failed(why).into()),
        };
        let (reply, answer) = Reply::new(agent);
        if self.link.calls.send(Call { asked, reply }).is_err() {
            return Ok(failed("Musicable has stopped letting agents control it.").into());
        }
        (self.link.wake)();
        let result = match answer.await {
            Ok(Ok(json)) => CallToolResult::success(vec![ContentBlock::text(json.to_string())]),
            Ok(Err(why)) => failed(why),
            Err(_) => failed(
                "Musicable stopped letting agents control it before it answered: \
                 get_history says whether the change was made.",
            ),
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::TrackId;

    use super::*;

    fn registry() -> Registry {
        let mut registry = Registry::new();
        musicable_commands::register_all(&mut registry);
        registry
    }

    fn call(name: &str, arguments: Json) -> Result<Asked, Refused> {
        let Json::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        asked(name, &arguments, &commands(&registry()))
    }

    fn refused(why: &str) -> Result<Asked, Refused> {
        Err(Refused::Arguments(why.to_owned()))
    }

    #[test]
    fn calls_are_checked_before_they_reach_the_window() {
        assert_eq!(
            call(RUN_COMMAND, json!({"id": "track.new_midi_track"})),
            Ok(Asked::Run(Run::One(
                "track.new_midi_track".into(),
                Args::new()
            )))
        );
        // As JSON reads them: the command takes them as its parameters' types.
        let args = Args::new().with("track", 2_i64).with("start", 4_i64);
        assert_eq!(
            call(
                RUN_COMMAND,
                json!({"id": "clip.insert_midi_clip", "args": {"track": 2, "start": 4}})
            ),
            Ok(Asked::Run(Run::One("clip.insert_midi_clip".into(), args)))
        );
        assert_eq!(
            call(RUN_COMMAND, json!({"id": "track.nonsense"})),
            refused("There's no command called track.nonsense: list_commands lists them.")
        );
        assert_eq!(
            call(RUN_COMMAND, json!({"id": "file.quit"})),
            refused("Agents can't run file.quit: it's left to the user.")
        );
        // The app asks the user for the file, which an agent must give.
        assert_eq!(
            call(RUN_COMMAND, json!({"id": "file.save_as"})),
            refused(
                "The argument file is missing: describe_command file.save_as gives its arguments."
            )
        );
        assert_eq!(
            call(
                RUN_COMMAND,
                json!({"id": "transport.set_tempo", "args": {"bpm": "fast"}})
            ),
            refused(
                "The argument bpm must be a number: describe_command transport.set_tempo gives \
                 its arguments."
            )
        );
        assert_eq!(
            call(RUN_COMMAND, json!({"id": "transport.play", "speed": 2})),
            refused("run_command takes no argument called speed.")
        );
        assert_eq!(
            call(
                RUN_COMMANDS,
                json!({"name": "Two", "commands": [
                    {"id": "transport.set_tempo", "args": {"bpm": 92}},
                    {"id": "transport.set_tempo", "args": {"tempo": 92}},
                ]})
            ),
            refused(
                "Command 2: There's no argument called tempo: describe_command \
                 transport.set_tempo gives its arguments."
            )
        );
        let tempo = |bpm: i64| ("transport.set_tempo".into(), Args::new().with("bpm", bpm));
        assert_eq!(
            call(
                RUN_COMMANDS,
                json!({"name": "Two", "commands": [
                    {"id": "transport.set_tempo", "args": {"bpm": 92}},
                    {"id": "transport.set_tempo", "args": {"bpm": 93}},
                ]})
            ),
            Ok(Asked::Run(Run::Batch(
                "Two".into(),
                vec![tempo(92), tempo(93)]
            )))
        );
        assert_eq!(
            call(RUN_COMMANDS, json!({"name": " ", "commands": []})),
            refused("The step needs a name.")
        );
        assert_eq!(
            call(GET_AUTOMATION, json!({"param": "track 3 volume"})),
            Ok(Asked::Automation(
                musicable_model::param::ParamAddress::Volume(musicable_model::Chain::Track(
                    TrackId(3)
                ))
            ))
        );
        assert_eq!(
            call(GET_CLIP, json!({"clip": 7})),
            Ok(Asked::Clip(ClipId(7)))
        );
        assert_eq!(
            call(GET_CLIP, json!({"clip": -1})),
            refused("clip must be an id: a whole number.")
        );
        assert_eq!(
            call(GET_AUTOMATION, json!({"param": "modulator 6 param 2"})),
            Ok(Asked::Automation(
                musicable_model::param::ParamAddress::Modulator(ModulatorId(6), 2)
            ))
        );
        assert_eq!(
            call(GET_MODULATOR, json!({"modulator": 6})),
            Ok(Asked::Modulator(ModulatorId(6)))
        );
        assert_eq!(call(GET_HISTORY, json!({})), Ok(Asked::History));
        assert_eq!(call(GET_PERFORMANCE, json!({})), Ok(Asked::Performance));
        assert_eq!(
            call("get_mood", json!({})),
            Err(Refused::NoTool("get_mood".into()))
        );
    }

    #[test]
    fn every_tool_has_a_schema_of_its_arguments() {
        let tools = list();
        assert_eq!(tools.len(), 12);
        for tool in &tools {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            assert!(tool.description.is_some() && tool.title.is_some());
        }
    }
}

//! The tools agents call: a few general ones that cover every command, so
//! a new command reaches agents with no change here, and reads of the
//! project, a clip, a device, a modulator, a lane of automation, the
//! editor's state, the undo history, what the project costs the CPU, what
//! plays and what's under way, and the messages the window shows.
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

use crate::waiting::{self, Until};
use crate::{Asked, Call, Reply, Run, Shot, Tree, reads};

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
pub const GET_STATUS: &str = "get_status";
pub const GET_MESSAGES: &str = "get_messages";
pub const GET_SETTINGS: &str = "get_settings";
pub const LIST_AUDIO_DEVICES: &str = "list_audio_devices";
pub const GET_PLUGINS: &str = "get_plugins";
pub const GET_LAYOUT: &str = "get_layout";
pub const GET_LEVELS: &str = "get_levels";
pub const WAIT_FOR: &str = "wait_for";
pub const GET_COMMAND_LOG: &str = "get_command_log";
pub const GET_WINDOWS: &str = "get_windows";
pub const GET_SCREENSHOT: &str = "get_screenshot";
pub const GET_UI_TREE: &str = "get_ui_tree";

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
             settings, and the transients analysis found, once it has) and its curves, by \
             parameter, if it has them.",
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
             input, last, as wire.connect takes them. A plugin's says whether it loaded, why \
             it plays nothing if it doesn't, and whether its window is open. filter keeps the \
             parameters whose names hold it, for a plugin with thousands.",
            object(
                json!({
                    "device": id_number("The device's id."),
                    "filter": {
                        "type": "string",
                        "description": "Text the parameters' names hold, in any case.",
                    },
                }),
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
             panel, which shows the selected track's, in the order they play; what the piano \
             roll and the Sample Editor have in sight, the tracks' heights, take and \
             automation lanes, and what's on the clipboards.",
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
        read(
            GET_STATUS,
            "Get Status",
            "Reads what plays and what's under way: whether the transport plays and records, \
             where the playhead is, in beats and seconds, the takes being recorded, the \
             question the window waits for the user to answer, if it asks one, and the work \
             that lands later, with nothing to wait on: audio files being analysed, segments \
             still to render, a graph compiling, an export, with how much of it is done, and \
             the plugins' and the library's scans. idle is true once none of that work is \
             under way, and wait_for waits until then. revision goes up with every change to \
             the project.",
            object(json!({}), &[]),
        ),
        read(
            WAIT_FOR,
            "Wait For",
            "Waits until what lands later has, then answers with met, true, the seconds it \
             waited, and the status as get_status reads it. until says what for: idle, no work \
             under way, as get_status's idle says; stopped, the transport not playing; \
             recorded, the takes in the project after Stop; analysed, no audio file being \
             analysed; scanned, no plugin scan and no library scan, file reading or presets \
             search; rendered, no segment left to render and no graph compiling; changed, the \
             project changed since the revision since, from get_status, or since the wait \
             began, as when the user edits it; answered, no question waiting for the user. \
             After timeout seconds, it answers with met false. Cancelling the call stops the \
             wait.",
            object(
                json!({
                    "until": {
                        "type": "string",
                        "enum": Until::names(),
                        "description": "What to wait for.",
                    },
                    "timeout": {
                        "type": "number",
                        "minimum": 0,
                        "maximum": 600,
                        "description": "The most seconds to wait: 60 unless it says. With 0, \
                                        it reads the status once.",
                    },
                    "since": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "With changed, the revision the project should move on \
                                        from, as get_status gave it.",
                    },
                }),
                &["until"],
            ),
        ),
        tool(
            GET_MESSAGES,
            "Reads the messages the window shows the user, oldest first: errors and notices, \
             such as a file or a plugin that's missing, a device that failed, or an export's \
             problems. With clear, it clears them too, as Dismiss does, so the next call gives \
             only those that came after.",
            object(
                json!({
                    "clear": {
                        "type": "boolean",
                        "description": "Whether to clear the messages once read, as Dismiss does.",
                    }
                }),
                &[],
            ),
        )
        .with_title("Get Messages")
        .annotate(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(false)
                .open_world(false),
        ),
        read(
            GET_SETTINGS,
            "Get Settings",
            "Reads the settings, as the settings file has them, by section: audio, behaviour \
             (the profile in use's), export, browser, recording, analysis, render_cache and \
             the rest; which of them options.set can change, by key, such as \
             audio.render_ahead; the profile in use; the settings file; what plays, its device \
             and driver type, sample rate, buffer, latency and inputs, or null when nothing \
             does; and the recording offset of the device playing, in frames. Settings aren't \
             steps in the undo history: an agent that changes one puts it back when it's done.",
            object(json!({}), &[]),
        ),
        read(
            GET_PLUGINS,
            "Get Plugins",
            "Lists the plugins installed, as the browser does, for device.add_plugin: each \
             one's format, id, name, vendor, kind (instrument, midi_effect or audio_effect) \
             and Render Ahead (auto, or never if it always renders live), and whether the \
             plugins are being scanned. A filter keeps those whose names, vendors or ids \
             hold it.",
            object(
                json!({
                    "filter": {
                        "type": "string",
                        "description": "Text the name, the vendor or the id holds, in any case.",
                    }
                }),
                &[],
            ),
        ),
        read(
            GET_LAYOUT,
            "Get Layout",
            "Reads how the window is laid out: each panel's id and title, which window it's \
             in, the main one or a window of its own, whether it's hidden, and whether it's \
             in sight, rather than hidden or behind another tab; each window of its own's \
             place, size and panels; the monitors; and the workspace and the profile in use. \
             view.toggle_* commands show and hide panels by these ids.",
            object(json!({}), &[]),
        ),
        read(
            GET_WINDOWS,
            "Get Windows",
            "Lists Musicable's windows: the main window and the windows of their own that panels \
             moved to, each with its window, main or its number from 1, its title, where it is \
             and how big, in pixels on the desktop, whether it's minimized or has the focus, and \
             the panels in sight in it; the dialogs open over the main window, such as \
             Settings; and the plugins' windows that are open, each with its device's id, its \
             title and whether get_screenshot can take it.",
            object(json!({}), &[]),
        ),
        read(
            GET_SCREENSHOT,
            "Get Screenshot",
            "Takes a picture of a window, the main window unless window says, of a panel, or of \
             a plugin device's window, as it's drawn, whatever covers it or wherever it is, and \
             gives it as an image after the window, the panel or the device, and its size in \
             pixels. With file, it saves it there as a PNG too. A minimized window can't be \
             taken, and a panel only while it's in sight, as get_layout says. The windows \
             panels moved to, and plugins' windows, are taken only on Windows so far.",
            object(
                json!({
                    "window": {
                        "description": "main, or a window of its own's number from 1, as \
                                        get_windows gives it.",
                        "anyOf": [
                            { "type": "string", "enum": ["main"] },
                            { "type": "integer", "minimum": 1 },
                        ],
                    },
                    "panel": {
                        "type": "string",
                        "description": "A panel's id, as get_layout gives it, such as mixer, to \
                                        take only it.",
                    },
                    "device": {
                        "type": "integer",
                        "description": "A plugin device's id, to take its plugin's window, \
                                        which get_windows lists while it's open.",
                    },
                    "file": {
                        "type": "string",
                        "description": "A full path to a .png file to save it to.",
                    },
                }),
                &[],
            ),
        ),
        read(
            GET_UI_TREE,
            "Get UI Tree",
            "Reads what's in a window, the main window unless window says, as a screen reader \
             does: its buttons, sliders, switches, menus, text and fields, as egui last drew \
             them, nested as they are. Each has its id, which stays the same from one frame to \
             the next, its role, such as button or slider, and where they're given, its label, \
             its value, its number and range, whether it's toggled, selected, expanded, \
             disabled or has the focus, what can be done to it, such as click or increment, and \
             where it is and how big, in pixels in the window, as get_screenshot's pictures of \
             it are. Plain containers are left out, and their insides moved up. Parts of \
             Musicable drawn without widgets, such as the clips in the arrangement, aren't in \
             it: get_project and get_editor say what's there.",
            object(
                json!({
                    "window": {
                        "description": "main, or a window of its own's number from 1, as \
                                        get_windows gives it.",
                        "anyOf": [
                            { "type": "string", "enum": ["main"] },
                            { "type": "integer", "minimum": 1 },
                        ],
                    },
                    "panel": {
                        "type": "string",
                        "description": "A panel's id, as get_layout gives it, to read only what's \
                                        in it.",
                    },
                    "filter": {
                        "type": "string",
                        "description": "Words to look for in labels, values and descriptions: \
                                        only what has them, with all that's inside it, and what \
                                        it's in, is read.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": LONGEST_TREE,
                        "description": "The most it reads, 300 unless it says. The answer's more \
                                        says how many it left out.",
                    },
                }),
                &[],
            ),
        ),
        read(
            GET_LEVELS,
            "Get Levels",
            "Reads the meters, as the mixer shows them: each track's and the master's peak \
             and RMS, left and right, and the highest sample since the peaks were cleared, in \
             dBFS, null for silence; where each modulator's outputs are now; and how far the \
             routings move each parameter they move. The meters move only while an audio \
             device plays, and fall back for a few seconds after the music stops.",
            object(json!({}), &[]),
        ),
        read(
            GET_COMMAND_LOG,
            "Get Command Log",
            "Reads the commands run this session, oldest first: the user's, from the menus, keys \
             and panels, and agents', each with its number n, its time in UTC, by, user or the \
             agent's name, the command and its args, or for run_commands the batch's name and \
             its commands, and the error, if it failed. It gives the last 100 unless last says, \
             and with since, only those after that number, to read what ran since the last call. \
             count says how many have run, and file where the session's log is, one entry a \
             line, which outlasts the app.",
            object(
                json!({
                    "since": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The number n of the entry to read after.",
                    },
                    "last": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": LONGEST_LOG,
                        "description": "The most entries to give, the latest: 100 unless it says.",
                    },
                }),
                &[],
            ),
        ),
        read(
            LIST_AUDIO_DEVICES,
            "List Audio Devices",
            "Lists the audio devices again, as the Audio settings and the mixer's input menus \
             do: the driver types this build plays through; the outputs, each with its id, \
             name, driver type, whether it's the system's default and whether it plays, for \
             options.set_audio_device; the audio inputs, each with its id, name and channels, \
             and the MIDI inputs, with their ids, for mixer.set_input.",
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
        GET_DEVICE => &["device", "filter"],
        GET_PLUGINS => &["filter"],
        GET_MODULATOR => &["modulator"],
        GET_AUTOMATION => &["param"],
        GET_MESSAGES => &["clear"],
        GET_COMMAND_LOG => &["since", "last"],
        GET_SCREENSHOT => &["window", "panel", "device", "file"],
        GET_WINDOWS => &[],
        GET_UI_TREE => &["window", "panel", "filter", "limit"],
        GET_PROJECT | GET_EDITOR | GET_HISTORY | GET_PERFORMANCE | GET_STATUS | GET_SETTINGS
        | LIST_AUDIO_DEVICES | GET_LAYOUT | GET_LEVELS => &[],
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
        GET_DEVICE => Asked::Device(
            DeviceId(id(arguments, "device")?),
            optional_text(arguments, "filter")?,
        ),
        GET_PLUGINS => Asked::Plugins(optional_text(arguments, "filter")?),
        GET_LAYOUT => Asked::Layout,
        GET_LEVELS => Asked::Levels,
        GET_WINDOWS => Asked::Windows,
        GET_SCREENSHOT => Asked::Screenshot(shot(arguments)?),
        GET_UI_TREE => {
            let window = window(arguments)?;
            let panel = optional_text(arguments, "panel")?;
            if panel.is_some() && window.is_some() {
                let problem = "Give the window or the panel, not both: a panel is in one window.";
                return Err(Refused::Arguments(problem.into()));
            }
            Asked::UiTree(Tree {
                window,
                panel,
                filter: optional_text(arguments, "filter")?.filter(|filter| !filter.is_empty()),
                limit: match optional_number(arguments, "limit", LONGEST_TREE)? {
                    Some(0) => {
                        let problem = format!("limit must be from 1 to {LONGEST_TREE}.");
                        return Err(Refused::Arguments(problem));
                    }
                    Some(limit) => limit as usize,
                    None => TREE,
                },
            })
        }
        GET_COMMAND_LOG => Asked::CommandLog {
            since: optional_number(arguments, "since", u64::MAX)?,
            last: optional_number(arguments, "last", LONGEST_LOG)?
                .map_or(LOG, |last| last as usize),
        },
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
        GET_STATUS => Asked::Status,
        GET_SETTINGS => Asked::Settings,
        LIST_AUDIO_DEVICES => Asked::AudioDevices,
        GET_MESSAGES => Asked::Messages {
            clear: flag(arguments, "clear")?,
        },
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

fn flag(arguments: &JsonObject, name: &str) -> Result<bool, Refused> {
    match arguments.get(name) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(on)) => Ok(*on),
        Some(_) => Err(Refused::Arguments(format!("{name} must be true or false."))),
    }
}

/// The window `arguments` name: the main window with `None`, or else a
/// window of its own, counted from 0.
fn window(arguments: &JsonObject) -> Result<Option<usize>, Refused> {
    match arguments.get("window") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(main)) if main == "main" => Ok(None),
        Some(window) => match window.as_u64() {
            Some(window @ 1..) => Ok(Some(window as usize - 1)),
            _ => {
                let problem = "window is main, or a window of its own's number from 1, as \
                               get_windows gives it.";
                Err(Refused::Arguments(problem.into()))
            }
        },
    }
}

/// How much of a window `get_ui_tree` reads unless it says, and the most
/// it can say.
const TREE: usize = 300;
const LONGEST_TREE: u64 = 5000;

/// What `get_screenshot`'s `arguments` ask a picture of.
fn shot(arguments: &JsonObject) -> Result<Shot, Refused> {
    let window = window(arguments)?;
    let panel = optional_text(arguments, "panel")?;
    if panel.is_some()
        && arguments
            .get("window")
            .is_some_and(|window| !window.is_null())
    {
        let problem = "Give the window or the panel, not both: a panel's picture comes from its \
                       window.";
        return Err(Refused::Arguments(problem.into()));
    }
    let device = match arguments.get("device") {
        None | Some(Json::Null) => None,
        Some(_) => Some(DeviceId(id(arguments, "device")?)),
    };
    let given = |name: &str| arguments.get(name).is_some_and(|value| !value.is_null());
    if device.is_some() && (given("window") || given("panel")) {
        let problem = "Give the device, the window or the panel, not more than one: a plugin's \
                       window is a window of its own.";
        return Err(Refused::Arguments(problem.into()));
    }
    let file = optional_text(arguments, "file")?.map(std::path::PathBuf::from);
    if let Some(file) = &file {
        if !file.is_absolute() {
            let problem = "file must be a full path.";
            return Err(Refused::Arguments(problem.into()));
        }
        if !(file.extension()).is_some_and(|extension| extension.eq_ignore_ascii_case("png")) {
            let problem = "file must be a .png file.";
            return Err(Refused::Arguments(problem.into()));
        }
    }
    Ok(Shot {
        window,
        panel,
        device,
        file,
    })
}

/// How many entries of the command log `get_command_log` gives unless it
/// says, and the most it can say.
const LOG: usize = 100;
const LONGEST_LOG: u64 = 10_000;

/// The whole number `name`, up to `most`, if it's there.
fn optional_number(arguments: &JsonObject, name: &str, most: u64) -> Result<Option<u64>, Refused> {
    match arguments.get(name) {
        None | Some(Json::Null) => Ok(None),
        Some(number) => match number.as_u64() {
            Some(number) if number <= most => Ok(Some(number)),
            _ => Err(Refused::Arguments(format!(
                "{name} must be a whole number, from 0 to {most}."
            ))),
        },
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

/// What the window answered, as the agent reads it: its JSON as text, and
/// a picture it took, its `png`, as an image after it.
fn content(mut json: Json) -> Vec<ContentBlock> {
    let png = match json.as_object_mut().and_then(|json| json.remove("png")) {
        Some(Json::String(png)) => Some(png),
        _ => None,
    };
    let mut content = vec![ContentBlock::text(json.to_string())];
    content.extend(png.map(|png| ContentBlock::image(png, "image/png")));
    content
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
        if request.name == WAIT_FOR {
            let result = match waiting::wait(&arguments) {
                Ok(wait) => self.wait_for(&agent, wait, &context).await,
                Err(why) => failed(why),
            };
            return Ok(result.into());
        }
        let asked = match asked(&request.name, &arguments, &self.link.commands) {
            Ok(asked) => asked,
            Err(Refused::NoTool(name)) => {
                let message = format!("there's no tool called {name}");
                return Err(ErrorData::invalid_params(message, None));
            }
            Err(Refused::Arguments(why)) => return Ok(failed(why).into()),
        };
        let result = match self.ask(&agent, asked).await {
            Ok(json) => CallToolResult::success(content(json)),
            Err(why) => failed(why),
        };
        Ok(result.into())
    }
}

impl Tools {
    /// Asks the window `asked` for `agent`, and waits for its answer.
    pub(crate) async fn ask(&self, agent: &str, asked: Asked) -> Result<Json, String> {
        let (reply, answer) = Reply::new(agent.to_owned());
        if self.link.calls.send(Call { asked, reply }).is_err() {
            return Err("Musicable has stopped letting agents control it.".into());
        }
        (self.link.wake)();
        match answer.await {
            Ok(answer) => answer,
            Err(_) => Err(
                "Musicable stopped letting agents control it before it answered: \
                           get_history says whether the change was made."
                    .into(),
            ),
        }
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
        assert_eq!(call(GET_STATUS, json!({})), Ok(Asked::Status));
        assert_eq!(call(GET_SETTINGS, json!({})), Ok(Asked::Settings));
        assert_eq!(call(GET_LAYOUT, json!({})), Ok(Asked::Layout));
        assert_eq!(call(GET_LEVELS, json!({})), Ok(Asked::Levels));
        assert_eq!(call(GET_WINDOWS, json!({})), Ok(Asked::Windows));
        let tree = Tree {
            window: None,
            panel: None,
            filter: None,
            limit: 300,
        };
        assert_eq!(
            call(GET_UI_TREE, json!({})),
            Ok(Asked::UiTree(tree.clone()))
        );
        assert_eq!(
            call(
                GET_UI_TREE,
                json!({"panel": "mixer", "filter": "Solo", "limit": 20})
            ),
            Ok(Asked::UiTree(Tree {
                panel: Some("mixer".into()),
                filter: Some("Solo".into()),
                limit: 20,
                ..tree.clone()
            }))
        );
        assert_eq!(
            call(GET_UI_TREE, json!({"window": 2, "filter": ""})),
            Ok(Asked::UiTree(Tree {
                window: Some(1),
                ..tree
            }))
        );
        assert_eq!(
            call(GET_UI_TREE, json!({"window": 1, "panel": "mixer"})),
            refused("Give the window or the panel, not both: a panel is in one window.")
        );
        assert_eq!(
            call(GET_UI_TREE, json!({"limit": 0})),
            refused("limit must be from 1 to 5000.")
        );
        assert_eq!(
            call(GET_UI_TREE, json!({"limit": 5001})),
            refused("limit must be a whole number, from 0 to 5000.")
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({})),
            Ok(Asked::Screenshot(Shot::default()))
        );
        let file = if cfg!(windows) {
            "C:\\shots\\mixer.PNG"
        } else {
            "/shots/mixer.PNG"
        };
        assert_eq!(
            call(GET_SCREENSHOT, json!({"panel": "mixer", "file": file})),
            Ok(Asked::Screenshot(Shot {
                window: None,
                panel: Some("mixer".into()),
                device: None,
                file: Some(file.into()),
            }))
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"device": 7})),
            Ok(Asked::Screenshot(Shot {
                device: Some(DeviceId(7)),
                ..Shot::default()
            }))
        );
        let one = "Give the device, the window or the panel, not more than one: a plugin's \
                   window is a window of its own.";
        assert_eq!(
            call(GET_SCREENSHOT, json!({"device": 7, "window": 1})),
            refused(one)
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"device": 7, "panel": "mixer"})),
            refused(one)
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"device": "eq"})),
            refused("device must be an id: a whole number.")
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"window": 2})),
            Ok(Asked::Screenshot(Shot {
                window: Some(1),
                ..Shot::default()
            }))
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"window": "main"})),
            Ok(Asked::Screenshot(Shot::default()))
        );
        let window = "window is main, or a window of its own's number from 1, as get_windows \
                      gives it.";
        assert_eq!(call(GET_SCREENSHOT, json!({"window": 0})), refused(window));
        assert_eq!(
            call(GET_SCREENSHOT, json!({"window": "side"})),
            refused(window)
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"window": 1, "panel": "mixer"})),
            refused(
                "Give the window or the panel, not both: a panel's picture comes from its window."
            )
        );
        assert_eq!(
            call(GET_SCREENSHOT, json!({"file": "mixer.png"})),
            refused("file must be a full path.")
        );
        let jpeg = if cfg!(windows) {
            "C:\\mixer.jpg"
        } else {
            "/mixer.jpg"
        };
        assert_eq!(
            call(GET_SCREENSHOT, json!({"file": jpeg})),
            refused("file must be a .png file.")
        );
        assert_eq!(
            call(GET_COMMAND_LOG, json!({})),
            Ok(Asked::CommandLog {
                since: None,
                last: 100
            })
        );
        assert_eq!(
            call(GET_COMMAND_LOG, json!({"since": 12, "last": 5})),
            Ok(Asked::CommandLog {
                since: Some(12),
                last: 5
            })
        );
        assert_eq!(
            call(GET_COMMAND_LOG, json!({"last": 10_001})),
            refused("last must be a whole number, from 0 to 10000.")
        );
        assert_eq!(
            call(GET_COMMAND_LOG, json!({"since": -1})),
            refused(&format!(
                "since must be a whole number, from 0 to {}.",
                u64::MAX
            ))
        );
        assert_eq!(
            call(GET_PLUGINS, json!({"filter": "surge"})),
            Ok(Asked::Plugins(Some("surge".into())))
        );
        assert_eq!(
            call(GET_DEVICE, json!({"device": 4, "filter": "cutoff"})),
            Ok(Asked::Device(DeviceId(4), Some("cutoff".into())))
        );
        assert_eq!(call(LIST_AUDIO_DEVICES, json!({})), Ok(Asked::AudioDevices));
        assert_eq!(
            call(GET_MESSAGES, json!({})),
            Ok(Asked::Messages { clear: false })
        );
        assert_eq!(
            call(GET_MESSAGES, json!({"clear": true})),
            Ok(Asked::Messages { clear: true })
        );
        assert_eq!(
            call("get_mood", json!({})),
            Err(Refused::NoTool("get_mood".into()))
        );
    }

    #[test]
    fn a_picture_comes_after_the_answer_as_an_image() {
        let answer = content(json!({"window": "main", "png": "iVBORw0KGgo="}));
        assert_eq!(answer.len(), 2);
        assert_eq!(answer[0].as_text().unwrap().text, r#"{"window":"main"}"#);
        let image = answer[1].as_image().unwrap();
        assert_eq!(
            (image.data.as_str(), image.mime_type.as_str()),
            ("iVBORw0KGgo=", "image/png")
        );
        assert_eq!(content(json!({"window": "main"})).len(), 1);
    }

    #[test]
    fn every_tool_has_a_schema_of_its_arguments() {
        let tools = list();
        assert_eq!(tools.len(), 24);
        for tool in &tools {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            assert!(tool.description.is_some() && tool.title.is_some());
        }
    }
}

//! The tools as agents use them: over an in-memory connection, and through
//! the bridge and a real pipe, with a window answering their calls on the
//! test's thread, as the app's window does.

use std::fs;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musicable_commands::{
    Context, Document, Editor, Keymap, NoLibrary, Registry, Request, Steps, Transport,
};
use musicable_model::clip::{AudioClip, ClipContent, Offset, Warping};
use musicable_model::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::Warp;
use musicable_model::{Chain, DeviceId, DeviceKind, PluginFormat, Project, TrackId, TrackKind};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
    ProgressNotificationParam,
};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Map, Value as Json, json};
use tokio::io::duplex;
use tokio::runtime::Runtime;

use crate::bridge::{self, Bridge};
use crate::local::{LocalTool, LocalTools};
use crate::pipe::Address;
use crate::tools::{self, Agents, Link, Tools};
use crate::{
    Call, CpuPart, CpuRow, Exporting, Host, Performance, RenderingAhead, Reply, Run, Status,
    Window, answer, host,
};

/// A transport that plays along, and does nothing else.
#[derive(Default)]
struct StubTransport {
    playing: bool,
    recording: bool,
    playhead: Beats,
}

impl Transport for StubTransport {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
    }

    fn locate(&mut self, at: Beats) {
        self.playhead = at;
    }

    fn playhead(&self) -> Beats {
        self.playhead
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }
}

/// A window of a document, which runs agents' commands as the app's does,
/// and keeps what they ask the app for.
struct TestWindow {
    commands: Registry,
    keymap: Keymap,
    document: Document,
    transport: StubTransport,
    requests: Vec<Request>,
    /// The agents whose calls it ran, in turn.
    agents: Vec<String>,
    /// What the CPU figures say.
    performance: Performance,
    /// What plays, and what's under way.
    status: Status,
    /// The messages the window shows.
    messages: Vec<String>,
}

impl TestWindow {
    fn new(project: Project) -> Self {
        let mut commands = Registry::new();
        musicable_commands::register_all(&mut commands);
        Self {
            commands,
            keymap: Keymap::default_keymap(),
            document: Document::new(project),
            transport: StubTransport::default(),
            requests: Vec::new(),
            agents: Vec::new(),
            performance: Performance::default(),
            status: Status::default(),
            messages: Vec::new(),
        }
    }
}

impl Window for TestWindow {
    fn commands(&self) -> &Registry {
        &self.commands
    }

    fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    fn project(&self) -> &Project {
        self.document.project()
    }

    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    fn history(&self) -> Steps<'_> {
        self.document.steps()
    }

    fn file(&self) -> Option<&Path> {
        None
    }

    fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    fn performance(&self) -> Performance {
        self.performance.clone()
    }

    fn status(&self) -> Status {
        self.status.clone()
    }

    fn messages(&self) -> Vec<String> {
        self.messages.clone()
    }

    fn clear_messages(&mut self) {
        self.messages.clear();
    }

    fn run(&mut self, run: Run, reply: Reply) {
        self.agents.push(reply.agent().to_owned());
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
            library: &mut NoLibrary,
        };
        match run {
            Run::One(id, args) => {
                let result = self.commands.run_with(&id, &args, &mut context);
                let undo = made_step(&mut self.document);
                reply.ran(result, undo.as_deref());
            }
            Run::Batch(name, commands) => {
                let result = self.commands.run_batch(&name, &commands, &mut context);
                let undo = made_step(&mut self.document);
                reply.ran_batch(result, undo.as_deref());
            }
        }
    }
}

/// The name of the step the commands just run made, if they made one.
fn made_step(document: &mut Document) -> Option<String> {
    let made = !document.take_committed().is_empty();
    made.then(|| document.undo_name().map(str::to_owned))
        .flatten()
}

/// An agent's client.
type Client = RunningService<RoleClient, ClientConfig>;

/// A client calling itself `name`.
fn client_called(name: &str) -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new(name, "1.0"),
    )
}

/// An agent connected to the tools, and the window that answers its calls.
struct Rig {
    runtime: Runtime,
    client: Client,
    calls: Receiver<Call>,
    window: TestWindow,
}

impl Rig {
    /// An agent connected to the tools over an in-memory stream, with a
    /// window of `project`.
    fn new(project: Project) -> Self {
        let runtime = runtime();
        let window = TestWindow::new(project);
        let (sender, calls) = mpsc::channel();
        let tools = Tools {
            link: Arc::new(Link {
                calls: sender,
                wake: Arc::new(|| {}),
                commands: tools::commands(&window.commands),
            }),
            connection: 1,
            agents: Agents::default(),
        };
        let (server, client) = duplex(1 << 16);
        runtime.spawn(host::serve(server, tools));
        let client =
            (runtime.block_on(client_called("tests").serve(client))).expect("the agent connects");
        Self {
            runtime,
            client,
            calls,
            window,
        }
    }

    /// Calls the tool `name` with `arguments`, answering the window's calls
    /// meanwhile. Gives back the JSON the tool answered, or why it failed.
    fn call(&mut self, name: &str, arguments: Json) -> Result<Json, String> {
        let Rig {
            runtime,
            client,
            calls,
            window,
        } = self;
        let result = call(runtime, client, name, arguments, || {
            while let Ok(call) = calls.try_recv() {
                answer(call, window);
            }
        });
        read(result)
    }
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("the runtime starts")
}

/// Calls the tool `name` with `arguments` through `client`, calling `pump`
/// meanwhile, as a window's frames come.
fn call(
    runtime: &Runtime,
    client: &Client,
    name: &str,
    arguments: Json,
    mut pump: impl FnMut(),
) -> CallToolResult {
    let Json::Object(arguments) = arguments else {
        panic!("a tool's arguments are an object");
    };
    let request = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
    runtime.block_on(async {
        let mut call = pin!(client.call_tool(request));
        for _ in 0..5000 {
            match tokio::time::timeout(Duration::from_millis(2), call.as_mut()).await {
                Ok(result) => return result.expect("the call gets an answer"),
                Err(_) => pump(),
            }
        }
        panic!("the call to {name} got no answer");
    })
}

/// What a tool answered, as JSON, or why it failed.
fn read(result: CallToolResult) -> Result<Json, String> {
    let text = (result.content.first())
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .expect("the result is text");
    match result.is_error {
        Some(true) => Err(text),
        _ => Ok(serde_json::from_str(&text).expect("the result is JSON")),
    }
}

fn ids(json: &Json) -> Vec<&str> {
    let commands = json["commands"].as_array().expect("commands are a list");
    commands.iter().map(|c| c["id"].as_str().unwrap()).collect()
}

#[test]
fn agents_see_the_tools_and_how_to_use_them() {
    let rig = Rig::new(Project::new());
    let tools = rig.runtime.block_on(rig.client.list_all_tools()).unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "list_commands",
            "describe_command",
            "run_command",
            "run_commands",
            "get_project",
            "get_clip",
            "get_device",
            "get_modulator",
            "get_automation",
            "get_editor",
            "get_history",
            "get_performance",
            "get_status",
            "get_messages",
            "get_settings",
            "get_plugins",
            "get_layout",
            "get_levels",
            "list_audio_devices",
        ]
    );
    let server = rig
        .client
        .peer_info()
        .expect("the server introduced itself");
    let info = server
        .server_info
        .as_ref()
        .expect("the server named itself");
    assert_eq!(info.name, "musicable");
    let instructions = server.instructions.as_deref().unwrap_or_default();
    assert!(instructions.contains("undo"), "{instructions}");
}

#[test]
fn commands_are_listed_and_described() {
    let mut rig = Rig::new(Project::new());
    let all = rig.call("list_commands", json!({})).unwrap();
    let all = ids(&all);
    assert!(all.len() > 150, "{} commands", all.len());
    // Those that only show a window, or ask the user, are the user's.
    for theirs in [
        "file.quit",
        "options.settings",
        "view.command_palette",
        "edit.rename",
    ] {
        assert!(!all.contains(&theirs), "{theirs} is listed");
    }

    let tempo = rig
        .call("list_commands", json!({"filter": "TEMPO"}))
        .unwrap();
    let set_tempo = (tempo["commands"].as_array().unwrap().iter())
        .find(|command| command["id"] == "transport.set_tempo")
        .expect("a filter finds commands by id");
    assert_eq!(set_tempo["title"], "Set Tempo");
    assert_eq!(
        set_tempo["args"],
        json!([
            {"name": "bpm", "type": "a number"},
            {"name": "at", "type": "a number of beats", "optional": true},
        ])
    );
    // It needs an argument, so it doesn't say whether it applies without.
    assert_eq!(set_tempo.get("applies"), None);

    let copy = rig
        .call("list_commands", json!({"filter": "edit.copy"}))
        .unwrap();
    assert_eq!(copy["commands"][0]["applies"], false);
    assert_eq!(copy["commands"][0]["keys"], json!(["Ctrl+C"]));

    let add = rig
        .call("describe_command", json!({"id": "notes.add"}))
        .unwrap();
    assert_eq!(add["title"], "Add Note");
    assert_eq!(add["input_schema"]["required"], json!(["start", "key"]));
    assert_eq!(add["input_schema"]["properties"]["key"]["type"], "integer");
    let save_as = rig
        .call("describe_command", json!({"id": "file.save_as"}))
        .unwrap();
    assert_eq!(save_as["alone"], true);
    assert_eq!(save_as["input_schema"]["required"], json!(["file"]));

    assert_eq!(
        rig.call("describe_command", json!({"id": "file.quit"})),
        Err("Agents can't run file.quit: it's left to the user.".into())
    );
    assert_eq!(
        rig.call("describe_command", json!({"id": "notes.dance"})),
        Err("There's no command called notes.dance: list_commands lists them.".into())
    );
}

#[test]
fn commands_run_as_a_menu_runs_them_and_can_be_undone() {
    let mut rig = Rig::new(Project::new());
    let track = rig
        .call("run_command", json!({"id": "track.new_midi_track"}))
        .unwrap();
    assert_eq!(track, json!({"result": 0, "undo": "New MIDI Track"}));
    let clip = rig
        .call(
            "run_command",
            json!({"id": "clip.insert_midi_clip", "args": {"track": 0, "start": 4, "end": 8}}),
        )
        .unwrap();
    assert_eq!(clip, json!({"result": [0], "undo": "Insert MIDI Clip"}));
    let note = rig
        .call(
            "run_command",
            json!({"id": "notes.add", "args": {"clip": 0, "start": 0.5, "key": 36, "velocity": 0.8}}),
        )
        .unwrap();
    assert_eq!(note, json!({"result": 0, "undo": "Add Note"}));
    assert_eq!(rig.window.agents, ["tests", "tests", "tests"]);

    // Changing only what's selected makes no step.
    let selected = rig
        .call(
            "run_command",
            json!({"id": "edit.select_clips", "args": {"clips": [0]}}),
        )
        .unwrap();
    assert_eq!(selected, json!({"result": null}));

    let history = rig.call("get_history", json!({})).unwrap();
    assert_eq!(
        history,
        json!({
            "steps": ["New MIDI Track", "Insert MIDI Clip", "Add Note"],
            "done": 3,
            "undo": "Add Note",
            "redo": null,
        })
    );
    let undo = rig.call("run_command", json!({"id": "edit.undo"})).unwrap();
    assert_eq!(undo["result"], Json::Null);
    let history = rig.call("get_history", json!({})).unwrap();
    assert_eq!(history["done"], 2);
    assert_eq!(history["redo"], "Add Note");
    let clip = rig.call("get_clip", json!({"clip": 0})).unwrap();
    assert_eq!(clip["notes"], json!([]));
}

#[test]
fn a_batch_is_one_step_to_undo_or_none_at_all() {
    let mut rig = Rig::new(Project::new());
    let started = rig
        .call(
            "run_commands",
            json!({"name": "Start a Beat", "commands": [
                {"id": "transport.set_tempo", "args": {"bpm": 92}},
                {"id": "track.new_midi_track"},
                {"id": "locator.add", "args": {"at": 16, "name": "Chorus"}},
            ]}),
        )
        .unwrap();
    assert_eq!(
        started,
        json!({"results": [null, 0, 0], "undo": "Start a Beat"})
    );
    let project = rig.call("get_project", json!({})).unwrap();
    assert_eq!(project["tempo"], json!([{"at": 0, "bpm": 92.0}]));
    assert_eq!(project["tracks"][0]["kind"], "midi");
    assert_eq!(
        project["locators"],
        json!([{"id": 0, "name": "Chorus", "at": 16}])
    );
    let history = rig.call("get_history", json!({})).unwrap();
    assert_eq!(history["steps"], json!(["Start a Beat"]));

    // The second fails, so the first's change goes too.
    assert_eq!(
        rig.call(
            "run_commands",
            json!({"name": "Rename", "commands": [
                {"id": "track.rename", "args": {"track": 0, "name": "Drums"}},
                {"id": "locator.rename", "args": {"locator": 9, "name": "Verse"}},
            ]}),
        ),
        Err("Command 2 failed: there's no locator 9. Nothing changed.".into())
    );
    let project = rig.call("get_project", json!({})).unwrap();
    assert_eq!(project["tracks"][0]["name"], "MIDI 1");
    let history = rig.call("get_history", json!({})).unwrap();
    assert_eq!(history["steps"], json!(["Start a Beat"]));

    // Commands that open, save or undo run on their own.
    assert_eq!(
        rig.call(
            "run_commands",
            json!({"name": "Undo Twice", "commands": [{"id": "edit.undo"}, {"id": "edit.undo"}]}),
        ),
        Err(
            "Command 1 failed: edit.undo runs on its own, not among other commands. Nothing \
             changed."
                .into()
        )
    );
}

#[test]
fn wrong_arguments_and_commands_that_dont_apply_say_why() {
    let mut rig = Rig::new(Project::new());
    assert_eq!(
        rig.call("run_command", json!({"id": "edit.copy"})),
        Err(
            "edit.copy doesn't apply now. Without arguments it acts on the selection, which \
             get_editor gives."
                .into()
        )
    );
    assert_eq!(
        rig.call(
            "run_command",
            json!({"id": "clip.move", "args": {"clips": "all", "by": 1}})
        ),
        Err(
            "The argument clips must be a list, each a clip: describe_command clip.move gives \
             its arguments."
                .into()
        )
    );
    assert_eq!(
        rig.call(
            "run_command",
            json!({"id": "locator.rename", "args": {"locator": 5, "name": "Verse"}})
        ),
        Err("There's no locator 5.".into())
    );
    assert_eq!(
        rig.call("run_command", json!({"id": "file.save_as", "args": {}})),
        Err(
            "The argument file is missing: describe_command file.save_as gives its arguments."
                .into()
        )
    );
    assert_eq!(
        rig.call("get_automation", json!({"param": "track 0 volume"})),
        Err("There's no parameter track 0 volume.".into())
    );
    assert_eq!(
        rig.call("get_clip", json!({"clip": 3})),
        Err("There's no clip 3.".into())
    );
    // Only those whose arguments fit reached the window.
    assert_eq!(rig.window.agents, ["tests", "tests"]);
}

#[test]
fn reads_give_clips_devices_automation_and_the_editor() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_midi_track"},
            {"id": "clip.insert_midi_clip", "args": {"track": 0, "start": 0, "end": 4}},
            {"id": "notes.add", "args": {"clip": 0, "start": 1, "key": 60, "length": 0.5}},
            {"id": "notes.add", "args": {"clip": 0, "start": 0, "key": 64, "velocity": 0.5}},
            {"id": "device.add", "args": {"kind": "eq", "track": 0}},
            {"id": "automation.add_point", "args": {"param": "track 0 volume", "at": 2, "value": 0.5}},
            {"id": "track.rename", "args": {"track": 0, "name": "Keys"}},
        ]}),
    )
    .unwrap();

    let project = rig.call("get_project", json!({})).unwrap();
    let track = &project["tracks"][0];
    assert_eq!(track["name"], "Keys");
    assert_eq!(track["gain"], 1.0);
    assert_eq!(
        track["devices"],
        json!([{"id": 0, "name": "EQ", "kind": "eq"}])
    );
    assert_eq!(
        track["clips"],
        json!([{"id": 0, "name": "MIDI Clip", "start": 0, "length": 4, "kind": "midi", "notes": 2}])
    );
    assert_eq!(
        project["automation"],
        json!([{"param": "track 0 volume", "points": 1, "overridden": false}])
    );
    assert_eq!(project["end"], 4);

    let clip = rig.call("get_clip", json!({"clip": 0})).unwrap();
    assert_eq!(clip["track"], 0);
    assert_eq!(clip["kind"], "midi");
    // In order of time.
    assert_eq!(
        clip["notes"],
        json!([
            {"id": 1, "key": 64, "start": 0, "length": 0.125, "velocity": 0.5},
            {"id": 0, "key": 60, "start": 1, "length": 0.5, "velocity": 0.787_401_56},
        ])
    );

    let eq = rig.call("get_device", json!({"device": 0})).unwrap();
    assert_eq!(eq["chain"], json!({"track": 0}));
    let params = eq["params"].as_array().unwrap();
    assert!(!params.is_empty());
    for (number, param) in params.iter().enumerate() {
        assert_eq!(param["param"], number);
        assert!(param["name"].as_str().is_some_and(|name| !name.is_empty()));
        assert!(param["value"].is_number() && param["shown"].is_string());
    }
    // A filter keeps the parameters whose names hold it, in any case.
    let name = params[0]["name"].as_str().unwrap().to_uppercase();
    let filtered = rig.call("get_device", json!({"device": 0, "filter": name}));
    let filtered = filtered.unwrap();
    let kept = filtered["params"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() <= params.len());
    assert!(kept.iter().all(|param| {
        let held = param["name"].as_str().unwrap().to_uppercase();
        held.contains(&name)
    }));
    let none = rig.call("get_device", json!({"device": 0, "filter": "no such knob"}));
    assert_eq!(none.unwrap()["params"], json!([]));

    let lane = rig
        .call("get_automation", json!({"param": "track 0 volume"}))
        .unwrap();
    assert_eq!(
        lane,
        json!({"param": "track 0 volume", "points": [{"at": 2, "value": 0.5}], "overridden": false})
    );
    let pan = rig
        .call("get_automation", json!({"param": "track 0 pan"}))
        .unwrap();
    assert_eq!(pan["points"], json!([]));

    rig.call(
        "run_command",
        json!({"id": "edit.select_clips", "args": {"clips": [0]}}),
    )
    .unwrap();
    let editor = rig.call("get_editor", json!({})).unwrap();
    assert_eq!(editor["selection"]["clips"], json!([0]));
    assert_eq!(editor["selected_track"], json!({"track": 0}));
    assert_eq!(editor["focus"], "arrangement");
    assert_eq!(editor["piano_roll"]["clip"], 0);
    assert_eq!(editor["take_lanes"], json!([]));
    assert_eq!(
        editor["clipboards"],
        json!({"clips": null, "notes": null, "devices": null})
    );
    assert!(
        editor["piano_roll"]["in_sight"]["end"].is_number(),
        "{editor}"
    );

    // What's copied, and the lanes shown.
    rig.call(
        "run_commands",
        json!({"name": "Look", "commands": [
            {"id": "edit.copy"},
            {"id": "automation.show_lane", "args": {"param": "track 0 volume"}},
        ]}),
    )
    .unwrap();
    let editor = rig.call("get_editor", json!({})).unwrap();
    assert_eq!(
        editor["clipboards"]["clips"],
        json!({"clips": 1, "length": 4})
    );
    assert_eq!(
        editor["automation_lanes"],
        json!([{"chain": {"track": 0}, "params": ["track 0 volume"]}])
    );
}

#[test]
fn reads_say_which_devices_are_bypassed_or_deactivated() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_audio_track"},
            {"id": "device.add", "args": {"kind": "eq", "track": 0}},
            {"id": "device.add", "args": {"kind": "delay", "track": 0}},
            {"id": "device.add", "args": {"kind": "utility", "track": 0}},
            {"id": "device.set_bypass", "args": {"devices": [0], "bypassed": true}},
            {"id": "device.set_active", "args": {"devices": [1], "active": false}},
        ]}),
    )
    .unwrap();
    let project = rig.call("get_project", json!({})).unwrap();
    assert_eq!(
        project["tracks"][0]["devices"],
        json!([
            {"id": 0, "name": "EQ", "kind": "eq", "bypassed": true},
            {"id": 1, "name": "Delay", "kind": "delay", "active": false},
            {"id": 2, "name": "Utility", "kind": "utility"},
        ])
    );
    let eq = rig.call("get_device", json!({"device": 0})).unwrap();
    assert_eq!((&eq["bypassed"], eq.get("active")), (&json!(true), None));
    let delay = rig.call("get_device", json!({"device": 1})).unwrap();
    assert_eq!(
        (delay.get("bypassed"), &delay["active"]),
        (None, &json!(false))
    );

    // The devices selected, in the order they play.
    rig.call(
        "run_command",
        json!({"id": "device.select", "args": {"devices": [2, 0]}}),
    )
    .unwrap();
    let editor = rig.call("get_editor", json!({})).unwrap();
    assert_eq!(editor["devices"], json!({"selected": [0, 2]}));
    assert_eq!(editor["focus"], "devices");
}

#[test]
fn reads_give_modulators_and_their_routings() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_audio_track"},
            {"id": "device.add", "args": {"kind": "utility", "track": 0}},
            {"id": "modulation.add", "args": {"kind": "lfo", "track": 0}},
            {"id": "modulation.add", "args": {"kind": "macro", "track": 0}},
            // A Macro starts with a knob.
            {"id": "modulation.add_control", "args": {"modulator": 1, "kind": "toggle"}},
            {"id": "modulation.add_routing", "args": {
                "modulator": 0, "param": "track 0 pan", "amount": 0.5,
            }},
            {"id": "modulation.add_routing", "args": {
                "modulator": 1, "output": 1, "param": "device 0 param 1", "amount": -0.25,
            }},
            {"id": "modulation.set_curve", "args": {"routing": 1, "curve": "stepped"}},
            {"id": "modulation.set_scale", "args": {"routing": 1, "modulator": 1}},
            {"id": "modulation.set_bypass", "args": {"routing": 0, "bypassed": true}},
            {"id": "automation.add_point", "args": {
                "param": "modulator 0 param 0", "at": 0, "value": 2,
            }},
        ]}),
    )
    .unwrap();

    let project = rig.call("get_project", json!({})).unwrap();
    let track = &project["tracks"][0];
    assert_eq!(
        track["modulators"],
        json!([
            {"id": 0, "name": "LFO 1", "kind": "lfo"},
            {"id": 1, "name": "Macro 1", "kind": "macro"},
        ])
    );
    assert_eq!(
        track["routings"],
        json!([
            {
                "id": 0, "modulator": 0, "output": 0, "param": "track 0 pan", "amount": 0.5,
                "curve": "linear", "lag": 0.0, "bypassed": true,
            },
            {
                "id": 1, "modulator": 1, "output": 1, "param": "device 0 param 1",
                "amount": -0.25, "curve": "stepped", "steps": 4,
                "scale": {"modulator": 1, "output": 0}, "lag": 0.0,
            },
        ])
    );
    // A channel without modulators says nothing of them.
    assert_eq!(project["master"].get("modulators"), None);
    assert_eq!(
        project["automation"],
        json!([{"param": "modulator 0 param 0", "points": 1, "overridden": false}])
    );

    let lfo = rig.call("get_modulator", json!({"modulator": 0})).unwrap();
    assert_eq!(
        (&lfo["kind"], &lfo["chain"], &lfo["position"]),
        (&json!("lfo"), &json!({"track": 0}), &json!(0))
    );
    let params = lfo["params"].as_array().unwrap();
    assert!(!params.is_empty());
    for (number, param) in params.iter().enumerate() {
        assert_eq!(param["param"], number);
        assert!(param["name"].as_str().is_some_and(|name| !name.is_empty()));
        assert!(param["value"].is_number() && param["shown"].is_string());
    }
    assert_eq!(
        lfo["outputs"],
        json!([{"output": 0, "name": "LFO 1", "range": "-1 to 1"}])
    );

    let macro_ = rig.call("get_modulator", json!({"modulator": 1})).unwrap();
    let controls: Vec<_> = (macro_["params"].as_array().unwrap().iter())
        .map(|p| (&p["param"], &p["name"], &p["kind"], &p["value"]))
        .collect();
    assert_eq!(
        controls,
        [
            (&json!(0), &json!("Knob 1"), &json!("knob"), &json!(0.0)),
            (&json!(1), &json!("Button 1"), &json!("toggle"), &json!(0.0)),
        ]
    );
    assert_eq!(
        rig.call("get_modulator", json!({"modulator": 9})),
        Err("There's no modulator 9.".into())
    );
}

#[test]
fn reads_give_the_wires_and_each_device_s_side_input() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_audio_track"},
            {"id": "track.rename", "args": {"track": 0, "name": "Kick"}},
            {"id": "track.new_audio_track"},
            {"id": "track.rename", "args": {"track": 1, "name": "Bass"}},
            {"id": "device.add", "args": {"kind": "compressor", "track": 1}},
            {"id": "device.add", "args": {"kind": "eq", "track": 0}},
            {"id": "wire.connect", "args": {"device": 0, "track": 0}},
            {"id": "wire.connect", "args": {"device": 0, "after": 1, "gain": 0.5}},
            {"id": "automation.add_point", "args": {"param": "wire 1", "at": 2, "value": 0.25}},
        ]}),
    )
    .unwrap();

    let project = rig.call("get_project", json!({})).unwrap();
    let kick = json!({"track": 0, "tap": "post_fx", "name": "Kick Post FX"});
    let after_eq = json!({"after": 1, "name": "Kick after EQ"});
    let to = json!({"device": 0, "port": "sidechain", "name": "Compressor on Bass"});
    assert_eq!(
        project["wires"],
        json!([
            {"id": 0, "from": kick, "to": to, "gain": 1.0},
            {"id": 1, "from": after_eq, "to": to, "gain": 0.5},
        ])
    );
    assert_eq!(project["automation"][0]["param"], "wire 1");

    let compressor = rig.call("get_device", json!({"device": 0})).unwrap();
    assert_eq!(
        compressor["ports"],
        json!([{"port": "sidechain", "name": "Sidechain", "wires": [
            {"id": 0, "from": kick, "gain": 1.0},
            {"id": 1, "from": after_eq, "gain": 0.5},
        ]}])
    );
    // Connecting turned the Sidechain on.
    let sidechain = &compressor["params"][8];
    assert_eq!(
        (&sidechain["name"], &sidechain["value"]),
        (&json!("Sidechain On"), &json!(1.0))
    );
    let eq = rig.call("get_device", json!({"device": 1})).unwrap();
    assert_eq!(eq.get("ports"), None);

    let lane = rig
        .call("get_automation", json!({"param": "wire 1"}))
        .unwrap();
    assert_eq!(lane["points"], json!([{"at": 2, "value": 0.25}]));

    // From the master: the bass feeds it, so that would close a loop.
    assert_eq!(
        rig.call(
            "run_command",
            json!({"id": "wire.connect", "args": {"device": 0, "master": true}})
        ),
        Err(
            "That would make a loop: what the Compressor on Bass puts out reaches Master Post \
             FX."
            .into()
        )
    );
}

#[test]
fn reads_give_the_inputs_tracks_take_from_other_tracks() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_audio_track"},
            {"id": "track.rename", "args": {"track": 0, "name": "Kick"}},
            {"id": "track.new_audio_track"},
            {"id": "track.rename", "args": {"track": 1, "name": "Print"}},
            {"id": "track.new_audio_track"},
            {"id": "track.new_audio_track"},
            {"id": "device.add", "args": {"kind": "eq", "track": 0}},
            {"id": "mixer.set_input", "args": {"track": 1, "from": 0, "tap": "post_fader"}},
            {"id": "mixer.set_input", "args": {"track": 2, "resampling": true}},
            {"id": "mixer.set_input", "args": {"track": 3, "after": 0}},
            {"id": "mixer.set_output", "args": {"track": 0, "sends_only": true}},
        ]}),
    )
    .unwrap();

    let project = rig.call("get_project", json!({})).unwrap();
    let tracks = project["tracks"].as_array().unwrap();
    assert_eq!(
        tracks[1]["input"],
        json!({"from": 0, "tap": "post_fader", "name": "Kick Post Fader"})
    );
    assert_eq!(tracks[1]["monitor"], "in");
    assert_eq!(
        tracks[2]["input"],
        json!({"resampling": true, "name": "Master Post Fader"})
    );
    assert_eq!(
        tracks[3]["input"],
        json!({"after": 0, "name": "Kick after EQ"})
    );
    // An interface's input isn't listed.
    assert_eq!(tracks[0].get("input"), None);
    assert_eq!(tracks[0]["output"], "sends_only");
    // The print takes what comes out of the kick.
    assert_eq!(
        rig.call(
            "run_command",
            json!({"id": "mixer.set_input", "args": {"track": 0, "from": 1}})
        ),
        Err("That would make a loop: what comes out of Kick reaches Print Post FX.".into())
    );
}

#[test]
fn a_plugins_side_inputs_are_its_ports_by_its_ids_for_them() {
    let mut project = Project::new();
    project.add_track("Kick", TrackKind::Audio);
    let bass = project.add_track("Bass", TrackKind::Audio);
    let input = |id, name: &str| PluginInput {
        id,
        name: name.into(),
    };
    let vocoder = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Vst3,
        id: "0123456789ABCDEF0123456789ABCDEF".into(),
        name: "Vocoder".into(),
        kind: PluginKind::AudioEffect,
        inputs: vec![input(1, "Carrier"), input(3, "Aux")],
        note_output: false,
        settings: PluginSettings::default(),
    }));
    project.track_mut(bass).unwrap().devices.push(vocoder);
    let mut rig = Rig::new(project);
    rig.call(
        "run_commands",
        json!({"name": "Key", "commands": [
            {"id": "wire.connect", "args": {"device": 0, "track": 0}},
            {"id": "wire.connect", "args": {"device": 0, "input": 3, "track": 0, "tap": "pre_fx",
                                             "gain": 0.5}},
        ]}),
    )
    .unwrap();

    // Without an input, the first.
    let project = rig.call("get_project", json!({})).unwrap();
    let post_fx = json!({"track": 0, "tap": "post_fx", "name": "Kick Post FX"});
    let pre_fx = json!({"track": 0, "tap": "pre_fx", "name": "Kick Pre FX"});
    let carrier = json!({"device": 0, "port": "input", "input": 1,
                         "name": "Vocoder Carrier on Bass"});
    let aux = json!({"device": 0, "port": "input", "input": 3, "name": "Vocoder Aux on Bass"});
    assert_eq!(
        project["wires"],
        json!([
            {"id": 0, "from": post_fx, "to": carrier, "gain": 1.0},
            {"id": 1, "from": pre_fx, "to": aux, "gain": 0.5},
        ])
    );
    let vocoder = rig.call("get_device", json!({"device": 0})).unwrap();
    assert_eq!(
        vocoder["ports"],
        json!([
            {"port": "input", "input": 1, "name": "Carrier", "wires": [
                {"id": 0, "from": post_fx, "gain": 1.0},
            ]},
            {"port": "input", "input": 3, "name": "Aux", "wires": [
                {"id": 1, "from": pre_fx, "gain": 0.5},
            ]},
        ])
    );
    // Only into the inputs it lists.
    let into = |input: i64| {
        json!({"id": "wire.connect", "args": {"device": 0, "input": input, "track": 0,
                                              "tap": "post_fader"}})
    };
    assert_eq!(
        rig.call("run_command", into(2)),
        Err("Vocoder has no side input 2.".into())
    );
}

#[test]
fn reads_give_note_wires_with_their_channels_and_each_instrument_s_note_input() {
    let mut rig = Rig::new(Project::new());
    rig.call(
        "run_commands",
        json!({"name": "Set Up", "commands": [
            {"id": "track.new_midi_track"},
            {"id": "track.rename", "args": {"track": 0, "name": "Keys"}},
            {"id": "track.new_midi_track"},
            {"id": "track.rename", "args": {"track": 1, "name": "Strings"}},
            {"id": "device.add", "args": {"kind": "synth", "track": 1}},
            {"id": "mixer.set_midi_to", "args": {"track": 0, "device": 0, "channel": 2}},
        ]}),
    )
    .unwrap();

    let project = rig.call("get_project", json!({})).unwrap();
    let keys = json!({"track": 0, "tap": "post_fx", "name": "Keys Post FX"});
    let to = json!({"device": 0, "port": "notes", "notes": true, "name": "Synth on Strings"});
    assert_eq!(
        project["wires"],
        json!([{"id": 0, "from": keys, "to": to, "channel": 2}])
    );
    let synth = rig.call("get_device", json!({"device": 0})).unwrap();
    assert_eq!(
        synth["ports"],
        json!([{"port": "notes", "notes": true, "name": "Notes", "wires": [
            {"id": 0, "from": keys, "channel": 2},
        ]}])
    );
    // Without a channel, it has none to list.
    let unset = json!({"id": "wire.set_channel", "args": {"wire": 0}});
    rig.call("run_command", unset).unwrap();
    let project = rig.call("get_project", json!({})).unwrap();
    assert_eq!(project["wires"], json!([{"id": 0, "from": keys, "to": to}]));
    // The Strings play their own synth already.
    let itself = json!({"id": "mixer.set_midi_to", "args": {"track": 1, "device": 0}});
    assert_eq!(
        rig.call("run_command", itself),
        Err("Synth is on Strings itself, which plays it already.".into())
    );
}

#[test]
fn an_audio_clip_gives_its_warp_with_each_mode_s_settings() {
    let mut project = Project::new();
    let track = project.add_track("Voice", TrackKind::Audio);
    let mut audio = AudioClip::new("Voice.wav");
    let warping = Warping {
        offset: Offset::Beats(Beats::ZERO),
        warp: Some(Warp::new(0, Beats::ZERO, 120.0)),
    };
    assert!(audio.set_warping(warping));
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    let clip = project.new_clip("Voice", range, ClipContent::Audio(audio));
    project.track_mut(track).unwrap().clips.push(clip);
    let mut rig = Rig::new(project);
    rig.call(
        "run_commands",
        json!({"name": "Texture", "commands": [
            {"id": "warp.set_mode", "args": {"clip": 0, "mode": "granular"}},
            {"id": "warp.set_follow_pitch", "args": {"clip": 0, "on": false}},
            {"id": "warp.set_grain_size", "args": {"clip": 0, "milliseconds": 65}},
            {"id": "warp.set_fluctuation", "args": {"clip": 0, "fluctuation": 60}},
        ]}),
    )
    .unwrap();
    let clip = rig.call("get_clip", json!({"clip": 0})).unwrap();
    let warp = &clip["warp"];
    assert_eq!(warp["mode"], "granular");
    assert_eq!(
        warp["granular"],
        json!({"follow_pitch": false, "grain_size": 65.0, "fluctuation": 60.0})
    );
    // The other modes' settings stay as they were.
    assert_eq!(
        warp["spectral"],
        json!({"high_quality": false, "formants": 100.0, "envelope": 128.0})
    );
    assert_eq!(warp["slice"]["preserve"], "transients");
    // None until analysis finds them.
    assert_eq!(warp["transients"], Json::Null);
}

#[test]
fn the_performance_says_what_each_device_costs_the_cpu() {
    let mut project = Project::new();
    let bass = project.add_track("Bass", TrackKind::Audio);
    let drive = project.new_device(DeviceKind::TestGain { gain: 0.5 });
    let id = drive.id;
    project.track_mut(bass).unwrap().devices.push(drive);
    let mut rig = Rig::new(project);
    // Without a device playing, there's nothing to say.
    let nothing = rig.call("get_performance", json!({})).unwrap();
    assert_eq!(
        nothing,
        json!({
            "playing": false,
            "audio": "",
            "load": null,
            "dropouts": 0,
            "render_ahead": null,
            "total": 0.0,
            "rows": [],
        })
    );
    let row = |part, average, longest| CpuRow {
        part,
        average,
        longest,
        ahead: false,
    };
    rig.window.performance = Performance {
        playing: true,
        audio: "48 kHz · 128 samples · 2.7 ms".into(),
        load: Some(0.25),
        dropouts: 2,
        render_ahead: None,
        rows: vec![
            row(CpuPart::Device(id), 0.342_17, 0.61),
            row(CpuPart::Mixer(Chain::Track(bass)), 0.004, 0.012),
            row(CpuPart::Modulators(Chain::Master), 0.001, 0.002),
            // A track and a device deleted since.
            row(CpuPart::Clips(TrackId(9)), 0.1, 0.1),
            row(CpuPart::Device(DeviceId(9)), 0.1, 0.1),
            row(CpuPart::Metronome, 0.0, 0.0),
        ],
        total: 0.547,
    };
    let performance = rig.call("get_performance", json!({})).unwrap();
    assert_eq!(
        performance,
        json!({
            "playing": true,
            "audio": "48 kHz · 128 samples · 2.7 ms",
            "load": 0.25,
            "dropouts": 2,
            "render_ahead": null,
            "total": 0.547,
            "rows": [
                {"device": id.0, "name": "Device", "chain": {"track": bass.0}, "renders": "live", "average": 0.342, "longest": 0.61},
                {"part": "mixer", "chain": {"track": bass.0}, "renders": "live", "average": 0.004, "longest": 0.012},
                {"part": "modulators", "chain": "master", "renders": "live", "average": 0.001, "longest": 0.002},
                {"part": "metronome", "renders": "live", "average": 0.0, "longest": 0.0},
            ],
        })
    );
    // Rendering ahead, a block has only to be ready before the lead runs
    // out, so how long one took doesn't matter.
    rig.window.performance = Performance {
        render_ahead: Some(RenderingAhead {
            setting: "normal",
            ahead: true,
            lead_ms: 48.26,
            target_ms: 50.0,
            load: 0.346_8,
            short_blocks: 3,
        }),
        rows: vec![CpuRow {
            ahead: true,
            ..row(CpuPart::Device(id), 0.342_17, 0.61)
        }],
        ..rig.window.performance.clone()
    };
    let performance = rig.call("get_performance", json!({})).unwrap();
    assert_eq!(
        performance["render_ahead"],
        json!({
            "setting": "normal",
            "ahead": true,
            "lead_ms": 48.3,
            "target_ms": 50.0,
            "load": 0.347,
            "short_blocks": 3,
        })
    );
    assert_eq!(
        performance["rows"],
        json!([
            {"device": id.0, "name": "Device", "chain": {"track": bass.0}, "renders": "ahead", "average": 0.342, "longest": null},
        ])
    );
    assert_eq!(
        rig.call("get_performance", json!({"track": 1})),
        Err("get_performance takes no argument called track.".into())
    );
}

#[test]
fn the_status_says_what_plays_and_whats_under_way() {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let mut rig = Rig::new(project);
    // A window with nothing under way is idle.
    let status = rig.call("get_status", json!({})).unwrap();
    assert_eq!(
        status,
        json!({
            "playing": false,
            "recording": false,
            "playhead": 0,
            "playhead_seconds": 0.0,
            "takes": [],
            "question": null,
            "idle": true,
            "analysing": [],
            "rendering_segments": 0,
            "compiling": false,
            "export": null,
            "scanning_plugins": false,
            "library": {"scanning": [], "reading": [], "finding_presets": false},
        })
    );
    rig.window.status = Status {
        playing: true,
        recording: true,
        playhead: Beats::from_ratio(19, 2),
        seconds: 4.750_02,
        takes: vec![(
            vocal,
            BeatRange::new(Beats::new(8), Beats::from_ratio(19, 2)),
        )],
        question: Some("Save the changes to Song before quitting?".into()),
        analysing: vec!["Samples/kick.wav".into()],
        rendering: 3,
        compiling: true,
        export: Some(Exporting {
            stage: "rendering",
            pass: 1,
            passes: 2,
            done: 0.237_6,
            speed: Some(12.345),
        }),
        scanning_plugins: true,
        scanning_folders: vec![("/Samples".into(), 120)],
        reading_folders: vec![("/Loops".into(), 7)],
        finding_presets: true,
    };
    let status = rig.call("get_status", json!({})).unwrap();
    assert_eq!(
        status,
        json!({
            "playing": true,
            "recording": true,
            "playhead": 9.5,
            "playhead_seconds": 4.75,
            "takes": [{"track": vocal.0, "start": 8, "end": 9.5}],
            "question": "Save the changes to Song before quitting?",
            "idle": false,
            "analysing": ["Samples/kick.wav"],
            "rendering_segments": 3,
            "compiling": true,
            "export": {"stage": "rendering", "pass": 1, "passes": 2, "done": 0.238, "speed": 12.3},
            "scanning_plugins": true,
            "library": {
                "scanning": [{"folder": "/Samples", "found": 120}],
                "reading": [{"folder": "/Loops", "left": 7}],
                "finding_presets": true,
            },
        })
    );
    // Each piece of work alone keeps it busy.
    let busy: [fn(&mut Status); 8] = [
        |status| status.analysing = vec!["a.wav".into()],
        |status| status.rendering = 1,
        |status| status.compiling = true,
        |status| status.export = Some(Exporting::default()),
        |status| status.scanning_plugins = true,
        |status| status.scanning_folders = vec![("/Samples".into(), 0)],
        |status| status.reading_folders = vec![("/Samples".into(), 1)],
        |status| status.finding_presets = true,
    ];
    for make_busy in busy {
        rig.window.status = Status::default();
        make_busy(&mut rig.window.status);
        let status = rig.call("get_status", json!({})).unwrap();
        assert_eq!(status["idle"], false, "{status}");
    }
    assert_eq!(
        rig.call("get_status", json!({"track": 1})),
        Err("get_status takes no argument called track.".into())
    );
}

#[test]
fn agents_read_the_messages_and_clear_them() {
    let mut rig = Rig::new(Project::new());
    assert_eq!(
        rig.call("get_messages", json!({})).unwrap(),
        json!({"messages": []})
    );
    rig.window.messages = vec![
        "Couldn't find the audio file kick.wav.".into(),
        "The plugin Surge XT is missing.".into(),
    ];
    let messages = json!({"messages": [
        "Couldn't find the audio file kick.wav.",
        "The plugin Surge XT is missing.",
    ]});
    // Reading leaves them, until clear says to clear them.
    assert_eq!(rig.call("get_messages", json!({})).unwrap(), messages);
    let cleared = rig.call("get_messages", json!({"clear": true}));
    assert_eq!(cleared.unwrap(), messages);
    assert!(rig.window.messages.is_empty());
    assert_eq!(
        rig.call("get_messages", json!({"clear": false})).unwrap(),
        json!({"messages": []})
    );
    assert_eq!(
        rig.call("get_messages", json!({"clear": "yes"})),
        Err("clear must be true or false.".into())
    );
}

#[test]
fn a_call_the_agent_gave_up_on_runs_nothing() {
    let mut window = TestWindow::new(Project::new());
    let (reply, answer_waiting) = Reply::new("tests".into());
    let id = musicable_commands::track::NEW_MIDI_TRACK;
    let call = Call {
        asked: crate::Asked::Run(Run::One(id, Default::default())),
        reply,
    };
    drop(answer_waiting);
    answer(call, &mut window);
    assert!(window.document.project().tracks.is_empty());
    assert!(window.agents.is_empty());
}

/// A folder of its own for a test, removed when it ends. Short, as a
/// socket's path can't be long.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temporary folder is writable");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_bridge_reaches_musicable_through_the_pipe_whenever_it_listens() {
    let dir = TempDir::new("bridge");
    let address = Address::new(&dir.0).unwrap();
    let mut window = TestWindow::new(Project::new());
    let start = |window: &TestWindow| {
        Host::start(address.clone(), &window.commands, || {}).expect("the host listens")
    };
    let host = start(&window);
    // A second Musicable can't listen there too.
    let taken = Host::start(address.clone(), &window.commands, || {});
    let error = taken.err().expect("the address is taken");
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);

    let runtime = runtime();
    let (agent, bridged) = duplex(1 << 16);
    let (input, output) = tokio::io::split(bridged);
    let bridge = Bridge::new(Ok(address.clone()));
    runtime.spawn(bridge::serve(bridge, input, output));
    let client = (runtime.block_on(client_called("claude-code").serve(agent)))
        .expect("the agent connects to the bridge");

    let mut host = Some(host);
    let run = |host: &Option<Host>, window: &mut TestWindow| {
        let result = call(
            &runtime,
            &client,
            "run_command",
            json!({"id": "track.new_audio_track"}),
            || {
                for call in host.iter().flat_map(Host::calls) {
                    answer(call, window);
                }
            },
        );
        read(result)
    };
    assert_eq!(
        run(&host, &mut window),
        Ok(json!({"result": 0, "undo": "New Audio Track"}))
    );
    // Musicable knows the agent by its client's name.
    assert_eq!(host.as_ref().unwrap().agents(), ["claude-code"]);
    assert_eq!(window.agents, ["claude-code"]);

    // Once Musicable stops letting agents in, the tools say so.
    host = None;
    let not_running = run(&host, &mut window).unwrap_err();
    assert!(
        not_running.starts_with("Musicable isn't running"),
        "{not_running}"
    );
    // And once it lets them in again, the bridge reaches it again.
    host = Some(start(&window));
    assert_eq!(
        run(&host, &mut window),
        Ok(json!({"result": 1, "undo": "New Audio Track"}))
    );
    assert_eq!(window.document.project().tracks.len(), 2);
}

/// Tools the bridge runs itself, for the tests: `audio_echo` gives back its
/// arguments, and `audio_wait` says how far it's got until it's cancelled,
/// then says it stopped on `stopped`.
struct BridgeTools {
    stopped: Mutex<mpsc::Sender<()>>,
}

impl BridgeTools {
    fn new() -> (Arc<Self>, Receiver<()>) {
        let (stopped, said) = mpsc::channel();
        let tools = Self {
            stopped: Mutex::new(stopped),
        };
        (Arc::new(tools), said)
    }
}

impl LocalTools for BridgeTools {
    fn tools(&self) -> Vec<LocalTool> {
        let tool = |name: &str, read_only| {
            let Json::Object(schema) = json!({"type": "object"}) else {
                unreachable!("it's an object");
            };
            LocalTool {
                name: name.to_owned(),
                title: name.to_owned(),
                description: format!("{name}, for the tests."),
                input_schema: schema.clone(),
                output_schema: schema,
                read_only,
            }
        };
        vec![tool("audio_echo", true), tool("audio_wait", false)]
    }

    fn instructions(&self) -> String {
        "The audio_ tools are the bridge's own.".to_owned()
    }

    fn call(
        &self,
        name: &str,
        arguments: &Map<String, Json>,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(f64),
    ) -> Result<Json, String> {
        match name {
            "audio_echo" => Ok(Json::Object(arguments.clone())),
            _ => {
                let mut done = 0.0;
                while !cancelled() {
                    progress(done);
                    done = (done + 0.01f64).min(1.0);
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = self.stopped.lock().unwrap().send(());
                Err("Cancelled.".to_owned())
            }
        }
    }
}

/// An agent's client that passes on the progress its calls report.
struct Watching {
    progress: tokio::sync::mpsc::UnboundedSender<ProgressNotificationParam>,
}

impl ClientHandler for Watching {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.progress.send(params);
    }

    fn get_info(&self) -> ClientConfig {
        client_called("watching")
    }
}

#[test]
fn the_bridge_runs_its_own_tools_with_musicable_closed() {
    let dir = TempDir::new("own");
    let (tools, _stopped) = BridgeTools::new();
    // Nothing listens there.
    let bridge = Bridge::with_local(Ok(Address::new(&dir.0).unwrap()), tools);
    let runtime = runtime();
    let (agent, bridged) = duplex(1 << 16);
    let (input, output) = tokio::io::split(bridged);
    runtime.spawn(bridge::serve(bridge, input, output));
    let client = (runtime.block_on(client_called("claude-code").serve(agent)))
        .expect("the agent connects to the bridge");

    // They come after the app's tools, as read-only as they are.
    let listed = runtime.block_on(client.list_all_tools()).unwrap();
    let names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();
    let app = tools::list().len();
    assert_eq!(names.len(), app + 2, "{names:?}");
    assert_eq!(names[app..], ["audio_echo", "audio_wait"]);
    let read_only = |i: usize| (listed[i].annotations.as_ref()).and_then(|a| a.read_only_hint);
    assert_eq!(
        (read_only(app), read_only(app + 1)),
        (Some(true), Some(false))
    );
    assert!(listed[app].output_schema.is_some());
    let server = client.peer_info().expect("the server introduced itself");
    let instructions = server.instructions.as_deref().unwrap_or_default();
    assert!(instructions.contains("undo"), "{instructions}");
    assert!(
        instructions.ends_with("The audio_ tools are the bridge's own."),
        "{instructions}"
    );

    // They run with nothing on the pipe, and give back structured JSON.
    let arguments = json!({"file": "/music/a.wav"});
    let echoed = call(&runtime, &client, "audio_echo", arguments.clone(), || {});
    assert_eq!(echoed.structured_content, Some(arguments.clone()));
    assert_eq!(read(echoed), Ok(arguments));
    // While the app's tools still ask for Musicable.
    let refused = read(call(&runtime, &client, "get_project", json!({}), || {}));
    let refused = refused.unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");
}

#[test]
fn a_tool_of_the_bridge_s_says_how_far_it_has_got_and_stops_when_cancelled() {
    let (tools, stopped) = BridgeTools::new();
    let bridge = Bridge::with_local(Err("Nowhere to listen.".to_owned()), tools);
    let runtime = runtime();
    let (agent, bridged) = duplex(1 << 16);
    let (input, output) = tokio::io::split(bridged);
    runtime.spawn(bridge::serve(bridge, input, output));
    let (progress, mut reported) = tokio::sync::mpsc::unbounded_channel();
    let client = (runtime.block_on(Watching { progress }.serve(agent)))
        .expect("the agent connects to the bridge");

    let request = CallToolRequestParams::new("audio_wait".to_owned());
    let first = runtime.block_on(async {
        let wait = client.call_tool_once(request);
        tokio::select! {
            _ = wait => panic!("the call ended before it was cancelled"),
            first = reported.recv() => first.expect("it reports progress"),
        }
        // Leaving the call cancels it.
    });
    assert_eq!(first.total, Some(1.0));
    assert!((0.0..=1.0).contains(&first.progress), "{first:?}");
    stopped
        .recv_timeout(Duration::from_secs(10))
        .expect("the tool stops once the call is cancelled");
}

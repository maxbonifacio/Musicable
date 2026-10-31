//! Agents controlling the app, as their clients do through `musicable
//! mcp`: once the user lets them in on the Agents page of Settings, their
//! commands run as a menu's do, each a step to undo, with the status bar
//! saying what they last changed. Files they give skip the dialogs, and New
//! and Open still ask the user about unsaved changes.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::options;
use musicable_io::audio::Decoder;
use musicable_io::project;
use musicable_mcp::bridge::{self, Bridge};
use musicable_mcp::pipe::Address;
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackKind};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::{RunningService, ServiceError};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use super::App;
use super::testing::{TempDir, window};
use crate::settings::{SETTINGS_FILE, Settings};

type Window = Harness<'static, App>;

/// An agent's client, connected to the app through the bridge, as `musicable
/// mcp` connects it, calling itself `claude-code`.
struct Agent {
    runtime: Runtime,
    client: RunningService<RoleClient, ClientConfig>,
}

impl Agent {
    /// Connects to the app whose settings are in `config_dir`, through the
    /// bridge, over an in-memory stream in place of standard input and
    /// output.
    fn connect(config_dir: &Path) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let address = Address::new(config_dir).map_err(|error| error.to_string());
        let (input, output) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(input);
        runtime.spawn(bridge::serve(Bridge::new(address), read, write));
        let client = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "2.0"),
        );
        let client = runtime.block_on(client.serve(output)).unwrap();
        Self { runtime, client }
    }

    /// Starts calling the tool `name` with `arguments`.
    fn start(
        &self,
        name: &str,
        arguments: Json,
    ) -> JoinHandle<Result<CallToolResult, ServiceError>> {
        let Json::Object(arguments) = arguments else {
            panic!("a tool's arguments are an object");
        };
        let request = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        let peer = self.client.peer().clone();
        self.runtime
            .spawn(async move { peer.call_tool(request).await })
    }

    /// Calls the tool `name` with `arguments`, with the window's frames
    /// going on meanwhile. Gives back the JSON it answered, or why it
    /// failed.
    fn call(&self, window: &mut Window, name: &str, arguments: Json) -> Result<Json, String> {
        let call = self.start(name, arguments);
        self.answer(window, call)
    }

    /// What the call answers, with the window's frames going on until it
    /// does.
    fn answer(
        &self,
        window: &mut Window,
        call: JoinHandle<Result<CallToolResult, ServiceError>>,
    ) -> Result<Json, String> {
        let started = Instant::now();
        while !call.is_finished() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the call got no answer"
            );
            window.step();
            std::thread::sleep(Duration::from_millis(2));
        }
        window.run();
        let result = self.runtime.block_on(call).unwrap();
        read(result.expect("the call gets an answer"))
    }
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

/// Steps the window until `done` says so.
fn wait_for(window: &mut Window, what: &str, done: impl Fn(&App) -> bool) {
    let started = Instant::now();
    while !done(window.state()) {
        assert!(started.elapsed() < Duration::from_secs(30), "{what} never");
        window.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    window.run();
}

/// Switches "Let agents control Musicable" on the Agents page of Settings.
fn switch_agents(window: &mut Window) {
    window.state_mut().session.run(&options::AGENTS).unwrap();
    window.run();
    window.get_by_label("Let agents control Musicable").click();
    window.run();
}

/// Two beats of a chord on a test instrument.
fn song() -> Project {
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let mut chord = MidiClip::default();
    for key in [60, 64, 67] {
        chord.add_note(Beats::ZERO, Beats::new(1), key, 0.8);
    }
    let range = BeatRange::new(Beats::ZERO, Beats::new(2));
    let clip = song.new_clip("Chord", range, ClipContent::Midi(chord));
    let instrument = song.new_device(DeviceKind::TestInstrument { level: 0.3 });
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    song
}

#[test]
fn agents_control_the_app_once_the_user_lets_them_in() {
    let dir = TempDir::new("agents");
    let mut window = window(&dir.0, None);
    let agent = Agent::connect(&dir.0);
    let refused = agent
        .call(&mut window, "get_project", json!({}))
        .unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");

    switch_agents(&mut window);
    window.get_by_label("Listening for agents. None is connected.");
    let kept = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(kept.agents.enabled);

    // The agent connects at its next call, and the status bar says so.
    let project = agent.call(&mut window, "get_project", json!({})).unwrap();
    assert_eq!(project["tracks"], json!([]));
    window.get_by_label("Connected: claude-code.");
    window.get_by_label("Agent connected");

    // Its commands run as a menu's do, each a step to undo.
    let track = agent.call(
        &mut window,
        "run_command",
        json!({"id": "track.new_midi_track"}),
    );
    assert_eq!(track, Ok(json!({"result": 0, "undo": "New MIDI Track"})));
    assert_eq!(window.state().session.project().tracks.len(), 1);
    window.get_by_label("Agent: New MIDI Track");
    let undone = agent.call(&mut window, "run_command", json!({"id": "edit.undo"}));
    assert_eq!(undone, Ok(json!({"result": null})));
    assert!(window.state().session.project().tracks.is_empty());
    window.get_by_label("Agent: Undo");
    let beat = agent.call(
        &mut window,
        "run_commands",
        json!({
            "name": "Start a Beat",
            "commands": [
                {"id": "track.new_midi_track"},
                {"id": "track.new_midi_track"},
            ],
        }),
    );
    // Ids aren't used again, even for what was undone.
    assert_eq!(beat, Ok(json!({"results": [1, 2], "undo": "Start a Beat"})));
    assert_eq!(window.state().session.undo_name(), Some("Start a Beat"));

    // Files it gives skip the dialogs.
    let saved = agent.call(
        &mut window,
        "run_command",
        json!({"id": "file.save_as", "args": {"file": dir.0.join("Beat")}}),
    );
    let file = dir.0.join("Beat.musicable");
    assert_eq!(saved, Ok(json!({"result": file})));
    assert!(!window.state().session.is_modified());
    assert_eq!(project::load(&file).unwrap().tracks.len(), 2);
    let relative = agent.call(
        &mut window,
        "run_command",
        json!({"id": "file.save_as", "args": {"file": "Beat 2"}}),
    );
    let relative = relative.unwrap_err();
    assert!(
        relative.starts_with("Beat 2 isn't a full path"),
        "{relative}"
    );

    window.get_by_label("Agent: Save Project As");

    // Switched off, the app lets it in no more.
    window.state_mut().settings_window.open(None);
    window.run();
    window.get_by_label("Let agents control Musicable").click();
    window.run();
    window.get_by_label("Off: Musicable doesn't listen for agents.");
    assert!(window.query_by_label("Agent: Save Project As").is_none());
    let refused = agent
        .call(&mut window, "get_project", json!({}))
        .unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");
}

#[test]
fn opening_asks_the_user_about_unsaved_changes_first() {
    let dir = TempDir::new("agents-open");
    let file = dir.0.join("Song.musicable");
    project::save(&file, &song()).unwrap();
    let mut window = window(&dir.0, None);
    switch_agents(&mut window);
    let agent = Agent::connect(&dir.0);
    let track = json!({"id": "track.new_audio_track"});
    agent.call(&mut window, "run_command", track).unwrap();
    assert!(window.state().session.is_modified());

    // The call waits for the user's answer, and other commands wait too.
    let open = json!({"id": "file.open", "args": {"file": file}});
    let call = agent.start("run_command", open.clone());
    wait_for(&mut window, "the question", |app| app.pending.is_some());
    window.get_by_label("Save the changes to Untitled?");
    // Reads go on, and the status says what the window asks.
    let status = agent.call(&mut window, "get_status", json!({})).unwrap();
    assert_eq!(
        status["question"],
        "Save the changes to Untitled before opening another project?"
    );
    let meanwhile = agent.call(
        &mut window,
        "run_command",
        json!({"id": "track.new_midi_track"}),
    );
    let meanwhile = meanwhile.unwrap_err();
    assert!(meanwhile.contains("waiting for the user"), "{meanwhile}");
    window.get_by_label("Cancel").click();
    let kept = agent.answer(&mut window, call).unwrap_err();
    assert_eq!(
        kept,
        "The user kept the project open, with its unsaved changes."
    );
    assert_eq!(window.state().session.project().tracks.len(), 1);

    let call = agent.start("run_command", open);
    wait_for(&mut window, "the question", |app| app.pending.is_some());
    window.get_by_label("Don't Save").click();
    let opened = agent.answer(&mut window, call);
    assert_eq!(opened, Ok(json!({"result": file})));
    assert_eq!(window.state().session.project().tracks[0].name, "Keys");
    assert_eq!(window.state().path.as_deref(), Some(file.as_path()));

    // An export says where it went, once it's written.
    let mix = dir.0.join("Mix.flac");
    let exported = agent.call(
        &mut window,
        "run_command",
        json!({"id": "file.export_audio", "args": {"file": mix}}),
    );
    assert_eq!(exported, Ok(json!({"result": [mix]})));
    let info = Decoder::open(&mix).unwrap().info();
    assert_eq!((info.sample_rate, info.frames), (48_000, 48_000));
    // The user's export settings stay as they were.
    let settings = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap();
    assert_eq!(settings.export, Default::default());
    let mp3 = agent.call(
        &mut window,
        "run_command",
        json!({"id": "file.export_audio", "args": {"file": dir.0.join("Mix.mp3")}}),
    );
    assert_eq!(
        mp3,
        Err("Musicable exports WAV, AIFF and FLAC files, not .mp3 files.".to_owned())
    );
    assert!(fs::metadata(dir.0.join("Mix.mp3")).is_err());
}

#[test]
fn agents_read_what_plays_and_what_the_window_says() {
    let dir = TempDir::new("agents-status");
    let mut window = window(&dir.0, None);
    switch_agents(&mut window);
    let agent = Agent::connect(&dir.0);
    let status = agent.call(&mut window, "get_status", json!({})).unwrap();
    assert_eq!(status["playing"], false, "{status}");
    assert_eq!(status["takes"], json!([]), "{status}");
    assert_eq!(status["question"], Json::Null, "{status}");

    // The messages the status bar shows, until an agent clears them.
    window.state_mut().messages.clear();
    let missing = "Couldn't find the audio file kick.wav.".to_owned();
    window.state_mut().messages.push(missing.clone());
    window.run();
    window.get_by_label(&missing);
    let read = agent.call(&mut window, "get_messages", json!({})).unwrap();
    assert_eq!(read, json!({"messages": [missing]}));
    let cleared = agent.call(&mut window, "get_messages", json!({"clear": true}));
    assert_eq!(cleared.unwrap(), read);
    assert!(window.state().messages.is_empty());
    assert!(window.query_by_label(&missing).is_none());
}

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
use musicable_commands::editor::PlayFrom;
use musicable_commands::options;
use musicable_io::audio::Decoder;
use musicable_io::project;
use musicable_mcp::bridge::{self, Bridge};
use musicable_mcp::pipe::Address;
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::plugin::{PluginDevice, PluginKind, PluginSettings};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, PluginFormat, Project, TrackKind};
use musicable_ui::PluginEntry;
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

#[test]
fn agents_change_settings_and_hear_what_they_were() {
    let dir = TempDir::new("agents-settings");
    let mut window = window(&dir.0, None);
    switch_agents(&mut window);
    let agent = Agent::connect(&dir.0);
    let read = agent.call(&mut window, "get_settings", json!({})).unwrap();
    assert_eq!(
        read["settings"]["audio"]["render_ahead"], "normal",
        "{read}"
    );
    assert_eq!(read["profile"], "Default", "{read}");
    assert_eq!(read["playing"], Json::Null, "{read}");
    let changeable = read["changeable"].as_array().unwrap();
    assert!(changeable.contains(&json!("audio.render_ahead")), "{read}");
    assert!(!changeable.contains(&json!("agents.enabled")), "{read}");

    // Render Ahead, as its own command, and the status bar says so.
    let long = json!({"id": "options.set_render_ahead", "args": {"render_ahead": "long"}});
    let changed = agent.call(&mut window, "run_command", long).unwrap();
    assert_eq!(
        changed,
        json!({"result": {"key": "audio.render_ahead", "was": "normal", "now": "long"}})
    );
    window.get_by_label("Agent: Set Render Ahead");
    let kept = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap();
    assert_eq!(kept.audio.render_ahead, crate::settings::RenderAhead::Long);

    // Any setting agents may change, by its name in the file.
    let set =
        |key: &str, value: &str| json!({"id": "options.set", "args": {"key": key, "value": value}});
    let capture = agent.call(
        &mut window,
        "run_command",
        set("recording.capture_minutes", "10"),
    );
    assert_eq!(
        capture.unwrap()["result"],
        json!({"key": "recording.capture_minutes", "was": 5, "now": 10})
    );
    let play_from = set("behaviour.play_from", "playhead");
    let play_from = agent.call(&mut window, "run_command", play_from).unwrap();
    assert_eq!(play_from["result"]["was"], "start_marker");
    assert_eq!(
        window.state().session.editor().behaviour.play_from,
        PlayFrom::Playhead
    );
    let rate = agent.call(
        &mut window,
        "run_command",
        set("export.sample_rate", "44100"),
    );
    assert_eq!(rate.unwrap()["result"]["was"], Json::Null);
    assert_eq!(window.state().settings.export.sample_rate, Some(44_100));
    let rate = agent.call(
        &mut window,
        "run_command",
        set("export.sample_rate", "none"),
    );
    assert_eq!(rate.unwrap()["result"]["now"], Json::Null);

    // What isn't a setting, isn't theirs to change, or can't be so, fails
    // and changes nothing.
    for (key, value, starts) in [
        (
            "audio.colour",
            "red",
            "There's no setting called audio.colour",
        ),
        (
            "agents.enabled",
            "false",
            "There's no setting called agents.enabled",
        ),
        (
            "render_ahead",
            "long",
            "There's no setting called render_ahead",
        ),
        (
            "analysis.fast",
            "maybe",
            "analysis.fast can't be maybe. It's true or false.",
        ),
        (
            "audio.render_ahead",
            "huge",
            "audio.render_ahead can't be huge.",
        ),
        (
            "browser.preview_volume",
            "40",
            "browser.preview_volume must be from",
        ),
    ] {
        let why = agent.call(&mut window, "run_command", set(key, value));
        let why = why.unwrap_err();
        assert!(why.starts_with(starts), "{key}: {why}");
    }
    assert!(window.state().settings.agents.enabled);
    assert_eq!(
        window.state().settings.audio.render_ahead,
        crate::settings::RenderAhead::Long
    );

    // A device that isn't there leaves the one playing.
    let device = json!({"id": "options.set_audio_device", "args": {"device": "No Such Card"}});
    let why = agent.call(&mut window, "run_command", device).unwrap_err();
    assert!(
        why.contains("No Such Card") || why.contains("couldn't be listed"),
        "{why}"
    );
    let nothing = json!({"id": "options.set_audio_device", "args": {}});
    let why = agent.call(&mut window, "run_command", nothing).unwrap_err();
    assert!(why.starts_with("Say what to change"), "{why}");

    // The devices there are, whatever this computer has.
    let devices = agent.call(&mut window, "list_audio_devices", json!({}));
    let devices = devices.unwrap();
    assert!(devices["outputs"].is_array(), "{devices}");
    assert!(devices["inputs"].is_array(), "{devices}");
    assert_eq!(devices["driver_types"][0], "system", "{devices}");
    let reset = json!({"id": "transport.reset_dropouts"});
    assert_eq!(
        agent.call(&mut window, "run_command", reset),
        Ok(json!({"result": null}))
    );
}

#[test]
fn agents_read_the_plugins_the_layout_and_the_meters() {
    let dir = TempDir::new("agents-reads");
    let file = dir.0.join("Song.musicable");
    let mut song = song();
    // A plugin this computer hasn't got.
    let missing = song.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "org.example.missing".into(),
        name: "Missing".into(),
        kind: PluginKind::AudioEffect,
        inputs: Vec::new(),
        note_output: false,
        settings: PluginSettings::default(),
    }));
    let missing_id = missing.id.0;
    song.tracks[0].devices.push(missing);
    project::save(&file, &song).unwrap();
    let mut window = window(&dir.0, Some(file));
    switch_agents(&mut window);
    let agent = Agent::connect(&dir.0);

    // The plugins the browser lists, all of them or those a filter finds.
    let entry = |name: &str, vendor: &str, kind| PluginEntry {
        format: PluginFormat::Clap,
        id: format!("org.{}.{}", vendor.to_lowercase(), name.to_lowercase()),
        name: name.into(),
        vendor: vendor.into(),
        kind,
        never_ahead: false,
    };
    window.state_mut().plugin_entries = vec![
        entry("Surge", "Surge", PluginKind::Instrument),
        PluginEntry {
            never_ahead: true,
            ..entry("Delay", "Acme", PluginKind::AudioEffect)
        },
    ];
    let plugins = agent.call(&mut window, "get_plugins", json!({})).unwrap();
    assert_eq!(plugins["installed"], 2, "{plugins}");
    assert_eq!(plugins["scanning"], false, "{plugins}");
    let found = agent.call(&mut window, "get_plugins", json!({"filter": "ACME"}));
    assert_eq!(
        found.unwrap()["plugins"],
        json!([{
            "format": "clap",
            "id": "org.acme.delay",
            "name": "Delay",
            "vendor": "Acme",
            "kind": "audio_effect",
            "render_ahead": "never",
        }])
    );

    // A plugin device says whether it plays, and why not: here nothing
    // tries to load it, with the plugins never scanned.
    let device = agent.call(&mut window, "get_device", json!({"device": missing_id}));
    let device = device.unwrap();
    assert_eq!(device["loaded"], false, "{device}");
    assert_eq!(device.get("problem"), Some(&Json::Null), "{device}");
    assert_eq!(device["window_open"], false, "{device}");

    // Where the panels are, and which are in sight.
    let layout = agent.call(&mut window, "get_layout", json!({})).unwrap();
    assert_eq!(layout["workspace"], "Default", "{layout}");
    assert_eq!(layout["profile"], "Default", "{layout}");
    assert_eq!(layout["windows"], json!([]), "{layout}");
    let panels = layout["panels"].as_array().unwrap();
    let mixer = |layout: &Json| {
        let panels = layout["panels"].as_array().unwrap();
        (panels.iter())
            .find(|panel| panel["title"] == "Mixer")
            .cloned()
            .unwrap()
    };
    assert!(
        panels.iter().all(|panel| panel["window"] == "main"),
        "{layout}"
    );
    let shown = mixer(&layout)["in_sight"].as_bool().unwrap();
    let toggle = json!({"id": "view.toggle_mixer"});
    agent.call(&mut window, "run_command", toggle).unwrap();
    let layout = agent.call(&mut window, "get_layout", json!({})).unwrap();
    assert_eq!(mixer(&layout)["in_sight"], !shown, "{layout}");
    assert_eq!(mixer(&layout)["hidden"], shown, "{layout}");

    // The meters, silent with no audio device playing.
    let levels = agent.call(&mut window, "get_levels", json!({})).unwrap();
    assert_eq!(levels["playing"], false, "{levels}");
    let silent = json!({"peak": [null, null], "rms": [null, null], "highest": null});
    assert_eq!(levels["master"], silent, "{levels}");
    assert_eq!(levels["tracks"][0]["name"], "Keys", "{levels}");
    assert_eq!(levels["tracks"][0]["peak"], silent["peak"], "{levels}");
}

#[test]
fn agents_clear_peaks_change_keys_name_workspaces_and_export_stems() {
    let dir = TempDir::new("agents-shell");
    let file = dir.0.join("Song.musicable");
    project::save(&file, &song()).unwrap();
    let mut window = window(&dir.0, Some(file));
    switch_agents(&mut window);
    let agent = Agent::connect(&dir.0);
    let run = |window: &mut Window, id: &str, args: Json| {
        agent.call(window, "run_command", json!({"id": id, "args": args}))
    };

    assert_eq!(
        run(&mut window, "mixer.clear_peaks", json!({})),
        Ok(json!({"result": null}))
    );

    // Keys, as the Keys page changes them, saying what they were.
    let set = run(
        &mut window,
        "options.set_keys",
        json!({"command": "transport.play_stop", "keys": ["Ctrl+Shift+Space", "Ctrl+K"]}),
    );
    assert_eq!(
        set,
        Ok(json!({"result": {
            "command": "transport.play_stop",
            "was": ["Space"],
            "now": ["Ctrl+K", "Ctrl+Shift+Space"],
            "taken": [{"key": "Ctrl+K", "from": "options.keyboard_shortcuts"}],
        }}))
    );
    let keymap = window.state().session.keymap().clone();
    let play_stop = musicable_commands::CommandId::from("transport.play_stop");
    assert_eq!(keymap.chords_for(&play_stop).count(), 2);
    let saved = fs::read_to_string(dir.0.join(crate::settings::KEYMAP_FILE)).unwrap();
    assert!(saved.contains("Ctrl+Shift+Space"), "{saved}");
    let reset = run(
        &mut window,
        "options.reset_keys",
        json!({"command": "transport.play_stop"}),
    );
    assert_eq!(reset.unwrap()["result"]["now"], json!(["Space"]));
    let unknown = run(
        &mut window,
        "options.set_keys",
        json!({"command": "transport.nothing", "keys": []}),
    );
    assert_eq!(
        unknown,
        Err("There's no command transport.nothing.".to_owned())
    );

    // Workspaces by name, where the user would be asked for one.
    let saved = run(&mut window, "workspace.save_as", json!({"name": "Mixing"}));
    assert_eq!(saved, Ok(json!({"result": {"workspace": "Mixing"}})));
    assert_eq!(window.state().workspaces.current_name(), "Mixing");
    let renamed = run(&mut window, "workspace.rename", json!({"name": "Mixdown"}));
    assert_eq!(renamed, Ok(json!({"result": {"workspace": "Mixdown"}})));
    assert_eq!(window.state().workspaces.current_name(), "Mixdown");
    let unnamed = run(&mut window, "workspace.save_as", json!({}));
    let unnamed = unnamed.unwrap_err();
    assert!(
        unnamed.starts_with("The argument name is missing"),
        "{unnamed}"
    );

    // Stems alone, at another rate and depth, without the tail.
    let stems = dir.0.join("Stems.wav");
    let exported = run(
        &mut window,
        "file.export_audio",
        json!({
            "file": stems,
            "range": "song",
            "bits": 16,
            "sample_rate": 44100,
            "tail": false,
            "mix": false,
            "stems": true,
        }),
    );
    let keys = dir.0.join("Stems 01 Keys.wav");
    assert_eq!(exported, Ok(json!({"result": [keys]})));
    assert!(fs::metadata(&stems).is_err());
    let info = Decoder::open(&keys).unwrap().info();
    assert_eq!((info.sample_rate, info.frames), (44_100, 44_100));
    let nowhere = run(
        &mut window,
        "file.export_audio",
        json!({"file": dir.0.join("Selected.wav"), "range": "selection"}),
    );
    assert_eq!(
        nowhere,
        Err("There's nothing to export: nothing is selected.".to_owned())
    );
    let missing = run(
        &mut window,
        "file.export_audio",
        json!({"file": stems, "stems": true, "tracks": [9]}),
    );
    assert_eq!(missing, Err("There's no track 9.".to_owned()));
}

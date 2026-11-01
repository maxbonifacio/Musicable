//! `musicable --headless`, the app with no window, as agents use it: through
//! the pipe, and through `musicable mcp`'s lifecycle tools, which start it,
//! restart it and quit it. And `musicable check`, which runs the scenarios
//! under `acceptance`, each in a headless Musicable of its own: on each
//! system CI runs on.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use musicable_mcp::client::{self, Client};
use musicable_mcp::pipe::Address;
use serde_json::{Value as Json, json};

const MUSICABLE: &str = env!("CARGO_BIN_EXE_musicable");

/// A folder of its own for each test, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("musicable-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts `musicable --headless --no-audio`, with its settings in `dir`, and
/// connects to it.
fn headless(dir: &Path, project: Option<&Path>) -> (Child, Client) {
    let mut command = Command::new(MUSICABLE);
    command.args(["--headless", "--no-audio"]);
    if let Some(project) = project {
        command.arg(project);
    }
    let mut child = (command.env("MUSICABLE_CONFIG_DIR", dir))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("headless Musicable starts");
    let address = Address::new(dir).unwrap();
    let wait = Duration::from_secs(60);
    let client = Client::connect_within(&address, "test", wait, || {
        let status = child.try_wait().ok().flatten()?;
        Some(format!("it stopped: {status}"))
    })
    .expect("headless Musicable answers");
    (child, client)
}

/// Waits for `child` to end, and gives back whether it ended well.
fn ended(child: &mut Child) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("it didn't end");
}

#[test]
fn headless_musicable_lets_agents_in_and_has_them_answer_its_questions() {
    let dir = TempDir::new("headless");
    let (mut child, client) = headless(&dir.0, None);
    let status = client.call("get_status", json!({})).unwrap();
    assert_eq!(status["headless"], true, "{status}");
    assert_eq!(status["process"], child.id());

    // Its commands are the app's.
    let made = (client.call("run_command", json!({"id": "track.new_midi_track"}))).unwrap();
    assert_eq!(made["result"], 0, "{made}");
    let project = client.call("get_project", json!({})).unwrap();
    assert_eq!(project["modified"], true);

    // It has no window, and says so.
    let shot = client.call("get_screenshot", json!({}));
    assert!(shot.unwrap_err().contains("headless"));
    let focused = client.call("focus_app", json!({}));
    assert_eq!(
        focused,
        Err("Musicable runs headless, with no window.".to_owned())
    );
    // Nor a user to ask, so an agent answers what the window asks.
    assert_eq!(
        client.call("answer_question", json!({"answer": "discard"})),
        Err("The window asks nothing now.".to_owned())
    );
    let address = Address::new(&dir.0).unwrap();
    let asking = std::thread::spawn(move || {
        let other = Client::connect(&address, "other").unwrap();
        other.call("run_command", json!({"id": "file.new"}))
    });
    let question = loop {
        let status = client.call("get_status", json!({})).unwrap();
        if let Some(question) = status["question"].as_str() {
            break question.to_owned();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        question,
        "Save the changes to Untitled before starting a new project?"
    );
    let answered = client.call("answer_question", json!({"answer": "discard"}));
    assert_eq!(
        answered,
        Ok(json!({"question": question, "went_ahead": true}))
    );
    assert_eq!(asking.join().unwrap(), Ok(json!({"result": null})));
    let project = client.call("get_project", json!({})).unwrap();
    assert_eq!(project["tracks"], json!([]));

    // Quitting with changes needs to know what to do with them.
    client
        .call("run_command", json!({"id": "track.new_audio_track"}))
        .unwrap();
    let quit = client.call("quit_app", json!({}));
    assert!(quit.unwrap_err().contains("say unsaved save or discard"));
    let quit = client.call("quit_app", json!({"unsaved": "discard"}));
    assert_eq!(quit, Ok(json!({"quitting": true, "saved": false})));
    assert!(ended(&mut child), "it ends well");
    // And it lets the pipe go.
    assert!(!client::answers(&Address::new(&dir.0).unwrap()));
}

#[test]
fn headless_musicable_saves_as_it_quits_and_opens_a_project_as_it_starts() {
    let dir = TempDir::new("headless-save");
    let song = dir.0.join("song.musicable");
    let (mut child, client) = headless(&dir.0, None);
    let saved = client.call(
        "run_commands",
        json!({"name": "Start", "commands": [{"id": "track.new_midi_track", "as": "keys"}, {"id": "track.rename", "args": {"track": "$keys", "name": "Keys"}}]}),
    );
    assert!(saved.is_ok(), "{saved:?}");
    // It has no file to save to yet.
    let quit = client.call("quit_app", json!({"unsaved": "save"}));
    assert!(quit.unwrap_err().starts_with("The project has no file yet"));
    let save = json!({"id": "file.save_as", "args": {"file": song}});
    client.call("run_command", save).unwrap();
    client
        .call(
            "run_command",
            json!({"id": "transport.set_tempo", "args": {"bpm": 97}}),
        )
        .unwrap();
    let quit = client.call("quit_app", json!({"unsaved": "save"}));
    assert_eq!(quit, Ok(json!({"quitting": true, "saved": true})));
    assert!(ended(&mut child));

    let (mut child, client) = headless(&dir.0, Some(&song));
    let project = client.call("get_project", json!({})).unwrap();
    assert_eq!(project["tracks"][0]["name"], "Keys");
    assert_eq!(project["tempo"][0]["bpm"], 97.0);
    assert_eq!(project["modified"], false);
    let quit = client.call("quit_app", json!({}));
    assert_eq!(quit, Ok(json!({"quitting": true, "saved": false})));
    assert!(ended(&mut child));
}

#[test]
fn a_second_headless_musicable_with_the_same_settings_says_why_it_cant() {
    let dir = TempDir::new("headless-twice");
    let (mut first, client) = headless(&dir.0, None);
    let second = Command::new(MUSICABLE)
        .args(["--headless", "--no-audio"])
        .env("MUSICABLE_CONFIG_DIR", &dir.0)
        .output()
        .unwrap();
    assert!(!second.status.success());
    let said = String::from_utf8_lossy(&second.stderr);
    assert!(said.contains("MUSICABLE_CONFIG_DIR"), "{said}");
    client
        .call("quit_app", json!({"unsaved": "discard"}))
        .unwrap();
    assert!(ended(&mut first));
}

/// `musicable mcp`, as an agent's client starts it.
struct Bridge {
    child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
    id: u64,
}

impl Bridge {
    fn start(dir: &Path) -> Self {
        let mut child = Command::new(MUSICABLE)
            .arg("mcp")
            .env("MUSICABLE_CONFIG_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("musicable mcp starts");
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let input = child.stdin.take();
        let mut bridge = Self {
            child,
            input,
            lines,
            id: 0,
        };
        let client = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "2.0"},
        });
        bridge.request("initialize", client);
        bridge.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        bridge
    }

    fn send(&mut self, message: Json) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Json) -> Json {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "{method} got no answer"
            );
            let line = match self.lines.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => panic!("musicable mcp stopped"),
            };
            let message: Json = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Json) -> Result<Json, String> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        match result["isError"].as_bool() {
            Some(true) => Err(text),
            _ => Ok(serde_json::from_str(&text).unwrap()),
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.input = None;
        let _ = self.child.wait();
    }
}

#[test]
fn the_bridge_launches_musicable_restarts_it_and_quits_it() {
    let dir = TempDir::new("lifecycle");
    let song = dir.0.join("song.musicable");
    let mut bridge = Bridge::start(&dir.0);
    let address = Address::new(&dir.0).unwrap();
    assert!(!client::answers(&address));

    let launched = (bridge.call("launch_app", json!({"headless": true, "audio": false}))).unwrap();
    assert_eq!(launched["headless"], true, "{launched}");
    let first = launched["process"].as_u64().unwrap();
    let status = bridge.call("get_status", json!({})).unwrap();
    assert_eq!(status["process"], first);
    // Only one at a time.
    let again = bridge.call("launch_app", json!({"headless": true}));
    assert!(
        again
            .unwrap_err()
            .starts_with("Musicable is running already")
    );

    // Restarting opens the project it had open.
    let save = json!({"id": "file.save_as", "args": {"file": song}});
    bridge.call("run_command", save).unwrap();
    bridge
        .call("run_command", json!({"id": "track.new_midi_track"}))
        .unwrap();
    let restarted = bridge.call(
        "restart_app",
        json!({"headless": true, "audio": false, "unsaved": "discard"}),
    );
    let restarted = restarted.unwrap();
    let second = restarted["process"].as_u64().unwrap();
    assert_ne!(second, first);
    assert_eq!(restarted["project"], json!(song));
    let project = bridge.call("get_project", json!({})).unwrap();
    assert_eq!(project["tracks"], json!([]), "the change was let go");
    assert_eq!(project["file"], json!(song.display().to_string()));

    let quit = bridge.call("quit_app", json!({}));
    assert_eq!(quit, Ok(json!({"quitting": true, "saved": false})));
    let started = Instant::now();
    while client::answers(&address) {
        assert!(started.elapsed() < Duration::from_secs(20), "it quits");
        std::thread::sleep(Duration::from_millis(50));
    }
    let gone = bridge.call("get_status", json!({}));
    assert!(gone.unwrap_err().starts_with("Musicable isn't running"));
}

#[test]
fn the_scenarios_under_acceptance_hold() {
    let acceptance = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../acceptance");
    let scenarios = musicable_app::check::scenarios(&acceptance);
    assert!(!scenarios.is_empty(), "there are scenarios");
    let checked = Command::new(MUSICABLE)
        .arg("check")
        .args(&scenarios)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&checked.stdout);
    assert!(checked.status.success(), "{said}");
}

#[test]
fn a_render_nulls_against_its_golden_or_says_how_far_off_it_is() {
    let dir = TempDir::new("check-render");
    let scenario = dir.0.join("render.toml");
    let write = |muted: bool| {
        let text = format!(
            "project = \"kick-and-bass\"\n\n\
             [[run]]\ncommand = \"mixer.set_mute\"\nargs = {{ track = 1, muted = {muted} }}\n\n\
             [[expect]]\nrender = {{ from = 0, to = 4 }}\ngolden = \"goldens/kick.flac\"\n"
        );
        fs::write(&scenario, text).unwrap();
    };
    let check = |bless: bool| {
        let mut command = Command::new(MUSICABLE);
        command.arg("check");
        if bless {
            command.arg("--bless");
        }
        let output = command.arg(&scenario).output().unwrap();
        let said = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.success(), said)
    };
    write(true);
    let (held, said) = check(false);
    assert!(!held && said.contains("There's no golden"), "{said}");
    let (held, said) = check(true);
    assert!(held && said.contains("wrote"), "{said}");
    assert!(dir.0.join("goldens/kick.flac").is_file());
    // The same render again nulls.
    let (held, said) = check(false);
    assert!(held, "{said}");
    // With the bass playing, it doesn't.
    write(false);
    let (held, said) = check(false);
    assert!(
        !held && said.contains("doesn't null against the golden"),
        "{said}"
    );
}

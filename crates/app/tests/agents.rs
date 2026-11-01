//! The real `musicable mcp`, started as an agent's client starts it,
//! speaking MCP over its standard input and output, and reaching a session
//! through a real pipe, as it reaches the app: on each system CI runs on.
//! And the audio tools, which it runs itself, and `musicable audio`, which
//! runs them in a shell.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use musicable_app::Session;
use musicable_commands::{Editor, Keymap, Registry, Steps};
use musicable_engine::{Engine, EngineConfig, EngineHandle, rt};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_mcp::pipe::Address;
use musicable_mcp::{Host, Reply, Run, Window};
use musicable_model::Project;
use serde_json::{Value as Json, json};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// A session answering agents' calls, as the app's window does.
struct SessionWindow {
    session: Session,
}

impl SessionWindow {
    fn new(handle: EngineHandle) -> Self {
        let session = Session::new(Project::new(), None, Keymap::default_keymap(), handle);
        Self { session }
    }

    /// The name of the step the commands run since `revision` made.
    fn step_since(&self, revision: u64, run: &Run) -> Option<&str> {
        let made = self.session.revision() != revision && !run.steps_through_history();
        made.then(|| self.session.undo_name()).flatten()
    }
}

impl Window for SessionWindow {
    fn commands(&self) -> &Registry {
        self.session.commands()
    }

    fn keymap(&self) -> &Keymap {
        self.session.keymap()
    }

    fn project(&self) -> &Project {
        self.session.project()
    }

    fn editor(&self) -> &Editor {
        self.session.editor()
    }

    fn history(&self) -> Steps<'_> {
        self.session.history()
    }

    fn file(&self) -> Option<&Path> {
        None
    }

    fn is_modified(&self) -> bool {
        self.session.is_modified()
    }

    fn run(&mut self, run: Run, reply: Reply) {
        let revision = self.session.revision();
        match &run {
            Run::One(id, args) => {
                let result = self.session.run_with(id, args);
                reply.ran(result, self.step_since(revision, &run));
            }
            Run::Batch(name, commands) => {
                let result = self.session.run_linked(name, commands);
                reply.ran_batch(result, self.step_since(revision, &run));
            }
        }
    }
}

/// `musicable mcp`, started as an agent's client starts it, with its
/// settings in a folder of its own.
struct Bridge {
    child: Child,
    input: Option<ChildStdin>,
    /// The lines it writes, as they come.
    lines: Receiver<String>,
    /// The id of the last request.
    id: u64,
}

impl Bridge {
    /// Starts `musicable mcp` with the config folder `dir`, and introduces
    /// the client to it, as `claude-code`.
    fn start(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_musicable"))
            .arg("mcp")
            .env("MUSICABLE_CONFIG_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("musicable mcp starts");
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
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
        let server = bridge.request("initialize", client, &mut || {});
        assert_eq!(server["serverInfo"]["name"], "musicable", "{server}");
        assert!(server["capabilities"]["tools"].is_object(), "{server}");
        bridge.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        bridge
    }

    fn send(&mut self, message: Json) {
        let input = self.input.as_mut().expect("its input is open");
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }

    /// Sends the request `method` with `params`, calling `pump` while it
    /// waits, as the window's frames come, and gives back the result.
    fn request(&mut self, method: &str, params: Json, pump: &mut dyn FnMut()) -> Json {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "{method} got no answer"
            );
            pump();
            let line = match self.lines.recv_timeout(Duration::from_millis(2)) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => panic!("musicable mcp stopped"),
            };
            let message: Json = serde_json::from_str(&line).expect("it writes JSON-RPC lines");
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }

    /// Calls the tool `name` with `arguments`, calling `pump` while it
    /// waits. Gives back the JSON the tool answered, or why it failed.
    fn call(
        &mut self,
        name: &str,
        arguments: Json,
        pump: &mut dyn FnMut(),
    ) -> Result<Json, String> {
        let params = json!({"name": name, "arguments": arguments});
        let result = self.request("tools/call", params, pump);
        let text = result["content"][0]["text"].as_str();
        let text = text.expect("the tool answers in text").to_owned();
        match result["isError"].as_bool() {
            Some(true) => Err(text),
            _ => Ok(serde_json::from_str(&text).expect("the answer is JSON")),
        }
    }

    /// Closes its input, as a client going does, and waits for it to end.
    fn close(mut self) -> bool {
        self.input = None;
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.success();
            }
            assert!(started.elapsed() < Duration::from_secs(30), "it never ends");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A folder of its own for the test, removed when it ends.
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

#[test]
fn the_real_bridge_reaches_musicable_through_a_real_pipe() {
    let dir = TempDir::new("mcp-bridge");
    let mut bridge = Bridge::start(&dir.0);
    let tools = bridge.request("tools/list", json!({}), &mut || {});
    let names: Vec<_> = (tools["tools"].as_array().unwrap().iter())
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"run_command"), "{names:?}");
    // Nothing listens yet.
    let refused = bridge.call("get_project", json!({}), &mut || {});
    let refused = refused.unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");

    // Once Musicable lets agents in, the bridge reaches it at the next
    // call.
    let (_engine, handle) = Engine::new(CONFIG);
    let mut window = SessionWindow::new(handle);
    let address = Address::new(&dir.0).unwrap();
    let host = Host::start(address, window.session.commands(), || {}).unwrap();
    let mut pump = || {
        for call in host.calls() {
            musicable_mcp::answer(call, &mut window);
        }
    };
    let track = json!({"id": "track.new_midi_track"});
    let made = bridge.call("run_command", track, &mut pump);
    assert_eq!(made, Ok(json!({"result": 0, "undo": "New MIDI Track"})));
    assert_eq!(host.agents(), ["claude-code"]);
    let project = bridge.call("get_project", json!({}), &mut pump).unwrap();
    assert_eq!(project["tracks"][0]["name"], "MIDI 1");
    let undo = json!({"id": "edit.undo"});
    let undone = bridge.call("run_command", undo, &mut pump);
    assert_eq!(undone, Ok(json!({"result": null})));
    let history = bridge.call("get_history", json!({}), &mut pump).unwrap();
    assert_eq!(history["redo"], "New MIDI Track", "{history}");
    drop(pump);
    assert!(window.session.project().tracks.is_empty());

    // Once it stops, the tools say so.
    drop(host);
    let refused = bridge.call("get_project", json!({}), &mut || {});
    let refused = refused.unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");
    assert!(
        bridge.close(),
        "musicable mcp ends well once its client goes"
    );
}

/// A tenth of a second of a stereo 16-bit 48 kHz tone at -6 dBFS, at
/// `path`.
fn tone(path: &Path) {
    let samples: Vec<f32> = (0..4800)
        .map(|i| 0.5 * (std::f32::consts::TAU * 441.0 * i as f32 / 48_000.0).sin())
        .collect();
    save_wav(path, 48_000, SampleFormat::Int16, &[&samples, &samples]).unwrap();
}

#[test]
fn the_real_bridge_runs_the_audio_tools_with_musicable_closed() {
    let dir = TempDir::new("mcp-audio");
    let file = dir.0.join("tone.wav");
    tone(&file);
    let mut bridge = Bridge::start(&dir.0);
    let tools = bridge.request("tools/list", json!({}), &mut || {});
    let names: Vec<_> = (tools["tools"].as_array().unwrap().iter())
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in ["audio_info", "audio_cut", "audio_loudness", "run_command"] {
        assert!(names.contains(&name), "{names:?}");
    }

    // Nothing listens, and the audio tools run all the same.
    let info = bridge.call("audio_info", json!({"files": [file]}), &mut || {});
    let info = info.unwrap();
    assert_eq!(info["files"][0]["frames"], 4800, "{info}");
    assert_eq!(info["files"][0]["peak_dbfs"], -6.02, "{info}");
    let out = dir.0.join("cut.flac");
    let cut = json!({"file": file, "from": 0.025, "out": out});
    let cut = bridge.call("audio_cut", cut, &mut || {}).unwrap();
    assert_eq!(cut["exact"], true, "{cut}");
    assert_eq!(cut["out"]["frames"], 3600, "{cut}");
    assert_eq!(cut["out"]["format"], "flac", "{cut}");
    let again = json!({"file": file, "out": out});
    let refused = bridge.call("audio_cut", again, &mut || {}).unwrap_err();
    assert!(
        refused.ends_with("set overwrite to write over it."),
        "{refused}"
    );
    let refused = bridge.call("get_project", json!({}), &mut || {});
    let refused = refused.unwrap_err();
    assert!(refused.starts_with("Musicable isn't running"), "{refused}");
    assert!(bridge.close());
}

#[test]
fn the_real_bridge_reads_the_log_with_musicable_closed() {
    let dir = TempDir::new("mcp-log");
    let mut bridge = Bridge::start(&dir.0);
    // Before Musicable has ever run, there's no log.
    let log = bridge.call("get_log", json!({}), &mut || {}).unwrap();
    assert_eq!(log["entries"], json!([]), "{log}");
    assert_eq!(log["file"], json!(dir.0.join("musicable.log")), "{log}");

    // A panic, and what came before it, after Musicable stopped.
    std::fs::write(
        dir.0.join("musicable.log"),
        "2026-10-31T05:00:00.000Z INFO  musicable_app::log_file: Musicable 0.2.0 started\n\
         2026-10-31T05:00:01.000Z ERROR panic: thread 'main' panicked at app.rs:1:\n\
         boom\n",
    )
    .unwrap();
    let errors = json!({"level": "error", "since": "2026-10-31T05:00:00Z"});
    let log = bridge.call("get_log", errors, &mut || {}).unwrap();
    assert_eq!(
        log["entries"],
        json!([{
            "time": "2026-10-31T05:00:01.000Z",
            "level": "error",
            "target": "panic",
            "message": "thread 'main' panicked at app.rs:1:\nboom",
        }]),
        "{log}"
    );
    assert_eq!(log["earlier"], false, "{log}");
    let latest = bridge.call("get_log", json!({"lines": 1}), &mut || {});
    assert_eq!(latest.unwrap()["earlier"], true);
    let wrong = bridge.call("get_log", json!({"since": "yesterday"}), &mut || {});
    let wrong = wrong.unwrap_err();
    assert!(wrong.starts_with("since must be a time in UTC"), "{wrong}");
    assert!(bridge.close());
}

/// Runs `musicable audio` with `words` in the folder `dir`, and gives back
/// its exit code and what it wrote to its output and to its errors.
fn audio(dir: &Path, words: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_musicable"))
        .arg("audio")
        .args(words)
        .current_dir(dir)
        .output()
        .expect("musicable audio runs");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    (
        output.status.code(),
        text(output.stdout),
        text(output.stderr),
    )
}

#[test]
fn musicable_audio_answers_in_text_or_json_and_fails_as_shells_expect() {
    let dir = TempDir::new("audio-command-line");
    tone(&dir.0.join("tone.wav"));
    let (code, out, errors) = audio(&dir.0, &["info", "tone.wav", "--json"]);
    assert_eq!((code, errors.as_str()), (Some(0), ""));
    let info: Json = serde_json::from_str(&out).unwrap();
    // A whole path, though the file was named in the working folder.
    let file = info["files"][0]["file"].as_str().unwrap();
    assert!(
        Path::new(file).is_absolute() && file.ends_with("tone.wav"),
        "{file}"
    );

    let cut = ["cut", "tone.wav", "--length", "0.01", "--out", "cut.wav"];
    let (code, out, errors) = audio(&dir.0, &cut);
    assert_eq!((code, errors.as_str()), (Some(0), ""));
    assert!(out.contains("\nexact: true\n"), "{out}");
    // It won't write over what it wrote, and says why.
    let (code, out, errors) = audio(&dir.0, &cut);
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert!(
        errors.ends_with("set overwrite to write over it.\n"),
        "{errors}"
    );
    let (code, out, errors) = audio(&dir.0, &[&cut[..], &["--json"]].concat());
    assert_eq!((code, errors.as_str()), (Some(1), ""));
    let error: Json = serde_json::from_str(&out).unwrap();
    assert!(
        error["error"].as_str().unwrap().contains("set overwrite"),
        "{out}"
    );
    // A file it can't read fails the command, though it reads the rest.
    let (code, out, _) = audio(&dir.0, &["info", "tone.wav", "missing.wav", "--json"]);
    assert_eq!(code, Some(1));
    let info: Json = serde_json::from_str(&out).unwrap();
    assert_eq!(info["files"][0]["frames"], 4800, "{info}");
    assert!(info["files"][1]["error"].is_string(), "{info}");

    let (code, out, _) = audio(&dir.0, &["--help"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("loudness"), "{out}");
    let (code, _, errors) = audio(&dir.0, &["louder", "tone.wav"]);
    assert_eq!(code, Some(1));
    assert!(
        errors.starts_with("There's no audio command called louder."),
        "{errors}"
    );
}

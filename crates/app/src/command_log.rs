//! The command log: each command run this session, the user's from the
//! menus, keys and panels and agents', with its arguments, the time, who
//! ran it and why it failed, if it did. Agents read it with
//! `get_command_log`, and each session's goes to a file of its own in the
//! `command logs` folder of the config folder, one JSON object a line, so
//! what led to a bug can be read, and run again, after the app has closed.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use musicable_commands::{Args, CommandId, json};
use serde_json::{Value as Json, json};

use crate::log_file;

/// The folder in the config folder that holds the sessions' command logs.
pub const COMMAND_LOGS: &str = "command logs";
/// How many sessions' logs the folder keeps, the latest.
const SESSIONS: usize = 20;
/// How many entries the app keeps for agents to read; the file keeps them
/// all.
const KEPT: usize = 10_000;

/// What ran: a command, or commands run as one step in the undo history.
pub(crate) enum Ran<'a> {
    One(&'a CommandId, &'a Args),
    Batch(&'a str, &'a [(CommandId, Args)]),
}

/// This session's commands, as they ran.
#[derive(Default)]
pub(crate) struct CommandLog {
    /// The latest entries, at most [`KEPT`], as JSON.
    entries: VecDeque<Json>,
    /// How many commands have run this session.
    count: u64,
    /// Who runs the commands now: an agent, by the name its client gives,
    /// or else the user.
    by: Option<String>,
    /// The session's file, and where it is, once there's one.
    file: Option<(File, PathBuf)>,
}

impl CommandLog {
    /// Writes the commands that run from now on to a file of this
    /// session's own in `dir`'s [`COMMAND_LOGS`] folder, named after the
    /// time, and deletes the oldest sessions' files but the latest
    /// [`SESSIONS`]. Without a folder, or if it can't be made, the log is
    /// only kept for agents to read.
    pub fn write_to(&mut self, dir: &Path) {
        let folder = dir.join(COMMAND_LOGS);
        if fs::create_dir_all(&folder).is_err() {
            return;
        }
        let name = log_file::utc(SystemTime::now()).replace(':', "-");
        let path = folder.join(format!("{name}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(&path);
        let Ok(mut file) = file else {
            log::warn!("couldn't write the command log {}", path.display());
            return;
        };
        for entry in &self.entries {
            let _ = writeln!(file, "{entry}");
        }
        self.file = Some((file, path));
        forget_old(&folder);
    }

    /// The file the commands go to, if there's one.
    pub fn path(&self) -> Option<&Path> {
        self.file.as_ref().map(|(_, path)| path.as_path())
    }

    /// Notes that the commands run from now on are `agent`'s, or with
    /// `None`, the user's.
    pub fn set_by(&mut self, agent: Option<String>) {
        self.by = agent;
    }

    /// Adds what `ran`, and why it failed, if it did.
    pub fn add(&mut self, ran: Ran<'_>, error: Option<String>) {
        self.count += 1;
        let mut entry = json!({
            "n": self.count,
            "time": log_file::utc(SystemTime::now()),
            "by": self.by.as_deref().unwrap_or("user"),
        });
        match ran {
            Ran::One(id, args) => {
                entry["command"] = id.as_str().into();
                entry["args"] = json::args_to_json(args).into();
            }
            Ran::Batch(name, commands) => {
                entry["batch"] = name.into();
                entry["commands"] = (commands.iter())
                    .map(
                        |(id, args)| json!({ "id": id.as_str(), "args": json::args_to_json(args) }),
                    )
                    .collect();
            }
        }
        if let Some(error) = error {
            entry["error"] = error.into();
        }
        if let Some((file, _)) = &mut self.file {
            let _ = writeln!(file, "{entry}");
        }
        if self.entries.len() == KEPT {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// The entries after number `since`, if it says, at most the `last`,
    /// oldest first, how many commands ran this session, and the file, as
    /// `get_command_log` gives them.
    pub fn read(&self, since: Option<u64>, last: usize) -> Json {
        let after = (self.entries.iter())
            .filter(|entry| since.is_none_or(|since| entry["n"].as_u64() > Some(since)))
            .collect::<Vec<_>>();
        let skip = after.len().saturating_sub(last);
        json!({
            "entries": after[skip..],
            "count": self.count,
            "file": self.path(),
        })
    }
}

/// Deletes the oldest command logs in `folder` but the latest [`SESSIONS`],
/// which their names, the times they started, put in order.
fn forget_old(folder: &Path) {
    let Ok(files) = fs::read_dir(folder) else {
        return;
    };
    let mut logs: Vec<PathBuf> = (files.flatten())
        .map(|file| file.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    logs.sort();
    let old = logs.len().saturating_sub(SESSIONS);
    for log in &logs[..old] {
        let _ = fs::remove_file(log);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_log_says_what_ran_and_who_ran_it() {
        let mut log = CommandLog::default();
        let tempo = CommandId::from("transport.set_tempo");
        log.add(Ran::One(&tempo, &Args::new().with("bpm", 92_i64)), None);
        log.set_by(Some("claude-code".into()));
        let commands = [(tempo.clone(), Args::new().with("bpm", 0_i64))];
        let error = Some("The tempo is from 20 to 999 BPM.".to_owned());
        log.add(Ran::Batch("Slower", &commands), error);
        log.set_by(None);
        let read = log.read(None, 100);
        assert_eq!(read["count"], 2);
        assert_eq!(read["file"], Json::Null);
        let entries = read["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let mut first = entries[0].clone();
        assert!(first["time"].as_str().unwrap().ends_with('Z'), "{first}");
        first["time"] = Json::Null;
        assert_eq!(
            first,
            json!({
                "n": 1,
                "time": null,
                "by": "user",
                "command": "transport.set_tempo",
                "args": {"bpm": 92},
            })
        );
        assert_eq!(entries[1]["by"], "claude-code");
        assert_eq!(entries[1]["batch"], "Slower");
        assert_eq!(
            entries[1]["commands"],
            json!([{"id": "transport.set_tempo", "args": {"bpm": 0}}])
        );
        assert_eq!(entries[1]["error"], "The tempo is from 20 to 999 BPM.");
        // Those after one, and the last of them.
        let after = log.read(Some(1), 100);
        assert_eq!(after["entries"].as_array().unwrap().len(), 1);
        assert_eq!(after["entries"][0]["n"], 2);
        let last = log.read(None, 1);
        assert_eq!(last["entries"][0]["n"], 2);
        assert_eq!(log.read(Some(2), 100)["entries"], json!([]));
    }

    #[test]
    fn each_session_has_a_file_and_the_oldest_go() {
        let dir = TempDir::new("command-log");
        let folder = dir.0.join(COMMAND_LOGS);
        fs::create_dir_all(&folder).unwrap();
        for old in 0..SESSIONS {
            let name = format!("2020-01-01T00-00-{old:02}.000Z.jsonl");
            fs::write(folder.join(name), "").unwrap();
        }
        let mut log = CommandLog::default();
        // What ran before there was a file goes into it too.
        let play = CommandId::from("transport.play");
        log.add(Ran::One(&play, &Args::new()), None);
        log.write_to(&dir.0);
        let stop = CommandId::from("transport.stop");
        log.add(Ran::One(&stop, &Args::new()), None);
        let path = log.path().unwrap().to_owned();
        assert_eq!(log.read(None, 10)["file"], json!(path));
        let lines: Vec<Json> = (fs::read_to_string(&path).unwrap().lines())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let commands: Vec<&str> = lines
            .iter()
            .map(|line| line["command"].as_str().unwrap())
            .collect();
        assert_eq!(commands, ["transport.play", "transport.stop"]);
        // The oldest session's file went, to keep the latest.
        assert_eq!(fs::read_dir(&folder).unwrap().count(), SESSIONS);
        assert!(!folder.join("2020-01-01T00-00-00.000Z.jsonl").exists());
        assert!(path.exists());
    }
}

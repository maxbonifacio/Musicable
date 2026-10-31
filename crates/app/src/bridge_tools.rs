//! The tools `musicable mcp` runs itself, with no window: the audio
//! commands, and `get_log`, which reads the log file, so it works after
//! Musicable has stopped too, as when it panicked.

use std::path::PathBuf;

use log::Level;
use musicable_mcp::local::{LocalTool, LocalTools};
use serde_json::{Map, Value as Json, json};

use crate::log_file::{self, LOG_FILE, Wanted};
use crate::toolkit;

/// The log tool's name.
pub const GET_LOG: &str = "get_log";
/// At most how many entries `get_log` gives back.
const MOST: u64 = 1000;
/// How many it gives back unless it's told.
const LATEST: u64 = 100;

/// What agents are told about the log tool.
const INSTRUCTIONS: &str = "\
get_log reads Musicable's log file, which keeps warnings, errors and panics, \
also after Musicable has stopped.";

/// The tools the bridge runs itself, for the Musicable whose settings are in
/// `config_dir`.
pub struct BridgeTools {
    config_dir: Option<PathBuf>,
}

impl BridgeTools {
    pub fn new(config_dir: Option<PathBuf>) -> Self {
        Self { config_dir }
    }

    fn get_log(&self, arguments: &Map<String, Json>) -> Result<Json, String> {
        if let Some(unknown) =
            (arguments.keys()).find(|key| !["lines", "level", "since"].contains(&key.as_str()))
        {
            return Err(format!("{GET_LOG} takes no argument called {unknown}."));
        }
        let most = match arguments.get("lines") {
            None | Some(Json::Null) => LATEST,
            Some(lines) => (lines.as_u64())
                .filter(|lines| (1..=MOST).contains(lines))
                .ok_or_else(|| format!("lines must be a whole number from 1 to {MOST}."))?,
        };
        let at_least = match arguments.get("level") {
            None | Some(Json::Null) => Level::Info,
            Some(Json::String(level)) => match level.as_str() {
                "error" => Level::Error,
                "warn" => Level::Warn,
                "info" => Level::Info,
                _ => return Err("level must be \"error\", \"warn\" or \"info\".".into()),
            },
            Some(_) => return Err("level must be \"error\", \"warn\" or \"info\".".into()),
        };
        let since = match arguments.get("since") {
            None | Some(Json::Null) => None,
            Some(Json::String(since)) => Some(log_file::utc_time(since).ok_or_else(|| {
                format!(
                    "since must be a time in UTC as the log has them, such as \
                     2026-10-31T05:00:00Z, not \"{since}\"."
                )
            })?),
            Some(_) => return Err("since must be text.".into()),
        };
        let Some(dir) = &self.config_dir else {
            return Ok(json!({ "file": null, "entries": [], "earlier": false }));
        };
        let wanted = Wanted {
            most: most as usize,
            at_least,
            since: since.as_deref(),
        };
        let (entries, earlier) = log_file::read(dir, &wanted);
        let entries: Vec<Json> = (entries.into_iter())
            .map(|entry| {
                json!({
                    "time": entry.time,
                    "level": entry.level.as_str().to_lowercase(),
                    "target": entry.target,
                    "message": entry.message,
                })
            })
            .collect();
        Ok(json!({
            "file": dir.join(LOG_FILE),
            "entries": entries,
            "earlier": earlier,
        }))
    }
}

impl LocalTools for BridgeTools {
    fn tools(&self) -> Vec<LocalTool> {
        let mut tools = toolkit::Tools.tools();
        tools.push(log_tool());
        tools
    }

    fn instructions(&self) -> String {
        format!("{}\n\n{INSTRUCTIONS}", toolkit::Tools.instructions())
    }

    fn call(
        &self,
        name: &str,
        arguments: &Map<String, Json>,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(f64),
    ) -> Result<Json, String> {
        match name {
            GET_LOG => self.get_log(arguments),
            _ => toolkit::Tools.call(name, arguments, cancelled, progress),
        }
    }
}

/// `get_log`, as `tools/list` describes it.
fn log_tool() -> LocalTool {
    let object = |json: Json| match json {
        Json::Object(object) => object,
        _ => unreachable!("a schema is an object"),
    };
    let entry = json!({
        "type": "object",
        "properties": {
            "time": {"type": "string", "description": "When, in UTC to the millisecond, such as 2026-10-31T05:00:00.123Z."},
            "level": {"type": "string", "enum": ["error", "warn", "info", "debug", "trace"]},
            "target": {"type": "string", "description": "Who logged it, such as wgpu_hal, musicable_app or panic."},
            "message": {"type": "string", "description": "What was logged, over several lines for a panic, with its calls."},
        },
        "required": ["time", "level", "target", "message"],
        "additionalProperties": false,
    });
    LocalTool {
        name: GET_LOG.to_owned(),
        title: "Get Log".to_owned(),
        description: "Reads the latest entries of Musicable's log file, musicable.log in its \
                      config folder, oldest first: warnings and errors, such as wgpu's, \
                      Musicable's own notes, such as the graphics adapter it draws with, and \
                      panics, with their calls. It reads the file, so it works while Musicable \
                      is closed, and after it stopped. level keeps those as bad as it, or \
                      worse, and since those logged at that time or after."
            .to_owned(),
        input_schema: object(json!({
            "type": "object",
            "properties": {
                "lines": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MOST,
                    "description": format!("At most how many entries, the latest: {LATEST} unless it's given."),
                },
                "level": {
                    "type": "string",
                    "enum": ["error", "warn", "info"],
                    "description": "Only entries this bad, or worse: info, the default, keeps them all.",
                },
                "since": {
                    "type": "string",
                    "description": "Only entries logged at this time or after, in UTC, such as 2026-10-31T05:00:00Z.",
                },
            },
            "additionalProperties": false,
        })),
        output_schema: object(json!({
            "type": "object",
            "properties": {
                "file": {"type": ["string", "null"], "description": "The log file, or null when Musicable has no config folder."},
                "entries": {"type": "array", "items": entry},
                "earlier": {"type": "boolean", "description": "Whether earlier entries that fit were left out, past lines."},
            },
            "required": ["file", "entries", "earlier"],
            "additionalProperties": false,
        })),
        read_only: true,
    }
}

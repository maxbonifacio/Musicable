//! The bridge's tools that start Musicable, which the app can't do for
//! itself: `launch_app`, which starts a build, with a project, as an app
//! or headless, and waits until it answers, and `restart_app`, which quits
//! the one running first, so a new build can replace it. `quit_app` and
//! `focus_app` are the app's own tools.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value as Json, json};

pub const LAUNCH_APP: &str = "launch_app";
pub const RESTART_APP: &str = "restart_app";

/// How long launching waits for Musicable to answer unless it says, and
/// the most it can say, in seconds.
const WAIT: u64 = 30;
const LONGEST_WAIT: u64 = 300;
/// How long restarting waits for the Musicable running to close.
pub(crate) const CLOSING: Duration = Duration::from_secs(30);

/// The tools, as `tools/list` gives them.
pub(crate) fn tools() -> Vec<Tool> {
    let launching = json!({
        "build": {
            "type": "string",
            "description": "The full path of the Musicable program to start, such as a build \
                            in target\\release. Without it, the program that runs this bridge.",
        },
        "project": {
            "type": "string",
            "description": "The full path of a project file to open.",
        },
        "headless": {
            "type": "boolean",
            "description": "Starts it with no window, as musicable --headless does: it lets \
                            agents in, and nobody else uses it.",
        },
        "audio": {
            "type": "boolean",
            "description": "Headless, whether it plays through the audio device the settings \
                            name, as the app does. true without it; false plays nothing, \
                            as a computer with no audio device does, and exports still render.",
        },
        "wait": {
            "type": "integer",
            "minimum": 1,
            "maximum": LONGEST_WAIT,
            "description": "How long to wait for it to answer, in seconds. 30 without it.",
        },
    });
    let mut restarting = launching.clone();
    restarting["unsaved"] = json!({
        "type": "string",
        "enum": ["save", "discard"],
        "description": "What to do with the unsaved changes of the Musicable running, as \
                        quit_app takes it.",
    });
    restarting["project"]["description"] = json!(
        "The full path of a project file to open. Without it, the file the Musicable running \
         had open, if it had one."
    );
    let tool = |name: &'static str, title: &str, about: &'static str, properties: Json| {
        let mut schema = JsonObject::new();
        schema.insert("type".into(), json!("object"));
        schema.insert("properties".into(), properties);
        schema.insert("additionalProperties".into(), json!(false));
        let annotations = ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .open_world(false);
        Tool::new(name, about, std::sync::Arc::new(schema))
            .with_title(title)
            .annotate(annotations)
    };
    vec![
        tool(
            LAUNCH_APP,
            "Launch Musicable",
            "Starts Musicable, a given build of it if it says, with a project if it says, as \
             the app or headless, with its settings where this bridge finds them, and answers \
             once it answers agents, with its process id. The app's window comes to the front. \
             The app answers only while the user lets agents in; headless always does. Fails \
             while a Musicable answers already: restart_app replaces it.",
            launching,
        ),
        tool(
            RESTART_APP,
            "Restart Musicable",
            "Quits the Musicable running, as quit_app does, waits for it to close, then starts \
             one again, as launch_app does, with the project it had open unless it says \
             another. Between the two, a build can replace the program, as it can't while it \
             runs. Without the Musicable running, it only starts one.",
            restarting,
        ),
    ]
}

/// What `launch_app` or `restart_app` was asked to start.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Launch {
    pub build: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub headless: bool,
    pub audio: bool,
    pub wait: Duration,
    /// For `restart_app`: what to do with the unsaved changes, as
    /// `quit_app` takes it.
    pub unsaved: Option<String>,
}

/// What `arguments` ask the tool `name` to start, or why they're wrong.
pub(crate) fn launch(name: &str, arguments: &JsonObject) -> Result<Launch, String> {
    let mut allowed = vec!["build", "project", "headless", "audio", "wait"];
    if name == RESTART_APP {
        allowed.push("unsaved");
    }
    if let Some(unknown) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        return Err(format!("{name} takes no argument called {unknown}."));
    }
    let path = |key: &str| -> Result<Option<PathBuf>, String> {
        match arguments.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(Json::String(path)) => {
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err(format!(
                        "{} isn't a full path: give the {key}'s full path, from the root of its \
                         drive.",
                        path.display()
                    ));
                }
                if !path.is_file() {
                    return Err(format!("There's no file {}.", path.display()));
                }
                Ok(Some(path))
            }
            Some(_) => Err(format!("{key} must be text: a full path.")),
        }
    };
    let flag = |key: &str, default: bool| match arguments.get(key) {
        None | Some(Json::Null) => Ok(default),
        Some(Json::Bool(on)) => Ok(*on),
        Some(_) => Err(format!("{key} must be true or false.")),
    };
    let wait = match arguments.get("wait") {
        None | Some(Json::Null) => WAIT,
        Some(wait) => match wait.as_u64() {
            Some(wait @ 1..=LONGEST_WAIT) => wait,
            _ => {
                return Err(format!(
                    "wait must be a whole number of seconds, from 1 to {LONGEST_WAIT}."
                ));
            }
        },
    };
    let unsaved = match arguments.get("unsaved") {
        None | Some(Json::Null) => None,
        Some(Json::String(unsaved)) if unsaved == "save" || unsaved == "discard" => {
            Some(unsaved.clone())
        }
        Some(_) => return Err("unsaved is save or discard.".into()),
    };
    let headless = flag("headless", false)?;
    let audio = flag("audio", true)?;
    if !headless && !audio {
        return Err("audio is for headless Musicable: the app plays as its settings say.".into());
    }
    Ok(Launch {
        build: path("build")?,
        project: path("project")?,
        headless,
        audio,
        wait: Duration::from_secs(wait),
        unsaved,
    })
}

/// Starts the program `program` as `launch` says, with its settings in
/// `config_dir`, in a process of its own that carries on once the bridge
/// has gone.
pub(crate) fn start(
    program: &Path,
    launch: &Launch,
    config_dir: Option<&Path>,
) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    if launch.headless {
        command.arg("--headless");
        if !launch.audio {
            command.arg("--no-audio");
        }
    }
    if let Some(project) = &launch.project {
        command.arg(project);
    }
    if let Some(dir) = config_dir {
        command.env("MUSICABLE_CONFIG_DIR", dir);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console of its own, and not in the bridge's group, so it
        // carries on once the agent's client stops the bridge.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()
}

/// Why the process `child`, which was starting, stopped, if it has.
pub(crate) fn stopped(child: &mut Child) -> Option<String> {
    match child.try_wait() {
        Ok(Some(status)) => Some(format!(
            "Musicable stopped as it started ({status}): get_log says why."
        )),
        Ok(None) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(json: Json) -> JsonObject {
        match json {
            Json::Object(object) => object,
            _ => panic!("an object"),
        }
    }

    #[test]
    fn launching_takes_a_build_a_project_and_how() {
        let program = std::env::current_exe().unwrap();
        let build = program.to_string_lossy().into_owned();
        assert_eq!(
            launch(
                LAUNCH_APP,
                &object(json!({"build": build, "headless": true, "audio": false}))
            ),
            Ok(Launch {
                build: Some(program.clone()),
                project: None,
                headless: true,
                audio: false,
                wait: Duration::from_secs(30),
                unsaved: None,
            })
        );
        assert_eq!(
            launch(LAUNCH_APP, &object(json!({"project": "song.musicable"}))),
            Err(
                "song.musicable isn't a full path: give the project's full path, from the root \
                 of its drive."
                    .into()
            )
        );
        assert_eq!(
            launch(LAUNCH_APP, &object(json!({"unsaved": "save"}))),
            Err("launch_app takes no argument called unsaved.".into())
        );
        assert_eq!(
            launch(RESTART_APP, &object(json!({"unsaved": "keep"}))),
            Err("unsaved is save or discard.".into())
        );
        assert_eq!(
            launch(RESTART_APP, &object(json!({"audio": false}))),
            Err("audio is for headless Musicable: the app plays as its settings say.".into())
        );
        assert_eq!(
            launch(RESTART_APP, &object(json!({"wait": 0}))),
            Err("wait must be a whole number of seconds, from 1 to 300.".into())
        );
        assert_eq!(tools().len(), 2);
    }
}

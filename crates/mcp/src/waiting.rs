//! `wait_for`: a call that waits until something that lands later has,
//! such as the takes after Stop or the analysis of a file, then answers
//! with the status. The server waits, reading the status over and over,
//! so the window answers each read as it does `get_status`'s and knows
//! nothing of the wait.

use std::time::Duration;

use rmcp::RoleServer;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use rmcp::service::RequestContext;
use serde_json::{Value as Json, json};
use tokio::time::Instant;

use crate::Asked;
use crate::tools::{Tools, failed};

/// How long a wait lasts unless the call says otherwise, and the longest
/// it can say.
const TIMEOUT: Duration = Duration::from_secs(60);
const LONGEST: Duration = Duration::from_secs(600);

/// How often the status is read while waiting.
const EVERY: Duration = Duration::from_millis(100);

/// What a call to `wait_for` waits for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Wait {
    pub until: Until,
    pub timeout: Duration,
    /// For [`Until::Changed`], the revision the project should move on
    /// from, or else the one it's at when the wait starts.
    pub since: Option<u64>,
}

/// What a wait waits for, as `get_status` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    /// No work under way, as `idle` says.
    Idle,
    /// The transport doesn't play.
    Stopped,
    /// Nothing records, and the takes recorded are in the project.
    Recorded,
    /// No audio file being analysed.
    Analysed,
    /// No plugin scan, and the library neither scans, reads nor looks for
    /// presets.
    Scanned,
    /// No segment left to render, and no graph compiling.
    Rendered,
    /// The project changed.
    Changed,
    /// No question waits for the user.
    Answered,
}

impl Until {
    pub const ALL: [(&str, Until); 8] = [
        ("idle", Until::Idle),
        ("stopped", Until::Stopped),
        ("recorded", Until::Recorded),
        ("analysed", Until::Analysed),
        ("scanned", Until::Scanned),
        ("rendered", Until::Rendered),
        ("changed", Until::Changed),
        ("answered", Until::Answered),
    ];

    /// The names `until` takes, in the order the tool lists them.
    pub fn names() -> Vec<&'static str> {
        Self::ALL.iter().map(|(name, _)| *name).collect()
    }

    fn named(name: &str) -> Option<Until> {
        (Self::ALL.iter()).find_map(|(key, until)| (*key == name).then_some(*until))
    }

    /// Whether `status`, as `get_status` reads, says the wait is over, the
    /// project having been at revision `since`.
    fn holds(self, status: &Json, since: u64) -> bool {
        let yes = |key: &str| status[key].as_bool() == Some(true);
        let none = |value: &Json| match value {
            Json::Null => true,
            Json::Array(list) => list.is_empty(),
            Json::Bool(on) => !on,
            Json::Number(count) => count.as_f64() == Some(0.0),
            _ => false,
        };
        let library = &status["library"];
        match self {
            Until::Idle => yes("idle"),
            Until::Stopped => !yes("playing"),
            Until::Recorded => none(&status["takes"]) && !(yes("playing") && yes("recording")),
            Until::Analysed => none(&status["analysing"]),
            Until::Scanned => {
                !yes("scanning_plugins")
                    && none(&library["scanning"])
                    && none(&library["reading"])
                    && none(&library["finding_presets"])
            }
            Until::Rendered => none(&status["rendering_segments"]) && !yes("compiling"),
            Until::Changed => status["revision"].as_u64().is_some_and(|now| now != since),
            Until::Answered => status["question"].is_null(),
        }
    }
}

/// The wait a call to `wait_for` with `arguments` asks for, or why it
/// can't.
pub(crate) fn wait(arguments: &JsonObject) -> Result<Wait, String> {
    if let Some(unknown) =
        (arguments.keys()).find(|key| !["until", "timeout", "since"].contains(&key.as_str()))
    {
        return Err(format!("wait_for takes no argument called {unknown}."));
    }
    let names = Until::names().join(", ");
    let until = match arguments.get("until") {
        Some(Json::String(name)) => {
            Until::named(name).ok_or_else(|| format!("until is one of {names}, not {name}."))?
        }
        Some(_) => return Err(format!("until must be text: one of {names}.")),
        None => return Err(format!("until is missing: one of {names}.")),
    };
    let longest = LONGEST.as_secs();
    let timeout = match arguments.get("timeout") {
        None | Some(Json::Null) => TIMEOUT,
        Some(seconds) => match seconds.as_f64() {
            Some(seconds) if (0.0..=LONGEST.as_secs_f64()).contains(&seconds) => {
                Duration::from_secs_f64(seconds)
            }
            _ => {
                return Err(format!(
                    "timeout is a number of seconds, from 0 to {longest}."
                ));
            }
        },
    };
    let since = match arguments.get("since") {
        None | Some(Json::Null) => None,
        Some(revision) => match revision.as_u64() {
            Some(revision) => Some(revision),
            None => {
                return Err("since is a revision, as get_status gives it: a whole number.".into());
            }
        },
    };
    if since.is_some() && until != Until::Changed {
        return Err("since goes with until changed.".into());
    }
    Ok(Wait {
        until,
        timeout,
        since,
    })
}

impl Tools {
    /// Reads the status every [`EVERY`] until `wait` is over or times out,
    /// or the agent cancels the call, and says how it went.
    ///
    /// The wait is over once the status says so twice in a row, so work
    /// that the window starts on the frame after a command, such as
    /// analysing the file it brought in, or takes in on the frame after
    /// the recorder finished, such as a take, has started or landed.
    pub(crate) async fn wait_for(
        &self,
        agent: &str,
        wait: Wait,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResult {
        let started = Instant::now();
        let mut since = wait.since;
        let mut held = false;
        loop {
            let status = match self.ask(agent, Asked::Status).await {
                Ok(status) => status,
                Err(why) => return failed(why),
            };
            let since = *since.get_or_insert_with(|| status["revision"].as_u64().unwrap_or(0));
            let holds = wait.until.holds(&status, since);
            let waited = started.elapsed();
            let out_of_time = waited >= wait.timeout;
            if (holds && (held || out_of_time)) || out_of_time {
                let waited = (waited.as_secs_f64() * 10.0).round() / 10.0;
                let answer = json!({ "met": holds, "waited_seconds": waited, "status": status });
                return CallToolResult::success(vec![ContentBlock::text(answer.to_string())]);
            }
            held = holds;
            let pause = EVERY.min(wait.timeout - waited);
            if tokio::time::timeout(pause, context.ct.cancelled())
                .await
                .is_ok()
            {
                return failed("The wait was cancelled.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(arguments: Json) -> Result<Wait, String> {
        let Json::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        wait(&arguments)
    }

    #[test]
    fn waits_take_a_condition_a_timeout_and_a_revision() {
        assert_eq!(
            wait_for(json!({"until": "analysed"})),
            Ok(Wait {
                until: Until::Analysed,
                timeout: TIMEOUT,
                since: None
            })
        );
        assert_eq!(
            wait_for(json!({"until": "changed", "timeout": 2.5, "since": 7})),
            Ok(Wait {
                until: Until::Changed,
                timeout: Duration::from_millis(2500),
                since: Some(7)
            })
        );
        assert_eq!(
            wait_for(json!({"until": "idle", "timeout": 0})).map(|wait| wait.timeout),
            Ok(Duration::ZERO)
        );
        let names = "idle, stopped, recorded, analysed, scanned, rendered, changed, answered";
        assert_eq!(
            wait_for(json!({})),
            Err(format!("until is missing: one of {names}."))
        );
        assert_eq!(
            wait_for(json!({"until": "done"})),
            Err(format!("until is one of {names}, not done."))
        );
        assert_eq!(
            wait_for(json!({"until": "idle", "timeout": 601})),
            Err("timeout is a number of seconds, from 0 to 600.".into())
        );
        assert_eq!(
            wait_for(json!({"until": "idle", "timeout": "soon"})),
            Err("timeout is a number of seconds, from 0 to 600.".into())
        );
        assert_eq!(
            wait_for(json!({"until": "changed", "since": -1})),
            Err("since is a revision, as get_status gives it: a whole number.".into())
        );
        assert_eq!(
            wait_for(json!({"until": "idle", "since": 3})),
            Err("since goes with until changed.".into())
        );
        assert_eq!(
            wait_for(json!({"until": "idle", "for": 3})),
            Err("wait_for takes no argument called for.".into())
        );
    }

    /// A status as `get_status` reads it with nothing under way, at
    /// revision 4.
    fn quiet() -> Json {
        json!({
            "playing": false,
            "recording": false,
            "takes": [],
            "question": null,
            "idle": true,
            "analysing": [],
            "rendering_segments": 0,
            "compiling": false,
            "export": null,
            "scanning_plugins": false,
            "library": {"scanning": [], "reading": [], "finding_presets": false},
            "revision": 4,
        })
    }

    /// A change to a status.
    type Change = fn(&mut Json);

    #[test]
    fn each_condition_reads_its_part_of_the_status() {
        for (name, until) in Until::ALL {
            let holds = until.holds(&quiet(), 4);
            assert_eq!(holds, until != Until::Changed, "{name}");
        }
        let busy: [(Until, Change); 13] = [
            (Until::Idle, |status| status["idle"] = false.into()),
            (Until::Stopped, |status| status["playing"] = true.into()),
            (Until::Recorded, |status| {
                status["takes"] = json!([{"track": 1, "start": 0, "end": 4}]);
            }),
            (Until::Recorded, |status| {
                status["playing"] = true.into();
                status["recording"] = true.into();
            }),
            (Until::Analysed, |status| {
                status["analysing"] = json!(["a.wav"])
            }),
            (Until::Scanned, |status| {
                status["scanning_plugins"] = true.into()
            }),
            (Until::Scanned, |status| {
                status["library"]["scanning"] = json!([{"folder": "/Samples", "found": 3}]);
            }),
            (Until::Scanned, |status| {
                status["library"]["reading"] = json!([{"folder": "/Samples", "left": 3}]);
            }),
            (Until::Scanned, |status| {
                status["library"]["finding_presets"] = true.into();
            }),
            (Until::Rendered, |status| {
                status["rendering_segments"] = 2.into()
            }),
            (Until::Rendered, |status| status["compiling"] = true.into()),
            (Until::Answered, |status| {
                status["question"] = "Save the changes to Song?".into();
            }),
            (Until::Changed, |_| {}),
        ];
        for (until, make_busy) in busy {
            let mut status = quiet();
            make_busy(&mut status);
            assert!(!until.holds(&status, 4), "{until:?}: {status}");
        }
        // Recording armed with the transport stopped records nothing.
        let mut status = quiet();
        status["recording"] = true.into();
        assert!(Until::Recorded.holds(&status, 4));
        assert!(Until::Changed.holds(&quiet(), 3));
    }
}

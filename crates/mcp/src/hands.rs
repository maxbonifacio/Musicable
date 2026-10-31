//! `use_mouse_and_keys`: what an agent does with the mouse and the keys in
//! a window, step by step, as egui's input, never the system's pointer.

use std::path::PathBuf;

use musicable_commands::{KeyChord, Modifiers};
use rmcp::model::JsonObject;
use serde_json::{Value as Json, json};

use crate::tools::Refused;

/// What an agent does with the mouse and keys: see
/// [`crate::Window::use_mouse_and_keys`].
#[derive(Debug, Clone, PartialEq)]
pub struct Hands {
    /// The window: the main window with `None`, or else a window of its
    /// own, counted from 0.
    pub window: Option<usize>,
    pub steps: Vec<Step>,
    /// What of the window to read as each frame is drawn, while the steps
    /// play out, if anything.
    pub watch: Option<Watch>,
}

/// What of the window to read each frame, as `get_ui_tree` reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Watch {
    pub panel: Option<String>,
    pub filter: Option<String>,
    pub limit: usize,
}

/// Where in the window a step points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// The middle of a node of the window's UI tree, by its id, wherever
    /// it is when the step comes.
    Node(u64),
    /// A place in the window, in pixels from its top left corner, as
    /// pictures of it have them.
    At([f32; 2]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

/// One step.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Moves the pointer to `to`, in a straight line over `seconds`, or at
    /// once.
    Move { to: Target, seconds: f32 },
    /// Presses a button where the pointer is, and holds it down.
    Down(Button),
    /// Lets go of a button.
    Up(Button),
    /// Moves the pointer to `at` and clicks `count` times, `interval`
    /// seconds apart.
    Click {
        at: Target,
        button: Button,
        count: u8,
        interval: f32,
    },
    /// Presses at `from`, moves to `to` over `seconds`, and lets go.
    Drag {
        from: Target,
        to: Target,
        button: Button,
        seconds: f32,
    },
    /// Moves the pointer to `at` and keeps it there for `seconds`.
    Hover { at: Target, seconds: f32 },
    /// Turns the mouse wheel by so many pixels across and down, where the
    /// pointer is, or at `at`.
    Scroll { by: [f32; 2], at: Option<Target> },
    /// Presses and lets go of a key chord, such as Ctrl+Z.
    Key(KeyChord),
    /// Holds modifier keys down from now on, as for a Shift-drag.
    Hold(Modifiers),
    /// Lets go of modifier keys.
    Release(Modifiers),
    /// Types text, into the text field with the focus.
    Text(String),
    /// Drops files on the window at `at`, as from the file manager.
    Drop { files: Vec<PathBuf>, at: Target },
    /// Does nothing for so many seconds.
    Wait(f32),
}

impl Step {
    /// How long the step takes, roughly, in seconds.
    fn seconds(&self) -> f32 {
        match self {
            Step::Move { seconds, .. } | Step::Drag { seconds, .. } => *seconds,
            Step::Hover { seconds, .. } | Step::Wait(seconds) => *seconds,
            Step::Click {
                count, interval, ..
            } => f32::from(*count) * interval,
            _ => 0.0,
        }
    }
}

/// The most steps a call takes, and the longest it can take.
pub const MOST_STEPS: usize = 200;
pub const LONGEST: f32 = 120.0;

/// The step kinds, by the key each step's object names it with.
const KINDS: [&str; 13] = [
    "move", "down", "up", "click", "drag", "hover", "scroll", "key", "hold", "release", "text",
    "drop", "wait",
];

/// The steps `arguments` give, from `use_mouse_and_keys`.
pub(crate) fn steps(arguments: &JsonObject) -> Result<Vec<Step>, Refused> {
    let Some(Json::Array(list)) = arguments.get("steps") else {
        return Err(refused("steps must be a list of steps."));
    };
    if list.is_empty() {
        return Err(refused("There are no steps."));
    }
    if list.len() > MOST_STEPS {
        return Err(refused(format!(
            "There are {} steps, and a call takes at most {MOST_STEPS}.",
            list.len()
        )));
    }
    let steps = (list.iter().enumerate())
        .map(|(index, step)| {
            step_from(step).map_err(|why| refused(format!("Step {}: {why}", index + 1)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let seconds: f32 = steps.iter().map(Step::seconds).sum();
    if seconds > LONGEST {
        return Err(refused(format!(
            "The steps take {seconds:.1} seconds, and a call takes at most {LONGEST}: split them \
             over several calls."
        )));
    }
    Ok(steps)
}

fn refused(why: impl Into<String>) -> Refused {
    Refused::Arguments(why.into())
}

/// One step, from its object, or why it isn't one.
fn step_from(step: &Json) -> Result<Step, String> {
    let Json::Object(step) = step else {
        return Err("a step is an object, such as {\"click\": \"3f2a\"}.".into());
    };
    let kinds: Vec<&str> = (KINDS.iter().copied())
        .filter(|kind| step.contains_key(*kind))
        .collect();
    let kind = match kinds.as_slice() {
        [kind] => *kind,
        [] => {
            return Err(format!(
                "a step says what it does with one of {}.",
                KINDS.join(", ")
            ));
        }
        _ => {
            return Err(format!(
                "a step does one thing, not {}.",
                kinds.join(" and ")
            ));
        }
    };
    let allowed: &[&str] = match kind {
        "move" | "hover" => &["seconds"],
        "down" | "up" | "key" | "hold" | "release" | "text" | "wait" => &[],
        "click" => &["button", "count", "interval"],
        "drag" => &["to", "button", "seconds"],
        "scroll" | "drop" => &["at"],
        _ => unreachable!("every kind is listed"),
    };
    if let Some(other) = (step.keys()).find(|key| *key != kind && !allowed.contains(&key.as_str()))
    {
        return Err(format!("a {kind} step takes no {other}."));
    }
    let value = &step[kind];
    Ok(match kind {
        "move" => Step::Move {
            to: target(value)?,
            seconds: seconds(step, "seconds", 0.0)?,
        },
        "down" => Step::Down(button(value)?),
        "up" => Step::Up(button(value)?),
        "click" => {
            let count = match step.get("count") {
                None | Some(Json::Null) => 1,
                Some(count) => match count.as_u64() {
                    Some(count @ 1..=3) => count as u8,
                    _ => return Err("count is 1, 2 or 3.".into()),
                },
            };
            let interval = seconds(step, "interval", 0.1)?;
            if !(0.02..=2.0).contains(&interval) {
                return Err("interval is from 0.02 to 2 seconds.".into());
            }
            Step::Click {
                at: target(value)?,
                button: button(step.get("button").unwrap_or(&Json::Null))?,
                count,
                interval,
            }
        }
        "drag" => Step::Drag {
            from: target(value)?,
            to: target(
                step.get("to")
                    .ok_or("a drag says where it goes, with to.")?,
            )?,
            button: button(step.get("button").unwrap_or(&Json::Null))?,
            seconds: seconds(step, "seconds", 0.3)?,
        },
        "hover" => Step::Hover {
            at: target(value)?,
            seconds: seconds(step, "seconds", 1.0)?,
        },
        "scroll" => Step::Scroll {
            by: pair(value).ok_or("scroll is how far across and down, in pixels: [0, 120].")?,
            at: match step.get("at") {
                None | Some(Json::Null) => None,
                Some(at) => Some(target(at)?),
            },
        },
        "key" => {
            let chord = value
                .as_str()
                .ok_or("key is a key chord, such as \"Ctrl+Z\".")?;
            Step::Key((chord.parse::<KeyChord>()).map_err(|error| format!("{chord}: {error}."))?)
        }
        "hold" => Step::Hold(modifiers(value)?),
        "release" => Step::Release(modifiers(value)?),
        "text" => Step::Text(
            value
                .as_str()
                .ok_or("text is the text to type.")?
                .to_owned(),
        ),
        "drop" => {
            let files = match value {
                Json::String(file) => vec![PathBuf::from(file)],
                Json::Array(files) => (files.iter())
                    .map(|file| file.as_str().map(PathBuf::from))
                    .collect::<Option<Vec<_>>>()
                    .ok_or("drop is a file's full path, or a list of them.")?,
                _ => return Err("drop is a file's full path, or a list of them.".into()),
            };
            if files.is_empty() {
                return Err("there are no files to drop.".into());
            }
            if let Some(file) = files.iter().find(|file| !file.is_absolute()) {
                return Err(format!("{} isn't a full path.", file.display()));
            }
            Step::Drop {
                files,
                at: target(step.get("at").ok_or("a drop says where, with at.")?)?,
            }
        }
        "wait" => Step::Wait(
            value
                .as_f64()
                .filter(|seconds| (0.0..=f64::from(LONGEST)).contains(seconds))
                .ok_or(format!("wait is from 0 to {LONGEST} seconds."))? as f32,
        ),
        _ => unreachable!("every kind is listed"),
    })
}

/// Where `value` points: a node's id from the UI tree, or a place in pixels.
fn target(value: &Json) -> Result<Target, String> {
    let problem = || {
        "a place is a node's id from get_ui_tree, such as \"3f2a\", or where in the window, in \
         pixels: [120, 48]."
            .to_owned()
    };
    match value {
        Json::String(id) => u64::from_str_radix(id, 16)
            .map(Target::Node)
            .map_err(|_| problem()),
        _ => pair(value)
            .filter(|[x, y]| *x >= 0.0 && *y >= 0.0)
            .map(Target::At)
            .ok_or_else(problem),
    }
}

/// Two numbers, `[x, y]`.
fn pair(value: &Json) -> Option<[f32; 2]> {
    match value.as_array()?.as_slice() {
        [x, y] => Some([x.as_f64()? as f32, y.as_f64()? as f32]),
        _ => None,
    }
}

fn button(value: &Json) -> Result<Button, String> {
    match value {
        Json::Null => Ok(Button::Left),
        Json::String(name) => match name.to_lowercase().as_str() {
            "left" => Ok(Button::Left),
            "right" => Ok(Button::Right),
            "middle" => Ok(Button::Middle),
            _ => Err(format!("there's no {name} button: left, right or middle.")),
        },
        _ => Err("a button is left, right or middle.".into()),
    }
}

/// Modifier keys, such as `Shift` or `Ctrl+Alt`.
fn modifiers(value: &Json) -> Result<Modifiers, String> {
    let text = value
        .as_str()
        .ok_or("modifiers are named, such as \"Shift\" or \"Ctrl+Alt\".")?;
    text.split('+')
        .map(str::trim)
        .map(|name| {
            Modifiers::from_name(name)
                .ok_or_else(|| format!("{name} isn't a modifier: Ctrl, Alt, Shift or Cmd."))
        })
        .try_fold(Modifiers::NONE, |held, name| Ok(held | name?))
}

/// The seconds `step` gives as `name`, `default` unless it says.
fn seconds(step: &JsonObject, name: &str, default: f32) -> Result<f32, String> {
    match step.get(name) {
        None | Some(Json::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|seconds| (0.0..=30.0).contains(seconds))
            .map(|seconds| seconds as f32)
            .ok_or(format!("{name} is from 0 to 30 seconds.")),
    }
}

/// The schema of `use_mouse_and_keys`'s steps.
pub(crate) fn schema() -> Json {
    let place = json!({
        "description": "A node's id from get_ui_tree, such as \"3f2a\", for its middle, \
                        wherever it is when the step comes, or where in the window in pixels, \
                        [x, y], as get_screenshot's pictures have it.",
        "anyOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
        ],
    });
    let button = json!({ "type": "string", "enum": ["left", "right", "middle"] });
    let seconds = |about: &str| json!({ "type": "number", "minimum": 0, "maximum": 30, "description": about });
    let one = |kind: &str, value: Json, more: Json| {
        let mut properties = json!({ kind: value });
        if let (Some(properties), Json::Object(more)) = (properties.as_object_mut(), more) {
            properties.extend(more);
        }
        json!({ "type": "object", "properties": properties, "required": [kind] })
    };
    json!({
        "type": "array",
        "minItems": 1,
        "maxItems": MOST_STEPS,
        "description": "The steps, in order. Each object does one thing, named by its key.",
        "items": {
            "anyOf": [
                one("move", place.clone(), json!({ "seconds": seconds("How long the move takes; 0, at once, unless it says.") })),
                one("down", button.clone(), json!({})),
                one("up", button.clone(), json!({})),
                one("click", place.clone(), json!({
                    "button": button.clone(),
                    "count": { "type": "integer", "minimum": 1, "maximum": 3, "description": "2 for a double-click." },
                    "interval": seconds("Seconds between clicks, 0.1 unless it says."),
                })),
                one("drag", place.clone(), json!({
                    "to": place.clone(),
                    "button": button,
                    "seconds": seconds("How long the move takes, 0.3 unless it says."),
                })),
                one("hover", place.clone(), json!({ "seconds": seconds("How long to stay, 1 unless it says.") })),
                one("scroll", json!({ "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Pixels across and down: [0, 120] scrolls down." }), json!({ "at": place.clone() })),
                one("key", json!({ "type": "string", "description": "A key chord, as the keymap writes them: \"Ctrl+Z\", \"Space\", \"Shift+F5\"." }), json!({})),
                one("hold", json!({ "type": "string", "description": "Modifiers to hold from now on: \"Shift\", \"Ctrl+Alt\"." }), json!({})),
                one("release", json!({ "type": "string", "description": "Modifiers to let go of." }), json!({})),
                one("text", json!({ "type": "string", "description": "Text to type into the field with the focus." }), json!({})),
                one("drop", json!({ "description": "A file's full path, or a list of them.", "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] }), json!({ "at": place })),
                one("wait", json!({ "type": "number", "minimum": 0, "maximum": LONGEST, "description": "Seconds to do nothing." }), json!({})),
            ],
        },
    })
}

#[cfg(test)]
mod tests {
    use musicable_commands::Key;

    use super::*;

    fn parse(steps: Json) -> Result<Vec<Step>, String> {
        let arguments = json!({ "steps": steps });
        super::steps(arguments.as_object().unwrap()).map_err(|refused| match refused {
            Refused::Arguments(why) => why,
            Refused::NoTool(name) => name,
        })
    }

    #[test]
    fn steps_are_read_with_their_defaults() {
        let kick = if cfg!(windows) {
            "C:\\audio\\kick.wav"
        } else {
            "/audio/kick.wav"
        };
        let steps = parse(json!([
            {"move": [10, 20.5]},
            {"click": "3f2a", "count": 2, "interval": 0.4},
            {"drag": "a", "to": [5, 5], "button": "right"},
            {"hover": "b", "seconds": 2},
            {"scroll": [0, 120]},
            {"key": "Ctrl+Z"},
            {"hold": "Shift+alt"},
            {"release": "Shift"},
            {"text": "Lead"},
            {"drop": kick, "at": "c"},
            {"down": "middle"},
            {"up": null},
            {"wait": 0.5},
        ]))
        .unwrap();
        assert_eq!(
            steps,
            [
                Step::Move {
                    to: Target::At([10.0, 20.5]),
                    seconds: 0.0
                },
                Step::Click {
                    at: Target::Node(0x3f2a),
                    button: Button::Left,
                    count: 2,
                    interval: 0.4
                },
                Step::Drag {
                    from: Target::Node(0xa),
                    to: Target::At([5.0, 5.0]),
                    button: Button::Right,
                    seconds: 0.3
                },
                Step::Hover {
                    at: Target::Node(0xb),
                    seconds: 2.0
                },
                Step::Scroll {
                    by: [0.0, 120.0],
                    at: None
                },
                Step::Key(KeyChord::new(Key::Z, Modifiers::CTRL)),
                Step::Hold(Modifiers::SHIFT | Modifiers::ALT),
                Step::Release(Modifiers::SHIFT),
                Step::Text("Lead".into()),
                Step::Drop {
                    files: vec![kick.into()],
                    at: Target::Node(0xc)
                },
                Step::Down(Button::Middle),
                Step::Up(Button::Left),
                Step::Wait(0.5),
            ]
        );
    }

    #[test]
    fn steps_that_make_no_sense_are_refused() {
        let cases = [
            (json!([]), "There are no steps."),
            (json!({"click": "a"}), "steps must be a list of steps."),
            (
                json!([{"click": "a", "move": "b"}]),
                "Step 1: a step does one thing, not move and click.",
            ),
            (
                json!([{"wave": 1}]),
                "Step 1: a step says what it does with one of move, down, up, click, drag, hover, scroll, key, hold, release, text, drop, wait.",
            ),
            (
                json!([{"click": "a", "to": "b"}]),
                "Step 1: a click step takes no to.",
            ),
            (
                json!([{"click": "a", "count": 4}]),
                "Step 1: count is 1, 2 or 3.",
            ),
            (
                json!([{"drag": "a"}]),
                "Step 1: a drag says where it goes, with to.",
            ),
            (
                json!([{"click": "zz"}]),
                "Step 1: a place is a node's id from get_ui_tree, such as \"3f2a\", or where in the window, in pixels: [120, 48].",
            ),
            (
                json!([{"wait": 1}, {"key": "Ctrl+Nope"}]),
                "Step 2: Ctrl+Nope: there's no key called \"Nope\".",
            ),
            (
                json!([{"hold": "Shift+Meta"}]),
                "Step 1: Meta isn't a modifier: Ctrl, Alt, Shift or Cmd.",
            ),
            (
                json!([{"down": "fourth"}]),
                "Step 1: there's no fourth button: left, right or middle.",
            ),
            (
                json!([{"drop": "kick.wav", "at": [1, 1]}]),
                "Step 1: kick.wav isn't a full path.",
            ),
            (
                json!([{"move": "a", "seconds": 31}]),
                "Step 1: seconds is from 0 to 30 seconds.",
            ),
            (
                json!([{"wait": 100}, {"wait": 30}]),
                "The steps take 130.0 seconds, and a call takes at most 120: split them over several calls.",
            ),
        ];
        for (steps, why) in cases {
            assert_eq!(parse(steps.clone()), Err(why.to_owned()), "{steps}");
        }
        let many = Json::Array(vec![json!({"wait": 0}); 201]);
        assert_eq!(
            parse(many),
            Err("There are 201 steps, and a call takes at most 200.".to_owned())
        );
    }
}

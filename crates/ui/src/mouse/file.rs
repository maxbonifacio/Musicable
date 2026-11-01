//! Mouse map files: what to change in the mouse map Musicable starts with.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use musicable_commands::Modifiers;

use super::{Gesture, MouseAction, MouseMap, Target, rank};

/// What a mouse map file's `format` field says, when it has one.
const FORMAT: &str = "musicable-mouse";
/// The mouse map file format version this build reads and writes.
const VERSION: i64 = 1;
/// What an entry gives a gesture to take its action away.
const NONE: &str = "none";

/// The comment at the top of the mouse map files Musicable writes.
const HEADER: &str = "\
# Musicable mouse map: changes to the mouse Musicable starts with, which works
# as Ableton Live's does.
#
# Each table is a place where gestures start, such as [clip_title], and each
# line under it gives a gesture there, with the modifiers held, what it does
# in place of what it did: \"Ctrl+Drag\" = \"move_free\". \"none\" takes that
# away, so the gesture does what it does without the modifiers, or nothing
# if none are held. Lines such as # \"Click\" = \"select\" show what a gesture
# does at first: take the # off to change it. Over each table, a comment
# lists what its gestures can do.
";

/// A mouse map file: changes to [`MouseMap::live`], the mouse map Musicable
/// starts with.
///
/// ```toml
/// format = "musicable-mouse"
/// version = 1
///
/// [clip_title]
/// "Ctrl+Drag" = "move_free"
/// "Alt+Drag" = "copy"
///
/// [clip_body]
/// "Drag" = "move"
/// "Double-click" = "none"
/// ```
///
/// Each table is a [`Target`], by its [`name`](Target::name), and each entry
/// under it gives a gesture there, with the modifiers held, the action it
/// does in place of the one it had: the modifiers and the gesture are
/// joined by `+`, as in key chords ([`Gesture::with`]). `"none"` takes the
/// action away, so the gesture does what it does without the modifiers, or
/// nothing if none are held. Gestures the file leaves out keep their
/// actions.
///
/// A bad entry doesn't stop the rest of the file from loading:
/// [`parse`](Self::parse) skips it and reports a [`MouseProblem`]. So does
/// an action the gesture can't do there ([`Target::actions`]). `format` and
/// `version` are optional, so a hand-written file can start at its first
/// table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MouseMapFile {
    /// What each gesture it changes does, by where it starts, the gesture
    /// and the modifiers held. `None` takes the action away.
    changes: BTreeMap<(Target, Gesture, Modifiers), Option<MouseAction>>,
}

impl MouseMapFile {
    /// Reads a mouse map file. Errors mean the file as a whole can't be
    /// used. Problems with single entries come back with the file, and the
    /// entries are skipped.
    pub fn parse(text: &str) -> Result<(Self, Vec<MouseProblem>), MouseFileError> {
        let mut table: toml::Table = toml::from_str(text).map_err(MouseFileError::Toml)?;
        match table.remove("format") {
            None => {}
            Some(toml::Value::String(format)) if format == FORMAT => {}
            Some(toml::Value::String(format)) => return Err(MouseFileError::NotAMouseMap(format)),
            Some(other) => return Err(MouseFileError::NotAMouseMap(other.to_string())),
        }
        match table.remove("version") {
            None => {}
            Some(toml::Value::Integer(VERSION)) => {}
            Some(other) => return Err(MouseFileError::UnsupportedVersion(other.to_string())),
        }

        let mut file = Self::default();
        let mut problems = Vec::new();
        let mut twice = BTreeSet::new();
        for (place, value) in &table {
            let Some(target) = Target::from_name(place) else {
                problems.push(MouseProblem::UnknownPlace {
                    place: place.clone(),
                });
                continue;
            };
            let toml::Value::Table(entries) = value else {
                problems.push(MouseProblem::NotGestures {
                    place: place.clone(),
                });
                continue;
            };
            for (written, value) in entries {
                match entry(target, written, value) {
                    Ok((gesture, mods, action)) => {
                        let key = (target, gesture, mods);
                        if file.changes.insert(key, action).is_some() {
                            twice.insert(key);
                        }
                    }
                    Err(problem) => problems.push(problem),
                }
            }
        }
        // Two spellings of one gesture, such as "Ctrl+Drag" and
        // "control+drag": neither counts.
        for (target, gesture, mods) in twice {
            file.changes.remove(&(target, gesture, mods));
            problems.push(MouseProblem::Twice {
                target,
                gesture,
                mods,
            });
        }
        Ok((file, problems))
    }

    /// Reads the mouse map file at `path`.
    pub fn read(path: &Path) -> Result<(Self, Vec<MouseProblem>), MouseFileError> {
        Self::parse(&fs::read_to_string(path).map_err(MouseFileError::Io)?)
    }

    /// The changes that turn [`MouseMap::live`] into `map`: an entry for
    /// each gesture whose action differs.
    pub fn diff(map: &MouseMap) -> Self {
        let live = MouseMap::live();
        let mut changes = BTreeMap::new();
        for (target, gesture, mods, action) in live.iter() {
            let now = map.bound(target, gesture, mods);
            if now != Some(action) {
                changes.insert((target, gesture, mods), now);
            }
        }
        for (target, gesture, mods, action) in map.iter() {
            if live.bound(target, gesture, mods) != Some(action) {
                changes.insert((target, gesture, mods), Some(action));
            }
        }
        Self { changes }
    }

    /// The mouse map: [`MouseMap::live`] with the file's changes.
    pub fn mouse_map(&self) -> MouseMap {
        let mut map = MouseMap::live();
        for (&(target, gesture, mods), &action) in &self.changes {
            match action {
                Some(action) => map.bind(target, gesture, mods, action),
                None => map.unbind(target, gesture, mods),
            };
        }
        map
    }

    /// The file as TOML: a table for every place, with every gesture that
    /// does something there at first, commented out unless the file changes
    /// it, to show what can be changed.
    pub fn to_toml(&self) -> String {
        let live = MouseMap::live();
        let mut text = format!("{HEADER}\nformat = \"{FORMAT}\"\nversion = {VERSION}\n");
        for target in Target::ALL {
            text += &format!("\n# {}.\n", target.title());
            // The gestures that can do the same, together.
            let mut can: Vec<(Vec<&str>, &[MouseAction])> = Vec::new();
            for gesture in target.gestures() {
                let actions = target.actions(gesture);
                match can.iter_mut().find(|(_, same)| *same == actions) {
                    Some((gestures, _)) => gestures.push(gesture.name()),
                    None => can.push((vec![gesture.name()], actions)),
                }
            }
            for (gestures, actions) in can {
                let names: Vec<&str> = actions.iter().map(|action| action.name()).collect();
                text += &format!("#   {}: {}\n", gestures.join(", "), names.join(", "));
            }
            text += &format!("[{}]\n", target.name());
            let mut lines: BTreeMap<_, (&str, Option<MouseAction>)> = BTreeMap::new();
            for (gesture, mods, action) in live.on(target) {
                lines.insert((gesture, rank(mods), mods), ("# ", Some(action)));
            }
            for (&(on, gesture, mods), &action) in &self.changes {
                if on == target {
                    lines.insert((gesture, rank(mods), mods), ("", action));
                }
            }
            for ((gesture, _, mods), (mark, action)) in lines {
                let action = action.map_or(NONE, MouseAction::name);
                text += &format!("{mark}\"{}\" = \"{action}\"\n", gesture.with(mods));
            }
        }
        text
    }

    /// Writes the file to `path`, creating its folder if need be.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_toml())
    }

    /// How many gestures the file changes.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// The entry `written = value` under `target`'s table: the gesture, the
/// modifiers, and what it does, `None` to take its action away.
fn entry(
    target: Target,
    written: &str,
    value: &toml::Value,
) -> Result<(Gesture, Modifiers, Option<MouseAction>), MouseProblem> {
    let place = target.name().to_owned();
    let (gesture, mods) = Gesture::parse(written).map_err(|why| MouseProblem::BadGesture {
        place: place.clone(),
        gesture: written.to_owned(),
        why,
    })?;
    let Some(name) = value.as_str() else {
        return Err(MouseProblem::NotAnAction {
            place,
            gesture: written.to_owned(),
        });
    };
    if name.trim().eq_ignore_ascii_case(NONE) {
        return Ok((gesture, mods, None));
    }
    let Some(action) = MouseAction::from_name(name) else {
        return Err(MouseProblem::UnknownAction {
            place,
            gesture: written.to_owned(),
            action: name.to_owned(),
        });
    };
    if !target.actions(gesture).contains(&action) {
        return Err(MouseProblem::CantDo {
            target,
            gesture,
            mods,
            action,
        });
    }
    Ok((gesture, mods, Some(action)))
}

/// Something wrong with one entry of a mouse map file. The rest of the file
/// still loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseProblem {
    /// The file has a table for a place this version doesn't have, so its
    /// entries are skipped.
    UnknownPlace { place: String },
    /// A place's entry isn't a table of gestures, so it's skipped.
    NotGestures { place: String },
    /// An entry's name isn't a gesture, so it's skipped.
    BadGesture {
        place: String,
        gesture: String,
        why: String,
    },
    /// An entry's value isn't the name of an action, so the gesture keeps
    /// its action.
    NotAnAction { place: String, gesture: String },
    /// No action has the name the entry gives, so the gesture keeps its
    /// action.
    UnknownAction {
        place: String,
        gesture: String,
        action: String,
    },
    /// The gesture can't do the action there, so it keeps its own.
    CantDo {
        target: Target,
        gesture: Gesture,
        mods: Modifiers,
        action: MouseAction,
    },
    /// Entries spelled differently give the gesture an action, such as
    /// "Ctrl+Drag" and "control+drag", so it keeps its own.
    Twice {
        target: Target,
        gesture: Gesture,
        mods: Modifiers,
    },
}

impl fmt::Display for MouseProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlace { place } => write!(
                f,
                "there's no place called \"{place}\", so the gestures under [{place}] are left \
                 out"
            ),
            Self::NotGestures { place } => write!(
                f,
                "{place} should be a table of gestures, such as [{place}] with \"Drag\" = \
                 \"move\" under it, so it's left out"
            ),
            Self::BadGesture {
                place,
                gesture,
                why,
            } => write!(f, "\"{gesture}\" under [{place}] is left out: {why}"),
            Self::NotAnAction { place, gesture } => write!(
                f,
                "\"{gesture}\" under [{place}] keeps its action: it should be given the name of \
                 one, or \"{NONE}\""
            ),
            Self::UnknownAction {
                place,
                gesture,
                action,
            } => write!(
                f,
                "\"{gesture}\" under [{place}] keeps its action: there's no action called \
                 \"{action}\""
            ),
            Self::CantDo {
                target,
                gesture,
                mods,
                action,
            } => {
                let can: Vec<String> = (target.actions(*gesture).iter())
                    .map(|action| format!("\"{}\"", action.name()))
                    .collect();
                write!(
                    f,
                    "\"{}\" under [{}] keeps its action: it can't \"{}\" there",
                    gesture.with(*mods),
                    target.name(),
                    action.name()
                )?;
                match can.as_slice() {
                    [] => f.write_str(", or anything else"),
                    [one] => write!(f, ", only {one}"),
                    [rest @ .., last] => write!(f, ", only {} or {last}", rest.join(", ")),
                }
            }
            Self::Twice {
                target,
                gesture,
                mods,
            } => write!(
                f,
                "\"{}\" is under [{}] more than once, so it keeps its action",
                gesture.with(*mods),
                target.name()
            ),
        }
    }
}

/// Why a mouse map file can't be used at all.
#[derive(Debug)]
pub enum MouseFileError {
    Io(io::Error),
    /// Not valid TOML.
    Toml(toml::de::Error),
    /// The `format` field names another kind of file.
    NotAMouseMap(String),
    /// A version this build doesn't read, such as one from a newer build.
    UnsupportedVersion(String),
}

impl fmt::Display for MouseFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read the mouse map file: {error}"),
            Self::Toml(error) => write!(f, "the mouse map file isn't valid: {error}"),
            Self::NotAMouseMap(format) => {
                write!(f, "this is a \"{format}\" file, not a \"{FORMAT}\" file")
            }
            Self::UnsupportedVersion(version) => write!(
                f,
                "the mouse map file is version {version}, and this version of Musicable reads \
                 version {VERSION}"
            ),
        }
    }
}

impl std::error::Error for MouseFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Toml(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_change_the_mouse_musicable_starts_with() {
        let text = "\
[clip_title]
\"Ctrl+Drag\" = \"move_free\"
\"alt + drag\" = \"copy\"

[clip_body]
Drag = \"move\"
DoubleClick = \"none\"
";
        let (file, problems) = MouseMapFile::parse(text).unwrap();
        assert_eq!(problems, []);
        assert_eq!(file.len(), 4);
        let map = file.mouse_map();
        let drag = |target, mods| map.action(target, Gesture::Drag, mods);
        assert_eq!(
            drag(Target::ClipTitle, Modifiers::CTRL),
            Some(MouseAction::MoveFree)
        );
        assert_eq!(
            drag(Target::ClipTitle, Modifiers::ALT),
            Some(MouseAction::Copy)
        );
        assert_eq!(
            drag(Target::ClipBody, Modifiers::NONE),
            Some(MouseAction::Move)
        );
        let double = map.action(Target::ClipBody, Gesture::DoubleClick, Modifiers::NONE);
        assert_eq!(double, None);
        // What the file leaves out keeps Live's.
        assert_eq!(
            drag(Target::ClipTitle, Modifiers::CMD),
            Some(MouseAction::Copy)
        );
        assert_eq!(
            drag(Target::Note, Modifiers::ALT),
            Some(MouseAction::Velocity)
        );
        assert_eq!(MouseMapFile::diff(&map), file);
    }

    #[test]
    fn bad_entries_are_skipped_and_said() {
        let text = "\
format = \"musicable-mouse\"
version = 1
ruler = 3

[clip_title]
\"Ctrl+Drag\" = \"move_free\"
\"Hyper+Drag\" = \"copy\"
\"Ctrl+Fling\" = \"copy\"
\"Drag\" = 5
\"Click\" = \"teleport\"
\"Shift+Drag\" = \"fade\"
\"Alt+Drag\" = \"copy\"
\"alt+DRAG\" = \"move\"

[attic]
\"Click\" = \"select\"
";
        let (file, problems) = MouseMapFile::parse(text).unwrap();
        let said: Vec<String> = problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            said,
            [
                "there's no place called \"attic\", so the gestures under [attic] are left out",
                "\"Click\" under [clip_title] keeps its action: there's no action called \
                 \"teleport\"",
                "\"Ctrl+Fling\" under [clip_title] is left out: there's no gesture called \
                 \"Fling\" (they are Click, Double-click, Drag, Wheel, Middle-drag and \
                 Right-drag)",
                "\"Drag\" under [clip_title] keeps its action: it should be given the name of \
                 one, or \"none\"",
                "\"Hyper+Drag\" under [clip_title] is left out: \"Hyper\" isn't a modifier (they \
                 are Ctrl, Alt, Shift and Cmd)",
                "\"Shift+Drag\" under [clip_title] keeps its action: it can't \"fade\" there, \
                 only \"move\", \"copy\", \"move_free\", \"select_time\", \"pan\" or \
                 \"zoom_to_area\"",
                "ruler should be a table of gestures, such as [ruler] with \"Drag\" = \
                 \"move\" under it, so it's left out",
                "\"Alt+Drag\" is under [clip_title] more than once, so it keeps its action",
            ]
        );
        // The good one applies.
        assert_eq!(file.len(), 1);
        let map = file.mouse_map();
        let drag = |mods| map.action(Target::ClipTitle, Gesture::Drag, mods);
        assert_eq!(drag(Modifiers::CTRL), Some(MouseAction::MoveFree));
        assert_eq!(drag(Modifiers::ALT), Some(MouseAction::MoveFree));
    }

    #[test]
    fn files_of_another_kind_or_version_are_not_used() {
        let error = |text| MouseMapFile::parse(text).unwrap_err().to_string();
        assert_eq!(
            error("format = \"musicable-keymap\""),
            "this is a \"musicable-keymap\" file, not a \"musicable-mouse\" file"
        );
        assert_eq!(
            error("version = 2"),
            "the mouse map file is version 2, and this version of Musicable reads version 1"
        );
        assert!(error("[clip_title").starts_with("the mouse map file isn't valid"));
        assert!(MouseMapFile::parse("").unwrap().0.is_empty());
    }

    #[test]
    fn written_files_read_back_and_show_what_can_change() {
        let mut map = MouseMap::live();
        map.bind(
            Target::Note,
            Gesture::Drag,
            Modifiers::SHIFT,
            MouseAction::Velocity,
        );
        map.unbind(Target::ClipBody, Gesture::DoubleClick, Modifiers::NONE);
        let file = MouseMapFile::diff(&map);
        assert_eq!(file.len(), 2);
        let text = file.to_toml();
        assert!(text.starts_with("# Musicable mouse map"), "{text}");
        let (read, problems) = MouseMapFile::parse(&text).unwrap();
        assert_eq!(problems, []);
        assert_eq!(read, file);
        assert_eq!(read.mouse_map(), map);

        // Every place, with what its gestures can do over it, and what they
        // do at first commented out.
        for target in Target::ALL {
            assert!(
                text.contains(&format!("\n[{}]\n", target.name())),
                "{target:?}"
            );
        }
        let note = "\
# A note.
#   Click, Double-click: select, toggle_select, delete_note
#   Drag: move, copy, velocity
[note]
# \"Click\" = \"select\"
# \"Ctrl+Click\" = \"toggle_select\"
# \"Shift+Click\" = \"toggle_select\"
# \"Cmd+Click\" = \"toggle_select\"
# \"Double-click\" = \"delete_note\"
# \"Drag\" = \"move\"
# \"Ctrl+Drag\" = \"copy\"
# \"Alt+Drag\" = \"velocity\"
\"Shift+Drag\" = \"velocity\"
# \"Cmd+Drag\" = \"copy\"
";
        assert!(text.contains(note), "{text}");
        assert!(text.contains("\n\"Double-click\" = \"none\"\n"), "{text}");
        // A file of Live's mouse changes nothing.
        let unchanged = MouseMapFile::diff(&MouseMap::live());
        assert!(unchanged.is_empty());
        assert!(
            MouseMapFile::parse(&unchanged.to_toml())
                .unwrap()
                .0
                .is_empty()
        );
    }
}

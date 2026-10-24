//! Workspaces: how the panels are laid out, in splits and tabs, in each
//! window, and the parts of the transport bar, the track headers and the
//! mixer strips, as the app keeps them in a file in the config folder.
//!
//! ```toml
//! format = "musicable-workspace"
//! version = 1
//! slot = 2
//!
//! [views.mixer_strip]
//! hidden = ["monitor"]
//! parts = ["name", "output", "input", "monitor", "sends", "pan", "fader", "readouts", "buttons", "clip_type"]
//!
//! [[windows]]
//! at = [-1912, 120]
//! size = [1600, 900]
//! maximized = true
//! hidden = ["piano_roll"]
//! layout = { across = [
//!     { panel = "browser", size = 260 },
//!     { down = [
//!         { panel = "arrangement" },
//!         { panel = "mixer", size = 300 },
//!         { tabs = ["devices", "piano_roll"], front = "devices" },
//!     ] },
//! ] }
//! ```
//!
//! `slot` is the workspace's slot, which the key that switches to it goes
//! by. A window's `at` is where its top left corner was left on the
//! desktop, in pixels, which a monitor left of the main one makes negative.
//! `size` is how big the inside of it was, in pixels at 100% scaling, which
//! its monitor's scaling makes bigger. `maximized` says it filled its
//! monitor, and then `at` and `size` are where it goes back to.
//!
//! A part's `size` is its width in a row of parts (`across`), or its height
//! in a column (`down`), in points; without one it takes its panel's
//! default size. The part with the arrangement takes what the others leave.
//! Hidden panels keep their place, to come back to. Panels the file doesn't
//! know are left out, and panels it doesn't have go where they go in the
//! default layout, hidden. `views` has only the views the user changed
//! ([`Views`]).

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::parts::Views;

const FORMAT: &str = "musicable-workspace";
const VERSION: u32 = 1;

/// The layout of each window's panels, and the views' parts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// The slot the workspace is in, from 1, which the key that switches to
    /// it goes by. Default is always in the first, so its file needn't say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// What the user changed about the transport bar, the track headers
    /// and the mixer strips, which look the same in every window.
    #[serde(skip_serializing_if = "Views::is_empty")]
    pub views: Views,
    /// The main window's first.
    pub windows: Vec<WindowLayout>,
}

/// Where a window is, and how its panels are laid out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
    /// Where the window's top left corner is on the desktop, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[i32; 2]>,
    /// How big the inside of the window is, in pixels at 100% scaling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    /// Whether the window fills its monitor. `at` and `size` are where it
    /// goes back to.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    /// The panels hidden, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    pub layout: Layout,
}

/// A window's panels, or a part of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(flatten)]
    pub split: Split,
    /// The part's width in a row, or its height in a column, in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// The panel whose tab is in front, of tabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front: Option<String>,
}

/// How a part of a window is split: not at all, into tabs, or into parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    /// One panel, by id.
    Panel(String),
    /// Panels in tabs, by id, left to right.
    Tabs(Vec<String>),
    /// Parts side by side, left to right.
    Across(Vec<Layout>),
    /// Parts one over another, top to bottom.
    Down(Vec<Layout>),
}

impl Default for Layout {
    /// No panels.
    fn default() -> Self {
        Self::new(Split::Across(Vec::new()))
    }
}

impl Layout {
    /// The panel with id `id`, at its default size.
    pub fn panel(id: &str) -> Self {
        Self::new(Split::Panel(id.to_owned()))
    }

    pub fn new(split: Split) -> Self {
        Self {
            split,
            size: None,
            front: None,
        }
    }
}

impl Workspace {
    /// Reads a workspace from the text of a workspace file.
    pub fn parse(text: &str) -> Result<Self, WorkspaceError> {
        #[derive(Deserialize)]
        struct Raw {
            format: Option<String>,
            version: Option<u32>,
            #[serde(flatten)]
            workspace: Workspace,
        }

        let raw: Raw = toml::from_str(text).map_err(WorkspaceError::Toml)?;
        if let Some(format) = raw.format.filter(|f| f != FORMAT) {
            return Err(WorkspaceError::NotWorkspace(format));
        }
        match raw.version.unwrap_or(VERSION) {
            VERSION => Ok(raw.workspace),
            version => Err(WorkspaceError::UnsupportedVersion(version)),
        }
    }

    pub fn to_toml(&self) -> String {
        let mut text = format!("format = \"{FORMAT}\"\nversion = {VERSION}\n");
        // What isn't the windows, as serde writes it.
        let mut rest = match toml::Value::try_from(self) {
            Ok(toml::Value::Table(table)) => table,
            _ => toml::Table::new(),
        };
        rest.remove("windows");
        if !rest.is_empty() {
            text.push('\n');
            text.push_str(&toml::to_string_pretty(&rest).expect("workspaces always serialize"));
        }
        // The windows' layouts inline, a part to a line, so the splits
        // show.
        for window in &self.windows {
            text.push_str("\n[[windows]]\n");
            if let Some([x, y]) = window.at {
                let _ = writeln!(text, "at = [{x}, {y}]");
            }
            if let Some([width, height]) = window.size {
                let _ = writeln!(text, "size = [{width}, {height}]");
            }
            if window.maximized {
                text.push_str("maximized = true\n");
            }
            if !window.hidden.is_empty() {
                let hidden = toml::Value::from(window.hidden.clone());
                let _ = writeln!(text, "hidden = {hidden}");
            }
            text.push_str("layout = ");
            write_layout(&mut text, &window.layout, 0);
            text.push('\n');
        }
        text
    }
}

/// Writes `layout` as an inline table, with each of its parts on a line of
/// its own, `depth` levels in.
fn write_layout(text: &mut String, layout: &Layout, depth: usize) {
    let quoted = |id: &str| toml::Value::from(id).to_string();
    text.push_str("{ ");
    match &layout.split {
        Split::Panel(id) => {
            let _ = write!(text, "panel = {}", quoted(id));
        }
        Split::Tabs(ids) => {
            let _ = write!(text, "tabs = {}", toml::Value::from(ids.clone()));
        }
        Split::Across(parts) | Split::Down(parts) => {
            let key = match layout.split {
                Split::Across(_) => "across",
                _ => "down",
            };
            let _ = writeln!(text, "{key} = [");
            for part in parts {
                text.push_str(&"    ".repeat(depth + 1));
                write_layout(text, part, depth + 1);
                text.push_str(",\n");
            }
            text.push_str(&"    ".repeat(depth));
            text.push(']');
        }
    }
    if let Some(size) = layout.size {
        let _ = write!(text, ", size = {size}");
    }
    if let Some(front) = &layout.front {
        let _ = write!(text, ", front = {}", quoted(front));
    }
    text.push_str(" }");
}

/// The longest a workspace's name can be, in characters.
pub const MAX_NAME: usize = 64;

/// `name` trimmed, if it can name a workspace, the file it's kept in being
/// named after it: or else why it can't. `taken` has the names of the
/// workspaces there are, which it can't be, whatever their case, as
/// Windows doesn't tell file names apart by case.
pub fn check_name<'a>(
    name: &str,
    mut taken: impl Iterator<Item = &'a str>,
) -> Result<String, String> {
    let name = name.trim();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    let numbered = ["COM", "LPT"];
    let upper = name.to_uppercase();
    let mut chars = upper.chars();
    let last = chars.next_back();
    let device = reserved.contains(&upper.as_str())
        || (numbered.contains(&chars.as_str()) && last.is_some_and(|c| matches!(c, '1'..='9')));
    if name.is_empty() {
        Err("Type a name.".to_owned())
    } else if let Some(c) = name
        .chars()
        .find(|&c| "\\/:*?\"<>|".contains(c) || c.is_control())
    {
        let shown = if c.is_control() {
            "a control character".to_owned()
        } else {
            c.to_string()
        };
        Err(format!("A name can't have {shown} in it."))
    } else if name.ends_with('.') {
        Err("A name can't end in a dot.".to_owned())
    } else if device {
        Err(format!("Windows keeps the name {name} for itself."))
    } else if name.chars().count() > MAX_NAME {
        Err(format!(
            "A name can't be longer than {MAX_NAME} characters."
        ))
    } else if taken.any(|other| other.to_lowercase() == name.to_lowercase()) {
        Err(format!("There's a workspace called {name} already."))
    } else {
        Ok(name.to_owned())
    }
}

/// Why a workspace file can't be used.
#[derive(Debug)]
pub enum WorkspaceError {
    Toml(toml::de::Error),
    /// The file's `format` says it's another kind of file.
    NotWorkspace(String),
    /// The file is from a version of Musicable that writes another version.
    UnsupportedVersion(u32),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(error) => write!(f, "the workspace file isn't valid: {error}"),
            Self::NotWorkspace(format) => {
                write!(f, "this is a \"{format}\" file, not a \"{FORMAT}\" file")
            }
            Self::UnsupportedVersion(version) => write!(
                f,
                "the workspace file is version {version}, and this version of Musicable reads \
                 version {VERSION}"
            ),
        }
    }
}

impl std::error::Error for WorkspaceError {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::parts::Lineup;

    fn sized(size: u32, layout: Layout) -> Layout {
        Layout {
            size: Some(size),
            ..layout
        }
    }

    #[test]
    fn workspaces_survive_a_round_trip() {
        let tabs = Layout {
            front: Some("piano_roll".into()),
            ..Layout::new(Split::Tabs(vec!["devices".into(), "piano_roll".into()]))
        };
        let workspace = Workspace {
            slot: Some(3),
            windows: vec![WindowLayout {
                at: Some([-1280, 40]),
                size: Some([1280, 800]),
                maximized: true,
                hidden: vec!["browser".into()],
                layout: Layout::new(Split::Across(vec![
                    sized(260, Layout::panel("browser")),
                    Layout::new(Split::Down(vec![
                        Layout::panel("arrangement"),
                        sized(300, Layout::panel("mixer")),
                        sized(240, tabs),
                    ])),
                ])),
            }],
            ..Workspace::default()
        };
        let text = workspace.to_toml();
        let written = r#"format = "musicable-workspace"
version = 1

slot = 3

[[windows]]
at = [-1280, 40]
size = [1280, 800]
maximized = true
hidden = ["browser"]
layout = { across = [
    { panel = "browser", size = 260 },
    { down = [
        { panel = "arrangement" },
        { panel = "mixer", size = 300 },
        { tabs = ["devices", "piano_roll"], size = 240, front = "piano_roll" },
    ] },
] }
"#;
        assert_eq!(text, written);
        assert_eq!(Workspace::parse(&text).unwrap(), workspace);
        assert_eq!(
            Workspace::parse(&Workspace::default().to_toml()).unwrap(),
            Workspace::default()
        );
    }

    #[test]
    fn the_views_parts_go_before_the_windows() {
        let lineup = Lineup {
            parts: vec!["cpu".into(), "position".into()],
            hidden: vec!["capture".into()],
            sizes: BTreeMap::from([("cpu".into(), 80)]),
        };
        let workspace = Workspace {
            views: Views(BTreeMap::from([("transport_bar".into(), lineup)])),
            windows: vec![WindowLayout {
                layout: Layout::panel("arrangement"),
                ..WindowLayout::default()
            }],
            ..Workspace::default()
        };
        let text = workspace.to_toml();
        let written = r#"format = "musicable-workspace"
version = 1

[views.transport_bar]
hidden = ["capture"]
parts = [
    "cpu",
    "position",
]

[views.transport_bar.sizes]
cpu = 80

[[windows]]
layout = { panel = "arrangement" }
"#;
        assert_eq!(text, written);
        assert_eq!(Workspace::parse(&text).unwrap(), workspace);
    }

    #[test]
    fn workspace_files_can_leave_things_out() {
        assert_eq!(Workspace::parse("").unwrap(), Workspace::default());
        let text = "[[windows]]\nlayout = { down = [{ panel = \"mixer\", later = 1 }] }\n";
        let workspace = Workspace::parse(text).unwrap();
        let layout = Layout::new(Split::Down(vec![Layout::panel("mixer")]));
        assert_eq!(
            workspace.windows,
            [WindowLayout {
                layout,
                ..WindowLayout::default()
            }]
        );
    }

    #[test]
    fn names_are_names_files_can_have() {
        let taken = ["Default", "Mixing"];
        let check = |name| check_name(name, taken.into_iter());
        assert_eq!(check("  Editing "), Ok("Editing".to_owned()));
        assert_eq!(check("Vocals 2.0"), Ok("Vocals 2.0".to_owned()));
        let problems = [
            ("", "Type a name."),
            ("   ", "Type a name."),
            ("mixing", "There's a workspace called mixing already."),
            ("Mix/Edit", "A name can't have / in it."),
            ("What?", "A name can't have ? in it."),
            ("Tab\there", "A name can't have a control character in it."),
            ("Final.", "A name can't end in a dot."),
            ("con", "Windows keeps the name con for itself."),
            ("LPT3", "Windows keeps the name LPT3 for itself."),
        ];
        for (name, problem) in problems {
            assert_eq!(check(name), Err(problem.to_owned()), "{name:?}");
        }
        assert!(check("COM10").is_ok());
        let long = "x".repeat(MAX_NAME + 1);
        assert_eq!(
            check(&long),
            Err("A name can't be longer than 64 characters.".to_owned())
        );
    }

    #[test]
    fn other_files_are_not_workspaces() {
        let settings = Workspace::parse("format = \"musicable-settings\"\n").unwrap_err();
        assert_eq!(
            settings.to_string(),
            "this is a \"musicable-settings\" file, not a \"musicable-workspace\" file"
        );
        let later = Workspace::parse("version = 2\n").unwrap_err();
        assert!(matches!(later, WorkspaceError::UnsupportedVersion(2)));
        assert!(matches!(
            Workspace::parse("windows = 3"),
            Err(WorkspaceError::Toml(_))
        ));
    }
}

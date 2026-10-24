//! Workspaces: how the window's panels are laid out, in splits and tabs, as
//! the app keeps it in a file in the config folder.
//!
//! ```toml
//! format = "musicable-workspace"
//! version = 1
//!
//! [[windows]]
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
//! A part's `size` is its width in a row of parts (`across`), or its height
//! in a column (`down`), in points; without one it takes its panel's
//! default size. The part with the arrangement takes what the others leave.
//! Hidden panels keep their place, to come back to. Panels the file doesn't
//! know are left out, and panels it doesn't have go where they go in the
//! default layout, hidden.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

const FORMAT: &str = "musicable-workspace";
const VERSION: u32 = 1;

/// The layout of each window's panels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// The main window's first.
    pub windows: Vec<WindowLayout>,
}

/// How a window's panels are laid out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
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
    use super::*;

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
            windows: vec![WindowLayout {
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
        };
        let text = workspace.to_toml();
        let written = r#"format = "musicable-workspace"
version = 1

[[windows]]
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
    fn workspace_files_can_leave_things_out() {
        assert_eq!(Workspace::parse("").unwrap(), Workspace::default());
        let text = "[[windows]]\nlayout = { down = [{ panel = \"mixer\", later = 1 }] }\n";
        let workspace = Workspace::parse(text).unwrap();
        let layout = Layout::new(Split::Down(vec![Layout::panel("mixer")]));
        assert_eq!(
            workspace.windows,
            [WindowLayout {
                hidden: Vec::new(),
                layout
            }]
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

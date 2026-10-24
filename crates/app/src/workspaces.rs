//! The user's workspaces, which lay the window's panels out: a file to
//! each in the `workspaces` folder in the config folder. There's one so
//! far, `Default.toml`, which the window starts with and which keeps the
//! layout as the user changes it.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_ui::{Workspace, WorkspaceError};

/// The folder of workspace files in the config folder.
pub const WORKSPACES_FOLDER: &str = "workspaces";
/// The file of the workspace the window starts with, in the workspaces
/// folder.
pub const DEFAULT_WORKSPACE: &str = "Default.toml";

/// The default workspace's file in the config folder `config_dir`.
pub fn default_file(config_dir: &Path) -> PathBuf {
    config_dir.join(WORKSPACES_FOLDER).join(DEFAULT_WORKSPACE)
}

/// Reads the workspace file at `path`: `None` with no file there.
pub fn read(path: &Path) -> Result<Option<Workspace>, ReadError> {
    match fs::read_to_string(path) {
        Ok(text) => Workspace::parse(&text)
            .map(Some)
            .map_err(ReadError::Workspace),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ReadError::Io(error)),
    }
}

/// Writes `workspace` to `path`, creating its folder if need be.
pub fn write(path: &Path, workspace: &Workspace) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, workspace.to_toml())
}

/// Why a workspace file can't be read.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Workspace(WorkspaceError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read the workspace file: {error}"),
            Self::Workspace(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ReadError {}

#[cfg(test)]
mod tests {
    use musicable_ui::{Layout, Split, WindowLayout};

    use super::*;

    #[test]
    fn workspaces_are_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("musicable-workspaces-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = default_file(&dir);
        assert!(read(&path).unwrap().is_none());
        let workspace = Workspace {
            windows: vec![WindowLayout {
                hidden: vec!["mixer".into()],
                layout: Layout::new(Split::Down(vec![
                    Layout::panel("arrangement"),
                    Layout::panel("mixer"),
                ])),
            }],
            ..Workspace::default()
        };
        write(&path, &workspace).unwrap();
        assert_eq!(read(&path).unwrap(), Some(workspace));
        fs::write(&path, "format = \"musicable-settings\"\n").unwrap();
        let error = read(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            "this is a \"musicable-settings\" file, not a \"musicable-workspace\" file"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

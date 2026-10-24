//! The user's workspaces, which lay the window's panels out: a file to each
//! in the `workspaces` folder in the config folder, named after it, such as
//! `Mixing.toml`. Default is always there, in `Default.toml` once the user
//! has changed it.
//!
//! Each workspace has a slot, from 1 to 9, for the command and the key that
//! switch to it: Default the first, and a new one the lowest slot free. Its
//! file keeps the slot, so it's the same the next time the app opens.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_commands::workspace::SLOTS;
use musicable_ui::{
    DEFAULT_WORKSPACE, Monitor, WindowLayout, Workspace, WorkspaceError, on_a_monitor,
};
use winit::dpi::{LogicalSize, PhysicalPosition};

/// The folder of workspace files in the config folder.
pub const WORKSPACES_FOLDER: &str = "workspaces";
/// The extension of workspace files.
const EXTENSION: &str = "toml";

/// The user's workspaces, in their slots, and the one in use.
#[derive(Debug, Clone, PartialEq)]
pub struct Workspaces {
    /// The folder of their files, if there's a config folder.
    folder: Option<PathBuf>,
    /// The name of each slot's workspace, the first being Default's.
    slots: Vec<Option<String>>,
    /// The slot of the workspace in use, from 0.
    current: usize,
}

impl Workspaces {
    /// The workspaces in the workspaces folder in the config folder
    /// `config_dir`, with Default in use. What keeps any out goes in
    /// `messages`.
    pub fn list(config_dir: Option<&Path>, messages: &mut Vec<String>) -> Self {
        let folder = config_dir.map(|dir| dir.join(WORKSPACES_FOLDER));
        let mut slots = vec![None; usize::from(SLOTS)];
        slots[0] = Some(DEFAULT_WORKSPACE.to_owned());
        let mut found = Vec::new();
        if let Some(folder) = &folder {
            match fs::read_dir(folder) {
                Ok(entries) => found = files(entries.flatten().map(|entry| entry.path())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => messages.push(format!(
                    "Couldn't list the workspaces in {}: {error}.",
                    folder.display()
                )),
            }
        }
        // A file's own slot, if no file before it in name order has it,
        // then the lowest slot free.
        let mut unplaced = Vec::new();
        for (name, slot) in found {
            match slot.and_then(|slot| slots.get_mut(slot)) {
                Some(free @ None) => *free = Some(name),
                _ => unplaced.push(name),
            }
        }
        let mut left_out = Vec::new();
        for name in unplaced {
            match slots.iter_mut().find(|slot| slot.is_none()) {
                Some(free) => *free = Some(name),
                None => left_out.push(name),
            }
        }
        if !left_out.is_empty() {
            messages.push(format!(
                "There's room for {SLOTS} workspaces, so Musicable leaves out {}.",
                left_out.join(", ")
            ));
        }
        Self {
            folder,
            slots,
            current: 0,
        }
    }

    /// The name of each slot's workspace, the first being Default's.
    pub fn slots(&self) -> &[Option<String>] {
        &self.slots
    }

    /// The slot of the workspace in use, from 0.
    pub fn current(&self) -> usize {
        self.current
    }

    /// The name of the workspace in use.
    pub fn current_name(&self) -> &str {
        self.name(self.current).unwrap_or(DEFAULT_WORKSPACE)
    }

    /// The name of the workspace in `slot`, if it has one.
    pub fn name(&self, slot: usize) -> Option<&str> {
        self.slots.get(slot)?.as_deref()
    }

    /// The slot of the workspace called `name`, if there's one.
    pub fn slot_named(&self, name: &str) -> Option<usize> {
        (self.slots.iter()).position(|slot| slot.as_deref() == Some(name))
    }

    /// The file of the workspace in `slot`.
    pub fn file(&self, slot: usize) -> Option<PathBuf> {
        let name = self.name(slot)?;
        Some(self.folder.as_ref()?.join(format!("{name}.{EXTENSION}")))
    }

    /// Uses the workspace in `slot` from now on.
    pub fn set_current(&mut self, slot: usize) {
        if self.name(slot).is_some() {
            self.current = slot;
        }
    }

    /// Reads the workspace in `slot`: `None` while it has no file, as
    /// Default hasn't until the user changes it.
    pub fn read(&self, slot: usize) -> Result<Option<Workspace>, ReadError> {
        match self.file(slot) {
            Some(path) => read(&path),
            None => Ok(None),
        }
    }

    /// Writes `workspace` to the file of the workspace in `slot`, with the
    /// slot. Without a config folder, there's nowhere to keep it.
    pub fn write(&self, slot: usize, workspace: &Workspace) -> io::Result<()> {
        let Some(path) = self.file(slot) else {
            return Ok(());
        };
        let slot = (slot > 0).then(|| u8::try_from(slot + 1).unwrap_or(SLOTS));
        let workspace = Workspace {
            slot,
            ..workspace.clone()
        };
        write(&path, &workspace)
    }

    /// Keeps `workspace` as a new one called `name`, in the lowest slot
    /// free, and returns the slot.
    pub fn add(&mut self, name: &str, workspace: &Workspace) -> io::Result<usize> {
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            let full = format!("there's room for {SLOTS} workspaces");
            return Err(io::Error::other(full));
        };
        self.slots[slot] = Some(name.to_owned());
        if let Err(error) = self.write(slot, workspace) {
            self.slots[slot] = None;
            return Err(error);
        }
        Ok(slot)
    }

    /// Calls the workspace in `slot` `name`, and its file too. Default keeps
    /// its name.
    pub fn rename(&mut self, slot: usize, name: &str) -> io::Result<()> {
        if slot == 0 || self.name(slot).is_none() {
            return Ok(());
        }
        if let (Some(from), Some(folder)) = (self.file(slot), &self.folder) {
            // A name in other letters is a rename too, on systems where
            // the file's the same.
            let to = folder.join(format!("{name}.{EXTENSION}"));
            match fs::rename(from, to) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.slots[slot] = Some(name.to_owned());
        Ok(())
    }

    /// Deletes the workspace in `slot`, and its file. Default can't be
    /// deleted. If the workspace was in use, Default is now.
    pub fn delete(&mut self, slot: usize) -> io::Result<()> {
        if slot == 0 || self.name(slot).is_none() {
            return Ok(());
        }
        if let Some(path) = self.file(slot) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.slots[slot] = None;
        if self.current == slot {
            self.current = 0;
        }
        Ok(())
    }
}

/// The workspace files among `paths`, by name, in name order, with the slot
/// each says it's in, from 0, if it says. Default's isn't one of them.
fn files(paths: impl Iterator<Item = PathBuf>) -> Vec<(String, Option<usize>)> {
    let mut found: Vec<_> = paths
        .filter(|path| {
            (path.extension()).is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            if name.eq_ignore_ascii_case(DEFAULT_WORKSPACE) || !path.is_file() {
                return None;
            }
            // A file that can't be read says so when it's switched to.
            let slot = read(&path)
                .ok()
                .flatten()
                .and_then(|workspace| workspace.slot);
            Some((name, slot.map(usize::from).filter(|&slot| slot > 1)))
        })
        .map(|(name, slot)| (name, slot.map(|slot| slot - 1)))
        .collect();
    found.sort_by(|a, b| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then(a.0.cmp(&b.0))
    });
    found
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

/// Where a window goes, from its [`WindowLayout`]: its top left corner on
/// the desktop in pixels, the size of its inside in pixels at 100% scaling,
/// and whether it's maximized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub at: Option<[i32; 2]>,
    pub size: Option<[u32; 2]>,
    pub maximized: bool,
}

impl Place {
    /// Where `window` says its window goes, if it says.
    pub fn of(window: &WindowLayout) -> Option<Self> {
        (window.at.is_some() || window.size.is_some() || window.maximized).then_some(Self {
            at: window.at,
            size: window.size,
            maximized: window.maximized,
        })
    }

    /// Puts `window` there. If the monitor it was on is gone, it stays on
    /// the one it's on, and it's never bigger than its monitor. A hidden
    /// window stays hidden.
    pub fn apply(self, window: &winit::window::Window) {
        let monitors = monitors(window);
        if window.is_maximized() {
            window.set_maximized(false);
        }
        let width = self.size.map_or(800, |[width, _]| width);
        if let Some([x, y]) = self.at.filter(|&at| on_a_monitor(at, width, &monitors)) {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }
        if let Some([width, height]) = self.size {
            let mut size = LogicalSize::new(f64::from(width), f64::from(height));
            if let Some(monitor) = window.current_monitor() {
                let room = monitor.size().to_logical::<f64>(monitor.scale_factor());
                size.width = size.width.min(room.width);
                size.height = size.height.min(room.height);
            }
            let _ = window.request_inner_size(size);
        }
        if self.maximized {
            egui_winit::set_maximized(window, true);
        }
    }
}

/// The monitors `window` can go on, the main one marked, where windows go.
pub fn monitors(window: &winit::window::Window) -> Vec<Monitor> {
    let primary = window.primary_monitor();
    (window.available_monitors())
        .map(|monitor| {
            let (at, size) = (monitor.position(), monitor.size());
            Monitor {
                at: [at.x, at.y],
                size: [size.width, size.height],
                primary: primary.as_ref() == Some(&monitor),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use musicable_ui::{Layout, Split};

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

    fn layout(hidden: &[&str]) -> Workspace {
        Workspace {
            slot: None,
            windows: vec![WindowLayout {
                hidden: hidden.iter().map(|&id| id.to_owned()).collect(),
                layout: Layout::new(Split::Down(vec![
                    Layout::panel("arrangement"),
                    Layout::panel("mixer"),
                ])),
                ..WindowLayout::default()
            }],
            ..Workspace::default()
        }
    }

    #[test]
    fn workspaces_are_written_and_read_back() {
        let dir = TempDir::new("workspaces-written");
        let path = dir.0.join(WORKSPACES_FOLDER).join("Default.toml");
        assert!(read(&path).unwrap().is_none());
        let workspace = layout(&["mixer"]);
        write(&path, &workspace).unwrap();
        assert_eq!(read(&path).unwrap(), Some(workspace));
        fs::write(&path, "format = \"musicable-settings\"\n").unwrap();
        let error = read(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            "this is a \"musicable-settings\" file, not a \"musicable-workspace\" file"
        );
    }

    #[test]
    fn workspaces_keep_their_slots() {
        let dir = TempDir::new("workspaces-slots");
        let mut messages = Vec::new();
        let mut workspaces = Workspaces::list(Some(&dir.0), &mut messages);
        assert_eq!(workspaces.slots()[0].as_deref(), Some("Default"));
        assert!(workspaces.slots()[1..].iter().all(Option::is_none));
        assert_eq!(
            workspaces.read(0).unwrap(),
            None,
            "no file until it's changed"
        );
        // A new one takes the lowest slot free, and its file keeps it.
        let mixing = workspaces.add("Mixing", &layout(&[])).unwrap();
        let editing = workspaces.add("Editing", &layout(&["mixer"])).unwrap();
        assert_eq!((mixing, editing), (1, 2));
        let text = fs::read_to_string(workspaces.file(editing).unwrap()).unwrap();
        assert!(text.contains("\nslot = 3\n"), "{text}");
        workspaces.delete(mixing).unwrap();
        assert!(!dir.0.join("workspaces/Mixing.toml").exists());
        // Editing stays in its slot, even where a file that sorts first
        // has none.
        let tracking = workspaces.add("Tracking", &layout(&[])).unwrap();
        assert_eq!(tracking, 1);
        fs::write(dir.0.join("workspaces/Basic.toml"), "").unwrap();
        fs::write(dir.0.join("workspaces/notes.txt"), "").unwrap();
        let listed = Workspaces::list(Some(&dir.0), &mut messages);
        let names: Vec<_> = listed.slots().iter().map(Option::as_deref).collect();
        assert_eq!(
            names[..5],
            [
                Some("Default"),
                Some("Tracking"),
                Some("Editing"),
                Some("Basic"),
                None
            ]
        );
        let editing = Workspace {
            slot: Some(3),
            ..layout(&["mixer"])
        };
        assert_eq!(listed.read(2).unwrap(), Some(editing));
        assert_eq!(messages, Vec::<String>::new());

        // Renaming renames the file, and in other letters too.
        let mut workspaces = listed;
        workspaces.set_current(2);
        workspaces.rename(2, "EDITING").unwrap();
        assert_eq!(workspaces.current_name(), "EDITING");
        assert_eq!(workspaces.slot_named("EDITING"), Some(2));
        let files: Vec<_> = fs::read_dir(dir.0.join(WORKSPACES_FOLDER))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("E") || name.starts_with("e"))
            .collect();
        assert_eq!(files, ["EDITING.toml"]);
        // Deleting the one in use leaves Default in use.
        workspaces.delete(2).unwrap();
        assert_eq!(workspaces.current(), 0);
        // Default stays.
        workspaces.delete(0).unwrap();
        workspaces.rename(0, "Mine").unwrap();
        assert_eq!(workspaces.current_name(), "Default");
    }

    #[test]
    fn there_is_room_for_nine() {
        let dir = TempDir::new("workspaces-nine");
        let folder = dir.0.join(WORKSPACES_FOLDER);
        fs::create_dir_all(&folder).unwrap();
        for n in 1..=9 {
            fs::write(folder.join(format!("Mine {n}.toml")), "").unwrap();
        }
        let mut messages = Vec::new();
        let mut workspaces = Workspaces::list(Some(&dir.0), &mut messages);
        assert_eq!(workspaces.name(8), Some("Mine 8"));
        assert_eq!(
            messages,
            ["There's room for 9 workspaces, so Musicable leaves out Mine 9."]
        );
        let error = workspaces.add("More", &layout(&[])).unwrap_err();
        assert_eq!(error.to_string(), "there's room for 9 workspaces");
        assert!(!folder.join("More.toml").exists());
    }
}

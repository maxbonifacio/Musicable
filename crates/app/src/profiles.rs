//! The user's profiles: their keys, mouse map, theme, workspaces and
//! behaviour, kept together to switch between, such as one with Live's
//! habits and one with Bitwig's.
//!
//! Default's files are the config folder's own: the keymap file, the mouse
//! map file, the workspaces folder, and the settings file's `[behaviour]`,
//! `[appearance]` and `[workspaces]`. Each of the user's other profiles is
//! a folder in the `profiles` folder there, named after it, such as
//! `profiles/Bitwig habits`, which holds the same files, its settings file
//! holding only those three, so it can be copied to another computer as it
//! is. What belongs to the computer rather than to habits stays in the
//! config folder whichever profile is in use: the audio and MIDI devices,
//! the recording settings, the library's folders, the plugins, and the
//! theme files, which a profile picks from.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::{KEYMAP_FILE, MOUSE_FILE};
use crate::workspaces::WORKSPACES_FOLDER;

/// The folder of the user's profiles but Default, in the config folder.
pub const PROFILES_FOLDER: &str = "profiles";
/// The profile whose files are the config folder's own.
pub const DEFAULT_PROFILE: &str = "Default";

/// The user's profiles, and the one in use.
#[derive(Debug, Clone, PartialEq)]
pub struct Profiles {
    /// The config folder, which holds Default's files and the profiles
    /// folder, if there's one.
    config_dir: Option<PathBuf>,
    /// Their names: Default's, then the others' in name order.
    names: Vec<String>,
    /// The one in use, from 0.
    current: usize,
}

impl Profiles {
    /// The profiles in the config folder `config_dir`, with Default in use.
    /// What keeps any out goes in `messages`.
    pub fn list(config_dir: Option<&Path>, messages: &mut Vec<String>) -> Self {
        let mut profiles = Self {
            config_dir: config_dir.map(Path::to_owned),
            names: vec![DEFAULT_PROFILE.to_owned()],
            current: 0,
        };
        profiles.relist(messages);
        profiles
    }

    /// Lists the profiles again, as after a folder is copied into the
    /// profiles folder. The one in use stays, even if its folder has gone,
    /// as its files are written again as they change.
    pub fn relist(&mut self, messages: &mut Vec<String>) {
        let current = self.current_name().to_owned();
        let mut names = Vec::new();
        if let Some(folder) = self.folder() {
            match fs::read_dir(&folder) {
                Ok(entries) => {
                    names = (entries.flatten())
                        .filter(|entry| entry.path().is_dir())
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .filter(|name| {
                            !name.starts_with('.') && !name.eq_ignore_ascii_case(DEFAULT_PROFILE)
                        })
                        .collect();
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => messages.push(format!(
                    "Couldn't list the profiles in {}: {error}.",
                    folder.display()
                )),
            }
        }
        if current != DEFAULT_PROFILE && !names.contains(&current) {
            names.push(current.clone());
        }
        self.names = vec![DEFAULT_PROFILE.to_owned()];
        self.names.extend(names);
        self.sort(&current);
    }

    /// Puts the profiles in order, Default first, with `current` in use.
    fn sort(&mut self, current: &str) {
        self.names[1..].sort_by(|a, b| (a.to_lowercase().cmp(&b.to_lowercase())).then(a.cmp(b)));
        self.current = self.index_of(current).unwrap_or(0);
    }

    /// Their names: Default's, then the others' in name order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The one in use, from 0.
    pub fn current(&self) -> usize {
        self.current
    }

    /// The name of the one in use.
    pub fn current_name(&self) -> &str {
        &self.names[self.current]
    }

    /// Where the profile called `name` is in the list, if there's one, by
    /// its name in other letters too, as Windows' folders are.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let names = || self.names.iter();
        (names().position(|other| other == name))
            .or_else(|| names().position(|other| other.to_lowercase() == name.to_lowercase()))
    }

    /// The profiles folder, if there's a config folder.
    pub fn folder(&self) -> Option<PathBuf> {
        Some(self.config_dir.as_ref()?.join(PROFILES_FOLDER))
    }

    /// The folder of the files of the profile `index`: the config folder,
    /// for Default.
    pub fn dir(&self, index: usize) -> Option<PathBuf> {
        let name = self.names.get(index)?;
        let config_dir = self.config_dir.as_ref()?;
        Some(match index {
            0 => config_dir.clone(),
            _ => config_dir.join(PROFILES_FOLDER).join(name),
        })
    }

    /// Uses the profile `index` from now on.
    pub fn set_current(&mut self, index: usize) {
        if index < self.names.len() {
            self.current = index;
        }
    }

    /// Makes a profile called `name`, with a folder of its own and no files
    /// in it yet, and returns where it is in the list. The one in use stays
    /// in use.
    pub fn add(&mut self, name: &str) -> io::Result<usize> {
        let Some(folder) = self.folder() else {
            return Err(io::Error::other("there's no config folder to keep it in"));
        };
        if self.index_of(name).is_some() {
            let taken = format!("there's a profile called {name} already");
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, taken));
        }
        fs::create_dir_all(&folder)?;
        fs::create_dir(folder.join(name))?;
        let current = self.current_name().to_owned();
        self.names.push(name.to_owned());
        self.sort(&current);
        Ok(self.index_of(name).unwrap_or(0))
    }

    /// Calls the profile `index` `name`, and its folder too. Default keeps
    /// its name.
    pub fn rename(&mut self, index: usize, name: &str) -> io::Result<()> {
        let (Some(from), Some(folder)) = (self.dir(index), self.folder()) else {
            return Ok(());
        };
        if index == 0 {
            return Ok(());
        }
        // A name in other letters is a rename too, where the folder's the
        // same.
        if self.index_of(name).is_some_and(|other| other != index) {
            let taken = format!("there's a profile called {name} already");
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, taken));
        }
        fs::rename(from, folder.join(name))?;
        let current = match index == self.current {
            true => name.to_owned(),
            false => self.current_name().to_owned(),
        };
        self.names[index] = name.to_owned();
        self.sort(&current);
        Ok(())
    }

    /// Deletes the profile `index`, with its folder and all in it. Default
    /// can't be deleted. If it was in use, Default is now.
    pub fn delete(&mut self, index: usize) -> io::Result<()> {
        if index == 0 || index >= self.names.len() {
            return Ok(());
        }
        if let Some(dir) = self.dir(index) {
            match fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let current = self.current_name().to_owned();
        self.names.remove(index);
        self.current = self.index_of(&current).unwrap_or(0);
        Ok(())
    }
}

/// Copies the keymap file, the mouse map file and the workspaces of the
/// profile whose files are in `from` into `to`, another profile's folder.
/// Its settings are left to the caller, as Default's are among the
/// computer's.
pub fn copy_files(from: &Path, to: &Path) -> io::Result<()> {
    let found = |result: io::Result<u64>| match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.map(drop),
    };
    for file in [KEYMAP_FILE, MOUSE_FILE] {
        found(fs::copy(from.join(file), to.join(file)))?;
    }
    let entries = match fs::read_dir(from.join(WORKSPACES_FOLDER)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let into = to.join(WORKSPACES_FOLDER);
    fs::create_dir_all(&into)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let toml =
            (path.extension()).is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
        if toml && path.is_file() {
            fs::copy(&path, into.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for each test, empty to start with.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("musicable-profiles-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn profiles_are_the_folders_in_the_profiles_folder() {
        let config = temp_dir("listing");
        let mut messages = Vec::new();
        let profiles = Profiles::list(Some(&config), &mut messages);
        assert_eq!(profiles.names(), ["Default"]);
        assert_eq!(profiles.dir(0), Some(config.clone()));
        let folder = config.join(PROFILES_FOLDER);
        for name in ["studio", "Bitwig habits", ".hidden", "default"] {
            fs::create_dir_all(folder.join(name)).unwrap();
        }
        fs::write(folder.join("notes.txt"), "not a profile").unwrap();
        let mut profiles = Profiles::list(Some(&config), &mut messages);
        assert_eq!(profiles.names(), ["Default", "Bitwig habits", "studio"]);
        assert_eq!(profiles.dir(2), Some(folder.join("studio")));
        assert_eq!(profiles.index_of("Studio"), Some(2));
        assert_eq!(profiles.index_of("Live habits"), None);
        // The one in use stays in use, and in the list, as others come and
        // go.
        profiles.set_current(2);
        fs::create_dir(folder.join("Ableton")).unwrap();
        fs::remove_dir(folder.join("studio")).unwrap();
        profiles.relist(&mut messages);
        assert_eq!(
            profiles.names(),
            ["Default", "Ableton", "Bitwig habits", "studio"]
        );
        assert_eq!(profiles.current_name(), "studio");
        assert!(messages.is_empty(), "{messages:?}");
        let _ = fs::remove_dir_all(&config);
        // Without a config folder there's only Default.
        let profiles = Profiles::list(None, &mut messages);
        assert_eq!(profiles.names(), ["Default"]);
        assert_eq!(profiles.dir(0), None);
    }

    #[test]
    fn profiles_are_made_renamed_and_deleted_with_their_folders() {
        let config = temp_dir("changing");
        let folder = config.join(PROFILES_FOLDER);
        let mut profiles = Profiles::list(Some(&config), &mut Vec::new());
        assert_eq!(profiles.add("Live habits").unwrap(), 1);
        assert_eq!(profiles.add("Bitwig habits").unwrap(), 1);
        assert!(folder.join("Live habits").is_dir());
        assert_eq!(
            profiles.names(),
            ["Default", "Bitwig habits", "Live habits"]
        );
        assert_eq!(profiles.current_name(), "Default");
        assert!(profiles.add("live HABITS").is_err());
        profiles.set_current(2);
        fs::write(folder.join("Live habits").join(KEYMAP_FILE), "keys").unwrap();
        assert!(profiles.rename(2, "bitwig habits").is_err());
        profiles.rename(2, "Ableton").unwrap();
        assert_eq!(profiles.names(), ["Default", "Ableton", "Bitwig habits"]);
        assert_eq!(profiles.current_name(), "Ableton");
        assert!(folder.join("Ableton").join(KEYMAP_FILE).is_file());
        assert!(!folder.join("Live habits").exists());
        // Default keeps its name, and can't be deleted.
        profiles.rename(0, "Mine").unwrap();
        profiles.delete(0).unwrap();
        assert_eq!(profiles.names()[0], "Default");
        // Deleting the one in use takes its folder, and Default's in use.
        profiles.delete(1).unwrap();
        assert!(!folder.join("Ableton").exists());
        assert_eq!(profiles.names(), ["Default", "Bitwig habits"]);
        assert_eq!(profiles.current_name(), "Default");
        let _ = fs::remove_dir_all(&config);
        assert!(Profiles::list(None, &mut Vec::new()).add("Studio").is_err());
    }

    #[test]
    fn copies_have_the_keys_mouse_and_workspaces() {
        let config = temp_dir("copying");
        let to = config.join(PROFILES_FOLDER).join("Copy");
        fs::create_dir_all(&to).unwrap();
        // Nothing to copy is nothing copied.
        copy_files(&config, &to).unwrap();
        assert_eq!(fs::read_dir(&to).unwrap().count(), 0);
        fs::write(config.join(KEYMAP_FILE), "keys").unwrap();
        fs::write(config.join(MOUSE_FILE), "mouse").unwrap();
        fs::write(config.join("keymap.backup.toml"), "old keys").unwrap();
        let workspaces = config.join(WORKSPACES_FOLDER);
        fs::create_dir_all(&workspaces).unwrap();
        fs::write(workspaces.join("Mixing.toml"), "layout").unwrap();
        fs::write(workspaces.join("notes.txt"), "not a workspace").unwrap();
        copy_files(&config, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join(KEYMAP_FILE)).unwrap(), "keys");
        assert_eq!(fs::read_to_string(to.join(MOUSE_FILE)).unwrap(), "mouse");
        assert!(!to.join("keymap.backup.toml").exists());
        let copied = to.join(WORKSPACES_FOLDER);
        assert_eq!(
            fs::read_to_string(copied.join("Mixing.toml")).unwrap(),
            "layout"
        );
        assert!(!copied.join("notes.txt").exists());
        let _ = fs::remove_dir_all(&config);
    }
}

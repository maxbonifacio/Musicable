//! The user's mouse map file: their changes to what the mouse does, made on
//! the Settings window's Mouse page or by editing the file. The manual's
//! Mouse page (`manual/src/customising/mouse.md`) describes the file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_ui::{MouseFileError, MouseMap, MouseMapFile, MouseProblem};

use crate::capital;

/// The copy of the mouse map file kept before writing over what would be
/// lost otherwise, in the same folder.
pub const BACKUP_FILE: &str = "mouse.backup.toml";

/// The user's mouse map file, what was wrong with it, and what happened to
/// it last.
///
/// Every change made on the Mouse page is saved to the file at once. The
/// file holds only the changes to the mouse map Musicable starts with,
/// [`MouseMap::live`], so new gestures in a later version reach the user.
/// Before writing over the file, Musicable copies it to [`BACKUP_FILE`] when
/// the write would lose something, as it does for the keymap file: a file it
/// can't read or one with mistakes, or the user's changes before Reset All
/// replaces them.
#[derive(Debug)]
pub struct UserMouse {
    /// The file, or `None` when there's no folder for it, and changes last
    /// until the app closes.
    path: Option<PathBuf>,
    /// What was wrong with entries of the file when it was last read.
    problems: Vec<MouseProblem>,
    /// Why the file couldn't be read or saved, the last time it couldn't.
    error: Option<String>,
    /// What the last change did to the file, when there's more to say than
    /// that it's saved.
    notice: Option<String>,
}

/// What the file holds, as far as writing over it is concerned.
enum Contents {
    /// No changes, or no file.
    Nothing,
    Changes,
    /// Entries Musicable skipped, which a new file wouldn't have.
    Mistakes,
    /// Something Musicable can't read as a mouse map at all.
    Unreadable,
}

impl UserMouse {
    /// Reads the mouse map file at `path`, and returns it with the mouse map
    /// it gives: the one Musicable starts with, and the file's changes. A
    /// missing file changes nothing, and one that can't be read leaves the
    /// mouse as it is at first.
    pub fn read(path: Option<PathBuf>) -> (Self, MouseMap) {
        let mut file = Self {
            path,
            problems: Vec::new(),
            error: None,
            notice: None,
        };
        let map = file.reload();
        (file, map)
    }

    /// Reads the file again, as after the user edited it, and returns the
    /// mouse map it gives.
    pub fn reload(&mut self) -> MouseMap {
        self.problems.clear();
        self.error = None;
        self.notice = None;
        let Some(path) = &self.path else {
            return MouseMap::live();
        };
        match MouseMapFile::read(path) {
            Ok((changes, problems)) => {
                self.problems = problems;
                changes.mouse_map()
            }
            Err(MouseFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                MouseMap::live()
            }
            Err(error) => {
                self.error = Some(format!(
                    "{}. Musicable's mouse does what it does at first.",
                    capital(error.to_string().trim_end())
                ));
                MouseMap::live()
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn problems(&self) -> &[MouseProblem] {
        &self.problems
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Saves `map` as what the user's mouse does. Saving the mouse map in
    /// use creates the file, to edit, when there's none.
    pub fn save(&mut self, map: &MouseMap) {
        let notice = match self.contents() {
            Contents::Nothing | Contents::Changes => None,
            Contents::Mistakes => Some(format!(
                "Your mouse map file had mistakes, so Musicable kept a copy of it as \
                 {BACKUP_FILE} before saving your change."
            )),
            Contents::Unreadable => Some(format!(
                "Musicable couldn't read your mouse map file, so it kept it as {BACKUP_FILE} and \
                 started a new one."
            )),
        };
        self.write(map, notice.is_some(), notice);
    }

    /// Makes every gesture do what it does at first again, and returns the
    /// mouse map.
    pub fn reset(&mut self) -> MouseMap {
        let backup = !matches!(self.contents(), Contents::Nothing);
        let mut notice = "Every gesture does what it does at first again.".to_owned();
        if backup {
            notice += &format!(" Your changes from before are in {BACKUP_FILE}.");
        }
        let live = MouseMap::live();
        self.write(&live, backup, Some(notice));
        live
    }

    /// Writes `map` to the file, copying the file to the backup first if
    /// `backup` is set. Says `notice` once it's written.
    fn write(&mut self, map: &MouseMap, backup: bool, notice: Option<String>) {
        let Some(path) = self.path.clone() else {
            self.notice = notice;
            return;
        };
        if backup && let Err(error) = fs::copy(&path, path.with_file_name(BACKUP_FILE)) {
            self.error = Some(format!(
                "Couldn't keep a copy of {} as {BACKUP_FILE}, so Musicable didn't save your mouse \
                 map there: {error}. It lasts until Musicable closes.",
                path.display()
            ));
            return;
        }
        match MouseMapFile::diff(map).write(&path) {
            Ok(()) => {
                self.problems.clear();
                self.error = None;
                self.notice = notice;
            }
            Err(error) => {
                self.error = Some(format!(
                    "Couldn't save your mouse map to {}: {error}. It lasts until Musicable closes.",
                    path.display()
                ));
            }
        }
    }

    /// What the file holds now. It's read again, in case the user edited it.
    fn contents(&self) -> Contents {
        let Some(path) = &self.path else {
            return Contents::Nothing;
        };
        match MouseMapFile::read(path) {
            Ok((_, problems)) if !problems.is_empty() => Contents::Mistakes,
            Ok((changes, _)) if !changes.is_empty() => Contents::Changes,
            Ok(_) => Contents::Nothing,
            Err(MouseFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                Contents::Nothing
            }
            Err(_) => Contents::Unreadable,
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_commands::Modifiers;
    use musicable_ui::{Gesture, MouseAction, Target};

    use super::*;
    use crate::settings::MOUSE_FILE;

    /// A folder of its own for each test, removed when it ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("musicable-mouse-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn mouse(&self) -> PathBuf {
            self.0.join(MOUSE_FILE)
        }

        fn backup(&self) -> PathBuf {
            self.0.join(BACKUP_FILE)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Live's mouse, with Ctrl-dragging a clip moving it off the grid.
    fn changed() -> MouseMap {
        let mut map = MouseMap::live();
        let (target, gesture) = (Target::ClipTitle, Gesture::Drag);
        map.bind(target, gesture, Modifiers::CTRL, MouseAction::MoveFree);
        map
    }

    #[test]
    fn mouse_map_files_change_what_the_mouse_does() {
        let folder = Folder::new("read");
        let path = folder.mouse();

        // No file: Live's mouse, and no problems.
        let (file, map) = UserMouse::read(Some(path.clone()));
        assert_eq!(map, MouseMap::live());
        assert_eq!((file.problems(), file.error()), (&[][..], None));

        // A change and a bad entry: the change, and a problem.
        let text = "[clip_title]\n\"Ctrl+Drag\" = \"move_free\"\n\"Drag\" = \"fade\"\n";
        fs::write(&path, text).unwrap();
        let (file, map) = UserMouse::read(Some(path.clone()));
        assert_eq!(map, changed());
        assert_eq!(file.problems().len(), 1, "{:?}", file.problems());

        // A file that isn't a mouse map: Live's mouse, and why.
        fs::write(&path, "format = \"something-else\"\n").unwrap();
        let (file, map) = UserMouse::read(Some(path));
        assert_eq!(map, MouseMap::live());
        let error = file.error().unwrap();
        assert!(
            error.starts_with("This is a \"something-else\" file"),
            "{error}"
        );
    }

    #[test]
    fn changes_are_saved_as_they_are_made() {
        let folder = Folder::new("save");
        let (mut file, _) = UserMouse::read(Some(folder.0.join("new").join(MOUSE_FILE)));
        file.save(&changed());
        let path = file.path().unwrap().to_owned();
        assert_eq!(UserMouse::read(Some(path.clone())).1, changed());
        // Just the change, and no copy of the file: nothing was lost.
        assert_eq!(MouseMapFile::read(&path).unwrap().0.len(), 1);
        assert!(!path.with_file_name(BACKUP_FILE).exists());
        assert_eq!((file.error(), file.notice()), (None, None));

        // Without a folder, the changes last until the app closes.
        let (mut file, _) = UserMouse::read(None);
        file.save(&changed());
        assert_eq!((file.error(), file.notice()), (None, None));
    }

    #[test]
    fn a_file_with_mistakes_is_kept_before_it_is_written_over() {
        let folder = Folder::new("mistakes");
        let text = "[clip_title]\n\"Drag\" = \"fade\"\n";
        fs::write(folder.mouse(), text).unwrap();
        let (mut file, map) = UserMouse::read(Some(folder.mouse()));
        assert_eq!(file.problems().len(), 1);

        file.save(&map);
        assert_eq!(fs::read_to_string(folder.backup()).unwrap(), text);
        assert_eq!(file.problems(), [], "the new file has none");
        assert!(file.notice().unwrap().contains("had mistakes"));
        // The next change has nothing to keep.
        fs::remove_file(folder.backup()).unwrap();
        file.save(&changed());
        assert!(!folder.backup().exists());

        // One Musicable can't read is kept too, and replaced.
        fs::write(folder.mouse(), "not a mouse map").unwrap();
        let (mut file, map) = UserMouse::read(Some(folder.mouse()));
        assert!(file.error().is_some());
        file.save(&map);
        let kept = fs::read_to_string(folder.backup()).unwrap();
        assert_eq!(kept, "not a mouse map");
        assert_eq!(file.error(), None);
        assert!(file.notice().unwrap().contains("couldn't read"));
    }

    #[test]
    fn reset_all_keeps_the_changes_from_before() {
        let folder = Folder::new("reset");
        let (mut file, _) = UserMouse::read(Some(folder.mouse()));
        file.save(&changed());
        assert_eq!(file.reset(), MouseMap::live());
        assert_eq!(UserMouse::read(Some(folder.mouse())).1, MouseMap::live());
        assert!(file.notice().unwrap().contains(BACKUP_FILE));
        assert_eq!(UserMouse::read(Some(folder.backup())).1, changed());

        // With nothing to keep, there's no copy to mention.
        let (mut file, _) = UserMouse::read(None);
        file.reset();
        assert_eq!(
            file.notice(),
            Some("Every gesture does what it does at first again.")
        );
    }
}

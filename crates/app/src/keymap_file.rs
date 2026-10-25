//! The user's keymap file: their changes to the default keys, made on the
//! Settings window's Keys page or by editing the file. The manual's Keys
//! page (`manual/src/customising/keys.md`) describes the file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_commands::{Keymap, KeymapFile, KeymapFileError, Problem};

use crate::capital;

/// The copy of the keymap file kept before writing over what would be lost
/// otherwise, in the same folder.
pub const BACKUP_FILE: &str = "keymap.backup.toml";

/// The user's keymap file, what was wrong with it, and what happened to it
/// last.
///
/// Every change of keys in the app is saved to the file at once. The file
/// holds only the changes to the default keymap, so new default keys in a
/// later version reach the user. Before writing over the file, Musicable
/// copies it to [`BACKUP_FILE`] when the write would lose something: a file
/// it can't read or one with mistakes, or the user's changes before Reset All
/// or an import replaces them.
#[derive(Debug)]
pub struct UserKeymap {
    /// The file, or `None` when there's no folder for it, and changes last
    /// until the app closes.
    path: Option<PathBuf>,
    /// The keymap the file changes.
    defaults: Keymap,
    /// What was wrong with entries of the file when it was last read.
    problems: Vec<Problem>,
    /// Why the file couldn't be read or saved, the last time it couldn't.
    error: Option<String>,
    /// What the last change of keys did to the file, when there's more to
    /// say than that it's saved.
    notice: Option<String>,
}

/// What the file holds, as far as writing over it is concerned.
enum Contents {
    /// No changes, or no file.
    Nothing,
    Changes,
    /// Entries Musicable skipped, which a new file wouldn't have.
    Mistakes,
    /// Something Musicable can't read as a keymap at all.
    Unreadable,
}

impl UserKeymap {
    /// Reads the keymap file at `path`, and returns it with the keys it
    /// gives: the default keymap with the file's changes. A missing file
    /// changes nothing, and one that can't be read leaves the default keys.
    pub fn read(path: Option<PathBuf>) -> (Self, Keymap) {
        let mut file = Self {
            path,
            defaults: Keymap::default_keymap(),
            problems: Vec::new(),
            error: None,
            notice: None,
        };
        let keymap = file.reload();
        (file, keymap)
    }

    /// Reads the file again, as after the user edited it, and returns the
    /// keys it gives.
    pub fn reload(&mut self) -> Keymap {
        let mut keymap = self.defaults.clone();
        self.problems.clear();
        self.error = None;
        self.notice = None;
        let Some(path) = &self.path else {
            return keymap;
        };
        match KeymapFile::read(path) {
            Ok((changes, problems)) => {
                changes.apply_to(&mut keymap);
                self.problems = problems;
            }
            Err(KeymapFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                self.error = Some(format!(
                    "{}. Musicable uses the default keys.",
                    capital(error.to_string().trim_end())
                ));
            }
        }
        keymap
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The keymap the file changes.
    pub fn defaults(&self) -> &Keymap {
        &self.defaults
    }

    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Saves `keymap` as the user's keys. Saving the keys in use creates the
    /// file, to edit, when there's none.
    pub fn save(&mut self, keymap: &Keymap) {
        let notice = match self.contents() {
            Contents::Nothing | Contents::Changes => None,
            Contents::Mistakes => Some(format!(
                "Your keymap file had mistakes, so Musicable kept a copy of it as {BACKUP_FILE} \
                 before saving your change."
            )),
            Contents::Unreadable => Some(format!(
                "Musicable couldn't read your keymap file, so it kept it as {BACKUP_FILE} and \
                 started a new one."
            )),
        };
        self.write(keymap, notice.is_some(), notice);
    }

    /// Gives every command its default keys again, and returns them.
    pub fn reset(&mut self) -> Keymap {
        let backup = !matches!(self.contents(), Contents::Nothing);
        let mut notice = "Every command has its default keys again.".to_owned();
        if backup {
            notice += &format!(
                " Your keys from before are in {BACKUP_FILE}: import it to get them back."
            );
        }
        let defaults = self.defaults.clone();
        self.write(&defaults, backup, Some(notice));
        defaults
    }

    /// Uses the keys in the keymap file at `from` in place of the user's, and
    /// returns them. Returns `None` when the file can't be used at all, and
    /// the notice says why.
    pub fn import(&mut self, from: &Path) -> Option<Keymap> {
        let (changes, problems) = match KeymapFile::read(from) {
            Ok(read) => read,
            Err(error) => {
                self.notice = Some(format!("Couldn't import {}: {error}.", from.display()));
                return None;
            }
        };
        let mut keymap = self.defaults.clone();
        changes.apply_to(&mut keymap);
        let backup = !matches!(self.contents(), Contents::Nothing);
        let mut notice = format!("Imported {}.", from.display());
        if backup {
            notice += &format!(
                " Your keys from before are in {BACKUP_FILE}: import it to get them back."
            );
        }
        if !problems.is_empty() {
            notice += "\nIt had mistakes, which Musicable left out:";
            for problem in &problems {
                notice += &format!("\n\u{2022} {problem}.");
            }
        }
        self.write(&keymap, backup, Some(notice));
        Some(keymap)
    }

    /// Writes `keymap` to the file at `to`, as the changes to the default
    /// keymap.
    pub fn export(&mut self, keymap: &Keymap, to: &Path) {
        let written = KeymapFile::diff(&self.defaults, keymap).write(to);
        self.notice = Some(match written {
            Ok(()) => format!("Exported your keys to {}.", to.display()),
            Err(error) => format!("Couldn't export your keys to {}: {error}.", to.display()),
        });
    }

    /// Writes `keymap` to the file, copying the file to the backup first if
    /// `backup` is set. Says `notice` once it's written.
    fn write(&mut self, keymap: &Keymap, backup: bool, notice: Option<String>) {
        let Some(path) = self.path.clone() else {
            self.notice = notice;
            return;
        };
        if backup && let Err(error) = fs::copy(&path, path.with_file_name(BACKUP_FILE)) {
            self.error = Some(format!(
                "Couldn't keep a copy of {} as {BACKUP_FILE}, so Musicable didn't save your keys \
                 there: {error}. They last until Musicable closes.",
                path.display()
            ));
            return;
        }
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| KeymapFile::diff(&self.defaults, keymap).write(&path));
        match written {
            Ok(()) => {
                self.problems.clear();
                self.error = None;
                self.notice = notice;
            }
            Err(error) => {
                self.error = Some(format!(
                    "Couldn't save your keys to {}: {error}. They last until Musicable closes.",
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
        match KeymapFile::read(path) {
            Ok((_, problems)) if !problems.is_empty() => Contents::Mistakes,
            Ok((changes, _)) if !changes.is_empty() => Contents::Changes,
            Ok(_) => Contents::Nothing,
            Err(KeymapFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                Contents::Nothing
            }
            Err(_) => Contents::Unreadable,
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_commands::{KeyChord, transport};

    use super::*;
    use crate::settings::KEYMAP_FILE;

    /// A folder of its own for each test, removed when it ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("musicable-keymap-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn keymap(&self) -> PathBuf {
            self.0.join(KEYMAP_FILE)
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

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    /// The default keymap, with the metronome on M as well as O.
    fn with_m() -> Keymap {
        let mut keymap = Keymap::default_keymap();
        keymap.bind(chord("M"), transport::TOGGLE_METRONOME);
        keymap
    }

    #[test]
    fn keymap_files_change_the_default_keys() {
        let folder = Folder::new("read");
        let path = folder.keymap();

        // No file: the default keys, and no problems.
        let (file, keymap) = UserKeymap::read(Some(path.clone()));
        assert_eq!(keymap, Keymap::default_keymap());
        assert_eq!(file.problems(), []);
        assert_eq!(file.error(), None);

        // A file with a change and a bad entry: the change, and a problem.
        let text = "[bindings]\n\"transport.play\" = [\"P\"]\n\"transport.stop\" = [\"Nope\"]\n";
        fs::write(&path, text).unwrap();
        let (file, keymap) = UserKeymap::read(Some(path.clone()));
        let chords: Vec<_> = keymap.chords_for(&transport::PLAY).collect();
        assert_eq!(chords, [chord("P")]);
        assert_eq!(file.problems().len(), 1, "{:?}", file.problems());

        // A file that isn't a keymap: the default keys, and why.
        fs::write(&path, "format = \"something-else\"\n").unwrap();
        let (file, keymap) = UserKeymap::read(Some(path));
        assert_eq!(keymap, Keymap::default_keymap());
        assert_eq!(file.problems(), []);
        let error = file.error().unwrap();
        assert!(
            error.starts_with("This is a \"something-else\" file"),
            "{error}"
        );
    }

    #[test]
    fn changes_are_saved_as_they_are_made() {
        let folder = Folder::new("save");
        let (mut file, _) = UserKeymap::read(Some(folder.keymap()));
        file.save(&with_m());
        assert_eq!(UserKeymap::read(Some(folder.keymap())).1, with_m());
        // Just the change, and no copy of the file: nothing was lost.
        let (changes, _) = KeymapFile::read(&folder.keymap()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(!folder.backup().exists());
        assert_eq!(file.notice(), None);

        // Without a folder, the keys last until the app closes.
        let (mut file, _) = UserKeymap::read(None);
        file.save(&with_m());
        assert_eq!((file.error(), file.notice()), (None, None));
    }

    #[test]
    fn a_file_with_mistakes_is_kept_before_it_is_written_over() {
        let folder = Folder::new("mistakes");
        let text = "[bindings]\n\"transport.stop\" = [\"Nope\"]\n";
        fs::write(folder.keymap(), text).unwrap();
        let (mut file, keymap) = UserKeymap::read(Some(folder.keymap()));
        assert_eq!(file.problems().len(), 1);

        file.save(&keymap);
        assert_eq!(fs::read_to_string(folder.backup()).unwrap(), text);
        assert_eq!(file.problems(), [], "the new file has none");
        assert!(file.notice().unwrap().contains("had mistakes"));
        // The next change has nothing to keep.
        fs::remove_file(folder.backup()).unwrap();
        file.save(&with_m());
        assert!(!folder.backup().exists());

        // One Musicable can't read is kept too, and replaced.
        fs::write(folder.keymap(), "not a keymap").unwrap();
        let (mut file, keymap) = UserKeymap::read(Some(folder.keymap()));
        assert!(file.error().is_some());
        file.save(&keymap);
        assert_eq!(fs::read_to_string(folder.backup()).unwrap(), "not a keymap");
        assert_eq!(file.error(), None);
        assert!(file.notice().unwrap().contains("couldn't read"));
    }

    #[test]
    fn reset_all_keeps_the_keys_from_before() {
        let folder = Folder::new("reset");
        let (mut file, _) = UserKeymap::read(Some(folder.keymap()));
        file.save(&with_m());
        assert_eq!(file.reset(), Keymap::default_keymap());
        assert_eq!(
            UserKeymap::read(Some(folder.keymap())).1,
            Keymap::default_keymap()
        );
        assert!(file.notice().unwrap().contains(BACKUP_FILE));

        // Importing the copy brings them back.
        let backup = folder.backup();
        assert_eq!(file.import(&backup), Some(with_m()));
        assert_eq!(UserKeymap::read(Some(folder.keymap())).1, with_m());
        assert!(file.notice().unwrap().starts_with("Imported"));
        // The default keys weren't worth a copy, so the copy stays.
        assert_eq!(UserKeymap::read(Some(backup)).1, with_m());

        // With nothing to keep, there's no copy to mention.
        let (mut file, _) = UserKeymap::read(None);
        file.reset();
        assert_eq!(
            file.notice(),
            Some("Every command has its default keys again.")
        );
    }

    #[test]
    fn imports_and_exports() {
        let folder = Folder::new("import");
        let (mut file, _) = UserKeymap::read(Some(folder.keymap()));
        let exported = folder.0.join("exported.toml");
        file.export(&with_m(), &exported);
        assert_eq!(
            file.notice(),
            Some(format!("Exported your keys to {}.", exported.display()).as_str())
        );
        assert!(
            !folder.keymap().exists(),
            "exporting doesn't change the user's file"
        );

        assert_eq!(file.import(&exported), Some(with_m()));
        assert_eq!(UserKeymap::read(Some(folder.keymap())).1, with_m());
        assert!(!folder.backup().exists(), "there was nothing to keep");

        // What an imported file gets wrong is left out, and listed.
        let mistakes = folder.0.join("mistakes.toml");
        let text = "[bindings]\n\"transport.play\" = [\"P\"]\n\"transport.stop\" = 7\n";
        fs::write(&mistakes, text).unwrap();
        let keymap = file.import(&mistakes).unwrap();
        assert_eq!(keymap.lookup(chord("P")), Some(&transport::PLAY));
        let notice = file.notice().unwrap();
        assert!(
            notice.contains("left out:\n\u{2022} transport.stop keeps"),
            "{notice}"
        );
        assert!(notice.contains(BACKUP_FILE), "the M was kept: {notice}");
        assert_eq!(UserKeymap::read(Some(folder.backup())).1, with_m());

        // A file that isn't a keymap changes nothing.
        let not = folder.0.join("not.toml");
        fs::write(&not, "format = \"something-else\"\n").unwrap();
        assert_eq!(file.import(&not), None);
        assert!(file.notice().unwrap().starts_with("Couldn't import"));
        assert_eq!(UserKeymap::read(Some(folder.keymap())).1, keymap);
    }

    #[test]
    fn saving_the_default_keys_creates_a_file_without_changes() {
        let folder = Folder::new("create");
        let (mut file, _) = UserKeymap::read(Some(folder.0.join("new").join(KEYMAP_FILE)));
        file.save(&Keymap::default_keymap());
        let text = fs::read_to_string(file.path().unwrap()).unwrap();
        assert!(text.starts_with("# Musicable keymap"), "{text}");
        assert!(KeymapFile::parse(&text).unwrap().0.is_empty());
        assert_eq!(file.error(), None);
    }
}

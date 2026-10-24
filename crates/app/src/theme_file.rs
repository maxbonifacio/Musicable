//! The theme in use: a built-in one, or a theme file in the themes folder,
//! which is read again whenever it changes. `docs/app.md` describes the
//! files.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{FontData, FontDefinitions, FontFamily};
use musicable_ui::{Base, Fonts, Theme, ThemeFile, ThemeFileError};

use crate::capital;

/// How often the theme file is looked at, to follow changes to it.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The theme to draw with, as `[appearance]` in settings.toml names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    BuiltIn(Base),
    /// A theme file, by its name in the themes folder.
    File(String),
}

impl Choice {
    /// The theme `name` names: `dark` or `light`, in any case, for the
    /// built-in ones, and otherwise a file in the themes folder, which is
    /// taken to end in `.toml` if it doesn't say. No name is the dark theme.
    pub fn from_name(name: &str) -> Self {
        let name = name.trim();
        if name.is_empty() {
            return Self::BuiltIn(Base::Dark);
        }
        match Base::from_name(name) {
            Some(base) => Self::BuiltIn(base),
            None if Path::new(name).extension().is_none() => Self::File(format!("{name}.toml")),
            None => Self::File(name.to_owned()),
        }
    }

    /// Its name, as settings.toml has it.
    pub fn name(&self) -> &str {
        match self {
            Self::BuiltIn(base) => base.name(),
            Self::File(name) => name,
        }
    }

    /// What the appearance window calls it: a file's name without its
    /// extension.
    pub fn title(&self) -> &str {
        match self {
            Self::BuiltIn(base) => base.title(),
            Self::File(name) => Path::new(name)
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or(name),
        }
    }
}

/// The theme Musicable draws with, the file it comes from, and what was
/// wrong with that file.
///
/// A theme file holds only its changes to a built-in theme, its base. Bad
/// entries are skipped, and a file that can't be used at all leaves its base
/// in its place, or the dark theme when even that can't be told.
#[derive(Debug)]
pub struct UserTheme {
    /// The themes folder, or `None` when there's no config folder.
    folder: Option<PathBuf>,
    choice: Choice,
    /// The theme to draw with: the one chosen, or what stands in for it.
    theme: Theme,
    /// The built-in theme the chosen one changes.
    base: Base,
    /// What was wrong with entries of the file, and with its fonts, when it
    /// was last read.
    problems: Vec<String>,
    /// What was wrong with the fonts, when they were last loaded.
    font_problems: Vec<String>,
    /// Why the file couldn't be used, when it couldn't.
    error: Option<String>,
    /// What the last export or the last look at the folder said.
    notice: Option<String>,
    /// The file as it was when it was last read, to tell when it changes.
    /// `None` when it wasn't there.
    stamp: Option<Stamp>,
    /// When the file was last looked at. `None` looks at it at the next
    /// [`poll`](Self::poll).
    looked: Option<Instant>,
    /// The fonts the theme names, for text and for monospaced text, as last
    /// loaded.
    font_names: (String, String),
    /// The fonts for egui, when they've changed since it was last given
    /// them.
    fonts: Option<FontDefinitions>,
}

/// What tells that a file has changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl UserTheme {
    /// The theme `choice` names, with theme files and their fonts in
    /// `folder`.
    pub fn new(folder: Option<PathBuf>, choice: Choice) -> Self {
        let mut theme = Self {
            folder,
            choice,
            theme: Theme::dark(),
            base: Base::Dark,
            problems: Vec::new(),
            font_problems: Vec::new(),
            error: None,
            notice: None,
            stamp: None,
            looked: None,
            font_names: Default::default(),
            fonts: None,
        };
        theme.read(false);
        theme
    }

    /// The theme to draw with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn choice(&self) -> &Choice {
        &self.choice
    }

    /// The built-in theme the chosen one changes, which a file that can't
    /// be used leaves in its place.
    pub fn base(&self) -> Base {
        self.base
    }

    /// The themes folder, if there's a config folder for it.
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// The chosen theme's file, if it's one.
    pub fn path(&self) -> Option<PathBuf> {
        match (&self.choice, &self.folder) {
            (Choice::File(name), Some(folder)) => Some(folder.join(name)),
            _ => None,
        }
    }

    /// What was wrong with entries of the file and with its fonts, which
    /// were left out, as the keyboard shortcuts window lists a keymap's. The
    /// rest of the file applies.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Why the file couldn't be used, when it couldn't.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// What the status bar says when the chosen theme's file couldn't be
    /// used as it is.
    pub fn message(&self) -> Option<String> {
        let Choice::File(name) = &self.choice else {
            return None;
        };
        if self.error.is_some() {
            Some(format!(
                "Musicable couldn't use your theme file {name}, so it uses the {} theme: see \
                 Options > Appearance.",
                self.base.name()
            ))
        } else if !self.problems.is_empty() {
            Some(format!(
                "Your theme file {name} has problems: see Options > Appearance."
            ))
        } else {
            None
        }
    }

    /// The fonts egui should write with, when they've changed since this
    /// was last called.
    pub fn take_fonts(&mut self) -> Option<FontDefinitions> {
        self.fonts.take()
    }

    /// Uses the theme `choice` names from now on.
    pub fn choose(&mut self, choice: Choice) {
        self.choice = choice;
        self.notice = None;
        self.read(false);
    }

    /// Reads the file again, and its fonts, as after editing them.
    pub fn reload(&mut self) {
        self.notice = None;
        self.read(true);
    }

    /// Reads the file again if it has changed since it was last read,
    /// looking at it at most every half a second. Returns whether it was
    /// read.
    pub fn poll(&mut self) -> bool {
        let Some(path) = self.path() else {
            return false;
        };
        if self.looked.is_some_and(|at| at.elapsed() < LOOK_EVERY) {
            return false;
        }
        self.looked = Some(Instant::now());
        if stamp(&path) == self.stamp {
            return false;
        }
        self.read(false);
        true
    }

    /// The theme files in the themes folder, by name, in order.
    pub fn files(&self) -> Vec<String> {
        let Some(entries) = self
            .folder
            .as_deref()
            .and_then(|dir| fs::read_dir(dir).ok())
        else {
            return Vec::new();
        };
        let mut files: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            })
            .collect();
        files.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
        files
    }

    /// Writes the theme in use to the file at `to`, as changes to its base.
    /// A file written into the themes folder becomes the theme in use, to
    /// edit; returns whether it did.
    pub fn export(&mut self, to: &Path) -> bool {
        let file = ThemeFile::diff(self.base, &self.theme);
        if let Err(error) = file.write(to) {
            self.notice = Some(format!(
                "Couldn't export the theme to {}: {error}.",
                to.display()
            ));
            return false;
        }
        let into_folder = (self.folder.as_deref())
            .zip(to.parent())
            .is_some_and(|(folder, parent)| same_folder(folder, parent));
        let name = to.file_name().and_then(OsStr::to_str);
        match name.filter(|_| into_folder) {
            Some(name) => {
                self.choose(Choice::File(name.to_owned()));
                self.notice = Some(format!(
                    "Exported the theme to {name}, which Musicable uses now: what you change \
                     in it shows as soon as you save it."
                ));
                true
            }
            None => {
                self.notice = Some(format!("Exported the theme to {}.", to.display()));
                false
            }
        }
    }

    /// Opens the themes folder in the system's file manager, creating it if
    /// need be.
    pub fn open_folder(&mut self) {
        let Some(folder) = &self.folder else {
            return;
        };
        if let Err(error) = fs::create_dir_all(folder).and_then(|()| show_folder(folder)) {
            self.notice = Some(format!("Couldn't open {}: {error}.", folder.display()));
        }
    }

    /// Reads the chosen theme, and loads its fonts if they're others than
    /// before, or if `fonts` is set.
    fn read(&mut self, fonts: bool) {
        self.problems.clear();
        self.error = None;
        let theme = match (&self.choice, self.path()) {
            (Choice::BuiltIn(base), _) => {
                self.stamp = None;
                self.base = *base;
                base.theme()
            }
            (Choice::File(_), None) => {
                self.base = Base::Dark;
                self.error = Some(
                    "Musicable found no folder for theme files, so it uses the dark theme."
                        .to_owned(),
                );
                Theme::dark()
            }
            (Choice::File(name), Some(path)) => {
                self.stamp = stamp(&path);
                match ThemeFile::read(&path) {
                    Ok((file, problems)) => {
                        self.base = file.base;
                        self.problems = problems.iter().map(ToString::to_string).collect();
                        file.theme()
                    }
                    Err(error) => {
                        if let ThemeFileError::Io(_) = error {
                            // Looked at again at the next poll, in case it
                            // was being written.
                            self.stamp = None;
                        }
                        self.base = error.base();
                        let why = match &error {
                            ThemeFileError::Io(error)
                                if error.kind() == io::ErrorKind::NotFound =>
                            {
                                format!("There's no {name} in the themes folder")
                            }
                            error => capital(error.to_string().trim_end()),
                        };
                        self.error = Some(format!(
                            "{why}. Musicable uses the {} theme in its place.",
                            self.base.name()
                        ));
                        self.base.theme()
                    }
                }
            }
        };
        let names = (theme.fonts.text.clone(), theme.fonts.monospace.clone());
        if fonts || names != self.font_names {
            let (definitions, problems) = font_definitions(&theme.fonts, self.folder.as_deref());
            self.fonts = Some(definitions);
            self.font_problems = problems;
            self.font_names = names;
        }
        self.problems.extend(self.font_problems.iter().cloned());
        self.theme = theme;
    }
}

/// Whether `a` and `b` are the same folder, however they're written.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// What tells whether the file at `path` has changed, or `None` if it isn't
/// there.
fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(Stamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// egui's fonts with those `fonts` names first, for text and for
/// monospaced text, and what was wrong with those that can't be used, which
/// leave egui's. Names are of font files in `folder`, or of egui's fonts.
fn font_definitions(fonts: &Fonts, folder: Option<&Path>) -> (FontDefinitions, Vec<String>) {
    let mut definitions = FontDefinitions::default();
    let mut problems = Vec::new();
    let families = [
        ("fonts.text", &fonts.text, FontFamily::Proportional),
        ("fonts.monospace", &fonts.monospace, FontFamily::Monospace),
    ];
    for (key, name, family) in families {
        if name.is_empty() {
            continue;
        }
        if !definitions.font_data.contains_key(name) {
            match read_font(folder, name) {
                Ok(font) => {
                    definitions.font_data.insert(name.clone(), Arc::new(font));
                }
                Err(why) => {
                    problems.push(format!("{key} keeps egui's font: {why}"));
                    continue;
                }
            }
        }
        let chain = definitions.families.entry(family).or_default();
        chain.retain(|font| font != name);
        chain.insert(0, name.clone());
    }
    (definitions, problems)
}

/// The font file `name` in `folder`, or why it can't be used.
fn read_font(folder: Option<&Path>, name: &str) -> Result<FontData, String> {
    let Some(folder) = folder else {
        return Err(format!("egui has no font called {name}"));
    };
    let path = folder.join(name);
    let bytes = fs::read(&path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => {
            format!("there's no {name} in the themes folder, and egui has no font by that name")
        }
        _ => format!("Musicable can't read {}: {error}", path.display()),
    })?;
    // egui can't draw with a file it can't read as a font, and gives up on
    // the whole window, so it never sees one.
    if skrifa::FontRef::from_index(&bytes, 0).is_err() {
        return Err(format!("{name} isn't a font file Musicable can read"));
    }
    Ok(FontData::from_owned(bytes))
}

/// Opens `folder` in the system's file manager.
fn show_folder(folder: &Path) -> io::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(program).arg(folder).spawn()?;
    // Waited for, so it doesn't linger once it's done.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use eframe::egui::Color32;

    use super::*;

    /// A config folder of its own for each test, removed when it ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("musicable-theme-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("themes")).unwrap();
            Self(dir)
        }

        fn themes(&self) -> PathBuf {
            self.0.join("themes")
        }

        /// Writes the theme file `name` in the themes folder.
        fn write(&self, name: &str, text: &str) {
            fs::write(self.themes().join(name), text).unwrap();
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn studio(folder: &Folder) -> UserTheme {
        UserTheme::new(Some(folder.themes()), Choice::File("Studio.toml".into()))
    }

    /// Looks at the file at the next poll, however soon it comes.
    fn look_now(theme: &mut UserTheme) {
        theme.looked = None;
    }

    const ORANGE: Color32 = Color32::from_rgb(255, 136, 0);

    #[test]
    fn themes_are_named_as_the_settings_name_them() {
        let file = |name: &str| Choice::File(name.to_owned());
        assert_eq!(Choice::from_name("dark"), Choice::BuiltIn(Base::Dark));
        assert_eq!(Choice::from_name("Light"), Choice::BuiltIn(Base::Light));
        assert_eq!(Choice::from_name(""), Choice::BuiltIn(Base::Dark));
        assert_eq!(Choice::from_name("Studio"), file("Studio.toml"));
        assert_eq!(Choice::from_name("Studio.toml"), file("Studio.toml"));
        for choice in [Choice::BuiltIn(Base::Light), file("Studio.toml")] {
            assert_eq!(Choice::from_name(choice.name()), choice);
        }
        assert_eq!(file("Studio.toml").title(), "Studio");
        assert_eq!(Choice::BuiltIn(Base::Light).title(), "Light");
    }

    #[test]
    fn theme_files_change_their_base_and_are_followed_as_they_change() {
        let folder = Folder::new("follow");
        folder.write("Studio.toml", "base = \"light\"\nplayhead = \"#ff8800\"\n");
        let mut theme = studio(&folder);
        assert_eq!(theme.theme().playhead, ORANGE);
        assert_eq!(theme.theme().lane, Theme::light().lane);
        assert_eq!(theme.base(), Base::Light);
        assert_eq!((theme.error(), theme.message()), (None, None));
        assert!(theme.problems().is_empty());
        // Its fonts are egui's, which egui has already.
        assert_eq!(theme.take_fonts(), None);

        // Nothing changed, so there's nothing to read.
        look_now(&mut theme);
        assert!(!theme.poll());

        let text =
            "base = \"light\"\nplayhead = \"#00ff00\"\n[widgets.resting]\nfill = \"#204060\"\n";
        folder.write("Studio.toml", text);
        look_now(&mut theme);
        assert!(theme.poll());
        assert_eq!(theme.theme().playhead, Color32::from_rgb(0, 255, 0));
        let resting = theme.theme().widgets.resting;
        assert_eq!(resting.fill, Color32::from_rgb(32, 64, 96));

        // Changes are looked for at most every half a second.
        folder.write("Studio.toml", "playhead = \"#ff8800\"\n");
        assert!(!theme.poll());
        assert_eq!(theme.theme().playhead, Color32::from_rgb(0, 255, 0));
    }

    #[test]
    fn a_broken_file_falls_back_to_its_base_and_says_so() {
        let folder = Folder::new("broken");
        folder.write("Studio.toml", "base = \"light\"\nplayhead = \"#ff8800\n");
        let mut theme = studio(&folder);
        assert_eq!(theme.theme(), &Theme::light());
        let error = theme.error().unwrap();
        assert!(
            error.starts_with("The theme file isn't valid TOML at line 2: ")
                && error.ends_with(". Musicable uses the light theme in its place."),
            "{error}"
        );
        let message = theme.message().unwrap();
        assert!(message.contains("uses the light theme"), "{message}");

        // Bad entries are left out, and the rest applies.
        folder.write(
            "Studio.toml",
            "base = \"light\"\nplayhead = 7\nlane = \"#123456\"\n",
        );
        look_now(&mut theme);
        assert!(theme.poll());
        assert_eq!(theme.theme().lane, Color32::from_rgb(18, 52, 86));
        assert_eq!(theme.theme().playhead, Theme::light().playhead);
        assert_eq!(
            theme.problems(),
            [
                "playhead keeps the base's value: it should be a colour, such as \"#ff8800\", \
                 or \"#ff880080\" with an opacity"
            ]
        );
        assert_eq!(theme.error(), None);
        assert_eq!(
            theme.message().as_deref(),
            Some("Your theme file Studio.toml has problems: see Options > Appearance.")
        );

        // Fixed, there's nothing to say.
        folder.write("Studio.toml", "base = \"light\"\nplayhead = \"#ff8800\"\n");
        look_now(&mut theme);
        assert!(theme.poll());
        assert_eq!(theme.message(), None);

        // Gone, the dark theme stands in for it.
        fs::remove_file(folder.themes().join("Studio.toml")).unwrap();
        look_now(&mut theme);
        assert!(theme.poll());
        assert_eq!(theme.theme(), &Theme::dark());
        assert_eq!(
            theme.error(),
            Some(
                "There's no Studio.toml in the themes folder. Musicable uses the dark theme in its place."
            )
        );
    }

    #[test]
    fn the_built_in_themes_need_no_file() {
        let mut theme = UserTheme::new(None, Choice::BuiltIn(Base::Light));
        assert_eq!(theme.theme(), &Theme::light());
        assert_eq!((theme.error(), theme.message()), (None, None));
        assert!(!theme.poll());
        theme.choose(Choice::BuiltIn(Base::Dark));
        assert_eq!(theme.theme(), &Theme::dark());

        // A file needs the folder.
        theme.choose(Choice::File("Studio.toml".into()));
        assert_eq!(theme.theme(), &Theme::dark());
        assert!(theme.error().is_some());
    }

    #[test]
    fn exported_themes_start_files_of_their_own() {
        let folder = Folder::new("export");
        let mut theme = UserTheme::new(Some(folder.themes()), Choice::BuiltIn(Base::Light));
        let mine = folder.themes().join("Mine.toml");
        assert!(theme.export(&mine));
        assert_eq!(theme.choice(), &Choice::File("Mine.toml".into()));
        assert_eq!(theme.theme(), &Theme::light());
        let (file, problems) = ThemeFile::read(&mine).unwrap();
        assert_eq!(
            (file.base, file.len(), problems),
            (Base::Light, 0, Vec::new())
        );
        assert!(theme.notice().unwrap().contains("uses now"));

        // What it changes is written out as changes.
        folder.write("Mine.toml", "base = \"light\"\nplayhead = \"#ff8800\"\n");
        theme.reload();
        let elsewhere = folder.0.join("Shared.toml");
        assert!(!theme.export(&elsewhere), "it isn't in the themes folder");
        assert_eq!(theme.choice(), &Choice::File("Mine.toml".into()));
        let (file, _) = ThemeFile::read(&elsewhere).unwrap();
        assert_eq!(file.len(), 1);
        assert_eq!(file.theme().playhead, ORANGE);
        assert_eq!(
            theme.notice(),
            Some(format!("Exported the theme to {}.", elsewhere.display()).as_str())
        );

        // The themes folder's files are on offer, in order.
        folder.write("a.toml", "");
        folder.write("Notes.txt", "");
        fs::create_dir_all(folder.themes().join("old.toml")).unwrap();
        assert_eq!(theme.files(), ["a.toml", "Mine.toml"]);
    }

    #[test]
    fn fonts_come_from_beside_the_theme_or_from_egui() {
        let folder = Folder::new("fonts");
        let family = |fonts: &FontDefinitions, family| fonts.families[&family][0].clone();
        folder.write("Studio.toml", "[fonts]\ntext = \"Hack\"\n");
        let mut theme = studio(&folder);
        let fonts = theme.take_fonts().expect("other fonts than egui's");
        assert_eq!(family(&fonts, FontFamily::Proportional), "Hack");
        assert_eq!(family(&fonts, FontFamily::Monospace), "Hack");
        assert_eq!(theme.take_fonts(), None, "egui has them now");

        // A font file beside the theme.
        let hack = FontDefinitions::default().font_data["Hack"]
            .bytes()
            .to_vec();
        fs::write(folder.themes().join("Mono.ttf"), hack).unwrap();
        folder.write("Studio.toml", "[fonts]\nmonospace = \"Mono.ttf\"\n");
        theme.reload();
        let fonts = theme.take_fonts().unwrap();
        assert_eq!(family(&fonts, FontFamily::Proportional), "Ubuntu-Light");
        assert_eq!(family(&fonts, FontFamily::Monospace), "Mono.ttf");
        assert!(theme.problems().is_empty(), "{:?}", theme.problems());

        // Fonts that can't be used leave egui's, and say why. They're read
        // again only when the theme names others, or on Reload.
        fs::write(folder.themes().join("Broken.ttf"), "not a font").unwrap();
        let text =
            "playhead = \"#ff8800\"\n[fonts]\ntext = \"Broken.ttf\"\nmonospace = \"Gone.ttf\"\n";
        folder.write("Studio.toml", text);
        look_now(&mut theme);
        assert!(theme.poll());
        let fonts = theme.take_fonts().unwrap();
        assert_eq!(family(&fonts, FontFamily::Proportional), "Ubuntu-Light");
        assert_eq!(family(&fonts, FontFamily::Monospace), "Hack");
        assert_eq!(
            theme.problems(),
            [
                "fonts.text keeps egui's font: Broken.ttf isn't a font file Musicable can read",
                "fonts.monospace keeps egui's font: there's no Gone.ttf in the themes folder, and \
                 egui has no font by that name"
            ]
        );
        assert_eq!(theme.theme().playhead, ORANGE, "the rest applies");
        folder.write("Studio.toml", &text.replace("ff8800", "00ff00"));
        look_now(&mut theme);
        assert!(theme.poll());
        assert_eq!(theme.take_fonts(), None);
        assert_eq!(theme.problems().len(), 2, "the fonts' problems stay");
    }
}

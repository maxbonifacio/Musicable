//! Theme files: a built-in theme to start from, and what to change in it.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use super::values::{parse_colour, to_hex};
use super::{Base, Theme};

/// What a theme file's `format` field says, when it has one.
const FORMAT: &str = "musicable-theme";
/// The theme file format version this build reads and writes.
const VERSION: i64 = 1;
/// The largest number a theme takes, as a size in points.
const MAX_NUMBER: f64 = 4096.0;

/// The comment at the top of the theme files Musicable writes.
const HEADER: &str = "\
# Musicable theme: changes to the built-in theme that `base` names, dark or
# light.
#
# Each line sets a colour, a size or a font. Lines that start with # keep the
# base's value: take the # off to change one. Colours are \"#rrggbb\", or
# \"#rrggbbaa\" with an opacity, and sizes are in points. Musicable follows
# changes to the file as you save them.
";

/// The comments over the groups of entries that aren't in a section, by the
/// first entry of each.
const GROUPS: [(&str, &str); 6] = [
    (
        "mute",
        "Mute, solo, arm and the others while they're on, the meters, and warnings.",
    ),
    ("icon_button", "The transport bar's sizes."),
    ("lane", "The arrangement's colours."),
    (
        "white_row",
        "The piano roll's colours. Its grid and playhead are the arrangement's.",
    ),
    (
        "warp_marker",
        "The Sample Editor's colours. Its waveform is the arrangement's.",
    ),
    ("ruler_height", "The arrangement's sizes."),
];

/// The comments over the sections.
const SECTIONS: [(&str, &str); 14] = [
    ("mixer", "The mixer's sizes."),
    (
        "devices",
        "The devices panel's sizes, and the browser's width.",
    ),
    ("piano_roll", "The piano roll's sizes."),
    ("sample_editor", "The Sample Editor's sizes."),
    ("browser", "The browser's sizes."),
    (
        "dock",
        "The sizes of the panels' tab bars and the gaps between panels.",
    ),
    (
        "widgets",
        "Menus, buttons, text fields, windows and the panels they're in.",
    ),
    (
        "widgets.passive",
        "What can't be clicked: labels, separators and frames.",
    ),
    (
        "widgets.resting",
        "Buttons, checkboxes and sliders at rest,",
    ),
    ("widgets.hovered", "with the mouse over them,"),
    ("widgets.pressed", "while they're pressed or dragged,"),
    (
        "widgets.open",
        "and with their menu open, as a combo box's is.",
    ),
    (
        "fonts",
        "Fonts: a font file beside this one, or egui's \"Ubuntu-Light\" or\n# \"Hack\". \
         Empty for egui's own.",
    ),
    ("fonts.sizes", "The sizes of the widgets' text."),
];

/// A theme file: a built-in theme, and changes to it.
///
/// ```toml
/// format = "musicable-theme"
/// version = 1
/// base = "dark"
///
/// playhead = "#ff8800"
///
/// [mixer]
/// strip = 110
///
/// [widgets.resting]
/// background = "#3a3a3a"
/// ```
///
/// Entries are named after [`Theme`]'s fields, with a section, such as
/// `[mixer]`, for each part of it that has fields of its own. An entry
/// replaces the base's value, and what the file leaves out keeps it.
///
/// A bad entry doesn't stop the rest of the file from applying:
/// [`parse`](Self::parse) skips it and reports a [`ThemeProblem`]. `format`,
/// `version` and `base` are optional, and a file without a `base` changes
/// the dark theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeFile {
    /// The built-in theme the file changes.
    pub base: Base,
    /// The changes, by their entries' names, with the section's in front
    /// for those in one: `playhead`, `mixer.strip`.
    changes: BTreeMap<String, toml::Value>,
}

impl ThemeFile {
    /// Reads a theme file. Errors mean the file as a whole can't be used.
    /// Problems with single entries come back with the file, and the entries
    /// are skipped.
    pub fn parse(text: &str) -> Result<(Self, Vec<ThemeProblem>), ThemeFileError> {
        let mut table: toml::Table = toml::from_str(text).map_err(|error| {
            let line = error
                .span()
                .map(|span| text[..span.start].matches('\n').count() + 1);
            ThemeFileError::Toml {
                line,
                message: error.message().trim_end().to_owned(),
                base: base_line(text),
            }
        })?;
        match table.remove("format") {
            None => {}
            Some(toml::Value::String(format)) if format == FORMAT => {}
            Some(toml::Value::String(format)) => return Err(ThemeFileError::NotATheme(format)),
            Some(other) => return Err(ThemeFileError::NotATheme(other.to_string())),
        }
        match table.remove("version") {
            None => {}
            Some(toml::Value::Integer(VERSION)) => {}
            Some(other) => return Err(ThemeFileError::UnsupportedVersion(other.to_string())),
        }
        let mut problems = Vec::new();
        let base = match table.remove("base") {
            None => Base::Dark,
            Some(value) => {
                let name = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                Base::from_name(&name).unwrap_or_else(|| {
                    problems.push(ThemeProblem::UnknownBase(name));
                    Base::Dark
                })
            }
        };

        let reference = entries(&to_table(&base.theme()));
        let mut file = Self {
            base,
            changes: BTreeMap::new(),
        };
        for (key, value) in entries(&table) {
            let section = format!("{key}.");
            match reference.get(&key) {
                Some(was) => match accept(was, &value) {
                    Some(value) => {
                        file.changes.insert(key, value);
                    }
                    None => problems.push(ThemeProblem::BadValue {
                        expected: expected(was),
                        key,
                    }),
                },
                None if reference.keys().any(|known| known.starts_with(&section)) => {
                    problems.push(ThemeProblem::Section { key });
                }
                None => problems.push(ThemeProblem::Unknown { key }),
            }
        }
        Ok((file, problems))
    }

    /// Reads the theme file at `path`.
    pub fn read(path: &Path) -> Result<(Self, Vec<ThemeProblem>), ThemeFileError> {
        Self::parse(&fs::read_to_string(path).map_err(ThemeFileError::Io)?)
    }

    /// The changes that turn the built-in theme `base` into `theme`: an
    /// entry for each value that differs.
    pub fn diff(base: Base, theme: &Theme) -> Self {
        let reference = entries(&to_table(&base.theme()));
        let changes = entries(&to_table(theme))
            .into_iter()
            .filter(|(key, value)| reference.get(key) != Some(value))
            .collect();
        Self { base, changes }
    }

    /// The theme: the base with the file's changes.
    pub fn theme(&self) -> Theme {
        let base = self.base.theme();
        let mut table = to_table(&base);
        for (key, value) in &self.changes {
            set(&mut table, key, value.clone());
        }
        // The changes were checked as they were read, so this can't fail.
        toml::Value::Table(table).try_into().unwrap_or(base)
    }

    /// The file as TOML: every entry of the theme, with those that keep the
    /// base's value commented out, to show what can be changed.
    pub fn to_toml(&self) -> String {
        let body = toml::to_string(&self.theme()).expect("themes always serialize");
        let mut text = format!(
            "{HEADER}\nformat = \"{FORMAT}\"\nversion = {VERSION}\nbase = \"{}\"\n",
            self.base.name()
        );
        let mut section = String::new();
        for line in body.lines() {
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name.to_owned();
                if let Some(comment) = comment(&SECTIONS, name) {
                    text += &format!("# {comment}\n");
                }
                text += &format!("{line}\n");
                continue;
            }
            let Some((name, _)) = line.split_once(" = ") else {
                text += &format!("{line}\n");
                continue;
            };
            let key = if section.is_empty() {
                name.to_owned()
            } else {
                format!("{section}.{name}")
            };
            if let Some(comment) = comment(&GROUPS, &key) {
                text += &format!("\n# {comment}\n");
            }
            let mark = if self.changes.contains_key(&key) {
                ""
            } else {
                "# "
            };
            text += &format!("{mark}{line}\n");
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

    /// How many values the file changes.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// `theme` as a table of tables, as a theme file has it.
fn to_table(theme: &Theme) -> toml::Table {
    toml::Table::try_from(theme).expect("themes always serialize")
}

/// The entries of `table` that aren't tables themselves, by their names,
/// with their sections' in front: `mixer.strip`.
fn entries(table: &toml::Table) -> BTreeMap<String, toml::Value> {
    fn collect(table: &toml::Table, prefix: &str, entries: &mut BTreeMap<String, toml::Value>) {
        for (name, value) in table {
            let key = format!("{prefix}{name}");
            match value {
                toml::Value::Table(inner) => collect(inner, &format!("{key}."), entries),
                _ => {
                    entries.insert(key, value.clone());
                }
            }
        }
    }
    let mut entries = BTreeMap::new();
    collect(table, "", &mut entries);
    entries
}

/// Sets the entry called `key` in `table`, as [`entries`] names them, to
/// `value`.
fn set(table: &mut toml::Table, key: &str, value: toml::Value) {
    match key.split_once('.') {
        Some((section, rest)) => {
            if let Some(toml::Value::Table(inner)) = table.get_mut(section) {
                set(inner, rest, value);
            }
        }
        None => {
            table.insert(key.to_owned(), value);
        }
    }
}

/// `value` as an entry whose base value is `was`, written as the theme writes
/// it: a colour where `was` is one, a number from 0 to [`MAX_NUMBER`] where
/// it's a number, and so on. `None` if it isn't one.
fn accept(was: &toml::Value, value: &toml::Value) -> Option<toml::Value> {
    use toml::Value::{Array, Boolean, Float, Integer, String};
    match (was, value) {
        (String(was), String(text)) if was.starts_with('#') => {
            parse_colour(text).map(|colour| String(to_hex(colour)))
        }
        (String(_), String(text)) => Some(String(text.clone())),
        (Float(_), Float(_) | Integer(_)) => {
            let number = value.as_float().or(value.as_integer().map(|n| n as f64))?;
            (0.0..=MAX_NUMBER)
                .contains(&number)
                .then_some(Float(number))
        }
        (Array(was), Array(items)) if was.len() == items.len() => {
            let items = items.iter().zip(was).map(|(item, was)| accept(was, item));
            items.collect::<Option<Vec<_>>>().map(Array)
        }
        (Boolean(_), Boolean(on)) => Some(Boolean(*on)),
        _ => None,
    }
}

/// What an entry whose base value is `was` should be, for a problem to say.
fn expected(was: &toml::Value) -> String {
    match was {
        toml::Value::String(was) if was.starts_with('#') => {
            "a colour, such as \"#ff8800\", or \"#ff880080\" with an opacity".to_owned()
        }
        toml::Value::String(_) => "the name of a font, such as \"Inter.ttf\"".to_owned(),
        toml::Value::Array(items) => match items.first() {
            Some(toml::Value::String(_)) => format!("a list of {} colours", items.len()),
            _ => format!(
                "{} numbers from 0 to {MAX_NUMBER}, such as [8, 3]",
                items.len()
            ),
        },
        toml::Value::Boolean(_) => "true or false".to_owned(),
        _ => format!("a number from 0 to {MAX_NUMBER}"),
    }
}

/// The comment in `comments` for `key`.
fn comment<'a>(comments: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    comments
        .iter()
        .find(|(first, _)| *first == key)
        .map(|(_, comment)| *comment)
}

/// The built-in theme a `base = "..."` line in `text` names, for a file
/// that isn't valid TOML, to fall back to.
fn base_line(text: &str) -> Option<Base> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        let value = value.split('#').next()?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        (key.trim() == "base").then(|| Base::from_name(value))?
    })
}

/// Something wrong with one entry of a theme file. The rest of the file
/// still applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeProblem {
    /// No theme has an entry by this name.
    Unknown { key: String },
    /// This names a section of the theme, which holds entries, not a value.
    Section { key: String },
    /// The entry's value isn't what it should be, so it keeps the base's.
    BadValue { key: String, expected: String },
    /// No built-in theme has the `base` the file names, so it changes the
    /// dark one.
    UnknownBase(String),
}

impl fmt::Display for ThemeProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { key } => {
                write!(f, "{key} isn't something a theme sets, so it's left out")
            }
            Self::Section { key } => write!(
                f,
                "{key} holds a section's entries, under [{key}], so it's left out"
            ),
            Self::BadValue { key, expected } => {
                write!(f, "{key} keeps the base's value: it should be {expected}")
            }
            Self::UnknownBase(name) => write!(
                f,
                "there's no built-in theme called {name}, so the file changes the dark one"
            ),
        }
    }
}

/// Why a theme file can't be used at all.
#[derive(Debug)]
pub enum ThemeFileError {
    Io(io::Error),
    /// Not valid TOML, at `line` where it says. `base` is the built-in theme
    /// the file's `base` line names, if it could be read.
    Toml {
        line: Option<usize>,
        message: String,
        base: Option<Base>,
    },
    /// The `format` field names another kind of file.
    NotATheme(String),
    /// A version this build doesn't read, such as one from a newer build.
    UnsupportedVersion(String),
}

impl ThemeFileError {
    /// The built-in theme to use in the file's place: its base, where that
    /// can be told.
    pub fn base(&self) -> Base {
        match self {
            Self::Toml {
                base: Some(base), ..
            } => *base,
            _ => Base::Dark,
        }
    }
}

impl fmt::Display for ThemeFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read the theme file: {error}"),
            Self::Toml {
                line: Some(line),
                message,
                ..
            } => write!(
                f,
                "the theme file isn't valid TOML at line {line}: {message}"
            ),
            Self::Toml { message, .. } => write!(f, "the theme file isn't valid TOML: {message}"),
            Self::NotATheme(format) => {
                write!(f, "this is a \"{format}\" file, not a \"{FORMAT}\" file")
            }
            Self::UnsupportedVersion(version) => write!(
                f,
                "the theme file is version {version}, and this version of Musicable reads \
                 version {VERSION}"
            ),
        }
    }
}

impl std::error::Error for ThemeFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Color32, vec2};

    use super::*;

    fn parse(text: &str) -> (ThemeFile, Vec<ThemeProblem>) {
        ThemeFile::parse(text).unwrap()
    }

    #[test]
    fn files_change_their_base() {
        let text = "\
            base = \"light\"\n\
            playhead = \"#ff8800\"\n\
            clips = [\"#000001\", \"#000002\", \"#000003\", \"#000004\", \"#000005\", \
                     \"#000006\", \"#000007\", \"#000008\"]\n\
            [mixer]\n\
            strip = 110\n\
            [widgets]\n\
            item_spacing = [10, 4.5]\n\
            [widgets.resting]\n\
            background = \"#3a3a3a80\"\n\
            [fonts]\n\
            text = \"Inter.ttf\"\n";
        let (file, problems) = parse(text);
        assert_eq!(problems, []);
        assert_eq!(file.base, Base::Light);
        assert_eq!(file.len(), 6);
        let theme = file.theme();
        let light = Theme::light();
        assert_eq!(theme.playhead, Color32::from_rgb(255, 136, 0));
        assert_eq!(theme.clips[7], Color32::from_rgb(0, 0, 8));
        assert_eq!(theme.mixer.strip, 110.0);
        assert_eq!(theme.widgets.item_spacing, vec2(10.0, 4.5));
        let background = Color32::from_rgba_unmultiplied(0x3a, 0x3a, 0x3a, 0x80);
        assert_eq!(theme.widgets.resting.background, background);
        assert_eq!(theme.fonts.text, "Inter.ttf");
        // The rest is the base's.
        assert_eq!(theme.lane, light.lane);
        assert_eq!(theme.mixer.gap, light.mixer.gap);
        assert_eq!(theme.widgets.hovered, light.widgets.hovered);
    }

    #[test]
    fn a_file_without_a_base_changes_the_dark_theme() {
        let (file, problems) = parse("[piano_roll]\nkeys = 70.0\n");
        assert_eq!(problems, []);
        let mut expected = Theme::dark();
        expected.piano_roll.keys = 70.0;
        assert_eq!(file.theme(), expected);
        assert_eq!(parse("").0.theme(), Theme::dark());
    }

    #[test]
    fn bad_entries_are_skipped_and_reported() {
        let text = "\
            format = \"musicable-theme\"\n\
            version = 1\n\
            base = \"solarized\"\n\
            playhed = \"#ffffff\"\n\
            playhead = \"orange\"\n\
            lane = \"#202020\"\n\
            browser = 4\n\
            clips = [\"#000000\"]\n\
            [mixer]\n\
            strip = -10\n\
            gap = 6\n\
            [widgets]\n\
            item_spacing = [1, 2, 3]\n\
            [fonts]\n\
            text = 12\n";
        let (file, problems) = parse(text);
        let key = |key: &str| key.to_owned();
        let expected = [
            ThemeProblem::UnknownBase(key("solarized")),
            ThemeProblem::Section {
                key: key("browser"),
            },
            ThemeProblem::BadValue {
                key: key("clips"),
                expected: key("a list of 8 colours"),
            },
            ThemeProblem::BadValue {
                key: key("fonts.text"),
                expected: key("the name of a font, such as \"Inter.ttf\""),
            },
            ThemeProblem::BadValue {
                key: key("mixer.strip"),
                expected: key("a number from 0 to 4096"),
            },
            ThemeProblem::BadValue {
                key: key("playhead"),
                expected: key("a colour, such as \"#ff8800\", or \"#ff880080\" with an opacity"),
            },
            ThemeProblem::Unknown {
                key: key("playhed"),
            },
            ThemeProblem::BadValue {
                key: key("widgets.item_spacing"),
                expected: key("2 numbers from 0 to 4096, such as [8, 3]"),
            },
        ];
        assert_eq!(problems, expected);
        // What was right applies, to the dark theme.
        let theme = file.theme();
        assert_eq!(theme.lane, Color32::from_gray(32));
        assert_eq!(theme.mixer.gap, 6.0);
        assert_eq!(theme.playhead, Theme::dark().playhead);
        assert_eq!(file.len(), 2);
        assert_eq!(
            problems[6].to_string(),
            "playhed isn't something a theme sets, so it's left out"
        );
        assert_eq!(
            problems[4].to_string(),
            "mixer.strip keeps the base's value: it should be a number from 0 to 4096"
        );
    }

    #[test]
    fn files_that_cant_be_used_say_why() {
        let error = ThemeFile::parse("base = \"light\"\nplayhead = \"#fff\n").unwrap_err();
        assert_eq!(error.base(), Base::Light, "it falls back to its base");
        let message = error.to_string();
        assert!(
            message.starts_with("the theme file isn't valid TOML at line 2: "),
            "{message}"
        );
        assert!(!message.contains('\n'), "{message}");
        let error = ThemeFile::parse("base = 'light' # mine\n[[").unwrap_err();
        assert_eq!(error.base(), Base::Light);
        assert_eq!(ThemeFile::parse("[[").unwrap_err().base(), Base::Dark);

        let error = ThemeFile::parse("format = \"musicable-keymap\"\n").unwrap_err();
        assert!(matches!(error, ThemeFileError::NotATheme(_)), "{error}");
        let error = ThemeFile::parse("version = 2\n").unwrap_err();
        assert_eq!(
            error.to_string(),
            "the theme file is version 2, and this version of Musicable reads version 1"
        );
        assert_eq!(error.base(), Base::Dark);
    }

    #[test]
    fn the_built_in_themes_write_out_as_files_that_change_nothing() {
        for base in Base::ALL {
            let file = ThemeFile::diff(base, &base.theme());
            assert!(file.is_empty());
            let text = file.to_toml();
            assert!(text.starts_with("# Musicable theme"), "{text}");
            let (read, problems) = ThemeFile::parse(&text).unwrap();
            assert_eq!(problems, []);
            assert_eq!(read, file);
            assert_eq!(read.theme(), base.theme());
            // Every entry is there, to change.
            let commented: Vec<_> = text.lines().filter(|l| l.starts_with("# ")).collect();
            assert!(commented.contains(&"# playhead = \"#f0f0f0\"") || base == Base::Light);
            let uncommented: String = text
                .lines()
                .map(|line| match line.strip_prefix("# ") {
                    Some(entry) if entry.contains(" = ") => entry,
                    _ => line,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let (all, problems) = parse(&uncommented);
            assert_eq!(problems, []);
            assert_eq!(all.theme(), base.theme());
            assert_eq!(all.len(), entries(&to_table(&base.theme())).len());
        }
    }

    #[test]
    fn changes_are_written_out_as_they_are_and_the_rest_as_comments() {
        let mut theme = Theme::light();
        theme.playhead = Color32::from_rgb(255, 136, 0);
        theme.widgets.hovered.corner = 2.0;
        theme.fonts.sizes.body = 14.0;
        let file = ThemeFile::diff(Base::Light, &theme);
        assert_eq!(file.len(), 3);
        let text = file.to_toml();
        assert!(text.contains("\nbase = \"light\"\n"), "{text}");
        assert!(text.contains("\nplayhead = \"#ff8800\"\n"), "{text}");
        assert!(text.contains("\n[widgets.hovered]\n"), "{text}");
        assert!(text.contains("\ncorner = 2.0\n"), "{text}");
        assert!(text.contains("\n[fonts.sizes]\nbody = 14.0\n"), "{text}");
        assert!(text.contains("\n# lane = "), "{text}");
        let (read, problems) = parse(&text);
        assert_eq!(problems, []);
        assert_eq!(read.theme(), theme);
        // Comments explain the groups, under 80 columns.
        assert!(
            text.contains("\n# The arrangement's colours.\n# lane = "),
            "{text}"
        );
        assert!(text.contains("\n# The mixer's sizes.\n[mixer]\n"), "{text}");
        for line in text.lines().filter(|line| !line.contains(" = ")) {
            assert!(line.len() <= 80, "{line}");
        }
    }

    #[test]
    fn every_group_and_section_has_its_entries() {
        let names = entries(&to_table(&Theme::dark()));
        for (first, _) in GROUPS {
            assert!(names.contains_key(first), "{first}");
        }
        for (section, _) in SECTIONS {
            let section = format!("{section}.");
            assert!(
                names.keys().any(|key| key.starts_with(&section)),
                "{section}"
            );
        }
    }

    #[test]
    fn every_colour_is_written_in_hex() {
        // A colour field without its #[serde(with)] would be written as a
        // list of whole numbers if egui's serde were turned on. Every size is
        // a float.
        for (key, value) in entries(&to_table(&Theme::dark())) {
            let whole = |value: &toml::Value| value.is_integer();
            let has_whole = match &value {
                toml::Value::Array(items) => items.iter().any(whole),
                value => whole(value),
            };
            assert!(
                !has_whole,
                "{key} = {value} isn't written as a theme writes it"
            );
        }
    }
}

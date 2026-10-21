//! Keymaps, which say which key chord runs which command, and keymap files,
//! which change them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{fmt, fs, io, process};

use serde::{Deserialize, Serialize};

use crate::{ChordError, CommandId, KeyChord, Registry};

/// The default keymap's file. It follows Ableton Live's shortcuts where Live
/// has one.
const DEFAULT_KEYMAP: &str = include_str!("../keymaps/default.toml");

/// Maps key chords to commands. A chord runs one command, and a command can
/// have any number of chords.
///
/// Keymaps are plain data. Musicable starts from the
/// [default keymap](Self::default_keymap), and the user's
/// [keymap file](KeymapFile) changes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<KeyChord, CommandId>,
}

impl Keymap {
    /// An empty keymap.
    pub fn new() -> Self {
        Self::default()
    }

    /// The keymap Musicable starts with, from `keymaps/default.toml`.
    pub fn default_keymap() -> Self {
        let (file, problems) =
            KeymapFile::parse(DEFAULT_KEYMAP).expect("the default keymap is a keymap file");
        debug_assert!(problems.is_empty(), "the default keymap has {problems:?}");
        let mut keymap = Self::new();
        file.apply_to(&mut keymap);
        keymap
    }

    /// Binds `chord` to `command`, returning the command it replaced, if any,
    /// so the UI can warn about conflicts.
    pub fn bind(&mut self, chord: KeyChord, command: impl Into<CommandId>) -> Option<CommandId> {
        self.bindings.insert(chord, command.into())
    }

    pub fn unbind(&mut self, chord: KeyChord) -> Option<CommandId> {
        self.bindings.remove(&chord)
    }

    /// Removes all of `command`'s chords.
    pub fn unbind_command(&mut self, command: &CommandId) {
        self.bindings.retain(|_, c| c != command);
    }

    /// Gives `command` the chords it has in `defaults`, in place of the ones
    /// it has now. A chord that another command has now is taken from it.
    /// Returns the chords taken, with the commands that had them.
    pub fn reset_command(
        &mut self,
        command: &CommandId,
        defaults: &Keymap,
    ) -> Vec<(KeyChord, CommandId)> {
        self.unbind_command(command);
        defaults
            .chords_for(command)
            .filter_map(|chord| Some((chord, self.bind(chord, command.clone())?)))
            .collect()
    }

    pub fn lookup(&self, chord: KeyChord) -> Option<&CommandId> {
        self.bindings.get(&chord)
    }

    pub fn chords_for<'a>(&'a self, command: &'a CommandId) -> impl Iterator<Item = KeyChord> + 'a {
        self.bindings
            .iter()
            .filter(move |(_, c)| *c == command)
            .map(|(&chord, _)| chord)
    }

    /// Every binding, in chord order.
    pub fn iter(&self) -> impl Iterator<Item = (KeyChord, &CommandId)> {
        self.bindings
            .iter()
            .map(|(&chord, command)| (chord, command))
    }

    /// Bindings that point at commands missing from `registry`.
    pub fn dangling<'a>(
        &'a self,
        registry: &'a Registry,
    ) -> impl Iterator<Item = (KeyChord, &'a CommandId)> {
        self.iter().filter(|(_, c)| registry.get(c).is_none())
    }

    fn commands(&self) -> BTreeSet<&CommandId> {
        self.bindings.values().collect()
    }
}

/// The keymap file format version this build reads and writes.
pub const KEYMAP_VERSION: u32 = 1;

/// What a keymap file's `format` field says, when it has one.
const KEYMAP_FORMAT: &str = "musicable-keymap";

/// The comment at the top of keymap files Musicable writes.
const HEADER: &str = "\
# Musicable keymap: changes to the default keymap.
#
# Each line under [bindings] gives a command the key chords that run it, in
# place of its default ones. An empty list, [], unbinds the command. A chord
# runs one command, so giving a chord to one command takes it from another.
";

/// A keymap file: changes to a base keymap, usually the default keymap.
///
/// ```toml
/// format = "musicable-keymap"
/// version = 1
///
/// [bindings]
/// "transport.toggle_metronome" = ["M", "Ctrl+M"]
/// "transport.toggle_loop" = []
/// ```
///
/// Each entry gives a command the chords that run it, replacing the chords it
/// has in the base. An entry with no chords unbinds the command, and commands
/// without an entry keep their chords. A chord runs one command, so an entry
/// that gives a chord to one command takes it from the command that had it.
///
/// A bad entry doesn't stop the rest of the file from loading:
/// [`parse`](Self::parse) skips it and reports a [`Problem`]. So does a chord
/// that two entries claim, which then runs neither command. `format` and
/// `version` are optional, so a hand-written file can start at `[bindings]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeymapFile {
    bindings: BTreeMap<CommandId, BTreeSet<KeyChord>>,
}

impl KeymapFile {
    /// Reads a keymap file. Errors mean the file as a whole can't be used.
    /// Problems with single entries come back with the file, and the entries
    /// are skipped.
    pub fn parse(text: &str) -> Result<(Self, Vec<Problem>), KeymapFileError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            format: Option<String>,
            version: Option<u32>,
            #[serde(default)]
            bindings: toml::Table,
        }

        let raw: Raw = toml::from_str(text).map_err(KeymapFileError::Toml)?;
        if let Some(format) = raw.format.filter(|f| f != KEYMAP_FORMAT) {
            return Err(KeymapFileError::NotAKeymap(format));
        }
        let version = raw.version.unwrap_or(KEYMAP_VERSION);
        if version != KEYMAP_VERSION {
            return Err(KeymapFileError::UnsupportedVersion(version));
        }

        let mut entries = Vec::new();
        flatten(&raw.bindings, "", &mut entries);
        let mut file = Self::default();
        let mut problems = Vec::new();
        for (command, value) in entries {
            match parse_chords(&command, value) {
                Ok(chords) => file.bindings.entry(command).or_default().extend(chords),
                Err(problem) => problems.push(problem),
            }
        }
        problems.extend(
            file.conflicts()
                .into_iter()
                .map(|(chord, commands)| Problem::Conflict { chord, commands }),
        );
        Ok((file, problems))
    }

    /// Reads the keymap file at `path`.
    pub fn read(path: &Path) -> Result<(Self, Vec<Problem>), KeymapFileError> {
        Self::parse(&fs::read_to_string(path).map_err(KeymapFileError::Io)?)
    }

    /// The changes that turn `base` into `keymap`: an entry for each command
    /// whose chords differ.
    pub fn diff(base: &Keymap, keymap: &Keymap) -> Self {
        let mut bindings = BTreeMap::new();
        for command in base.commands().union(&keymap.commands()) {
            let before: BTreeSet<_> = base.chords_for(command).collect();
            let after: BTreeSet<_> = keymap.chords_for(command).collect();
            if before != after {
                bindings.insert((*command).clone(), after);
            }
        }
        Self { bindings }
    }

    /// Makes the file's changes to `keymap`.
    pub fn apply_to(&self, keymap: &mut Keymap) {
        let conflicts: BTreeSet<KeyChord> = self.conflicts().into_keys().collect();
        for command in self.bindings.keys() {
            keymap.unbind_command(command);
        }
        for &chord in &conflicts {
            keymap.unbind(chord);
        }
        for (command, chords) in &self.bindings {
            for &chord in chords.difference(&conflicts) {
                keymap.bind(chord, command.clone());
            }
        }
    }

    /// The file as TOML, starting with a comment that explains it.
    pub fn to_toml(&self) -> String {
        #[derive(Serialize)]
        struct Raw<'a> {
            format: &'a str,
            version: u32,
            bindings: BTreeMap<&'a str, Vec<String>>,
        }

        let raw = Raw {
            format: KEYMAP_FORMAT,
            version: KEYMAP_VERSION,
            bindings: self
                .bindings
                .iter()
                .map(|(command, chords)| {
                    let chords = chords.iter().map(KeyChord::to_string).collect();
                    (command.as_str(), chords)
                })
                .collect(),
        };
        let body = toml::to_string(&raw).expect("keymaps always serialize");
        format!("{HEADER}\n{body}")
    }

    /// Writes the file to `path`, replacing any file there.
    ///
    /// It writes a temporary file next to `path` and then renames it over the
    /// old one, so a crash or a full disk never leaves a half-written keymap.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        write_replacing(path, self.to_toml().as_bytes())
    }

    /// How many commands the file changes.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Chords that more than one entry claims, with the commands claiming
    /// them.
    fn conflicts(&self) -> BTreeMap<KeyChord, Vec<CommandId>> {
        let mut claims: BTreeMap<KeyChord, Vec<CommandId>> = BTreeMap::new();
        for (command, chords) in &self.bindings {
            for &chord in chords {
                claims.entry(chord).or_default().push(command.clone());
            }
        }
        claims.retain(|_, commands| commands.len() > 1);
        claims
    }
}

/// Writes `bytes` to a new file next to `path`, then renames it over `path`,
/// as project files are saved.
fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU32 = AtomicU32::new(0);
    let Some(name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path doesn't name a file",
        ));
    };
    let mut temporary = OsString::from(".");
    temporary.push(name);
    let write = WRITES.fetch_add(1, Ordering::Relaxed);
    temporary.push(format!(".{}-{write}.tmp", process::id()));
    let temporary = path.with_file_name(temporary);
    let written = fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        // The error that matters is the one being returned.
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Collects the entries of a `[bindings]` table. Command ids are dotted, so
/// TOML reads a bare `transport.play = ...` as a `play` entry in a nested
/// `transport` table: join those back into one id.
fn flatten<'a>(
    table: &'a toml::Table,
    prefix: &str,
    entries: &mut Vec<(CommandId, &'a toml::Value)>,
) {
    for (name, value) in table {
        let id = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::Table(inner) => flatten(inner, &id, entries),
            _ => entries.push((CommandId::from(id), value)),
        }
    }
}

/// An entry's chords: a list of them, or one on its own.
fn parse_chords(command: &CommandId, value: &toml::Value) -> Result<Vec<KeyChord>, Problem> {
    let not_chords = || Problem::NotChords {
        command: command.clone(),
    };
    let items = match value {
        toml::Value::Array(items) => items.as_slice(),
        toml::Value::String(_) => std::slice::from_ref(value),
        _ => return Err(not_chords()),
    };
    items
        .iter()
        .map(|item| {
            let text = item.as_str().ok_or_else(not_chords)?;
            text.parse().map_err(|error| Problem::BadChord {
                command: command.clone(),
                chord: text.to_owned(),
                error,
            })
        })
        .collect()
}

/// Something wrong with one entry of a keymap file. The rest of the file
/// still loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The entry isn't a key chord or a list of them, so it's skipped.
    NotChords { command: CommandId },
    /// One of the entry's chords doesn't parse, so the entry is skipped.
    BadChord {
        command: CommandId,
        chord: String,
        error: ChordError,
    },
    /// Several entries claim the chord, so it runs none of their commands.
    Conflict {
        chord: KeyChord,
        commands: Vec<CommandId>,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotChords { command } => write!(
                f,
                "{command} keeps its default chords: it should be given a key chord or a list \
                 of them"
            ),
            Self::BadChord {
                command,
                chord,
                error,
            } => write!(
                f,
                "{command} keeps its default chords: \"{chord}\" isn't a key chord ({error})"
            ),
            Self::Conflict { chord, commands } => {
                write!(f, "{chord} is given to ")?;
                for (i, command) in commands.iter().enumerate() {
                    match i {
                        0 => {}
                        _ if i + 1 == commands.len() => f.write_str(" and ")?,
                        _ => f.write_str(", ")?,
                    }
                    write!(f, "{command}")?;
                }
                f.write_str(", so it runs none of them")
            }
        }
    }
}

/// Why a keymap file can't be used at all.
#[derive(Debug)]
pub enum KeymapFileError {
    Io(io::Error),
    /// Not valid TOML, or a field that isn't what it should be.
    Toml(toml::de::Error),
    /// The `format` field names another kind of file.
    NotAKeymap(String),
    /// A version this build doesn't read, such as one from a newer build.
    UnsupportedVersion(u32),
}

impl fmt::Display for KeymapFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read the keymap file: {error}"),
            Self::Toml(error) => write!(f, "the keymap file isn't valid: {error}"),
            Self::NotAKeymap(format) => {
                write!(
                    f,
                    "this is a \"{format}\" file, not a \"{KEYMAP_FORMAT}\" file"
                )
            }
            Self::UnsupportedVersion(version) => write!(
                f,
                "the keymap file is version {version}, and this version of Musicable reads \
                 version {KEYMAP_VERSION}"
            ),
        }
    }
}

impl std::error::Error for KeymapFileError {
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
    use crate::{Key, Modifiers};

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    fn command(id: &str) -> CommandId {
        id.into()
    }

    /// A small base keymap.
    fn base() -> Keymap {
        let mut map = Keymap::new();
        map.bind(chord("Space"), "transport.play_stop");
        map.bind(chord("Shift+Space"), "transport.play");
        map.bind(chord("O"), "transport.toggle_metronome");
        map.bind(chord("Ctrl+L"), "transport.toggle_loop");
        map
    }

    /// Reads `text` on top of [`base`].
    fn load(text: &str) -> (Keymap, Vec<Problem>) {
        let (file, problems) = KeymapFile::parse(text).unwrap();
        let mut map = base();
        file.apply_to(&mut map);
        (map, problems)
    }

    #[test]
    fn rebinding_reports_the_replaced_command() {
        let mut map = Keymap::new();
        let space = KeyChord::from(Key::Space);
        assert_eq!(map.bind(space, "transport.play"), None);
        assert_eq!(
            map.bind(space, "transport.stop"),
            Some("transport.play".into())
        );
        assert_eq!(map.lookup(space), Some(&"transport.stop".into()));
    }

    #[test]
    fn resetting_a_command_takes_its_default_chords_back() {
        let loop_ = command("transport.toggle_loop");
        let metronome = command("transport.toggle_metronome");
        let mut map = base();
        map.bind(chord("Ctrl+L"), metronome.clone());
        map.bind(chord("M"), metronome.clone());
        map.bind(chord("L"), loop_.clone());

        // The loop gets Ctrl+L back from the metronome, and loses L.
        let taken = map.reset_command(&loop_, &base());
        assert_eq!(taken, [(chord("Ctrl+L"), metronome.clone())]);
        assert_eq!(
            map.chords_for(&loop_).collect::<Vec<_>>(),
            [chord("Ctrl+L")]
        );
        assert_eq!(map.lookup(chord("L")), None);
        let chords: Vec<_> = map.chords_for(&metronome).collect();
        assert_eq!(chords, [chord("M"), chord("O")]);

        assert_eq!(map.reset_command(&metronome, &base()), []);
        assert_eq!(map, base());

        // A command with no default chords ends up with none.
        let play = command("transport.play");
        map.bind(chord("P"), play.clone());
        map.reset_command(&play, &Keymap::new());
        assert_eq!(map.chords_for(&play).count(), 0);
    }

    #[test]
    fn dangling_bindings_are_found() {
        let mut reg = Registry::new();
        assert!(reg.register("transport.play", "Play", |_| {}));
        assert!(!reg.register("transport.play", "Play again", |_| {}));
        let mut map = Keymap::new();
        map.bind(Key::Space.into(), "transport.play");
        map.bind(Key::X.into(), "does.not.exist");
        let dangling: Vec<_> = map.dangling(&reg).map(|(_, c)| c.as_str()).collect();
        assert_eq!(dangling, ["does.not.exist"]);
    }

    #[test]
    fn an_entry_replaces_the_commands_chords() {
        let (map, problems) = load(
            r#"
            [bindings]
            "transport.toggle_metronome" = ["M", "Ctrl+M"]
            "transport.toggle_loop" = []
            "#,
        );
        assert_eq!(problems, []);
        let metronome = command("transport.toggle_metronome");
        let chords: Vec<_> = map.chords_for(&metronome).collect();
        assert_eq!(chords, [chord("M"), chord("Ctrl+M")]);
        assert_eq!(map.lookup(chord("O")), None);
        assert_eq!(map.chords_for(&command("transport.toggle_loop")).count(), 0);
        // Commands without an entry keep their chords.
        assert_eq!(
            map.lookup(chord("Space")),
            Some(&command("transport.play_stop"))
        );
    }

    #[test]
    fn an_entry_takes_a_chord_from_another_command() {
        let (map, problems) = load(r#"bindings = { "transport.play" = "Space" }"#);
        assert_eq!(problems, []);
        assert_eq!(map.lookup(chord("Space")), Some(&command("transport.play")));
        assert_eq!(map.lookup(chord("Shift+Space")), None);
        assert_eq!(map.chords_for(&command("transport.play_stop")).count(), 0);
    }

    #[test]
    fn dotted_keys_name_commands() {
        let (map, problems) = load(
            r#"
            [bindings]
            transport.play = ["P"]

            [bindings.edit]
            undo = ["Ctrl+Z"]
            "#,
        );
        assert_eq!(problems, []);
        assert_eq!(map.lookup(chord("P")), Some(&command("transport.play")));
        assert_eq!(map.lookup(chord("Ctrl+Z")), Some(&command("edit.undo")));
    }

    #[test]
    fn a_bad_entry_is_skipped() {
        let (map, problems) = load(
            r#"
            [bindings]
            "transport.play" = ["P", "Ctrl+Spce"]
            "transport.toggle_loop" = 5
            "transport.toggle_metronome" = ["M"]
            "#,
        );
        assert_eq!(
            problems,
            [
                Problem::BadChord {
                    command: command("transport.play"),
                    chord: "Ctrl+Spce".into(),
                    error: ChordError::UnknownKey("Spce".into()),
                },
                Problem::NotChords {
                    command: command("transport.toggle_loop"),
                },
            ]
        );
        // The bad entries leave their commands as they were.
        assert_eq!(
            map.lookup(chord("Shift+Space")),
            Some(&command("transport.play"))
        );
        assert_eq!(map.lookup(chord("P")), None);
        assert_eq!(
            map.lookup(chord("Ctrl+L")),
            Some(&command("transport.toggle_loop"))
        );
        // The good one still applies.
        assert_eq!(
            map.lookup(chord("M")),
            Some(&command("transport.toggle_metronome"))
        );
        assert_eq!(
            problems[0].to_string(),
            "transport.play keeps its default chords: \"Ctrl+Spce\" isn't a key chord (there's \
             no key called \"Spce\")"
        );
    }

    #[test]
    fn a_chord_two_entries_claim_runs_neither() {
        let (map, problems) = load(
            r#"
            [bindings]
            "transport.toggle_loop" = ["L", "Ctrl+L"]
            "transport.toggle_metronome" = ["l"]
            "transport.play_stop" = ["Space"]
            "#,
        );
        assert_eq!(
            problems,
            [Problem::Conflict {
                chord: chord("L"),
                commands: vec![
                    command("transport.toggle_loop"),
                    command("transport.toggle_metronome")
                ],
            }]
        );
        assert_eq!(
            problems[0].to_string(),
            "L is given to transport.toggle_loop and transport.toggle_metronome, so it runs none of them"
        );
        assert_eq!(map.lookup(chord("L")), None);
        assert_eq!(
            map.lookup(chord("Ctrl+L")),
            Some(&command("transport.toggle_loop"))
        );
        // The metronome lost O to its entry and L to the conflict.
        assert_eq!(
            map.chords_for(&command("transport.toggle_metronome"))
                .count(),
            0
        );
    }

    #[test]
    fn files_that_cant_be_used_are_errors() {
        let error = |text| KeymapFile::parse(text).unwrap_err();
        assert!(matches!(error("[bindings"), KeymapFileError::Toml(_)));
        assert!(matches!(error("bindngs = {}"), KeymapFileError::Toml(_)));
        assert!(matches!(
            error("version = \"one\""),
            KeymapFileError::Toml(_)
        ));
        assert!(matches!(
            error("format = \"musicable-project\""),
            KeymapFileError::NotAKeymap(f) if f == "musicable-project"
        ));
        assert!(matches!(
            error("version = 2"),
            KeymapFileError::UnsupportedVersion(2)
        ));
        assert!(KeymapFile::parse("").unwrap().0.is_empty());
        assert!(KeymapFile::parse("format = \"musicable-keymap\"\nversion = 1").is_ok());
    }

    #[test]
    fn a_saved_file_holds_only_the_changes() {
        let mut map = base();
        map.bind(chord("O"), "transport.toggle_loop");
        map.bind(chord("Ctrl+Shift+Up"), "transport.tempo_up");
        let file = KeymapFile::diff(&base(), &map);
        assert_eq!(
            file.to_toml(),
            format!(
                "{HEADER}\nformat = \"musicable-keymap\"\nversion = 1\n\n[bindings]\n\
                 \"transport.tempo_up\" = [\"Ctrl+Shift+Up\"]\n\
                 \"transport.toggle_loop\" = [\"O\", \"Ctrl+L\"]\n\
                 \"transport.toggle_metronome\" = []\n"
            )
        );
        assert!(KeymapFile::diff(&map, &map).is_empty());
    }

    #[test]
    fn saving_and_loading_round_trips() {
        // Random edits to the default keymap, from a fixed seed.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let commands = [
            "a.one",
            "a.two",
            "b.three",
            "transport.play",
            "transport.stop",
        ];
        for _ in 0..200 {
            let base = Keymap::default_keymap();
            let mut map = base.clone();
            for _ in 0..next(12) {
                let key = Key::ALL[next(Key::ALL.len())];
                let mods = [
                    Modifiers::NONE,
                    Modifiers::CTRL,
                    Modifiers::SHIFT | Modifiers::ALT,
                ];
                let chord = KeyChord::new(key, mods[next(mods.len())]);
                match next(3) {
                    0 => {
                        map.unbind(chord);
                    }
                    _ => {
                        map.bind(chord, commands[next(commands.len())]);
                    }
                }
            }
            let text = KeymapFile::diff(&base, &map).to_toml();
            let (file, problems) = KeymapFile::parse(&text).unwrap();
            assert_eq!(problems, []);
            let mut loaded = base.clone();
            file.apply_to(&mut loaded);
            assert_eq!(loaded, map, "{text}");
        }
    }

    #[test]
    fn remapping_by_editing_the_keymap_file() {
        let dir = std::env::temp_dir().join(format!("musicable-keymap-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keymap.toml");

        // The app saves the user's changes...
        let mut map = Keymap::default_keymap();
        map.bind(chord("M"), "transport.toggle_metronome");
        KeymapFile::diff(&Keymap::default_keymap(), &map)
            .write(&path)
            .unwrap();

        // ...the user edits the file by hand...
        let text = fs::read_to_string(&path).unwrap();
        let text = text.replace("[\"M\", \"O\"]", "[\"Ctrl+M\"]");
        fs::write(&path, text).unwrap();

        // ...and the next start loads it.
        let (file, problems) = KeymapFile::read(&path).unwrap();
        assert_eq!(problems, []);
        let mut loaded = Keymap::default_keymap();
        file.apply_to(&mut loaded);
        let metronome = command("transport.toggle_metronome");
        assert_eq!(
            loaded.chords_for(&metronome).collect::<Vec<_>>(),
            [chord("Ctrl+M")]
        );
        fs::remove_dir_all(&dir).unwrap();

        assert!(matches!(
            KeymapFile::read(&path),
            Err(KeymapFileError::Io(_))
        ));
    }
}

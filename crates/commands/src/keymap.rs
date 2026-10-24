//! Keymaps, which say which key chord runs which command, and keymap files,
//! which change them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{fmt, fs, io, process};

use serde::{Deserialize, Serialize};

use crate::{ChordError, CommandId, Focus, KeyChord, Registry, panel};

/// The default keymap's file. It follows Ableton Live's shortcuts where Live
/// has one.
const DEFAULT_KEYMAP: &str = include_str!("../keymaps/default.toml");

/// Where a key binding works: everywhere, or in one panel, while it has the
/// focus, in place of what the chord runs everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Everywhere,
    /// The panel with this id, one of [`panel::ALL`].
    Panel(&'static str),
}

impl Scope {
    /// The scope of the panel with id `id`, if there's one.
    pub fn panel(id: &str) -> Option<Self> {
        panel::find(id).map(Self::Panel)
    }

    /// Where `focus` is.
    pub fn of(focus: Focus) -> Self {
        Self::Panel(focus.panel())
    }
}

/// Maps key chords to commands, everywhere and in single panels. A chord
/// runs one command in each place: in the panel with the focus, the
/// panel's own if it has one, or else the one everywhere. A command can
/// have any number of chords.
///
/// Keymaps are plain data. Musicable starts from the
/// [default keymap](Self::default_keymap), and the user's
/// [keymap file](KeymapFile) changes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<(Scope, KeyChord), CommandId>,
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

    /// Binds `chord` to `command` everywhere, returning the command it
    /// replaced, if any, so the UI can warn about conflicts.
    pub fn bind(&mut self, chord: KeyChord, command: impl Into<CommandId>) -> Option<CommandId> {
        self.bind_in(Scope::Everywhere, chord, command)
    }

    /// Binds `chord` to `command` in `scope`, returning the command it
    /// replaced there, if any.
    pub fn bind_in(
        &mut self,
        scope: Scope,
        chord: KeyChord,
        command: impl Into<CommandId>,
    ) -> Option<CommandId> {
        self.bindings.insert((scope, chord), command.into())
    }

    /// Unbinds `chord` everywhere, but not in the panels that bind it.
    pub fn unbind(&mut self, chord: KeyChord) -> Option<CommandId> {
        self.unbind_in(Scope::Everywhere, chord)
    }

    pub fn unbind_in(&mut self, scope: Scope, chord: KeyChord) -> Option<CommandId> {
        self.bindings.remove(&(scope, chord))
    }

    /// Removes all of `command`'s chords, everywhere and in every panel.
    pub fn unbind_command(&mut self, command: &CommandId) {
        self.bindings.retain(|_, c| c != command);
    }

    /// Removes `command`'s chords in `scope`.
    fn unbind_command_in(&mut self, command: &CommandId, scope: Scope) {
        self.bindings
            .retain(|&(place, _), c| place != scope || c != command);
    }

    /// Gives `command` the chords it has in `defaults`, everywhere and in
    /// each panel, in place of the ones it has now. A chord that another
    /// command has in the same place now is taken from it. Returns the
    /// chords taken, with where and the commands that had them.
    pub fn reset_command(
        &mut self,
        command: &CommandId,
        defaults: &Keymap,
    ) -> Vec<(Scope, KeyChord, CommandId)> {
        self.unbind_command(command);
        defaults
            .bindings_for(command)
            .filter_map(|(scope, chord)| {
                Some((scope, chord, self.bind_in(scope, chord, command.clone())?))
            })
            .collect()
    }

    /// The command `chord` runs everywhere.
    pub fn lookup(&self, chord: KeyChord) -> Option<&CommandId> {
        self.lookup_in(Scope::Everywhere, chord)
    }

    /// The command `chord` runs in `scope` itself.
    pub fn lookup_in(&self, scope: Scope, chord: KeyChord) -> Option<&CommandId> {
        self.bindings.get(&(scope, chord))
    }

    /// The command `chord` runs while `focus` is where it is: the panel's
    /// own, or else the one everywhere.
    pub fn lookup_focused(&self, chord: KeyChord, focus: Focus) -> Option<&CommandId> {
        (self.lookup_in(Scope::of(focus), chord)).or_else(|| self.lookup(chord))
    }

    /// `command`'s chords everywhere.
    pub fn chords_for<'a>(&'a self, command: &'a CommandId) -> impl Iterator<Item = KeyChord> + 'a {
        self.chords_in(command, Scope::Everywhere)
    }

    /// `command`'s chords in `scope` itself.
    pub fn chords_in<'a>(
        &'a self,
        command: &'a CommandId,
        scope: Scope,
    ) -> impl Iterator<Item = KeyChord> + 'a {
        self.bindings_for(command)
            .filter(move |&(place, _)| place == scope)
            .map(|(_, chord)| chord)
    }

    /// The chords that run `command` while `focus` is where it is: its own
    /// in the panel, and those everywhere that the panel doesn't give
    /// another command, in chord order.
    pub fn chords_focused(&self, command: &CommandId, focus: Focus) -> Vec<KeyChord> {
        let scope = Scope::of(focus);
        let everywhere = (self.chords_for(command))
            .filter(|&chord| self.lookup_in(scope, chord).is_none_or(|c| c == command));
        let mut chords: Vec<_> = self.chords_in(command, scope).chain(everywhere).collect();
        chords.sort();
        chords.dedup();
        chords
    }

    /// Where `command` has chords, and the chords: everywhere first, then
    /// panel by panel, in chord order.
    pub fn bindings_for<'a>(
        &'a self,
        command: &'a CommandId,
    ) -> impl Iterator<Item = (Scope, KeyChord)> + 'a {
        self.iter()
            .filter(move |(_, _, c)| *c == command)
            .map(|(scope, chord, _)| (scope, chord))
    }

    /// Every binding: everywhere first, then panel by panel, in chord order.
    pub fn iter(&self) -> impl Iterator<Item = (Scope, KeyChord, &CommandId)> {
        self.bindings
            .iter()
            .map(|(&(scope, chord), command)| (scope, chord, command))
    }

    /// Bindings that point at commands missing from `registry`.
    pub fn dangling<'a>(
        &'a self,
        registry: &'a Registry,
    ) -> impl Iterator<Item = (Scope, KeyChord, &'a CommandId)> {
        self.iter().filter(|(_, _, c)| registry.get(c).is_none())
    }

    /// Each command with chords, with where it has them.
    fn commands(&self) -> BTreeSet<(Scope, &CommandId)> {
        self.iter()
            .map(|(scope, _, command)| (scope, command))
            .collect()
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
#
# Lines under [panels.piano_roll], and the other panels' tables, do the same
# for the keys that work in that panel only, while it has the focus. There
# they win over the keys under [bindings].
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
///
/// [panels.piano_roll]
/// "notes.toggle_draw" = ["D"]
/// ```
///
/// Each entry gives a command the chords that run it, replacing the chords it
/// has in the base. An entry with no chords unbinds the command, and commands
/// without an entry keep their chords. A chord runs one command, so an entry
/// that gives a chord to one command takes it from the command that had it.
/// Entries under `[bindings]` work everywhere, and those under a panel's
/// table in `[panels]` work in that panel only, while it has the focus,
/// where they win over the ones everywhere.
///
/// A bad entry doesn't stop the rest of the file from loading:
/// [`parse`](Self::parse) skips it and reports a [`Problem`]. So does a chord
/// that two entries claim in one place, which then runs neither command
/// there, and a panel this version doesn't have. `format` and `version` are
/// optional, so a hand-written file can start at `[bindings]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeymapFile {
    bindings: BTreeMap<(Scope, CommandId), BTreeSet<KeyChord>>,
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
            #[serde(default)]
            panels: toml::Table,
        }

        let raw: Raw = toml::from_str(text).map_err(KeymapFileError::Toml)?;
        if let Some(format) = raw.format.filter(|f| f != KEYMAP_FORMAT) {
            return Err(KeymapFileError::NotAKeymap(format));
        }
        let version = raw.version.unwrap_or(KEYMAP_VERSION);
        if version != KEYMAP_VERSION {
            return Err(KeymapFileError::UnsupportedVersion(version));
        }

        let mut problems = Vec::new();
        let mut tables = vec![(Scope::Everywhere, &raw.bindings)];
        for (name, value) in &raw.panels {
            match (Scope::panel(name), value) {
                (Some(scope), toml::Value::Table(table)) => tables.push((scope, table)),
                (Some(_), _) => problems.push(Problem::NotBindings {
                    panel: name.clone(),
                }),
                (None, _) => problems.push(Problem::UnknownPanel {
                    panel: name.clone(),
                }),
            }
        }
        let mut file = Self::default();
        for (scope, table) in tables {
            let mut entries = Vec::new();
            flatten(table, "", &mut entries);
            for (command, value) in entries {
                match parse_chords(&command, value) {
                    Ok(chords) => file
                        .bindings
                        .entry((scope, command))
                        .or_default()
                        .extend(chords),
                    Err(problem) => problems.push(problem),
                }
            }
        }
        problems.extend(
            file.conflicts()
                .into_iter()
                .map(|((scope, chord), commands)| Problem::Conflict {
                    scope,
                    chord,
                    commands,
                }),
        );
        Ok((file, problems))
    }

    /// Reads the keymap file at `path`.
    pub fn read(path: &Path) -> Result<(Self, Vec<Problem>), KeymapFileError> {
        Self::parse(&fs::read_to_string(path).map_err(KeymapFileError::Io)?)
    }

    /// The changes that turn `base` into `keymap`: an entry for each command
    /// whose chords differ, everywhere or in a panel.
    pub fn diff(base: &Keymap, keymap: &Keymap) -> Self {
        let mut bindings = BTreeMap::new();
        for &(scope, command) in base.commands().union(&keymap.commands()) {
            let before: BTreeSet<_> = base.chords_in(command, scope).collect();
            let after: BTreeSet<_> = keymap.chords_in(command, scope).collect();
            if before != after {
                bindings.insert((scope, command.clone()), after);
            }
        }
        Self { bindings }
    }

    /// Makes the file's changes to `keymap`.
    pub fn apply_to(&self, keymap: &mut Keymap) {
        let conflicts: BTreeSet<(Scope, KeyChord)> = self.conflicts().into_keys().collect();
        for (scope, command) in self.bindings.keys() {
            keymap.unbind_command_in(command, *scope);
        }
        for &(scope, chord) in &conflicts {
            keymap.unbind_in(scope, chord);
        }
        for ((scope, command), chords) in &self.bindings {
            for &chord in chords {
                if !conflicts.contains(&(*scope, chord)) {
                    keymap.bind_in(*scope, chord, command.clone());
                }
            }
        }
    }

    /// The file as TOML, starting with a comment that explains it.
    pub fn to_toml(&self) -> String {
        type Entries<'a> = BTreeMap<&'a str, Vec<String>>;

        #[derive(Serialize)]
        struct Raw<'a> {
            format: &'a str,
            version: u32,
            bindings: Entries<'a>,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            panels: BTreeMap<&'a str, Entries<'a>>,
        }

        let mut raw = Raw {
            format: KEYMAP_FORMAT,
            version: KEYMAP_VERSION,
            bindings: BTreeMap::new(),
            panels: BTreeMap::new(),
        };
        for ((scope, command), chords) in &self.bindings {
            let entries = match scope {
                Scope::Everywhere => &mut raw.bindings,
                Scope::Panel(panel) => raw.panels.entry(panel).or_default(),
            };
            let chords = chords.iter().map(KeyChord::to_string).collect();
            entries.insert(command.as_str(), chords);
        }
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

    /// How many commands the file changes, counting each place once.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Chords that more than one entry claims in one place, with the
    /// commands claiming them.
    fn conflicts(&self) -> BTreeMap<(Scope, KeyChord), Vec<CommandId>> {
        let mut claims: BTreeMap<(Scope, KeyChord), Vec<CommandId>> = BTreeMap::new();
        for ((scope, command), chords) in &self.bindings {
            for &chord in chords {
                claims
                    .entry((*scope, chord))
                    .or_default()
                    .push(command.clone());
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
    /// Several entries claim the chord in one place, so it runs none of
    /// their commands there.
    Conflict {
        scope: Scope,
        chord: KeyChord,
        commands: Vec<CommandId>,
    },
    /// `[panels]` names a panel this version doesn't have, so its entries
    /// are skipped.
    UnknownPanel { panel: String },
    /// A panel's entry in `[panels]` isn't a table of commands, so it's
    /// skipped.
    NotBindings { panel: String },
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
            Self::Conflict {
                scope,
                chord,
                commands,
            } => {
                write!(f, "{chord} is given to ")?;
                for (i, command) in commands.iter().enumerate() {
                    match i {
                        0 => {}
                        _ if i + 1 == commands.len() => f.write_str(" and ")?,
                        _ => f.write_str(", ")?,
                    }
                    write!(f, "{command}")?;
                }
                if let Scope::Panel(panel) = scope {
                    write!(f, " in panels.{panel}")?;
                }
                f.write_str(", so it runs none of them")
            }
            Self::UnknownPanel { panel } => write!(
                f,
                "there's no panel called \"{panel}\", so the keys under panels.{panel} are \
                 left out"
            ),
            Self::NotBindings { panel } => write!(
                f,
                "panels.{panel} should give commands key chords, as [bindings] does, so it's \
                 left out"
            ),
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
        assert_eq!(
            taken,
            [(Scope::Everywhere, chord("Ctrl+L"), metronome.clone())]
        );
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
        let dangling: Vec<_> = map.dangling(&reg).map(|(.., c)| c.as_str()).collect();
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
                scope: Scope::Everywhere,
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
    fn a_panel_s_keys_win_while_it_has_the_focus() {
        let (everywhere, own) = (command("a.everywhere"), command("b.own"));
        let roll = Scope::Panel(panel::PIANO_ROLL);
        let mut map = Keymap::new();
        map.bind(chord("Up"), everywhere.clone());
        map.bind(chord("Down"), everywhere.clone());
        map.bind_in(roll, chord("Up"), own.clone());
        assert_eq!(
            map.lookup_focused(chord("Up"), Focus::PIANO_ROLL),
            Some(&own)
        );
        assert_eq!(
            map.lookup_focused(chord("Up"), Focus::ARRANGEMENT),
            Some(&everywhere)
        );
        // The piano roll keeps Down for the command everywhere.
        assert_eq!(
            map.lookup_focused(chord("Down"), Focus::PIANO_ROLL),
            Some(&everywhere)
        );
        assert_eq!(
            map.chords_focused(&everywhere, Focus::PIANO_ROLL),
            [chord("Down")]
        );
        assert_eq!(
            map.chords_focused(&everywhere, Focus::MIXER),
            [chord("Up"), chord("Down")]
        );
        assert_eq!(map.chords_focused(&own, Focus::PIANO_ROLL), [chord("Up")]);
        assert_eq!(map.chords_focused(&own, Focus::MIXER), []);
        let bindings: Vec<_> = map.bindings_for(&everywhere).collect();
        assert_eq!(
            bindings,
            [
                (Scope::Everywhere, chord("Up")),
                (Scope::Everywhere, chord("Down"))
            ]
        );
        // Unbinding everywhere leaves the panel's own.
        map.unbind(chord("Up"));
        assert_eq!(map.lookup_in(roll, chord("Up")), Some(&own));
    }

    #[test]
    fn panels_have_tables_of_their_own_in_files() {
        let (map, problems) = load(
            r#"
            [bindings]
            "transport.play" = ["P"]

            [panels.piano_roll]
            "notes.transpose_up" = ["Up"]
            "notes.transpose_down" = ["P", "Down"]
            "notes.nudge_left" = ["Down"]

            [panels.later]
            "transport.stop" = ["S"]

            [panels]
            mixer = 4
            "#,
        );
        let roll = Scope::Panel(panel::PIANO_ROLL);
        assert_eq!(
            problems,
            [
                Problem::UnknownPanel {
                    panel: "later".into()
                },
                Problem::NotBindings {
                    panel: "mixer".into()
                },
                Problem::Conflict {
                    scope: roll,
                    chord: chord("Down"),
                    commands: vec![command("notes.nudge_left"), command("notes.transpose_down")],
                },
            ]
        );
        assert_eq!(
            problems[0].to_string(),
            "there's no panel called \"later\", so the keys under panels.later are left out"
        );
        assert_eq!(
            problems[2].to_string(),
            "Down is given to notes.nudge_left and notes.transpose_down in panels.piano_roll, so \
             it runs none of them"
        );
        let up = chord("Up");
        assert_eq!(
            map.lookup_focused(up, Focus::PIANO_ROLL),
            Some(&command("notes.transpose_up"))
        );
        assert_eq!(map.lookup_focused(up, Focus::ARRANGEMENT), None);
        // P runs Transpose Down in the piano roll, and Play elsewhere.
        assert_eq!(
            map.lookup_focused(chord("P"), Focus::PIANO_ROLL),
            Some(&command("notes.transpose_down"))
        );
        assert_eq!(map.lookup(chord("P")), Some(&command("transport.play")));
        assert_eq!(map.lookup_in(roll, chord("Down")), None);
    }

    #[test]
    fn a_panel_s_changes_are_saved_under_its_table() {
        let mut map = base();
        let roll = Scope::Panel(panel::PIANO_ROLL);
        map.bind_in(roll, chord("Up"), "notes.transpose_up");
        map.bind(chord("M"), "transport.toggle_metronome");
        let file = KeymapFile::diff(&base(), &map);
        assert_eq!(file.len(), 2);
        assert_eq!(
            file.to_toml(),
            format!(
                "{HEADER}\nformat = \"musicable-keymap\"\nversion = 1\n\n[bindings]\n\
                 \"transport.toggle_metronome\" = [\"M\", \"O\"]\n\n\
                 [panels.piano_roll]\n\"notes.transpose_up\" = [\"Up\"]\n"
            )
        );
        let (read, problems) = KeymapFile::parse(&file.to_toml()).unwrap();
        assert_eq!((read, problems), (file, Vec::new()));
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
                let scopes = [
                    Scope::Everywhere,
                    Scope::Panel(panel::PIANO_ROLL),
                    Scope::Panel(panel::MIXER),
                ];
                let scope = scopes[next(scopes.len())];
                match next(3) {
                    0 => {
                        map.unbind_in(scope, chord);
                    }
                    _ => {
                        map.bind_in(scope, chord, commands[next(commands.len())]);
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

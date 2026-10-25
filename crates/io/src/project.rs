//! Musicable's own project files.
//!
//! A project file is JSON, laid out as `docs/project-format.md` describes.
//! Every file says which version of the format it uses. Loading upgrades a
//! file from any older version, and refuses one from a newer version rather
//! than lose what it can't read. Saving always writes the current version.
//!
//! Loading checks every rule of the format and says which field broke one.
//! Saving checks the same rules, so it never writes a file that won't load.
//! A project saves to the same bytes every time, so re-saving an unchanged
//! project leaves the file as it was.

mod migrate;
mod schema;

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};

use musicable_model::Project;
use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{Formatter, PrettyFormatter};

/// The file extension for project files, without the dot.
pub const EXTENSION: &str = "musicable";

/// What every project file's `format` field holds.
pub const FORMAT: &str = "musicable-project";

/// The format version this build writes. It reads every version up to this
/// one.
pub const VERSION: u32 = 15;

/// How far from the start of the song a position in a file can be, either
/// way, in ticks. It's the largest whole number a double holds exactly, so a
/// JSON reader that uses doubles, such as JavaScript's, reads every position
/// exactly. At 120 BPM it's about 440 years.
pub const MAX_TICKS: i64 = (1 << 53) - 1;

/// Loads the project file at `path`.
pub fn load(path: impl AsRef<Path>) -> Result<Project, LoadError> {
    let bytes = fs::read(path).map_err(LoadError::Io)?;
    from_slice(&bytes)
}

/// Saves `project` to `path`, replacing any file already there.
///
/// It writes a temporary file next to `path` and then renames it over the
/// old one, so a crash or a full disk never leaves a half-written project.
pub fn save(path: impl AsRef<Path>, project: &Project) -> Result<(), SaveError> {
    let text = to_string(project)?;
    write_replacing(path.as_ref(), text.as_bytes()).map_err(SaveError::Io)
}

/// Reads a project from the text of a project file.
pub fn from_str(text: &str) -> Result<Project, LoadError> {
    from_slice(text.as_bytes())
}

/// Reads a project from the bytes of a project file.
pub fn from_slice(bytes: &[u8]) -> Result<Project, LoadError> {
    // Some Windows editors start UTF-8 files with a byte order mark.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut document: Value = serde_json::from_slice(bytes).map_err(LoadError::Syntax)?;
    let version = check_header(&document)?;
    let file = if version == VERSION {
        // Straight from the text, so errors say which line they're on and a
        // field given twice is caught.
        read(bytes, Position::Keep)?
    } else {
        migrate::upgrade(&mut document, version)?;
        read_upgraded(&document)?
    };
    Ok(file.to_project()?)
}

/// Writes `project` as the text of a project file.
pub fn to_string(project: &Project) -> Result<String, SaveError> {
    schema::check_offsets(project).map_err(SaveError::Invalid)?;
    let file = schema::File::from(project);
    // Checked as loading will check it, so saving never writes a file that
    // won't load.
    file.to_project().map_err(SaveError::Invalid)?;
    let mut text = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut text, Layout::default());
    file.serialize(&mut serializer)
        .expect("a checked file always serializes");
    let mut text = String::from_utf8(text).expect("JSON is UTF-8");
    text.push('\n');
    Ok(text)
}

/// Lays files out as `serde_json`'s pretty printer does, a field or an item
/// to a line, two spaces to a level, but for arrays in arrays, such as an
/// expression curve's points, which take a line each.
struct Layout {
    pretty: PrettyFormatter<'static>,
    /// For each array and object open, whether it's an array, and whether
    /// it's all on one line.
    open: Vec<(bool, bool)>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            pretty: PrettyFormatter::with_indent(b"  "),
            open: Vec::new(),
        }
    }
}

impl Layout {
    fn inline(&self) -> bool {
        self.open.last().is_some_and(|&(_, inline)| inline)
    }

    fn begin(&mut self, array: bool) -> bool {
        let inline = self
            .open
            .last()
            .is_some_and(|&(parent, inline)| inline || (array && parent));
        self.open.push((array, inline));
        inline
    }

    fn end(&mut self) -> bool {
        self.open.pop().expect("an array or object open").1
    }
}

impl Formatter for Layout {
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.begin(true) {
            true => writer.write_all(b"["),
            false => self.pretty.begin_array(writer),
        }
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.end() {
            true => writer.write_all(b"]"),
            false => self.pretty.end_array(writer),
        }
    }

    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        match self.inline() {
            true if first => Ok(()),
            true => writer.write_all(b", "),
            false => self.pretty.begin_array_value(writer, first),
        }
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.inline() {
            true => Ok(()),
            false => self.pretty.end_array_value(writer),
        }
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.begin(false) {
            true => writer.write_all(b"{"),
            false => self.pretty.begin_object(writer),
        }
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.end() {
            true => writer.write_all(b"}"),
            false => self.pretty.end_object(writer),
        }
    }

    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        match self.inline() {
            true if first => Ok(()),
            true => writer.write_all(b", "),
            false => self.pretty.begin_object_key(writer, first),
        }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.inline() {
            true => writer.write_all(b": "),
            false => self.pretty.begin_object_value(writer),
        }
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self.inline() {
            true => Ok(()),
            false => self.pretty.end_object_value(writer),
        }
    }
}

/// Reads a file that has been upgraded from an older version.
fn read_upgraded(document: &Value) -> Result<schema::File, LoadError> {
    // Through text, not straight from the `Value`: a `Value` holds numbers as
    // f64s, and rounding to f64 and then to f32 is a bit off for a few f32s.
    let text = serde_json::to_vec(document).expect("a `Value` always serializes");
    // Lines and columns in the upgraded text would only mislead.
    read(&text, Position::Drop)
}

/// Reads a file of the current version from its text.
fn read(text: &[u8], position: Position) -> Result<schema::File, LoadError> {
    let mut text = serde_json::Deserializer::from_slice(text);
    serde_path_to_error::deserialize(&mut text).map_err(|error| {
        let path = error.path().to_string();
        // The path of the file as a whole prints as ".".
        let path = if path == "." { String::new() } else { path };
        let error = error.into_inner();
        let mut message = error.to_string();
        let at = format!(" at line {} column {}", error.line(), error.column());
        if position == Position::Drop && message.ends_with(&at) {
            message.truncate(message.len() - at.len());
        }
        Invalid::new(path, message).into()
    })
}

/// Whether an error's message keeps the line and column it happened at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Keep,
    Drop,
}

/// The file's format version, once it's clear the file is a project file
/// this build can read.
fn check_header(document: &Value) -> Result<u32, LoadError> {
    if document.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(LoadError::NotAProject);
    }
    let version = document
        .get("version")
        .and_then(Value::as_u64)
        .filter(|&version| version >= 1)
        .ok_or_else(|| Invalid::new("version", "must be a whole number, 1 or more"))?;
    match u32::try_from(version) {
        Ok(version) if version <= VERSION => Ok(version),
        _ => Err(LoadError::NewerVersion { version }),
    }
}

/// Writes `bytes` to a new file next to `path`, then renames it over `path`.
fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SAVES: AtomicU32 = AtomicU32::new(0);
    let Some(name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path doesn't name a file",
        ));
    };
    let mut temporary = OsString::from(".");
    temporary.push(name);
    let save = SAVES.fetch_add(1, Ordering::Relaxed);
    temporary.push(format!(".{}-{save}.tmp", process::id()));
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

/// Why a project file didn't load.
#[derive(Debug)]
pub enum LoadError {
    /// The file couldn't be read.
    Io(io::Error),
    /// The file isn't JSON. The error says where it went wrong.
    Syntax(serde_json::Error),
    /// The file is JSON, but not a Musicable project.
    NotAProject,
    /// A newer Musicable saved the file, in a format version this one can't
    /// read.
    NewerVersion { version: u64 },
    /// The file breaks a rule of the format.
    Invalid(Invalid),
}

/// Why a project didn't save.
#[derive(Debug)]
pub enum SaveError {
    /// The file couldn't be written. A file already at the path is left as
    /// it was.
    Io(io::Error),
    /// The project holds something the format can't, such as a gain that
    /// isn't a finite number.
    Invalid(Invalid),
}

/// A broken rule of the format, and where in the file it's broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    /// The field, such as `tracks[2].gain`. Empty for the file as a whole.
    pub path: String,
    /// What's wrong with it.
    pub message: String,
}

impl Invalid {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl From<Invalid> for LoadError {
    fn from(invalid: Invalid) -> Self {
        Self::Invalid(invalid)
    }
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl Error for Invalid {}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "couldn't read the file: {error}"),
            Self::Syntax(error) => write!(f, "the file isn't valid JSON: {error}"),
            Self::NotAProject => write!(f, "the file isn't a Musicable project"),
            Self::NewerVersion { version } => write!(
                f,
                "a newer version of Musicable saved this project, in format version {version}, \
                 and this version reads up to {VERSION}"
            ),
            Self::Invalid(invalid) => write!(f, "{invalid}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Syntax(error) => Some(error),
            Self::Invalid(invalid) => Some(invalid),
            Self::NotAProject | Self::NewerVersion { .. } => None,
        }
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "couldn't write the file: {error}"),
            Self::Invalid(invalid) => write!(f, "can't save this project: {invalid}"),
        }
    }
}

impl Error for SaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(invalid) => Some(invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::{TestTone, TrackKind};

    use super::*;

    #[test]
    fn upgraded_files_read_floats_exactly() {
        let awkward = f32::from_bits(0x15ae_43fd);
        let mut project = Project::new();
        let id = project.add_track("A", TrackKind::Audio);
        let track = project.track_mut(id).unwrap();
        track.gain = awkward;
        track.tone = Some(TestTone {
            frequency: 440.0,
            level: -awkward,
        });
        let document: Value = serde_json::from_str(&to_string(&project).unwrap()).unwrap();
        let through_f64: f32 =
            serde_json::from_value(document["tracks"][0]["gain"].clone()).unwrap();
        assert_ne!(
            through_f64.to_bits(),
            awkward.to_bits(),
            "serde_json now reads this f32 from a `Value` exactly, so `read_upgraded` could \
             read straight from the `Value`"
        );
        let upgraded = read_upgraded(&document).unwrap().to_project().unwrap();
        assert_eq!(upgraded.tracks[0].gain.to_bits(), awkward.to_bits());
        let level = upgraded.tracks[0].tone.unwrap().level;
        assert_eq!(level.to_bits(), (-awkward).to_bits());
    }

    #[test]
    fn errors_in_upgraded_files_have_no_line_numbers() {
        let mut document: Value =
            serde_json::from_str(&to_string(&Project::new()).unwrap()).unwrap();
        document["looping"] = "yes".into();
        let Err(LoadError::Invalid(invalid)) = read_upgraded(&document) else {
            panic!("a string isn't a boolean");
        };
        assert_eq!(
            invalid,
            Invalid::new(
                "looping",
                "invalid type: string \"yes\", expected a boolean"
            )
        );
    }
}

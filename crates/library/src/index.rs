//! The library's index: a record for each audio file in the folders the user
//! added, its places, kept in a file in the config folder so the browser has
//! them at once when Musicable starts.
//!
//! A record holds what the file's name and header say, which is all the
//! library fills in so far, and has room for what analysis will add: tags
//! with where each came from and how sure it is, measured values, an
//! embedding for finding similar sounds, and the version of the analyser
//! that made them, so a newer one can make them again. The user's own tags
//! go in it too. Fields the library doesn't fill in aren't written, and
//! fields this version doesn't know are ignored, so an index from a newer
//! version still opens.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// What the index file says it is.
const FORMAT: &str = "musicable-library";
/// The index file's version. A file of an older version is read as far as
/// it goes, and of a newer one too: see the module's docs.
const VERSION: u32 = 1;

/// The extensions of the files the library indexes: those Musicable reads.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "flac"];

/// Whether the file called `name` is one the library indexes, by its
/// extension.
pub fn is_audio_file(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    AUDIO_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(extension))
}

/// What the library knows about one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Where the file is in its place: from the place's folder, with `/`
    /// between names.
    pub path: String,
    /// Its size in bytes, and when it last changed, in milliseconds since
    /// 1970 began: when either differs, the file is read again.
    pub size: u64,
    pub modified: u64,
    /// What its header says, or `None` if it couldn't be read as audio.
    pub audio: Option<AudioFacts>,
    /// Its tags, the user's and, later, analysis's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    /// What analysis measured: nothing yet.
    #[serde(default, skip_serializing_if = "Measured::is_empty")]
    pub measured: Measured,
    /// Where analysis's model places the sound, for finding similar ones:
    /// nothing yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// The analyser that made the measurements, the model's tags and the
    /// embedding, such as `"ced-tiny 1"`: nothing yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyser: Option<String>,
}

impl Record {
    /// A record of a file, with nothing analysed and no tags.
    pub fn new(
        path: impl Into<String>,
        size: u64,
        modified: u64,
        audio: Option<AudioFacts>,
    ) -> Self {
        Self {
            path: path.into(),
            size,
            modified,
            audio,
            tags: Vec::new(),
            measured: Measured::default(),
            embedding: None,
            analyser: None,
        }
    }

    /// The file's name, extension and all.
    pub fn name(&self) -> &str {
        self.path
            .rsplit_once('/')
            .map_or(&self.path, |(_, name)| name)
    }

    /// The file's name without its extension, as the browser shows it.
    pub fn stem(&self) -> &str {
        let name = self.name();
        name.rsplit_once('.').map_or(name, |(stem, _)| stem)
    }

    /// The folder it's in, from its place's folder: empty if it's in that
    /// folder itself.
    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(folder, _)| folder)
    }

    /// Whether it has the tag `id`, from anyone.
    pub fn has_tag(&self, id: &str) -> bool {
        self.tags.iter().any(|tag| tag.id == id)
    }
}

/// What an audio file's header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFacts {
    pub format: Format,
    pub sample_rate: u32,
    pub channels: u16,
    /// The file's length, in frames.
    pub frames: u64,
}

impl AudioFacts {
    /// How long the file plays, in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }
}

impl From<musicable_io::audio::Info> for AudioFacts {
    fn from(info: musicable_io::audio::Info) -> Self {
        use musicable_io::audio::Format as Io;
        Self {
            format: match info.format {
                Io::Wav => Format::Wav,
                Io::Aiff => Format::Aiff,
                Io::Flac => Format::Flac,
            },
            sample_rate: info.sample_rate,
            channels: u16::try_from(info.channels).unwrap_or(u16::MAX),
            frames: info.frames,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Wav,
    Aiff,
    Flac,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wav => "WAV",
            Self::Aiff => "AIFF",
            Self::Flac => "FLAC",
        }
    }
}

/// A tag on a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    /// Its id in the tag vocabulary, which never changes once it's out:
    /// `instrument/drums/kick`, say. The user's own words go in as they
    /// type them.
    pub id: String,
    /// Who gave it.
    pub source: TagSource,
    /// How sure its source is, from 0 to 1: for tags a model suggests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

impl Tag {
    /// A tag the user gave.
    pub fn yours(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source: TagSource::You,
            score: None,
        }
    }
}

/// Who gave a tag, in the order the browser trusts them: analysis never
/// changes the user's tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagSource {
    /// The user.
    You,
    /// Words in the file's name and folders, matched against the
    /// vocabulary.
    Name,
    /// A measurement, such as its length saying it's a one-shot.
    Measured,
    /// A model that heard it, with a score.
    Model,
}

/// What analysis measures of a sound. Analysis isn't built yet, so each is
/// `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Measured {
    /// Integrated loudness, in LUFS (ITU-R BS.1770).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loudness: Option<f32>,
    /// True peak, in dBTP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak: Option<f32>,
    /// Whether it's a one-shot rather than a loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_shot: Option<bool>,
    /// A loop's tempo, in beats per minute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo: Option<f64>,
    /// A one-shot's root note, as a MIDI note number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<u8>,
    /// A loop's key, such as `"A minor"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The spectral centroid, in hertz: how bright it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<f32>,
}

impl Measured {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A folder the user added, and the audio files in it and in the folders
/// under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    folder: PathBuf,
    /// In order of path. Shared, so a copy of the index is quick to make,
    /// as for saving it on another thread.
    files: Arc<Vec<Record>>,
    /// Whether it has been scanned since Musicable started, so its files
    /// are as they are now.
    scanned: bool,
    /// The index's generation when its files last changed: no two places
    /// have the same.
    generation: u64,
    /// How many of the files have tags.
    tagged: usize,
}

impl Place {
    fn new(folder: PathBuf, generation: u64) -> Self {
        Self::with_files(folder, Vec::new(), generation)
    }

    /// A place with `files`, in order of path, with no path twice.
    fn with_files(folder: PathBuf, mut files: Vec<Record>, generation: u64) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        let tagged = files.iter().filter(|r| !r.tags.is_empty()).count();
        Self {
            folder,
            files: Arc::new(files),
            scanned: false,
            generation,
            tagged,
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The folder's own name, as the browser shows the place.
    pub fn name(&self) -> String {
        match self.folder.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.folder.to_string_lossy().into_owned(),
        }
    }

    /// The files, in order of path.
    pub fn files(&self) -> &[Record] {
        &self.files
    }

    /// The files, shared: for a scan on another thread, which keeps what
    /// hasn't changed.
    pub fn shared_files(&self) -> Arc<Vec<Record>> {
        self.files.clone()
    }

    pub fn scanned(&self) -> bool {
        self.scanned
    }

    /// Whether any of the files have tags.
    pub fn has_tags(&self) -> bool {
        self.tagged > 0
    }

    /// Changes when its files do, though not when their tags do, for what's
    /// worked out from their paths. No two places have the same.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The file at `path`, from the place's folder with `/` between names,
    /// by its index in [`files`](Self::files).
    pub fn find(&self, path: &str) -> Option<usize> {
        self.files
            .binary_search_by(|record| record.path.as_str().cmp(path))
            .ok()
    }

    /// Where the file `record` is on disk.
    pub fn path_of(&self, record: &Record) -> PathBuf {
        let mut path = self.folder.clone();
        path.extend(record.path.split('/'));
        path
    }
}

/// The places, and their files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    places: Vec<Place>,
    /// Counts the changes, so what's worked out from the index, such as
    /// search results, knows when to work it out again.
    generation: u64,
}

/// Where a file is in the index: its place and its index in the place's
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId {
    pub place: usize,
    pub file: usize,
}

impl Index {
    pub const fn new() -> Self {
        Self {
            places: Vec::new(),
            generation: 0,
        }
    }

    /// The index in the file at `path`. A missing file is an empty index.
    pub fn read(path: &Path) -> Result<Self, IndexError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(IndexError::Io(error)),
        };
        let file: File = serde_json::from_str(&text).map_err(IndexError::Invalid)?;
        if file.format != FORMAT {
            return Err(IndexError::NotAnIndex);
        }
        let mut index = Self::new();
        for place in file.places {
            index.generation += 1;
            let place = Place::with_files(place.folder, place.files, index.generation);
            index.places.push(place);
        }
        Ok(index)
    }

    /// Writes the index to the file at `path`, whole or not at all: it goes
    /// to a file next to it first, which then takes its place.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let file = FileRef {
            format: FORMAT,
            version: VERSION,
            places: self
                .places
                .iter()
                .map(|place| PlaceRef {
                    folder: &place.folder,
                    files: &place.files,
                })
                .collect(),
        };
        let json = serde_json::to_vec(&file).map_err(io::Error::other)?;
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        fs::write(&part, json)?;
        fs::rename(&part, path)
    }

    pub fn places(&self) -> &[Place] {
        &self.places
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The place whose folder is `folder`, by its index.
    pub fn place(&self, folder: &Path) -> Option<usize> {
        self.places.iter().position(|place| place.folder == folder)
    }

    /// Adds a place for `folder`, with no files until it's scanned, and
    /// returns its index. A folder that's a place already keeps it.
    pub fn add_place(&mut self, folder: PathBuf) -> usize {
        if let Some(index) = self.place(&folder) {
            return index;
        }
        self.generation += 1;
        self.places.push(Place::new(folder, self.generation));
        self.places.len() - 1
    }

    /// Removes the place for `folder`, and its files. Returns whether there
    /// was one.
    pub fn remove_place(&mut self, folder: &Path) -> bool {
        let before = self.places.len();
        self.places.retain(|place| place.folder != folder);
        let removed = self.places.len() != before;
        if removed {
            self.generation += 1;
        }
        removed
    }

    /// Keeps only the places whose folders are in `folders`, adds the ones
    /// missing, and puts them in that order.
    pub fn set_places(&mut self, folders: &[PathBuf]) {
        let mut old = std::mem::take(&mut self.places);
        self.generation += 1;
        for folder in folders {
            if self.places.iter().any(|place| &place.folder == folder) {
                continue;
            }
            let place = match old.iter().position(|place| &place.folder == folder) {
                Some(index) => old.swap_remove(index),
                None => {
                    self.generation += 1;
                    Place::new(folder.clone(), self.generation)
                }
            };
            self.places.push(place);
        }
    }

    /// Replaces the files of the place for `folder` with `files`, which a
    /// scan found, in order of path. Returns whether there's such a place.
    pub fn set_files(&mut self, folder: &Path, files: Vec<Record>) -> bool {
        let Some(index) = self.place(folder) else {
            return false;
        };
        self.generation += 1;
        let place = &mut self.places[index];
        *place = Place::with_files(folder.to_owned(), files, self.generation);
        place.scanned = true;
        true
    }

    /// The file at `id`.
    pub fn get(&self, id: FileId) -> Option<&Record> {
        self.places.get(id.place)?.files.get(id.file)
    }

    /// Where the file at `id` is on disk.
    pub fn path_of(&self, id: FileId) -> Option<PathBuf> {
        let place = self.places.get(id.place)?;
        Some(place.path_of(place.files.get(id.file)?))
    }

    /// The file at `path` on disk, if it's in a place.
    pub fn find(&self, path: &Path) -> Option<FileId> {
        self.places.iter().enumerate().find_map(|(index, place)| {
            let relative = path.strip_prefix(&place.folder).ok()?;
            let names: Option<Vec<&str>> = relative.iter().map(|name| name.to_str()).collect();
            let file = place.find(&names?.join("/"))?;
            Some(FileId { place: index, file })
        })
    }

    /// Gives the file at `id` the user's tag `tag`, or with `on` false takes
    /// whatever tag `tag` it has away. Returns whether that changed it.
    pub fn tag(&mut self, id: FileId, tag: &str, on: bool) -> bool {
        let Some(place) = self.places.get_mut(id.place) else {
            return false;
        };
        let Some(record) = place.files.get(id.file) else {
            return false;
        };
        if record.has_tag(tag) == on {
            return false;
        }
        let had = !record.tags.is_empty();
        // Copies the files only if a copy of the index still has them.
        let record = &mut Arc::make_mut(&mut place.files)[id.file];
        match on {
            true => record.tags.push(Tag::yours(tag)),
            false => record.tags.retain(|t| t.id != tag),
        }
        let has = !record.tags.is_empty();
        place.tagged = place.tagged + usize::from(has) - usize::from(had);
        self.generation += 1;
        true
    }

    /// How many files the places hold.
    pub fn len(&self) -> usize {
        self.places.iter().map(|place| place.files.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why the index file couldn't be read.
#[derive(Debug)]
pub enum IndexError {
    Io(io::Error),
    Invalid(serde_json::Error),
    /// The file is something else.
    NotAnIndex,
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "couldn't read the library's index: {error}"),
            Self::Invalid(error) => write!(f, "the library's index is damaged: {error}"),
            Self::NotAnIndex => f.write_str("the library's index file is something else"),
        }
    }
}

impl std::error::Error for IndexError {}

/// The index file, as it's read.
#[derive(Deserialize)]
struct File {
    format: String,
    #[serde(rename = "version")]
    _version: u32,
    #[serde(default)]
    places: Vec<PlaceFile>,
}

#[derive(Deserialize)]
struct PlaceFile {
    folder: PathBuf,
    #[serde(default)]
    files: Vec<Record>,
}

/// The index file, as it's written.
#[derive(Serialize)]
struct FileRef<'a> {
    format: &'static str,
    version: u32,
    places: Vec<PlaceRef<'a>>,
}

#[derive(Serialize)]
struct PlaceRef<'a> {
    folder: &'a Path,
    files: &'a [Record],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(frames: u64) -> Option<AudioFacts> {
        Some(AudioFacts {
            format: Format::Wav,
            sample_rate: 48_000,
            channels: 2,
            frames,
        })
    }

    #[test]
    fn records_say_their_names_and_folders() {
        let record = Record::new("Drums/Kicks/Kick 808.wav", 1, 2, facts(48_000));
        assert_eq!(record.name(), "Kick 808.wav");
        assert_eq!(record.stem(), "Kick 808");
        assert_eq!(record.folder(), "Drums/Kicks");
        assert_eq!(record.audio.unwrap().seconds(), 1.0);
        let top = Record::new("Clap", 1, 2, None);
        assert_eq!((top.name(), top.stem(), top.folder()), ("Clap", "Clap", ""));
        assert!(is_audio_file("a.WAV") && is_audio_file("b.aif") && is_audio_file("c.flac"));
        assert!(!is_audio_file("wav") && !is_audio_file("d.mp3") && !is_audio_file("e.wav.txt"));
    }

    #[test]
    fn the_index_file_keeps_records_and_tags() {
        let folder = std::env::temp_dir().join(format!("musicable-index-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        let path = folder.join("library.json");
        assert_eq!(Index::read(&path).unwrap(), Index::new());

        let mut index = Index::new();
        let drums = index.add_place(folder.join("Drums"));
        index.add_place(folder.join("Loops"));
        assert_eq!(index.add_place(folder.join("Drums")), drums);
        let mut snare = Record::new("Snare.aif", 10, 20, facts(100));
        snare.measured.tempo = Some(120.0);
        let files = vec![
            Record::new("Kick.wav", 1, 2, facts(48_000)),
            snare,
            Record::new("Broken.wav", 3, 4, None),
        ];
        assert!(index.set_files(&folder.join("Drums"), files));
        let kick = index.find(&folder.join("Drums").join("Kick.wav")).unwrap();
        assert!(index.tag(kick, "instrument/drums/kick", true));
        assert!(!index.tag(kick, "instrument/drums/kick", true));
        index.write(&path).unwrap();
        assert!(!folder.join("library.json.part").exists());

        let read = Index::read(&path).unwrap();
        let place = &read.places()[0];
        let paths: Vec<&str> = place.files().iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["Broken.wav", "Kick.wav", "Snare.aif"]);
        assert_eq!(place.files()[1].tags, [Tag::yours("instrument/drums/kick")]);
        assert_eq!(place.files()[2].measured.tempo, Some(120.0));
        assert_eq!(read.places()[1].files(), []);
        // Only what's there is written.
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("embedding") && !text.contains("loudness"),
            "{text}"
        );
        assert!(text.starts_with(r#"{"format":"musicable-library","version":1,"#));

        // Fields from a newer version are skipped.
        let newer = text.replace(r#""size":1,"#, r#""size":1,"colour":"red","#);
        fs::write(&path, newer).unwrap();
        assert_eq!(Index::read(&path).unwrap().len(), 3);
        fs::write(&path, r#"{"format":"musicable-project","version":1}"#).unwrap();
        assert!(matches!(Index::read(&path), Err(IndexError::NotAnIndex)));
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn places_follow_the_folders_given() {
        let mut index = Index::new();
        let (a, b, c) = (
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        );
        index.add_place(a.clone());
        index.add_place(b.clone());
        index.set_files(&b, vec![Record::new("x.wav", 1, 1, None)]);
        let generation = index.generation();
        index.set_places(&[c.clone(), b.clone(), c.clone()]);
        let folders: Vec<&Path> = index.places().iter().map(Place::folder).collect();
        assert_eq!(folders, [c.as_path(), b.as_path()]);
        assert_eq!(index.places()[1].files().len(), 1);
        assert!(index.generation() > generation);
        assert!(index.remove_place(&b) && !index.remove_place(&b));
        assert_eq!(index.len(), 0);
    }
}

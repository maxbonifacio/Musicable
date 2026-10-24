//! The browser's library in the app: the folders of samples the user added,
//! indexed in `library.json` in the config folder, the presets, and the
//! previews. The file work happens on the background queue: reading the
//! index file, scanning folders, reading the headers of the files the
//! scans find, finding presets, opening the files to preview and drawing
//! their waveforms, and saving the index.
//!
//! Commands reach the library through [`LibraryLink`], which implements
//! [`musicable_commands::Library`]. `docs/app.md` describes the browser.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use musicable_engine::media::{AudioData, AudioReader, Source, Streamer};
use musicable_io::audio::Decoder;
use musicable_library::presets::{self, Loaded};
use musicable_library::scan::{header, keep_tags};
use musicable_library::{
    Adder, AudioFacts, Busy, FileId, Index, IndexError, Prepared, Preset, Priority, Query, Queue,
    Record, Searcher, scan,
};
use musicable_model::plugin::{PluginDevice, PluginSettings, PluginState};
use musicable_model::{DeviceId, DeviceKind, PluginFormat};
use musicable_plugins::Waker;
use musicable_ui::Peaks;

use crate::audio_files::{clip_path, waveform};
use crate::media::FileReader;
use crate::plugins::Plugins;

/// The index file's name in the config folder.
pub const LIBRARY_FILE: &str = "library.json";
/// How many files a search or [`FIND_SIMILAR`](musicable_commands::browser::FIND_SIMILAR)
/// lists in the browser, at most.
pub const LISTED: usize = 1000;
/// Files up to this many frames long are read whole to preview them: about
/// twelve seconds at 44.1 kHz. Longer ones stream from disk.
const PREVIEW_WHOLE: u64 = 1 << 19;
/// How long the index waits after a change before it's saved, so a run of
/// changes, such as tagging files one after another, saves it once.
const SAVE_AFTER: Duration = Duration::from_secs(2);
/// How many files' headers a job reads before the jobs waiting have their
/// turn: about half a second's work where opening a file takes 7 ms, as on
/// Windows while the antivirus looks at each.
const READ_AT_ONCE: usize = 64;

/// What the headers of files of a place say, by their paths from its
/// folder: `None` for a file that isn't audio Musicable reads.
type Headers = Vec<(String, Option<AudioFacts>)>;

/// The browser's library.
pub struct Library {
    /// The index file. `None` keeps the library in memory only, as for
    /// tests.
    file: Option<PathBuf>,
    /// Where Musicable's own presets go: a folder for each device.
    presets_folder: Option<PathBuf>,
    index: Index,
    searcher: Searcher,
    /// Whether the index file has been read: until then the library is
    /// empty, and nothing is scanned.
    loaded: bool,
    /// `None` for a library that does nothing: one for a session that
    /// exports, or for tests that don't need one.
    queue: Option<Arc<Queue>>,
    /// What the queue's jobs send back.
    done: Sender<Done>,
    received: Receiver<Done>,
    waker: Option<Waker>,
    /// The scans running, by folder.
    scans: BTreeMap<PathBuf, Scan>,
    /// The places whose files' headers are being read, by folder.
    readings: BTreeMap<PathBuf, Reading>,
    /// The id the next scan or reading gets.
    next_scan: u64,
    presets: Vec<Preset>,
    /// Whether the presets are being looked for.
    finding_presets: bool,
    /// How many times the presets have been looked for. Only the latest
    /// look's are kept, so one that finishes after it can't hide a preset
    /// saved in between.
    presets_looks: u64,
    /// What the browser lists, and what that found.
    listing: Listing,
    found: Vec<FileId>,
    total: usize,
    preview: Option<Previewing>,
    /// The id the next preview gets.
    next_preview: u64,
    /// What the engine is to do with previews, oldest first.
    for_engine: Vec<ToEngine>,
    /// Streams the previews of long files. Started with the first.
    streamer: OnceLock<Arc<Streamer>>,
    /// When the index first changed since it was saved, if it has.
    changed: Option<Instant>,
    saving: bool,
    /// Headers read while the index was being saved, which wait for the
    /// save, as taking them in would copy the place's files.
    read_while_saving: Vec<(PathBuf, u64, Headers)>,
    /// Whether headers were read since the index was last saved. They're
    /// saved once a place's are all read, and when the app closes, rather
    /// than after every few, as a large library's file takes a while to
    /// write.
    read_unsaved: bool,
    busy: Busy,
    /// What went wrong since the last [`take_problems`](Self::take_problems).
    problems: Vec<String>,
}

/// A scan under way: which it is, how many files it has found, whether to
/// stop it, and the place's files when it started.
struct Scan {
    id: u64,
    found: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    old: Arc<Vec<Record>>,
}

/// A reading of the headers of a place's files under way: which it is, how
/// many are left to read, and whether to stop it.
struct Reading {
    id: u64,
    left: usize,
    cancel: Arc<AtomicBool>,
}

/// What the browser lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// What a search found: nothing for an empty one.
    Search(String),
    /// The files most like this one.
    Similar(PathBuf),
}

/// The file previewing, or opening to preview.
#[derive(Debug)]
pub struct Previewing {
    pub id: u64,
    pub path: PathBuf,
    /// How long it is, in its own frames, once it's open.
    pub frames: Option<u64>,
    /// Its waveform, once it's drawn.
    pub peaks: Option<Arc<Peaks>>,
}

/// What the session is to tell the engine about previews.
pub enum ToEngine {
    /// Play `source` in place of what's previewing, as preview `id`.
    Play {
        id: u64,
        source: Source,
    },
    Stop,
}

/// What a job of the queue found.
enum Done {
    Loaded {
        index: Result<Index, IndexError>,
        prepared: Vec<Prepared>,
    },
    Scanned {
        folder: PathBuf,
        id: u64,
        scanned: io::Result<Option<(Vec<Record>, Prepared)>>,
    },
    /// What the headers of files of the place for `folder` say.
    Read {
        folder: PathBuf,
        id: u64,
        read: Headers,
    },
    Presets {
        look: u64,
        found: Vec<Preset>,
    },
    Opened {
        id: u64,
        opened: Result<(Source, u64), String>,
    },
    Drawn {
        id: u64,
        peaks: Arc<Peaks>,
    },
    Saved(io::Result<()>),
}

impl Library {
    /// A library that has nothing and does nothing, as a session that
    /// exports has.
    pub fn none() -> Self {
        let (done, received) = mpsc::channel();
        Self {
            file: None,
            presets_folder: None,
            index: Index::new(),
            searcher: Searcher::new(),
            loaded: true,
            queue: None,
            done,
            received,
            waker: None,
            scans: BTreeMap::new(),
            readings: BTreeMap::new(),
            next_scan: 1,
            presets: Vec::new(),
            finding_presets: false,
            presets_looks: 0,
            listing: Listing::Search(String::new()),
            found: Vec::new(),
            total: 0,
            preview: None,
            next_preview: 1,
            for_engine: Vec::new(),
            streamer: OnceLock::new(),
            changed: None,
            saving: false,
            read_while_saving: Vec::new(),
            read_unsaved: false,
            busy: Busy::Idle,
            problems: Vec::new(),
        }
    }

    /// The library in the index file `file`, which the queue reads. Once
    /// it's read, every folder is scanned again, and the presets in
    /// `presets_folder` and VST3's folders are found. Without a file, the
    /// library starts empty and stays in memory. Jobs that finish call
    /// `waker`, from the queue's threads, so the app takes in what they
    /// found.
    pub fn open(
        file: Option<PathBuf>,
        presets_folder: Option<PathBuf>,
        queue: Arc<Queue>,
        waker: Option<Waker>,
    ) -> Self {
        let mut library = Self::none();
        library.file = file.clone();
        library.presets_folder = presets_folder;
        library.loaded = false;
        library.queue = Some(queue);
        library.waker = waker;
        let done = library.done.clone();
        library.run(Priority::Now, move || {
            let index = match &file {
                Some(file) => Index::read(file),
                None => Ok(Index::new()),
            };
            let prepared = match &index {
                Ok(index) => index
                    .places()
                    .iter()
                    .map(|place| Searcher::prepare(place.files()))
                    .collect(),
                Err(_) => Vec::new(),
            };
            let _ = done.send(Done::Loaded { index, prepared });
        });
        library
    }

    /// Runs `job` on the queue, and wakes the app when it's done.
    fn run(&self, priority: Priority, job: impl FnOnce() + Send + 'static) {
        let Some(queue) = &self.queue else {
            return;
        };
        let waker = self.waker.clone();
        queue.add(priority, move || {
            job();
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    /// Takes in what the queue's jobs found, and saves the index when it's
    /// due. Returns whether anything changed.
    pub fn tick(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.received.try_recv() {
            any = true;
            match done {
                Done::Loaded { index, prepared } => self.loaded(index, prepared),
                Done::Scanned {
                    folder,
                    id,
                    scanned,
                } => self.scanned(&folder, id, scanned),
                Done::Read { folder, id, read } => self.headers_read(folder, id, read),
                Done::Presets { look, found } => {
                    if look == self.presets_looks {
                        self.presets = found;
                        self.finding_presets = false;
                    }
                }
                Done::Opened { id, opened } => self.opened(id, opened),
                Done::Drawn { id, peaks } => {
                    if let Some(preview) = self.preview.as_mut().filter(|p| p.id == id) {
                        preview.peaks = Some(peaks);
                    }
                }
                Done::Saved(saved) => {
                    self.saving = false;
                    if let Err(error) = saved {
                        self.problems
                            .push(format!("Couldn't save the browser's library: {error}."));
                    }
                    for (folder, id, read) in std::mem::take(&mut self.read_while_saving) {
                        self.headers_read(folder, id, read);
                    }
                }
            }
        }
        let due = self.changed.is_some_and(|at| at.elapsed() >= SAVE_AFTER);
        if due && !self.saving {
            self.save();
        }
        any
    }

    fn loaded(&mut self, index: Result<Index, IndexError>, prepared: Vec<Prepared>) {
        self.loaded = true;
        match index {
            Ok(index) => {
                // Folders added before the file was read stay.
                let added: Vec<PathBuf> = self
                    .index
                    .places()
                    .iter()
                    .map(|place| place.folder().to_owned())
                    .collect();
                self.index = index;
                for (place, prepared) in prepared.into_iter().enumerate() {
                    self.searcher.put(&self.index, place, prepared);
                }
                for folder in added {
                    self.index.add_place(folder);
                }
            }
            Err(error) => self.broken(&error),
        }
        self.searcher.update(&self.index);
        self.rescan_all();
        self.relist();
    }

    /// Keeps the index file that couldn't be read beside the new one, so
    /// the tags in it aren't lost.
    fn broken(&mut self, error: &IndexError) {
        let Some(file) = &self.file else {
            return;
        };
        let mut kept = file.as_os_str().to_owned();
        kept.push(".broken");
        let kept = PathBuf::from(kept);
        let problem = match fs::rename(file, &kept) {
            Ok(()) => format!(
                "Couldn't read the browser's library, {}: {error}. It's kept as {}, and the \
                 library starts again.",
                file.display(),
                kept.display()
            ),
            Err(_) => format!(
                "Couldn't read the browser's library, {}: {error}.",
                file.display()
            ),
        };
        self.problems.push(problem);
    }

    fn scanned(
        &mut self,
        folder: &Path,
        id: u64,
        scanned: io::Result<Option<(Vec<Record>, Prepared)>>,
    ) {
        // A scan of a folder taken out of the library meanwhile, or scanned
        // again, has nothing to add, and the scan after it runs on.
        if self.scans.get(folder).is_none_or(|scan| scan.id != id) {
            return;
        }
        let Some(scan) = self.scans.remove(folder) else {
            return;
        };
        match scanned {
            Ok(Some((mut records, prepared))) => {
                let Some(place) = self.index.place(folder) else {
                    // Taken out of the library meanwhile.
                    return;
                };
                // The scan went by the files as they were when it started,
                // so tags given or taken away since stay so.
                let now = self.index.places()[place].shared_files();
                if !Arc::ptr_eq(&now, &scan.old) {
                    keep_tags(&mut records, &now);
                }
                self.index.set_files(folder, records);
                self.searcher.put(&self.index, place, prepared);
                self.changed();
                self.relist();
                self.read(folder);
            }
            // Only scans overtaken stop, and they return above.
            Ok(None) => {}
            Err(error) => {
                if self.index.place(folder).is_some() {
                    self.problems.push(format!(
                        "Couldn't look through {}: {error}. The browser lists the files it \
                         had before.",
                        folder.display()
                    ));
                }
            }
        }
    }

    fn opened(&mut self, id: u64, opened: Result<(Source, u64), String>) {
        let Some(preview) = self.preview.as_mut().filter(|p| p.id == id) else {
            return;
        };
        match opened {
            Ok((source, frames)) => {
                preview.frames = Some(frames);
                self.for_engine.push(ToEngine::Play { id, source });
            }
            Err(error) => {
                self.problems.push(format!(
                    "Couldn't preview {}: {error}.",
                    preview.path.display()
                ));
                self.preview = None;
            }
        }
    }

    /// Notes that the index changed, so it's saved soon.
    fn changed(&mut self) {
        self.changed.get_or_insert_with(Instant::now);
    }

    /// Saves the index on the queue: a copy of it, which shares the files.
    fn save(&mut self) {
        self.changed = None;
        self.read_unsaved = false;
        let Some(file) = self.file.clone() else {
            return;
        };
        self.saving = true;
        let index = self.index.clone();
        let done = self.done.clone();
        self.run(Priority::Later, move || {
            let _ = done.send(Done::Saved(index.write(&file)));
        });
    }

    /// Scans every folder again, and finds the presets.
    fn rescan_all(&mut self) {
        let folders: Vec<PathBuf> = self
            .index
            .places()
            .iter()
            .map(|place| place.folder().to_owned())
            .collect();
        for folder in folders {
            self.scan(&folder);
        }
        self.find_presets();
    }

    /// Scans `folder`, a place, again, stopping a scan of it under way.
    fn scan(&mut self, folder: &Path) {
        let Some(place) = self.index.place(folder) else {
            return;
        };
        let Some(queue) = &self.queue else {
            return;
        };
        // Slower while the engine records.
        let pace = queue.pace();
        if let Some(scan) = self.scans.remove(folder) {
            scan.cancel.store(true, Relaxed);
        }
        // The headers read so far are in the files the scan goes by, and
        // the reading after it reads the rest.
        if let Some(reading) = self.readings.remove(folder) {
            reading.cancel.store(true, Relaxed);
        }
        let old = self.index.places()[place].shared_files();
        let id = self.next_scan;
        self.next_scan += 1;
        let found = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Scan {
            id,
            found: found.clone(),
            cancel: cancel.clone(),
            old: old.clone(),
        };
        self.scans.insert(folder.to_owned(), running);
        let folder = folder.to_owned();
        let done = self.done.clone();
        let waker = self.waker.clone();
        self.run(Priority::Later, move || {
            let wanted = || {
                pace.step();
                !cancel.load(Relaxed)
            };
            let mut report = |files| {
                found.store(files, Relaxed);
                // So the browser shows how far it got.
                if let Some(waker) = &waker {
                    waker();
                }
            };
            let scanned = scan(&folder, &old, &wanted, &mut report).map(|records| {
                records.map(|records| {
                    let prepared = Searcher::prepare(&records);
                    (records, prepared)
                })
            });
            let _ = done.send(Done::Scanned {
                folder,
                id,
                scanned,
            });
        });
    }

    /// Reads the headers of the files of the place for `folder` that its
    /// scan left to read, a few at a time in jobs on the queue, each adding
    /// the next as it finishes, so previews and waveforms go between them.
    fn read(&mut self, folder: &Path) {
        let (Some(place), Some(queue)) = (self.index.place(folder), &self.queue) else {
            return;
        };
        let files = self.index.places()[place].files().iter();
        let paths: Vec<String> = files
            .filter(|record| record.unread)
            .map(|record| record.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let id = self.next_scan;
        self.next_scan += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let reading = Reading {
            id,
            left: paths.len(),
            cancel: cancel.clone(),
        };
        if let Some(reading) = self.readings.insert(folder.to_owned(), reading) {
            reading.cancel.store(true, Relaxed);
        }
        let unread = Arc::new(Unread {
            folder: folder.to_owned(),
            id,
            paths,
            next: AtomicUsize::new(0),
            cancel,
            adder: queue.adder(),
            done: self.done.clone(),
            waker: self.waker.clone(),
        });
        // A job for each thread, of which the queue runs as many as it
        // allows.
        for _ in 0..queue.threads() {
            let unread = unread.clone();
            queue.add(Priority::Later, move || read_some(&unread));
        }
    }

    /// Takes in the headers a job read of files of the place for `folder`.
    fn headers_read(&mut self, folder: PathBuf, id: u64, read: Headers) {
        // Those of a reading stopped meanwhile, by another scan or the
        // folder being taken out, are left.
        if self
            .readings
            .get(&folder)
            .is_none_or(|reading| reading.id != id)
        {
            return;
        }
        if self.saving {
            self.read_while_saving.push((folder, id, read));
            return;
        }
        self.index.set_audio(&folder, &read);
        self.read_unsaved = true;
        let Some(reading) = self.readings.get_mut(&folder) else {
            return;
        };
        reading.left = reading.left.saturating_sub(read.len());
        if reading.left == 0 {
            self.readings.remove(&folder);
            self.changed();
        }
    }

    /// Finds the presets in Musicable's folder for them and VST3's.
    fn find_presets(&mut self) {
        let Some(queue) = &self.queue else {
            return;
        };
        let pace = queue.pace();
        self.finding_presets = true;
        self.presets_looks += 1;
        let look = self.presets_looks;
        let mut folders: Vec<PathBuf> = self.presets_folder.iter().cloned().collect();
        folders.extend(presets::vst3_folders());
        let done = self.done.clone();
        self.run(Priority::Later, move || {
            let wanted = || {
                pace.step();
                true
            };
            let found = presets::find(&folders, &wanted).unwrap_or_default();
            let _ = done.send(Done::Presets { look, found });
        });
    }

    /// Works out again what the browser lists, as the index changed.
    fn relist(&mut self) {
        match self.listing.clone() {
            Listing::Search(text) => {
                self.search(&text, 0);
            }
            Listing::Similar(file) => {
                // The file may have gone.
                if self.find_similar(&file, 0).is_err() {
                    self.search("", 0);
                }
            }
        }
    }

    /// Searches the library for `text`, lists what it finds in the browser,
    /// and returns the first `limit` files.
    pub fn search(&mut self, text: &str, limit: usize) -> Vec<PathBuf> {
        self.searcher.update(&self.index);
        let found = self
            .searcher
            .search(&self.index, &Query::new(text), limit.max(LISTED));
        self.listing = Listing::Search(text.to_owned());
        self.total = found.total;
        self.found = found.hits.iter().map(|hit| hit.file).collect();
        self.paths(limit)
    }

    /// Lists the files most like `file` in the browser, and returns the
    /// first `limit`.
    pub fn find_similar(&mut self, file: &Path, limit: usize) -> Result<Vec<PathBuf>, String> {
        let like = self.find(file)?;
        self.searcher.update(&self.index);
        let found = self.searcher.similar(&self.index, like, limit.max(LISTED));
        self.listing = Listing::Similar(file.to_owned());
        self.total = found.len();
        self.found = found.iter().map(|hit| hit.file).collect();
        Ok(self.paths(limit))
    }

    /// The first `limit` files listed.
    fn paths(&self, limit: usize) -> Vec<PathBuf> {
        let listed = self.found.iter().take(limit);
        listed.filter_map(|&id| self.index.path_of(id)).collect()
    }

    /// The file of the library at `path`.
    fn find(&self, path: &Path) -> Result<FileId, String> {
        self.index
            .find(path)
            .ok_or_else(|| format!("{} isn't in the library", path.display()))
    }

    /// Previews `file` in place of what's previewing: the queue opens it,
    /// and draws its waveform.
    pub fn preview(&mut self, file: &Path) -> Result<(), String> {
        if self.queue.is_none() {
            return Err("there's nothing to preview with here".to_owned());
        }
        if !file.is_file() {
            return Err(format!("{} isn't there", file.display()));
        }
        let id = self.next_preview;
        self.next_preview += 1;
        self.preview = Some(Previewing {
            id,
            path: file.to_owned(),
            frames: None,
            peaks: None,
        });
        let streamer = self
            .streamer
            .get_or_init(|| Arc::new(Streamer::spawn(false)))
            .clone();
        let (path, done) = (file.to_owned(), self.done.clone());
        self.run(Priority::Now, move || {
            let opened = open(&path, &streamer).map_err(|error| error.to_string());
            let _ = done.send(Done::Opened { id, opened });
        });
        let (path, done) = (file.to_owned(), self.done.clone());
        self.run(Priority::Soon, move || {
            if let Some(peaks) = waveform(&path, || true) {
                let peaks = Arc::new(peaks);
                let _ = done.send(Done::Drawn { id, peaks });
            }
        });
        Ok(())
    }

    pub fn stop_preview(&mut self) {
        self.preview = None;
        self.for_engine.push(ToEngine::Stop);
    }

    /// Gives `file` the user's tag `tag`, or takes it away.
    pub fn tag(&mut self, file: &Path, tag: &str, on: bool) -> Result<bool, String> {
        let id = self.find(file)?;
        let changed = self.index.tag(id, tag, on);
        if changed {
            self.changed();
            self.relist();
        }
        Ok(changed)
    }

    /// Adds `folder` to the library, and scans it.
    pub fn add_folder(&mut self, folder: &Path) -> Result<(), String> {
        let folder = std::path::absolute(folder).map_err(|error| error.to_string())?;
        if !folder.is_dir() {
            return Err(format!("{} isn't a folder", folder.display()));
        }
        for place in self.index.places() {
            let other = place.folder();
            if folder.starts_with(other) {
                return Err(format!(
                    "{} is in the library already, in {}",
                    folder.display(),
                    other.display()
                ));
            }
            if other.starts_with(&folder) {
                return Err(format!(
                    "{} is in {}: take it out of the library first",
                    other.display(),
                    folder.display()
                ));
            }
        }
        self.index.add_place(folder.clone());
        self.searcher.update(&self.index);
        self.changed();
        self.scan(&folder);
        self.relist();
        Ok(())
    }

    /// Takes `folder` out of the library.
    pub fn remove_folder(&mut self, folder: &Path) -> Result<(), String> {
        if let Some(scan) = self.scans.remove(folder) {
            scan.cancel.store(true, Relaxed);
        }
        if let Some(reading) = self.readings.remove(folder) {
            reading.cancel.store(true, Relaxed);
        }
        if !self.index.remove_place(folder) {
            return Err(format!("{} isn't in the library", folder.display()));
        }
        self.searcher.update(&self.index);
        self.changed();
        self.relist();
        Ok(())
    }

    /// Scans `folder` again, or every folder and the presets.
    pub fn rescan(&mut self, folder: Option<&Path>) -> Result<(), String> {
        match folder {
            Some(folder) if self.index.place(folder).is_none() => {
                Err(format!("{} isn't in the library", folder.display()))
            }
            Some(folder) => {
                self.scan(folder);
                Ok(())
            }
            None => {
                self.rescan_all();
                Ok(())
            }
        }
    }

    /// Says what the engine is doing, so scans leave it room.
    pub fn set_busy(&mut self, busy: Busy) {
        if busy != self.busy {
            self.busy = busy;
            if let Some(queue) = &self.queue {
                queue.set_busy(busy);
            }
        }
    }

    /// What the engine is to do with previews since the last call.
    pub fn take_for_engine(&mut self) -> Vec<ToEngine> {
        std::mem::take(&mut self.for_engine)
    }

    /// What went wrong since the last call, for the app to show.
    pub fn take_problems(&mut self) -> Vec<String> {
        std::mem::take(&mut self.problems)
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Whether the index file has been read.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The folders being scanned, and how many files each has found so far.
    pub fn scanning(&self) -> Vec<(PathBuf, usize)> {
        let scans = self.scans.iter();
        scans
            .map(|(folder, scan)| (folder.clone(), scan.found.load(Relaxed)))
            .collect()
    }

    /// The folders whose files' headers are being read, and how many are
    /// left to read in each.
    pub fn reading(&self) -> Vec<(PathBuf, usize)> {
        let readings = self.readings.iter();
        readings
            .map(|(folder, reading)| (folder.clone(), reading.left))
            .collect()
    }

    pub fn listing(&self) -> &Listing {
        &self.listing
    }

    /// What the browser lists, best first, and how many files that found
    /// in all.
    pub fn found(&self) -> (&[FileId], usize) {
        (&self.found, self.total)
    }

    /// Where Musicable's own presets go, if it has a folder for them.
    pub fn presets_folder(&self) -> Option<&Path> {
        self.presets_folder.as_deref()
    }

    /// Saves presets in `folder` from now on, and finds the presets again,
    /// with those in it.
    pub fn set_presets_folder(&mut self, folder: Option<PathBuf>) {
        if folder != self.presets_folder {
            self.presets_folder = folder;
            self.find_presets();
        }
    }

    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    pub fn is_finding_presets(&self) -> bool {
        self.finding_presets
    }

    pub fn preview_playing(&self) -> Option<&Previewing> {
        self.preview.as_ref()
    }

    /// The device the preset at `path` holds. A VST3 preset is for the
    /// installed VST3 plugin with its class id.
    fn load_preset(&self, path: &Path, plugins: &Plugins) -> Result<DeviceKind, String> {
        match presets::load(path).map_err(|error| error.to_string())? {
            Loaded::Device(kind) => Ok(kind),
            Loaded::Vst3 { class_id, state } => {
                let installed = plugins.installed().iter().find(|plugin| {
                    plugin.format == PluginFormat::Vst3 && plugin.id.eq_ignore_ascii_case(&class_id)
                });
                let Some(plugin) = installed else {
                    return Err(format!(
                        "the VST3 plugin {} is for isn't installed",
                        path.display()
                    ));
                };
                Ok(DeviceKind::Plugin(PluginDevice {
                    format: PluginFormat::Vst3,
                    id: plugin.id.clone(),
                    name: plugin.name.clone(),
                    instrument: plugin.is_instrument(),
                    settings: PluginSettings {
                        state: Some(state),
                        params: Vec::new(),
                    },
                }))
            }
        }
    }

    /// Saves `device` as a preset, the project's device `id`, with its
    /// plugin's state saved afresh, and finds the presets again.
    fn save_preset(
        &mut self,
        id: DeviceId,
        device: &DeviceKind,
        device_name: &str,
        name: Option<&str>,
        plugins: &Plugins,
    ) -> Result<PathBuf, String> {
        let Some(folder) = self.presets_folder.clone() else {
            return Err("there's no folder for presets".to_owned());
        };
        let mut device = device.clone();
        if let (Some(plugin), Some(instance)) = (device.plugin_mut(), plugins.instance(id)) {
            match instance.save_state() {
                Ok(state) => plugin.settings.state = Some(PluginState::new(state)),
                Err(error) => self
                    .problems
                    .push(format!("{error}, so the preset has the state from before")),
            }
        }
        let path = match name {
            Some(name) => {
                let path = presets::path_for(&folder, device_name, name);
                if path.exists() {
                    return Err(format!(
                        "there's a preset called {name} for {device_name} already"
                    ));
                }
                path
            }
            None => (1..)
                .map(|n| presets::path_for(&folder, device_name, &format!("{device_name} {n}")))
                .find(|path| !path.exists())
                .expect("a number not taken"),
        };
        presets::save(&path, &device).map_err(|error| error.to_string())?;
        self.find_presets();
        Ok(path)
    }

    /// Waits until the queue has done everything asked of it, and takes in
    /// what it found, which may ask more of it, as a scan has headers read.
    /// For tests.
    #[cfg(test)]
    pub fn wait(&mut self) {
        loop {
            if let Some(queue) = &self.queue {
                queue.wait();
            }
            if !self.tick() {
                break;
            }
        }
    }
}

impl Drop for Library {
    /// Stops the scans and the readings, and saves the index if it changed
    /// since it was last saved, with the headers read so far, so the next
    /// start reads only the rest.
    fn drop(&mut self) {
        for scan in self.scans.values() {
            scan.cancel.store(true, Relaxed);
        }
        for reading in self.readings.values() {
            reading.cancel.store(true, Relaxed);
        }
        // A save on the queue may not have started, and a queue that stops
        // doesn't start what's waiting.
        let unsaved =
            self.changed.is_some() || self.saving || self.read_unsaved || !self.readings.is_empty();
        if let (true, Some(file)) = (unsaved, self.file.clone()) {
            // A copy on the queue may be saving an older one.
            if let Some(queue) = &self.queue {
                queue.wait();
            }
            self.saving = false;
            let mut read = std::mem::take(&mut self.read_while_saving);
            for done in self.received.try_iter() {
                if let Done::Read {
                    folder,
                    id,
                    read: headers,
                } = done
                {
                    read.push((folder, id, headers));
                }
            }
            for (folder, id, headers) in read {
                self.headers_read(folder, id, headers);
            }
            let _ = self.index.write(&file);
        }
    }
}

/// The files of a place whose headers a reading reads, and how far its
/// jobs have got.
struct Unread {
    folder: PathBuf,
    id: u64,
    /// Their paths, from the place's folder.
    paths: Vec<String>,
    /// The first of `paths` no job has taken.
    next: AtomicUsize,
    cancel: Arc<AtomicBool>,
    adder: Adder,
    done: Sender<Done>,
    waker: Option<Waker>,
}

/// Reads the headers of the next few files of `unread`, sends what they
/// say, and adds a job to read more while there are more, after the jobs
/// added meanwhile.
fn read_some(unread: &Arc<Unread>) {
    if unread.cancel.load(Relaxed) {
        return;
    }
    let start = unread.next.fetch_add(READ_AT_ONCE, Relaxed);
    let Some(paths) = unread.paths.get(start..).filter(|paths| !paths.is_empty()) else {
        return;
    };
    let paths = &paths[..paths.len().min(READ_AT_ONCE)];
    // Slower while the engine records.
    let pace = unread.adder.pace();
    let mut read = Vec::with_capacity(paths.len());
    for path in paths {
        pace.step();
        if unread.cancel.load(Relaxed) {
            return;
        }
        let mut file = unread.folder.clone();
        file.extend(path.split('/'));
        // A file the decoder breaks on isn't audio Musicable reads.
        let audio = panic::catch_unwind(|| header(&file)).ok().flatten();
        read.push((path.clone(), audio));
    }
    let _ = unread.done.send(Done::Read {
        folder: unread.folder.clone(),
        id: unread.id,
        read,
    });
    if let Some(waker) = &unread.waker {
        waker();
    }
    if start + READ_AT_ONCE < unread.paths.len() {
        let next = unread.clone();
        unread.adder.add(Priority::Later, move || read_some(&next));
    }
}

/// The file at `path`, opened to preview, and how many frames it has.
fn open(path: &Path, streamer: &Streamer) -> io::Result<(Source, u64)> {
    let decoder = Decoder::open(path).map_err(io::Error::other)?;
    let mut reader = FileReader(decoder);
    let frames = reader.frames();
    let source = if frames <= PREVIEW_WHOLE {
        Source::Whole(Arc::new(AudioData::read(&mut reader)?))
    } else {
        Source::Stream(streamer.stream(Box::new(reader)))
    };
    Ok((source, frames))
}

/// Where Musicable's own presets go: the Presets folder in the Musicable
/// folder in the user's music folder.
pub fn presets_folder() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let home = if cfg!(windows) {
        var("USERPROFILE")
    } else {
        var("HOME")
    }?;
    Some(
        Path::new(&home)
            .join("Music")
            .join("Musicable")
            .join("Presets"),
    )
}

/// The library and the plugins, as the commands reach them.
pub(crate) struct LibraryLink<'a> {
    pub library: &'a mut Library,
    pub plugins: &'a mut Plugins,
    /// The open project's folder, which a sampler's sample can be named
    /// from.
    pub folder: Option<PathBuf>,
}

impl musicable_commands::Library for LibraryLink<'_> {
    fn search(&mut self, text: &str, limit: usize) -> Vec<PathBuf> {
        self.library.search(text, limit)
    }

    fn find_similar(&mut self, file: &Path, limit: usize) -> Result<Vec<PathBuf>, String> {
        self.library.find_similar(file, limit)
    }

    fn preview(&mut self, file: &Path) -> Result<(), String> {
        self.library.preview(file)
    }

    fn stop_preview(&mut self) {
        self.library.stop_preview();
    }

    fn tag(&mut self, file: &Path, tag: &str, on: bool) -> Result<bool, String> {
        self.library.tag(file, tag, on)
    }

    fn add_folder(&mut self, folder: &Path) -> Result<(), String> {
        self.library.add_folder(folder)
    }

    fn remove_folder(&mut self, folder: &Path) -> Result<(), String> {
        self.library.remove_folder(folder)
    }

    fn rescan(&mut self, folder: Option<&Path>) -> Result<(), String> {
        self.library.rescan(folder)
    }

    /// A sampler's sample in the project's folder is named from there, as
    /// one loaded into the sampler is.
    fn load_preset(&mut self, preset: &Path) -> Result<DeviceKind, String> {
        let mut device = self.library.load_preset(preset, self.plugins)?;
        if let Some(sample) = sample_mut(&mut device)
            && Path::new(sample.as_str()).is_absolute()
        {
            *sample = clip_path(Path::new(sample.as_str()), self.folder.as_deref());
        }
        Ok(device)
    }

    /// A sampler's preset names its sample in full, where the project names
    /// it from its folder, so it plays in any project.
    fn save_preset(
        &mut self,
        id: DeviceId,
        device: &DeviceKind,
        device_name: &str,
        name: Option<&str>,
    ) -> Result<PathBuf, String> {
        let mut device = device.clone();
        if let (Some(sample), Some(folder)) = (sample_mut(&mut device), &self.folder)
            && Path::new(sample.as_str()).is_relative()
        {
            *sample = clip_path(&folder.join(sample.as_str()), None);
        }
        self.library
            .save_preset(id, &device, device_name, name, self.plugins)
    }
}

/// The sample `device` plays, if it's a sampler with one.
fn sample_mut(device: &mut DeviceKind) -> Option<&mut String> {
    device.native_mut()?.sample.as_mut()
}

#[cfg(test)]
mod tests {
    use musicable_engine::EngineConfig;
    use musicable_io::audio::{SampleFormat, save_wav};

    use super::*;

    /// A folder of its own for each test, removed when it ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let name = format!("musicable-library-{name}-{}", std::process::id());
            let dir = std::env::temp_dir().join(name);
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

    /// A mono WAV of `frames` frames at `path`.
    fn wav(path: &Path, frames: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let samples = vec![0.25f32; frames];
        save_wav(path, 44_100, SampleFormat::Int16, &[&samples]).unwrap();
    }

    fn open(dir: &Path) -> Library {
        let file = dir.join(LIBRARY_FILE);
        let queue = Arc::new(Queue::new(2));
        let mut library = Library::open(Some(file), Some(dir.join("Presets")), queue, None);
        library.wait();
        library
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let names = paths.iter().map(|path| path.file_name().unwrap());
        names
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn folders_are_scanned_searched_and_kept_with_their_tags() {
        let dir = TempDir::new("folders");
        let samples = dir.0.join("Samples");
        for name in ["Drums/Kick 01.wav", "Drums/Snare 01.wav", "Keys/Pad.wav"] {
            wav(&samples.join(name), 100);
        }
        fs::write(samples.join("Notes.txt"), "not audio").unwrap();
        let mut library = open(&dir.0);
        assert!(library.is_loaded());
        assert!(library.index().is_empty());

        library.add_folder(&samples).unwrap();
        // A folder in it, or one it's in, is in the library already.
        assert!(library.add_folder(&samples.join("Drums")).is_err());
        assert!(library.add_folder(&dir.0).is_err());
        assert!(library.add_folder(&dir.0.join("Nowhere")).is_err());
        assert_eq!(library.scanning().len(), 1);
        library.wait();
        assert!(library.scanning().is_empty());
        assert_eq!(library.index().len(), 3);

        // What a search finds is what the browser lists.
        assert_eq!(names(&library.search("kick", 10)), ["Kick 01.wav"]);
        assert_eq!(library.listing(), &Listing::Search("kick".to_owned()));
        assert_eq!(library.found().1, 1);
        assert_eq!(library.search("drums", 10).len(), 2);
        let kick = samples.join("Drums").join("Kick 01.wav");
        let like = library.find_similar(&kick, 10).unwrap();
        assert_eq!(names(&like)[0], "Snare 01.wav");
        assert_eq!(library.listing(), &Listing::Similar(kick.clone()));
        assert!(
            library
                .find_similar(&samples.join("Notes.txt"), 10)
                .is_err()
        );

        // Tags are searched, and kept in the index file.
        assert_eq!(library.tag(&kick, "dusty", true), Ok(true));
        assert_eq!(library.tag(&kick, "dusty", true), Ok(false));
        assert_eq!(library.search("dusty", 10), std::slice::from_ref(&kick));
        drop(library);
        let mut library = open(&dir.0);
        let id = library.index().find(&kick).unwrap();
        assert!(library.index().get(id).unwrap().has_tag("dusty"));
        // And the folder was scanned again on opening, as files change.
        wav(&samples.join("Keys/Pluck.wav"), 100);
        library.rescan(Some(&samples)).unwrap();
        library.wait();
        assert_eq!(library.index().len(), 4);
        assert!(library.rescan(Some(&dir.0)).is_err());

        library.remove_folder(&samples).unwrap();
        assert!(library.index().is_empty());
        assert!(library.remove_folder(&samples).is_err());
        // The listing went with it.
        assert_eq!(library.found(), (&[][..], 0));
        assert!(library.take_problems().is_empty());
    }

    #[test]
    fn tags_given_while_a_folder_is_scanned_stay() {
        let dir = TempDir::new("tag-race");
        let samples = dir.0.join("Samples");
        wav(&samples.join("Kick.wav"), 100);
        let queue = Arc::new(Queue::new(1));
        let file = dir.0.join(LIBRARY_FILE);
        let mut library = Library::open(Some(file), None, queue.clone(), None);
        library.wait();
        library.add_folder(&samples).unwrap();
        library.wait();
        // Holds the queue's one thread, so the rescan waits.
        let (release, released) = mpsc::channel::<()>();
        queue.add(Priority::Now, move || {
            let _ = released.recv();
        });
        library.rescan(Some(&samples)).unwrap();
        let kick = samples.join("Kick.wav");
        assert_eq!(library.tag(&kick, "dusty", true), Ok(true));
        release.send(()).unwrap();
        library.wait();
        let id = library.index().find(&kick).unwrap();
        assert!(library.index().get(id).unwrap().has_tag("dusty"));
    }

    #[test]
    fn files_are_listed_before_their_headers_are_read() {
        let dir = TempDir::new("listed");
        let samples = dir.0.join("Samples");
        for name in ["Drums/Kick 01.wav", "Drums/Snare 01.wav", "Keys/Pad.wav"] {
            wav(&samples.join(name), 100);
        }
        let queue = Arc::new(Queue::new(1));
        let file = dir.0.join(LIBRARY_FILE);
        let mut library = Library::open(Some(file), None, queue.clone(), None);
        library.wait();
        library.add_folder(&samples).unwrap();
        queue.wait();
        // Holds the queue's one thread once the scan is done, so no header
        // is read.
        let (release, released) = mpsc::channel::<()>();
        queue.add(Priority::Now, move || {
            let _ = released.recv();
        });
        library.tick();
        assert!(library.scanning().is_empty());
        assert_eq!(library.reading(), [(samples.clone(), 3)]);
        assert_eq!(names(&library.search("kick", 10)), ["Kick 01.wav"]);
        let files = library.index().places()[0].files();
        assert!(files.iter().all(|file| file.unread && file.audio.is_none()));
        // Find Similar has them too.
        let kick = samples.join("Drums").join("Kick 01.wav");
        let like = library.find_similar(&kick, 10).unwrap();
        assert_eq!(names(&like), ["Snare 01.wav"]);

        release.send(()).unwrap();
        library.wait();
        assert!(library.reading().is_empty());
        let files = library.index().places()[0].files();
        let frames: Vec<Option<u64>> = files.iter().map(|f| f.audio.map(|a| a.frames)).collect();
        assert_eq!(frames, [Some(100); 3]);
        assert!(files.iter().all(|file| !file.unread));
    }

    #[test]
    fn headers_left_unread_when_the_app_closed_are_read_on_the_next_start() {
        let dir = TempDir::new("unread");
        let samples = dir.0.join("Samples");
        wav(&samples.join("Kick.wav"), 100);
        wav(&samples.join("Snare.wav"), 200);
        // As a library closed while it read the snare's header leaves it.
        let record = |name: &str| {
            let metadata = fs::metadata(samples.join(name)).unwrap();
            let modified = metadata.modified().unwrap();
            let since = modified.duration_since(std::time::UNIX_EPOCH).unwrap();
            Record::found(name, metadata.len(), since.as_millis() as u64)
        };
        let mut kick = record("Kick.wav");
        // What it read, which isn't read again.
        let read = header(&samples.join("Kick.wav")).unwrap();
        kick.audio = Some(AudioFacts { frames: 1, ..read });
        kick.unread = false;
        let mut index = Index::new();
        index.add_place(samples.clone());
        index.set_files(&samples, vec![kick, record("Snare.wav")]);
        index.write(&dir.0.join(LIBRARY_FILE)).unwrap();

        let library = open(&dir.0);
        let files = library.index().places()[0].files();
        assert_eq!(files[0].audio.map(|a| a.frames), Some(1));
        assert_eq!(files[1].audio.map(|a| a.frames), Some(200));
        assert!(!files[1].unread);
        // And they're kept.
        drop(library);
        let index = Index::read(&dir.0.join(LIBRARY_FILE)).unwrap();
        assert!(index.places()[0].files().iter().all(|file| !file.unread));
    }

    #[test]
    fn headers_read_while_the_index_saves_wait_for_it() {
        let mut library = Library::none();
        let folder = PathBuf::from("Samples");
        library.index.add_place(folder.clone());
        let files = vec![
            Record::found("Kick.wav", 1, 1),
            Record::found("Snare.wav", 1, 1),
        ];
        library.index.set_files(&folder, files);
        let reading = Reading {
            id: 3,
            left: 2,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        library.readings.insert(folder.clone(), reading);
        library.saving = true;
        let facts = AudioFacts {
            format: musicable_library::Format::Wav,
            sample_rate: 44_100,
            channels: 1,
            frames: 10,
        };
        library.headers_read(
            folder.clone(),
            3,
            vec![("Kick.wav".to_owned(), Some(facts))],
        );
        // One from a reading stopped before.
        library.headers_read(
            folder.clone(),
            2,
            vec![("Snare.wav".to_owned(), Some(facts))],
        );
        assert!(library.index().places()[0].files()[0].unread);
        library.done.send(Done::Saved(Ok(()))).unwrap();
        library.tick();
        let files = library.index().places()[0].files();
        assert_eq!((files[0].audio, files[0].unread), (Some(facts), false));
        assert!(files[1].unread);
        assert_eq!(library.reading(), [(folder.clone(), 1)]);
        assert!(library.changed.is_none());
        // Saved once they're all read.
        library.headers_read(folder, 3, vec![("Snare.wav".to_owned(), None)]);
        assert!(library.reading().is_empty());
        assert!(library.changed.is_some());
    }

    #[test]
    fn a_scan_overtaken_leaves_the_scan_after_it_running() {
        let mut library = Library::none();
        let folder = PathBuf::from("Samples");
        let running = Scan {
            id: 2,
            found: Arc::new(AtomicUsize::new(10)),
            cancel: Arc::new(AtomicBool::new(false)),
            old: Arc::default(),
        };
        library.scans.insert(folder.clone(), running);
        library.scanned(&folder, 1, Ok(None));
        assert_eq!(library.scanning(), [(folder.clone(), 10)]);
        library.scanned(&folder, 2, Ok(None));
        assert!(library.scanning().is_empty());
    }

    #[test]
    fn an_index_file_that_cant_be_read_is_kept_aside() {
        let dir = TempDir::new("broken");
        let file = dir.0.join(LIBRARY_FILE);
        fs::write(&file, "{ not the library").unwrap();
        let mut library = open(&dir.0);
        assert!(library.is_loaded());
        let problems = library.take_problems();
        assert!(problems[0].starts_with("Couldn't read the browser's library"));
        let kept = fs::read_to_string(dir.0.join("library.json.broken")).unwrap();
        assert_eq!(kept, "{ not the library");
    }

    #[test]
    fn previews_open_their_files_for_the_engine_and_draw_their_waveforms() {
        let dir = TempDir::new("previews");
        let (short, long) = (dir.0.join("Short.wav"), dir.0.join("Long.wav"));
        wav(&short, 100);
        wav(&long, PREVIEW_WHOLE as usize + 1);
        let mut library = open(&dir.0);

        library.preview(&short).unwrap();
        assert_eq!(library.preview_playing().unwrap().frames, None);
        library.wait();
        let preview = library.preview_playing().unwrap();
        assert_eq!((preview.id, preview.frames), (1, Some(100)));
        assert_eq!(
            preview.peaks.as_ref().map(|peaks| peaks.frames()),
            Some(100)
        );
        let sent = library.take_for_engine();
        assert!(matches!(
            sent[..],
            [ToEngine::Play {
                id: 1,
                source: Source::Whole(_)
            }]
        ));
        // A long file streams.
        library.preview(&long).unwrap();
        library.wait();
        let sent = library.take_for_engine();
        assert!(matches!(
            sent[..],
            [ToEngine::Play {
                id: 2,
                source: Source::Stream(_)
            }]
        ));
        library.stop_preview();
        assert!(library.preview_playing().is_none());
        assert!(matches!(library.take_for_engine()[..], [ToEngine::Stop]));

        // A file that isn't there, or isn't audio.
        assert!(library.preview(&dir.0.join("Gone.wav")).is_err());
        let text = dir.0.join("Notes.wav");
        fs::write(&text, "not audio").unwrap();
        library.preview(&text).unwrap();
        library.wait();
        assert!(library.preview_playing().is_none());
        assert!(library.take_for_engine().is_empty());
        let problems = library.take_problems();
        assert!(problems[0].starts_with("Couldn't preview"), "{problems:?}");
        // A library without a queue previews nothing.
        assert!(Library::none().preview(&short).is_err());
    }

    #[test]
    fn devices_save_as_presets_that_load_again() {
        let dir = TempDir::new("presets");
        let plugins = Plugins::new(EngineConfig {
            sample_rate: 48_000,
            max_block: 512,
        });
        let mut library = open(&dir.0);
        let gain = DeviceKind::TestGain { gain: 0.5 };
        let device = DeviceId(7);
        let saved = library
            .save_preset(device, &gain, "Test Gain", Some("Quiet"), &plugins)
            .unwrap();
        assert_eq!(
            saved,
            dir.0.join("Presets/Test Gain/Quiet.musicable-preset")
        );
        // A name taken is refused, and without one it's numbered.
        let again = library.save_preset(device, &gain, "Test Gain", Some("Quiet"), &plugins);
        assert_eq!(
            again,
            Err("there's a preset called Quiet for Test Gain already".to_owned())
        );
        let numbered = library.save_preset(device, &gain, "Test Gain", None, &plugins);
        let numbered = numbered.unwrap();
        assert_eq!(numbered.file_stem().unwrap(), "Test Gain 1");
        library.wait();
        let found: Vec<&str> = library.presets().iter().map(|p| p.name.as_str()).collect();
        assert!(found.starts_with(&["Quiet", "Test Gain 1"]), "{found:?}");
        // What a look for them finds once a later one has started is out
        // of date, however late it comes.
        let found = library.presets().len();
        let look = library.presets_looks - 1;
        let stale = Done::Presets {
            look,
            found: Vec::new(),
        };
        library.done.send(stale).unwrap();
        library.tick();
        assert_eq!(library.presets().len(), found);
        assert_eq!(library.load_preset(&saved, &plugins), Ok(gain));
        assert!(library.load_preset(&dir.0.join("Gone"), &plugins).is_err());
    }
}

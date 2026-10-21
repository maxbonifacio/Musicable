//! The audio files the open project's clips play: found from the project's
//! folder, read with `musicable-io`'s decoders, and handed to the engine
//! whole or streaming.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use musicable_engine::media::{AudioData, AudioReader, Media, Source, Stream, Streamer};
use musicable_io::audio::Decoder;
use musicable_model::Project;
use musicable_model::clip::{ClipContent, ClipId};

/// Files up to this many frames long load whole when a graph is built:
/// about 44 seconds at 48 kHz, which is 16 MB of a stereo file. Longer ones
/// stream from disk.
const LOAD_WHOLE: u64 = 1 << 21;

/// An audio file, as the engine reads it.
struct FileReader(Decoder);

impl AudioReader for FileReader {
    fn channels(&self) -> usize {
        self.0.info().channels
    }

    fn sample_rate(&self) -> u32 {
        self.0.info().sample_rate
    }

    fn frames(&self) -> u64 {
        self.0.info().frames
    }

    fn read(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
        Ok(self.0.read(start, out)?)
    }
}

/// The audio files of the open project's clips, kept while a graph plays
/// them.
///
/// Each graph the session builds gets its [`Media`] from here, on the
/// compiler's thread. A file that a graph has already isn't read again, and
/// a clip that streams keeps its stream, so rebuilding the graph for an edit
/// doesn't interrupt the clips that play. What no graph holds any more is
/// let go, and opening a project reads its files again.
pub(crate) struct MediaPool {
    /// Whether streams wait for the disk: see [`Streamer::spawn`].
    wait: bool,
    load_whole: u64,
    /// Started when a clip first streams.
    streamer: OnceLock<Streamer>,
    files: Mutex<Files>,
}

#[derive(Default)]
struct Files {
    /// The files loaded whole, by path.
    whole: HashMap<PathBuf, Weak<AudioData>>,
    /// Each streaming clip's stream, and the file it streams.
    streams: HashMap<ClipId, (PathBuf, Weak<Stream>)>,
    /// The files whose problems were reported.
    reported: BTreeSet<PathBuf>,
    /// Problems for the app to show, oldest first.
    problems: Vec<String>,
}

impl Files {
    /// The source a graph already has for `clip`, playing `path`.
    fn cached(&self, clip: ClipId, path: &Path) -> Option<Source> {
        if let Some(data) = self.whole.get(path).and_then(Weak::upgrade) {
            return Some(Source::Whole(data));
        }
        match self.streams.get(&clip) {
            Some((streamed, stream)) if streamed == path => stream.upgrade().map(Source::Stream),
            _ => None,
        }
    }

    /// Reports `problem` with the file at `path`, unless one was reported
    /// already.
    fn report(&mut self, path: &Path, problem: String) {
        if self.reported.insert(path.to_owned()) {
            self.problems.push(problem);
        }
    }
}

impl MediaPool {
    /// With `wait`, clips that stream wait for the disk instead of playing
    /// silence where it's behind: for rendering offline.
    pub(crate) fn new(wait: bool) -> Self {
        Self {
            wait,
            load_whole: LOAD_WHOLE,
            streamer: OnceLock::new(),
            files: Mutex::default(),
        }
    }

    /// The audio of `project`'s audio clips, whose relative paths start from
    /// `folder`. Clips whose files can't be read have none, and the problem
    /// is reported.
    pub(crate) fn media(&self, project: &Project, folder: Option<&Path>) -> Media {
        {
            let mut files = self.files();
            files.whole.retain(|_, data| data.strong_count() > 0);
            files
                .streams
                .retain(|_, (_, stream)| stream.strong_count() > 0);
        }
        let mut media = Media::new();
        for clip in project.tracks.iter().flat_map(|track| &track.clips) {
            let ClipContent::Audio(audio) = &clip.content else {
                continue;
            };
            let path = Path::new(&audio.file);
            let path = match (path.is_absolute(), folder) {
                (true, _) => path.to_owned(),
                (false, Some(folder)) => folder.join(path),
                (false, None) => {
                    let problem = format!(
                        "Couldn't find the audio file {}: it's in the project's folder, and \
                         the project isn't saved.",
                        audio.file
                    );
                    self.files().report(path, problem);
                    continue;
                }
            };
            match self.source(clip.id, &path) {
                Ok(source) => media.insert(clip.id, source),
                Err(error) => {
                    let problem = format!(
                        "Couldn't read the audio file {}: {error}. Its clips are silent.",
                        path.display()
                    );
                    self.files().report(&path, problem);
                }
            }
        }
        media
    }

    /// The audio `clip` plays from the file at `path`: what a graph has
    /// already, or the file loaded whole or streaming. The lock isn't held
    /// while the file is read, so the app can ask for problems meanwhile.
    fn source(&self, clip: ClipId, path: &Path) -> io::Result<Source> {
        if let Some(source) = self.files().cached(clip, path) {
            return Ok(source);
        }
        let mut reader = FileReader(Decoder::open(path)?);
        if reader.frames() <= self.load_whole {
            let data = Arc::new(AudioData::read(&mut reader)?);
            let whole = Arc::downgrade(&data);
            self.files().whole.insert(path.to_owned(), whole);
            Ok(Source::Whole(data))
        } else {
            let streamer = self.streamer.get_or_init(|| Streamer::spawn(self.wait));
            let stream = streamer.stream(Box::new(reader));
            let streaming = (path.to_owned(), Arc::downgrade(&stream));
            self.files().streams.insert(clip, streaming);
            Ok(Source::Stream(stream))
        }
    }

    /// What went wrong reading files since the last call, for the app to
    /// show: files that couldn't be read when a graph was built, and ones
    /// that failed while they streamed. Each file's problem is reported
    /// once.
    pub(crate) fn take_problems(&self) -> Vec<String> {
        let mut files = self.files();
        let failed: Vec<PathBuf> = files
            .streams
            .values()
            .filter(|(_, stream)| stream.upgrade().is_some_and(|stream| stream.failed()))
            .map(|(path, _)| path.clone())
            .collect();
        for path in failed {
            let problem = format!(
                "Reading the audio file {} failed while it played. The parts that failed \
                 are silent.",
                path.display()
            );
            files.report(&path, problem);
        }
        std::mem::take(&mut files.problems)
    }

    /// Forgets the files read and the problems reported, for a newly opened
    /// project: its files are read again, as they are now, even those a
    /// graph still plays, and their problems are reported again.
    pub(crate) fn forget(&self) {
        *self.files() = Files::default();
    }

    fn files(&self) -> MutexGuard<'_, Files> {
        // Nothing that holds the lock leaves the files half changed.
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use musicable_io::audio::{SampleFormat, save_wav};
    use musicable_model::TrackKind;
    use musicable_model::clip::AudioClip;
    use musicable_model::time::{BeatRange, Beats};

    use super::*;

    /// A folder of its own for each test, empty.
    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("musicable-media-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// A project with a clip of each file on an audio track.
    fn project(files: &[&str]) -> Project {
        let mut project = Project::new();
        let track = project.add_track("Audio", TrackKind::Audio);
        for (i, file) in files.iter().enumerate() {
            let range = BeatRange::new(Beats::new(i as i64), Beats::new(i as i64 + 1));
            let content = ClipContent::Audio(AudioClip::new(*file));
            let clip = project.new_clip(*file, range, content);
            project.track_mut(track).unwrap().clips.push(clip);
        }
        project
    }

    fn ids(project: &Project) -> Vec<ClipId> {
        project.tracks[0].clips.iter().map(|c| c.id).collect()
    }

    fn write(path: &Path, frames: usize) {
        let samples: Vec<f32> = (0..frames).map(|i| (i % 100) as f32 / 128.0).collect();
        save_wav(path, 48_000, SampleFormat::Int16, &[&samples]).unwrap();
    }

    #[test]
    fn short_files_load_whole_and_long_ones_stream() {
        let folder = folder("whole-and-streams");
        fs::create_dir(folder.join("Audio")).unwrap();
        write(&folder.join("Audio/short.wav"), 100);
        write(&folder.join("long.wav"), 101);
        let absolute = folder.join("long.wav").to_string_lossy().into_owned();
        let project = project(&["Audio/short.wav", "long.wav", &absolute]);
        let mut pool = MediaPool::new(true);
        pool.load_whole = 100;
        let media = pool.media(&project, Some(&folder));
        let [short, long, again] = ids(&project)[..] else {
            panic!("three clips");
        };
        assert!(matches!(media.get(short), Some(Source::Whole(data)) if data.frames() == 100));
        let (Some(Source::Stream(first)), Some(Source::Stream(second))) =
            (media.get(long), media.get(again))
        else {
            panic!("the long file streams");
        };
        // A stream follows one clip, so each clip has its own.
        assert!(!Arc::ptr_eq(first, second));
        assert_eq!(first.frames(), 101);
        assert_eq!(pool.take_problems(), Vec::<String>::new());
    }

    #[test]
    fn rebuilt_graphs_get_what_the_old_ones_have() {
        let folder = folder("rebuilt");
        write(&folder.join("short.wav"), 10);
        write(&folder.join("long.wav"), 1000);
        let project = project(&["short.wav", "long.wav"]);
        let mut pool = MediaPool::new(true);
        pool.load_whole = 100;
        let media = pool.media(&project, Some(&folder));
        let again = pool.media(&project, Some(&folder));
        let [short, long] = ids(&project)[..] else {
            panic!("two clips");
        };
        let same = |a: &Media, b: &Media, id| match (a.get(id), b.get(id)) {
            (Some(Source::Whole(a)), Some(Source::Whole(b))) => Arc::ptr_eq(a, b),
            (Some(Source::Stream(a)), Some(Source::Stream(b))) => Arc::ptr_eq(a, b),
            _ => false,
        };
        assert!(same(&media, &again, short) && same(&media, &again, long));
        // Once nothing holds them, they're read again.
        drop((media, again));
        let fresh = pool.media(&project, Some(&folder));
        assert!(fresh.get(short).is_some() && fresh.get(long).is_some());
        assert!(pool.files().whole.len() == 1 && pool.files().streams.len() == 1);
        // Opened again, a project reads its files again, as they are now.
        pool.forget();
        let reopened = pool.media(&project, Some(&folder));
        assert!(reopened.get(short).is_some() && reopened.get(long).is_some());
        assert!(!same(&fresh, &reopened, short) && !same(&fresh, &reopened, long));
    }

    #[test]
    fn files_that_cant_be_read_are_reported_once() {
        let folder = folder("problems");
        fs::write(folder.join("notes.txt"), "not audio").unwrap();
        let project = project(&["missing.wav", "notes.txt", "missing.wav"]);
        let pool = MediaPool::new(true);
        let media = pool.media(&project, Some(&folder));
        assert!(media.is_empty());
        let problems = pool.take_problems();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[0].contains("missing.wav") && problems[0].ends_with("Its clips are silent.")
        );
        assert!(
            problems[1].contains("not a WAV, AIFF or FLAC file"),
            "{problems:?}"
        );
        pool.media(&project, Some(&folder));
        assert_eq!(pool.take_problems(), Vec::<String>::new());
        // A project opened again reports them again.
        pool.forget();
        pool.media(&project, Some(&folder));
        assert_eq!(pool.take_problems().len(), 2);
        // Without a folder, relative paths lead nowhere.
        pool.media(&project, None);
        let problems = pool.take_problems();
        assert!(
            problems.iter().any(|p| p.contains("isn't saved")),
            "{problems:?}"
        );
    }
}

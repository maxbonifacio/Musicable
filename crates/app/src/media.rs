//! The audio files the open project's clips and samplers play: found from
//! the project's folder, read with `musicable-io`'s decoders, and handed to
//! the engine whole or streaming, with the segments that clips in Spectral
//! play from the render cache.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use musicable_engine::clips::spectral::Plan;
use musicable_engine::devices::Sample;
use musicable_engine::media::{AudioData, AudioReader, Media, Source, Stream, Streamer};
use musicable_io::audio::Decoder;
use musicable_model::Project;
use musicable_model::clip::{ClipContent, ClipId};
use musicable_model::time::Beats;

use crate::renders::{self, FileKey, FileRef, RenderCache};

/// Files up to this many frames long load whole when a graph is built:
/// about 44 seconds at 48 kHz, which is 16 MB of a stereo file. Longer ones
/// stream from disk.
const LOAD_WHOLE: u64 = 1 << 21;

/// An audio file, as the engine reads it.
pub(crate) struct FileReader(pub(crate) Decoder);

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

/// The audio files of the open project's clips and samplers, kept while a
/// graph plays them.
///
/// Each graph the session builds gets its [`Media`] from here, on the
/// compiler's thread. A file that a graph has already isn't read again, and
/// a clip that streams keeps its stream, so rebuilding the graph for an edit
/// doesn't interrupt the clips that play. What no graph holds any more is
/// let go, and opening a project reads its files again. A sampler's sample
/// loads whole, however long it is, as the sampler reads anywhere in it.
///
/// A clip in Spectral gets the segments it plays from the render cache,
/// which renders the ones it doesn't have yet in the background, or, where
/// streams wait for the disk, as for an export, before the graph is built.
pub(crate) struct MediaPool {
    /// Whether streams wait for the disk, and graphs for their clips'
    /// segments: see [`Streamer::spawn`].
    wait: bool,
    load_whole: u64,
    /// Started when a clip first streams.
    streamer: OnceLock<Streamer>,
    files: Mutex<Files>,
    renders: Arc<RenderCache>,
    /// Set as the session goes: segments that a graph waits for stop
    /// rendering, as nothing will play it.
    stop: AtomicBool,
}

#[derive(Default)]
struct Files {
    /// The files loaded whole, by path, with their keys as they were read.
    whole: HashMap<PathBuf, (Weak<AudioData>, FileKey)>,
    /// The samplers' samples, by path.
    samples: HashMap<PathBuf, Weak<Sample>>,
    /// Each streaming clip's stream, the file it streams, and its key.
    streams: HashMap<ClipId, (PathBuf, Weak<Stream>, FileKey)>,
    /// The files whose problems were reported.
    reported: BTreeSet<PathBuf>,
    /// Problems for the app to show, oldest first.
    problems: Vec<String>,
}

impl Files {
    /// The source a graph already has for `clip`, playing `path`, and its
    /// file's key.
    fn cached(&self, clip: ClipId, path: &Path) -> Option<(Source, FileKey)> {
        if let Some((data, key)) = self.whole.get(path)
            && let Some(data) = data.upgrade()
        {
            return Some((Source::Whole(data), *key));
        }
        match self.streams.get(&clip) {
            Some((streamed, stream, key)) if streamed == path => stream
                .upgrade()
                .map(|stream| (Source::Stream(stream), *key)),
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
    /// silence where it's behind, and graphs wait for their clips' segments
    /// to render: for rendering offline. The segments are the app's render
    /// cache's.
    pub(crate) fn new(wait: bool) -> Self {
        Self::with_renders(wait, RenderCache::global().clone())
    }

    /// Like [`new`](Self::new), with segments from `renders`.
    pub(crate) fn with_renders(wait: bool, renders: Arc<RenderCache>) -> Self {
        Self {
            wait,
            load_whole: LOAD_WHOLE,
            streamer: OnceLock::new(),
            files: Mutex::default(),
            renders,
            stop: AtomicBool::new(false),
        }
    }

    /// The audio of `project`'s audio clips, with the segments of those in
    /// Spectral, and its samplers' samples, whose relative paths start
    /// from `folder`. Clips and samplers whose files can't be read have
    /// none, and the problem is reported. The segments still to render do,
    /// those after `playhead` first.
    pub(crate) fn media(&self, project: &Project, folder: Option<&Path>, playhead: Beats) -> Media {
        {
            let mut files = self.files();
            files.whole.retain(|_, (data, _)| data.strong_count() > 0);
            files.samples.retain(|_, sample| sample.strong_count() > 0);
            files
                .streams
                .retain(|_, (_, stream, _)| stream.strong_count() > 0);
        }
        self.renders.prune();
        let mut media = Media::new();
        let mut jobs = Vec::new();
        for clip in project.tracks.iter().flat_map(|track| &track.clips) {
            let ClipContent::Audio(audio) = &clip.content else {
                continue;
            };
            let Some(path) = self.find(&audio.file, folder, "audio file") else {
                continue;
            };
            match self.source(clip.id, &path) {
                Ok((source, key)) => {
                    let (channels, rate) = (source.channels(), source.sample_rate());
                    let plan = Plan::new(clip, &project.tempo, channels, rate, source.frames());
                    if let Some(plan) = plan {
                        let whole = match &source {
                            Source::Whole(data) => Some(Arc::downgrade(data)),
                            Source::Stream(_) => None,
                        };
                        let file = FileRef { key, path, whole };
                        let renders = self.renders.plan(&file, plan, self.wait, &mut jobs);
                        media.insert_renders(clip.id, renders);
                    }
                    media.insert(clip.id, source);
                }
                Err(error) => {
                    let problem = format!(
                        "Couldn't read the audio file {}: {error}. Its clips are silent.",
                        path.display()
                    );
                    self.files().report(&path, problem);
                }
            }
        }
        for device in project.chains().flat_map(|(_, devices)| devices) {
            let Some(file) = device.kind.native().and_then(|n| n.sample.as_ref()) else {
                continue;
            };
            let Some(path) = self.find(file, folder, "sample") else {
                continue;
            };
            match self.sample(&path) {
                Ok(sample) => media.insert_sample(device.id, sample),
                Err(error) => {
                    let problem = format!(
                        "Couldn't read the sample {}: {error}. Its sampler is silent.",
                        path.display()
                    );
                    self.files().report(&path, problem);
                }
            }
        }
        renders::order(&mut jobs, &project.tempo, playhead);
        match self.wait {
            true => self.renders.render_now(jobs, &self.stop),
            false => self.renders.render_later(jobs),
        }
        media
    }

    /// Stops rendering the segments a graph waits for, as the session goes
    /// and nothing will play it.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    /// Where the file a project names `file` is: relative paths start from
    /// `folder`. A project that isn't saved has no folder, so a relative
    /// path leads nowhere, and the problem with the `what`, such as "audio
    /// file", is reported.
    fn find(&self, file: &str, folder: Option<&Path>, what: &str) -> Option<PathBuf> {
        let path = Path::new(file);
        match (path.is_absolute(), folder) {
            (true, _) => Some(path.to_owned()),
            (false, Some(folder)) => Some(folder.join(path)),
            (false, None) => {
                let problem = format!(
                    "Couldn't find the {what} {file}: it's in the project's folder, and the \
                     project isn't saved."
                );
                self.files().report(path, problem);
                None
            }
        }
    }

    /// The audio `clip` plays from the file at `path`, and the file's key:
    /// what a graph has already, or the file loaded whole or streaming. The
    /// lock isn't held while the file is read, so the app can ask for
    /// problems meanwhile.
    fn source(&self, clip: ClipId, path: &Path) -> io::Result<(Source, FileKey)> {
        if let Some(cached) = self.files().cached(clip, path) {
            return Ok(cached);
        }
        let key = FileKey::of(path)?;
        let mut reader = FileReader(Decoder::open(path)?);
        if reader.frames() <= self.load_whole {
            let data = Arc::new(AudioData::read(&mut reader)?);
            let whole = (Arc::downgrade(&data), key);
            self.files().whole.insert(path.to_owned(), whole);
            Ok((Source::Whole(data), key))
        } else {
            let streamer = self.streamer.get_or_init(|| Streamer::spawn(self.wait));
            let stream = streamer.stream(Box::new(reader));
            let streaming = (path.to_owned(), Arc::downgrade(&stream), key);
            self.files().streams.insert(clip, streaming);
            Ok((Source::Stream(stream), key))
        }
    }

    /// The sample in the file at `path`, for samplers: what a graph has
    /// already, or the file read whole. The lock isn't held while it's read.
    fn sample(&self, path: &Path) -> io::Result<Arc<Sample>> {
        if let Some(sample) = self.files().samples.get(path).and_then(Weak::upgrade) {
            return Ok(sample);
        }
        let mut reader = FileReader(Decoder::open(path)?);
        let sample = Arc::new(Sample::new(&AudioData::read(&mut reader)?));
        let weak = Arc::downgrade(&sample);
        self.files().samples.insert(path.to_owned(), weak);
        Ok(sample)
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
            .filter(|(_, stream, _)| stream.upgrade().is_some_and(|stream| stream.failed()))
            .map(|(path, _, _)| path.clone())
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

    use musicable_engine::clips::spectral::Renders;
    use musicable_io::audio::{SampleFormat, save_wav};
    use musicable_model::clip::{AudioClip, Offset, Warping};
    use musicable_model::native::{NativeDevice, NativeKind};
    use musicable_model::time::{BeatRange, Beats};
    use musicable_model::warp::{Warp, WarpMode};
    use musicable_model::{DeviceId, DeviceKind, TrackKind};

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

    /// Adds a MIDI track with a sampler playing each of `files`, and one
    /// without a sample, to `project`, and returns their ids.
    fn samplers(project: &mut Project, files: &[&str]) -> Vec<DeviceId> {
        let track = project.add_track("Samplers", TrackKind::Midi);
        let mut devices = Vec::new();
        for file in files
            .iter()
            .map(|file| Some(file.to_string()))
            .chain([None])
        {
            let mut sampler = NativeDevice::new(NativeKind::Sampler);
            sampler.sample = file;
            devices.push(project.new_device(DeviceKind::Native(sampler)));
        }
        let ids = devices.iter().map(|device| device.id).collect();
        project.track_mut(track).unwrap().devices = devices;
        ids
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
        let media = pool.media(&project, Some(&folder), Beats::ZERO);
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
        let media = pool.media(&project, Some(&folder), Beats::ZERO);
        let again = pool.media(&project, Some(&folder), Beats::ZERO);
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
        let fresh = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(fresh.get(short).is_some() && fresh.get(long).is_some());
        assert!(pool.files().whole.len() == 1 && pool.files().streams.len() == 1);
        // Opened again, a project reads its files again, as they are now.
        pool.forget();
        let reopened = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(reopened.get(short).is_some() && reopened.get(long).is_some());
        assert!(!same(&fresh, &reopened, short) && !same(&fresh, &reopened, long));
    }

    #[test]
    fn clips_in_spectral_play_segments_from_the_render_cache() {
        let folder = folder("spectral");
        write(&folder.join("voice.wav"), 48_000);
        write(&folder.join("long.wav"), 96_000);
        let mut project = project(&["voice.wav", "long.wav", "voice.wav"]);
        // The first two in Spectral, at 100 BPM, so they play faster than
        // their files; the third plays its file as it is.
        for clip in &mut project.tracks[0].clips[..2] {
            let ClipContent::Audio(audio) = &mut clip.content else {
                unreachable!("an audio clip");
            };
            let mut warp = Warp::new(0, Beats::ZERO, 100.0);
            warp.mode = WarpMode::Spectral;
            let warping = Warping {
                offset: Offset::Beats(Beats::ZERO),
                warp: Some(warp),
            };
            assert!(audio.set_warping(warping));
        }
        let [voice, long, plain] = ids(&project)[..] else {
            panic!("three clips");
        };
        // Waiting, as for an export, the graph gets them rendered, from the
        // file in memory and the one that streams.
        let renders = RenderCache::new();
        let mut pool = MediaPool::with_renders(true, renders.clone());
        pool.load_whole = 50_000;
        let media = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(matches!(media.get(long), Some(Source::Stream(_))));
        assert!(media.renders(voice).is_some_and(Renders::is_ready));
        assert!(media.renders(long).is_some_and(Renders::is_ready));
        assert!(media.renders(plain).is_none());
        assert_eq!(renders.pending(), 0);

        // Played live, they render in the background, once there's a
        // queue, and the graph plays its live preview meanwhile.
        let renders = RenderCache::new();
        let live = MediaPool::with_renders(false, renders.clone());
        let playing = live.media(&project, Some(&folder), Beats::ZERO);
        let slots = |media: &Media| {
            let renders = [voice, long].map(|clip| media.renders(clip).unwrap());
            renders.map(|renders| renders.slots.clone()).concat()
        };
        assert!(slots(&playing).iter().all(|slot| !slot.is_ready()));
        assert_eq!(renders.pending(), slots(&playing).len());
        // An export meanwhile renders them there and then, for both.
        let export = MediaPool::with_renders(true, renders.clone());
        let exported = export.media(&project, Some(&folder), Beats::ZERO);
        let mut shared = slots(&playing).into_iter().zip(slots(&exported));
        assert!(shared.all(|(a, b)| Arc::ptr_eq(&a, &b) && a.is_ready()));
        assert_eq!(live.take_problems(), Vec::<String>::new());
    }

    #[test]
    fn files_that_cant_be_read_are_reported_once() {
        let folder = folder("problems");
        fs::write(folder.join("notes.txt"), "not audio").unwrap();
        let project = project(&["missing.wav", "notes.txt", "missing.wav"]);
        let pool = MediaPool::new(true);
        let media = pool.media(&project, Some(&folder), Beats::ZERO);
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
        pool.media(&project, Some(&folder), Beats::ZERO);
        assert_eq!(pool.take_problems(), Vec::<String>::new());
        // A project opened again reports them again.
        pool.forget();
        pool.media(&project, Some(&folder), Beats::ZERO);
        assert_eq!(pool.take_problems().len(), 2);
        // Without a folder, relative paths lead nowhere.
        pool.media(&project, None, Beats::ZERO);
        let problems = pool.take_problems();
        assert!(
            problems.iter().any(|p| p.contains("isn't saved")),
            "{problems:?}"
        );
    }

    #[test]
    fn samplers_load_their_samples_whole_once_for_each_file() {
        let folder = folder("samples");
        write(&folder.join("kick.wav"), 1000);
        fs::write(folder.join("notes.txt"), "not audio").unwrap();
        // A clip of the file too, which streams.
        let mut project = project(&["kick.wav"]);
        let files = ["kick.wav", "kick.wav", "missing.wav", "notes.txt"];
        let [kick, same, missing, notes, none] = samplers(&mut project, &files)[..] else {
            panic!("five samplers");
        };
        let mut pool = MediaPool::new(true);
        pool.load_whole = 100;
        let media = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(matches!(
            media.get(ids(&project)[0]),
            Some(Source::Stream(_))
        ));
        let sample = Arc::clone(media.sample(kick).unwrap());
        assert_eq!((sample.frames(), sample.channels()), (1000, 1));
        // Samplers of the same file share it, and so do rebuilt graphs.
        assert!(Arc::ptr_eq(&sample, media.sample(same).unwrap()));
        let rebuilt = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(Arc::ptr_eq(&sample, rebuilt.sample(kick).unwrap()));
        assert!(
            [missing, notes, none]
                .iter()
                .all(|&id| media.sample(id).is_none())
        );
        let problems = pool.take_problems();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[0].contains("missing.wav") && problems[0].ends_with("Its sampler is silent."),
            "{problems:?}"
        );
        assert!(problems[1].contains("notes.txt"), "{problems:?}");
        // Once nothing holds it, it's read again.
        drop((media, rebuilt, sample));
        assert!(pool.files().samples.values().all(|s| s.strong_count() == 0));
        let fresh = pool.media(&project, Some(&folder), Beats::ZERO);
        assert!(fresh.sample(kick).is_some());
        assert_eq!(pool.files().samples.len(), 1);

        // Without a folder, relative paths lead nowhere.
        let mut unsaved = Project::new();
        samplers(&mut unsaved, &["kick.wav"]);
        assert!(pool.media(&unsaved, None, Beats::ZERO).is_empty());
        assert_eq!(
            pool.take_problems(),
            [
                "Couldn't find the sample kick.wav: it's in the project's folder, and the project \
              isn't saved."
            ]
        );
    }
}

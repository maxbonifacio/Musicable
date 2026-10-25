//! Rendering clips in Spectral ahead of playback, and the render cache that
//! keeps what's rendered.
//!
//! A clip in Spectral plays segments of its file stretched ahead of time,
//! and a live stretch until they're ready (see
//! `musicable_engine::clips::spectral`). Each graph the session builds asks
//! for its clips' segments here, on the compiler's thread: a segment that
//! another graph has already, or is waiting for, is shared, and the others
//! render in the background on the library's queue, those the playhead
//! plays next first, or there and then, on every core, for an export,
//! which waits for them.
//!
//! What's rendered stays in memory while a graph plays it, and in the
//! render cache's folder, so the next song that plays a segment, in this
//! project or another, finds it ready. A segment is kept by its key: its
//! file's, from the file's path, size and when it last changed, with the
//! segment's own, which says everything its render depends on but not where
//! in the song it plays. Each is a file of its own in the folder, named by
//! its key: a header, then its channels one after another, as little-endian
//! 32-bit floats. The folder holds up to a size, and always leaves some of
//! the disk free: past either, the segments used longest ago go first.

use std::collections::HashMap;
use std::fs;
use std::hash::Hasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::SystemTime;

use musicable_analysis::cache::Stamp;
use musicable_engine::clips::spectral::{Plan, Rendered, Renders, render};
use musicable_engine::media::{AudioData, DataReader};
use musicable_io::audio::Decoder;
use musicable_library::{Adder, Priority};
use musicable_model::time::{Beats, TempoMap};
use musicable_plugins::Waker;
use siphasher::sip::SipHasher13;

use crate::media::FileReader;

/// A gigabyte, as the settings count the cache's limits: 2^30 bytes, as
/// Windows counts them.
pub(crate) const GB: u64 = 1 << 30;

/// What a segment's file starts with, and the version of what follows.
const MAGIC: [u8; 4] = *b"MSRN";
const FORMAT: u32 = 1;
/// How long a segment file's header is: the magic, the version, the
/// channels, the sample rate and the frames.
const HEADER: usize = 24;
const EXTENSION: &str = "render";

/// The file a clip plays, as the render cache knows it: by its path, its
/// size and when it last changed, as it was when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileKey(u64);

impl FileKey {
    /// The file at `path`, as it is now.
    pub(crate) fn of(path: &Path) -> io::Result<Self> {
        let stamp = Stamp::of(path)?;
        let mut hasher = SipHasher13::new();
        hasher.write(path.to_string_lossy().as_bytes());
        hasher.write_u64(stamp.size);
        hasher.write_u64(stamp.modified);
        Ok(Self(hasher.finish()))
    }
}

/// A file that a clip in Spectral plays, for rendering its segments: its
/// key, where it is, and its audio, while a graph holds the file whole.
#[derive(Debug, Clone)]
pub(crate) struct FileRef {
    pub(crate) key: FileKey,
    pub(crate) path: PathBuf,
    pub(crate) whole: Option<Weak<AudioData>>,
}

/// How much the render cache's folder holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The most its segments take, in bytes.
    pub(crate) most: u64,
    /// How much of the disk it always leaves free, in bytes.
    pub(crate) free: u64,
}

/// The render cache: the segments that graphs play, rendered or still to
/// be, and the folder that keeps them.
pub(crate) struct RenderCache {
    /// The segments handed out, by key, so graphs share them.
    slots: Mutex<HashMap<u64, Weak<Rendered>>>,
    background: Mutex<Background>,
    disk: Mutex<Option<Disk>>,
    /// The segments asked for in the background and not done yet.
    pending: AtomicUsize,
}

/// Where segments render in the background.
#[derive(Default)]
struct Background {
    adder: Option<Adder>,
    /// Wakes the window as each is done, for the status bar.
    waker: Option<Waker>,
    /// The segments asked for before there was a queue.
    waiting: Vec<Job>,
}

/// A segment to render: one of `plan`'s, from its clip's file.
pub(crate) struct Job {
    key: u64,
    plan: Arc<Plan>,
    segment: usize,
    file: FileRef,
}

impl RenderCache {
    /// A cache in memory alone, which renders in the background once it's
    /// [started](Self::start).
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Mutex::default(),
            background: Mutex::default(),
            disk: Mutex::new(None),
            pending: AtomicUsize::new(0),
        })
    }

    /// The app's, which every session shares, so an export finds what
    /// playback rendered.
    pub(crate) fn global() -> &'static Arc<Self> {
        static GLOBAL: OnceLock<Arc<RenderCache>> = OnceLock::new();
        GLOBAL.get_or_init(Self::new)
    }

    /// Renders in the background from now on, on the queue `adder` adds to,
    /// waking the window with `waker` as each segment is done: first those
    /// asked for already.
    pub(crate) fn start(self: &Arc<Self>, adder: Adder, waker: Option<Waker>) {
        let waiting = {
            let mut background = self.background();
            background.adder = Some(adder.clone());
            background.waker = waker;
            std::mem::take(&mut background.waiting)
        };
        self.add(&adder, waiting);
    }

    /// Keeps segments in `folder` from now on, as far as `limits` lets it,
    /// or in memory alone, without one.
    pub(crate) fn set_folder(&self, folder: Option<PathBuf>, limits: Limits) {
        *self.disk() = folder.map(|folder| Disk {
            folder,
            limits,
            used: None,
        });
    }

    /// Holds as much as `limits` lets it from now on, letting go of
    /// segments now if it holds more.
    pub(crate) fn set_limits(&self, limits: Limits) {
        if let Some(disk) = self.disk().as_mut() {
            disk.limits = limits;
            disk.make_room(0);
        }
    }

    /// Where it keeps segments, if it does.
    pub(crate) fn folder(&self) -> Option<PathBuf> {
        self.disk().as_ref().map(|disk| disk.folder.clone())
    }

    /// How many segments are still to render in the background.
    pub(crate) fn pending(&self) -> usize {
        self.pending.load(Relaxed)
    }

    /// The segments of `plan`, for a clip playing `file`: those a graph has
    /// already, and the others to render, which go in `jobs`. For a graph
    /// that `waits` for its segments, so do those a graph has that are
    /// still to render in the background: whichever renders one first
    /// fills it.
    pub(crate) fn plan(
        &self,
        file: &FileRef,
        plan: Plan,
        waits: bool,
        jobs: &mut Vec<Job>,
    ) -> Renders {
        let plan = Arc::new(plan);
        let mut slots = self.slots();
        let mut renders = Vec::with_capacity(plan.segments().len());
        for (segment, planned) in plan.segments().iter().enumerate() {
            let key = key(file.key, planned.key());
            let (slot, wanted) = match slots.get(&key).and_then(Weak::upgrade) {
                Some(slot) => {
                    let wanted = waits && !slot.is_ready() && jobs.iter().all(|j| j.key != key);
                    (slot, wanted)
                }
                None => {
                    let slot = Arc::new(Rendered::new());
                    slots.insert(key, Arc::downgrade(&slot));
                    (slot, true)
                }
            };
            if wanted {
                jobs.push(Job {
                    key,
                    plan: plan.clone(),
                    segment,
                    file: file.clone(),
                });
            }
            renders.push(slot);
        }
        Renders {
            plan,
            slots: renders.into(),
        }
    }

    /// Lets go of the segments no graph has any more.
    pub(crate) fn prune(&self) {
        self.slots().retain(|_, slot| slot.strong_count() > 0);
    }

    /// Renders `jobs` here and now, on as many threads as the computer has
    /// cores, unless `stop` is set, which it looks at between segments.
    pub(crate) fn render_now(&self, jobs: Vec<Job>, stop: &AtomicBool) {
        let next = AtomicUsize::new(0);
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        std::thread::scope(|scope| {
            for _ in 0..cores.min(jobs.len()) {
                scope.spawn(|| {
                    while !stop.load(Relaxed) {
                        let Some(job) = jobs.get(next.fetch_add(1, Relaxed)) else {
                            break;
                        };
                        self.run(job);
                    }
                });
            }
        });
    }

    /// Renders `jobs` in the background, in order, once there's a queue.
    pub(crate) fn render_later(self: &Arc<Self>, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        self.pending.fetch_add(jobs.len(), Relaxed);
        let adder = {
            let mut background = self.background();
            match background.adder.clone() {
                Some(adder) => adder,
                None => {
                    background.waiting.extend(jobs);
                    return;
                }
            }
        };
        self.add(&adder, jobs);
    }

    fn add(self: &Arc<Self>, adder: &Adder, jobs: Vec<Job>) {
        for job in jobs {
            let cache = self.clone();
            adder.add(Priority::Soon, move || {
                cache.run(&job);
                cache.pending.fetch_sub(1, Relaxed);
                let waker = cache.background().waker.clone();
                if let Some(waker) = waker {
                    waker();
                }
            });
        }
    }

    /// Fills the segment `job` asks for, unless no graph wants it any more
    /// or it's filled already: from the folder, where it's kept, or else
    /// rendered, and then kept.
    fn run(&self, job: &Job) {
        let slot = self.slots().get(&job.key).and_then(Weak::upgrade);
        let Some(slot) = slot.filter(|slot| !slot.is_ready()) else {
            return;
        };
        if let Some(audio) = self.load(job) {
            slot.fill(audio);
            return;
        }
        match render_from(job) {
            Ok(audio) => {
                slot.fill(audio);
                if let Some(audio) = slot.get() {
                    self.keep(job.key, audio);
                }
            }
            Err(error) => log::warn!(
                "couldn't render a segment of {}: {error}",
                job.file.path.display()
            ),
        }
    }

    /// The segment `job` asks for, if the folder keeps it, which counts as
    /// using it.
    fn load(&self, job: &Job) -> Option<AudioData> {
        let path = self.disk().as_ref()?.path(job.key);
        let frames = job.plan.segments()[job.segment].frames();
        let audio = read_segment(&path, job.plan.channels(), job.plan.sample_rate(), frames)?;
        let _ = touch(&path);
        Some(audio)
    }

    /// Keeps `audio`, the segment of key `key`, in the folder, if there's
    /// one and room in it.
    fn keep(&self, key: u64, audio: &AudioData) {
        let bytes = (HEADER + 4 * audio.channels() * audio.frames() as usize) as u64;
        let path = {
            let mut disk = self.disk();
            let Some(disk) = disk.as_mut() else {
                return;
            };
            if !disk.make_room(bytes) {
                return;
            }
            disk.path(key)
        };
        match write_segment(&path, audio) {
            Ok(()) => {
                if let Some(disk) = self.disk().as_mut() {
                    disk.used = disk.used.map(|used| used + bytes);
                }
            }
            Err(error) => log::warn!("couldn't keep a segment in the render cache: {error}"),
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<u64, Weak<Rendered>>> {
        // Nothing that holds a lock leaves what it guards half changed.
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn background(&self) -> MutexGuard<'_, Background> {
        self.background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn disk(&self) -> MutexGuard<'_, Option<Disk>> {
        self.disk.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Puts `jobs` in the order to render them in: the segments that the
/// playhead, at `playhead` in a song at `tempo`, plays next first, then
/// the ones it has passed, each in the order the song plays them.
pub(crate) fn order(jobs: &mut [Job], tempo: &TempoMap, playhead: Beats) {
    let when = |job: &Job| {
        let rate = job.plan.sample_rate();
        let segment = &job.plan.segments()[job.segment];
        let passed = (segment.end() as f64) <= tempo.sample_at(playhead, rate);
        (passed, segment.start() as f64 / f64::from(rate))
    };
    jobs.sort_by(|a, b| {
        let (a, b) = (when(a), when(b));
        a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
    });
}

/// A segment's key in the cache: its file's and its own.
fn key(file: FileKey, segment: u64) -> u64 {
    let mut hasher = SipHasher13::new();
    hasher.write_u64(file.0);
    hasher.write_u64(segment);
    hasher.finish()
}

/// Renders the segment `job` asks for, from its file's audio in memory, or
/// else from the file, as long as it hasn't changed since it was read.
fn render_from(job: &Job) -> io::Result<AudioData> {
    let segment = &job.plan.segments()[job.segment];
    if let Some(data) = job.file.whole.as_ref().and_then(Weak::upgrade) {
        return render(&job.plan, segment, &mut DataReader(data));
    }
    if FileKey::of(&job.file.path)? != job.file.key {
        return Err(io::Error::other("the file changed since it was read"));
    }
    let mut reader = FileReader(Decoder::open(&job.file.path)?);
    render(&job.plan, segment, &mut reader)
}

/// The render cache's folder, and how much it holds.
struct Disk {
    folder: PathBuf,
    limits: Limits,
    /// How many bytes its segments take, once it's looked.
    used: Option<u64>,
}

impl Disk {
    fn path(&self, key: u64) -> PathBuf {
        self.folder.join(format!("{key:016x}.{EXTENSION}"))
    }

    /// Makes room for `bytes` more: while the folder would hold more than
    /// it may, or leave less of the disk free than it must, the segments
    /// used longest ago go, until it's a little under, so the next ones
    /// needn't look again. Returns whether there's room.
    fn make_room(&mut self, bytes: u64) -> bool {
        if fs::create_dir_all(&self.folder).is_err() {
            return false;
        }
        let Limits { most, free: keep } = self.limits;
        let fits = |used: u64, free: Option<u64>, slack: u64| {
            used + bytes + slack <= most && free.is_none_or(|free| free >= keep + bytes + slack)
        };
        let free = free_space(&self.folder);
        if self.used.is_some_and(|used| fits(used, free, 0)) {
            return true;
        }
        let mut files = segments_in(&self.folder);
        let mut used: u64 = files.iter().map(|file| file.size).sum();
        let mut free = free;
        let slack = (most / 10).min(GB);
        files.sort_by_key(|file| file.used);
        for file in files {
            if fits(used, free, slack) {
                break;
            }
            if fs::remove_file(&file.path).is_ok() {
                used -= file.size;
                free = free.map(|free| free + file.size);
            }
        }
        self.used = Some(used);
        fits(used, free, 0)
    }
}

/// A segment's file in the folder.
struct Kept {
    path: PathBuf,
    size: u64,
    /// When it was last used: its modification time.
    used: SystemTime,
}

/// The segments' files in `folder`.
fn segments_in(folder: &Path) -> Vec<Kept> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let segment = |entry: fs::DirEntry| {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != EXTENSION)
        {
            return None;
        }
        let metadata = entry.metadata().ok()?;
        let used = metadata.modified().ok()?;
        Some(Kept {
            path,
            size: metadata.len(),
            used,
        })
    };
    entries.flatten().filter_map(segment).collect()
}

/// Writes `audio` to the segment file at `path`.
fn write_segment(path: &Path, audio: &AudioData) -> io::Result<()> {
    let frames = audio.frames();
    let channels = audio.channels();
    let mut bytes = Vec::with_capacity(HEADER + 4 * channels * frames as usize);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&FORMAT.to_le_bytes());
    bytes.extend_from_slice(&(channels as u32).to_le_bytes());
    bytes.extend_from_slice(&audio.sample_rate().to_le_bytes());
    bytes.extend_from_slice(&frames.to_le_bytes());
    for channel in 0..channels {
        for sample in audio.channel(channel) {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    write_whole(path, &bytes)
}

/// The segment in the file at `path`, if it's there and has `channels`
/// channels of `frames` frames at `rate`.
fn read_segment(path: &Path, channels: usize, rate: u32, frames: usize) -> Option<AudioData> {
    let bytes = fs::read(path).ok()?;
    let (header, samples) = bytes.split_at_checked(HEADER)?;
    let word = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let length = u64::from(word(16)) | u64::from(word(20)) << 32;
    let fits = header[..4] == MAGIC
        && word(4) == FORMAT
        && word(8) as usize == channels
        && word(12) == rate
        && length == frames as u64
        && frames > 0
        && samples.len() == 4 * channels * frames;
    if !fits {
        return None;
    }
    let channel = |bytes: &[u8]| {
        let (samples, _) = bytes.as_chunks::<4>();
        samples.iter().copied().map(f32::from_le_bytes).collect()
    };
    Some(AudioData::new(
        rate,
        samples.chunks_exact(4 * frames).map(channel).collect(),
    ))
}

/// Marks the file at `path` as used now, so it goes last.
fn touch(path: &Path) -> io::Result<()> {
    fs::File::options()
        .append(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

/// Writes `bytes` to the file at `path` whole: to a file of its own beside
/// it, then renamed to `path`, replacing what's there, so a reader never
/// sees half of it.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let mut part = path.as_os_str().to_owned();
    part.push(format!(
        ".{}-{}.part",
        std::process::id(),
        WRITES.fetch_add(1, Relaxed)
    ));
    fs::write(&part, bytes)?;
    fs::rename(&part, path).inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })
}

/// How much of the disk that holds `folder` is free for the user, in bytes,
/// if the system says.
#[cfg(unix)]
fn free_space(folder: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(folder.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a NUL-terminated string, and statvfs fills `stat`
    // when it returns 0.
    let stat = unsafe {
        if libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    // The counts are 64 bits on Linux and the free blocks 32 on macOS.
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn free_space(folder: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;

    let path: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0;
    // SAFETY: `path` is a NUL-terminated wide string that outlives the call,
    // and `free` a u64 for it to fill.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(not(any(unix, windows)))]
fn free_space(_: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::time::Duration;

    use musicable_engine::clips::spectral::SegmentPlan;
    use musicable_io::audio::{SampleFormat, save_wav};
    use musicable_library::Queue;
    use musicable_model::clip::{AudioClip, Clip, ClipContent, ClipId, Offset};
    use musicable_model::time::TimeSignature;
    use musicable_model::warp::{Warp, WarpMode};

    use super::*;

    const RATE: u32 = 48_000;
    /// The test file's length: 20 seconds.
    const FRAMES: u64 = 20 * RATE as u64;

    /// A folder of its own for each test, empty.
    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("musicable-renders-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Writes `frames` frames of a chord to a stereo file at `path`.
    fn write_frames(path: &Path, frames: u64) {
        let tone = |f: f64, i: u64| (TAU * f * i as f64 / f64::from(RATE)).sin() as f32;
        let left: Vec<f32> = (0..frames)
            .map(|i| 0.2 * (tone(220.0, i) + tone(277.2, i)))
            .collect();
        let right: Vec<f32> = (0..frames).map(|i| 0.3 * tone(330.0, i)).collect();
        save_wav(path, RATE, SampleFormat::Float32, &[&left, &right]).unwrap();
    }

    /// The test file, written to `path`, as the render cache knows it.
    fn write(path: &Path) -> FileRef {
        write_frames(path, FRAMES);
        FileRef {
            key: FileKey::of(path).unwrap(),
            path: path.to_owned(),
            whole: None,
        }
    }

    /// A song at 120 BPM.
    fn song() -> TempoMap {
        TempoMap::new(120.0, TimeSignature::FOUR_FOUR)
    }

    /// A clip from beat `start`, `length` beats long, of the test file
    /// warped in Spectral at 100 BPM, so it plays faster than the file.
    fn clip(start: i64, length: i64) -> Clip {
        let mut audio = AudioClip::new("chord.wav");
        let mut warp = Warp::new(0, Beats::ZERO, 100.0);
        warp.mode = WarpMode::Spectral;
        audio.warp = Some(warp);
        audio.offset = Offset::Beats(Beats::ZERO);
        Clip {
            id: ClipId(1),
            name: String::new(),
            start: Beats::new(start),
            length: Beats::new(length),
            content: ClipContent::Audio(audio),
        }
    }

    fn plan(clip: &Clip) -> Plan {
        Plan::new(clip, &song(), 2, RATE, FRAMES).expect("a clip in Spectral")
    }

    /// Where each of `jobs`' segments starts.
    fn starts(jobs: &[Job]) -> Vec<i64> {
        let start = |job: &Job| job.plan.segments()[job.segment].start();
        jobs.iter().map(start).collect()
    }

    fn rendered(renders: &Renders) -> usize {
        renders.slots.iter().filter(|slot| slot.is_ready()).count()
    }

    /// Marks the file at `path` as last used at `when`.
    fn used_at(path: &Path, when: SystemTime) {
        let file = fs::File::options().append(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn segments_render_in_the_background_and_graphs_share_them() {
        let folder = folder("background");
        let file = write(&folder.join("chord.wav"));
        let cache = RenderCache::new();
        let mut jobs = Vec::new();
        let renders = cache.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        let count = renders.slots.len();
        assert!(count >= 3, "{count} segments");
        assert_eq!(jobs.len(), count);
        // A copy of the clip later in the song plays the same segments, and
        // shares them.
        let mut more = Vec::new();
        let copy = cache.plan(&file, plan(&clip(64, 32)), false, &mut more);
        assert_eq!(more.len(), 0);
        let mut shared = renders.slots.iter().zip(copy.slots.iter());
        assert!(shared.all(|(a, b)| Arc::ptr_eq(a, b)));

        // Asked for before there's a queue, they wait for one.
        cache.render_later(jobs);
        assert_eq!((cache.pending(), rendered(&renders)), (count, 0));
        let queue = Queue::new(2);
        let woken = Arc::new(AtomicUsize::new(0));
        let waker: Waker = {
            let woken = woken.clone();
            Arc::new(move || {
                woken.fetch_add(1, Relaxed);
            })
        };
        cache.start(queue.adder(), Some(waker));
        queue.wait();
        assert!(renders.is_ready() && copy.is_ready());
        assert_eq!((cache.pending(), woken.load(Relaxed)), (0, count));
        // Each is what the engine renders of it, to the bit.
        let decoder = Decoder::open(&file.path).unwrap();
        let data = Arc::new(AudioData::read(&mut FileReader(decoder)).unwrap());
        for (segment, slot) in renders.plan.segments().iter().zip(renders.slots.iter()) {
            let expected = render(&renders.plan, segment, &mut DataReader(data.clone())).unwrap();
            assert!(slot.get() == Some(&expected));
        }
    }

    #[test]
    fn the_folder_keeps_segments_for_next_time() {
        let folder = folder("kept");
        let file = write(&folder.join("chord.wav"));
        let kept = folder.join("renders");
        let limits = Limits {
            most: 20 * GB,
            free: 0,
        };
        let first = RenderCache::new();
        first.set_folder(Some(kept.clone()), limits);
        assert_eq!(first.folder(), Some(kept.clone()));
        let mut jobs = Vec::new();
        let renders = first.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        first.render_now(jobs, &AtomicBool::new(false));
        assert!(renders.is_ready());
        let files = segments_in(&kept);
        assert_eq!(files.len(), renders.slots.len());
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(86_400);
        for file in &files {
            used_at(&file.path, long_ago);
        }

        // The next time the app runs, they come from the folder: the file
        // they'd render from is gone.
        fs::remove_file(&file.path).unwrap();
        let next = RenderCache::new();
        next.set_folder(Some(kept.clone()), limits);
        let mut jobs = Vec::new();
        let again = next.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        assert_eq!(jobs.len(), again.slots.len());
        next.render_now(jobs, &AtomicBool::new(false));
        assert!(again.is_ready());
        let mut same = renders.slots.iter().zip(again.slots.iter());
        assert!(same.all(|(a, b)| a.get() == b.get()));
        // Reading them counts as using them.
        assert!(segments_in(&kept).iter().all(|file| file.used > long_ago));
    }

    #[test]
    fn the_folder_lets_go_of_the_segments_used_longest_ago() {
        let folder = folder("limits");
        let day = |n: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(n * 86_400);
        for (name, days) in [("b", 2), ("a", 1), ("c", 3)] {
            let path = folder.join(format!("{name}.{EXTENSION}"));
            fs::write(&path, [0; 300]).unwrap();
            used_at(&path, day(days));
        }
        fs::write(folder.join("notes.txt"), [0; 300]).unwrap();
        let mut disk = Disk {
            folder: folder.clone(),
            limits: Limits {
                most: 1_000,
                free: 0,
            },
            used: None,
        };
        // It holds 900 bytes, and 200 more are coming: the one used longest
        // ago goes, which leaves room to spare.
        assert!(disk.make_room(200));
        assert_eq!(names(&folder), ["b.render", "c.render", "notes.txt"]);
        assert_eq!(disk.used, Some(600));
        assert!(disk.make_room(200));
        assert_eq!(names(&folder).len(), 3);
        // Holding less from now on, it lets go now.
        disk.limits.most = 400;
        assert!(disk.make_room(0));
        assert_eq!(names(&folder), ["c.render", "notes.txt"]);
        // Where the disk can't leave as much free as it should, it keeps
        // nothing.
        disk.limits = Limits {
            most: u64::MAX / 4,
            free: u64::MAX / 4,
        };
        assert!(!disk.make_room(100));
        assert_eq!(names(&folder), ["notes.txt"]);
    }

    #[test]
    fn a_damaged_segment_file_isnt_read() {
        let folder = folder("damaged");
        let path = folder.join(format!("segment.{EXTENSION}"));
        let audio = AudioData::new(RATE, vec![vec![0.25, -0.5, 1.0], vec![0.0, 0.125, -1.0]]);
        write_segment(&path, &audio).unwrap();
        assert!(read_segment(&path, 2, RATE, 3) == Some(audio));
        // Not the segment asked for.
        assert!(read_segment(&path, 1, RATE, 3).is_none());
        assert!(read_segment(&path, 2, 44_100, 3).is_none());
        assert!(read_segment(&path, 2, RATE, 4).is_none());
        // Cut short, from a newer version, not a segment, or not there.
        let bytes = fs::read(&path).unwrap();
        let mut newer = bytes.clone();
        newer[4] = 2;
        let mut other = bytes.clone();
        other[0] = b'R';
        for damaged in [&bytes[..bytes.len() - 1], &newer[..], &other[..], &[]] {
            fs::write(&path, damaged).unwrap();
            assert!(read_segment(&path, 2, RATE, 3).is_none());
        }
        assert!(read_segment(&folder.join("missing.render"), 2, RATE, 3).is_none());
    }

    #[test]
    fn segments_nothing_wants_dont_render() {
        let folder = folder("unwanted");
        let file = write(&folder.join("chord.wav"));
        let kept = folder.join("renders");
        let cache = RenderCache::new();
        let limits = Limits {
            most: 20 * GB,
            free: 0,
        };
        cache.set_folder(Some(kept.clone()), limits);
        // Stopped, as when its session goes.
        let mut jobs = Vec::new();
        let renders = cache.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        cache.render_now(jobs, &AtomicBool::new(true));
        assert_eq!(rendered(&renders), 0);
        drop(renders);
        cache.prune();
        assert!(cache.slots().is_empty());
        // Let go of before they render.
        let mut jobs = Vec::new();
        drop(cache.plan(&file, plan(&clip(0, 32)), false, &mut jobs));
        assert!(!jobs.is_empty());
        cache.render_now(jobs, &AtomicBool::new(false));
        // From a file that changed since it was read.
        let mut jobs = Vec::new();
        let renders = cache.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        write_frames(&file.path, FRAMES + 1);
        cache.render_now(jobs, &AtomicBool::new(false));
        assert_eq!(rendered(&renders), 0);
        assert_eq!(segments_in(&kept).len(), 0);
    }

    #[test]
    fn the_segments_the_playhead_plays_next_render_first() {
        let folder = folder("order");
        let file = write(&folder.join("chord.wav"));
        let cache = RenderCache::new();
        let mut jobs = Vec::new();
        let renders = cache.plan(&file, plan(&clip(0, 32)), false, &mut jobs);
        let segments = renders.plan.segments();
        let all: Vec<i64> = segments.iter().map(SegmentPlan::start).collect();
        jobs.reverse();
        order(&mut jobs, &song(), Beats::ZERO);
        assert_eq!(starts(&jobs), all);
        // From the middle of the second segment on, the first comes last.
        let middle = segments[1].start() + segments[1].frames() as i64 / 2;
        let playhead = Beats::from_f64(middle as f64 / f64::from(RATE) * 2.0);
        order(&mut jobs, &song(), playhead);
        let mut later = all[1..].to_vec();
        later.push(all[0]);
        assert_eq!(starts(&jobs), later);
    }
}

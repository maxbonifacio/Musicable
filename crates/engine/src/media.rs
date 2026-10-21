//! Audio files for audio clips: in memory, or streaming from disk.
//!
//! A clip player reads a file's audio from a [`Source`]. Short files load
//! whole ([`AudioData`]). Longer ones stream ([`Stream`]): a disk thread (the
//! [`Streamer`]'s) keeps the stretch of the file a clip plays next in memory,
//! and the stretch it plays after the loop jumps back, in a fixed set of
//! chunks that the audio thread copies from without locking or allocating.
//! The player says where it's about to read every block. [`Media`] holds a
//! project's sources by clip, for
//! [`compile_with_media`](crate::compile::compile_with_media).
//!
//! The engine doesn't read files itself: the disk thread reads through
//! [`AudioReader`], which the app implements with `musicable-io`'s decoders.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock, Weak};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use musicable_model::clip::ClipId;

/// Random access to an audio file's frames, for the disk thread.
pub trait AudioReader: Send {
    fn channels(&self) -> usize;

    fn sample_rate(&self) -> u32;

    /// The file's length, in frames.
    fn frames(&self) -> u64;

    /// Reads frames from frame `start` on into `out`, interleaved, as many
    /// as fit in it or as the file has left, and returns how many.
    fn read(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize>;
}

/// A whole audio file in memory, a channel at a time.
#[derive(Clone, PartialEq)]
pub struct AudioData {
    sample_rate: u32,
    channels: Vec<Vec<f32>>,
}

impl AudioData {
    /// # Panics
    ///
    /// If there are no channels, the channels aren't all the same length,
    /// or the sample rate is zero.
    pub fn new(sample_rate: u32, channels: Vec<Vec<f32>>) -> Self {
        assert!(sample_rate > 0, "audio needs a sample rate");
        assert!(!channels.is_empty(), "audio needs a channel");
        assert!(
            channels.iter().all(|c| c.len() == channels[0].len()),
            "the channels differ in length"
        );
        Self {
            sample_rate,
            channels,
        }
    }

    /// Reads all of `reader`'s audio.
    pub fn read(reader: &mut dyn AudioReader) -> io::Result<Self> {
        let count = reader.channels();
        let frames = usize::try_from(reader.frames()).map_err(|_| io::ErrorKind::OutOfMemory)?;
        if count == 0 || reader.sample_rate() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the audio has no channels or no sample rate",
            ));
        }
        let mut channels: Vec<Vec<f32>> = (0..count).map(|_| Vec::with_capacity(frames)).collect();
        let mut buffer = vec![0.0; count * 8192];
        let mut at = 0;
        while at < frames {
            let read = reader.read(at as u64, &mut buffer)?;
            if read == 0 {
                break;
            }
            for frame in buffer[..read * count].chunks_exact(count) {
                for (channel, &sample) in channels.iter_mut().zip(frame) {
                    channel.push(sample);
                }
            }
            at += read;
        }
        Ok(Self::new(reader.sample_rate(), channels))
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    pub fn frames(&self) -> u64 {
        self.channels[0].len() as u64
    }

    pub fn channel(&self, index: usize) -> &[f32] {
        &self.channels[index]
    }
}

impl fmt::Debug for AudioData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioData")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels())
            .field("frames", &self.frames())
            .finish()
    }
}

/// Where a clip player reads a file's audio from.
#[derive(Debug, Clone)]
pub enum Source {
    /// The whole file, in memory.
    Whole(Arc<AudioData>),
    /// The file, streaming from disk.
    Stream(Arc<Stream>),
}

impl Source {
    pub fn channels(&self) -> usize {
        match self {
            Self::Whole(data) => data.channels(),
            Self::Stream(stream) => stream.channels,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::Whole(data) => data.sample_rate(),
            Self::Stream(stream) => stream.sample_rate,
        }
    }

    pub fn frames(&self) -> u64 {
        match self {
            Self::Whole(data) => data.frames(),
            Self::Stream(stream) => stream.frames,
        }
    }
}

/// The audio each audio clip plays, by clip. A clip without a source, such
/// as one whose file is missing, is silent.
#[derive(Debug, Clone, Default)]
pub struct Media {
    sources: HashMap<ClipId, Source>,
}

impl Media {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, clip: ClipId, source: Source) {
        self.sources.insert(clip, source);
    }

    pub fn get(&self, clip: ClipId) -> Option<&Source> {
        self.sources.get(&clip)
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Frames in a chunk, the piece of a file the disk thread reads at once:
/// about a sixth of a second at 48 kHz.
pub const CHUNK: u64 = 8192;

/// How much of a file a stream keeps in memory from where its clip reads
/// next, in seconds of the file.
const AHEAD_SECONDS: f64 = 1.0;
/// And from where the clip reads after the loop jumps back.
const LOOP_SECONDS: f64 = 0.4;

/// No position: what a stream holds while its clip has none to give.
const NOWHERE: u64 = u64::MAX;

// A chunk slot's state is the chunk it holds, shifted up two bits, and one
// of these.
const EMPTY: u64 = 0;
/// The disk thread is writing the chunk.
const LOADING: u64 = 1;
/// The chunk is ready to read.
const READY: u64 = 2;
/// A player is copying from the chunk.
const READING: u64 = 3;

/// A file's audio, streaming from disk: the part of it a clip plays next,
/// and the part it plays after the loop jumps back, in fixed sets of chunks.
/// Made by [`Streamer::stream`].
///
/// The player [hints](Stream::hint) every block where it's about to read,
/// and [reads](Stream::read) on the audio thread, copying from chunks that
/// are loaded. A chunk that isn't is silent and counts as
/// [missed](Stream::missed), unless the stream waits for the disk, as it
/// does when rendering offline.
pub struct Stream {
    channels: usize,
    sample_rate: u32,
    frames: u64,
    /// The chunks from where the clip reads next.
    ahead: Box<[Slot]>,
    /// The chunks from where it reads after the loop jumps back.
    after_loop: Box<[Slot]>,
    /// The first frame the clip reads next: where it is while it plays, or
    /// where it starts. [`NOWHERE`] before the first hint.
    next: AtomicU64,
    /// The first frame it reads after the loop jumps back, or [`NOWHERE`].
    loop_next: AtomicU64,
    /// Whether reads wait for chunks to load rather than miss them.
    wait: bool,
    missed: AtomicU64,
    failed: AtomicBool,
    disk: Arc<Disk>,
}

/// A chunk's worth of a file, a channel after another.
struct Slot {
    /// The chunk it holds, shifted up two bits, and its state.
    state: AtomicU64,
    data: UnsafeCell<Box<[f32]>>,
}

// SAFETY: a slot's data is only written by the disk thread, after it moves
// the slot to LOADING, and only read by a player that moved it from READY to
// READING, so nobody reads data while it's written. The atomic state passes
// the data between them: the disk thread's store of READY releases what it
// wrote to the reader's acquire, and the reader's store of READY releases its
// reads before the disk thread acquires the slot to load into it again.
unsafe impl Sync for Stream {}

impl Slot {
    fn new(samples: usize) -> Self {
        Self {
            state: AtomicU64::new(EMPTY),
            data: UnsafeCell::new(vec![0.0; samples].into_boxed_slice()),
        }
    }
}

impl Stream {
    fn new(channels: usize, sample_rate: u32, frames: u64, wait: bool, disk: Arc<Disk>) -> Self {
        let chunks =
            |seconds: f64| (seconds * f64::from(sample_rate) / CHUNK as f64).ceil() as usize;
        let slots = |count: usize| {
            (0..count)
                .map(|_| Slot::new(channels * CHUNK as usize))
                .collect()
        };
        Self {
            channels,
            sample_rate,
            frames,
            // A read can start anywhere in the first chunk, so the second
            // chunk past the window's length still counts.
            ahead: slots(chunks(AHEAD_SECONDS) + 1),
            after_loop: slots(chunks(LOOP_SECONDS) + 1),
            next: AtomicU64::new(NOWHERE),
            loop_next: AtomicU64::new(NOWHERE),
            wait,
            missed: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            disk,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Reads that found their chunk not loaded and played silence.
    pub fn missed(&self) -> u64 {
        self.missed.load(Relaxed)
    }

    /// Whether reading the file failed. The parts that failed play silence.
    pub fn failed(&self) -> bool {
        self.failed.load(Relaxed)
    }

    /// Whether the chunks a clip needs to read `frames` frames from `start`
    /// are in memory.
    pub fn is_loaded(&self, start: u64, frames: u64) -> bool {
        let end = (start + frames).min(self.frames);
        let mut at = start;
        while at < end {
            let chunk = at / CHUNK;
            let ready = |slots: &[Slot]| {
                slots[(chunk % slots.len() as u64) as usize]
                    .state
                    .load(Acquire)
                    == chunk << 2 | READY
            };
            if !ready(&self.ahead) && !ready(&self.after_loop) {
                return false;
            }
            at = (chunk + 1) * CHUNK;
        }
        true
    }

    /// Tells the disk thread where the clip reads next: from frame `next`,
    /// and from `after_loop` once the loop jumps back, if it's going to.
    /// Players call it every block, on the audio thread. Real-time safe.
    pub fn hint(&self, next: u64, after_loop: Option<u64>) {
        let after_loop = after_loop.unwrap_or(NOWHERE);
        let moved =
            |old: u64, new: u64| old == NOWHERE || new == NOWHERE || old / CHUNK != new / CHUNK;
        let before = self.next.swap(next, Relaxed);
        let loop_before = self.loop_next.swap(after_loop, Relaxed);
        if moved(before, next) || (after_loop != loop_before && moved(loop_before, after_loop)) {
            self.disk.wake();
        }
    }

    /// Copies frames from frame `start` of the file into `out`, a channel
    /// to each slice, as many as the slices are long. Frames past the end of
    /// the file are silent, and so are chunks that aren't loaded, unless the
    /// stream waits for the disk. Slices beyond the file's channels are left
    /// alone. Returns whether every chunk was there. Real-time safe, unless
    /// it waits.
    pub fn read(&self, start: u64, out: &mut [&mut [f32]]) -> bool {
        let channels = out.len().min(self.channels);
        let out = &mut out[..channels];
        let frames = out.first().map_or(0, |c| c.len());
        let mut all = true;
        let mut done = 0;
        while done < frames {
            let at = start + done as u64;
            if at >= self.frames {
                for channel in out.iter_mut() {
                    channel[done..].fill(0.0);
                }
                break;
            }
            let chunk = at / CHUNK;
            let within = (at % CHUNK) as usize;
            let count = (CHUNK as usize - within).min(frames - done);
            let copied = self.with_chunk(chunk, |data| {
                for (c, channel) in out.iter_mut().enumerate() {
                    let from = &data[c * CHUNK as usize + within..][..count];
                    channel[done..done + count].copy_from_slice(from);
                }
            });
            if !copied {
                all = false;
                for channel in out.iter_mut() {
                    channel[done..done + count].fill(0.0);
                }
            }
            done += count;
        }
        all
    }

    /// Hands `chunk`'s data to `copy`, from whichever set of slots holds it,
    /// and returns whether one did. Waits for it to load if the stream waits.
    fn with_chunk(&self, chunk: u64, mut copy: impl FnMut(&[f32])) -> bool {
        let ready = chunk << 2 | READY;
        loop {
            for slots in [&self.ahead, &self.after_loop] {
                let slot = &slots[(chunk % slots.len() as u64) as usize];
                if slot
                    .state
                    .compare_exchange(ready, chunk << 2 | READING, Acquire, Relaxed)
                    .is_ok()
                {
                    // SAFETY: the slot is READING, so the disk thread leaves
                    // its data alone until it's READY again.
                    let data: &[f32] = unsafe { &*slot.data.get() };
                    copy(data);
                    slot.state.store(ready, Release);
                    return true;
                }
            }
            self.disk.wake();
            if !self.wait || !self.disk.alive.load(Acquire) {
                self.missed.fetch_add(1, Relaxed);
                return false;
            }
            thread::yield_now();
        }
    }

    /// Loads the chunk of each window `depth` chunks in, if it isn't loaded,
    /// and returns whether it loaded one. Runs on the disk thread.
    fn load(&self, depth: usize, reader: &mut dyn AudioReader, scratch: &mut Vec<f32>) -> bool {
        let mut loaded = false;
        for (slots, hint) in [
            (&self.ahead, &self.next),
            (&self.after_loop, &self.loop_next),
        ] {
            let hint = hint.load(Relaxed);
            if hint == NOWHERE || depth >= slots.len() {
                continue;
            }
            let chunk = hint / CHUNK + depth as u64;
            if chunk * CHUNK >= self.frames {
                continue;
            }
            let slot = &slots[(chunk % slots.len() as u64) as usize];
            let state = slot.state.load(Acquire);
            if state == chunk << 2 | READY || state & 3 == READING {
                // Loaded already, or a player is still copying the chunk it
                // holds: try again next round.
                continue;
            }
            if slot
                .state
                .compare_exchange(state, chunk << 2 | LOADING, Acquire, Relaxed)
                .is_err()
            {
                continue;
            }
            // SAFETY: the slot is LOADING, so no player reads its data.
            let data = unsafe { &mut *slot.data.get() };
            if self.fill(chunk, reader, scratch, data).is_err() {
                data.fill(0.0);
                self.failed.store(true, Relaxed);
            }
            slot.state.store(chunk << 2 | READY, Release);
            loaded = true;
        }
        loaded
    }

    /// Reads `chunk` into `data`, a channel after another. What the file
    /// doesn't have is silent.
    fn fill(
        &self,
        chunk: u64,
        reader: &mut dyn AudioReader,
        scratch: &mut Vec<f32>,
        data: &mut [f32],
    ) -> io::Result<()> {
        let channels = self.channels;
        let length = CHUNK as usize;
        scratch.resize(channels * length, 0.0);
        let mut done = 0;
        while done < length {
            let read = reader.read(chunk * CHUNK + done as u64, &mut scratch[done * channels..])?;
            if read == 0 {
                break;
            }
            done += read;
        }
        for (frame, samples) in scratch[..done * channels]
            .chunks_exact(channels)
            .enumerate()
        {
            for (c, &sample) in samples.iter().enumerate() {
                data[c * length + frame] = sample;
            }
        }
        for c in 0..channels {
            data[c * length + done..(c + 1) * length].fill(0.0);
        }
        Ok(())
    }
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("frames", &self.frames)
            .field("missed", &self.missed())
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}

/// What the disk thread shares with its streams.
struct Disk {
    thread: OnceLock<Thread>,
    /// Cleared when the disk thread stops, so waiting reads give up.
    alive: AtomicBool,
    stop: AtomicBool,
}

impl Disk {
    /// Wakes the disk thread to load what's wanted now. Real-time safe.
    fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}

/// Reads streams' audio from disk ahead of their players, on a thread of
/// its own, `musicable-disk`. Each stream keeps its chunks while anything
/// holds it; the disk thread forgets streams nobody holds.
pub struct Streamer {
    disk: Arc<Disk>,
    streams: Sender<(Weak<Stream>, Box<dyn AudioReader>)>,
    thread: Option<JoinHandle<()>>,
    wait: bool,
}

impl Streamer {
    /// Starts the disk thread. With `wait`, players wait for chunks that
    /// aren't loaded yet instead of playing silence where they are: for
    /// rendering offline, faster than real time.
    ///
    /// # Panics
    ///
    /// If the operating system can't start a thread.
    pub fn spawn(wait: bool) -> Self {
        let disk = Arc::new(Disk {
            thread: OnceLock::new(),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let (streams, added) = mpsc::channel();
        let shared = disk.clone();
        let thread = thread::Builder::new()
            .name("musicable-disk".into())
            .spawn(move || {
                run(&shared, added);
                shared.alive.store(false, Release);
            })
            .expect("failed to start the disk thread");
        let _ = disk.thread.set(thread.thread().clone());
        Self {
            disk,
            streams,
            thread: Some(thread),
            wait,
        }
    }

    /// Whether players of this streamer's streams wait for the disk.
    pub fn waits(&self) -> bool {
        self.wait
    }

    /// A stream of `reader`'s audio. The disk thread starts loading the
    /// file's start once the stream's player first hints where it reads.
    pub fn stream(&self, reader: Box<dyn AudioReader>) -> Arc<Stream> {
        let stream = Arc::new(Stream::new(
            reader.channels().max(1),
            reader.sample_rate().max(1),
            reader.frames(),
            self.wait,
            self.disk.clone(),
        ));
        // Only fails if the disk thread is gone, which reads find out.
        let _ = self.streams.send((Arc::downgrade(&stream), reader));
        self.disk.wake();
        stream
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        self.disk.stop.store(true, Relaxed);
        self.disk.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl fmt::Debug for Streamer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Streamer")
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

/// The disk thread: loads what each stream wants, the most urgent chunks of
/// every stream first, and sleeps when there's nothing to load.
fn run(disk: &Disk, added: Receiver<(Weak<Stream>, Box<dyn AudioReader>)>) {
    let mut streams: Vec<(Weak<Stream>, Box<dyn AudioReader>)> = Vec::new();
    let mut scratch = Vec::new();
    while !disk.stop.load(Relaxed) {
        streams.extend(added.try_iter());
        streams.retain(|(stream, _)| stream.strong_count() > 0);
        let depth = streams
            .iter()
            .filter_map(|(stream, _)| stream.upgrade())
            .map(|s| s.ahead.len().max(s.after_loop.len()))
            .max()
            .unwrap_or(0);
        let mut loaded = false;
        'deeper: for d in 0..depth {
            for (stream, reader) in &mut streams {
                if let Some(stream) = stream.upgrade() {
                    loaded |= stream.load(d, reader.as_mut(), &mut scratch);
                }
                if disk.stop.load(Relaxed) {
                    break 'deeper;
                }
            }
        }
        if !loaded {
            thread::park_timeout(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of `frames` frames in memory, each sample its frame number
    /// plus a tenth of its channel number.
    pub(crate) struct Ramp {
        pub channels: usize,
        pub frames: u64,
        pub sample_rate: u32,
    }

    impl AudioReader for Ramp {
        fn channels(&self) -> usize {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn frames(&self) -> u64 {
            self.frames
        }

        fn read(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
            let frames =
                (out.len() / self.channels).min(self.frames.saturating_sub(start) as usize);
            for (i, frame) in out.chunks_exact_mut(self.channels).take(frames).enumerate() {
                for (c, sample) in frame.iter_mut().enumerate() {
                    *sample = (start + i as u64) as f32 + c as f32 / 10.0;
                }
            }
            Ok(frames)
        }
    }

    fn ramp(frames: u64) -> Box<Ramp> {
        Box::new(Ramp {
            channels: 2,
            frames,
            sample_rate: 48_000,
        })
    }

    #[test]
    fn whole_files_read_into_channels() {
        let data = AudioData::read(ramp(20_000).as_mut()).unwrap();
        assert_eq!(
            (data.channels(), data.frames(), data.sample_rate()),
            (2, 20_000, 48_000)
        );
        assert_eq!(data.channel(0)[19_999], 19_999.0);
        assert_eq!(data.channel(1)[3], 3.1);
    }

    #[test]
    fn waiting_streams_read_every_frame() {
        let streamer = Streamer::spawn(true);
        let stream = streamer.stream(ramp(100_000));
        let (mut left, mut right) = (vec![0.0; 3000], vec![0.0; 3000]);
        for start in [0, 2000, 9000, 50_000, 98_000] {
            stream.hint(start, None);
            assert!(stream.read(start, &mut [&mut left, &mut right]));
            for i in 0..3000 {
                let frame = start + i as u64;
                let expected = if frame < 100_000 { frame as f32 } else { 0.0 };
                assert_eq!(left[i], expected, "frame {frame}");
                let expected = if frame < 100_000 { expected + 0.1 } else { 0.0 };
                assert_eq!(right[i], expected, "frame {frame}");
            }
        }
        assert_eq!(stream.missed(), 0);
    }

    #[test]
    fn real_time_streams_miss_what_isnt_loaded_and_load_it() {
        let streamer = Streamer::spawn(false);
        let stream = streamer.stream(ramp(1_000_000));
        let mut out = vec![1.0; 100];
        // Nothing's loaded before the first hint.
        assert!(!stream.read(500_000, &mut [&mut out]));
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(stream.missed() > 0);
        stream.hint(500_000, Some(0));
        while !(stream.is_loaded(500_000, 40_000) && stream.is_loaded(0, 16_000)) {
            thread::yield_now();
        }
        assert!(stream.read(500_000, &mut [&mut out]));
        assert_eq!(out[0], 500_000.0);
        // The loop's start is there too.
        assert!(stream.read(0, &mut [&mut out]));
        assert_eq!(out[99], 99.0);
    }

    #[test]
    fn reads_past_the_end_are_silent() {
        let streamer = Streamer::spawn(true);
        let stream = streamer.stream(ramp(10));
        stream.hint(0, None);
        let mut out = vec![1.0; 10];
        assert!(stream.read(5, &mut [&mut out]));
        assert_eq!(out[..5], [5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(out[5..], [0.0; 5]);
    }

    /// Fails every read.
    struct Broken;

    impl AudioReader for Broken {
        fn channels(&self) -> usize {
            1
        }

        fn sample_rate(&self) -> u32 {
            48_000
        }

        fn frames(&self) -> u64 {
            100
        }

        fn read(&mut self, _start: u64, _out: &mut [f32]) -> io::Result<usize> {
            Err(io::Error::other("the disk is gone"))
        }
    }

    #[test]
    fn failed_reads_are_silent_and_reported() {
        let streamer = Streamer::spawn(true);
        let stream = streamer.stream(Box::new(Broken));
        stream.hint(0, None);
        let mut out = vec![1.0; 50];
        assert!(stream.read(0, &mut [&mut out]));
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(stream.failed());
    }

    #[test]
    fn waiting_reads_give_up_when_the_disk_thread_stops() {
        let streamer = Streamer::spawn(true);
        let stream = streamer.stream(ramp(100_000));
        drop(streamer);
        let mut out = vec![1.0; 100];
        assert!(!stream.read(0, &mut [&mut out]));
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

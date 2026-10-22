//! An input's audio on the engine's timeline, written by the thread that
//! lines the input up and read, without locks, by the output stream and
//! anything else that records.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, fence};

/// An input's audio, lined up with the output: frame `w` holds what the
/// input captured as the output's engine frame `w` was heard. That's where
/// a recording of it belongs, and the output plays it a fixed delay later
/// to monitor it.
///
/// One thread writes the newest frames, and any number of threads read them
/// at once, without locks or waiting: the newest
/// [`capacity`](Self::capacity) frames are there to read. Cloning is cheap,
/// and every clone reads the same audio. Reading is real-time safe.
#[derive(Clone)]
pub struct InputBuffer {
    ring: Arc<Ring>,
}

/// Why frames couldn't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Some of them haven't been captured yet.
    NotYet,
    /// Some of them are older than the buffer holds, or from before the
    /// input started.
    Gone,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotYet => "the input hasn't captured those frames yet",
            Self::Gone => "the input buffer no longer holds those frames",
        })
    }
}

impl std::error::Error for ReadError {}

struct Ring {
    channels: usize,
    /// Frames per channel, a power of two.
    capacity: u64,
    /// Planar: channel `c`'s frame `w` is at `c * capacity + w % capacity`.
    /// Samples are stored as their bits, so a read racing a write is no data
    /// race, and the read is thrown away.
    samples: Box<[AtomicU32]>,
    /// Whether anything has been written yet.
    begun: AtomicBool,
    /// Whether the writer is gone: nothing more will be written.
    ended: AtomicBool,
    /// The first frame written.
    start: AtomicU64,
    /// Just after the last frame written: frames can be read up to here.
    end: AtomicU64,
    /// Just after the last frame the writer has started writing: ahead of
    /// `end` while it writes. A frame `capacity` or more before it may have
    /// been overwritten.
    claim: AtomicU64,
}

impl InputBuffer {
    /// A buffer of `channels` channels holding at least `frames` frames,
    /// and its one writer. Allocates.
    pub(crate) fn new(channels: usize, frames: u64) -> (InputBuffer, InputWriter) {
        let capacity = frames.max(1).next_power_of_two();
        let samples = (0..channels as u64 * capacity)
            .map(|_| AtomicU32::new(0))
            .collect();
        let ring = Arc::new(Ring {
            channels,
            capacity,
            samples,
            begun: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
            claim: AtomicU64::new(0),
        });
        (
            InputBuffer { ring: ring.clone() },
            InputWriter {
                ring,
                silence: vec![0.0; InputWriter::SILENCE * channels],
            },
        )
    }

    pub fn channels(&self) -> usize {
        self.ring.channels
    }

    /// The most frames the buffer holds: how far behind the newest frame a
    /// reader can be.
    pub fn capacity(&self) -> u64 {
        self.ring.capacity
    }

    /// Whether the input has stopped: nothing more will be written. What was
    /// written can still be read, until the buffer is dropped.
    pub fn has_ended(&self) -> bool {
        self.ring.ended.load(Relaxed)
    }

    /// The engine frames that can be read now, from the oldest the buffer
    /// still holds to the newest captured. Empty until the input has lined
    /// up with the output.
    pub fn frames(&self) -> Range<u64> {
        let ring = &*self.ring;
        let end = ring.end.load(Acquire);
        if !ring.begun.load(Relaxed) {
            return 0..0;
        }
        let start = ring.start.load(Relaxed);
        let oldest = start.max(end.saturating_sub(ring.capacity));
        oldest..end
    }

    /// Reads frames `from..from + n` of every channel into `out`,
    /// interleaved, where `out` holds `n` frames. Real-time safe.
    pub fn read(&self, from: u64, out: &mut [f32]) -> Result<(), ReadError> {
        let ring = &*self.ring;
        let channels = ring.channels;
        assert!(
            channels > 0 && out.len().is_multiple_of(channels),
            "{} samples don't split into {channels} channels",
            out.len()
        );
        let frames = (out.len() / channels) as u64;
        self.check(from, frames)?;
        for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
            let slot = ring.slot(from + i as u64);
            for (c, sample) in frame.iter_mut().enumerate() {
                *sample = ring.load(c, slot);
            }
        }
        self.recheck(from)
    }

    /// Reads frames `from..from + out.len()` of channel `channel` into
    /// `out`. Real-time safe.
    pub fn read_channel(
        &self,
        from: u64,
        channel: usize,
        out: &mut [f32],
    ) -> Result<(), ReadError> {
        let ring = &*self.ring;
        assert!(
            channel < ring.channels,
            "no channel {channel} in {}",
            ring.channels
        );
        self.check(from, out.len() as u64)?;
        for (i, sample) in out.iter_mut().enumerate() {
            *sample = ring.load(channel, ring.slot(from + i as u64));
        }
        self.recheck(from)
    }

    /// Whether frames `from..from + frames` are there to read.
    fn check(&self, from: u64, frames: u64) -> Result<(), ReadError> {
        let range = self.frames();
        if from + frames > range.end {
            Err(ReadError::NotYet)
        } else if from < range.start {
            Err(ReadError::Gone)
        } else {
            Ok(())
        }
    }

    /// Whether frames from `from` on, just read, could have been overwritten
    /// while they were read.
    fn recheck(&self, from: u64) -> Result<(), ReadError> {
        // Pairs with the writer's fence: a read that saw any sample written
        // after its claim sees the claim.
        fence(Acquire);
        let claim = self.ring.claim.load(Relaxed);
        if from + self.ring.capacity < claim {
            Err(ReadError::Gone)
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for InputBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputBuffer")
            .field("channels", &self.ring.channels)
            .field("capacity", &self.ring.capacity)
            .field("frames", &self.frames())
            .finish()
    }
}

impl Ring {
    fn slot(&self, frame: u64) -> usize {
        (frame & (self.capacity - 1)) as usize
    }

    fn load(&self, channel: usize, slot: usize) -> f32 {
        let index = channel * self.capacity as usize + slot;
        f32::from_bits(self.samples[index].load(Relaxed))
    }

    fn store(&self, channel: usize, slot: usize, sample: f32) {
        let index = channel * self.capacity as usize + slot;
        self.samples[index].store(sample.to_bits(), Relaxed);
    }
}

/// The one thing that writes an [`InputBuffer`]: the newest frames, in
/// order. Real-time safe once begun.
pub(crate) struct InputWriter {
    ring: Arc<Ring>,
    /// Zeros, for writing silence.
    silence: Vec<f32>,
}

impl Drop for InputWriter {
    fn drop(&mut self) {
        self.ring.ended.store(true, Relaxed);
    }
}

impl InputWriter {
    /// Silence comes in chunks of this many frames.
    const SILENCE: usize = 256;

    pub fn channels(&self) -> usize {
        self.ring.channels
    }

    /// Whether anything has been written.
    pub fn has_begun(&self) -> bool {
        self.ring.begun.load(Relaxed)
    }

    /// Just after the last frame written: where the next one goes. Only
    /// meaningful once begun.
    pub fn end(&self) -> u64 {
        self.ring.end.load(Relaxed)
    }

    /// Makes `frame` the first frame, where writing starts. Does nothing
    /// once begun.
    pub fn begin(&mut self, frame: u64) {
        if self.has_begun() {
            return;
        }
        let ring = &*self.ring;
        ring.claim.store(frame, Relaxed);
        ring.start.store(frame, Relaxed);
        ring.end.store(frame, Release);
        ring.begun.store(true, Release);
    }

    /// Writes interleaved `samples` as the next frames.
    pub fn append(&mut self, samples: &[f32]) {
        let ring = &*self.ring;
        let channels = ring.channels;
        debug_assert!(self.has_begun(), "the writer begins before it writes");
        debug_assert!(samples.len().is_multiple_of(channels));
        // Never more than the buffer holds at once, so the claim stays
        // within a buffer of what readers check against.
        for chunk in samples.chunks(ring.capacity as usize * channels) {
            let end = ring.end.load(Relaxed);
            let new_end = end + (chunk.len() / channels) as u64;
            // The claim never goes back: skipping ahead can leave it beyond
            // the end.
            ring.claim
                .store(ring.claim.load(Relaxed).max(new_end), Relaxed);
            // Pairs with the readers' fence: see `InputBuffer::recheck`.
            fence(Release);
            for (i, frame) in chunk.chunks_exact(channels).enumerate() {
                let slot = ring.slot(end + i as u64);
                for (c, &sample) in frame.iter().enumerate() {
                    ring.store(c, slot, sample);
                }
            }
            ring.end.store(new_end, Release);
        }
    }

    /// Writes `frames` frames of silence. Past a buffer's worth, older
    /// frames would be overwritten anyway: it skips ahead, and writes the
    /// last buffer's worth.
    pub fn append_silence(&mut self, frames: u64) {
        let (channels, capacity) = (self.ring.channels, self.ring.capacity);
        let skipped = frames.saturating_sub(capacity);
        if skipped > 0 {
            // Claim everything up to the new end first, so readers of what
            // the buffer held find it gone, then skip.
            let ring = &*self.ring;
            let target = ring.end.load(Relaxed) + frames;
            ring.claim.store(target, Relaxed);
            fence(Release);
            ring.end.store(target - capacity, Release);
        }
        let mut left = frames - skipped;
        while left > 0 {
            let n = left.min(Self::SILENCE as u64) as usize;
            let silence = std::mem::take(&mut self.silence);
            self.append(&silence[..n * channels]);
            self.silence = silence;
            left -= n as u64;
        }
    }
}

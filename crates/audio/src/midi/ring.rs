//! A MIDI input's messages on the engine's timeline, written by the input
//! and read, without locks, by the output stream and anything else that
//! records.

use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, fence};

/// A message from a MIDI input: the engine frame heard as it came in, and
/// a MIDI 1.0 channel message, its status byte and then its data bytes,
/// with 0 for those it hasn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    pub frame: u64,
    pub bytes: [u8; 3],
}

/// What a MIDI input played, lined up with the output: each message with
/// the engine frame heard as it came in, in the order they came, so frames
/// never go back. A player heard that frame as they played the message,
/// so that's where a recording of it belongs, and the output plays it a
/// fixed delay later, to monitor it.
///
/// One thread writes, and any number read at once, each with a
/// [`MidiReader`] of its own, without locks or waiting: the newest
/// [`capacity`](Self::capacity) messages are there to read. Cloning is
/// cheap, and every clone reads the same messages. Reading is real-time
/// safe.
#[derive(Clone)]
pub struct MidiBuffer {
    ring: Arc<Ring>,
}

struct Ring {
    /// A power of two.
    slots: Box<[Slot]>,
    /// Messages written so far.
    written: AtomicU64,
    /// No message with a frame below this is still to come.
    settled: AtomicU64,
    /// Whether the writer is gone: nothing more will be written.
    ended: AtomicBool,
}

/// One message. Its parts are atomics, so a read racing a write is no data
/// race: the sequence number tells the read to throw away what it got.
#[derive(Default)]
struct Slot {
    /// `2n + 2` once it holds message `n`, odd while it's being written.
    seq: AtomicU64,
    frame: AtomicU64,
    /// The message's three bytes.
    message: AtomicU32,
}

impl MidiBuffer {
    /// A buffer holding at least `capacity` messages, and its one writer.
    /// Allocates. Streams make their MIDI inputs' buffers: this is for
    /// anything else that stands in for one, such as a recording's tests.
    pub fn new(capacity: usize) -> (MidiBuffer, MidiWriter) {
        let slots = (0..capacity.max(1).next_power_of_two())
            .map(|_| Slot::default())
            .collect();
        let ring = Arc::new(Ring {
            slots,
            written: AtomicU64::new(0),
            settled: AtomicU64::new(0),
            ended: AtomicBool::new(false),
        });
        let writer = MidiWriter {
            ring: ring.clone(),
            next: 0,
            last: 0,
        };
        (MidiBuffer { ring }, writer)
    }

    /// How many of the newest messages it holds.
    pub fn capacity(&self) -> usize {
        self.ring.slots.len()
    }

    /// Whether the input is gone, such as when its stream closed: every
    /// message it brought is in the buffer.
    pub fn has_ended(&self) -> bool {
        self.ring.ended.load(Acquire)
    }

    /// No message with a frame below this is still to come, so a reader
    /// that has read up to it has them all. All of them once the input has
    /// ended.
    pub fn settled(&self) -> u64 {
        match self.has_ended() {
            true => u64::MAX,
            false => self.ring.settled.load(Acquire),
        }
    }

    /// Says no message with a frame below `frame` is still to come. The
    /// stream says so as it plays: an input stamps each message as it
    /// comes in, with the frame heard then. It never goes back.
    pub fn settle(&self, frame: u64) {
        self.ring.settled.fetch_max(frame, Release);
    }

    /// A reader from the oldest message the buffer holds.
    pub fn reader(&self) -> MidiReader {
        let written = self.ring.written.load(Acquire);
        MidiReader {
            ring: self.ring.clone(),
            next: written.saturating_sub(self.ring.slots.len() as u64),
            lost: 0,
        }
    }

    /// A reader from the next message to come.
    pub fn reader_from_now(&self) -> MidiReader {
        MidiReader {
            ring: self.ring.clone(),
            next: self.ring.written.load(Acquire),
            lost: 0,
        }
    }
}

/// Reads a [`MidiBuffer`]'s messages in order, from where it was made.
pub struct MidiReader {
    ring: Arc<Ring>,
    /// The next message's number.
    next: u64,
    lost: u64,
}

impl MidiReader {
    /// The next message, if it has come and its frame is below `before`.
    /// Real-time safe.
    ///
    /// A reader that falls more than the buffer's capacity behind loses
    /// the messages overwritten, counted in [`lost`](Self::lost), and goes
    /// on from the oldest left.
    pub fn next_before(&mut self, before: u64) -> Option<MidiEvent> {
        let capacity = self.ring.slots.len() as u64;
        loop {
            let written = self.ring.written.load(Acquire);
            if self.next >= written {
                return None;
            }
            if written - self.next > capacity {
                self.lost += written - capacity - self.next;
                self.next = written - capacity;
            }
            let slot = &self.ring.slots[(self.next % capacity) as usize];
            let holds = 2 * self.next + 2;
            if slot.seq.load(Acquire) == holds {
                let frame = slot.frame.load(Relaxed);
                let message = slot.message.load(Relaxed);
                fence(Acquire);
                if slot.seq.load(Relaxed) == holds {
                    if frame >= before {
                        return None;
                    }
                    self.next += 1;
                    let [status, a, b, _] = message.to_le_bytes();
                    return Some(MidiEvent {
                        frame,
                        bytes: [status, a, b],
                    });
                }
            }
            // Overwritten as it was read: the writer has gone round since.
            self.lost += 1;
            self.next += 1;
        }
    }

    /// Messages it lost, overwritten before it could read them.
    pub fn lost(&self) -> u64 {
        self.lost
    }
}

/// Writes a [`MidiBuffer`]. Dropping it ends the buffer.
pub struct MidiWriter {
    ring: Arc<Ring>,
    next: u64,
    /// The last message's frame.
    last: u64,
}

impl MidiWriter {
    /// Adds a message, as heard at engine frame `frame`, or at the last
    /// message's frame if that's later: frames never go back. Real-time
    /// safe.
    pub fn push(&mut self, frame: u64, bytes: [u8; 3]) {
        let frame = frame.max(self.last);
        self.last = frame;
        let n = self.next;
        let slot = &self.ring.slots[(n % self.ring.slots.len() as u64) as usize];
        slot.seq.store(2 * n + 1, Relaxed);
        fence(Release);
        slot.frame.store(frame, Relaxed);
        let [status, a, b] = bytes;
        slot.message
            .store(u32::from_le_bytes([status, a, b, 0]), Relaxed);
        slot.seq.store(2 * n + 2, Release);
        self.ring.written.store(n + 1, Release);
        self.next = n + 1;
    }
}

impl Drop for MidiWriter {
    fn drop(&mut self) {
        self.ring.ended.store(true, Release);
    }
}

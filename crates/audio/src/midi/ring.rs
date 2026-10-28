//! A MIDI input's messages on the engine's timeline, written by the input
//! and read, without locks, by the output stream and anything else that
//! records, and the buffer they're in, which holds any events packed into
//! words ([`EventBuffer`]).

use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64, fence};

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
    /// Each message's three bytes, in a word.
    events: EventBuffer<1>,
}

impl MidiBuffer {
    /// A buffer holding at least `capacity` messages, and its one writer.
    /// Allocates. Streams make their MIDI inputs' buffers: this is for
    /// anything else that stands in for one, such as a recording's tests.
    pub fn new(capacity: usize) -> (MidiBuffer, MidiWriter) {
        let (events, writer) = EventBuffer::new(capacity);
        (MidiBuffer { events }, MidiWriter { writer })
    }

    /// How many of the newest messages it holds.
    pub fn capacity(&self) -> usize {
        self.events.capacity()
    }

    /// Whether the input is gone, such as when its stream closed: every
    /// message it brought is in the buffer.
    pub fn has_ended(&self) -> bool {
        self.events.has_ended()
    }

    /// No message with a frame below this is still to come, so a reader
    /// that has read up to it has them all. All of them once the input has
    /// ended.
    pub fn settled(&self) -> u64 {
        self.events.settled()
    }

    /// Says no message with a frame below `frame` is still to come. The
    /// stream says so as it plays: an input stamps each message as it
    /// comes in, with the frame heard then. It never goes back.
    pub fn settle(&self, frame: u64) {
        self.events.settle(frame);
    }

    /// A reader from the oldest message the buffer holds.
    pub fn reader(&self) -> MidiReader {
        MidiReader {
            events: self.events.reader(),
        }
    }

    /// A reader from the next message to come.
    pub fn reader_from_now(&self) -> MidiReader {
        MidiReader {
            events: self.events.reader_from_now(),
        }
    }
}

/// Reads a [`MidiBuffer`]'s messages in order, from where it was made.
pub struct MidiReader {
    events: EventReader<1>,
}

impl MidiReader {
    /// The next message, if it has come and its frame is below `before`.
    /// Real-time safe.
    ///
    /// A reader that falls more than the buffer's capacity behind loses
    /// the messages overwritten, counted in [`lost`](Self::lost), and goes
    /// on from the oldest left.
    pub fn next_before(&mut self, before: u64) -> Option<MidiEvent> {
        let (frame, [message]) = self.events.next_before(before)?;
        let [status, a, b, ..] = message.to_le_bytes();
        Some(MidiEvent {
            frame,
            bytes: [status, a, b],
        })
    }

    /// Messages it lost, overwritten before it could read them.
    pub fn lost(&self) -> u64 {
        self.events.lost()
    }
}

/// Writes a [`MidiBuffer`]. Dropping it ends the buffer.
pub struct MidiWriter {
    writer: EventWriter<1>,
}

impl MidiWriter {
    /// Adds a message, as heard at engine frame `frame`, or at the last
    /// message's frame if that's later: frames never go back. Real-time
    /// safe.
    pub fn push(&mut self, frame: u64, bytes: [u8; 3]) {
        let [status, a, b] = bytes;
        let message = u64::from_le_bytes([status, a, b, 0, 0, 0, 0, 0]);
        self.writer.push(frame, [message]);
    }
}

/// Events on the engine's timeline, each `WORDS` words of whatever its
/// writer packs into them, with the engine frame it belongs to, in the
/// order they were written, so frames never go back: a [`MidiBuffer`]'s
/// messages, or anything else a recording reads as it would a MIDI
/// input's, such as the notes a track takes from another.
///
/// One thread writes, and any number read at once, each with an
/// [`EventReader`] of its own, without locks or waiting: the newest
/// [`capacity`](Self::capacity) events are there to read. Cloning is
/// cheap, and every clone reads the same events. Reading is real-time
/// safe.
pub struct EventBuffer<const WORDS: usize> {
    ring: Arc<Ring<WORDS>>,
}

// Not derived, which would ask for the words to be `Clone`.
impl<const WORDS: usize> Clone for EventBuffer<WORDS> {
    fn clone(&self) -> Self {
        Self {
            ring: self.ring.clone(),
        }
    }
}

struct Ring<const WORDS: usize> {
    /// A power of two.
    slots: Box<[Slot<WORDS>]>,
    /// Events written so far.
    written: AtomicU64,
    /// No event with a frame below this is still to come.
    settled: AtomicU64,
    /// Whether the writer is gone: nothing more will be written.
    ended: AtomicBool,
}

/// One event. Its parts are atomics, so a read racing a write is no data
/// race: the sequence number tells the read to throw away what it got.
struct Slot<const WORDS: usize> {
    /// `2n + 2` once it holds event `n`, odd while it's being written.
    seq: AtomicU64,
    frame: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl<const WORDS: usize> EventBuffer<WORDS> {
    /// A buffer holding at least `capacity` events, and its one writer.
    /// Allocates.
    pub fn new(capacity: usize) -> (EventBuffer<WORDS>, EventWriter<WORDS>) {
        let slots = (0..capacity.max(1).next_power_of_two())
            .map(|_| Slot {
                seq: AtomicU64::new(0),
                frame: AtomicU64::new(0),
                words: std::array::from_fn(|_| AtomicU64::new(0)),
            })
            .collect();
        let ring = Arc::new(Ring {
            slots,
            written: AtomicU64::new(0),
            settled: AtomicU64::new(0),
            ended: AtomicBool::new(false),
        });
        let writer = EventWriter {
            ring: ring.clone(),
            next: 0,
            last: 0,
        };
        (EventBuffer { ring }, writer)
    }

    /// How many of the newest events it holds.
    pub fn capacity(&self) -> usize {
        self.ring.slots.len()
    }

    /// Whether the writer is gone: every event it wrote is in the buffer.
    pub fn has_ended(&self) -> bool {
        self.ring.ended.load(Acquire)
    }

    /// No event with a frame below this is still to come, so a reader that
    /// has read up to it has them all. All of them once the writer has
    /// ended.
    pub fn settled(&self) -> u64 {
        match self.has_ended() {
            true => u64::MAX,
            false => self.ring.settled.load(Acquire),
        }
    }

    /// Says no event with a frame below `frame` is still to come. It never
    /// goes back.
    pub fn settle(&self, frame: u64) {
        self.ring.settled.fetch_max(frame, Release);
    }

    /// Whether `other` is this buffer, or a clone of it: one with the same
    /// events.
    pub fn same_buffer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.ring, &other.ring)
    }

    /// A reader from the oldest event the buffer holds.
    pub fn reader(&self) -> EventReader<WORDS> {
        let written = self.ring.written.load(Acquire);
        EventReader {
            ring: self.ring.clone(),
            next: written.saturating_sub(self.ring.slots.len() as u64),
            lost: 0,
        }
    }

    /// A reader from the next event to come.
    pub fn reader_from_now(&self) -> EventReader<WORDS> {
        EventReader {
            ring: self.ring.clone(),
            next: self.ring.written.load(Acquire),
            lost: 0,
        }
    }
}

/// Reads an [`EventBuffer`]'s events in order, from where it was made.
pub struct EventReader<const WORDS: usize> {
    ring: Arc<Ring<WORDS>>,
    /// The next event's number.
    next: u64,
    lost: u64,
}

impl<const WORDS: usize> EventReader<WORDS> {
    /// The next event, its frame and its words, if it has come and its
    /// frame is below `before`. Real-time safe.
    ///
    /// A reader that falls more than the buffer's capacity behind loses
    /// the events overwritten, counted in [`lost`](Self::lost), and goes
    /// on from the oldest left.
    pub fn next_before(&mut self, before: u64) -> Option<(u64, [u64; WORDS])> {
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
                let words = std::array::from_fn(|i| slot.words[i].load(Relaxed));
                fence(Acquire);
                if slot.seq.load(Relaxed) == holds {
                    if frame >= before {
                        return None;
                    }
                    self.next += 1;
                    return Some((frame, words));
                }
            }
            // Overwritten as it was read: the writer has gone round since.
            self.lost += 1;
            self.next += 1;
        }
    }

    /// Events it lost, overwritten before it could read them.
    pub fn lost(&self) -> u64 {
        self.lost
    }
}

/// Writes an [`EventBuffer`]. Dropping it ends the buffer.
pub struct EventWriter<const WORDS: usize> {
    ring: Arc<Ring<WORDS>>,
    next: u64,
    /// The last event's frame.
    last: u64,
}

impl<const WORDS: usize> EventWriter<WORDS> {
    /// Adds an event, at engine frame `frame`, or at the last event's frame
    /// if that's later: frames never go back. Real-time safe.
    pub fn push(&mut self, frame: u64, words: [u64; WORDS]) {
        let frame = frame.max(self.last);
        self.last = frame;
        let n = self.next;
        let slot = &self.ring.slots[(n % self.ring.slots.len() as u64) as usize];
        slot.seq.store(2 * n + 1, Relaxed);
        fence(Release);
        slot.frame.store(frame, Relaxed);
        for (word, value) in slot.words.iter().zip(words) {
            word.store(value, Relaxed);
        }
        slot.seq.store(2 * n + 2, Release);
        self.ring.written.store(n + 1, Release);
        self.next = n + 1;
    }

    /// Says no event with a frame below `frame` is still to come, as
    /// [`EventBuffer::settle`] does. Real-time safe.
    pub fn settle(&self, frame: u64) {
        self.ring.settled.fetch_max(frame, Release);
    }
}

impl<const WORDS: usize> Drop for EventWriter<WORDS> {
    fn drop(&mut self) {
        self.ring.ended.store(true, Release);
    }
}

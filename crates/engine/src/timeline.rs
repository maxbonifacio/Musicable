//! The transport's course through engine time, for recording: where in the
//! song each frame the engine rendered played, and whether it recorded.
//!
//! The engine publishes a [`Segment`] wherever the transport stops running
//! on steadily: where it starts or stops playing or recording, jumps, or
//! where the graph's latency changes. A recorder reads them from the
//! [`Timeline`] to place what the inputs captured, frame by frame, without
//! asking the audio thread anything.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use rtrb::{Consumer, Producer, RingBuffer};

/// Segments the engine can publish before a reader takes them. A recorder
/// reads every few milliseconds, and a segment starts at most a few times a
/// block.
const CAPACITY: usize = 4096;

/// A stretch of engine time over which the transport ran on steadily: from
/// [`time`](Self::time) to where the next segment starts, the song position
/// moved on a frame a frame while playing, or stood still while stopped,
/// and the rest held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Segment {
    /// The engine time it starts at: see [`Stats::time`](crate::Stats::time).
    pub time: u64,
    /// The song position at that time, in samples.
    pub position: u64,
    pub playing: bool,
    /// Whether recording was on: the record button.
    pub record: bool,
    /// Whether the transport recorded (see
    /// [`TransportState::recording`](crate::TransportState::recording)).
    pub recording: bool,
    /// How many frames late the graph's output played
    /// ([`PreparedGraph::latency`](crate::PreparedGraph::latency)): what
    /// was heard at engine time `t` played at `t` minus this.
    pub latency: u64,
}

impl Segment {
    /// The song position at engine time `time`, from the segment's start.
    pub fn position_at(&self, time: u64) -> u64 {
        if self.playing {
            self.position + time.saturating_sub(self.time)
        } else {
            self.position
        }
    }

    /// Whether `next`, at a later time, is just this segment running on.
    fn runs_on_to(&self, next: &Segment) -> bool {
        next.position == self.position_at(next.time)
            && (next.playing, next.record, next.recording, next.latency)
                == (self.playing, self.record, self.recording, self.latency)
    }
}

struct Shared {
    /// Engine time up to which the segments published describe the
    /// transport: the end of the last block rendered.
    until: AtomicU64,
    /// Segments the engine couldn't publish, because the queue was full.
    dropped: AtomicU64,
}

/// What a reader got from [`Timeline::read`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    /// The engine time up to which the segments read so far describe the
    /// transport: the last one runs on at least that far.
    pub until: u64,
    /// Whether segments were lost since the last read, because the queue
    /// was full: the segments read don't tell the whole course since then.
    pub lost: bool,
}

/// The reading end of the engine's timeline, from
/// [`EngineHandle::take_timeline`](crate::EngineHandle::take_timeline).
pub struct Timeline {
    segments: Consumer<Segment>,
    shared: Arc<Shared>,
    /// `dropped` as of the last read.
    dropped: u64,
}

impl Timeline {
    /// Moves the segments published since the last call onto the end of
    /// `out`, oldest first, and says how far they go.
    pub fn read(&mut self, out: &mut Vec<Segment>) -> Read {
        // Pairs with the engine's store: the segments for every block before
        // `until` were pushed before it.
        let until = self.shared.until.load(Acquire);
        while let Ok(segment) = self.segments.pop() {
            out.push(segment);
        }
        let dropped = self.shared.dropped.load(Relaxed);
        let lost = dropped != self.dropped;
        self.dropped = dropped;
        Read { until, lost }
    }
}

impl std::fmt::Debug for Timeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timeline")
            .field("until", &self.shared.until.load(Relaxed))
            .field("waiting", &self.segments.slots())
            .finish()
    }
}

/// The engine's end of the timeline.
pub(crate) struct TimelineWriter {
    segments: Producer<Segment>,
    shared: Arc<Shared>,
    /// The segment running now.
    last: Option<Segment>,
}

/// A timeline's two ends. Allocates.
pub(crate) fn timeline() -> (TimelineWriter, Timeline) {
    let (producer, consumer) = RingBuffer::new(CAPACITY);
    let shared = Arc::new(Shared {
        until: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    let writer = TimelineWriter {
        segments: producer,
        shared: shared.clone(),
        last: None,
    };
    let reader = Timeline {
        segments: consumer,
        shared,
        dropped: 0,
    };
    (writer, reader)
}

impl TimelineWriter {
    /// Notes the transport as a block starts: a new segment, unless it's
    /// the last one running on. Real-time safe.
    pub fn block(&mut self, segment: Segment) {
        if self.last.is_some_and(|last| last.runs_on_to(&segment)) {
            return;
        }
        self.last = Some(segment);
        if self.segments.push(segment).is_err() {
            self.shared.dropped.fetch_add(1, Relaxed);
        }
    }

    /// Notes that the blocks before engine time `time` have been rendered.
    pub fn until(&self, time: u64) {
        self.shared.until.store(time, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(time: u64, position: u64, playing: bool) -> Segment {
        Segment {
            time,
            position,
            playing,
            ..Segment::default()
        }
    }

    #[test]
    fn segments_start_only_where_the_transport_changes_course() {
        let (mut writer, mut reader) = timeline();
        writer.block(segment(0, 100, false));
        // Stopped: the position stands still.
        writer.block(segment(64, 100, false));
        // Playing from there, on and on.
        writer.block(segment(128, 100, true));
        writer.block(segment(192, 164, true));
        // A jump, then recording.
        writer.block(segment(256, 0, true));
        writer.block(Segment {
            recording: true,
            record: true,
            ..segment(320, 64, true)
        });
        writer.until(384);
        let mut out = Vec::new();
        let read = reader.read(&mut out);
        assert_eq!(
            read,
            Read {
                until: 384,
                lost: false
            }
        );
        let starts: Vec<(u64, u64)> = out.iter().map(|s| (s.time, s.position)).collect();
        assert_eq!(starts, [(0, 100), (128, 100), (256, 0), (320, 64)]);
        assert_eq!(out[1].position_at(200), 172);
        assert_eq!(out[0].position_at(100), 100);
        // A new latency starts a segment too.
        writer.block(Segment {
            latency: 32,
            ..out[3]
        });
        out.clear();
        reader.read(&mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_full_queue_counts_what_it_drops() {
        let (mut writer, mut reader) = timeline();
        for i in 0..CAPACITY as u64 + 3 {
            writer.block(segment(i, i * 2, false));
        }
        let mut out = Vec::new();
        assert!(reader.read(&mut out).lost);
        assert_eq!(out.len(), CAPACITY);
        assert!(!reader.read(&mut out).lost);
    }
}

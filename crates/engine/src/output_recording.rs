//! Recording what the engine hands the audio device, for checks: every
//! frame of every callback, as the device got it, and on the same clock,
//! what each side of the engine applied and when.
//!
//! The live side copies each callback's output into a ring, with a note of
//! the callback's engine time and the device's counters then, and both
//! sides log what they apply into rings of their own: events, graphs, tempo
//! maps and loop wraps. A thread off the audio thread drains them through
//! the [`OutputRecording`] that [`EngineHandle::record_output`] hands back,
//! so the callback only copies.
//!
//! [`EngineHandle::record_output`]: crate::EngineHandle::record_output

use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::{AudioRef, Event};

/// How many seconds of a stereo output the ring holds before a slow reader
/// loses frames, and of eight channels, a quarter of that.
const SECONDS: usize = 4;

/// How many callbacks and applied events the rings hold before a reader
/// takes them.
const NOTES: usize = 1 << 14;

/// A device callback the output recording kept, or lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Callback {
    /// The engine time of its first frame: see
    /// [`Stats::time`](crate::Stats::time).
    pub time: u64,
    pub frames: u32,
    pub channels: u16,
    /// Whether its frames are in the ring. A reader that let the ring fill
    /// up loses them, and should put as much silence in their place.
    pub kept: bool,
    /// The device's underruns and overruns so far, as the backend counted
    /// them by the time the callback rendered
    /// ([`Engine::record_xrun`](crate::Engine::record_xrun)).
    pub xruns: u64,
    /// The engine's short blocks so far: see
    /// [`Stats::short_blocks`](crate::Stats::short_blocks).
    pub short_blocks: u64,
    /// How far the engine had rendered ahead of the callback.
    pub lead: u64,
}

/// What a side of the engine applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Applied {
    /// An event: a parameter or a note, on its node's frame, or a
    /// transport command.
    Event(Event),
    /// A new graph.
    Graph,
    /// A new tempo map.
    Tempo,
    /// The playhead jumped back to the loop start, at the loop end.
    Wrap,
}

/// What a side of the engine applied, at the engine time it applied it on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedAt {
    pub time: u64,
    pub applied: Applied,
}

/// What an [`OutputRecording`] and the engine's ends of it share.
#[derive(Default)]
struct Shared {
    /// Applied events the engine couldn't log, as the ring was full.
    lost: AtomicU64,
    /// Callbacks the live side couldn't note.
    lost_callbacks: AtomicU64,
    /// Whether each end has stopped writing: the live side's, then the
    /// ahead side's.
    ended: [AtomicBool; 2],
}

/// The live side's end: the output, a callback at a time, and what it
/// applied.
pub(crate) struct OutputTap {
    samples: Producer<f32>,
    callbacks: Producer<Callback>,
    pub(crate) log: EventLog,
}

/// A side's log of what it applied.
pub(crate) struct EventLog {
    applied: Producer<AppliedAt>,
    shared: Arc<Shared>,
    /// Which end it is: 0 for the live side, 1 for the ahead side.
    side: usize,
}

/// The reading end: see the module's documentation.
pub struct OutputRecording {
    samples: Consumer<f32>,
    callbacks: Consumer<Callback>,
    applied: [Consumer<AppliedAt>; 2],
    shared: Arc<Shared>,
}

/// What an [`OutputRecording`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Read {
    /// Whether both sides have stopped writing and everything they wrote has
    /// been read.
    pub ended: bool,
    /// Applied events lost so far, as the rings were full.
    pub lost: u64,
    /// Callbacks lost so far, frames and all.
    pub lost_callbacks: u64,
}

/// An output recording's three ends: the live side's, the ahead side's
/// log, and the reader. Allocates.
pub(crate) fn output_recording(sample_rate: u32) -> (OutputTap, EventLog, OutputRecording) {
    let (samples_tx, samples_rx) = RingBuffer::new(sample_rate as usize * 2 * SECONDS);
    let (callbacks_tx, callbacks_rx) = RingBuffer::new(NOTES);
    let (live_tx, live_rx) = RingBuffer::new(NOTES);
    let (core_tx, core_rx) = RingBuffer::new(NOTES);
    let shared = Arc::new(Shared::default());
    let log = |applied, side| EventLog {
        applied,
        shared: shared.clone(),
        side,
    };
    let tap = OutputTap {
        samples: samples_tx,
        callbacks: callbacks_tx,
        log: log(live_tx, 0),
    };
    let core = log(core_tx, 1);
    let reader = OutputRecording {
        samples: samples_rx,
        callbacks: callbacks_rx,
        applied: [live_rx, core_rx],
        shared,
    };
    (tap, core, reader)
}

impl OutputTap {
    /// Keeps a callback's `output`, interleaved in `channels` channels, as
    /// it went to the device, with `callback`'s note. Real-time safe.
    pub(crate) fn interleaved(&mut self, callback: Callback, output: &[f32]) {
        self.keep(callback, output.len(), |chunk| {
            chunk.fill_from_iter(output.iter().copied());
        });
    }

    /// [`interleaved`](Self::interleaved), for an output with a slice a
    /// channel.
    pub(crate) fn planar(&mut self, callback: Callback, output: AudioRef<'_>) {
        let (frames, channels) = (output.frames(), output.channels());
        let samples = (0..frames).flat_map(|i| (0..channels).map(move |c| output.channel(c)[i]));
        self.keep(callback, frames * channels, |chunk| {
            chunk.fill_from_iter(samples);
        });
    }

    fn keep(
        &mut self,
        callback: Callback,
        samples: usize,
        fill: impl FnOnce(rtrb::chunks::WriteChunkUninit<'_, f32>),
    ) {
        let kept = match self.samples.write_chunk_uninit(samples) {
            Ok(chunk) => {
                fill(chunk);
                true
            }
            Err(_) => false,
        };
        if self.callbacks.push(Callback { kept, ..callback }).is_err() {
            self.log.shared.lost_callbacks.fetch_add(1, Relaxed);
        }
    }
}

impl EventLog {
    /// Notes that its side applied `applied` on engine time `time`.
    /// Real-time safe.
    pub(crate) fn log(&mut self, time: u64, applied: Applied) {
        if self.applied.push(AppliedAt { time, applied }).is_err() {
            self.shared.lost.fetch_add(1, Relaxed);
        }
    }
}

impl Drop for EventLog {
    /// Its side writes no more, whether it was told to stop or the engine
    /// went away.
    fn drop(&mut self) {
        self.shared.ended[self.side].store(true, Release);
    }
}

impl EventLog {
    /// Says its side writes no more, as it lets go of it. Real-time safe.
    pub(crate) fn end(&self) {
        self.shared.ended[self.side].store(true, Release);
    }
}

impl OutputRecording {
    /// Moves what the engine wrote since the last call onto the ends of
    /// `samples`, interleaved as each callback had them, `callbacks` and
    /// `applied`, the latter in no particular order.
    pub fn read(
        &mut self,
        samples: &mut Vec<f32>,
        callbacks: &mut Vec<Callback>,
        applied: &mut Vec<AppliedAt>,
    ) -> Read {
        // Pairs with the ends' stores: what they wrote before they ended
        // is in the rings.
        let ended = self.shared.ended.iter().all(|end| end.load(Acquire));
        // The samples of each callback come before its note.
        let notes = self.callbacks.slots();
        let mut wanted = 0;
        if let Ok(chunk) = self.callbacks.read_chunk(notes) {
            for callback in chunk {
                if callback.kept {
                    wanted += callback.frames as usize * callback.channels as usize;
                }
                callbacks.push(callback);
            }
        }
        if let Ok(chunk) = self.samples.read_chunk(wanted) {
            samples.extend(chunk);
        }
        for ring in &mut self.applied {
            while let Ok(at) = ring.pop() {
                applied.push(at);
            }
        }
        let empty = self.callbacks.is_empty() && self.applied.iter().all(Consumer::is_empty);
        Read {
            ended: ended && empty,
            lost: self.shared.lost.load(Relaxed),
            lost_callbacks: self.shared.lost_callbacks.load(Relaxed),
        }
    }
}

impl std::fmt::Debug for OutputRecording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputRecording")
            .field("samples", &self.samples.slots())
            .field("callbacks", &self.callbacks.slots())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_each_end_writes_is_read_in_order_and_the_end_is_noticed() {
        let (mut tap, mut core, mut reader) = output_recording(100);
        let callback = Callback {
            time: 64,
            frames: 2,
            channels: 2,
            ..Callback::default()
        };
        tap.interleaved(callback, &[1.0, 2.0, 3.0, 4.0]);
        // Left 5 and 6, right 7 and 8.
        let planar = [5.0, 6.0, 7.0, 8.0];
        tap.planar(
            Callback {
                time: 66,
                ..callback
            },
            AudioRef::from_planar(&planar, 2),
        );
        tap.log.log(65, Applied::Graph);
        core.log(64, Applied::Tempo);
        let (mut samples, mut callbacks, mut applied) = (Vec::new(), Vec::new(), Vec::new());
        let read = reader.read(&mut samples, &mut callbacks, &mut applied);
        assert!(!read.ended);
        assert_eq!(samples, [1.0, 2.0, 3.0, 4.0, 5.0, 7.0, 6.0, 8.0]);
        assert_eq!(
            callbacks
                .iter()
                .map(|c| (c.time, c.kept))
                .collect::<Vec<_>>(),
            [(64, true), (66, true)]
        );
        assert_eq!(applied.len(), 2);
        // A ring too full for a callback loses its frames, and says so.
        let long = vec![0.0; 100 * 2 * SECONDS + 2];
        tap.interleaved(
            Callback {
                frames: long.len() as u32 / 2,
                ..callback
            },
            &long,
        );
        callbacks.clear();
        reader.read(&mut samples, &mut callbacks, &mut applied);
        assert!(!callbacks[0].kept);
        core.end();
        drop(tap);
        assert!(
            reader
                .read(&mut samples, &mut callbacks, &mut applied)
                .ended
        );
    }
}

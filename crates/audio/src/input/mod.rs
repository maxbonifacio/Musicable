//! Audio inputs, lined up with the output.
//!
//! Every input a stream opens is lined up with the output's timeline: its
//! [`InputBuffer`] holds, at each engine frame, what the input captured as
//! that frame was heard. A recording reads it there, so it lands where it
//! was played, and the output reads it a fixed delay later to monitor it
//! ([`StreamStats::input_delay`](crate::StreamStats::input_delay)).
//! `docs/audio.md` describes how.
//!
//! Only Windows records so far: elsewhere, a stream's inputs fail to open,
//! and only the tests use most of this.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

mod bus;
mod clock;
mod conform;
mod ring;
#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};

pub(crate) use bus::{Bus, BusInput};
pub(crate) use clock::Playback;
pub(crate) use conform::{Conformer, Packet};
#[cfg(feature = "asio")]
pub(crate) use ring::InputWriter;
pub use ring::{InputBuffer, ReadError};

/// How long an input's buffer holds what it captured, in seconds, at
/// least: how far behind a recording can fall before it loses audio.
const BUFFER_SECONDS: u64 = 8;

/// Frames in an input buffer for an output at `rate`.
pub(crate) fn buffer_frames(rate: u32) -> u64 {
    (u64::from(rate) * BUFFER_SECONDS).next_power_of_two()
}

/// How far behind the output an input can fall before monitoring counts it
/// as stalled, in frames at `rate`: a quarter of a second.
pub(crate) fn max_lag(rate: u32) -> u64 {
    u64::from(rate) / 4
}

/// A device to record from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// Identifies the input across runs: give it in
    /// [`StreamRequest::inputs`](crate::StreamRequest::inputs) to open it.
    pub id: String,
    pub name: String,
    /// Whether it's the system's default input.
    pub is_default: bool,
    pub kind: InputKind,
    /// How many channels it records.
    pub channels: u16,
}

/// What an input records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InputKind {
    /// What comes into an input device: a microphone, a line input.
    Device,
    /// What an output device plays: everything the system mixes for it,
    /// Musicable's own output included when it plays there. Windows only.
    Loopback,
}

/// What prefixes a loopback input's id, before the id of the output device
/// it records.
pub(crate) const LOOPBACK_PREFIX: &str = "loopback:";

/// The id of the input that records what the output device `output` plays.
pub fn loopback_id(output: &str) -> String {
    format!("{LOOPBACK_PREFIX}{output}")
}

/// The output device a loopback input's id names, or `None` if the id isn't
/// a loopback input's.
pub fn loopback_of(id: &str) -> Option<&str> {
    id.strip_prefix(LOOPBACK_PREFIX)
}

/// An input a stream opened, or tried to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputInfo {
    /// As requested.
    pub id: String,
    pub name: String,
    /// 0 if it didn't open.
    pub channels: u16,
    /// Its first channel among the engine's device inputs
    /// ([`ProcessContext::input`](musicable_engine::ProcessContext::input)):
    /// each input's channels follow the one before's.
    pub first_channel: usize,
    /// The rate the device captures at, which can differ from the output's.
    pub sample_rate: u32,
    pub clock: InputClock,
    /// Why it didn't open, or stopped. Its channels are silent then.
    pub error: Option<String>,
}

/// How an input keeps time with the output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum InputClock {
    /// On the output device's own clock, at its rate: its frames line up
    /// with the output's exactly, and are copied as they are.
    #[default]
    Output,
    /// On another clock, or at another rate: resampled to the output's,
    /// following the drift between the two clocks.
    Resampled,
}

/// A running input's counters, from its start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputStats {
    /// Frames the device captured.
    pub captured: u64,
    /// Frames the device lost: dropouts on the input side, silent in its
    /// buffer.
    pub lost: u64,
    /// Times the input had to move against the output to stay lined up:
    /// a jump in what it recorded each time. An input on the output's
    /// clock should never slip.
    pub slips: u64,
    /// Packets the device flagged as not following on from the one before:
    /// a glitch on the input side, even when no frames were lost.
    pub discontinuities: u64,
    /// Whether it has lined up with the output yet. Its buffer is empty
    /// until it has: a fraction of a second after it opens.
    pub aligned: bool,
    /// How it keeps time with the output now. An input that seemed to share
    /// the output's clock, but drifts against it, goes over to resampling.
    pub clock: InputClock,
    /// For a resampled input, how much faster its clock runs than the
    /// output's, in parts per million.
    pub drift_ppm: f64,
    /// Why it stopped, if it did. Its buffer gets no more frames then.
    pub error: Option<String>,
}

/// An input's counters, as its capture thread keeps them.
#[derive(Debug, Default)]
pub(crate) struct InputCounters {
    captured: AtomicU64,
    lost: AtomicU64,
    slips: AtomicU64,
    discontinuities: AtomicU64,
    aligned: AtomicBool,
    resampled: AtomicBool,
    /// Parts per billion.
    drift: AtomicI64,
    error: Mutex<Option<String>>,
}

impl InputCounters {
    pub fn captured(&self, frames: u64) {
        self.captured.fetch_add(frames, Relaxed);
    }

    pub fn lost(&self, frames: u64) {
        self.lost.fetch_add(frames, Relaxed);
    }

    pub fn slip(&self) {
        self.slips.fetch_add(1, Relaxed);
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn discontinuity(&self) {
        self.discontinuities.fetch_add(1, Relaxed);
    }

    pub fn set_aligned(&self, aligned: bool) {
        self.aligned.store(aligned, Relaxed);
    }

    pub fn set_resampled(&self, resampled: bool) {
        self.resampled.store(resampled, Relaxed);
    }

    /// `drift` as a fraction: 1e-6 is a part per million.
    pub fn set_drift(&self, drift: f64) {
        self.drift.store((drift * 1e9).round() as i64, Relaxed);
    }

    /// Records why the input stopped. Not real-time safe.
    pub fn fail(&self, error: String) {
        *self
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }

    pub fn snapshot(&self) -> InputStats {
        InputStats {
            captured: self.captured.load(Relaxed),
            lost: self.lost.load(Relaxed),
            slips: self.slips.load(Relaxed),
            discontinuities: self.discontinuities.load(Relaxed),
            aligned: self.aligned.load(Relaxed),
            clock: if self.resampled.load(Relaxed) {
                InputClock::Resampled
            } else {
                InputClock::Output
            },
            drift_ppm: self.drift.load(Relaxed) as f64 / 1e3,
            error: self
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        }
    }
}

/// An input that opened, as a backend hands it to the stream.
#[derive(Clone)]
pub(crate) struct OpenedInput {
    pub name: String,
    pub channels: u16,
    pub rate: u32,
    pub clock: InputClock,
    pub buffer: InputBuffer,
    pub counters: Arc<InputCounters>,
    /// The room it needs on the bus: see [`BusInput::room`].
    pub room: u64,
}

/// The inputs a stream asked for, `ids`, as they `opened` or didn't: what
/// to tell the stream's users about each, and what goes on the bus, all in
/// the order asked for.
pub(crate) fn assemble(
    ids: &[String],
    opened: Vec<Result<OpenedInput, String>>,
) -> (Vec<InputInfo>, Vec<StreamInput>, Vec<BusInput>) {
    let (mut infos, mut stream, mut bus) = (Vec::new(), Vec::new(), Vec::new());
    let mut first_channel = 0;
    for (id, opened) in ids.iter().zip(opened) {
        match opened {
            Ok(input) => {
                infos.push(InputInfo {
                    id: id.clone(),
                    name: input.name,
                    channels: input.channels,
                    first_channel,
                    sample_rate: input.rate,
                    clock: input.clock,
                    error: None,
                });
                first_channel += usize::from(input.channels);
                stream.push(StreamInput {
                    buffer: Some(input.buffer.clone()),
                    counters: input.counters,
                });
                bus.push(BusInput {
                    buffer: input.buffer,
                    room: input.room,
                });
            }
            Err(error) => {
                infos.push(InputInfo {
                    id: id.clone(),
                    name: id.clone(),
                    channels: 0,
                    first_channel,
                    sample_rate: 0,
                    clock: InputClock::Output,
                    error: Some(error.clone()),
                });
                stream.push(StreamInput::failed(error));
            }
        }
    }
    (infos, stream, bus)
}

/// An input as a stream holds it, for its users: its buffer, if it opened,
/// and its counters.
pub(crate) struct StreamInput {
    pub buffer: Option<InputBuffer>,
    pub counters: Arc<InputCounters>,
}

impl StreamInput {
    /// An input that didn't open, and why.
    pub fn failed(error: String) -> StreamInput {
        let counters = Arc::new(InputCounters::default());
        counters.fail(error);
        StreamInput {
            buffer: None,
            counters,
        }
    }
}

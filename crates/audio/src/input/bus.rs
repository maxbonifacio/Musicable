//! The engine's device inputs for each callback: every input's channels,
//! read from its [`InputBuffer`] a fixed delay behind what the output plays.
//!
//! An input's newest frame is always some way behind the output's: the
//! output writes ahead of what's heard, and the input captures behind it.
//! The delay is how far, plus a little room: what monitoring an input adds
//! to its latency. It's set from the first callbacks once an input starts,
//! and grows if an input falls further behind, which drops out what's
//! monitored for a moment, not what's recorded. It never shrinks, so
//! monitoring never jumps back. An input that falls far behind has stalled,
//! and is silent until it catches up, and one that stopped is silent, rather
//! than holding back the rest.

use musicable_engine::{AudioBuffer, AudioRef};

use super::ring::InputBuffer;

/// One input on the bus.
pub(crate) struct BusInput {
    pub buffer: InputBuffer,
    /// Frames of room to leave beyond how far behind the input is when it
    /// starts: a packet's worth for an input that captures on a thread of
    /// its own, since packets arrive in steps. 0 for one written in the
    /// output's own callback, which is never later than it was.
    pub room: u64,
}

pub(crate) struct Bus {
    inputs: Vec<BusInput>,
    /// Each input's first channel on the bus.
    firsts: Vec<usize>,
    channels: usize,
    buffer: AudioBuffer,
    /// How far behind an input can fall before it counts as stalled.
    max_lag: u64,
    delay: Option<u64>,
    late: u64,
}

impl Bus {
    /// A bus of `inputs`, one after another, for callbacks of up to
    /// `max_frames` frames. An input more than `max_lag` frames behind has
    /// stalled. Allocates.
    pub fn new(inputs: Vec<BusInput>, max_frames: usize, max_lag: u64) -> Bus {
        let mut firsts = Vec::with_capacity(inputs.len());
        let mut channels = 0;
        for input in &inputs {
            firsts.push(channels);
            channels += input.buffer.channels();
        }
        Bus {
            inputs,
            firsts,
            channels,
            buffer: AudioBuffer::new(channels, max_frames),
            max_lag,
            delay: None,
            late: 0,
        }
    }

    /// How far behind the output the inputs play, in frames: 0 until an
    /// input starts.
    pub fn delay(&self) -> u64 {
        self.delay.unwrap_or(0)
    }

    /// Frames that played silent because an input hadn't captured them in
    /// time.
    pub fn late(&self) -> u64 {
        self.late
    }

    /// The bus for the output's engine frames `time..time + frames`: each
    /// input's frames from `delay` frames before. Real-time safe.
    ///
    /// # Panics
    ///
    /// If `frames` is more than the bus was made for.
    pub fn fill(&mut self, time: u64, frames: usize) -> AudioRef<'_> {
        if self.channels == 0 {
            return AudioRef::empty(frames);
        }
        // How far behind the newest frame each input is: the delay has to
        // cover every one that has started, and hasn't stopped or stalled.
        let needed = time + frames as u64;
        let mut required = None::<u64>;
        let mut lagging = 0;
        for input in &self.inputs {
            let range = input.buffer.frames();
            let lag = needed.saturating_sub(range.end);
            if range.is_empty() || input.buffer.has_ended() || lag > self.max_lag {
                continue;
            }
            lagging = lagging.max(lag);
            let delay = lag + input.room;
            required = Some(required.map_or(delay, |r| r.max(delay)));
        }
        match (self.delay, required) {
            (None, Some(required)) => self.delay = Some(required),
            (Some(delay), Some(required)) if lagging > delay => {
                self.late += (lagging - delay).min(frames as u64);
                self.delay = Some(required);
            }
            _ => {}
        }
        let mut view = self.buffer.view_mut(frames);
        let Some(delay) = self.delay else {
            view.clear();
            return self.buffer.view(frames);
        };
        // The frames that play now are of `from..from + frames`.
        let from = time as i64 - delay as i64;
        for (input, &first) in self.inputs.iter().zip(&self.firsts) {
            let range = input.buffer.frames();
            let start = from.max(range.start as i64);
            let end = (from + frames as i64).min(range.end as i64);
            for c in 0..input.buffer.channels() {
                let out = view.channel_mut(first + c);
                out.fill(0.0);
                if start >= end {
                    continue;
                }
                let part = &mut out[(start - from) as usize..(end - from) as usize];
                if input.buffer.read_channel(start as u64, c, part).is_err() {
                    part.fill(0.0);
                }
            }
        }
        self.buffer.view(frames)
    }
}

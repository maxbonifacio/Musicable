//! The engine's MIDI messages for each callback: every MIDI input's, a
//! fixed delay after they were heard.
//!
//! A message stamped with the frame heard as it came in plays that frame
//! plus the delay, so the time between messages stays as it was played,
//! wherever the wake-ups fall. The delay is how far ahead of what's heard
//! a callback renders, at most: the output's latency and a period. It's
//! set from the first callback the output plays, and grows to the most a
//! callback has rendered ahead since, so a message is never too late for
//! its frame. It never shrinks, so monitoring never jumps back.

use musicable_engine::MidiMessage;

use super::ring::{MidiBuffer, MidiReader};

/// The most MIDI messages the engine gets in one callback. The rest wait
/// for the next.
pub(crate) const MAX_MESSAGES: usize = 1024;

/// How far behind the frame heard as a callback starts the stream says
/// its MIDI inputs have settled, in milliseconds: an input stamps a
/// message as it comes in, and writes it a moment later.
const SETTLE_MS: u64 = 5;

/// One MIDI input on the bus.
struct Input {
    /// Its place among the stream's MIDI inputs.
    index: usize,
    buffer: MidiBuffer,
    reader: MidiReader,
}

pub(crate) struct MidiBus {
    inputs: Vec<Input>,
    /// The callback's messages. Capacity for [`MAX_MESSAGES`].
    messages: Vec<MidiMessage>,
    delay: Option<u64>,
    late: u64,
}

impl Default for MidiBus {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl MidiBus {
    /// A bus of the MIDI inputs that opened, each with its place among the
    /// stream's, taking their messages from those to come. Allocates.
    pub fn new(inputs: Vec<(usize, MidiBuffer)>) -> MidiBus {
        let inputs = inputs
            .into_iter()
            .map(|(index, buffer)| Input {
                index,
                reader: buffer.reader_from_now(),
                buffer,
            })
            .collect();
        MidiBus {
            inputs,
            messages: Vec::with_capacity(MAX_MESSAGES),
            delay: None,
            late: 0,
        }
    }

    /// How far after they were heard the engine plays the inputs'
    /// messages, in frames: 0 until the output plays.
    pub fn delay(&self) -> u64 {
        self.delay.unwrap_or(0)
    }

    /// Messages that played later than their frame, on the callback's first
    /// frame: those that came before the delay settled, or in a callback
    /// that had too many.
    pub fn late(&self) -> u64 {
        self.late
    }

    /// The messages for the output's engine frames `time..time + frames`,
    /// in order of time, as the engine takes them. `heard` is the engine
    /// frame heard now, at the output's `rate`: `None`, or below 0, before
    /// the output plays its first frame, when they wait. Real-time safe.
    pub fn fill(
        &mut self,
        time: u64,
        frames: usize,
        heard: Option<f64>,
        rate: u32,
    ) -> &[MidiMessage] {
        self.messages.clear();
        let Some(heard) = heard.filter(|&heard| heard >= 0.0 && !self.inputs.is_empty()) else {
            return &self.messages;
        };
        let heard = heard as u64;
        let end = time + frames as u64;
        let ahead = end.saturating_sub(heard);
        let delay = self.delay.map_or(ahead, |delay| delay.max(ahead));
        self.delay = Some(delay);
        // Messages heard before this play before the callback ends.
        let before = end.saturating_sub(delay);
        let settled = heard.saturating_sub(u64::from(rate) * SETTLE_MS / 1000);
        for input in &mut self.inputs {
            input.buffer.settle(settled);
            while self.messages.len() < MAX_MESSAGES {
                let Some(event) = input.reader.next_before(before) else {
                    break;
                };
                let at = event.frame + delay;
                if at < time {
                    self.late += 1;
                }
                self.messages.push(MidiMessage {
                    time: at,
                    input: input.index,
                    bytes: event.bytes,
                });
            }
        }
        // Each input's are in order already: insertion keeps them so.
        for i in 1..self.messages.len() {
            let mut j = i;
            while j > 0 && self.messages[j - 1].time > self.messages[j].time {
                self.messages.swap(j - 1, j);
                j -= 1;
            }
        }
        &self.messages
    }
}

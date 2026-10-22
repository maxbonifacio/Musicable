//! What processing a plugin takes, whatever its format: its buffers and
//! moving audio between them and the engine's, the notes sounding on it,
//! and the transport.

use std::slice;

use musicable_engine::{AudioMut, Inputs, NoteId, ProcessContext};
use musicable_model::time::{Beats, TimeSignature};

/// How many notes can sound on a plugin at once.
const SOUNDING_CAPACITY: usize = 512;

/// A parameter change from the main thread, for the next block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ParamChange {
    pub(crate) id: u32,
    pub(crate) value: f64,
}

/// The buffers for one side of a plugin's audio ports, as CLAP calls them,
/// or buses, as VST3 does: every channel of every port, allocated once.
pub(crate) struct Channels {
    /// The samples of every channel of every port, one channel after
    /// another. Only ever touched through `channels`, which point into it.
    _samples: Vec<f32>,
    /// One pointer per channel, port after port.
    channels: Vec<*mut f32>,
    /// Where each port's channels start in `channels`, and how many it has.
    ports: Vec<(usize, usize)>,
    /// The port audio goes in or comes out of.
    main: Option<usize>,
    frames: usize,
}

impl Default for Channels {
    fn default() -> Self {
        Self::new(&[], None, 0)
    }
}

impl Channels {
    /// Buffers `frames` long for ports with these channel counts, `main`
    /// being the one audio goes in or comes out of.
    pub(crate) fn new(counts: &[u32], main: Option<usize>, frames: usize) -> Self {
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let mut samples = vec![0.0f32; total * frames];
        let base = samples.as_mut_ptr();
        // SAFETY: each channel starts inside `samples`, or at its end when
        // it holds nothing.
        let channels = (0..total)
            .map(|c| unsafe { base.add(c * frames) })
            .collect();
        let mut ports = Vec::with_capacity(counts.len());
        let mut start = 0;
        for &count in counts {
            ports.push((start, count as usize));
            start += count as usize;
        }
        Self {
            _samples: samples,
            channels,
            ports,
            main: main.filter(|&m| m < counts.len()),
            frames,
        }
    }

    /// The port audio goes in or comes out of, if there's one.
    pub(crate) fn main(&self) -> Option<usize> {
        self.main
    }

    /// Where port `port`'s channel pointers are, one per channel, for the
    /// plugin. They stay put for as long as the buffers live.
    pub(crate) fn pointers(&mut self, port: usize) -> *mut *mut f32 {
        let (first, _) = self.ports[port];
        // SAFETY: `first` is at most the number of channels.
        unsafe { self.channels.as_mut_ptr().add(first) }
    }

    /// Port `port`'s channels.
    fn port_channels(&self, port: usize) -> &[*mut f32] {
        let (first, count) = self.ports[port];
        &self.channels[first..][..count]
    }

    /// Silences the first `frames` samples of every channel.
    pub(crate) fn clear(&mut self, frames: usize) {
        debug_assert!(frames <= self.frames);
        for &channel in &self.channels {
            // SAFETY: every channel holds `self.frames` samples, and
            // nothing else uses them now.
            unsafe { slice::from_raw_parts_mut(channel, frames) }.fill(0.0);
        }
    }

    /// Mixes the node's inputs into port `port`: a mono port gets stereo
    /// inputs as their middle, and every channel gets a mono input.
    pub(crate) fn mix_into(&self, port: usize, inputs: Inputs<'_>, frames: usize) {
        debug_assert!(frames <= self.frames);
        let channels = self.port_channels(port);
        let mono = channels.len() == 1;
        for (c, &channel) in channels.iter().enumerate() {
            // SAFETY: the port's buffers hold `frames` samples, and the
            // plugin isn't using them.
            let channel = unsafe { slice::from_raw_parts_mut(channel, frames) };
            for input in inputs.iter() {
                if mono && input.channels() > 1 {
                    for ((sample, left), right) in channel
                        .iter_mut()
                        .zip(input.channel(0))
                        .zip(input.channel(1))
                    {
                        *sample += 0.5 * (left + right);
                    }
                } else if input.channels() == 1 || c < input.channels() {
                    let source = input.channel(if input.channels() == 1 { 0 } else { c });
                    for (sample, s) in channel.iter_mut().zip(source) {
                        *sample += s;
                    }
                }
            }
        }
    }

    /// Plays port `port`: its first two channels, or its one channel on
    /// both sides. A channel whose bit is set in `silent` plays silence,
    /// whatever its buffer holds.
    pub(crate) fn play(&self, port: usize, frames: usize, silent: u64, output: &mut AudioMut<'_>) {
        debug_assert!(frames <= self.frames);
        let channels = self.port_channels(port);
        let pick = |c: usize| {
            let channel = *channels.get(c)?;
            (silent & 1u64.checked_shl(c as u32).unwrap_or(0) == 0).then_some(channel)
        };
        let sources = match channels.len() {
            0 => [None, None],
            1 => [pick(0), pick(0)],
            _ => [pick(0), pick(1)],
        };
        for c in 0..output.channels() {
            match sources.get(c).copied().flatten() {
                // SAFETY: the port's buffers hold `frames` samples, which
                // the plugin wrote and is done with.
                Some(channel) => output
                    .channel_mut(c)
                    .copy_from_slice(unsafe { slice::from_raw_parts(channel, frames) }),
                None => output.channel_mut(c).fill(0.0),
            }
        }
    }
}

/// The notes sounding on a plugin, by the engine's ids, with the ids the
/// plugin knows them by. Allocated once.
pub(crate) struct Sounding {
    notes: Vec<SoundingNote>,
    next_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SoundingNote {
    pub(crate) note: NoteId,
    /// Never negative: CLAP's and VST3's note ids both use -1 for none.
    pub(crate) id: i32,
    pub(crate) key: i16,
}

impl Sounding {
    pub(crate) fn new() -> Self {
        Self {
            notes: Vec::with_capacity(SOUNDING_CAPACITY),
            next_id: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.notes.clear();
    }

    /// Starts `note` on `key`, unless too many notes are sounding.
    pub(crate) fn start(&mut self, note: NoteId, key: u8) -> Option<SoundingNote> {
        if self.notes.len() == self.notes.capacity() {
            return None;
        }
        let sounding = SoundingNote {
            note,
            id: self.next_id,
            key: key.into(),
        };
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        self.notes.push(sounding);
        Some(sounding)
    }

    pub(crate) fn find(&self, note: NoteId) -> Option<SoundingNote> {
        self.notes.iter().find(|n| n.note == note).copied()
    }

    pub(crate) fn end(&mut self, note: NoteId) -> Option<SoundingNote> {
        let index = self.notes.iter().position(|n| n.note == note)?;
        Some(self.notes.swap_remove(index))
    }
}

/// The transport at a block's first frame, for a plugin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Transport {
    pub(crate) playing: bool,
    /// The song position in samples, in beats (quarter notes) and in
    /// seconds.
    pub(crate) samples: u64,
    pub(crate) beats: f64,
    pub(crate) seconds: f64,
    pub(crate) bpm: f64,
    /// The bar the position is in, and where it started, in beats.
    pub(crate) bar: i64,
    pub(crate) bar_start: f64,
    pub(crate) signature: TimeSignature,
    /// The loop's start and end, in beats and in seconds, while looping is
    /// on.
    pub(crate) looping: Option<Loop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Loop {
    pub(crate) beats: [f64; 2],
    pub(crate) seconds: [f64; 2],
}

impl Transport {
    /// The transport at the first frame of the block `ctx` is for.
    pub(crate) fn at(ctx: &ProcessContext<'_>) -> Self {
        let (rate, tempo) = (ctx.sample_rate.max(1), ctx.tempo);
        let seconds_per_sample = 1.0 / f64::from(rate);
        let position = ctx.transport.position as f64;
        let beats = tempo.beats_at(position, rate);
        let at = Beats::from_f64(beats);
        let bar = tempo.bar_at(at).bar;
        let seconds = |at: Beats| tempo.sample_at(at, rate) * seconds_per_sample;
        Self {
            playing: ctx.transport.playing,
            samples: ctx.transport.position,
            beats,
            seconds: position * seconds_per_sample,
            bpm: tempo.bpm_at(at),
            bar,
            bar_start: tempo.bar_start(bar).to_f64(),
            signature: tempo.signature_at(at),
            looping: ctx.transport.looping.map(|range| Loop {
                beats: [range.start.to_f64(), range.end.to_f64()],
                seconds: [seconds(range.start), seconds(range.end)],
            }),
        }
    }
}

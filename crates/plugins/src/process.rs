//! What processing a plugin takes, whatever its format: its buffers and
//! moving audio between them and the engine's, the notes sounding on it,
//! the notes it plays, and the transport.

use std::ops::Range;
use std::slice;

use musicable_engine::{
    AudioMut, AudioRef, MidiNotes, NoteEvent, NoteId, Notes, Played, ProcessContext,
};
use musicable_model::clip::Dimension;
use musicable_model::time::{Beats, TimeSignature};

/// How many notes can sound on a plugin at once.
const SOUNDING_CAPACITY: usize = 512;

/// The ids of the notes a plugin plays count up from here, clear of the
/// ids of clips' notes, whose clip ids never reach the top bit.
const MADE_NOTES: u64 = 1 << 63;

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

    /// Mixes a node's inputs, those into one of its ports, into port
    /// `port`: a mono port gets stereo inputs as their middle, and every
    /// channel gets a mono input.
    pub(crate) fn mix_into<'a>(
        &self,
        port: usize,
        inputs: impl IntoIterator<Item = AudioRef<'a>>,
        frames: usize,
    ) {
        debug_assert!(frames <= self.frames);
        let channels = self.port_channels(port);
        let mono = channels.len() == 1;
        for input in inputs {
            for (c, &channel) in channels.iter().enumerate() {
                // SAFETY: the port's buffers hold `frames` samples, and the
                // plugin isn't using them.
                let channel = unsafe { slice::from_raw_parts_mut(channel, frames) };
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
    /// The MIDI channel it plays on, for a plugin that takes MPE.
    pub(crate) channel: i16,
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
            channel: 0,
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

/// The notes a plugin plays, as a MIDI effect does, as the engine's notes:
/// each under an id of the engine's own, so none clashes with the notes
/// that come into the plugin. Whether it plays notes of its format, which
/// name a note by its id for it or by channel and key, or MIDI, they're
/// read as MIDI is ([`MidiNotes`]), each channel as a keyboard plays it:
/// a key held again ends the note on it first. Allocated once.
pub(crate) struct Made {
    midi: MidiNotes,
    /// The plugin's ids for the notes sounding that it gave one, with their
    /// channels and keys.
    ids: Vec<(i32, u8, u8)>,
    /// Whether the notes sounding end at the start of the next block, as
    /// the plugin starts again.
    ending: bool,
}

impl Made {
    pub(crate) fn new() -> Self {
        Self {
            midi: MidiNotes::every_channel(MADE_NOTES),
            ids: Vec::with_capacity(SOUNDING_CAPACITY),
            ending: false,
        }
    }

    /// Has the notes sounding end at the start of the next block
    /// ([`start_block`](Self::start_block)), as the plugin restarts, which
    /// forgets them.
    pub(crate) fn end_soon(&mut self) {
        self.ending = true;
    }

    /// Ends the notes sounding, if the plugin restarted since the last
    /// block, as if let go at no particular speed, and lets go of the
    /// switches it held on, such as the sustain pedal.
    pub(crate) fn start_block(&mut self, emit: &mut dyn FnMut(Played)) {
        if std::mem::take(&mut self.ending) {
            self.midi.release_all(&mut *emit);
            self.ids.clear();
        }
    }

    /// Forgets the notes sounding, without ending them: as a plugin starts
    /// afresh where its notes were ended for it.
    pub(crate) fn forget(&mut self) {
        self.midi.forget_notes();
        self.ids.clear();
    }

    /// The plugin starts a note on `key` on `channel`, and calls it `id`,
    /// or -1 for nothing.
    pub(crate) fn start(
        &mut self,
        id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
        emit: &mut dyn FnMut(Played),
    ) {
        let (Ok(channel), Ok(key)) = (u8::try_from(channel.max(0)), u8::try_from(key)) else {
            return;
        };
        if channel > 15 || key > 127 {
            return;
        }
        // The note on the key ends, and its id with it.
        self.ids
            .retain(|&(i, c, k)| (c, k) != (channel, key) && i != id);
        self.midi.start(channel, key, velocity as f32, &mut *emit);
        if id >= 0 && self.ids.len() < self.ids.capacity() {
            self.ids.push((id, channel, key));
        }
    }

    /// The plugin ends a note: the one it calls `id`, or with -1, the one
    /// on `key` on `channel`, either -1 for any.
    pub(crate) fn end(
        &mut self,
        id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
        emit: &mut dyn FnMut(Played),
    ) {
        let (channels, keys) = self.named(id, channel, key);
        for c in channels {
            for k in keys.clone() {
                if self.midi.sounding(c, k).is_some() {
                    self.midi.end(c, k, velocity as f32, &mut *emit);
                    self.ids.retain(|&(_, ic, ik)| (ic, ik) != (c, k));
                }
            }
        }
    }

    /// The plugin changes the expression of a note, or of notes, named as
    /// [`end`](Self::end) names them.
    pub(crate) fn express(
        &mut self,
        id: i32,
        channel: i16,
        key: i16,
        dimension: Dimension,
        value: f64,
        emit: &mut dyn FnMut(Played),
    ) {
        let (channels, keys) = self.named(id, channel, key);
        for c in channels {
            for k in keys.clone() {
                if let Some(note) = self.midi.sounding(c, k) {
                    let value = value as f32;
                    emit(Played::Note(NoteEvent::Expression {
                        note,
                        dimension,
                        value,
                    }));
                }
            }
        }
    }

    /// The plugin plays a MIDI message.
    pub(crate) fn midi(&mut self, bytes: [u8; 3], emit: &mut dyn FnMut(Played)) {
        if matches!(bytes[0] & 0xF0, 0x80 | 0x90) {
            let (channel, key) = (bytes[0] & 0x0F, bytes[1] & 0x7F);
            self.ids.retain(|&(_, c, k)| (c, k) != (channel, key));
        }
        self.midi.take(bytes, &mut *emit);
    }

    /// The channels and keys of the notes the plugin names: the one it
    /// calls `id`, or with -1, those on `key` on `channel`, either -1 for
    /// any.
    fn named(&self, id: i32, channel: i16, key: i16) -> (Range<u8>, Range<u8>) {
        if id >= 0 {
            return match self.ids.iter().find(|&&(i, ..)| i == id) {
                Some(&(_, c, k)) => (c..c + 1, k..k + 1),
                None => (0..0, 0..0),
            };
        }
        let range = |value: i16, count: u8| match u8::try_from(value) {
            Ok(v) if v < count => v..v + 1,
            Ok(_) => 0..0,
            Err(_) => 0..count,
        };
        (range(channel, 16), range(key, 128))
    }
}

/// Sends what a plugin played, `offset` frames into the block, where its
/// notes go.
pub(crate) fn send(notes: &mut Notes<'_>, offset: usize, played: Played) {
    let offset = offset.min(notes.frames().saturating_sub(1));
    match played {
        Played::Note(note) => notes.send(offset, note),
        Played::Controller { controller, value } => {
            notes.send_controller(offset, controller, value)
        }
    };
}

/// The transport at a block's first frame, for a plugin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Transport {
    pub(crate) playing: bool,
    /// Whether the transport records as it plays.
    pub(crate) recording: bool,
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
            recording: ctx.transport.recording,
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

#[cfg(test)]
mod tests {
    use musicable_model::clip::Controller;

    use super::*;

    const RELEASE: f32 = musicable_model::clip::Note::RELEASE_VELOCITY;

    fn on(n: u64, key: u8, velocity: f32) -> Played {
        Played::Note(NoteEvent::On {
            note: NoteId(MADE_NOTES + n),
            key,
            velocity,
        })
    }

    fn off(n: u64, velocity: f32) -> Played {
        Played::Note(NoteEvent::Off {
            note: NoteId(MADE_NOTES + n),
            velocity,
        })
    }

    /// What `play` has the plugin's notes play.
    fn played(
        made: &mut Made,
        play: impl FnOnce(&mut Made, &mut dyn FnMut(Played)),
    ) -> Vec<Played> {
        let mut played = Vec::new();
        play(made, &mut |p| played.push(p));
        played
    }

    #[test]
    fn a_plugins_notes_get_ids_of_their_own_and_end_by_its_ids_or_keys() {
        let mut made = Made::new();
        let started = played(&mut made, |m, emit| {
            m.start(7, 0, 60, 0.25, &mut *emit);
            m.start(-1, 1, 64, 1.0, &mut *emit);
        });
        assert_eq!(started, [on(0, 60, 0.25), on(1, 64, 1.0)]);
        // By its id, by its key, or by none of it.
        let ended = played(&mut made, |m, emit| {
            m.end(7, -1, -1, 0.5, &mut *emit);
            m.end(7, -1, -1, 0.5, &mut *emit);
            m.end(-1, 1, 64, 0.0, &mut *emit);
        });
        assert_eq!(ended, [off(0, 0.5), off(1, 0.0)]);
        // An id it gives again names the new note, and naming nothing ends
        // every note, on any channel.
        let again = played(&mut made, |m, emit| {
            m.start(7, 2, 60, 1.0, &mut *emit);
            m.start(7, 3, 62, 1.0, &mut *emit);
            m.end(7, -1, -1, 0.5, &mut *emit);
            m.start(-1, 4, 67, 1.0, &mut *emit);
            m.end(-1, -1, -1, 0.5, &mut *emit);
        });
        assert_eq!(
            again,
            [
                on(2, 60, 1.0),
                on(3, 62, 1.0),
                off(3, 0.5),
                on(4, 67, 1.0),
                off(2, 0.5),
                off(4, 0.5)
            ]
        );
    }

    #[test]
    fn a_plugins_expression_and_midi_play_as_midi_does() {
        let mut made = Made::new();
        let pressure = |n: u64, value: f32| {
            Played::Note(NoteEvent::Expression {
                note: NoteId(MADE_NOTES + n),
                dimension: Dimension::Pressure,
                value,
            })
        };
        let notes = played(&mut made, |m, emit| {
            m.start(3, 0, 60, 1.0, &mut *emit);
            m.express(3, -1, -1, Dimension::Pressure, 0.5, &mut *emit);
            m.express(-1, 0, 60, Dimension::Pressure, 0.75, &mut *emit);
            // As MIDI: a note on the same key ends the first, whose id
            // then names nothing.
            m.midi([0x90, 60, 127], &mut *emit);
            m.express(3, -1, -1, Dimension::Pressure, 1.0, &mut *emit);
            m.midi([0xB0, 64, 127], &mut *emit);
        });
        let sustain = Played::Controller {
            controller: Controller::SUSTAIN,
            value: 1.0,
        };
        assert_eq!(
            notes,
            [
                on(0, 60, 1.0),
                pressure(0, 0.5),
                pressure(0, 0.75),
                off(0, RELEASE),
                on(1, 60, 1.0),
                sustain
            ]
        );
        // Out of MIDI's range, nothing starts.
        let none = played(&mut made, |m, emit| {
            m.start(1, 16, 60, 1.0, &mut *emit);
            m.start(1, 0, 128, 1.0, &mut *emit);
        });
        assert!(none.is_empty());
    }

    #[test]
    fn a_plugin_that_starts_again_ends_its_notes_or_forgets_them() {
        let mut made = Made::new();
        played(&mut made, |m, emit| {
            m.start(1, 0, 60, 1.0, &mut *emit);
            m.midi([0xB0, 64, 127], &mut *emit);
        });
        assert!(played(&mut made, |m, emit| m.start_block(&mut *emit)).is_empty());
        made.end_soon();
        let ended = played(&mut made, |m, emit| m.start_block(&mut *emit));
        let let_go = Played::Controller {
            controller: Controller::SUSTAIN,
            value: 0.0,
        };
        assert_eq!(ended, [off(0, RELEASE), let_go]);
        assert!(played(&mut made, |m, emit| m.start_block(&mut *emit)).is_empty());
        // Forgotten, its notes end nowhere, and the next has an id of its
        // own still.
        let next = played(&mut made, |m, emit| {
            m.start(1, 0, 62, 1.0, &mut *emit);
            m.forget();
            m.end(1, -1, -1, 0.5, &mut *emit);
            m.start(1, 0, 62, 1.0, &mut *emit);
        });
        assert_eq!(next, [on(1, 62, 1.0), on(2, 62, 1.0)]);
    }
}

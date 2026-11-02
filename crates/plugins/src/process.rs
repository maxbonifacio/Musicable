//! What processing a plugin takes, whatever its format: its buffers and
//! moving audio between them and the engine's, the notes sounding on it,
//! the notes it plays, in the notes pass or as it renders, and the
//! transport.

use std::ops::Range;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};

use musicable_engine::{
    AudioMut, AudioRef, MidiNotes, NoteEvent, NoteId, Notes, Played, ProcessContext, played_notes,
};
use musicable_model::clip::{Dimension, Note};
use musicable_model::time::{Beats, TimeSignature};

/// How many notes can sound on a plugin at once.
const SOUNDING_CAPACITY: usize = 512;

/// The number of the next plugin instance to play notes, for the range of
/// ids its notes take ([`played_notes`]). 0 is the MIDI monitors'.
static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

/// A number for a plugin instance's notes no other plugin's has, once it
/// has counted past every number the ids have room for.
fn next_player() -> u32 {
    loop {
        let n = NEXT_PLAYER.fetch_add(1, Ordering::Relaxed);
        // The monitors' number, as the count goes round.
        if played_notes(n) != played_notes(0) {
            return n;
        }
    }
}

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
    /// One pointer per channel, port after port: the host's own, which the
    /// host reads and writes the buffers through.
    channels: Vec<*mut f32>,
    /// A copy of `channels` for the plugin, which hands it over as its
    /// buffers. A plugin can write to it, as the formats hand it over
    /// writable, so it's copied afresh before every block, and the host
    /// never goes by it.
    shown: Vec<*mut f32>,
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
        let channels: Vec<*mut f32> = (0..total)
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
            shown: channels.clone(),
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
    /// plugin. They stay put for as long as the buffers live, and are
    /// copied afresh as the buffers are cleared, before every block.
    pub(crate) fn pointers(&mut self, port: usize) -> *mut *mut f32 {
        let (first, _) = self.ports[port];
        // SAFETY: `first` is at most the number of channels.
        unsafe { self.shown.as_mut_ptr().add(first) }
    }

    /// Port `port`'s channels.
    fn port_channels(&self, port: usize) -> &[*mut f32] {
        let (first, count) = self.ports[port];
        &self.channels[first..][..count]
    }

    /// Silences the first `frames` samples of every channel, and puts back
    /// the pointers the plugin gets, whatever it did to them.
    pub(crate) fn clear(&mut self, frames: usize) {
        debug_assert!(frames <= self.frames);
        self.shown.copy_from_slice(&self.channels);
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
    /// The MIDI channel it plays on: its own, for a plugin that takes MPE,
    /// or the one a note wire gives it.
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

    /// Starts `note` on `key` on `channel`, unless too many notes are
    /// sounding.
    pub(crate) fn start(&mut self, note: NoteId, key: u8, channel: u8) -> Option<SoundingNote> {
        if self.notes.len() == self.notes.capacity() {
            return None;
        }
        let sounding = SoundingNote {
            note,
            id: self.next_id,
            key: key.into(),
            channel: channel.into(),
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
/// that come into the plugin. The notes of its format name their note by
/// any of an id, a channel and a key, as CLAP's do, each -1 for any: an end
/// or a change of expression is for every note sounding that has all those
/// it gives. A note the plugin gives an id of its own sounds until it's
/// ended, whatever starts on its key, as CLAP's voice stacking and VST3's
/// note ids allow. One without an id, and MIDI, are read as MIDI is
/// ([`MidiNotes`]), each channel as a keyboard plays it: a key held again
/// ends the note on it first. An end or a change that gives an id the
/// plugin gave no note sounding, and a key, is for the note it started
/// without one on that key, as some VST3 plugins give an id to a note's
/// end alone. Allocated once.
pub(crate) struct Made {
    midi: MidiNotes,
    /// The notes sounding that the plugin gave an id of its own.
    own: Vec<Own>,
    /// Whether the notes sounding end at the start of the next block, as
    /// the plugin starts again.
    ending: bool,
    /// Whether the plugin played since without anything taking its notes,
    /// so what it holds of them is stale.
    idle: bool,
}

/// A note sounding that a plugin gave an id of its own.
#[derive(Debug, Clone, Copy)]
struct Own {
    /// The plugin's id for it.
    id: i32,
    channel: u8,
    key: u8,
    /// The engine's id for it.
    note: NoteId,
}

impl Own {
    /// Whether the plugin names it with `id`, `channel` and `key`, each -1
    /// for any.
    fn named(&self, id: i32, channel: i16, key: i16) -> bool {
        (id < 0 || id == self.id)
            && (channel < 0 || channel == i16::from(self.channel))
            && (key < 0 || key == i16::from(self.key))
    }
}

impl Made {
    /// The notes of a plugin instance, under ids of their own, which no
    /// other instance's notes have.
    pub(crate) fn new() -> Self {
        Self::numbered(next_player())
    }

    /// The notes of the plugin with number `n`, for the ids they take
    /// ([`played_notes`]).
    fn numbered(n: u32) -> Self {
        Self {
            midi: MidiNotes::every_channel(played_notes(n)),
            own: Vec::with_capacity(SOUNDING_CAPACITY),
            ending: false,
            idle: false,
        }
    }

    /// The plugin played a block whose notes nothing took. Those it ended
    /// meanwhile weren't read, so once something takes its notes again it
    /// forgets those it had sounding ([`start_block`](Self::start_block)),
    /// which ended where they went as the graph took them away.
    pub(crate) fn idle(&mut self) {
        self.idle = true;
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
        if std::mem::take(&mut self.idle) {
            self.ending = false;
            self.forget();
            return;
        }
        if std::mem::take(&mut self.ending) {
            for own in self.own.drain(..) {
                emit(Played::Note(NoteEvent::Off {
                    note: own.note,
                    velocity: Note::RELEASE_VELOCITY,
                }));
            }
            self.midi.release_all(&mut *emit);
        }
    }

    /// Forgets the notes sounding, without ending them: as a plugin starts
    /// afresh where its notes were ended for it.
    pub(crate) fn forget(&mut self) {
        self.midi.forget_notes();
        self.own.clear();
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
        let velocity = (velocity as f32).clamp(0.0, 1.0);
        if id < 0 {
            self.midi.start(channel, key, velocity, &mut *emit);
        } else if self.own.len() < self.own.capacity() {
            let note = self.midi.fresh_id();
            self.own.push(Own {
                id,
                channel,
                key,
                note,
            });
            emit(Played::Note(NoteEvent::On {
                note,
                key,
                velocity,
                channel: None,
            }));
        }
    }

    /// The plugin ends the notes it names with `id`, `channel` and `key`,
    /// each -1 for any.
    pub(crate) fn end(
        &mut self,
        id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
        emit: &mut dyn FnMut(Played),
    ) {
        let velocity = (velocity as f32).clamp(0.0, 1.0);
        let mut ended = false;
        self.own.retain(|own| {
            let named = own.named(id, channel, key);
            if named {
                let note = own.note;
                emit(Played::Note(NoteEvent::Off { note, velocity }));
                ended = true;
            }
            !named
        });
        // A note without an id has none to name it by.
        if self.by_key(id, key, ended) {
            let (channels, keys) = keys(channel, key);
            for c in channels {
                for k in keys.clone() {
                    if self.midi.sounding(c, k).is_some() {
                        self.midi.end(c, k, velocity, &mut *emit);
                    }
                }
            }
        }
    }

    /// The plugin changes the expression of the notes it names, as
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
        let value = value as f32;
        let express = |note| {
            Played::Note(NoteEvent::Expression {
                note,
                dimension,
                value,
            })
        };
        let mut changed = false;
        for own in self.own.iter().filter(|own| own.named(id, channel, key)) {
            emit(express(own.note));
            changed = true;
        }
        if self.by_key(id, key, changed) {
            let (channels, keys) = keys(channel, key);
            for c in channels {
                for k in keys.clone() {
                    if let Some(note) = self.midi.sounding(c, k) {
                        emit(express(note));
                    }
                }
            }
        }
    }

    /// Whether an end or a change that names notes with `id` and `key`, and
    /// found those with ids of the plugin's own if `found`, is for the
    /// notes started without one too: when it gives no id, or gives one no
    /// note has, and a key.
    fn by_key(&self, id: i32, key: i16, found: bool) -> bool {
        id < 0 || (!found && key >= 0 && !self.own.iter().any(|own| own.id == id))
    }

    /// The plugin plays a MIDI message. All Sound Off and All Notes Off end
    /// the notes on its channel, those with ids of the plugin's own too.
    pub(crate) fn midi(&mut self, bytes: [u8; 3], emit: &mut dyn FnMut(Played)) {
        if bytes[0] & 0xF0 == 0xB0 && matches!(bytes[1], 120 | 123) {
            let channel = i16::from(bytes[0] & 0x0F);
            let velocity = Note::RELEASE_VELOCITY.into();
            self.end(-1, channel, -1, velocity, &mut *emit);
        }
        self.midi.take(bytes, &mut *emit);
    }
}

/// The channels and keys a plugin names with `channel` and `key`, either
/// -1 for any.
fn keys(channel: i16, key: i16) -> (Range<u8>, Range<u8>) {
    let range = |value: i16, count: u8| match u8::try_from(value) {
        Ok(v) if v < count => v..v + 1,
        Ok(_) => 0..0,
        Err(_) => 0..count,
    };
    (range(channel, 16), range(key, 128))
}

/// Where the notes a plugin plays as it processes a block go, as the
/// engine's ([`Made`]).
pub(crate) enum PlayedTo<'a, 'n> {
    /// Nowhere: nothing takes them.
    Nowhere,
    /// Where the node's notes go, as a MIDI effect plays them, in the notes
    /// pass.
    Notes(&'a mut Notes<'n>),
    /// Kept for the node to send where its made notes go once its stage
    /// has rendered, as an instrument or an audio effect plays them out
    /// ([`Kept`]).
    Kept(&'a mut Kept),
}

impl PlayedTo<'_, '_> {
    /// Whether anything takes them.
    pub(crate) fn takes(&self) -> bool {
        !matches!(self, Self::Nowhere)
    }

    /// Counts `count` notes the plugin played as dropped, as there was no
    /// room for them on their way out of it.
    pub(crate) fn drop(&mut self, count: u64) {
        match self {
            Self::Nowhere => {}
            Self::Notes(notes) => notes.drop_more(count),
            Self::Kept(kept) => kept.dropped += count,
        }
    }

    /// Sends or keeps `played`, `offset` frames into the block from engine
    /// time `time`.
    pub(crate) fn play(&mut self, time: u64, offset: usize, played: Played) {
        match self {
            Self::Nowhere => {}
            Self::Notes(notes) => send(notes, offset, played),
            Self::Kept(kept) => kept.keep(time + offset as u64, played),
        }
    }
}

/// The notes a plugin played out as it rendered, as the engine's, kept for
/// its node to send once its stage has rendered
/// ([`Node::made_notes`](musicable_engine::Node::made_notes)): each at the
/// engine time it's on, so the parts of a block its switch has it render
/// as blocks of their own each land where they were played. As many as a
/// block can send, [`Notes::CAPACITY`]: more are dropped and counted.
/// Allocated once.
pub(crate) struct Kept {
    notes: Vec<(u64, Played)>,
    /// How many notes there was no room for since it last sent them.
    dropped: u64,
}

impl Kept {
    pub(crate) fn new() -> Self {
        Self {
            notes: Vec::with_capacity(Notes::CAPACITY),
            dropped: 0,
        }
    }

    /// Keeps `played`, at engine time `time`, if there's room, and counts
    /// it as dropped if not.
    pub(crate) fn keep(&mut self, time: u64, played: Played) {
        if self.notes.len() < self.notes.capacity() {
            self.notes.push((time, played));
        } else {
            self.dropped += 1;
        }
    }

    /// Sends the notes kept where the node's made notes go, for the block
    /// from engine time `time`, and forgets them. One from before the block
    /// comes on its first frame.
    pub(crate) fn send(&mut self, time: u64, notes: &mut Notes<'_>) {
        notes.drop_more(std::mem::take(&mut self.dropped));
        for (at, played) in self.notes.drain(..) {
            let offset = usize::try_from(at.saturating_sub(time)).unwrap_or(usize::MAX);
            send(notes, offset, played);
        }
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

    /// The first id of the notes of the plugin the tests number 1.
    const MADE_NOTES: u64 = played_notes(1);

    fn on(n: u64, key: u8, velocity: f32) -> Played {
        Played::Note(NoteEvent::On {
            note: NoteId(MADE_NOTES + n),
            key,
            velocity,
            channel: None,
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
    fn the_channel_pointers_a_plugin_rewrites_are_put_back() {
        let mut channels = Channels::new(&[2], Some(0), 4);
        let pointers = channels.pointers(0);
        // The plugin points its first channel at a buffer of its own.
        let mut its_own = [7.0f32; 4];
        // SAFETY: the port has two channels.
        unsafe { pointers.write(its_own.as_mut_ptr()) };
        let mut out = [0.0f32; 8];
        let mut output = AudioMut::from_planar(&mut out, 2);
        channels.play(0, 4, 0, &mut output);
        // The host plays and clears its own buffers, not the plugin's.
        assert_eq!(out, [0.0; 8]);
        channels.clear(4);
        assert_eq!(its_own, [7.0; 4]);
        // SAFETY: as above.
        assert_ne!(unsafe { pointers.read() }, its_own.as_mut_ptr());
    }

    #[test]
    fn each_plugins_notes_have_ids_no_others_have() {
        let first = |made: &mut Made| {
            let started = played(made, |m, emit| m.start(-1, 0, 60, 1.0, &mut *emit));
            match started[..] {
                [Played::Note(NoteEvent::On { note, .. })] => note.0,
                _ => panic!("one note starts: {started:?}"),
            }
        };
        let (a, b) = (first(&mut Made::new()), first(&mut Made::new()));
        // Clear of each other's, the MIDI monitors' and the note wires'.
        let range = |id: u64| id & !0xFFFF_FFFF;
        assert_ne!(range(a), range(b));
        for id in [a, b] {
            assert_ne!(range(id), played_notes(0));
            assert_eq!(id >> 62, 0b10, "{id:#x}");
        }
    }

    #[test]
    fn a_plugins_notes_get_ids_of_their_own_and_end_by_its_ids_or_keys() {
        let mut made = Made::numbered(1);
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
        // An id it gives again names both notes, which their channels and
        // keys tell apart, and naming nothing ends every note, on any
        // channel.
        let again = played(&mut made, |m, emit| {
            m.start(7, 2, 60, 1.0, &mut *emit);
            m.start(7, 3, 62, 1.0, &mut *emit);
            m.end(7, 3, 62, 0.5, &mut *emit);
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
    fn notes_a_plugin_gives_ids_of_its_own_sound_together_on_one_key() {
        let mut made = Made::numbered(1);
        let started = played(&mut made, |m, emit| {
            m.start(10, 0, 60, 1.0, &mut *emit);
            m.start(11, 0, 60, 0.5, &mut *emit);
            m.start(12, 0, 60, 0.25, &mut *emit);
        });
        assert_eq!(started, [on(0, 60, 1.0), on(1, 60, 0.5), on(2, 60, 0.25)]);
        // Each goes on answering to its id, and the key names those left.
        let timbre = |n: u64| {
            Played::Note(NoteEvent::Expression {
                note: NoteId(MADE_NOTES + n),
                dimension: Dimension::Timbre,
                value: 0.5,
            })
        };
        let ended = played(&mut made, |m, emit| {
            m.end(10, 0, 60, 0.5, &mut *emit);
            m.express(11, 0, 60, Dimension::Timbre, 0.5, &mut *emit);
            m.end(-1, 0, 60, 0.25, &mut *emit);
        });
        assert_eq!(ended, [off(0, 0.5), timbre(1), off(1, 0.25), off(2, 0.25)]);
        // MIDI's All Notes Off ends them too.
        let all_off = played(&mut made, |m, emit| {
            m.start(13, 1, 64, 1.0, &mut *emit);
            m.start(14, 2, 64, 1.0, &mut *emit);
            m.midi([0xB1, 123, 0], &mut *emit);
        });
        assert_eq!(all_off, [on(3, 64, 1.0), on(4, 64, 1.0), off(3, RELEASE)]);
    }

    #[test]
    fn an_end_with_an_id_no_note_has_ends_the_note_started_without_one_on_its_key() {
        let mut made = Made::numbered(1);
        played(&mut made, |m, emit| {
            m.start(-1, 0, 60, 1.0, &mut *emit);
            m.start(-1, 0, 64, 1.0, &mut *emit);
            m.start(7, 0, 67, 1.0, &mut *emit);
        });
        let pressure = |n: u64| {
            Played::Note(NoteEvent::Expression {
                note: NoteId(MADE_NOTES + n),
                dimension: Dimension::Pressure,
                value: 0.5,
            })
        };
        // As a VST3 plugin may end a note, or change it, with an id it
        // gave only then.
        let ended = played(&mut made, |m, emit| {
            m.express(5, 0, 60, Dimension::Pressure, 0.5, &mut *emit);
            m.end(5, 0, 60, 0.5, &mut *emit);
        });
        assert_eq!(ended, [pressure(0), off(0, 0.5)]);
        // Without a key, or with an id a note has, it names no other.
        let none = played(&mut made, |m, emit| {
            m.end(5, 0, -1, 0.5, &mut *emit);
            m.end(7, 0, 64, 0.5, &mut *emit);
        });
        assert!(none.is_empty());
        let ended = played(&mut made, |m, emit| m.end(7, 0, 67, 0.5, &mut *emit));
        assert_eq!(ended, [off(2, 0.5)]);
    }

    #[test]
    fn a_plugins_expression_and_midi_play_as_midi_does() {
        let mut made = Made::numbered(1);
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
            // As MIDI: a note on the same key ends one there without an id,
            // and one with an id of the plugin's own plays on.
            m.start(-1, 0, 62, 1.0, &mut *emit);
            m.midi([0x90, 62, 127], &mut *emit);
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
                on(1, 62, 1.0),
                off(1, RELEASE),
                on(2, 62, 1.0),
                on(3, 60, 1.0),
                pressure(0, 1.0),
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
    fn notes_kept_past_a_blocks_room_are_counted() {
        let mut kept = Kept::new();
        for n in 0..Notes::CAPACITY as u64 + 3 {
            kept.keep(n, on(n, 60, 1.0));
        }
        assert_eq!(kept.notes.len(), Notes::CAPACITY);
        assert_eq!(kept.dropped, 3);
    }

    #[test]
    fn a_plugin_whose_notes_nothing_took_forgets_those_it_had() {
        let mut made = Made::numbered(1);
        played(&mut made, |m, emit| {
            m.start(3, 0, 60, 1.0, &mut *emit);
            m.start(-1, 0, 62, 1.0, &mut *emit);
        });
        // Nothing takes them for a while, and then something does: what
        // it had sounding ended as the graph took its notes away.
        made.idle();
        made.end_soon();
        assert!(played(&mut made, |m, emit| m.start_block(&mut *emit)).is_empty());
        let ended = played(&mut made, |m, emit| m.end(-1, -1, -1, 0.5, &mut *emit));
        assert!(ended.is_empty(), "{ended:?}");
        assert!(made.own.is_empty());
    }

    #[test]
    fn a_plugin_that_starts_again_ends_its_notes_or_forgets_them() {
        let mut made = Made::numbered(1);
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

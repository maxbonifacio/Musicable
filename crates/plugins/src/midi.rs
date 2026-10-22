//! Notes and controllers as MIDI 1.0 messages, for plugins that take MIDI:
//! as MPE, each note on a channel of its own with its own expression, or on
//! one channel, as a keyboard plays.
//!
//! It writes MIDI the way the engine reads it from MIDI inputs, so a
//! performance recorded from MIDI plays back as it was played: MPE member
//! channels bend 48 semitones either way, and one channel bends 2, MIDI's
//! default, whatever the instrument does with that.

use musicable_engine::{NodeEvent, NodeEventKind, NoteEvent, NoteId};
use musicable_model::clip::{Controller, Dimension};

/// How many notes it keeps track of at once, as many as sound on a plugin.
const CAPACITY: usize = 512;

/// The channel controllers go on: an MPE zone's master channel, or the one
/// channel notes go on.
const MASTER: u8 = 0;
/// MPE's member channels: the 15 after the master, a lower zone.
const MEMBERS: std::ops::RangeInclusive<u8> = 1..=15;
/// How far an MPE member channel bends either way, in semitones: MPE's
/// default.
const MPE_BEND_RANGE: f32 = 48.0;
/// How far a channel bends either way when notes share it: MIDI's default.
const BEND_RANGE: f32 = 2.0;
/// Pitch bend at rest.
const CENTRE: u16 = 8192;
/// MPE's third dimension at rest, CC 74's middle.
const TIMBRE_REST: u8 = 64;
/// The controller MPE's third dimension, slide, goes on.
const SLIDE: u8 = 74;

/// How notes go on channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// MPE's lower zone: each note on a member channel of its own, with
    /// its pitch bend, pressure and slide on that channel.
    Mpe,
    /// Every note on one channel. Pressure goes as each key's own, while
    /// pitch bend and slide go to the channel, and so to every note on it:
    /// the last note's to change wins.
    Channel,
}

/// Plays notes and controllers as MIDI messages. Allocated once.
pub(crate) struct MidiOut {
    layout: Layout,
    /// The notes sounding, with their channels and keys.
    notes: Vec<Playing>,
    channels: [Channel; 16],
    /// Counts notes starting and ending, to tell which channel was used
    /// longest ago.
    clock: u64,
    /// Whether the plugin has been told how its channels are laid out
    /// since it activated.
    started: bool,
}

#[derive(Debug, Clone, Copy)]
struct Playing {
    note: NoteId,
    channel: u8,
    key: u8,
}

/// What a channel was last sent, and how busy it is.
#[derive(Debug, Clone, Copy, Default)]
struct Channel {
    /// Notes sounding on it.
    notes: u16,
    /// When a note last started or ended on it.
    used: u64,
    /// Its pitch bend, pressure and slide, once sent.
    bend: Option<u16>,
    pressure: Option<u8>,
    timbre: Option<u8>,
}

/// A note's starting expression: the values its events set on the frame it
/// starts.
#[derive(Debug, Clone, Copy, Default)]
struct Start {
    bend: Option<f32>,
    pressure: Option<f32>,
    timbre: Option<f32>,
}

/// A value from 0.0 to 1.0 as MIDI's 7 bits.
fn seven(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 127.0).round() as u8
}

impl MidiOut {
    pub(crate) fn new(layout: Layout) -> Self {
        Self {
            layout,
            notes: Vec::with_capacity(CAPACITY),
            channels: [Channel::default(); 16],
            clock: 0,
            started: false,
        }
    }

    /// Forgets everything, as the plugin activates, to lay notes out as
    /// `layout` says: nothing sounds, and the plugin is told its layout
    /// again before anything plays.
    pub(crate) fn reset(&mut self, layout: Layout) {
        self.layout = layout;
        self.notes.clear();
        self.channels = [Channel::default(); 16];
        self.clock = 0;
        self.started = false;
    }

    /// The channel and key `note` plays on, while it sounds.
    pub(crate) fn find(&self, note: NoteId) -> Option<(u8, u8)> {
        let playing = self.notes.iter().find(|p| p.note == note)?;
        Some((playing.channel, playing.key))
    }

    /// The messages that tell the plugin how its channels are laid out,
    /// once after it activates, before anything plays: for MPE, the MPE
    /// configuration message for a lower zone of 15 member channels, and
    /// each member channel's pitch bend range, 48 semitones, for an
    /// instrument that doesn't follow the configuration message.
    pub(crate) fn start(&mut self, emit: &mut impl FnMut([u8; 3])) {
        if std::mem::replace(&mut self.started, true) || self.layout != Layout::Mpe {
            return;
        }
        // Registered parameter 6, MPE's configuration, set to 15 members.
        rpn(emit, MASTER, [0, 6], 15);
        for channel in MEMBERS {
            // Registered parameter 0, the pitch bend range: 48 semitones.
            rpn(emit, channel, [0, 0], MPE_BEND_RANGE as u8);
        }
    }

    /// The messages that play `event`. For a note starting, `after` is the
    /// events that come after it in the block, whose expression for the
    /// note on the same frame sets how it starts: MIDI sets a channel's
    /// expression before the note it's for.
    pub(crate) fn note(
        &mut self,
        event: NoteEvent,
        offset: usize,
        after: &[NodeEvent],
        emit: &mut impl FnMut([u8; 3]),
    ) {
        match event {
            NoteEvent::On {
                note,
                key,
                velocity,
            } => {
                let start = starting(note, offset, after);
                self.on(note, key.min(127), velocity, start, emit);
            }
            NoteEvent::Expression {
                note,
                dimension,
                value,
            } => {
                if let Some(playing) = self.notes.iter().find(|p| p.note == note).copied() {
                    self.express(playing, dimension, value, emit);
                }
            }
            NoteEvent::Off { note, velocity } => {
                let Some(index) = self.notes.iter().position(|p| p.note == note) else {
                    return;
                };
                let playing = self.notes.swap_remove(index);
                self.clock += 1;
                let channel = &mut self.channels[usize::from(playing.channel)];
                channel.notes = channel.notes.saturating_sub(1);
                channel.used = self.clock;
                emit([0x80 | playing.channel, playing.key, seven(velocity)]);
            }
        }
    }

    /// The message that moves `controller`, on the master channel.
    pub(crate) fn controller(
        &mut self,
        controller: Controller,
        value: f32,
        emit: &mut impl FnMut([u8; 3]),
    ) {
        emit([0xB0 | MASTER, controller.number(), seven(value)]);
    }

    fn on(
        &mut self,
        note: NoteId,
        key: u8,
        velocity: f32,
        start: Start,
        emit: &mut impl FnMut([u8; 3]),
    ) {
        // A note already sounding starts again.
        if self.find(note).is_some() {
            let off = NoteEvent::Off {
                note,
                velocity: 0.0,
            };
            self.note(off, 0, &[], emit);
        }
        if self.notes.len() == self.notes.capacity() {
            return;
        }
        let channel = match self.layout {
            Layout::Mpe => self.free_channel(),
            Layout::Channel => MASTER,
        };
        self.clock += 1;
        let state = &mut self.channels[usize::from(channel)];
        state.notes += 1;
        state.used = self.clock;
        let playing = Playing { note, channel, key };
        match self.layout {
            // MPE senders set all three before every note, so the note
            // starts as it should whatever the channel played last.
            Layout::Mpe => {
                let bend = bend(start.bend.unwrap_or(0.0), MPE_BEND_RANGE);
                let pressure = seven(start.pressure.unwrap_or(0.0));
                let timbre = start.timbre.map_or(TIMBRE_REST, seven);
                self.send_bend(channel, bend, true, emit);
                self.send_pressure(channel, pressure, true, emit);
                self.send_timbre(channel, timbre, true, emit);
            }
            // The channel is every note's, so it only changes for a note
            // that starts with expression of its own, or one at rest after
            // another moved it.
            Layout::Channel => {
                let bend = bend(start.bend.unwrap_or(0.0), BEND_RANGE);
                if start.bend.is_some() || self.channels[usize::from(channel)].bend.is_some() {
                    self.send_bend(channel, bend, false, emit);
                }
                if let Some(timbre) = start.timbre {
                    self.send_timbre(channel, seven(timbre), false, emit);
                }
            }
        }
        emit([0x90 | channel, key, seven(velocity).max(1)]);
        self.notes.push(playing);
        if self.layout == Layout::Channel
            && let Some(pressure) = start.pressure
        {
            emit([0xA0 | channel, key, seven(pressure)]);
        }
    }

    fn express(
        &mut self,
        playing: Playing,
        dimension: Dimension,
        value: f32,
        emit: &mut impl FnMut([u8; 3]),
    ) {
        let channel = playing.channel;
        match (dimension, self.layout) {
            (Dimension::PitchBend, Layout::Mpe) => {
                self.send_bend(channel, bend(value, MPE_BEND_RANGE), false, emit);
            }
            (Dimension::PitchBend, Layout::Channel) => {
                self.send_bend(channel, bend(value, BEND_RANGE), false, emit);
            }
            (Dimension::Pressure, Layout::Mpe) => {
                self.send_pressure(channel, seven(value), false, emit);
            }
            (Dimension::Pressure, Layout::Channel) => {
                emit([0xA0 | channel, playing.key, seven(value)]);
            }
            (Dimension::Timbre, _) => self.send_timbre(channel, seven(value), false, emit),
        }
    }

    /// The member channel for a note starting: one with no notes on it,
    /// used longest ago, or failing that, the one with fewest notes, used
    /// longest ago.
    fn free_channel(&self) -> u8 {
        MEMBERS
            .min_by_key(|&c| {
                let channel = &self.channels[usize::from(c)];
                (channel.notes, channel.used)
            })
            .expect("MPE has member channels")
    }

    fn send_bend(&mut self, channel: u8, bend: u16, always: bool, emit: &mut impl FnMut([u8; 3])) {
        let sent = &mut self.channels[usize::from(channel)].bend;
        if always || *sent != Some(bend) {
            *sent = Some(bend);
            emit([0xE0 | channel, (bend & 0x7F) as u8, (bend >> 7) as u8]);
        }
    }

    fn send_pressure(
        &mut self,
        channel: u8,
        pressure: u8,
        always: bool,
        emit: &mut impl FnMut([u8; 3]),
    ) {
        let sent = &mut self.channels[usize::from(channel)].pressure;
        if always || *sent != Some(pressure) {
            *sent = Some(pressure);
            emit([0xD0 | channel, pressure, 0]);
        }
    }

    fn send_timbre(
        &mut self,
        channel: u8,
        timbre: u8,
        always: bool,
        emit: &mut impl FnMut([u8; 3]),
    ) {
        let sent = &mut self.channels[usize::from(channel)].timbre;
        if always || *sent != Some(timbre) {
            *sent = Some(timbre);
            emit([0xB0 | channel, SLIDE, timbre]);
        }
    }
}

/// How `note`, starting at `offset`, starts: its expression in the events
/// on the same frame after it.
fn starting(note: NoteId, offset: usize, after: &[NodeEvent]) -> Start {
    let mut start = Start::default();
    for event in after.iter().take_while(|e| e.offset == offset) {
        if let NodeEventKind::Note(NoteEvent::Expression {
            note: n,
            dimension,
            value,
        }) = event.kind
            && n == note
        {
            let slot = match dimension {
                Dimension::PitchBend => &mut start.bend,
                Dimension::Pressure => &mut start.pressure,
                Dimension::Timbre => &mut start.timbre,
            };
            *slot = Some(value);
        }
    }
    start
}

/// A bend of `semitones` as MIDI's 14 bits, on a channel that bends `range`
/// semitones either way: as far as it goes, if it's further.
fn bend(semitones: f32, range: f32) -> u16 {
    let steps = (semitones / range * f32::from(CENTRE)).round();
    (steps + f32::from(CENTRE)).clamp(0.0, 16_383.0) as u16
}

/// Sets registered parameter `number` (its two bytes, coarse then fine) on
/// `channel` to `value`, and then selects none, so a stray data entry
/// changes nothing.
fn rpn(emit: &mut impl FnMut([u8; 3]), channel: u8, number: [u8; 2], value: u8) {
    let control = 0xB0 | channel;
    emit([control, 101, number[0]]);
    emit([control, 100, number[1]]);
    emit([control, 6, value]);
    emit([control, 38, 0]);
    emit([control, 101, 127]);
    emit([control, 100, 127]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(note: u64, key: u8, velocity: f32) -> NoteEvent {
        NoteEvent::On {
            note: NoteId(note),
            key,
            velocity,
        }
    }

    fn off(note: u64) -> NoteEvent {
        NoteEvent::Off {
            note: NoteId(note),
            velocity: 0.5,
        }
    }

    fn expression(note: u64, dimension: Dimension, value: f32) -> NoteEvent {
        NoteEvent::Expression {
            note: NoteId(note),
            dimension,
            value,
        }
    }

    fn event(offset: usize, note: NoteEvent) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Note(note),
        }
    }

    /// Plays `events`, as a node's block, and returns the messages.
    fn play(out: &mut MidiOut, events: &[NodeEvent]) -> Vec<[u8; 3]> {
        let mut messages = Vec::new();
        let mut emit = |message| messages.push(message);
        out.start(&mut emit);
        for (i, e) in events.iter().enumerate() {
            match e.kind {
                NodeEventKind::Note(note) => out.note(note, e.offset, &events[i + 1..], &mut emit),
                NodeEventKind::Controller { controller, value } => {
                    out.controller(controller, value, &mut emit);
                }
                _ => {}
            }
        }
        messages
    }

    #[test]
    fn mpe_starts_by_setting_the_zone_and_the_bend_ranges() {
        let mut out = MidiOut::new(Layout::Mpe);
        let messages = play(&mut out, &[]);
        assert_eq!(
            messages[..6],
            [
                [0xB0, 101, 0],
                [0xB0, 100, 6],
                [0xB0, 6, 15],
                [0xB0, 38, 0],
                [0xB0, 101, 127],
                [0xB0, 100, 127],
            ]
        );
        assert_eq!(messages.len(), 6 * 16);
        assert_eq!(
            messages[6..9],
            [[0xB1, 101, 0], [0xB1, 100, 0], [0xB1, 6, 48]]
        );
        // Once, until the plugin activates again.
        assert!(play(&mut out, &[]).is_empty());
        out.reset(Layout::Mpe);
        assert_eq!(play(&mut out, &[]).len(), 6 * 16);
        // A channel of notes needs no setting up.
        assert!(play(&mut MidiOut::new(Layout::Channel), &[]).is_empty());
    }

    #[test]
    fn mpe_puts_each_note_on_a_channel_with_its_expression_before_it() {
        let mut out = MidiOut::new(Layout::Mpe);
        play(&mut out, &[]);
        let events = [
            event(0, on(1, 60, 1.0)),
            event(0, expression(1, Dimension::PitchBend, 12.0)),
            event(0, expression(1, Dimension::Timbre, 0.25)),
            event(0, on(2, 64, 0.5)),
            event(10, expression(2, Dimension::Pressure, 1.0)),
            event(20, expression(1, Dimension::PitchBend, -48.0)),
            // Nothing new: nothing sent.
            event(30, expression(1, Dimension::PitchBend, -48.0)),
            event(40, off(1)),
        ];
        assert_eq!(
            play(&mut out, &events),
            [
                // A quarter of the way up: 12 semitones of 48.
                [0xE1, 0, 80],
                [0xD1, 0, 0],
                [0xB1, 74, 32],
                [0x91, 60, 127],
                // The second note at rest, on the next channel.
                [0xE2, 0, 64],
                [0xD2, 0, 0],
                [0xB2, 74, 64],
                [0x92, 64, 64],
                [0xD2, 127, 0],
                [0xE1, 0, 0],
                [0x81, 60, 64],
            ]
        );
        assert_eq!(out.find(NoteId(2)), Some((2, 64)));
        assert_eq!(out.find(NoteId(1)), None);
        // The next note takes a channel no note is on, the one let go
        // longest ago: channel 3, never used.
        let messages = play(&mut out, &[event(0, on(3, 67, 1.0))]);
        assert_eq!(messages.last(), Some(&[0x93, 67, 127]));
    }

    #[test]
    fn mpe_shares_channels_once_all_fifteen_are_busy() {
        let mut out = MidiOut::new(Layout::Mpe);
        play(&mut out, &[]);
        let events: Vec<NodeEvent> = (0..16)
            .map(|n| event(0, on(n, 60 + n as u8, 1.0)))
            .collect();
        let messages = play(&mut out, &events);
        let channels: Vec<u8> = messages
            .iter()
            .filter(|m| m[0] & 0xF0 == 0x90)
            .map(|m| m[0] & 0x0F)
            .collect();
        let mut expected: Vec<u8> = (1..=15).collect();
        // The 16th shares the channel used longest ago.
        expected.push(1);
        assert_eq!(channels, expected);
    }

    #[test]
    fn a_channel_of_notes_bends_them_together_and_presses_each_key() {
        let mut out = MidiOut::new(Layout::Channel);
        let events = [
            // A note at rest leaves the channel alone.
            event(0, on(1, 60, 1.0)),
            event(5, expression(1, Dimension::PitchBend, 2.0)),
            event(5, expression(1, Dimension::Pressure, 0.5)),
            // Bent as the channel already is: nothing to set.
            event(10, on(2, 64, 1.0)),
            event(10, expression(2, Dimension::PitchBend, 2.0)),
            // At rest, after the channel moved: back to rest first.
            event(20, on(3, 67, 1.0)),
            event(30, expression(3, Dimension::Timbre, 1.0)),
            event(40, off(2)),
        ];
        assert_eq!(
            play(&mut out, &events),
            [
                [0x90, 60, 127],
                // As far up as it goes, two semitones.
                [0xE0, 127, 127],
                [0xA0, 60, 64],
                [0x90, 64, 127],
                [0xE0, 0, 64],
                [0x90, 67, 127],
                [0xB0, 74, 127],
                [0x80, 64, 64],
            ]
        );
    }

    #[test]
    fn controllers_go_on_the_master_channel() {
        for layout in [Layout::Mpe, Layout::Channel] {
            let mut out = MidiOut::new(layout);
            play(&mut out, &[]);
            let mut messages = Vec::new();
            out.controller(Controller::SUSTAIN, 1.0, &mut |m| messages.push(m));
            out.controller(Controller::MOD_WHEEL, 64.0 / 127.0, &mut |m| {
                messages.push(m)
            });
            assert_eq!(messages, [[0xB0, 64, 127], [0xB0, 1, 64]]);
        }
    }

    #[test]
    fn bends_go_as_far_as_the_range_and_round_trip_midi_s_values() {
        assert_eq!(bend(0.0, 48.0), 8192);
        assert_eq!(bend(48.0, 48.0), 16_383);
        assert_eq!(bend(-48.0, 48.0), 0);
        assert_eq!(bend(-100.0, 48.0), 0);
        // What the engine reads from a member channel comes back as it was.
        for value in [0u16, 1, 4095, 8191, 8192, 8193, 12_000, 16_383] {
            let semitones = (f32::from(value) - 8192.0) / 8192.0 * 48.0;
            assert_eq!(bend(semitones, 48.0), value);
        }
    }
}

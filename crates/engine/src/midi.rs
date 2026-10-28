//! MIDI from devices: the messages the stream's MIDI inputs bring the
//! engine ([`MidiMessage`]), turning them into notes ([`MidiNotes`]), and
//! the node that plays a MIDI track's input on its instrument
//! ([`MidiMonitor`]), from a MIDI input or another track's notes.
//!
//! Notes from MIDI carry their expression as the engine's notes do, a note
//! at a time: a channel's pitch bend, pressure and slide belong to the
//! notes sounding on it, as MPE has it. A keyboard that plays every note on
//! one channel bends them all together. Other controllers, such as the mod
//! wheel and the sustain pedal, move for every note, as clips' controllers
//! do ([`Played::Controller`]).

use musicable_model::Monitor;
use musicable_model::clip::{Controller, Dimension};
use musicable_model::routing::Tap;

use crate::compile::MidiSource;
use crate::{
    AudioMut, Inputs, Node, NodeEventKind, NoteEvent, NoteId, Notes, ParamId, ProcessContext,
    TransportState,
};

/// A message from one of the stream's MIDI inputs, for the engine to play
/// at engine time `time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiMessage {
    /// The engine frame it plays at.
    pub time: u64,
    /// Which of the stream's MIDI inputs it came from: its place among
    /// them, as the [`InputLayout`](crate::InputLayout) lays them out.
    pub input: usize,
    /// A MIDI 1.0 channel message: its status byte, then its data bytes,
    /// with 0 for those it hasn't.
    pub bytes: [u8; 3],
}

/// The speed a note let go at no particular speed is let go at, as
/// [`Note::RELEASE_VELOCITY`](musicable_model::clip::Note::RELEASE_VELOCITY).
const RELEASE: f32 = 0.5;
/// A channel's pitch bend range until it's told otherwise, in semitones
/// either way: MIDI's default, and an MPE zone's master channel's.
const BEND_RANGE: f32 = 2.0;
/// An MPE zone's member channels' pitch bend range until they're told
/// otherwise.
const MPE_BEND_RANGE: f32 = 48.0;
/// The registered parameter number that selects none.
const NO_RPN: [u8; 2] = [127, 127];

/// A channel message, as far as notes care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Voice {
    /// A note off, at a speed, or a note on at velocity 0, at none.
    Off {
        key: u8,
        velocity: Option<u8>,
    },
    On {
        key: u8,
        velocity: u8,
    },
    KeyPressure {
        key: u8,
        value: u8,
    },
    Control {
        controller: u8,
        value: u8,
    },
    Pressure(u8),
    /// From −8192 to 8191.
    Bend(i16),
}

/// The channel and what a channel message says, or `None` for anything
/// else: program changes and system messages.
fn parse(bytes: [u8; 3]) -> Option<(u8, Voice)> {
    let [status, a, b] = bytes;
    let (a, b) = (a & 0x7F, b & 0x7F);
    let voice = match status & 0xF0 {
        0x80 => Voice::Off {
            key: a,
            velocity: Some(b),
        },
        0x90 if b == 0 => Voice::Off {
            key: a,
            velocity: None,
        },
        0x90 => Voice::On {
            key: a,
            velocity: b,
        },
        0xA0 => Voice::KeyPressure { key: a, value: b },
        0xB0 => Voice::Control {
            controller: a,
            value: b,
        },
        0xD0 => Voice::Pressure(a),
        0xE0 => Voice::Bend(((i16::from(b) << 7) | i16::from(a)) - 8192),
        _ => return None,
    };
    Some((status & 0x0F, voice))
}

fn unit(value: u8) -> f32 {
    f32::from(value) / 127.0
}

/// What a MIDI input plays, as [`MidiNotes`] reads it: a note starting,
/// changing or ending, or a controller moving for every note, as
/// [`NodeEventKind::Controller`] has it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Played {
    Note(NoteEvent),
    /// `value` is from 0.0 to 1.0.
    Controller {
        controller: Controller,
        value: f32,
    },
}

/// The controller a control change moves, or `None` for those MIDI uses
/// to say something else: slide (74), a note's own expression; registered
/// and non-registered parameters and their data entry (6, 38 and 96 to
/// 101); and the channel mode messages, from 120 up.
fn moved(controller: u8) -> Option<Controller> {
    match controller {
        6 | 38 | 74 | 96..=101 => None,
        number => Controller::new(number),
    }
}

/// What each controller holds, as it last moved, for those that have.
#[derive(Debug, Clone, Copy)]
struct Held([Option<f32>; Controller::MAX as usize + 1]);

impl Held {
    const NONE: Self = Self([None; Controller::MAX as usize + 1]);

    fn set(&mut self, controller: Controller, value: f32) {
        self.0[usize::from(controller.number())] = Some(value);
    }

    /// The controllers that have moved, and what they hold, in order.
    fn moved(&self) -> impl Iterator<Item = (Controller, f32)> + '_ {
        Controller::all().filter_map(|c| Some((c, self.0[usize::from(c.number())]?)))
    }

    /// The switches held on, such as the sustain pedal when it's down.
    fn switches_on(&self) -> impl Iterator<Item = Controller> + '_ {
        self.moved()
            .filter(|&(c, value)| c.is_switch() && Controller::is_on(value))
            .map(|(c, _)| c)
    }

    /// Lets go of the switches held on, giving each to `emit`.
    fn let_go(&mut self, mut emit: impl FnMut(Controller)) {
        for c in Controller::all().filter(|c| c.is_switch()) {
            let held = &mut self.0[usize::from(c.number())];
            if held.is_some_and(Controller::is_on) {
                *held = Some(0.0);
                emit(c);
            }
        }
    }
}

/// What a channel holds for the notes on it.
#[derive(Debug, Clone, Copy)]
struct Channel {
    /// Pitch bend, from −1.0 to 1.0 of the range.
    bend: f32,
    /// The pitch bend range, in semitones either way.
    range: f32,
    pressure: f32,
    /// Slide, once the channel has sent it.
    timbre: Option<f32>,
    /// The registered parameter data entry sets, by its number's two bytes:
    /// [`NO_RPN`] for none.
    rpn: [u8; 2],
    /// What data entry has set so far: its coarse and fine bytes.
    data: [u8; 2],
}

impl Channel {
    const fn new() -> Self {
        Self {
            bend: 0.0,
            range: BEND_RANGE,
            pressure: 0.0,
            timbre: None,
            rpn: NO_RPN,
            data: [0, 0],
        }
    }
}

/// Turns what a MIDI input plays into the engine's notes: starts, changes
/// of expression and ends, each note with an id of its own.
///
/// It reads MIDI 1.0 channel messages. Notes start with note on and end
/// with note off, and take their channel's expression: its pitch bend, in
/// the range RPN 0 sets (2 semitones until it does), its pressure, or the
/// key's own, and its slide (CC 74). A note starts with its channel's
/// expression as it is, where that's away from rest.
///
/// Taking every channel, it reads them as MPE does: channel 1 (0 here) is
/// the master of a zone of the 15 channels after it, unless an MPE
/// configuration message (RPN 6) sets other zones. Member channels bend 48
/// semitones until they're told otherwise, and the master's pitch bend
/// bends every note in its zone as well as its own. Taking one channel, it
/// reads that channel alone, as a keyboard plays it.
///
/// Other controllers, such as the mod wheel and the sustain pedal, move for
/// every note, from any channel it takes ([`Played::Controller`]), except
/// those that say something else: slide (74), and data entry and parameter
/// numbers (6, 38 and 96 to 101). All Sound Off and All Notes Off (CC 120 and 123)
/// end a channel's notes, and Reset All Controllers (CC 121) takes its
/// expression back to rest and lets go of the switches held on.
///
/// Real-time safe once made.
pub struct MidiNotes {
    /// The one channel it takes, or `None` for every channel.
    channel: Option<u8>,
    next_id: u64,
    channels: [Channel; 16],
    /// Each channel's zone master, for a member channel of an MPE zone.
    masters: [Option<u8>; 16],
    /// The notes sounding, by channel and key.
    sounding: Box<[[Option<NoteId>; 128]; 16]>,
    /// What the controllers hold, as the input last moved them.
    controllers: Held,
}

impl MidiNotes {
    /// Notes from MIDI channel `channel`, from 0 to 15, or with `None`,
    /// from every channel, as MPE. Their ids count up from `first_id`.
    pub fn new(channel: Option<u8>, first_id: u64) -> Self {
        let mut notes = Self {
            channel,
            next_id: first_id,
            channels: [Channel::new(); 16],
            masters: [None; 16],
            sounding: Box::new([[None; 128]; 16]),
            controllers: Held::NONE,
        };
        if channel.is_none() {
            notes.set_zone(0, 15);
        }
        notes
    }

    /// The one channel it takes, or `None` for every channel.
    pub fn channel(&self) -> Option<u8> {
        self.channel
    }

    /// The controllers the input has moved, and what they hold now, in
    /// order.
    pub fn controllers(&self) -> impl Iterator<Item = (Controller, f32)> + '_ {
        self.controllers.moved()
    }

    /// Reads one message, and gives `emit` what it plays, in order.
    /// Messages on a channel it doesn't take, and anything but channel
    /// messages, do nothing.
    pub fn take(&mut self, bytes: [u8; 3], mut emit: impl FnMut(Played)) {
        let Some((channel, voice)) = parse(bytes) else {
            return;
        };
        if self.channel.is_some_and(|c| c != channel) {
            return;
        }
        self.play(channel, voice, &mut |note| emit(Played::Note(note)));
        if let Voice::Control { controller, value } = voice {
            self.move_controller(controller, value, &mut emit);
        }
    }

    /// What `voice` on `channel` does to notes.
    fn play(&mut self, channel: u8, voice: Voice, emit: &mut impl FnMut(NoteEvent)) {
        let c = usize::from(channel);
        match voice {
            Voice::On { key, velocity } => {
                let slot = &mut self.sounding[c][usize::from(key)];
                if let Some(note) = slot.take() {
                    emit(NoteEvent::Off {
                        note,
                        velocity: RELEASE,
                    });
                }
                let note = NoteId(self.next_id);
                self.next_id = self.next_id.wrapping_add(1);
                *slot = Some(note);
                emit(NoteEvent::On {
                    note,
                    key,
                    velocity: unit(velocity),
                });
                let pitch = self.pitch(channel);
                let Channel {
                    pressure, timbre, ..
                } = self.channels[c];
                let at_start = [
                    (Dimension::PitchBend, (pitch != 0.0).then_some(pitch)),
                    (Dimension::Pressure, (pressure != 0.0).then_some(pressure)),
                    (Dimension::Timbre, timbre),
                ];
                for (dimension, value) in at_start {
                    if let Some(value) = value {
                        emit(NoteEvent::Expression {
                            note,
                            dimension,
                            value,
                        });
                    }
                }
            }
            Voice::Off { key, velocity } => {
                if let Some(note) = self.sounding[c][usize::from(key)].take() {
                    let velocity = velocity.map_or(RELEASE, unit);
                    emit(NoteEvent::Off { note, velocity });
                }
            }
            Voice::KeyPressure { key, value } => {
                if let Some(note) = self.sounding[c][usize::from(key)] {
                    emit(NoteEvent::Expression {
                        note,
                        dimension: Dimension::Pressure,
                        value: unit(value),
                    });
                }
            }
            Voice::Pressure(value) => {
                self.channels[c].pressure = unit(value);
                self.express(channel, Dimension::Pressure, emit);
            }
            Voice::Bend(value) => {
                self.channels[c].bend = f32::from(value) / if value < 0 { 8192.0 } else { 8191.0 };
                self.bend(channel, emit);
            }
            Voice::Control { controller, value } => {
                self.control(channel, controller, value, emit);
            }
        }
    }

    /// Moves the controller a control change moves, if it's one that moves
    /// for every note. Reset All Controllers lets go of the switches held
    /// on.
    fn move_controller(&mut self, controller: u8, value: u8, emit: &mut impl FnMut(Played)) {
        if controller == 121 {
            self.controllers.let_go(|controller| {
                emit(Played::Controller {
                    controller,
                    value: 0.0,
                });
            });
        } else if let Some(controller) = moved(controller) {
            let value = unit(value);
            self.controllers.set(controller, value);
            emit(Played::Controller { controller, value });
        }
    }

    /// Ends every note sounding, as if let go at no particular speed, and
    /// lets go of the switches held on, such as the sustain pedal, where
    /// they played. It still knows the switches are held, for when they
    /// next move.
    pub fn release_all(&mut self, mut emit: impl FnMut(Played)) {
        for slot in self.sounding.iter_mut().flatten() {
            if let Some(note) = slot.take() {
                emit(Played::Note(NoteEvent::Off {
                    note,
                    velocity: RELEASE,
                }));
            }
        }
        for controller in self.controllers.switches_on() {
            emit(Played::Controller {
                controller,
                value: 0.0,
            });
        }
    }

    /// Forgets the notes sounding, without ending them: for notes that
    /// played nowhere. Letting them go then does nothing.
    pub fn forget_notes(&mut self) {
        for slot in self.sounding.iter_mut().flatten() {
            *slot = None;
        }
    }

    /// An id no note it has played had.
    fn fresh_id(&mut self) -> NoteId {
        let note = NoteId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        note
    }

    /// A control change on `channel`.
    fn control(
        &mut self,
        channel: u8,
        controller: u8,
        value: u8,
        emit: &mut impl FnMut(NoteEvent),
    ) {
        let c = usize::from(channel);
        match controller {
            74 => {
                self.channels[c].timbre = Some(unit(value));
                self.express(channel, Dimension::Timbre, emit);
            }
            101 => self.channels[c].rpn[0] = value,
            100 => self.channels[c].rpn[1] = value,
            // A non-registered parameter: data entry sets that instead.
            98 | 99 => self.channels[c].rpn = NO_RPN,
            6 => {
                self.channels[c].data = [value, 0];
                self.registered(channel);
            }
            38 => {
                self.channels[c].data[1] = value;
                self.registered(channel);
            }
            120 | 123 => {
                for slot in &mut self.sounding[c] {
                    if let Some(note) = slot.take() {
                        emit(NoteEvent::Off {
                            note,
                            velocity: RELEASE,
                        });
                    }
                }
            }
            121 => {
                let ch = &mut self.channels[c];
                let timbre = ch.timbre.take();
                ch.pressure = 0.0;
                ch.bend = 0.0;
                ch.rpn = NO_RPN;
                self.bend(channel, emit);
                self.express(channel, Dimension::Pressure, emit);
                if timbre.is_some_and(|t| t != 0.0) {
                    self.channels[c].timbre = Some(0.0);
                    self.express(channel, Dimension::Timbre, emit);
                    self.channels[c].timbre = None;
                }
            }
            _ => {}
        }
    }

    /// Data entry for a registered parameter on `channel`: the pitch bend
    /// range, or an MPE zone.
    fn registered(&mut self, channel: u8) {
        let c = usize::from(channel);
        let Channel { rpn, data, .. } = self.channels[c];
        match rpn {
            [0, 0] => {
                let range = f32::from(data[0]) + f32::from(data[1].min(99)) / 100.0;
                // A zone's members all bend as far.
                match self.masters[c] {
                    Some(master) => {
                        for m in 0..16 {
                            if self.masters[m] == Some(master) {
                                self.channels[m].range = range;
                            }
                        }
                    }
                    None => self.channels[c].range = range,
                }
            }
            [0, 6] if self.channel.is_none() && (channel == 0 || channel == 15) => {
                self.set_zone(channel, data[0]);
            }
            _ => {}
        }
    }

    /// Makes `master`, channel 0 or 15, the master channel of an MPE zone of
    /// `members` channels next to it: up from 1 for 0's, down from 14 for
    /// 15's. Channels the other zone had go to this one. No members ends
    /// the zone.
    fn set_zone(&mut self, master: u8, members: u8) {
        let members = members.min(15);
        let other = 15 - master;
        for c in 0..16 {
            let lost = self.masters[c] == Some(master)
                || (members == 15 && self.masters[c] == Some(other));
            if lost {
                self.masters[c] = None;
                self.channels[c].range = BEND_RANGE;
            }
        }
        for n in 1..=members {
            let c = usize::from(if master == 0 { n } else { 15 - n });
            self.masters[c] = Some(master);
            self.channels[c].range = MPE_BEND_RANGE;
        }
        let m = usize::from(master);
        self.masters[m] = None;
        self.channels[m].range = BEND_RANGE;
    }

    /// A note on `channel`'s pitch bend, in semitones: its own channel's,
    /// and its zone master's.
    fn pitch(&self, channel: u8) -> f32 {
        let bend = |c: usize| self.channels[c].bend * self.channels[c].range;
        let c = usize::from(channel);
        let master = self.masters[c].map_or(0.0, |m| bend(usize::from(m)));
        (bend(c) + master).clamp(-120.0, 120.0)
    }

    /// Sends the pitch of the notes `channel`'s pitch bend bends: its own,
    /// and its zone's, for a master.
    fn bend(&mut self, channel: u8, emit: &mut impl FnMut(NoteEvent)) {
        for c in 0..16u8 {
            if c == channel || self.masters[usize::from(c)] == Some(channel) {
                let value = self.pitch(c);
                for note in self.sounding[usize::from(c)].iter().flatten() {
                    emit(NoteEvent::Expression {
                        note: *note,
                        dimension: Dimension::PitchBend,
                        value,
                    });
                }
            }
        }
    }

    /// Sends `channel`'s pressure or slide to the notes on it.
    fn express(&self, channel: u8, dimension: Dimension, emit: &mut impl FnMut(NoteEvent)) {
        let ch = &self.channels[usize::from(channel)];
        let value = match dimension {
            Dimension::Pressure => ch.pressure,
            Dimension::Timbre => ch.timbre.unwrap_or(0.0),
            Dimension::PitchBend => self.pitch(channel),
        };
        for note in self.sounding[usize::from(channel)].iter().flatten() {
            emit(NoteEvent::Expression {
                note: *note,
                dimension,
                value,
            });
        }
    }
}

/// The ids of the notes a [`MidiMonitor`] plays start here, clear of the
/// ids of clips' notes ([`clips::note_id`](crate::clips::note_id)), whose
/// clip ids never reach the top bit.
const LIVE_NOTES: u64 = 1 << 63;

/// The most clip notes a [`MidiMonitor`] keeps track of at once, as a clip
/// player does, and the most notes from its side input.
const MAX_CLIP_NOTES: usize = 1024;

/// The notes a [`MidiMonitor`] plays on from its side input for notes, each
/// under an id of its own, and the controllers, as the side input last
/// moved them.
struct Taken {
    /// Each note sounding: its id as it came, and as it went on.
    sounding: Vec<(NoteId, NoteId)>,
    controllers: Held,
}

impl Taken {
    fn new() -> Self {
        Self {
            sounding: Vec::with_capacity(MAX_CLIP_NOTES),
            controllers: Held::NONE,
        }
    }

    /// Ends every note sounding, as if let go at no particular speed, and
    /// lets go of the switches held on, as
    /// [`MidiNotes::release_all`] does.
    fn release_all(&mut self, notes: &mut Notes<'_>) {
        for (_, note) in self.sounding.drain(..) {
            let off = NoteEvent::Off {
                note,
                velocity: RELEASE,
            };
            notes.send(0, off);
        }
        for controller in self.controllers.switches_on() {
            notes.send_controller(0, controller, 0.0);
        }
    }
}

/// Plays what comes into a MIDI track's input on its instrument, as the
/// track's input monitoring says: it sits between the track's clip player
/// and its instrument, on the way the notes go
/// ([`Graph::send_notes`](crate::Graph::send_notes)).
///
/// An instrument track plays its input as players expect of one: always
/// with monitoring set to In, while it's armed with Auto, and never with
/// Off. Its clips play too, except with In, and while an armed track with
/// Auto records over them. The notes sounding stop when either stops
/// playing. It takes arming and monitoring as it renders, after it has
/// played the block's notes, so they change what plays from the next
/// block.
///
/// Controllers, such as the sustain pedal, go on from whichever of the two
/// plays, as their notes do. When one stops playing, the switches it held
/// on are let go, so no notes ring on. When the clips play again, their
/// controllers go back to what the clips hold, as the clip player chases
/// them where playback starts.
///
/// The input's messages come from the MIDI inputs its [`MidiSource`] says,
/// turned into notes by [`MidiNotes`], in the blocks the stream places them
/// in ([`ProcessContext::midi`]). Or with a side input for notes, its input
/// is the notes that come into that, another track's: it plays them as it
/// would a MIDI input's, each under an id of its own, so they can't clash
/// with its clips' notes.
pub struct MidiMonitor {
    monitor: Monitor,
    armed: bool,
    /// Where the input's messages come from, or `None` if its MIDI input
    /// isn't open.
    source: Option<MidiSource>,
    /// The tap whose notes come into its side input for notes, when that's
    /// its input.
    side: Option<Tap>,
    notes: MidiNotes,
    taken: Taken,
    /// The notes held on the input a monitor it took over from had, when
    /// that was another: they end at the next block.
    retired: MidiNotes,
    retired_taken: Taken,
    retiring: bool,
    /// The clips' notes it sent on that sound now.
    clip_notes: Vec<NoteId>,
    /// What the clips' controllers hold, as the clips last moved them,
    /// whether they played or not.
    clip_controllers: Held,
    /// Whether it played the input in the last block.
    playing_input: bool,
    /// Whether it played the clips in the last block.
    playing_clips: bool,
}

impl MidiMonitor {
    /// 1.0 arms the track, 0.0 disarms it, as
    /// [`InputMonitor::ARMED`](crate::nodes::InputMonitor::ARMED).
    pub const ARMED: ParamId = ParamId(0);
    /// The track's monitoring, as
    /// [`InputMonitor::monitor_value`](crate::nodes::InputMonitor::monitor_value)
    /// gives it.
    pub const MONITOR: ParamId = ParamId(1);

    /// A track's monitor, without an input.
    pub fn new(monitor: Monitor, armed: bool) -> Self {
        Self {
            monitor,
            armed,
            source: None,
            side: None,
            notes: MidiNotes::new(None, LIVE_NOTES),
            taken: Taken::new(),
            retired: MidiNotes::new(None, LIVE_NOTES),
            retired_taken: Taken::new(),
            retiring: false,
            clip_notes: Vec::with_capacity(MAX_CLIP_NOTES),
            clip_controllers: Held::NONE,
            playing_input: false,
            playing_clips: true,
        }
    }

    /// Takes the messages `source` says.
    pub fn with_source(mut self, source: MidiSource) -> Self {
        self.notes = MidiNotes::new(source.channel, LIVE_NOTES);
        self.source = Some(source);
        self.side = None;
        self
    }

    /// Takes the notes that come into its side input for notes
    /// ([`Graph::send_side_notes`](crate::Graph::send_side_notes)) as its
    /// input, in place of a MIDI input's: another track's notes, from
    /// `from`. A monitor that takes over from one taking another tap's ends
    /// the notes that one held, as it would a MIDI input's.
    pub fn with_side_input(mut self, from: Tap) -> Self {
        self.source = None;
        self.side = Some(from);
        self
    }

    /// Whether it plays the input, and whether the clips.
    fn plays(&self, transport: &TransportState) -> (bool, bool) {
        match self.monitor {
            Monitor::In => (true, false),
            Monitor::Auto => (self.armed, !(self.armed && transport.recording)),
            Monitor::Off => (false, true),
        }
    }

    /// Sends on one of the clips' notes: a start if the clips play, and the
    /// rest always. Instruments pass over notes they don't know, and a note
    /// that started before the monitor came along still ends.
    fn clip_note(&mut self, offset: usize, note: NoteEvent, clips: bool, notes: &mut Notes<'_>) {
        let id = note.note();
        match note {
            NoteEvent::On { .. } => {
                if clips && self.clip_notes.len() < MAX_CLIP_NOTES && notes.send(offset, note) {
                    self.clip_notes.push(id);
                }
            }
            NoteEvent::Expression { .. } => {
                notes.send(offset, note);
            }
            NoteEvent::Off { .. } => {
                if let Some(i) = self.clip_notes.iter().position(|&n| n == id) {
                    self.clip_notes.swap_remove(i);
                }
                notes.send(offset, note);
            }
        }
    }

    /// Sends on a note from the side input, which plays, under an id of its
    /// own.
    fn take_note(&mut self, offset: usize, note: NoteEvent, notes: &mut Notes<'_>) {
        let sounding = &mut self.taken.sounding;
        let at = sounding.iter().position(|&(taken, _)| taken == note.note());
        match note {
            NoteEvent::On { key, velocity, .. } => {
                // The same note starting again ends the last.
                if let Some(i) = at {
                    let (_, note) = sounding.swap_remove(i);
                    let velocity = RELEASE;
                    notes.send(offset, NoteEvent::Off { note, velocity });
                }
                if sounding.len() < MAX_CLIP_NOTES {
                    let fresh = self.notes.fresh_id();
                    let on = NoteEvent::On {
                        note: fresh,
                        key,
                        velocity,
                    };
                    if notes.send(offset, on) {
                        sounding.push((note.note(), fresh));
                    }
                }
            }
            NoteEvent::Expression {
                dimension, value, ..
            } => {
                if let Some(i) = at {
                    let note = sounding[i].1;
                    let changed = NoteEvent::Expression {
                        note,
                        dimension,
                        value,
                    };
                    notes.send(offset, changed);
                }
            }
            NoteEvent::Off { velocity, .. } => {
                if let Some(i) = at {
                    let (_, note) = sounding.swap_remove(i);
                    notes.send(offset, NoteEvent::Off { note, velocity });
                }
            }
        }
    }

    /// Whether `previous` took the same input.
    fn same_input(&self, previous: &MidiMonitor) -> bool {
        (self.source, self.side) == (previous.source, previous.side)
    }
}

impl Node for MidiMonitor {
    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<MidiMonitor>() else {
            return;
        };
        std::mem::swap(&mut self.clip_notes, &mut previous.clip_notes);
        self.clip_controllers = previous.clip_controllers;
        self.playing_input = previous.playing_input;
        self.playing_clips = previous.playing_clips;
        // Notes still to end from before the last swap, if no block has
        // played since. Then the monitor it took over from has held
        // nothing on its own input.
        if previous.retiring {
            std::mem::swap(&mut self.retired, &mut previous.retired);
            std::mem::swap(&mut self.retired_taken, &mut previous.retired_taken);
            self.retiring = true;
        }
        // The notes held on the same input play on. Those held on another
        // end at the next block: a swap can't send notes.
        if self.same_input(previous) {
            std::mem::swap(&mut self.notes, &mut previous.notes);
            std::mem::swap(&mut self.taken, &mut previous.taken);
        } else if !self.retiring {
            std::mem::swap(&mut self.retired, &mut previous.notes);
            std::mem::swap(&mut self.retired_taken, &mut previous.taken);
            self.retiring = true;
        }
        // New notes take ids no note before had.
        self.notes.next_id = (self.notes.next_id)
            .max(self.retired.next_id)
            .max(previous.notes.next_id);
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.clear();
        for event in ctx.events {
            match event.kind {
                NodeEventKind::Param {
                    param: Self::ARMED,
                    value,
                } => self.armed = value >= 0.5,
                NodeEventKind::Param {
                    param: Self::MONITOR,
                    value,
                } => {
                    self.monitor = match value.round() as i64 {
                        0 => Monitor::In,
                        2 => Monitor::Off,
                        _ => Monitor::Auto,
                    }
                }
                _ => {}
            }
        }
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let (input, clips) = self.plays(&ctx.transport);
        if std::mem::take(&mut self.retiring) {
            match self.playing_input {
                true => {
                    self.retired.release_all(|played| send(notes, 0, played));
                    self.retired_taken.release_all(notes);
                }
                false => self.retired.forget_notes(),
            }
            self.retired_taken.sounding.clear();
            self.retired_taken.controllers = Held::NONE;
        }
        if !clips {
            for note in self.clip_notes.drain(..) {
                notes.send(
                    0,
                    NoteEvent::Off {
                        note,
                        velocity: RELEASE,
                    },
                );
            }
        }
        match (self.playing_clips, clips) {
            (true, false) => {
                for controller in self.clip_controllers.switches_on() {
                    notes.send_controller(0, controller, 0.0);
                }
            }
            (false, true) => {
                for (controller, value) in self.clip_controllers.moved() {
                    notes.send_controller(0, controller, value);
                }
            }
            _ => {}
        }
        self.playing_clips = clips;
        match (self.playing_input, input) {
            (true, false) => {
                self.notes.release_all(|played| send(notes, 0, played));
                self.taken.release_all(notes);
            }
            // What's held now never played. The side input's notes held
            // weren't kept, but its controllers go back to what it holds,
            // as the clips' do.
            (false, true) => {
                self.notes.forget_notes();
                for (controller, value) in self.taken.controllers.moved() {
                    notes.send_controller(0, controller, value);
                }
            }
            _ => {}
        }
        self.playing_input = input;

        // The clips' notes and the input's, in order of offset.
        let source = self.source;
        let mut messages = ctx
            .midi
            .iter()
            .filter(|m| source.is_some_and(|s| s.input.is_none_or(|i| i == m.input)))
            .peekable();
        let last = ctx.frames.saturating_sub(1);
        for event in ctx.events.iter().filter(|e| e.kind.is_played()) {
            while let Some(message) = messages.next_if(|m| offset(ctx, m) < event.offset) {
                let at = offset(ctx, message).min(last);
                self.notes.take(message.bytes, |played| {
                    if input {
                        send(notes, at, played);
                    }
                });
            }
            match event.kind {
                NodeEventKind::Note(note) => self.clip_note(event.offset, note, clips, notes),
                NodeEventKind::Controller { controller, value } => {
                    self.clip_controllers.set(controller, value);
                    if clips {
                        notes.send_controller(event.offset, controller, value);
                    }
                }
                NodeEventKind::SideNote(note) if self.side.is_some() && input => {
                    self.take_note(event.offset, note, notes);
                }
                NodeEventKind::SideController { controller, value } if self.side.is_some() => {
                    self.taken.controllers.set(controller, value);
                    if input {
                        notes.send_controller(event.offset, controller, value);
                    }
                }
                _ => {}
            }
        }
        for message in messages {
            let at = offset(ctx, message).min(last);
            self.notes.take(message.bytes, |played| {
                if input {
                    send(notes, at, played);
                }
            });
        }
    }
}

/// Sends what an input played on to where `notes` go, `at` frames into the
/// block.
fn send(notes: &mut Notes<'_>, at: usize, played: Played) {
    match played {
        Played::Note(note) => notes.send(at, note),
        Played::Controller { controller, value } => notes.send_controller(at, controller, value),
    };
}

/// Where in `ctx`'s block `message` plays.
fn offset(ctx: &ProcessContext<'_>, message: &MidiMessage) -> usize {
    message.time.saturating_sub(ctx.time) as usize
}

#[cfg(test)]
mod tests;

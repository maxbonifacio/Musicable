//! Processing a VST3 plugin: the engine's node for it, and what processing
//! needs.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use musicable_engine::switch::Switch;
use musicable_engine::{
    AudioMut, EngineConfig, Inputs, Node, NodeEventKind, NodeMaker, NoteEvent, Notes, Played,
    ProcessContext,
};
use musicable_model::clip::{Controller, Dimension};
use vst3::ComRef;
use vst3::Steinberg::Vst::ControllerNumbers_::{kAfterTouch, kCtrlPolyPressure, kPitchBend};
use vst3::Steinberg::Vst::Event_::EventTypes_::{
    kLegacyMIDICCOutEvent, kNoteExpressionValueEvent, kNoteOffEvent, kNoteOnEvent,
    kPolyPressureEvent,
};
use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::{kBrightnessTypeID, kTuningTypeID};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kBarPositionValid, kContTimeValid, kCycleActive, kCycleValid, kPlaying, kProjectTimeMusicValid,
    kRecording, kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::ProcessModes_::{kPrefetch, kRealtime};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, Event, Event__type0, IAudioProcessor,
    IAudioProcessorTrait, NoteExpressionTypeID, NoteExpressionValueEvent, NoteOffEvent,
    NoteOnEvent, PolyPressureEvent, ProcessContext as Context, ProcessData,
};
use vst3::Steinberg::{int32, kResultOk, tresult, uint16, uint32};

use super::changes::{EVENT_CAPACITY, Events, InputChanges, OutputChanges, QUEUE_CAPACITY};
use super::host::Shared;
use super::module::Module;
use crate::process::{self, Channels, Made, ParamChange, Sounding, SoundingNote, Transport};
use crate::threads::{AudioThread, PROCESSING};

/// Everything processing a plugin needs. Whoever holds the instance's
/// `busy` flag has it: the node processing, or the main thread setting the
/// plugin up.
pub(crate) struct Audio {
    /// What the plugin is activated for, while it is.
    pub(crate) config: Option<EngineConfig>,
    pub(crate) inputs: Buses,
    pub(crate) outputs: Buses,
    /// Whether the plugin takes notes: it has an event input.
    pub(crate) takes_notes: bool,
    /// The parameter each MIDI controller moves on each channel, if any.
    pub(crate) controllers: Controllers,
    pub(crate) changes: InputChanges,
    pub(crate) out_changes: OutputChanges,
    pub(crate) events: Events,
    /// The events the plugin plays in a block, which it keeps while it
    /// plays in the notes pass.
    pub(crate) out_events: Events,
    pub(crate) sounding: Sounding,
    /// The notes it plays, as the engine's.
    pub(crate) made: Made,
    /// The transport, as the plugin last got it.
    pub(crate) context: Context,
    /// Frames processed since the plugin was activated.
    pub(crate) steady_time: i64,
    pub(crate) from_main: rtrb::Consumer<ParamChange>,
    /// Values for the controller: the graph's changes, and the plugin's.
    pub(crate) to_main: rtrb::Producer<ParamChange>,
}

impl Audio {
    pub(crate) fn new(
        from_main: rtrb::Consumer<ParamChange>,
        to_main: rtrb::Producer<ParamChange>,
    ) -> Self {
        Self {
            config: None,
            inputs: Buses::default(),
            outputs: Buses::default(),
            takes_notes: false,
            controllers: Box::new([[None; CONTROLLERS]; 16]),
            changes: InputChanges::new(),
            out_changes: OutputChanges::new(),
            events: Events::new(EVENT_CAPACITY, false),
            out_events: Events::new(Notes::CAPACITY, true),
            sounding: Sounding::new(),
            made: Made::new(),
            context: context(None, 0, 0),
            steady_time: 0,
            from_main,
            to_main,
        }
    }

    /// Gets ready for the plugin to activate for `config`, with buses of
    /// these channel counts, and the main ones.
    pub(crate) fn prepare(
        &mut self,
        config: EngineConfig,
        inputs: &BusInfo,
        outputs: &BusInfo,
        takes_notes: bool,
        controllers: Controllers,
    ) {
        self.config = Some(config);
        self.inputs = Buses::new(inputs, config.max_block);
        self.outputs = Buses::new(outputs, config.max_block);
        self.takes_notes = takes_notes;
        self.controllers = controllers;
        self.changes.clear();
        self.events.clear();
        self.sounding.clear();
        // The notes it played end, as it forgets them.
        self.out_events.clear();
        self.made.end_soon();
        self.context = context(None, config.sample_rate, 0);
        self.steady_time = 0;
    }
}

/// How many MIDI controllers there are.
pub(crate) const CONTROLLERS: usize = Controller::MAX as usize + 1;

/// The parameter each MIDI controller moves, by channel and number.
pub(crate) type Controllers = Box<[[Option<u32>; CONTROLLERS]; 16]>;

/// A plugin's audio buses on one side: their channel counts and names,
/// which is the main one, and which are on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BusInfo {
    pub(crate) channels: Vec<u32>,
    pub(crate) names: Vec<String>,
    pub(crate) main: Option<usize>,
    /// Whether the host turned each on: the main one, and a side input
    /// while something's wired to it.
    pub(crate) on: Vec<bool>,
}

/// The buffers for one side of a plugin's audio buses.
#[derive(Default)]
pub(crate) struct Buses {
    pub(crate) channels: Channels,
    buffers: Vec<AudioBusBuffers>,
    /// Each bus's silence flags, which it's handed over with every block:
    /// every channel of a bus that's off, which holds silence.
    silence: Vec<u64>,
    /// The side inputs that are on, by index, which is also the node's side
    /// input they get.
    sides: Vec<usize>,
}

impl Buses {
    fn new(info: &BusInfo, frames: usize) -> Self {
        let mut channels = Channels::new(&info.channels, info.main, frames);
        let buffers = info
            .channels
            .iter()
            .enumerate()
            .map(|(bus, &count)| AudioBusBuffers {
                numChannels: count as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: channels.pointers(bus),
                },
            })
            .collect();
        let on = |bus| info.on.get(bus).copied().unwrap_or(false);
        let silence = (info.channels.iter().enumerate())
            .map(|(bus, &count)| {
                if on(bus) {
                    0
                } else {
                    u64::MAX >> (64 - count.clamp(1, 64))
                }
            })
            .collect();
        let sides = (0..info.channels.len())
            .filter(|&bus| on(bus) && Some(bus) != info.main)
            .collect();
        Self {
            channels,
            buffers,
            silence,
            sides,
        }
    }

    /// Silences every bus, and gets the buffers ready to hand over.
    fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
        for (buffer, &silence) in self.buffers.iter_mut().zip(&self.silence) {
            buffer.silenceFlags = silence;
        }
    }
}

/// Makes the engine's nodes for a plugin instance: see
/// [`Instance::node_source`](super::Instance::node_source). Every node it
/// makes processes the same plugin.
#[derive(Clone)]
pub struct NodeSource {
    pub(crate) shared: Arc<Shared>,
    pub(crate) module: Arc<Module>,
}

impl NodeSource {
    pub fn node(&self) -> Vst3Node {
        Vst3Node {
            shared: Arc::clone(&self.shared),
            _module: Arc::clone(&self.module),
            resetting: false,
        }
    }
}

/// The compiler makes a plugin device's node with it: see
/// [`Devices`](musicable_engine::Devices).
impl NodeMaker for NodeSource {
    fn make(&self) -> Box<dyn Node> {
        Box::new(self.node())
    }
}

impl fmt::Debug for NodeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeSource")
            .field("plugin", &self.shared.name)
            .finish_non_exhaustive()
    }
}

/// A VST3 plugin, as a node in the engine's graph.
///
/// The node mixes its main input into the plugin's main input bus, and
/// each side input into the plugin's bus of that index while it's on, and
/// plays its main output bus. It sends the plugin its parameter changes and
/// notes, and the transport. Until the plugin is active, while it's being
/// set up, and if it fails, the node plays silence.
///
/// While the node is stopped, as its device is bypassed, the plugin gets
/// its parameter changes in blocks of no frames. VST3 resets a plugin by
/// turning its processing off and on again, which it does before it
/// processes again.
///
/// A MIDI effect plays in the notes pass ([`Node::play_notes`]): the plugin
/// processes the block there, with no sound, and the notes it plays go
/// where the node's notes go, each under an id of the engine's own (see
/// [`Made`]). The controllers it plays as VST3's legacy MIDI go as MIDI
/// does. Those it was playing as it starts again, after its setup changes,
/// end at once. The plugins that make sound play no notes yet.
pub struct Vst3Node {
    shared: Arc<Shared>,
    /// Keeps the plugin's code loaded for as long as the node can call it.
    _module: Arc<Module>,
    /// Whether the plugin resets before it next processes.
    resetting: bool,
}

impl fmt::Debug for Vst3Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vst3Node")
            .field("plugin", &self.shared.name)
            .finish_non_exhaustive()
    }
}

impl Node for Vst3Node {
    fn latency(&self) -> usize {
        self.shared.latency()
    }

    /// Carries on with a reset due on the same plugin.
    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Vst3Node>()
            && Arc::ptr_eq(&previous.shared, &self.shared)
        {
            self.resetting = previous.resetting;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let shared = &self.shared;
        shared.running.store(true, Ordering::Release);
        let Some(_busy) = shared.try_enter() else {
            output.clear();
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        let played = run(
            shared,
            &mut self.resetting,
            ctx,
            inputs,
            Some(&mut *output),
            None,
            audio,
        );
        if !played {
            output.clear();
        }
    }

    /// Processes the plugin for the block, with no sound, and sends the
    /// notes it plays where the node's notes go: a MIDI effect's.
    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let shared = &self.shared;
        shared.running.store(true, Ordering::Release);
        let Some(_busy) = shared.try_enter() else {
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        let none = Inputs::NONE;
        run(
            shared,
            &mut self.resetting,
            ctx,
            none,
            None,
            Some(notes),
            audio,
        );
    }

    /// The plugin stays processing, as VST3 sees it, so the main thread
    /// can hand it changes, and gets no blocks.
    fn stop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
    }

    /// Hands the plugin the block's parameter changes, and the main
    /// thread's, in a block of no frames, if there are any.
    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        let shared = &self.shared;
        let Some(_busy) = shared.try_enter() else {
            return;
        };
        if shared.lifecycle() != PROCESSING {
            return;
        }
        // SAFETY: as in `run`.
        let Some(processor) = (unsafe { ComRef::from_raw(shared.processor()) }) else {
            return;
        };
        let _audio_thread = AudioThread::enter();
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        param_changes(ctx, audio);
        audio.events.clear();
        if audio.changes.is_empty() {
            return;
        }
        audio.changes.ready();
        // SAFETY: processing, holding `busy`; a block of no frames reads
        // no buffers.
        unsafe { process(processor, audio, 0, mode(ctx)) };
        audio.changes.clear();
    }

    /// Has the plugin reset before it next processes.
    fn reset(&mut self) {
        self.resetting = true;
    }
}

/// Processes the plugin for one block, holding `busy`: false if it can't.
/// It resets first if `resetting`. It plays its main output into `output`,
/// if there's one, and the notes it plays into `notes`, if there are.
fn run(
    shared: &Shared,
    resetting: &mut bool,
    ctx: &ProcessContext<'_>,
    inputs: Inputs<'_>,
    output: Option<&mut AudioMut<'_>>,
    notes: Option<&mut Notes<'_>>,
    audio: &mut Audio,
) -> bool {
    if shared.lifecycle() != PROCESSING {
        return false;
    }
    let Some(config) = audio.config else {
        return false;
    };
    let frames = ctx.frames;
    if frames > config.max_block || ctx.sample_rate != config.sample_rate || frames == 0 {
        return false;
    }
    // SAFETY: a processing plugin's processor stays alive until the main
    // thread, which waits for `busy`, deactivates it.
    let Some(processor) = (unsafe { ComRef::from_raw(shared.processor()) }) else {
        return false;
    };
    let _audio_thread = AudioThread::enter();
    if std::mem::take(resetting) {
        // SAFETY: processing, holding `busy`. VST3 lets the processing
        // thread turn processing off and on.
        unsafe {
            processor.setProcessing(0);
            processor.setProcessing(1);
        }
        // Its voices are gone, and so are the notes it played, which ended
        // where they went as it was bypassed.
        audio.sounding.clear();
        audio.made.forget();
    }
    audio.inputs.clear(frames);
    audio.outputs.clear(frames);
    let buses = &audio.inputs;
    if let Some(main) = buses.channels.main() {
        buses.channels.mix_into(main, inputs.iter(), frames);
    }
    for &bus in &buses.sides {
        buses
            .channels
            .mix_into(bus, inputs.side(bus as u32), frames);
    }
    events_for_block(ctx, audio);
    audio.context = context(
        Some(&Transport::at(ctx)),
        config.sample_rate,
        audio.steady_time,
    );
    // SAFETY: processing, holding `busy`, with buffers `frames` long.
    let result = unsafe { process(processor, audio, frames, mode(ctx)) };
    audio.steady_time += frames as i64;
    if result != kResultOk {
        return false;
    }
    if let Some(notes) = notes {
        let Audio {
            out_events, made, ..
        } = audio;
        made.start_block(&mut |p| process::send(notes, 0, p));
        for event in out_events.list() {
            let offset = usize::try_from(event.sampleOffset).unwrap_or(0);
            played(event, made, &mut |p| process::send(notes, offset, p));
        }
    }
    let Some(output) = output else {
        return true;
    };
    let Some(main) = audio.outputs.channels.main() else {
        return false;
    };
    let silent = audio.outputs.buffers[main].silenceFlags;
    audio.outputs.channels.play(main, frames, silent, output);
    true
}

/// The mode a block is processed in: prefetch for a block rendered ahead
/// of the device, as Cubase's ASIO-Guard has it, which VST3 lets a host
/// change from one call to the next, and real time for the rest.
fn mode(ctx: &ProcessContext<'_>) -> int32 {
    match ctx.ahead {
        true => kPrefetch as int32,
        false => kRealtime as int32,
    }
}

/// Plays an event the plugin played, as the engine's notes: see [`Made`].
/// Its legacy MIDI's controllers, channel pressure, pitch bend and key
/// pressure go as MIDI.
fn played(event: &Event, made: &mut Made, emit: &mut dyn FnMut(Played)) {
    // SAFETY, for each union field read: the event's type says which it
    // is.
    match event.r#type {
        t if t == kNoteOnEvent as uint16 => {
            let on = unsafe { event.__field0.noteOn };
            let velocity = f64::from(on.velocity);
            made.start(on.noteId, on.channel, on.pitch, velocity, emit);
        }
        t if t == kNoteOffEvent as uint16 => {
            let off = unsafe { event.__field0.noteOff };
            let velocity = f64::from(off.velocity);
            made.end(off.noteId, off.channel, off.pitch, velocity, emit);
        }
        t if t == kPolyPressureEvent as uint16 => {
            let p = unsafe { event.__field0.polyPressure };
            let pressure = f64::from(p.pressure);
            made.express(
                p.noteId,
                p.channel,
                p.pitch,
                Dimension::Pressure,
                pressure,
                emit,
            );
        }
        t if t == kNoteExpressionValueEvent as uint16 => {
            let expression = unsafe { event.__field0.noteExpressionValue };
            let (id, value) = (expression.noteId, expression.value);
            let (dimension, value) = match expression.typeId {
                kind if kind == kTuningTypeID as NoteExpressionTypeID => {
                    (Dimension::PitchBend, 240.0 * (value - 0.5))
                }
                kind if kind == kBrightnessTypeID as NoteExpressionTypeID => {
                    (Dimension::Timbre, value)
                }
                _ => return,
            };
            // It names its note by id alone.
            if id >= 0 {
                made.express(id, -1, -1, dimension, value, emit);
            }
        }
        t if t == kLegacyMIDICCOutEvent as uint16 => {
            let cc = unsafe { event.__field0.midiCCOut };
            let channel = (cc.channel as u8) & 0x0F;
            let (value, value2) = ((cc.value as u8) & 0x7F, (cc.value2 as u8) & 0x7F);
            let bytes = match cc.controlNumber {
                number @ 0..=127 => [0xB0 | channel, number, value],
                number if number == kAfterTouch as u8 => [0xD0 | channel, value, 0],
                number if number == kPitchBend as u8 => [0xE0 | channel, value, value2],
                number if number == kCtrlPolyPressure as u8 => [0xA0 | channel, value, value2],
                _ => return,
            };
            made.midi(bytes, emit);
        }
        _ => {}
    }
}

/// Calls the plugin's `process` with what's in `audio`, for a block
/// `frames` long, or of no frames to hand over parameter changes, in
/// `mode`, and hands back what the controller is to hear of.
///
/// # Safety
///
/// The caller holds `busy`, with the plugin processing, and `audio`'s buffers
/// at least `frames` long.
unsafe fn process(
    processor: ComRef<'_, IAudioProcessor>,
    audio: &mut Audio,
    frames: usize,
    mode: int32,
) -> tresult {
    audio.out_changes.clear();
    audio.out_events.clear();
    let mut data = ProcessData {
        processMode: mode,
        symbolicSampleSize: kSample32 as int32,
        numSamples: frames as i32,
        numInputs: audio.inputs.buffers.len() as i32,
        numOutputs: audio.outputs.buffers.len() as i32,
        inputs: audio.inputs.buffers.as_mut_ptr(),
        outputs: audio.outputs.buffers.as_mut_ptr(),
        inputParameterChanges: audio.changes.as_ptr(),
        outputParameterChanges: audio.out_changes.as_ptr(),
        inputEvents: if audio.takes_notes {
            audio.events.as_ptr()
        } else {
            std::ptr::null_mut()
        },
        outputEvents: audio.out_events.as_ptr(),
        processContext: &mut audio.context,
    };
    // SAFETY: the caller's promise; everything `data` points to stays put
    // for the call.
    let result = unsafe { processor.process(&mut data) };
    let to_main = &mut audio.to_main;
    let mut send = |id, value| {
        // A full queue drops values the controller would show: it shows
        // the next ones.
        let _ = to_main.push(ParamChange { id, value });
    };
    audio.changes.synced(&mut send);
    audio.out_changes.last_values(&mut send);
    result
}

/// Hands the plugin the changes the main thread has for it, `pending` after
/// the ones waiting for the nodes, as many blocks of no frames as it takes.
/// Says whether there were any.
///
/// # Safety
///
/// The caller holds `busy`, with the plugin processing.
pub(crate) unsafe fn flush(
    processor: ComRef<'_, IAudioProcessor>,
    audio: &mut Audio,
    pending: &mut Vec<ParamChange>,
) -> bool {
    let mut handed = false;
    loop {
        audio.changes.clear();
        audio.events.clear();
        // No more changes than parameters a block can change, so none is
        // dropped.
        while audio.changes.len() < QUEUE_CAPACITY {
            let Ok(change) = audio.from_main.pop() else {
                break;
            };
            audio.changes.push(change.id, 0, change.value, false);
        }
        let room = QUEUE_CAPACITY - audio.changes.len();
        for change in pending.drain(..room.min(pending.len())) {
            audio.changes.push(change.id, 0, change.value, false);
        }
        if audio.changes.is_empty() {
            break;
        }
        handed = true;
        audio.changes.ready();
        // SAFETY: the caller's promise; a block of no frames reads no
        // buffers.
        unsafe { process(processor, audio, 0, kRealtime as int32) };
    }
    audio.changes.clear();
    handed
}

/// Puts the block's parameter changes and events in the plugin's lists:
/// changes from the main thread first, then the node's own, in order.
fn events_for_block(ctx: &ProcessContext<'_>, audio: &mut Audio) {
    audio.changes.clear();
    audio.events.clear();
    while !audio.changes.is_full() {
        let Ok(change) = audio.from_main.pop() else {
            break;
        };
        audio.changes.push(change.id, 0, change.value, false);
    }
    for event in ctx.events {
        let offset = event.offset as i32;
        match event.kind {
            // The switch's bypass is its own.
            NodeEventKind::Param { param, .. } if param == Switch::BYPASS => {}
            NodeEventKind::Param { param, value } => {
                if let Ok(id) = u32::try_from(param.0) {
                    audio.changes.push(id, offset, value, true);
                }
            }
            NodeEventKind::Note(note) => note_events(audio, offset, note),
            // A controller moves the parameter the plugin maps it to, on
            // its channel.
            NodeEventKind::Controller {
                controller,
                value,
                channel,
            } => {
                if let Some(id) = mapped(&audio.controllers, controller, channel) {
                    audio.changes.push(id, offset, value.into(), true);
                }
            }
            // VST3 has no modulation.
            _ => {}
        }
    }
    audio.changes.ready();
}

/// Puts the parameter changes from the main thread and the block's in the
/// plugin's list, all at the block's first frame, for a plugin that isn't
/// processing the block: the block's notes are dropped.
fn param_changes(ctx: &ProcessContext<'_>, audio: &mut Audio) {
    audio.changes.clear();
    while !audio.changes.is_full() {
        let Ok(change) = audio.from_main.pop() else {
            break;
        };
        audio.changes.push(change.id, 0, change.value, false);
    }
    for event in ctx.events {
        match event.kind {
            NodeEventKind::Param { param, value } if param != Switch::BYPASS => {
                if let Ok(id) = u32::try_from(param.0) {
                    audio.changes.push(id, 0, value, true);
                }
            }
            // A controller's parameter, such as the sustain pedal's, comes
            // back where it was left.
            NodeEventKind::Controller {
                controller,
                value,
                channel,
            } => {
                if let Some(id) = mapped(&audio.controllers, controller, channel) {
                    audio.changes.push(id, 0, value.into(), true);
                }
            }
            _ => {}
        }
    }
}

/// The parameter the plugin maps `controller` to on `channel`, or with
/// `None`, on the first.
fn mapped(controllers: &Controllers, controller: Controller, channel: Option<u8>) -> Option<u32> {
    let channel = channel.map_or(0, |c| c.min(15));
    controllers[usize::from(channel)][usize::from(controller.number())]
}

/// The events that play `note` on the plugin, on the first channel, or the
/// one a note wire gives it.
fn note_events(audio: &mut Audio, offset: i32, note: NoteEvent) {
    if !audio.takes_notes {
        return;
    }
    let event = |kind: uint16, payload| Event {
        busIndex: 0,
        sampleOffset: offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kind,
        __field0: payload,
    };
    let off = |sounding: SoundingNote, velocity: f32| {
        event(
            kNoteOffEvent as uint16,
            Event__type0 {
                noteOff: NoteOffEvent {
                    channel: sounding.channel,
                    pitch: sounding.key,
                    velocity,
                    noteId: sounding.id,
                    tuning: 0.0,
                },
            },
        )
    };
    let expression = |sounding: SoundingNote, kind: NoteExpressionTypeID, value: f64| {
        event(
            kNoteExpressionValueEvent as uint16,
            Event__type0 {
                noteExpressionValue: NoteExpressionValueEvent {
                    typeId: kind,
                    noteId: sounding.id,
                    value: value.clamp(0.0, 1.0),
                },
            },
        )
    };
    let event = match note {
        NoteEvent::On {
            note,
            key,
            velocity,
            channel,
        } => {
            if let Some(old) = audio.sounding.end(note) {
                audio.events.push(off(old, 0.0));
            }
            let channel = channel.map_or(0, |c| c.min(15));
            let Some(sounding) = audio.sounding.start(note, key.min(127), channel) else {
                return;
            };
            event(
                kNoteOnEvent as uint16,
                Event__type0 {
                    noteOn: NoteOnEvent {
                        channel: sounding.channel,
                        pitch: sounding.key,
                        tuning: 0.0,
                        velocity,
                        length: 0,
                        noteId: sounding.id,
                    },
                },
            )
        }
        NoteEvent::Expression {
            note,
            dimension,
            value,
        } => {
            let Some(sounding) = audio.sounding.find(note) else {
                return;
            };
            let value = f64::from(value);
            match dimension {
                // VST3's tuning goes from 120 semitones down to 120 up.
                Dimension::PitchBend => expression(sounding, kTuningTypeID, 0.5 + value / 240.0),
                Dimension::Timbre => expression(sounding, kBrightnessTypeID, value),
                Dimension::Pressure => event(
                    kPolyPressureEvent as uint16,
                    Event__type0 {
                        polyPressure: PolyPressureEvent {
                            channel: sounding.channel,
                            pitch: sounding.key,
                            pressure: value.clamp(0.0, 1.0) as f32,
                            noteId: sounding.id,
                        },
                    },
                ),
            }
        }
        NoteEvent::Off { note, velocity } => {
            let Some(sounding) = audio.sounding.end(note) else {
                return;
            };
            off(sounding, velocity)
        }
    };
    audio.events.push(event);
}

/// The transport as VST3 gives it, for the first frame of a block, or
/// none.
fn context(transport: Option<&Transport>, sample_rate: u32, steady_time: i64) -> Context {
    // SAFETY: the context is numbers, and all zeroes is a valid one.
    let mut context: Context = unsafe { std::mem::zeroed() };
    context.sampleRate = f64::from(sample_rate);
    context.continousTimeSamples = steady_time;
    context.state = kContTimeValid as uint32;
    let Some(transport) = transport else {
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        return context;
    };
    context.state |=
        (kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid) as uint32;
    if transport.playing {
        context.state |= kPlaying as uint32;
    }
    if transport.recording {
        context.state |= kRecording as uint32;
    }
    context.projectTimeSamples = transport.samples.min(i64::MAX as u64) as i64;
    context.projectTimeMusic = transport.beats;
    context.barPositionMusic = transport.bar_start;
    context.tempo = transport.bpm;
    context.timeSigNumerator = transport.signature.numerator().into();
    context.timeSigDenominator = transport.signature.denominator().into();
    if let Some(range) = transport.looping {
        context.state |= (kCycleActive | kCycleValid) as uint32;
        context.cycleStartMusic = range.beats[0];
        context.cycleEndMusic = range.beats[1];
    }
    context
}

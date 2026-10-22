//! Processing a VST3 plugin: the engine's node for it, and what processing
//! needs.

use std::fmt;
use std::sync::Arc;

use musicable_engine::{
    AudioMut, EngineConfig, Inputs, Node, NodeEventKind, NodeMaker, NoteEvent, ProcessContext,
};
use musicable_model::clip::Dimension;
use vst3::ComRef;
use vst3::Steinberg::Vst::Event_::EventTypes_::{
    kNoteExpressionValueEvent, kNoteOffEvent, kNoteOnEvent, kPolyPressureEvent,
};
use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::{kBrightnessTypeID, kTuningTypeID};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kBarPositionValid, kContTimeValid, kCycleActive, kCycleValid, kPlaying, kProjectTimeMusicValid,
    kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
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
use crate::process::{Channels, ParamChange, Sounding, SoundingNote, Transport};
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
    pub(crate) changes: InputChanges,
    pub(crate) out_changes: OutputChanges,
    pub(crate) events: Events,
    pub(crate) out_events: Events,
    pub(crate) sounding: Sounding,
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
            changes: InputChanges::new(),
            out_changes: OutputChanges::new(),
            events: Events::new(EVENT_CAPACITY, false),
            out_events: Events::new(0, true),
            sounding: Sounding::new(),
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
    ) {
        self.config = Some(config);
        self.inputs = Buses::new(inputs, config.max_block);
        self.outputs = Buses::new(outputs, config.max_block);
        self.takes_notes = takes_notes;
        self.changes.clear();
        self.events.clear();
        self.sounding.clear();
        self.context = context(None, config.sample_rate, 0);
        self.steady_time = 0;
    }
}

/// A plugin's audio buses on one side: their channel counts, and which is
/// the main one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BusInfo {
    pub(crate) channels: Vec<u32>,
    pub(crate) main: Option<usize>,
}

/// The buffers for one side of a plugin's audio buses.
#[derive(Default)]
pub(crate) struct Buses {
    pub(crate) channels: Channels,
    buffers: Vec<AudioBusBuffers>,
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
        Self { channels, buffers }
    }

    /// Silences every bus, and gets the buffers ready to hand over.
    fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
        for buffer in &mut self.buffers {
            buffer.silenceFlags = 0;
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
/// The node mixes its inputs into the plugin's main input bus and plays
/// its main output bus. It sends the plugin its parameter changes and
/// notes, and the transport. Until the plugin is active, while it's being
/// set up, and if it fails, the node plays silence.
pub struct Vst3Node {
    shared: Arc<Shared>,
    /// Keeps the plugin's code loaded for as long as the node can call it.
    _module: Arc<Module>,
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

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let shared = &self.shared;
        let Some(_busy) = shared.try_enter() else {
            output.clear();
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        if !run(shared, ctx, inputs, output, audio) {
            output.clear();
        }
    }
}

/// Processes the plugin for one block, holding `busy`: false if it can't.
fn run(
    shared: &Shared,
    ctx: &ProcessContext<'_>,
    inputs: Inputs<'_>,
    output: &mut AudioMut<'_>,
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
    audio.inputs.clear(frames);
    audio.outputs.clear(frames);
    if let Some(main) = audio.inputs.channels.main() {
        audio.inputs.channels.mix_into(main, inputs, frames);
    }
    events_for_block(ctx, audio);
    audio.context = context(
        Some(&Transport::at(ctx)),
        config.sample_rate,
        audio.steady_time,
    );
    // SAFETY: processing, holding `busy`, with buffers `frames` long.
    let result = unsafe { process(processor, audio, frames) };
    audio.steady_time += frames as i64;
    if result != kResultOk {
        return false;
    }
    let Some(main) = audio.outputs.channels.main() else {
        return false;
    };
    let silent = audio.outputs.buffers[main].silenceFlags;
    audio.outputs.channels.play(main, frames, silent, output);
    true
}

/// Calls the plugin's `process` with what's in `audio`, for a block
/// `frames` long, or of no frames to hand over parameter changes, and
/// hands back what the controller is to hear of.
///
/// # Safety
///
/// The caller holds `busy`, with the plugin processing, and `audio`'s buffers
/// at least `frames` long.
unsafe fn process(
    processor: ComRef<'_, IAudioProcessor>,
    audio: &mut Audio,
    frames: usize,
) -> tresult {
    audio.out_changes.clear();
    let mut data = ProcessData {
        processMode: kRealtime as int32,
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
        unsafe { process(processor, audio, 0) };
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
            NodeEventKind::Param { param, value } => {
                audio.changes.push(param.0, offset, value, true);
            }
            NodeEventKind::Note(note) => note_events(audio, offset, note),
            // VST3 has no modulation.
            _ => {}
        }
    }
    audio.changes.ready();
}

/// The events that play `note` on the plugin.
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
                    channel: 0,
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
        } => {
            if let Some(old) = audio.sounding.end(note) {
                audio.events.push(off(old, 0.0));
            }
            let Some(sounding) = audio.sounding.start(note, key.min(127)) else {
                return;
            };
            event(
                kNoteOnEvent as uint16,
                Event__type0 {
                    noteOn: NoteOnEvent {
                        channel: 0,
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
                            channel: 0,
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

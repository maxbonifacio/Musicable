//! Processing a plugin: the engine's node for it, and what processing
//! needs.

use std::fmt;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_TRANSPORT,
    CLAP_NOTE_EXPRESSION_BRIGHTNESS, CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING,
    CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
    CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_LOOP_ACTIVE,
    CLAP_TRANSPORT_IS_PLAYING, CLAP_TRANSPORT_IS_RECORDING, clap_event_header,
    clap_event_transport,
};
use clap_sys::ext::note_ports::{
    CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI_MPE, clap_note_dialect,
};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use musicable_engine::switch::Switch;
use musicable_engine::{
    AudioMut, EngineConfig, Inputs, Node, NodeEvent, NodeEventKind, NodeMaker, NoteEvent, NoteId,
    ProcessContext,
};
use musicable_model::clip::Dimension;

use super::events::{self, Event, InputEvents, OutEvent};
use super::host::Shared;
use super::library::Library;
use super::voices::{VOICE_CAPACITY, Voices};
use crate::midi::{Layout, MidiOut};
use crate::process::{Channels, ParamChange, Sounding, SoundingNote, Transport};
use crate::threads::{ACTIVE, AudioThread, FAILED, PROCESSING};

/// How many events a plugin can get in one block, beyond which they're
/// dropped and counted.
pub(crate) const EVENT_CAPACITY: usize = 4096;

/// Everything processing a plugin needs. Whoever holds the instance's
/// `busy` flag has it: the node processing, or the main thread setting the
/// plugin up.
pub(crate) struct Audio {
    /// What the plugin is activated for, while it is.
    pub(crate) config: Option<EngineConfig>,
    pub(crate) inputs: Ports,
    pub(crate) outputs: Ports,
    /// Where notes go, if the plugin takes them.
    pub(crate) note_port: Option<NotePort>,
    pub(crate) events: InputEvents,
    pub(crate) sounding: Sounding,
    pub(crate) voices: Voices,
    /// Notes and controllers as MIDI, for a plugin that takes MIDI.
    pub(crate) midi: MidiOut,
    /// Frames processed since the plugin was activated.
    pub(crate) steady_time: i64,
    pub(crate) from_main: rtrb::Consumer<ParamChange>,
    pub(crate) to_main: rtrb::Producer<OutEvent>,
}

impl Audio {
    pub(crate) fn new(
        from_main: rtrb::Consumer<ParamChange>,
        to_main: rtrb::Producer<OutEvent>,
    ) -> Self {
        Self {
            config: None,
            inputs: Ports::default(),
            outputs: Ports::default(),
            note_port: None,
            events: InputEvents::with_capacity(EVENT_CAPACITY),
            sounding: Sounding::new(),
            voices: Voices::new(),
            midi: MidiOut::new(Layout::Channel),
            steady_time: 0,
            from_main,
            to_main,
        }
    }

    /// Gets ready for the plugin to activate for `config`, with these
    /// ports.
    pub(crate) fn prepare(
        &mut self,
        config: EngineConfig,
        inputs: &[PortInfo],
        outputs: &[PortInfo],
        note_port: Option<NotePort>,
    ) {
        self.config = Some(config);
        self.inputs = Ports::new(inputs, config.max_block);
        self.outputs = Ports::new(outputs, config.max_block);
        self.note_port = note_port;
        self.events.clear();
        self.sounding.clear();
        self.voices.clear();
        let mpe = note_port.is_some_and(|p| p.dialect == CLAP_NOTE_DIALECT_MIDI_MPE);
        self.midi
            .reset(if mpe { Layout::Mpe } else { Layout::Channel });
        self.steady_time = 0;
    }
}

/// An audio port, as the plugin describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortInfo {
    pub(crate) channels: u32,
    pub(crate) main: bool,
}

/// The note port notes go to, and how they're sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotePort {
    pub(crate) index: u16,
    /// CLAP's own note events, MIDI with MPE, or MIDI.
    pub(crate) dialect: clap_note_dialect,
    /// Whether it takes MIDI, as controllers go, whatever notes go as.
    pub(crate) midi: bool,
}

/// The buffers for one side of a plugin's audio ports.
#[derive(Default)]
pub(crate) struct Ports {
    channels: Channels,
    buffers: Vec<clap_audio_buffer>,
}

impl Ports {
    fn new(ports: &[PortInfo], frames: usize) -> Self {
        let counts: Vec<u32> = ports.iter().map(|p| p.channels).collect();
        // The port the plugin calls main, or the first.
        let main = ports
            .iter()
            .position(|p| p.main)
            .or((!ports.is_empty()).then_some(0));
        let mut channels = Channels::new(&counts, main, frames);
        let buffers = ports
            .iter()
            .enumerate()
            .map(|(port, info)| clap_audio_buffer {
                data32: channels.pointers(port),
                data64: ptr::null_mut(),
                channel_count: info.channels,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        Self { channels, buffers }
    }

    /// Silences every port, and gets the buffers ready to hand over.
    fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
        for buffer in &mut self.buffers {
            buffer.constant_mask = 0;
        }
    }
}

/// Makes the engine's nodes for a plugin instance: see
/// [`Instance::node_source`](super::Instance::node_source). Every node it
/// makes processes the same plugin.
#[derive(Clone)]
pub struct NodeSource {
    pub(crate) shared: Arc<Shared>,
    pub(crate) library: Arc<Library>,
}

impl NodeSource {
    pub fn node(&self) -> ClapNode {
        ClapNode {
            shared: Arc::clone(&self.shared),
            _library: Arc::clone(&self.library),
            transport: transport(None),
            ended: Vec::with_capacity(VOICE_CAPACITY),
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

/// A plugin, as a node in the engine's graph.
///
/// The node mixes its inputs into the plugin's main input port and plays
/// its main output port. It sends the plugin its parameter and note events,
/// and the transport. Until the plugin is active, while it's being set up,
/// and if it fails, the node plays silence.
///
/// Stopping the node, as its device is bypassed, stops the plugin
/// processing. Its parameter changes go to the plugin's `flush` meanwhile,
/// and the plugin resets before it processes again.
pub struct ClapNode {
    shared: Arc<Shared>,
    /// Keeps the plugin's code loaded for as long as the node can call it.
    _library: Arc<Library>,
    /// The transport for the block, where the plugin can see it.
    transport: clap_event_transport,
    /// The notes whose voices the plugin ended in the last block.
    ended: Vec<NoteId>,
    /// Whether the plugin resets before it next processes.
    resetting: bool,
}

impl fmt::Debug for ClapNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClapNode")
            .field("plugin", &self.shared.name)
            .finish_non_exhaustive()
    }
}

impl Node for ClapNode {
    fn latency(&self) -> usize {
        self.shared.latency()
    }

    /// Carries on with a reset due on the same plugin.
    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<ClapNode>()
            && Arc::ptr_eq(&previous.shared, &self.shared)
        {
            self.resetting = previous.resetting;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let Self {
            shared,
            transport,
            ended,
            resetting,
            ..
        } = self;
        ended.clear();
        shared.running.store(true, Ordering::Release);
        let Some(_busy) = shared.try_enter() else {
            output.clear();
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        if !run(shared, transport, resetting, ctx, inputs, output, audio) {
            output.clear();
        }
        // Both hold as many as there are voices, so this doesn't allocate.
        ended.append(&mut audio.voices.ended);
    }

    fn ended_voices(&self) -> &[NoteId] {
        &self.ended
    }

    /// Stops the plugin processing, or has the next flush stop it, if the
    /// main thread has it.
    fn stop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        if let Some(_busy) = self.shared.try_enter() {
            let _audio_thread = AudioThread::enter();
            stop_processing(&self.shared);
        }
    }

    /// Hands the plugin's `flush` the block's parameter changes and
    /// modulation, and the main thread's, at the block's first frame. The
    /// plugin's own changes go back to the main thread, as they do from
    /// processing. A plugin still processing stops first.
    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.ended.clear();
        let shared = &*self.shared;
        let Some(_busy) = shared.try_enter() else {
            return;
        };
        let _audio_thread = AudioThread::enter();
        stop_processing(shared);
        if shared.lifecycle() != ACTIVE {
            return;
        }
        let params = shared.params.load(Ordering::Acquire);
        // SAFETY: the extension lives as long as the plugin.
        let Some(flush) = (unsafe { params.as_ref() }).and_then(|p| p.flush) else {
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        audio.events.clear();
        while !audio.events.is_full() {
            let Ok(change) = audio.from_main.pop() else {
                break;
            };
            audio.events.push(Event::param(0, change.id, change.value));
        }
        for event in ctx.events {
            match event.kind {
                NodeEventKind::Param { param, value } if param != Switch::BYPASS => {
                    audio.events.push(Event::param(0, param.0, value));
                }
                NodeEventKind::Modulation {
                    param,
                    amount,
                    note: None,
                } => audio.events.push(Event::modulation(0, param.0, amount)),
                _ => {}
            }
        }
        if audio.events.is_empty() {
            return;
        }
        let in_events = audio.events.list();
        let mut out = events::Output {
            queue: &mut audio.to_main,
            voices: None,
        };
        let out_events = events::output_list(&mut out);
        // SAFETY: active and not processing, on an audio thread, with lists
        // that stay put for the call.
        unsafe { flush(shared.plugin(), &in_events, &out_events) };
        audio.events.clear();
    }

    /// Has the plugin reset before it next processes.
    fn reset(&mut self) {
        self.resetting = true;
    }
}

/// Stops the plugin processing, if it is, holding `busy` on an audio
/// thread.
fn stop_processing(shared: &Shared) {
    if shared.lifecycle() != PROCESSING {
        return;
    }
    let plugin = shared.plugin();
    // SAFETY: a processing plugin stays alive until the main thread, which
    // waits for `busy`, deactivates it.
    if let Some(stop) = unsafe { (*plugin).stop_processing } {
        // SAFETY: processing, on an audio thread.
        unsafe { stop(plugin) };
    }
    shared.set_lifecycle(ACTIVE);
}

/// Processes the plugin for one block, holding `busy`: false if it can't.
/// It resets first if `resetting`.
fn run(
    shared: &Shared,
    transport_event: &mut clap_event_transport,
    resetting: &mut bool,
    ctx: &ProcessContext<'_>,
    inputs: Inputs<'_>,
    output: &mut AudioMut<'_>,
    audio: &mut Audio,
) -> bool {
    let lifecycle = shared.lifecycle();
    if lifecycle != ACTIVE && lifecycle != PROCESSING {
        return false;
    }
    let Some(config) = audio.config else {
        return false;
    };
    let frames = ctx.frames;
    if frames > config.max_block || ctx.sample_rate != config.sample_rate || frames == 0 {
        return false;
    }
    let plugin = shared.plugin();
    // SAFETY: an active plugin stays alive until the main thread, which
    // waits for `busy`, deactivates it.
    let clap = unsafe { &*plugin };
    let _audio_thread = AudioThread::enter();
    if lifecycle == ACTIVE {
        // SAFETY: active and not processing, on an audio thread.
        let started = clap
            .start_processing
            .is_none_or(|start| unsafe { start(plugin) });
        if !started {
            shared.set_lifecycle(FAILED);
            return false;
        }
        shared.set_lifecycle(PROCESSING);
    }
    if std::mem::take(resetting) {
        // SAFETY: active, on an audio thread.
        if let Some(reset) = clap.reset {
            // SAFETY: as above.
            unsafe { reset(plugin) };
        }
        // Its voices are gone.
        audio.sounding.clear();
        audio.voices.clear();
        audio.midi.restart();
    }

    audio.inputs.clear(frames);
    audio.outputs.clear(frames);
    if let Some(main) = audio.inputs.channels.main() {
        audio.inputs.channels.mix_into(main, inputs, frames);
    }
    events_for_block(ctx, audio);
    *transport_event = transport(Some(ctx));

    let in_events = audio.events.list();
    let mut out = events::Output {
        queue: &mut audio.to_main,
        voices: Some(&mut audio.voices),
    };
    let out_events = events::output_list(&mut out);
    let process = clap_process {
        steady_time: audio.steady_time,
        frames_count: frames as u32,
        transport: transport_event,
        audio_inputs: audio.inputs.buffers.as_ptr(),
        audio_outputs: audio.outputs.buffers.as_mut_ptr(),
        audio_inputs_count: audio.inputs.buffers.len() as u32,
        audio_outputs_count: audio.outputs.buffers.len() as u32,
        in_events: &in_events,
        out_events: &out_events,
    };
    // SAFETY: processing, on an audio thread, with buffers `frames`
    // long and event lists that stay put for the call.
    let status = clap
        .process
        .map_or(CLAP_PROCESS_ERROR, |process_fn| unsafe {
            process_fn(plugin, &process)
        });
    audio.steady_time += frames as i64;
    if status == CLAP_PROCESS_ERROR {
        return false;
    }
    let Some(main) = audio.outputs.channels.main() else {
        return false;
    };
    audio.outputs.channels.play(main, frames, 0, output);
    true
}

/// Puts the block's events in the plugin's list: parameter changes from the
/// main thread first, then the node's own events, in order.
fn events_for_block(ctx: &ProcessContext<'_>, audio: &mut Audio) {
    audio.events.clear();
    while !audio.events.is_full() {
        let Ok(change) = audio.from_main.pop() else {
            break;
        };
        audio.events.push(Event::param(0, change.id, change.value));
    }
    // A plugin that takes notes as MIDI hears how its channels are laid out
    // before anything else.
    if let Some(port) = audio
        .note_port
        .filter(|p| p.dialect != CLAP_NOTE_DIALECT_CLAP)
    {
        let Audio { midi, events, .. } = audio;
        midi.start(&mut |bytes| events.push(Event::midi(0, port.index, bytes)));
    }
    for (i, event) in ctx.events.iter().enumerate() {
        let time = event.offset as u32;
        match event.kind {
            // The switch's bypass is its own.
            NodeEventKind::Param { param, .. } if param == Switch::BYPASS => {}
            NodeEventKind::Param { param, value } => {
                audio.events.push(Event::param(time, param.0, value));
            }
            NodeEventKind::Modulation {
                param,
                amount,
                note: None,
            } => {
                audio.events.push(Event::modulation(time, param.0, amount));
            }
            NodeEventKind::Modulation {
                param,
                amount,
                note: Some(note),
            } => {
                // The voice the plugin plays the note with, while it sounds,
                // by its note id, or by its channel and key for a plugin that
                // takes MIDI.
                let (Some(port), Some(voice)) = (audio.note_port, audio.voices.find(note)) else {
                    continue;
                };
                let id = if port.dialect == CLAP_NOTE_DIALECT_CLAP {
                    voice.id
                } else {
                    -1
                };
                audio.events.push(Event::note_modulation(
                    time,
                    param.0,
                    amount,
                    id,
                    port.index as i16,
                    voice.channel,
                    voice.key,
                ));
            }
            NodeEventKind::Note(note) => {
                note_events(audio, event.offset, note, &ctx.events[i + 1..])
            }
            // Controllers go as MIDI, to a plugin that takes it.
            NodeEventKind::Controller { controller, value } => {
                if let Some(port) = audio.note_port.filter(|p| p.midi) {
                    let Audio { midi, events, .. } = audio;
                    let mut emit = |bytes| events.push(Event::midi(time, port.index, bytes));
                    midi.controller(controller, value, &mut emit);
                }
            }
            _ => {}
        }
    }
}

/// The events that play `note` on the plugin, in its note dialect: `after`
/// is the block's events after it.
fn note_events(audio: &mut Audio, offset: usize, note: NoteEvent, after: &[NodeEvent]) {
    let Some(port) = audio.note_port else {
        return;
    };
    if port.dialect == CLAP_NOTE_DIALECT_CLAP {
        clap_note_events(audio, port, offset as u32, note);
        return;
    }
    let time = offset as u32;
    let Audio {
        midi,
        events,
        sounding,
        voices,
        ..
    } = audio;
    let mut emit = |bytes| events.push(Event::midi(time, port.index, bytes));
    midi.note(note, offset, after, &mut emit);
    match note {
        NoteEvent::On { note, key, .. } => {
            sounding.end(note);
            // The voice, on the channel it went to, for modulation.
            if let (Some(mut voice), Some((channel, _))) =
                (sounding.start(note, key.min(127)), midi.find(note))
            {
                voice.channel = channel.into();
                voices.start(voice);
            }
        }
        NoteEvent::Off { note, .. } => {
            sounding.end(note);
        }
        NoteEvent::Expression { .. } => {}
    }
}

/// The events that play `note` on a plugin that takes CLAP's own.
fn clap_note_events(audio: &mut Audio, port: NotePort, time: u32, note: NoteEvent) {
    let index = port.index as i16;
    let end = |audio: &mut Audio, sounding: SoundingNote, velocity: f32| {
        audio.events.push(Event::note(
            CLAP_EVENT_NOTE_OFF,
            time,
            sounding.id,
            index,
            sounding.key,
            velocity.into(),
        ));
    };
    match note {
        NoteEvent::On {
            note,
            key,
            velocity,
        } => {
            if let Some(old) = audio.sounding.end(note) {
                end(audio, old, 0.0);
            }
            let Some(sounding) = audio.sounding.start(note, key.min(127)) else {
                audio.events.dropped += 1;
                return;
            };
            audio.voices.start(sounding);
            audio.events.push(Event::note(
                CLAP_EVENT_NOTE_ON,
                time,
                sounding.id,
                index,
                sounding.key,
                velocity.into(),
            ));
        }
        NoteEvent::Expression {
            note,
            dimension,
            value,
        } => {
            let Some(sounding) = audio.sounding.find(note) else {
                return;
            };
            let expression = match dimension {
                Dimension::PitchBend => CLAP_NOTE_EXPRESSION_TUNING,
                Dimension::Pressure => CLAP_NOTE_EXPRESSION_PRESSURE,
                Dimension::Timbre => CLAP_NOTE_EXPRESSION_BRIGHTNESS,
            };
            audio.events.push(Event::expression(
                time,
                expression,
                sounding.id,
                index,
                sounding.key,
                value.into(),
            ));
        }
        NoteEvent::Off { note, velocity } => {
            if let Some(sounding) = audio.sounding.end(note) {
                end(audio, sounding, velocity);
            }
        }
    }
}

/// The transport as CLAP gives it, for the first frame of the block `ctx`
/// is for, or none.
fn transport(ctx: Option<&ProcessContext<'_>>) -> clap_event_transport {
    let header = clap_event_header {
        size: size_of::<clap_event_transport>() as u32,
        time: 0,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_: CLAP_EVENT_TRANSPORT,
        flags: 0,
    };
    let beattime = |beats: f64| (beats * CLAP_BEATTIME_FACTOR as f64).round() as i64;
    let sectime = |seconds: f64| (seconds * CLAP_SECTIME_FACTOR as f64).round() as i64;
    let Some(ctx) = ctx else {
        return clap_event_transport {
            header,
            flags: 0,
            song_pos_beats: 0,
            song_pos_seconds: 0,
            tempo: 0.0,
            tempo_inc: 0.0,
            loop_start_beats: 0,
            loop_end_beats: 0,
            loop_start_seconds: 0,
            loop_end_seconds: 0,
            bar_start: 0,
            bar_number: 0,
            tsig_num: 4,
            tsig_denom: 4,
        };
    };
    let at = Transport::at(ctx);
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO
        | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
        | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
        | CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
    if at.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    if at.recording {
        flags |= CLAP_TRANSPORT_IS_RECORDING;
    }
    let mut looping = [0; 4];
    if let Some(range) = at.looping {
        flags |= CLAP_TRANSPORT_IS_LOOP_ACTIVE;
        looping = [
            beattime(range.beats[0]),
            beattime(range.beats[1]),
            sectime(range.seconds[0]),
            sectime(range.seconds[1]),
        ];
    }
    clap_event_transport {
        header,
        flags,
        song_pos_beats: beattime(at.beats),
        song_pos_seconds: sectime(at.seconds),
        tempo: at.bpm,
        tempo_inc: 0.0,
        loop_start_beats: looping[0],
        loop_end_beats: looping[1],
        loop_start_seconds: looping[2],
        loop_end_seconds: looping[3],
        bar_start: beattime(at.bar_start),
        bar_number: at.bar.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
        tsig_num: at.signature.numerator().into(),
        tsig_denom: at.signature.denominator().into(),
    }
}

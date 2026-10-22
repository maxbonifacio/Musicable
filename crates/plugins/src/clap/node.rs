//! Processing a plugin: the engine's node for it, and what processing
//! needs.

use std::fmt;
use std::ptr;
use std::slice;
use std::sync::Arc;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_TRANSPORT,
    CLAP_NOTE_EXPRESSION_BRIGHTNESS, CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING,
    CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
    CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_LOOP_ACTIVE,
    CLAP_TRANSPORT_IS_PLAYING, CLAP_TRANSPORT_IS_RECORDING, clap_event_header,
    clap_event_transport,
};
use clap_sys::ext::note_ports::{CLAP_NOTE_DIALECT_CLAP, clap_note_dialect};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::id::clap_id;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use musicable_engine::{
    AudioMut, EngineConfig, Inputs, Node, NodeEventKind, NodeMaker, NoteEvent, NoteId,
    ProcessContext,
};
use musicable_model::clip::Dimension;
use musicable_model::time::Beats;

use super::events::{self, Event, InputEvents, OutEvent};
use super::host::{ACTIVE, AudioThread, FAILED, PROCESSING, Shared};
use super::library::Library;

/// How many events a plugin can get in one block, beyond which they're
/// dropped and counted.
pub(crate) const EVENT_CAPACITY: usize = 4096;
/// How many notes can sound on a plugin at once.
const SOUNDING_CAPACITY: usize = 512;

/// A parameter change from the main thread, for the next block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ParamChange {
    pub(crate) id: clap_id,
    pub(crate) value: f64,
}

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
    /// CLAP's own note events, or MIDI.
    pub(crate) dialect: clap_note_dialect,
}

/// The buffers for one side of a plugin's audio ports.
pub(crate) struct Ports {
    /// The samples of every channel of every port, one channel after
    /// another. Only ever touched through `channels`, which point into it.
    _samples: Vec<f32>,
    /// One pointer per channel, port after port.
    channels: Vec<*mut f32>,
    buffers: Vec<clap_audio_buffer>,
    /// Where each port's channels start in `channels`.
    first: Vec<usize>,
    /// The port audio goes in or comes out of: the one the plugin calls
    /// main, or the first.
    main: Option<usize>,
    frames: usize,
}

impl Default for Ports {
    fn default() -> Self {
        Self::new(&[], 0)
    }
}

impl Ports {
    fn new(ports: &[PortInfo], frames: usize) -> Self {
        let total: usize = ports.iter().map(|p| p.channels as usize).sum();
        let mut samples = vec![0.0f32; total * frames];
        let base = samples.as_mut_ptr();
        // SAFETY: each channel starts inside `samples`, or at its end when
        // it holds nothing.
        let mut channels: Vec<*mut f32> = (0..total)
            .map(|c| unsafe { base.add(c * frames) })
            .collect();
        let mut first = Vec::with_capacity(ports.len());
        let mut start = 0;
        for port in ports {
            first.push(start);
            start += port.channels as usize;
        }
        let pointers = channels.as_mut_ptr();
        let buffers = ports
            .iter()
            .zip(&first)
            .map(|(port, &first)| clap_audio_buffer {
                // SAFETY: `first` is at most the number of channels.
                data32: unsafe { pointers.add(first) },
                data64: ptr::null_mut(),
                channel_count: port.channels,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        let main = ports
            .iter()
            .position(|p| p.main)
            .or((!ports.is_empty()).then_some(0));
        Self {
            _samples: samples,
            channels,
            buffers,
            first,
            main,
            frames,
        }
    }

    /// Port `port`'s channels, `frames` long.
    fn port_channels(&self, port: usize, frames: usize) -> impl Iterator<Item = *mut f32> + '_ {
        debug_assert!(frames <= self.frames);
        let count = self.buffers[port].channel_count as usize;
        self.channels[self.first[port]..][..count].iter().copied()
    }

    /// Silences every port, and gets the buffers ready to hand over.
    fn clear(&mut self, frames: usize) {
        for &channel in &self.channels {
            // SAFETY: every channel holds `self.frames` samples, and
            // nothing else uses them now.
            unsafe { slice::from_raw_parts_mut(channel, frames) }.fill(0.0);
        }
        for buffer in &mut self.buffers {
            buffer.constant_mask = 0;
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
    note: NoteId,
    id: i32,
    key: i16,
}

impl Sounding {
    fn new() -> Self {
        Self {
            notes: Vec::with_capacity(SOUNDING_CAPACITY),
            next_id: 0,
        }
    }

    fn clear(&mut self) {
        self.notes.clear();
    }

    /// Starts `note` on `key`, unless too many notes are sounding.
    fn start(&mut self, note: NoteId, key: u8) -> Option<SoundingNote> {
        if self.notes.len() == self.notes.capacity() {
            return None;
        }
        let sounding = SoundingNote {
            note,
            id: self.next_id,
            key: key.into(),
        };
        // CLAP's note ids are never negative.
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        self.notes.push(sounding);
        Some(sounding)
    }

    fn find(&self, note: NoteId) -> Option<SoundingNote> {
        self.notes.iter().find(|n| n.note == note).copied()
    }

    fn end(&mut self, note: NoteId) -> Option<SoundingNote> {
        let index = self.notes.iter().position(|n| n.note == note)?;
        Some(self.notes.swap_remove(index))
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
pub struct ClapNode {
    shared: Arc<Shared>,
    /// Keeps the plugin's code loaded for as long as the node can call it.
    _library: Arc<Library>,
    /// The transport for the block, where the plugin can see it.
    transport: clap_event_transport,
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

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let Self {
            shared, transport, ..
        } = self;
        let Some(_busy) = shared.try_enter() else {
            output.clear();
            return;
        };
        // SAFETY: `busy` is held, so nothing else touches it.
        let audio = unsafe { &mut *shared.audio.get() };
        if !run(shared, transport, ctx, inputs, output, audio) {
            output.clear();
        }
    }
}

/// Processes the plugin for one block, holding `busy`: false if it can't.
fn run(
    shared: &Shared,
    transport_event: &mut clap_event_transport,
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

    audio.inputs.clear(frames);
    audio.outputs.clear(frames);
    if let Some(main) = audio.inputs.main {
        mix_into(&audio.inputs, main, inputs, frames);
    }
    events_for_block(ctx, audio);
    *transport_event = transport(Some(ctx));

    let in_events = audio.events.list();
    let out_events = events::output_list(&mut audio.to_main);
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
    let Some(main) = audio.outputs.main else {
        return false;
    };
    play(&audio.outputs, main, frames, output);
    true
}

/// Mixes the node's inputs into an input port.
fn mix_into(ports: &Ports, port: usize, inputs: Inputs<'_>, frames: usize) {
    let mono = ports.buffers[port].channel_count == 1;
    for (c, channel) in ports.port_channels(port, frames).enumerate() {
        // SAFETY: the port's buffers hold `frames` samples, and the plugin
        // isn't using them.
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

/// Plays an output port: its first two channels, or its one channel on
/// both sides.
fn play(ports: &Ports, port: usize, frames: usize, output: &mut AudioMut<'_>) {
    let channels: [Option<*mut f32>; 2] = {
        let mut port_channels = ports.port_channels(port, frames);
        let first = port_channels.next();
        [first, port_channels.next().or(first)]
    };
    for c in 0..output.channels() {
        match channels.get(c).copied().flatten() {
            // SAFETY: the port's buffers hold `frames` samples, which the
            // plugin wrote and is done with.
            Some(channel) => output
                .channel_mut(c)
                .copy_from_slice(unsafe { slice::from_raw_parts(channel, frames) }),
            None => output.channel_mut(c).fill(0.0),
        }
    }
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
    for event in ctx.events {
        let time = event.offset as u32;
        match event.kind {
            NodeEventKind::Param { param, value } => {
                audio.events.push(Event::param(time, param.0, value));
            }
            NodeEventKind::Modulation { param, amount } => {
                audio.events.push(Event::modulation(time, param.0, amount));
            }
            NodeEventKind::Note(note) => note_events(audio, time, note),
            _ => {}
        }
    }
}

/// The events that play `note` on the plugin, in its note dialect.
fn note_events(audio: &mut Audio, time: u32, note: NoteEvent) {
    let Some(port) = audio.note_port else {
        return;
    };
    let clap = port.dialect == CLAP_NOTE_DIALECT_CLAP;
    let index = port.index as i16;
    let end = |audio: &mut Audio, sounding: SoundingNote, velocity: f32| {
        audio.events.push(if clap {
            Event::note(
                CLAP_EVENT_NOTE_OFF,
                time,
                sounding.id,
                index,
                sounding.key,
                velocity.into(),
            )
        } else {
            Event::midi(
                time,
                port.index,
                [0x80, sounding.key as u8, midi_velocity(velocity, 0)],
            )
        });
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
            audio.events.push(if clap {
                Event::note(
                    CLAP_EVENT_NOTE_ON,
                    time,
                    sounding.id,
                    index,
                    sounding.key,
                    velocity.into(),
                )
            } else {
                Event::midi(
                    time,
                    port.index,
                    [0x90, sounding.key as u8, midi_velocity(velocity, 1)],
                )
            });
        }
        NoteEvent::Expression {
            note,
            dimension,
            value,
        } => {
            // MIDI has no way to change one note's expression without MPE.
            let Some(sounding) = audio.sounding.find(note).filter(|_| clap) else {
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

/// A velocity from 0.0 to 1.0 as MIDI's, no lower than `min`.
fn midi_velocity(velocity: f32, min: u8) -> u8 {
    ((velocity * 127.0).round() as i32).clamp(min.into(), 127) as u8
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
    let (rate, tempo) = (ctx.sample_rate.max(1), ctx.tempo);
    let seconds_per_sample = 1.0 / f64::from(rate);
    let position = ctx.transport.position as f64;
    let beats = tempo.beats_at(position, rate);
    let at = Beats::from_f64(beats);
    let bar = tempo.bar_at(at);
    let signature = tempo.signature_at(at);
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO
        | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
        | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
        | CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
    if ctx.transport.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    if ctx.transport.recording {
        flags |= CLAP_TRANSPORT_IS_RECORDING;
    }
    let mut looping = [0; 4];
    if let Some(range) = ctx.transport.looping {
        flags |= CLAP_TRANSPORT_IS_LOOP_ACTIVE;
        let seconds = |at: Beats| tempo.sample_at(at, rate) * seconds_per_sample;
        looping = [
            beattime(range.start.to_f64()),
            beattime(range.end.to_f64()),
            sectime(seconds(range.start)),
            sectime(seconds(range.end)),
        ];
    }
    clap_event_transport {
        header,
        flags,
        song_pos_beats: beattime(beats),
        song_pos_seconds: sectime(position * seconds_per_sample),
        tempo: tempo.bpm_at(at),
        tempo_inc: 0.0,
        loop_start_beats: looping[0],
        loop_end_beats: looping[1],
        loop_start_seconds: looping[2],
        loop_end_seconds: looping[3],
        bar_start: beattime(tempo.bar_start(bar.bar).to_f64()),
        bar_number: bar.bar.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
        tsig_num: signature.numerator().into(),
        tsig_denom: signature.denominator().into(),
    }
}

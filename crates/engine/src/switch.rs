//! The switch a device's node sits in, which bypasses it: see [`Switch`],
//! and [`NoteSwitch`] for a MIDI effect's.

use std::collections::VecDeque;
use std::ops::Range;

use crate::graph::Routed;
use crate::nodes::{Delay, Smoothed};
use crate::{
    AudioBuffer, AudioMut, Automation, EngineConfig, Inputs, MidiMessage, Node, NodeEvent,
    NodeEventKind, NoteId, Notes, ParamId, ProcessContext,
};

/// A device's node, in the switch that bypasses it. The compiler puts one
/// in each device's place, with the device's key.
///
/// While the device plays, the switch plays the device and keeps a copy of
/// what comes into its main input, delayed by the device's latency: the dry
/// copy. The device hears its side inputs too, such as a compressor's
/// sidechain, but they never play in its place.
/// Bypassing the device ([`BYPASS`](Self::BYPASS)) crossfades to the dry
/// copy over 10 ms, so nothing else in the mix moves, and stops the device
/// ([`Node::stop`]) once the fade is over. While it's stopped, it gets its
/// events through [`Node::flush`], and only the dry copy costs anything.
/// Bringing it back starts it afresh at once ([`Node::reset`]), and
/// crossfades back to it after its latency, so a device that looks ahead
/// has caught up by the time it's heard.
///
/// A switch applies to what comes into the device from the frame its event
/// is on, so the fade starts at the output the device's latency later,
/// where that frame comes out. After delay compensation, a
/// [`Bypass`](musicable_model::param::ParamAddress::Bypass) lane switches on
/// the frame it asks for. Where the device starts or stops partway through
/// a block, it renders that part of the block as a block of its own, so
/// the switch renders the same whatever the blocks.
///
/// A deactivated device's switch ([`deactivated`](Self::deactivated))
/// never starts its device, and plays silence. The compiler leaves it out
/// of the signal, so the device's latency goes too, but keeps it in the
/// graph, so a plugin stops on the audio thread when the graph takes over,
/// and goes on taking its parameter changes.
pub struct Switch {
    device: Box<dyn Node>,
    /// Whether the device is bypassed, as of the last frame that came in.
    bypassed: bool,
    /// Whether the device renders: from the frame it's brought back until
    /// the fade to the dry copy is over.
    running: bool,
    deactivated: bool,
    /// Whether the device was bypassed in the project the switch was built
    /// from.
    built: bool,
    /// Whether a lane moves the switch in this graph.
    automated: bool,
    /// A switch to make at the start of the next block, before its events:
    /// back to the project's state, once a lane stops moving it.
    pending: Option<bool>,
    /// Whether the switch has rendered a frame: until it has, a switch
    /// applies at once, as a new device has nothing to fade from.
    started: bool,
    /// The device's latency, as it was when the switch was prepared.
    latency: usize,
    /// Makes the dry copy.
    dry: Delay,
    /// The dry copy, for the blocks that mix it with the device.
    dry_copy: AudioBuffer,
    /// How far over to the dry copy the output is, in frames of the fade:
    /// 0 plays the device, and `fade` the dry copy.
    over: u32,
    /// Which way the output is heading: to the dry copy, or to the device.
    to_dry: bool,
    /// How many frames a fade takes.
    fade: u32,
    /// The switches due at the output, oldest first: the engine time each
    /// fade starts at, and which way it goes.
    due: VecDeque<(u64, bool)>,
    /// The events of a part of a block the device renders, moved to their
    /// offsets in the part.
    shifted: Vec<NodeEvent>,
}

/// How many switches can be on their way through a device at once. One
/// more replaces the last, so the latest always lands.
const DUE: usize = 16;

/// How many events a part of a block can hand the device at once. Beyond
/// them, the part is cut in two, and more than this on one frame come a
/// frame late.
const SHIFTED: usize = 256;

impl Switch {
    /// The switch's parameter: 1.0 bypasses the device, and 0.0 brings it
    /// back. CLAP (`CLAP_INVALID_ID`) and VST3 (`kNoParamId`) keep its id
    /// from plugins, so it's never one of a plugin's, and the device's node
    /// ignores it.
    pub const BYPASS: ParamId = ParamId(u32::MAX as u64);

    /// Puts `device`, a device's node, in a switch, bypassed or not.
    pub fn new(device: Box<dyn Node>, bypassed: bool) -> Self {
        Self {
            device,
            bypassed,
            running: !bypassed,
            deactivated: false,
            built: bypassed,
            automated: false,
            pending: None,
            started: false,
            latency: 0,
            dry: Delay::new(0),
            dry_copy: AudioBuffer::default(),
            over: 0,
            to_dry: bypassed,
            fade: 1,
            due: VecDeque::new(),
            shifted: Vec::new(),
        }
    }

    /// A deactivated device's switch: it never starts `device`, and plays
    /// silence, with no latency. A device that was playing in the graph
    /// before stops, and its node still takes its parameter changes
    /// ([`Node::flush`]).
    pub fn deactivated(device: Box<dyn Node>) -> Self {
        Self {
            deactivated: true,
            ..Self::new(device, true)
        }
    }

    /// Whether the device is bypassed, as of the last frame rendered.
    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// Whether the device renders: not while it's bypassed, once the fade
    /// is over, or while it's deactivated.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The device's node.
    pub fn device(&self) -> &dyn Node {
        &*self.device
    }

    pub fn device_mut(&mut self) -> &mut dyn Node {
        &mut *self.device
    }

    /// Whether the output has got where it's heading.
    fn settled(&self) -> bool {
        self.over == if self.to_dry { self.fade } else { 0 }
    }

    /// Whether the device is fading out for good: bypassed, with no
    /// switch on its way.
    fn leaving(&self) -> bool {
        self.running && self.bypassed && self.to_dry && self.due.is_empty()
    }

    /// A switch coming into the device at engine time `time`.
    fn switch(&mut self, time: u64, bypassed: bool) {
        if bypassed == self.bypassed {
            return;
        }
        self.bypassed = bypassed;
        if !self.started {
            // Nothing has played yet, so it's as if the switch had been
            // built this way.
            self.to_dry = bypassed;
            self.over = if bypassed { self.fade } else { 0 };
            if !bypassed && !self.running {
                self.device.reset();
            }
            self.running = !bypassed;
            return;
        }
        let due = (time + self.latency as u64, bypassed);
        if self.due.len() == DUE {
            self.due.pop_back();
        }
        self.due.push_back(due);
        if !bypassed && !self.running {
            self.device.reset();
            self.running = true;
        }
    }

    /// Mixes the dry copy into `frames` of `output`, which hold the
    /// device's output while it runs, as the fade has it.
    fn mix(&mut self, frames: Range<usize>, output: &mut AudioMut<'_>) {
        let dry = self.dry_copy.view(output.frames()).range(frames.clone());
        let mut out = output.range_mut(frames.clone());
        if !self.running || (self.settled() && self.to_dry) {
            out.copy_from(dry);
            return;
        }
        if self.settled() {
            return;
        }
        let (over, to_dry, fade) = (self.over, self.to_dry, self.fade);
        for (c, channel) in out.channels_mut().enumerate() {
            let mut at = over;
            for (sample, dry) in channel.iter_mut().zip(dry.channel(c)) {
                at = if to_dry {
                    (at + 1).min(fade)
                } else {
                    at.saturating_sub(1)
                };
                let m = at as f32 / fade as f32;
                *sample = *sample * (1.0 - m) + dry * m;
            }
        }
        let len = frames.len() as u32;
        self.over = if to_dry {
            over.saturating_add(len).min(fade)
        } else {
            over.saturating_sub(len)
        };
    }

    /// Renders a block in parts that end where something switches.
    fn switch_block(
        &mut self,
        ctx: &ProcessContext<'_>,
        inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        let frames = ctx.frames;
        self.dry
            .process(ctx, inputs, &mut self.dry_copy.view_mut(frames));
        if let Some(bypassed) = self.pending.take() {
            self.switch(ctx.time, bypassed);
        }
        let mut at = 0;
        // The first event not yet looked at for a switch.
        let mut next = 0;
        loop {
            while let Some(event) = ctx.events.get(next)
                && event.offset <= at
            {
                if let Some(bypassed) = switch_value(event) {
                    self.switch(ctx.time + at as u64, bypassed);
                }
                next += 1;
            }
            let now = ctx.time + at as u64;
            while let Some(&(time, to_dry)) = self.due.front()
                && time <= now
            {
                self.to_dry = to_dry;
                self.due.pop_front();
            }
            if self.leaving() && self.settled() {
                self.device.stop();
                self.running = false;
            }
            if at == frames {
                break;
            }
            let mut end = frames;
            if let Some(event) = ctx.events[next..]
                .iter()
                .find(|e| switch_value(e).is_some())
            {
                end = end.min(event.offset);
            }
            if let Some(&(time, _)) = self.due.front() {
                end = end.min(usize::try_from(time - ctx.time).unwrap_or(usize::MAX));
            }
            if self.leaving() {
                end = end.min(at + (self.fade - self.over) as usize);
            }
            debug_assert!(end > at, "each part has a frame");
            if self.running {
                let device = &mut *self.device;
                render_part(device, &mut self.shifted, ctx, inputs, at..end, output);
            } else {
                let events = events_in(ctx.events, at..end);
                self.device.flush(&ProcessContext { events, ..*ctx });
            }
            self.mix(at..end, output);
            self.started = true;
            at = end;
        }
    }
}

impl Node for Switch {
    fn channels(&self) -> usize {
        self.device.channels()
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.device.prepare(config);
        self.latency = if self.deactivated {
            0
        } else {
            self.device.latency()
        };
        let channels = self.device.channels();
        self.dry = Delay::new(self.latency).with_channels(channels);
        self.dry.prepare(config);
        self.dry_copy = AudioBuffer::new(channels, config.max_block);
        let fade = Smoothed::RAMP_SECONDS * f64::from(config.sample_rate);
        self.fade = (fade.round() as u32).max(1);
        self.over = if self.to_dry { self.fade } else { 0 };
        self.due = VecDeque::with_capacity(DUE);
        self.shifted = Vec::with_capacity(SHIFTED);
    }

    fn latency(&self) -> usize {
        self.latency
    }

    /// Its own parameter takes its automation as events, which switch it
    /// on the frame of each step. The device's take theirs as the device
    /// does.
    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        match param {
            Self::BYPASS if self.deactivated => None,
            Self::BYPASS => {
                self.automated = true;
                Some(automation)
            }
            _ => self.device.automate(param, automation),
        }
    }

    fn modulated(&mut self, param: ParamId) {
        if param != Self::BYPASS {
            self.device.modulated(param);
        }
    }

    /// Carries on with the switch it replaces, its device's state and any
    /// fade under way. Once a lane stops moving it, it goes back to the
    /// project's state, which a value sent since overrides. A device that
    /// was deactivated starts afresh, and one that's deactivated now stops.
    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<Switch>() else {
            return;
        };
        self.device.adopt(&mut *previous.device);
        match (previous.deactivated, self.deactivated) {
            (false, false) => {
                self.dry.adopt(&mut previous.dry);
                self.started = previous.started;
                self.bypassed = previous.bypassed;
                self.running = previous.running;
                self.over = previous.over.min(self.fade);
                self.to_dry = previous.to_dry;
                std::mem::swap(&mut self.due, &mut previous.due);
                self.pending = previous.pending;
                if previous.automated && !self.automated {
                    self.pending = Some(self.built);
                }
            }
            (false, true) if previous.running => self.device.stop(),
            (true, false) if self.running => self.device.reset(),
            _ => {}
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        if self.deactivated {
            self.device.flush(ctx);
            output.clear();
            return;
        }
        let switching = ctx.events.iter().any(|e| switch_value(e).is_some());
        if switching || self.pending.is_some() || !self.due.is_empty() || !self.settled() {
            self.switch_block(ctx, inputs, output);
            return;
        }
        self.started = true;
        if self.running {
            // The dry copy carries on, ready for a switch.
            if self.latency > 0 {
                let mut dry = self.dry_copy.view_mut(ctx.frames);
                self.dry.process(ctx, inputs, &mut dry);
            }
            self.device.process(ctx, inputs, output);
        } else {
            self.device.flush(ctx);
            self.dry.process(ctx, inputs, output);
        }
    }

    /// A bypassed or deactivated device passes the notes sent to it on
    /// unchanged, for a device whose notes go somewhere.
    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        if !self.bypassed {
            self.device.play_notes(ctx, notes);
            return;
        }
        for event in ctx.events {
            match event.kind {
                NodeEventKind::Note(note) => {
                    notes.send(event.offset, note);
                }
                NodeEventKind::Controller { controller, value } => {
                    notes.send_controller(event.offset, controller, value);
                }
                _ => {}
            }
        }
    }

    fn ended_voices(&self) -> &[NoteId] {
        self.device.ended_voices()
    }

    fn live(&self) -> Option<bool> {
        self.device.live()
    }
}

/// A MIDI effect's node, in the switch that bypasses it: as a device's
/// [`Switch`] is, for a device that plays notes on rather than sound. The
/// compiler puts one in each MIDI effect's place, with the device's key,
/// out of the signal and in the notes' way.
///
/// The notes that come to the switch go to its device, which plays in the
/// notes pass ([`Node::play_notes`]), with them and its parameter changes
/// for the block, and what the device plays goes where the switch's notes
/// go. It plays no sound. Bypassing the device ([`Switch::BYPASS`]) passes
/// the notes on unchanged instead, and stops the device ([`Node::stop`]),
/// which gets its events through [`Node::flush`] until it's brought back,
/// as if it had played nothing yet ([`Node::reset`]). Either way, a switch
/// ends the notes sounding where it lands, so none hangs: the device's,
/// which it plays no more, and those passed on, whose ends it would never
/// hear. Where the device starts or stops partway through a block, it plays
/// that part of the block as a block of its own.
///
/// The modulation of the device's parameters comes as the block renders,
/// after the notes have played, so it reaches the device at the start of
/// its next block.
///
/// A deactivated MIDI effect's switch ([`deactivated`](Self::deactivated))
/// never starts its device. The compiler leaves it out of the notes' way,
/// but keeps it in the graph, so a plugin stops on the audio thread when
/// the graph takes over, and goes on taking its parameter changes.
pub struct NoteSwitch {
    device: Box<dyn Node>,
    /// Whether the device is bypassed, as of the last frame played.
    bypassed: bool,
    deactivated: bool,
    /// Whether the device was bypassed in the project the switch was built
    /// from.
    built: bool,
    /// Whether a lane moves the switch in this graph.
    automated: bool,
    /// A switch to make at the start of the next block, before its events:
    /// back to the project's state, once a lane stops moving it.
    pending: Option<bool>,
    /// What the device plays in a part of a block, on its way on.
    played: Vec<Routed>,
    /// The modulation that came as the last block rendered, for the
    /// device's next block.
    late: Vec<NodeEvent>,
    /// The block's events, after the modulation that came late.
    merged: Vec<NodeEvent>,
    /// The events of a part of a block, moved to their offsets in the part.
    shifted: Vec<NodeEvent>,
}

/// How much modulation can wait for a MIDI effect's next block. Beyond it,
/// more is dropped.
const LATE: usize = 256;

/// How many events a MIDI effect's block can have with the modulation that
/// came late. Beyond them, that modulation is dropped.
const MERGED: usize = 4096;

impl NoteSwitch {
    /// Puts `device`, a MIDI effect's node, in a switch, bypassed or not.
    pub fn new(device: Box<dyn Node>, bypassed: bool) -> Self {
        Self {
            device,
            bypassed,
            deactivated: false,
            built: bypassed,
            automated: false,
            pending: None,
            played: Vec::new(),
            late: Vec::new(),
            merged: Vec::new(),
            shifted: Vec::new(),
        }
    }

    /// A deactivated MIDI effect's switch: it never starts `device`, and
    /// passes nothing on. A device that was playing in the graph before
    /// stops, and its node still takes its parameter changes
    /// ([`Node::flush`]).
    pub fn deactivated(device: Box<dyn Node>) -> Self {
        Self {
            deactivated: true,
            ..Self::new(device, true)
        }
    }

    /// Whether the device is bypassed, as of the last frame played.
    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// The device's node.
    pub fn device(&self) -> &dyn Node {
        &*self.device
    }

    pub fn device_mut(&mut self) -> &mut dyn Node {
        &mut *self.device
    }
}

impl Node for NoteSwitch {
    fn channels(&self) -> usize {
        self.device.channels()
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.device.prepare(config);
        self.played = Vec::with_capacity(Notes::CAPACITY);
        self.late = Vec::with_capacity(LATE);
        self.merged = Vec::with_capacity(MERGED);
        self.shifted = Vec::with_capacity(SHIFTED);
    }

    /// Its own parameter takes its automation as events, which switch it
    /// on the frame of each step. The device's take theirs as the device
    /// does.
    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        match param {
            Switch::BYPASS if self.deactivated => None,
            Switch::BYPASS => {
                self.automated = true;
                Some(automation)
            }
            _ => self.device.automate(param, automation),
        }
    }

    fn modulated(&mut self, param: ParamId) {
        if param != Switch::BYPASS {
            self.device.modulated(param);
        }
    }

    /// Carries on with the switch it replaces and its device's state. Once
    /// a lane stops moving it, it goes back to the project's state, which a
    /// value sent since overrides. A device that was deactivated starts
    /// afresh, and one that's deactivated now stops.
    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<NoteSwitch>() else {
            return;
        };
        self.device.adopt(&mut *previous.device);
        match (previous.deactivated, self.deactivated) {
            (false, false) => {
                self.bypassed = previous.bypassed;
                self.pending = previous.pending;
                if previous.automated && !self.automated {
                    self.pending = Some(self.built);
                }
                std::mem::swap(&mut self.late, &mut previous.late);
            }
            (false, true) if !previous.bypassed => self.device.stop(),
            (true, false) if !self.bypassed => self.device.reset(),
            _ => {}
        }
    }

    /// Plays nothing. A deactivated device takes the block's events, and
    /// any other keeps the modulation for its next block, whether it plays
    /// it or, bypassed, takes it through [`Node::flush`].
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        if self.deactivated {
            self.device.flush(ctx);
            return;
        }
        for event in ctx.events {
            if matches!(event.kind, NodeEventKind::Modulation { .. })
                && self.late.len() < self.late.capacity()
            {
                self.late.push(NodeEvent {
                    offset: 0,
                    ..*event
                });
            }
        }
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let Self {
            device,
            bypassed,
            deactivated,
            pending,
            played,
            late,
            merged,
            shifted,
            ..
        } = self;
        if *deactivated {
            return;
        }
        // The modulation that came late, before the block's events.
        let mut events = ctx.events;
        if !late.is_empty() {
            if late.len() + events.len() <= merged.capacity() {
                merged.clear();
                merged.extend_from_slice(late);
                merged.extend_from_slice(events);
                events = merged;
            } else {
                notes.drop_more(late.len() as u64);
            }
            late.clear();
        }
        let ctx = &ProcessContext { events, ..*ctx };
        if let Some(to) = pending.take() {
            switch(&mut **device, bypassed, to, 0, notes);
        }
        let frames = ctx.frames;
        let (mut at, mut next) = (0, 0);
        while at < frames {
            while let Some(event) = ctx.events.get(next)
                && event.offset <= at
            {
                if let Some(to) = switch_value(event) {
                    switch(&mut **device, bypassed, to, at, notes);
                }
                next += 1;
            }
            let end = (ctx.events[next..].iter())
                .find(|e| switch_value(e).is_some())
                .map_or(frames, |e| e.offset);
            if *bypassed {
                let events = events_in(ctx.events, at..end);
                device.flush(&ProcessContext { events, ..*ctx });
                pass_on(events, notes);
            } else {
                in_parts(ctx, at..end, shifted, |part, frames| {
                    played.clear();
                    let mut buffer = Notes::buffer(played, part.frames);
                    device.play_notes(part, &mut buffer);
                    notes.drop_more(buffer.dropped());
                    for r in played.iter() {
                        let event = NodeEvent {
                            offset: frames.start + r.event.offset,
                            ..r.event
                        };
                        pass_on(std::slice::from_ref(&event), notes);
                    }
                });
            }
            at = end;
        }
    }

    fn live(&self) -> Option<bool> {
        self.device.live()
    }
}

/// A MIDI effect's switch to `bypassed`, `at` frames into the block: the
/// notes sounding where the switch's notes go end there, and the device
/// stops or starts afresh.
fn switch(device: &mut dyn Node, bypassed: &mut bool, to: bool, at: usize, notes: &mut Notes<'_>) {
    if to == *bypassed {
        return;
    }
    *bypassed = to;
    notes.end_all(at);
    if to {
        device.stop();
    } else {
        device.reset();
    }
}

/// Sends the notes and controllers among `events` on, unchanged.
fn pass_on(events: &[NodeEvent], notes: &mut Notes<'_>) {
    for event in events {
        match event.kind {
            NodeEventKind::Note(note) => {
                notes.send(event.offset, note);
            }
            NodeEventKind::Controller { controller, value } => {
                notes.send_controller(event.offset, controller, value);
            }
            _ => {}
        }
    }
}

/// Whether `event` bypasses the device or brings it back, if it's a
/// switch.
fn switch_value(event: &NodeEvent) -> Option<bool> {
    match event.kind {
        NodeEventKind::Param {
            param: Switch::BYPASS,
            value,
        } => Some(value >= 0.5),
        _ => None,
    }
}

/// The events on `frames` of a block.
fn events_in(events: &[NodeEvent], frames: Range<usize>) -> &[NodeEvent] {
    let first = events.partition_point(|e| e.offset < frames.start);
    let last = events.partition_point(|e| e.offset < frames.end);
    &events[first..last]
}

/// Renders `frames` of the block `ctx` is for with `device`, into the same
/// frames of `output`, as a block of its own, through `shifted`, as
/// [`in_parts`] has it.
fn render_part(
    device: &mut dyn Node,
    shifted: &mut Vec<NodeEvent>,
    ctx: &ProcessContext<'_>,
    inputs: Inputs<'_>,
    frames: Range<usize>,
    output: &mut AudioMut<'_>,
) {
    in_parts(ctx, frames, shifted, |part, frames| {
        process_part(device, part, inputs, frames, output);
    });
}

/// Has `each` render or play `frames` of the block `ctx` is for, as a block
/// of its own, with its context and frames, its events moved to their
/// offsets in it through `shifted`, which has room for [`SHIFTED`]. Where
/// more events than that fall in them, the frames are cut into parts.
fn in_parts(
    ctx: &ProcessContext<'_>,
    frames: Range<usize>,
    shifted: &mut Vec<NodeEvent>,
    mut each: impl FnMut(&ProcessContext<'_>, Range<usize>),
) {
    if frames == (0..ctx.frames) {
        each(ctx, frames);
        return;
    }
    let mut events = events_in(ctx.events, frames.clone());
    if frames.start == 0 {
        each(&part(*ctx, frames.clone(), events), frames);
        return;
    }
    let mut start = frames.start;
    while start < frames.end {
        let (end, count) = match events.get(SHIFTED) {
            None => (frames.end, events.len()),
            Some(cut) if cut.offset > start => {
                let count = events.partition_point(|e| e.offset < cut.offset);
                (cut.offset, count)
            }
            // More events on one frame than there's room for: the rest
            // come on the next.
            Some(_) => (start + 1, SHIFTED),
        };
        shifted.clear();
        shifted.extend(events[..count].iter().map(|e| NodeEvent {
            offset: e.offset.saturating_sub(start),
            ..*e
        }));
        each(&part(*ctx, start..end, shifted), start..end);
        events = &events[count..];
        start = end;
    }
}

/// The context for `frames` of the block `ctx` is for, as a block of its
/// own with `events`.
fn part<'a>(
    ctx: ProcessContext<'a>,
    frames: Range<usize>,
    events: &'a [NodeEvent],
) -> ProcessContext<'a> {
    let start = frames.start as u64;
    let mut transport = ctx.transport;
    if transport.playing {
        transport.position += start;
    }
    // A message that came too late plays on the block's first frame.
    let late = |m: &MidiMessage| m.time.saturating_sub(ctx.time);
    let first = ctx.midi.partition_point(|m| late(m) < start);
    let last = ctx.midi.partition_point(|m| late(m) < frames.end as u64);
    ProcessContext {
        frames: frames.len(),
        time: ctx.time + start,
        transport,
        events,
        input: ctx.input.range(frames),
        midi: &ctx.midi[first..last],
        ..ctx
    }
}

/// Renders `device` over `frames` of `output`, with `part` its context and
/// the same frames of `inputs`, side inputs' included.
fn process_part(
    device: &mut dyn Node,
    part: &ProcessContext<'_>,
    inputs: Inputs<'_>,
    frames: Range<usize>,
    output: &mut AudioMut<'_>,
) {
    let inputs = inputs.range(frames.clone());
    device.process(part, inputs, &mut output.range_mut(frames));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    use super::*;
    use crate::NoteEvent;
    use crate::devices::testing::Run;

    /// Frames a fade takes at 48 kHz.
    const FADE: usize = 480;

    /// What a [`Probe`] went through.
    #[derive(Debug, Default)]
    struct Counts {
        rendered: AtomicUsize,
        stops: AtomicUsize,
        resets: AtomicUsize,
        flushed: AtomicUsize,
    }

    impl Counts {
        fn get(&self) -> [usize; 4] {
            [&self.rendered, &self.stops, &self.resets, &self.flushed].map(|n| n.load(Relaxed))
        }
    }

    /// A device for tests: what comes in, `latency` frames late and at
    /// half the level, counting what it renders and what it's asked to do.
    struct Probe {
        delay: Delay,
        counts: Arc<Counts>,
    }

    impl Probe {
        fn boxed(latency: usize) -> (Box<dyn Node>, Arc<Counts>) {
            let counts = Arc::new(Counts::default());
            let probe = Probe {
                delay: Delay::new(latency),
                counts: Arc::clone(&counts),
            };
            (Box::new(probe), counts)
        }
    }

    impl Node for Probe {
        fn prepare(&mut self, config: &EngineConfig) {
            self.delay.prepare(config);
        }

        fn latency(&self) -> usize {
            self.delay.frames()
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<Probe>() {
                self.delay.adopt(&mut previous.delay);
            }
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            self.delay.process(ctx, inputs, output);
            for channel in output.channels_mut() {
                channel.iter_mut().for_each(|s| *s *= 0.5);
            }
            self.counts.rendered.fetch_add(ctx.frames, Relaxed);
        }

        fn stop(&mut self) {
            self.counts.stops.fetch_add(1, Relaxed);
        }

        fn flush(&mut self, ctx: &ProcessContext<'_>) {
            self.counts.flushed.fetch_add(ctx.events.len(), Relaxed);
        }

        fn reset(&mut self) {
            self.delay.reset();
            self.counts.resets.fetch_add(1, Relaxed);
        }
    }

    fn switch(at: usize, bypassed: bool) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Param {
                param: Switch::BYPASS,
                value: if bypassed { 1.0 } else { 0.0 },
            },
        }
    }

    /// A mono signal with no two frames alike.
    fn signal(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| 1.0 + i as f32 / 1000.0).collect()
    }

    /// `signal` `by` frames late, silent before.
    fn late(signal: &[f32], by: usize) -> Vec<f32> {
        let mut late = vec![0.0; by.min(signal.len())];
        late.extend_from_slice(&signal[..signal.len() - late.len()]);
        late
    }

    fn render(switch: Switch, block: usize, input: &[f32], events: &[NodeEvent]) -> Vec<f32> {
        let mut run = Run::new(Box::new(switch)).with_block(block);
        let output = run.render(&[input.to_vec()], events);
        assert_eq!(output[0], output[1], "both channels alike");
        output.into_iter().next().unwrap()
    }

    /// What the switch plays while it fades over from `from` to `to`, the
    /// fade's first frame on `start`: `dry` is how far over to `to` the
    /// output is on each frame of the fade.
    fn faded(from: &[f32], to: &[f32], start: usize, dry: impl Fn(usize) -> f32) -> Vec<f32> {
        let mut out = from[..start].to_vec();
        for i in start..from.len() {
            let m = if i - start < FADE {
                dry(i - start)
            } else {
                1.0
            };
            out.push(from[i] * (1.0 - m) + to[i] * m);
        }
        out
    }

    fn ramp_up(k: usize) -> f32 {
        (k + 1) as f32 / FADE as f32
    }

    #[test]
    fn a_bypassed_device_plays_its_input_late_by_its_latency() {
        let (probe, counts) = Probe::boxed(5);
        let input = signal(300);
        let out = render(Switch::new(probe, true), 64, &input, &[]);
        assert_eq!(out, late(&input, 5));
        // It never ran, and took its events as it went.
        assert_eq!(counts.get(), [0, 0, 0, 0]);
    }

    #[test]
    fn bypassing_crossfades_to_the_input_over_10_ms_then_stops_the_device() {
        for latency in [0, 7] {
            let (probe, counts) = Probe::boxed(latency);
            let input = signal(1000);
            let out = render(Switch::new(probe, false), 64, &input, &[switch(100, true)]);
            let dry = late(&input, latency);
            let wet: Vec<f32> = dry.iter().map(|s| s * 0.5).collect();
            // The fade starts where the frame the switch came in on comes
            // out.
            assert_eq!(out, faded(&wet, &dry, 100 + latency, ramp_up), "{latency}");
            // The device ran until the fade was over, and stopped.
            assert_eq!(counts.get(), [100 + latency + FADE, 1, 0, 0]);
        }
    }

    #[test]
    fn a_device_brought_back_starts_afresh_and_fades_in_after_its_latency() {
        let (probe, counts) = Probe::boxed(7);
        let input = signal(1000);
        let out = render(Switch::new(probe, true), 64, &input, &[switch(200, false)]);
        let dry = late(&input, 7);
        let wet: Vec<f32> = dry.iter().map(|s| s * 0.5).collect();
        // How far over to the dry copy the output still is on each frame of
        // the fade.
        let expected: Vec<f32> = (0..1000_usize)
            .map(|i| match i.checked_sub(207) {
                None => dry[i],
                Some(k) if k < FADE => {
                    let m = (FADE - k - 1) as f32 / FADE as f32;
                    wet[i] * (1.0 - m) + dry[i] * m
                }
                Some(_) => wet[i],
            })
            .collect();
        assert_eq!(out, expected);
        // It was reset and ran from the frame it came back on.
        assert_eq!(counts.get(), [800, 0, 1, 0]);
    }

    #[test]
    fn switches_render_the_same_whatever_the_blocks() {
        let input = signal(2000);
        let events = [
            switch(100, true),
            switch(130, false),
            switch(400, true),
            switch(1300, false),
            switch(1301, true),
            switch(1302, false),
        ];
        let outputs: Vec<Vec<f32>> = [1, 37, 64, 512, 2000]
            .into_iter()
            .map(|block| {
                render(
                    Switch::new(Probe::boxed(7).0, false),
                    block,
                    &input,
                    &events,
                )
            })
            .collect();
        for output in &outputs[1..] {
            assert_eq!(output, &outputs[0]);
        }
        // Brought back before it stopped, the device carried on; brought
        // back after, it started afresh.
        let (probe, counts) = Probe::boxed(7);
        render(Switch::new(probe, false), 64, &input, &events);
        assert_eq!(counts.get()[1..3], [1, 1]);
    }

    #[test]
    fn a_new_switch_carries_on_with_the_fade_under_way() {
        let input = signal(1000);
        let events = [switch(100, true)];
        let whole = render(Switch::new(Probe::boxed(7).0, false), 64, &input, &events);
        let mut run = Run::new(Box::new(Switch::new(Probe::boxed(7).0, false))).with_block(64);
        let mut out = run.render(&[input[..300].to_vec()], &events)[0].clone();
        run.swap(Box::new(Switch::new(Probe::boxed(7).0, false)));
        out.extend_from_slice(&run.render(&[input[300..].to_vec()], &[])[0]);
        assert_eq!(out, whole);
    }

    #[test]
    fn a_switch_its_lane_stops_moving_goes_back_to_the_projects_state() {
        let input = signal(1000);
        // The lane had the device playing, where the project bypasses it.
        let mut automated = Switch::new(Probe::boxed(7).0, true);
        automated.automated = true;
        let mut run = Run::new(Box::new(automated)).with_block(64);
        let mut out = run.render(&[input[..300].to_vec()], &[switch(0, false)])[0].clone();
        run.swap(Box::new(Switch::new(Probe::boxed(7).0, true)));
        out.extend_from_slice(&run.render(&[input[300..].to_vec()], &[])[0]);
        let dry = late(&input, 7);
        let wet: Vec<f32> = dry.iter().map(|s| s * 0.5).collect();
        assert_eq!(out, faded(&wet, &dry, 307, ramp_up));
    }

    #[test]
    fn a_stopped_device_still_takes_its_parameters() {
        let (probe, counts) = Probe::boxed(0);
        let param = |at: usize| NodeEvent {
            offset: at,
            kind: NodeEventKind::Param {
                param: ParamId(0),
                value: 0.5,
            },
        };
        let events = [param(10), param(200), param(900), switch(950, false)];
        render(Switch::new(probe, true), 64, &signal(1000), &events);
        // The switch's own event goes nowhere.
        assert_eq!(counts.get(), [50, 0, 1, 3]);
    }

    #[test]
    fn a_deactivated_device_is_silent_and_stops() {
        let input = signal(300);
        let mut run = Run::new(Box::new(Switch::new(Probe::boxed(7).0, false))).with_block(64);
        run.render(std::slice::from_ref(&input), &[]);
        let (probe, counts) = Probe::boxed(7);
        let mut deactivated = Switch::deactivated(probe);
        deactivated.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 64,
        });
        assert_eq!(deactivated.latency(), 0);
        run.swap(Box::new(deactivated));
        let out = run.render(&[input], &[switch(10, false)]);
        assert!(out.iter().flatten().all(|&s| s == 0.0));
        assert_eq!(counts.get(), [0, 1, 0, 1]);
    }

    /// What a test MIDI effect went through: the blocks it played, by
    /// engine time and length, the parameter changes and modulation in
    /// them, with the engine time of each, and how many times it was
    /// stopped, reset and flushed.
    #[derive(Debug)]
    struct Log {
        blocks: Vec<(u64, usize)>,
        changes: Vec<(u64, NodeEventKind)>,
        stops: usize,
        resets: usize,
        flushes: usize,
    }

    /// A MIDI effect for tests: plays the notes it gets an octave up, as
    /// themselves, and keeps a log, which [`log`] reads.
    struct Octave(Log);

    impl Octave {
        fn boxed() -> Box<dyn Node> {
            Box::new(Self(Log {
                blocks: Vec::with_capacity(64),
                changes: Vec::with_capacity(64),
                stops: 0,
                resets: 0,
                flushes: 0,
            }))
        }
    }

    /// The log of the [`Octave`] in the switch `node` of `graph`.
    fn log(graph: &mut crate::PreparedGraph, node: crate::NodeId) -> &Log {
        let switch = graph.nodes[node.0].as_mut().downcast_mut::<NoteSwitch>();
        let octave = switch.unwrap().device_mut().downcast_mut::<Octave>();
        &octave.unwrap().0
    }

    impl Node for Octave {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, _: &mut AudioMut<'_>) {
            panic!("a MIDI effect plays its notes and renders nothing");
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            let log = &mut self.0;
            log.blocks.push((ctx.time, ctx.frames));
            for event in ctx.events {
                let time = ctx.time + event.offset as u64;
                match event.kind {
                    NodeEventKind::Note(mut note) => {
                        if let NoteEvent::On { key, .. } = &mut note {
                            *key += 12;
                        }
                        notes.send(event.offset, note);
                    }
                    NodeEventKind::Param { .. } | NodeEventKind::Modulation { .. } => {
                        log.changes.push((time, event.kind));
                    }
                    _ => {}
                }
            }
        }

        fn stop(&mut self) {
            self.0.stops += 1;
        }

        fn reset(&mut self) {
            self.0.resets += 1;
        }

        fn flush(&mut self, _: &ProcessContext<'_>) {
            self.0.flushes += 1;
        }
    }

    /// Plays the notes it's given for each block, in turn.
    struct Script {
        blocks: Vec<Vec<(usize, NoteEvent)>>,
        next: usize,
    }

    impl Node for Script {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, _: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for &(at, note) in self.blocks.get(self.next).into_iter().flatten() {
                notes.send(at, note);
            }
            self.next += 1;
        }
    }

    /// Keeps the notes it gets, with their offsets.
    struct Kept(Vec<(usize, NoteEvent)>);

    impl Node for Kept {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.0.clear();
            for event in ctx.events {
                if let NodeEventKind::Note(note) = event.kind {
                    self.0.push((event.offset, note));
                }
            }
            output.clear();
        }
    }

    fn on(note: u64, key: u8) -> NoteEvent {
        NoteEvent::On {
            note: NoteId(note),
            key,
            velocity: 1.0,
        }
    }

    fn off(note: u64, velocity: f32) -> NoteEvent {
        NoteEvent::Off {
            note: NoteId(note),
            velocity,
        }
    }

    /// A graph of `script`, playing through `switch` to a node that keeps
    /// what it gets: their ids, in that order.
    fn notes_through(
        switch: NoteSwitch,
        script: Vec<Vec<(usize, NoteEvent)>>,
    ) -> (crate::PreparedGraph, [crate::NodeId; 3]) {
        use crate::graph::{Graph, NodeKey};
        let mut g = Graph::new();
        let script = g.add(
            NodeKey(0),
            Script {
                blocks: script,
                next: 0,
            },
        );
        let switch = g.add(NodeKey(1), switch);
        let kept = g.add(NodeKey(2), Kept(Vec::with_capacity(32)));
        g.send_notes(script, switch).unwrap();
        g.send_notes(switch, kept).unwrap();
        g.set_output(kept).unwrap();
        let config = EngineConfig {
            sample_rate: 48_000,
            max_block: 64,
        };
        (g.prepare(&config).unwrap(), [script, switch, kept])
    }

    /// Plays a block of 64 frames of `graph` from engine time `time`, with
    /// `events` for `node`, and modulation for it, as it comes once the
    /// notes have played. Returns the notes the last node kept.
    fn play(
        graph: &mut crate::PreparedGraph,
        [_, node, kept]: [crate::NodeId; 3],
        time: u64,
        events: &[NodeEvent],
        modulation: &[NodeEvent],
    ) -> Vec<(usize, NoteEvent)> {
        use crate::{AudioRef, Block, ReferenceRenderer, Renderer, TransportState};
        let tempo = musicable_model::time::TempoMap::default();
        let block = Block {
            frames: 64,
            time,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        for &event in events {
            assert!(graph.route(node, event));
        }
        crate::rt::scope(|| {
            assert_eq!(graph.play_notes(&block), 0);
            for &event in modulation {
                assert!(graph.route(node, event));
            }
            graph.load_routed();
            ReferenceRenderer.render(graph, &block);
            graph.clear_routed();
        });
        let kept = graph.nodes[kept.0].as_mut().downcast_mut::<Kept>();
        kept.unwrap().0.clone()
    }

    #[test]
    fn a_midi_effect_plays_the_notes_on_and_bypassed_passes_them_by() {
        let effect = Octave::boxed();
        let script = vec![
            vec![(0, on(1, 60))],
            vec![(32, on(2, 62))],
            vec![(4, off(2, 0.25)), (20, on(3, 64))],
        ];
        let (mut graph, nodes) = notes_through(NoteSwitch::new(effect, false), script);
        assert_eq!(play(&mut graph, nodes, 0, &[], &[]), [(0, on(1, 72))]);
        // Bypassed partway through a block, the note the effect played
        // ends where it switches, and the next passes by unchanged.
        let release = musicable_model::clip::Note::RELEASE_VELOCITY;
        assert_eq!(
            play(&mut graph, nodes, 64, &[switch(16, true)], &[]),
            [(16, off(1, release)), (32, on(2, 62))]
        );
        // Brought back, it starts afresh, and the note passed by ends where
        // it switches if it's still sounding: this one ended before.
        assert_eq!(
            play(&mut graph, nodes, 128, &[switch(8, false)], &[]),
            [(4, off(2, 0.25)), (20, on(3, 76))]
        );
        let log = log(&mut graph, nodes[1]);
        // The parts of a block it played, as blocks of their own.
        assert_eq!(log.blocks, [(0, 64), (64, 16), (136, 56)]);
        assert_eq!((log.stops, log.resets), (1, 1));
        // While stopped, it took its events through `flush`, in each part
        // of a block it was bypassed for.
        assert_eq!(log.flushes, 2);
    }

    #[test]
    fn a_midi_effect_gets_its_parameters_in_time_and_its_modulation_a_block_late() {
        let effect = Octave::boxed();
        let (mut graph, nodes) = notes_through(NoteSwitch::new(effect, false), Vec::new());
        let param = NodeEventKind::Param {
            param: ParamId(4),
            value: 0.5,
        };
        let modulation = NodeEventKind::Modulation {
            param: ParamId(4),
            amount: 0.25,
            note: None,
        };
        let at = |offset, kind| NodeEvent { offset, kind };
        play(
            &mut graph,
            nodes,
            0,
            &[at(30, param)],
            &[at(40, modulation)],
        );
        play(&mut graph, nodes, 64, &[], &[]);
        let log = log(&mut graph, nodes[1]);
        assert_eq!(log.changes, [(30, param), (64, modulation)]);
    }

    #[test]
    fn a_deactivated_midi_effect_stops_and_still_takes_its_parameters() {
        let effect = Octave::boxed();
        let script = vec![vec![(0, on(1, 60))]];
        let (mut graph, nodes) = notes_through(NoteSwitch::deactivated(effect), script);
        // It takes over from the effect playing in the graph before.
        let playing = Octave::boxed();
        graph.nodes[nodes[1].0].adopt(&mut NoteSwitch::new(playing, false));
        let param = NodeEvent {
            offset: 30,
            kind: NodeEventKind::Param {
                param: ParamId(4),
                value: 0.5,
            },
        };
        assert!(play(&mut graph, nodes, 0, &[param], &[]).is_empty());
        let log = log(&mut graph, nodes[1]);
        assert!(log.blocks.is_empty());
        assert_eq!((log.stops, log.flushes), (1, 1));
    }
}

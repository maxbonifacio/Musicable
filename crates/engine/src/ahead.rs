//! Rendering ahead of the audio device: while nothing in the graph is
//! played live, the engine renders it up to a lead ahead of the device, in
//! blocks of its own, and the device callback plays what it rendered.
//!
//! The engine has two sides. The ahead side, the [`Core`], holds the graph,
//! the renderer, the transport and the events not yet due, and renders
//! blocks just as the engine did before it rendered ahead. One thread at a
//! time holds it, through a token ([`Shared::try_lock`]): the audio thread
//! when it renders a block itself, or a worker of the parallel renderer
//! rendering ahead between callbacks. The live side is the audio thread's
//! own ([`Engine`](crate::Engine)): the previews, the record taps' writers,
//! and what it plays from.
//!
//! What the ahead side renders ahead goes into rings, which the live side
//! plays from at its own time: the output and the cue output into
//! [`Shared`]'s, each record tap into its writer's ([`WriterRing`]), and the
//! transport's course into a queue of [`Course`] entries, one wherever it
//! doesn't run on. Engine time `t` has place `t % capacity` in a ring, and
//! the ahead side never gets more than a ring ahead of the live side, so the
//! two never touch the same frames at once: the ahead side says how far it
//! has rendered with [`Shared::rendered`], and the live side how far it has
//! played with [`Shared::played`].
//!
//! When the live side needs frames the ahead side hasn't rendered, it takes
//! the token and renders them itself, as the device asks, with the device
//! inputs and the MIDI messages for them. While anything the ahead side
//! renders is live ([`Node::live`](crate::Node::live)), it does that for
//! every block.
//!
//! A graph with live sources comes in two halves
//! ([`Graph::prepare`](crate::Graph::prepare)): the ahead side renders the
//! half that renders ahead, and the live side renders the live half itself,
//! block by block as the device asks, after the ahead side has rendered
//! those frames, reading what passes between the halves from their queues.
//! The ahead side tells the live side what it needs in a queue of its own,
//! in order of time ([`ToLive`]): each graph's live half, for the live side
//! to install at the frame the ahead side installed the rest; each tempo
//! map, from its frame; and the events for nodes in the live half. Both
//! halves of a graph take over at the same engine time, the furthest frame
//! the ahead side had rendered. If something that rendered live renders
//! ahead in the new graph, as when a track is disarmed, the ahead side waits
//! there for the live side, which hands it over when it gets there.

use std::cell::UnsafeCell;
use std::fmt;
use std::hint::spin_loop;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::time::Duration;

use musicable_model::time::{BeatRange, TempoMap};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::buffer::source_channel;
use crate::engine::{CoreGarbage, CoreMessage, Counters, EVENT_CAPACITY, MAX_WRITERS, messages};
use crate::graph::take_over;
use crate::output_recording::{self, EventLog};
use crate::parallel::{CachePadded, Failure};
use crate::timeline::{Segment, TimelineWriter};
use crate::transport::Transport;
use crate::{
    AheadWaker, AudioMut, AudioRef, Block, EngineConfig, Event, MidiMessage, NodeEvent,
    NodeEventKind, NodeKey, Notes, PreparedGraph, Renderer, TransportState, rt,
};

#[cfg(test)]
mod tests;

/// How far ahead of the audio device the engine renders while nothing in
/// the graph is played live: see
/// [`EngineHandle::set_lead`](crate::EngineHandle::set_lead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lead {
    /// Renders each block as the device asks for it.
    #[default]
    Off,
    /// About 20 ms.
    Short,
    /// About 50 ms.
    Normal,
    /// About 200 ms.
    Long,
}

impl Lead {
    pub const ALL: [Lead; 4] = [Lead::Off, Lead::Short, Lead::Normal, Lead::Long];

    pub fn duration(self) -> Duration {
        Duration::from_millis(match self {
            Lead::Off => 0,
            Lead::Short => 20,
            Lead::Normal => 50,
            Lead::Long => 200,
        })
    }

    /// How many frames it is at `sample_rate`, rounded up.
    pub fn frames(self, sample_rate: u32) -> u64 {
        let millis = self.duration().as_millis() as u64;
        (millis * u64::from(sample_rate)).div_ceil(1000)
    }
}

/// The longest block the engine renders ahead, where
/// [`EngineConfig::max_block`] allows it and the lead is at least twice as
/// long. Shorter leads take blocks of 512 and 128 frames, and then the
/// device's period, so a thread rendering ahead is never long in a block
/// the device is waiting for. An app renders ahead in blocks this long
/// with a `max_block` at least this long.
///
/// The `realtime` benchmark's ahead table measured it: on four threads, a
/// 64-track project took about 5% less time in blocks of 1024 frames than
/// of 512, and ARA's renderers prefer them.
pub const AHEAD_BLOCK: usize = 1024;

/// The block sizes the ahead side steps down through as its lead runs
/// short, before the device's period.
const STEPS: [usize; 3] = [AHEAD_BLOCK, 512, 128];

/// The lead while the transport stands still, in frames, so Play starts
/// within about 12 ms.
const STOPPED_LEAD: u64 = 512;

/// Course entries the ahead side can queue: one wherever the course doesn't
/// run on, which is a few a lead, unless a loop shorter than a block plays.
pub(crate) const COURSE_CAPACITY: usize = 1024;

/// Notes the ahead side can queue for one writer. It renders ahead only
/// while each writer's queue has room for the most a block can record
/// ([`Notes::CAPACITY`]), so it never drops one.
const WRITER_NOTES: usize = EVENT_CAPACITY;

/// What the ahead side can queue for the live side: the events a block
/// can send it, as many as the engine holds, twice over, and the graphs
/// and tempo maps between.
pub(crate) const TO_LIVE_CAPACITY: usize = 2 * EVENT_CAPACITY + 2 * LIVE_DEBT;

/// The most graphs and tempo maps the ahead side hands the live side before
/// the live side has handed back what they replace: as many as it can
/// keep while its garbage queue is full.
pub(crate) const LIVE_DEBT: usize = 8;

/// How many frames each ring holds: the longest lead, and a block beyond
/// it, which the ahead side can start just short of the lead.
pub(crate) fn ring_frames(config: EngineConfig) -> usize {
    Lead::Long.frames(config.sample_rate) as usize + config.max_block
}

/// Up to two channels of sound the ahead side rendered, for the live side
/// to play: engine time `t` at place `t % capacity`.
pub(crate) struct Ring {
    samples: Box<[UnsafeCell<f32>]>,
    channels: usize,
    capacity: usize,
}

// SAFETY: the ahead side writes only frames the live side has played or
// doesn't know are rendered yet, and the live side reads only frames the
// ahead side has rendered, as `Shared::rendered` and `Shared::played` say,
// with release and acquire on both sides: no sample is read and written at
// once.
unsafe impl Sync for Ring {}

impl Ring {
    /// Room for `capacity` frames of `channels` channels. Allocates.
    pub(crate) fn new(channels: usize, capacity: usize) -> Self {
        let samples = std::iter::repeat_with(|| UnsafeCell::new(0.0))
            .take(channels * capacity)
            .collect();
        Self {
            samples,
            channels,
            capacity,
        }
    }

    /// Where `frames` frames from engine time `time` go: two runs, each a
    /// place, where in the frames it starts, and how long it is.
    fn runs(&self, time: u64, frames: usize) -> [(usize, usize, usize); 2] {
        debug_assert!(frames <= self.capacity, "a block fits in a ring");
        let place = (time % self.capacity as u64) as usize;
        let first = frames.min(self.capacity - place);
        [(place, 0, first), (0, first, frames - first)]
    }

    /// Writes `frames` frames of `audio` from engine time `time` into each
    /// channel, as copying it into a buffer with the ring's channels would:
    /// a mono source into every channel. `None` writes silence.
    ///
    /// # Safety
    ///
    /// The live side has played these frames' places, and doesn't know
    /// they're rendered yet. One thread writes at a time.
    pub(crate) unsafe fn write(&self, time: u64, frames: usize, audio: Option<AudioRef<'_>>) {
        let samples = UnsafeCell::raw_get(self.samples.as_ptr());
        for c in 0..self.channels {
            let source = audio.and_then(|a| Some(a.channel(source_channel(a.channels(), c)?)));
            for (place, from, len) in self.runs(time, frames) {
                // SAFETY: the run is inside channel `c`, and the caller keeps
                // the live side off it.
                unsafe {
                    let to = samples.add(c * self.capacity + place);
                    match source {
                        Some(source) => {
                            let source = &source[from..from + len];
                            std::ptr::copy_nonoverlapping(source.as_ptr(), to, len);
                        }
                        None => std::ptr::write_bytes(to, 0, len),
                    }
                }
            }
        }
    }

    /// Reads `out.frames()` frames from engine time `time` into `out`'s
    /// channels, up to the ring's.
    ///
    /// # Safety
    ///
    /// The ahead side has rendered these frames, as the caller has seen,
    /// and the live side hasn't played them.
    pub(crate) unsafe fn read(&self, time: u64, out: &mut AudioMut<'_>) {
        let samples = UnsafeCell::raw_get(self.samples.as_ptr());
        let frames = out.frames();
        for c in 0..self.channels.min(out.channels()) {
            let to = out.channel_mut(c);
            for (place, from, len) in self.runs(time, frames) {
                // SAFETY: the run is inside channel `c`, and the caller says
                // the ahead side is done with it.
                unsafe {
                    let source = samples.add(c * self.capacity + place);
                    std::ptr::copy_nonoverlapping(source, to[from..from + len].as_mut_ptr(), len);
                }
            }
        }
    }
}

/// The transport's course from an engine time on, as the ahead side
/// rendered it, with how many channels the output and the cue output had:
/// what the live side needs to play the rings, and to say where the song
/// is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Course {
    pub segment: Segment,
    /// The loop region, while looping is on, and how many times the
    /// playhead has jumped: the rest of the transport the live half sees.
    pub looping: Option<BeatRange>,
    pub jumps: u64,
    /// The output's channels, none without a graph or with the output in
    /// the live half.
    pub output: u8,
    /// The cue output's, none without one.
    pub cue: u8,
}

impl Course {
    fn runs_on_to(&self, next: &Course) -> bool {
        self.segment.runs_on_to(&next.segment)
            && (self.output, self.cue) == (next.output, next.cue)
            && (self.looping, self.jumps) == (next.looping, next.jumps)
    }

    /// The transport as a block from engine time `time` on, in the course,
    /// has it.
    pub(crate) fn transport_at(&self, time: u64) -> TransportState {
        let segment = self.segment;
        TransportState {
            playing: segment.playing,
            position: segment.position_at(time),
            looping: self.looping,
            record: segment.record,
            recording: segment.recording,
            jumps: self.jumps,
        }
    }
}

/// What the ahead side sends the live side, in order of engine time.
pub(crate) enum ToLive {
    /// A graph the ahead side installed at `at`, for the live side to
    /// install there too: the graph's live half, if it has one, and whether
    /// the ahead side waits there for the live side to hand over the nodes
    /// that rendered live and render ahead from then on.
    Graph {
        at: u64,
        live: Option<Box<PreparedGraph>>,
        drain: bool,
    },
    /// A tempo map, for the live half from `at` on.
    Tempo { at: u64, tempo: Box<TempoMap> },
    /// An event for a node in the live half: for its time, or, without
    /// one, for when the live side takes it.
    Event { time: Option<u64>, event: Event },
}

/// What the ahead side rendered of a writer's record taps, for the live
/// side to hand the writer at its own time. The writer's two ends share it.
pub(crate) struct WriterRing {
    /// The tap's sound, in two channels: a mono tap in both, and silence
    /// where the graph hasn't the tap.
    sound: Ring,
    /// The engine time the ahead side started writing it at, and where it
    /// stopped: the writer gets silence outside. `u64::MAX` until then.
    from: AtomicU64,
    until: AtomicU64,
}

/// A note a writer's note record played, with the engine time it played
/// at.
type TimedNote = (u64, NodeEventKind);

/// The ahead side's end of a writer's queues.
pub(crate) struct CoreWriter {
    key: NodeKey,
    ring: Arc<WriterRing>,
    notes: Producer<TimedNote>,
}

/// The live side's end.
pub(crate) struct LiveQueue {
    ring: Arc<WriterRing>,
    notes: Consumer<TimedNote>,
}

/// The two ends of the queues for the writer with `key`, for an engine with
/// `config`. Allocates.
pub(crate) fn writer_queues(key: NodeKey, config: EngineConfig) -> (CoreWriter, LiveQueue) {
    let ring = Arc::new(WriterRing {
        sound: Ring::new(2, ring_frames(config)),
        from: AtomicU64::new(u64::MAX),
        until: AtomicU64::new(u64::MAX),
    });
    let (notes, notes_rx) = RingBuffer::new(WRITER_NOTES);
    let core = CoreWriter {
        key,
        ring: ring.clone(),
        notes,
    };
    let live = LiveQueue {
        ring,
        notes: notes_rx,
    };
    (core, live)
}

impl LiveQueue {
    /// Notes a writer's note record can hold for one stretch the live side
    /// plays: as many as its queue holds.
    pub(crate) const NOTES: usize = WRITER_NOTES;

    /// Hands `writer` what the ahead side rendered of its record tap for
    /// `frames` frames from engine time `time`, with silence where it
    /// didn't write it, using `tap` for room: two channels and at least as
    /// many frames.
    ///
    /// # Safety
    ///
    /// The ahead side has rendered these frames, and the live side hasn't
    /// played them.
    pub(crate) unsafe fn play_sound(
        &mut self,
        writer: &mut dyn crate::TapWriter,
        time: u64,
        frames: usize,
        tap: &mut crate::AudioBuffer,
    ) {
        let end = time + frames as u64;
        let from = self.ring.from.load(Acquire).clamp(time, end);
        let until = self.ring.until.load(Acquire).clamp(time, end);
        if from < until {
            if from > time {
                writer.write(time, AudioRef::empty((from - time) as usize));
            }
            let len = (until - from) as usize;
            // SAFETY: as the caller says.
            unsafe { self.ring.sound.read(from, &mut tap.view_mut(len)) };
            writer.write(from, tap.view(len));
            if until < end {
                writer.write(until, AudioRef::empty((end - until) as usize));
            }
        } else {
            writer.write(time, AudioRef::empty(frames));
        }
    }

    /// Hands `writer` the notes the ahead side's note record played in the
    /// same frames, after [`play_sound`](Self::play_sound), using `notes`
    /// for room: [`NOTES`](Self::NOTES) of them.
    pub(crate) fn play_notes(
        &mut self,
        writer: &mut dyn crate::TapWriter,
        time: u64,
        frames: usize,
        notes: &mut Vec<NodeEvent>,
    ) {
        let end = time + frames as u64;
        notes.clear();
        while let Ok(&(at, kind)) = self.notes.peek()
            && at < end
        {
            let _ = self.notes.pop();
            if notes.len() < notes.capacity() {
                let offset = at.saturating_sub(time) as usize;
                notes.push(NodeEvent { offset, kind });
            }
        }
        writer.write_notes(time, frames, notes);
    }
}

/// What the live side and the threads rendering ahead share.
pub(crate) struct Shared {
    /// Whether a thread holds the core.
    held: CachePadded<AtomicBool>,
    /// Set while the live side waits for the core, so whoever holds it lets
    /// go after its block.
    wanted: AtomicBool,
    /// The core's time: the end of the last block it rendered, stored once
    /// that block is in the rings.
    pub(crate) rendered: CachePadded<AtomicU64>,
    /// The live side's time: how far it has played, so the core may write
    /// those places in the rings again.
    pub(crate) played: CachePadded<AtomicU64>,
    /// The lead the core aims for, in frames, as of its last block: 0
    /// while it renders each block as the device asks.
    pub(crate) target: AtomicU64,
    /// Whether the lead has been whole since the target was last 0, or the
    /// core last couldn't render ahead: the blocks the live side renders
    /// itself after that are short ones.
    pub(crate) primed: AtomicBool,
    /// The device's period, the last callback's length: the shortest block
    /// the core renders ahead.
    pub(crate) period: AtomicUsize,
    pub(crate) output: Ring,
    pub(crate) cue: Ring,
    /// The graphs and tempo maps the core has handed the live side whose
    /// predecessors the live side hasn't handed back yet ([`LIVE_DEBT`]).
    pub(crate) live_debt: AtomicUsize,
    /// A panic on a thread rendering ahead, for the audio thread to raise.
    pub(crate) failure: Failure,
    core: UnsafeCell<Core>,
}

// SAFETY: the core is reached only through the token, which one thread
// holds at a time, and the rest is atomics and rings with rules of their
// own.
unsafe impl Sync for Shared {}

impl Shared {
    /// With rings for an engine with `config`. Allocates.
    pub(crate) fn new(core: Core) -> Self {
        let frames = ring_frames(core.config);
        Self {
            held: CachePadded(AtomicBool::new(false)),
            wanted: AtomicBool::new(false),
            rendered: CachePadded(AtomicU64::new(0)),
            played: CachePadded(AtomicU64::new(0)),
            target: AtomicU64::new(0),
            primed: AtomicBool::new(false),
            period: AtomicUsize::new(core.config.max_block),
            output: Ring::new(2, frames),
            cue: Ring::new(2, frames),
            live_debt: AtomicUsize::new(0),
            failure: Failure::default(),
            core: UnsafeCell::new(core),
        }
    }

    /// The core, unless another thread holds it. Real-time safe.
    pub(crate) fn try_lock(&self) -> Option<Token<'_>> {
        // A token only once it's won: dropping one lets go.
        let won = (self.held).compare_exchange(false, true, Acquire, Relaxed);
        won.is_ok().then(|| Token(self))
    }

    /// The core, for the live side: if a thread rendering ahead holds it,
    /// waits for it to finish its block, working on the block meanwhile.
    /// Real-time safe.
    pub(crate) fn lock(&self, waker: Option<&AheadWaker>) -> Token<'_> {
        if let Some(token) = self.try_lock() {
            return token;
        }
        self.wanted.store(true, Relaxed);
        let mut spins = 0u32;
        let token = loop {
            if let Some(token) = self.try_lock() {
                break token;
            }
            if waker.is_some_and(AheadWaker::help) {
                continue;
            }
            spins = spins.wrapping_add(1);
            match spins.is_multiple_of(64) {
                true => rt::allowed(std::thread::yield_now),
                false => spin_loop(),
            }
        };
        self.wanted.store(false, Relaxed);
        token
    }

    /// How many frames the core has rendered ahead of the live side.
    pub(crate) fn lead(&self) -> u64 {
        let played = self.played.load(Acquire);
        self.rendered.load(Acquire).saturating_sub(played)
    }

    /// Whether the core would render ahead now: its lead is short, and the
    /// live side doesn't want it.
    fn wants_ahead(&self) -> bool {
        let target = self.target.load(Relaxed);
        target > 0 && !self.wanted.load(Relaxed) && self.lead() < target
    }

    /// Renders ahead until the lead is whole, the live side wants the core,
    /// or the core can't go on, unless another thread holds it: for a
    /// renderer's thread, with denormals flushed. A panic waits for the
    /// audio thread to raise it.
    pub(crate) fn render_ahead(&self) {
        while self.wants_ahead() {
            let Some(mut token) = self.try_lock() else {
                return;
            };
            let ran = panic::catch_unwind(AssertUnwindSafe(|| {
                rt::scope(|| token.core().render_ahead(self))
            }));
            drop(token);
            match ran {
                // The live side may have played on while it held the core,
                // and asked for no more, as it was held.
                Ok(Stop::Whole) => {}
                Ok(Stop::Wanted | Stop::Blocked) => return,
                Err(payload) => {
                    self.failure.record(payload);
                    return;
                }
            }
        }
    }

    /// Renders ahead on the audio thread, for a renderer without threads
    /// of its own, until the lead is whole or the core can't go on.
    pub(crate) fn render_ahead_here(&self) {
        if self.wants_ahead()
            && let Some(mut token) = self.try_lock()
        {
            token.core().render_ahead(self);
        }
    }
}

/// The core, held by one thread. Dropping it lets go.
pub(crate) struct Token<'a>(&'a Shared);

impl Token<'_> {
    pub(crate) fn core(&mut self) -> &mut Core {
        // SAFETY: the token is the only way to the core, and one thread
        // holds it at a time.
        unsafe { &mut *self.0.core.get() }
    }
}

impl Drop for Token<'_> {
    fn drop(&mut self) {
        self.0.held.store(false, Release);
    }
}

/// The engine's ahead side, for a renderer's threads to render: see
/// [`Renderer::render_ahead`]. It points into the engine, which stops the
/// threads rendering it before it goes.
pub struct AheadWork(*const Shared);

// SAFETY: `Shared` is `Sync`, and the engine keeps it alive for as long as
// a thread can render it.
unsafe impl Send for AheadWork {}
// SAFETY: as above.
unsafe impl Sync for AheadWork {}

impl AheadWork {
    pub(crate) fn new(shared: &Shared) -> Self {
        Self(shared)
    }

    /// Renders ahead until the lead is whole, unless another thread holds
    /// the ahead side or the audio thread wants it: on a renderer's
    /// real-time thread, with denormals flushed.
    pub(crate) fn run(&self) {
        // SAFETY: the engine keeps it alive while threads can call this.
        unsafe { &*self.0 }.render_ahead();
    }
}

impl fmt::Debug for AheadWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AheadWork").finish_non_exhaustive()
    }
}

/// Why the core stopped rendering ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The lead is whole, or there's nothing to render ahead.
    Whole,
    /// The live side wants the core.
    Wanted,
    /// It can't render the next block ahead: an event for a node that can
    /// be live is due, or the course queue or a writer's notes queue is
    /// full.
    Blocked,
}

/// How the core renders a block.
pub(crate) enum Drive<'a, 'm> {
    /// As the device asks, for the live side: with the device inputs for
    /// its callback, from frame `offset` of them, and the MIDI messages
    /// still to play, for at most `max` frames, the rest of the callback.
    /// It takes in what was sent first only if `receive`, for the first
    /// block the live side renders in a callback: what's sent while a
    /// callback renders waits for the next, as it always has.
    Live {
        input: AudioRef<'a>,
        midi: &'m mut &'a [MidiMessage],
        offset: usize,
        max: usize,
        receive: bool,
    },
    /// Ahead, in a block of its own, without inputs.
    Ahead,
}

/// An event waiting for its time.
#[derive(Clone, Copy)]
pub(crate) struct Pending {
    pub(crate) time: u64,
    pub(crate) event: Event,
}

impl Pending {
    /// Puts the event, for a node, into `graph` for a block from engine
    /// time `start`: on its frame, or on the first if it's late. Says
    /// whether it was late, and whether `graph` had the node. Each side of
    /// the engine applies events this way, so a node gets them on the same
    /// frames from either.
    pub(crate) fn route(self, graph: Option<&mut PreparedGraph>, start: u64) -> (bool, bool) {
        let (node, kind) = match self.event {
            Event::Param { node, param, value } => (node, NodeEventKind::Param { param, value }),
            Event::Note { node, note } => (node, NodeEventKind::Note(note)),
            Event::Transport(_) => unreachable!("a transport command is for no node"),
        };
        let event = NodeEvent {
            offset: (self.time.max(start) - start) as usize,
            kind,
        };
        let found = match graph.and_then(|graph| Some((graph.find(node)?, graph))) {
            Some((node, graph)) => {
                let routed = graph.route(node, event);
                debug_assert!(routed, "a graph has room for the events the engine holds");
                true
            }
            None => false,
        };
        (self.time < start, found)
    }
}

/// Holds `event` in `pending` until engine time `time`, after the others
/// for that time, unless it holds as many as it can: then it's dropped,
/// and counted in `counters`.
pub(crate) fn hold(pending: &mut Vec<Pending>, counters: &Counters, time: u64, event: Event) {
    if pending.len() >= EVENT_CAPACITY {
        counters.events_dropped.fetch_add(1, Relaxed);
        return;
    }
    let at = pending.partition_point(|p| p.time <= time);
    pending.insert(at, Pending { time, event });
}

/// What one side of the engine applied, for a test to apply the same to
/// another engine at the same engine times: each with the time it applied
/// on.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Applied {
    /// An event: a node's, applied to it on its frame, or to none if the
    /// graph hasn't it, or a transport command.
    Event(Event),
    Swap,
    Tempo,
    /// The playhead jumped back to the loop start, at the loop end.
    Wrap,
}

/// The engine's ahead side: what renders its blocks.
pub(crate) struct Core {
    config: EngineConfig,
    renderer: Box<dyn Renderer>,
    pub(crate) graph: Option<Box<PreparedGraph>>,
    transport: Transport,
    /// The engine time of the next frame it renders.
    time: u64,
    messages: Consumer<CoreMessage>,
    garbage: Producer<CoreGarbage>,
    /// Events not yet due, by time and then in arrival order.
    pending: Vec<Pending>,
    counters: Arc<Counters>,
    timeline: TimelineWriter,
    course: Producer<Course>,
    /// The course of the last block it rendered.
    pub(crate) last: Course,
    /// The writers' ends of their queues, for the blocks it renders ahead.
    writers: Vec<CoreWriter>,
    /// What the live side needs from it: see [`ToLive`].
    to_live: Producer<ToLive>,
    /// Whether it waits, at its time, for the live side to hand over the
    /// nodes that rendered live up to there and render ahead from there:
    /// it neither takes in what's sent nor renders until then.
    draining: bool,
    /// Where it logs what it applies, while an output recording is on.
    log: Option<Box<EventLog>>,
    /// What it applied, and at what time, once a test asks for it
    /// ([`Core::keep_log`]).
    #[cfg(test)]
    pub(crate) applied: Option<Vec<(u64, Applied)>>,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: EngineConfig,
        renderer: Box<dyn Renderer>,
        messages: Consumer<CoreMessage>,
        garbage: Producer<CoreGarbage>,
        counters: Arc<Counters>,
        timeline: TimelineWriter,
        course: Producer<Course>,
        to_live: Producer<ToLive>,
    ) -> Self {
        Self {
            config,
            renderer,
            graph: None,
            transport: Transport::new(config.sample_rate),
            time: 0,
            messages,
            garbage,
            pending: Vec::with_capacity(EVENT_CAPACITY),
            counters,
            timeline,
            course,
            last: Course::default(),
            writers: Vec::with_capacity(MAX_WRITERS),
            to_live,
            draining: false,
            log: None,
            #[cfg(test)]
            applied: None,
        }
    }

    /// Has the renderer's threads render ahead with `work`, if it has any.
    pub(crate) fn wake_with(&mut self, work: AheadWork) -> Option<AheadWaker> {
        self.renderer.render_ahead(work)
    }

    /// Renders ahead until the lead is whole, the live side wants the core,
    /// or it can't go on.
    pub(crate) fn render_ahead(&mut self, shared: &Shared) -> Stop {
        loop {
            if shared.wanted.load(Relaxed) {
                return Stop::Wanted;
            }
            if self.render(shared, Drive::Ahead).is_some() {
                continue;
            }
            let target = shared.target.load(Relaxed);
            let lead = self.time.saturating_sub(shared.played.load(Acquire));
            if target > 0 && lead < target {
                // The blocks the live side renders now aren't short ones.
                shared.primed.store(false, Relaxed);
                return Stop::Blocked;
            }
            if target > 0 {
                shared.primed.store(true, Relaxed);
            }
            return Stop::Whole;
        }
    }

    /// Renders the next block, as the device asks or ahead, and returns its
    /// length: `None` if it can't render it ahead now. It takes everything
    /// the control thread has sent first.
    pub(crate) fn render(&mut self, shared: &Shared, drive: Drive<'_, '_>) -> Option<usize> {
        // Like the nodes, on any thread, so the notes they play and the
        // modulation they send come out the same on every one.
        rt::flush_denormals(|| self.render_block(shared, drive))
    }

    /// Takes what the control thread has sent and applies the transport
    /// changes due, as the next block would first, without rendering it:
    /// for a device callback of no frames.
    pub(crate) fn take_in(&mut self, shared: &Shared) {
        rt::flush_denormals(|| self.begin_block(shared, true));
    }

    /// What every block starts with: it takes what was sent, if `receive`,
    /// applies the transport changes due, whoever renders it, and says
    /// what lead to aim for now. Returns that lead.
    fn begin_block(&mut self, shared: &Shared, receive: bool) -> u64 {
        if receive {
            self.receive(shared);
        }
        self.apply_transport(self.time);
        let jumps = self.transport.state().jumps;
        self.transport.wrap();
        if self.transport.state().jumps != jumps {
            #[cfg(test)]
            log(&mut self.applied, self.time, Applied::Wrap);
            self.output_log(self.time, output_recording::Applied::Wrap);
        }
        self.aim(shared)
    }

    /// Says what lead to aim for now, and returns it.
    fn aim(&self, shared: &Shared) -> u64 {
        let target = self.target();
        shared.target.store(target, Relaxed);
        if target == 0 {
            shared.primed.store(false, Relaxed);
        }
        target
    }

    /// The course from its time on, as it stands: what it renders next.
    pub(crate) fn course(&self) -> Course {
        let state = self.transport.state();
        let graph = self.graph.as_deref();
        Course {
            segment: Segment {
                time: self.time,
                position: state.position,
                playing: state.playing,
                record: state.record,
                recording: state.recording,
                latency: graph.map_or(0, |graph| graph.latency() as u64),
            },
            looping: state.looping,
            jumps: state.jumps,
            output: graph.map_or(0, |graph| graph.output_channels() as u8),
            cue: graph.map_or(0, |graph| graph.cue_channels() as u8),
        }
    }

    fn render_block(&mut self, shared: &Shared, drive: Drive<'_, '_>) -> Option<usize> {
        let start = self.time;
        // The block ends at the next transport change, where the loop jumps
        // back and where recording punches in or out, so each block has one
        // transport state.
        let receive = !matches!(drive, Drive::Live { receive: false, .. });
        let target = self.begin_block(shared, receive);
        // It renders nothing until the live side has handed over what
        // renders ahead from here on.
        if self.draining {
            return None;
        }
        let most = match &drive {
            Drive::Live { max, .. } => *max,
            Drive::Ahead => {
                let lead = start.saturating_sub(shared.played.load(Acquire));
                if target == 0 || lead >= target || self.course.is_full() || !self.room_ahead() {
                    return None;
                }
                self.ahead_block(lead, shared.period.load(Relaxed))
            }
        };
        let mut end = start + most.min(self.config.max_block) as u64;
        for frames in [
            self.transport.frames_to_loop_end(),
            self.transport.frames_to_punch(),
        ]
        .into_iter()
        .flatten()
        {
            end = end.min(start + frames);
        }
        // Those due now have been taken.
        let next = (self.pending.iter()).find(|p| matches!(p.event, Event::Transport(_)));
        if let Some(p) = next {
            end = end.min(p.time);
        }
        let ahead = matches!(drive, Drive::Ahead);
        if ahead && let Some(graph) = self.graph.as_deref() {
            // An event for a node that can be live comes in a block the
            // device asks for, so whatever it changes, such as arming a
            // track, applies on its frame with the inputs there.
            let due = self.pending.iter().take_while(|p| p.time < end);
            if let Some(p) = due.into_iter().find(|p| can_be_live(graph, &p.event)) {
                if p.time <= start {
                    return None;
                }
                end = p.time;
            }
        }

        let state = self.transport.state();
        let course = self.course();
        self.timeline.block(course.segment);
        if ahead && !self.last.runs_on_to(&course) {
            // It checked there's room.
            let pushed = self.course.push(course);
            debug_assert!(pushed.is_ok(), "the course queue has room");
        }
        self.last = course;

        let due = self.pending.partition_point(|p| p.time < end);
        let mut graph = self.graph.as_deref_mut();
        let (mut late, mut unmatched, mut dropped) = (0, 0, 0);
        for p in self.pending.drain(..due) {
            let (Event::Param { node, .. } | Event::Note { node, .. }) = p.event else {
                // Those due now were applied, and the next ends the block.
                continue;
            };
            // The live side applies it on its frame, if it hasn't played
            // that yet, and counts it late if it has. It takes it once it
            // has installed this graph's live half, as the queue to it is in
            // order of time.
            if let Some(graph) = graph.as_deref()
                && graph.find(node).is_none()
                && graph.renders_live(node)
            {
                let sent = ToLive::Event {
                    time: Some(p.time),
                    event: p.event,
                };
                if self.to_live.push(sent).is_err() {
                    dropped += 1;
                }
                continue;
            }
            #[cfg(test)]
            log(
                &mut self.applied,
                p.time.max(start),
                Applied::Event(p.event),
            );
            if let Some(log) = &mut self.log {
                log.log(p.time.max(start), output_recording::Applied::Event(p.event));
            }
            let (was_late, found) = p.route(graph.as_deref_mut(), start);
            late += u64::from(was_late);
            unmatched += u64::from(!found);
        }
        if late > 0 {
            self.counters.events_late.fetch_add(late, Relaxed);
        }
        if unmatched > 0 {
            self.counters.events_unmatched.fetch_add(unmatched, Relaxed);
        }
        if dropped > 0 {
            self.counters.events_dropped.fetch_add(dropped, Relaxed);
        }

        let frames = (end - start) as usize;
        let (input, midi) = match drive {
            Drive::Live {
                input,
                midi,
                offset,
                max,
                ..
            } => {
                let (messages, later) = messages(midi, end, frames == max);
                *midi = later;
                (input.range(offset..offset + frames), messages)
            }
            Drive::Ahead => (AudioRef::empty(frames), &[][..]),
        };
        if let Some(graph) = graph {
            let block = Block {
                frames,
                time: start,
                transport: state,
                tempo: self.transport.tempo(),
                input,
                midi,
                ahead,
            };
            let dropped = graph.run(&mut *self.renderer, &block);
            if dropped > 0 {
                self.counters.events_dropped.fetch_add(dropped, Relaxed);
            }
            if ahead {
                let dropped = keep(shared, graph, &mut self.writers, start, frames);
                if dropped > 0 {
                    self.counters.events_dropped.fetch_add(dropped, Relaxed);
                }
            }
        }
        self.time = end;
        self.transport.advance(frames as u64);
        self.timeline.until(end);
        self.counters.furthest.store(end, Relaxed);
        // The block may have changed what's live, by arming a track, say,
        // or fading out the last input: the lead to aim for after it.
        self.aim(shared);
        // Pairs with the live side's load: what's in the rings for the
        // block is there before it reads them.
        shared.rendered.store(end, Release);
        Some(frames)
    }

    /// The lead to aim for now, in frames: none without a graph, while
    /// anything in it is live, or while it has an output or a cue output
    /// with more channels than the rings hold.
    fn target(&self) -> u64 {
        let lead = self.counters.lead_setting.load(Relaxed);
        let Some(graph) = self.graph.as_deref() else {
            return 0;
        };
        if lead == 0 || graph.output_channels() > 2 || graph.cue_channels() > 2 || graph.is_live() {
            return 0;
        }
        match self.transport.state().playing {
            true => lead,
            false => lead.min(STOPPED_LEAD.min(self.config.max_block as u64)),
        }
    }

    /// Whether every queue a block rendered ahead fills can take a block's
    /// worth: each writer's notes queue, the graph's queues into its live
    /// half, and the queue to the live side.
    fn room_ahead(&self) -> bool {
        (self.writers.iter()).all(|writer| writer.notes.slots() >= Notes::CAPACITY)
            && self
                .graph
                .as_deref()
                .is_none_or(PreparedGraph::queues_have_room)
            && self.to_live.slots() >= EVENT_CAPACITY + LIVE_DEBT
    }

    /// The length of the next block to render ahead, with `lead` frames
    /// rendered already and the device asking for `period` at a time.
    fn ahead_block(&self, lead: u64, period: usize) -> usize {
        let max = self.config.max_block;
        let step = STEPS
            .into_iter()
            .find(|&step| step <= max && lead >= 2 * step as u64);
        step.unwrap_or(period.clamp(1, max))
    }

    /// Applies the transport changes due by `start` and takes them out of
    /// the events waiting.
    fn apply_transport(&mut self, start: u64) {
        let due = self.pending.partition_point(|p| p.time <= start);
        let mut kept = 0;
        for i in 0..due {
            let p = self.pending[i];
            match p.event {
                Event::Transport(command) => {
                    if p.time < start {
                        self.counters.events_late.fetch_add(1, Relaxed);
                    }
                    #[cfg(test)]
                    log(&mut self.applied, start, Applied::Event(p.event));
                    self.output_log(start, output_recording::Applied::Event(p.event));
                    self.transport.apply(command);
                }
                _ => {
                    self.pending[kept] = p;
                    kept += 1;
                }
            }
        }
        self.pending.drain(kept..due);
    }

    /// Takes everything the control thread has sent, unless it waits for
    /// the live side to hand over what renders ahead from here on.
    fn receive(&mut self, shared: &Shared) {
        while !self.draining {
            // What it can't take yet waits, with all that comes after it.
            match self.messages.peek() {
                // Hold a swap until there is room to hand back what it replaces.
                Ok(CoreMessage::Swap(_)) if self.graph.is_some() && self.garbage.is_full() => break,
                Ok(CoreMessage::Tempo(..) | CoreMessage::Writer(..) | CoreMessage::Log(..))
                    if self.garbage.is_full() =>
                {
                    break;
                }
                // And a graph or a tempo map until the live side has room for
                // what it hands the live side.
                Ok(CoreMessage::Swap(_) | CoreMessage::Tempo(..)) if !self.room_to_live(shared) => {
                    break;
                }
                Ok(CoreMessage::Event { time: None, event }) if self.to_live.is_full() => {
                    if self.forwards(event) {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
            match self.messages.pop() {
                Ok(CoreMessage::Swap(graph)) => {
                    #[cfg(test)]
                    log(&mut self.applied, self.time, Applied::Swap);
                    self.output_log(self.time, output_recording::Applied::Graph);
                    self.install(shared, graph);
                }
                Ok(CoreMessage::Tempo(tempo, live)) => {
                    #[cfg(test)]
                    log(&mut self.applied, self.time, Applied::Tempo);
                    self.output_log(self.time, output_recording::Applied::Tempo);
                    let old = self.transport.set_tempo(tempo);
                    // `receive` checked there is room.
                    let returned = self.garbage.push(CoreGarbage::Tempo(old));
                    debug_assert!(returned.is_ok(), "the garbage queue has room");
                    let at = self.time;
                    self.hand_live(shared, ToLive::Tempo { at, tempo: live });
                }
                // For a node that renders live, as soon as the live side
                // takes it, which is now for it.
                Ok(CoreMessage::Event { time: None, event }) if self.forwards(&event) => {
                    let sent = ToLive::Event { time: None, event };
                    // `receive` checked there is room.
                    let pushed = self.to_live.push(sent);
                    debug_assert!(pushed.is_ok(), "the queue to the live side has room");
                }
                Ok(CoreMessage::Event { time, event }) => {
                    self.schedule(time.unwrap_or(self.time), event);
                }
                Ok(CoreMessage::Resend { time, event }) => {
                    self.schedule(time.map_or(self.time, |t| t.max(self.time)), event);
                }
                Ok(CoreMessage::Writer(key, writer)) => self.take_writer(key, writer),
                Ok(CoreMessage::Log(log)) => {
                    if let Some(gone) = std::mem::replace(&mut self.log, log) {
                        gone.end();
                        // `receive` checked there is room.
                        let returned = self.garbage.push(CoreGarbage::Log(gone));
                        debug_assert!(returned.is_ok(), "the garbage queue has room");
                    }
                }
                Err(_) => break,
            }
        }
    }

    /// Takes everything the control thread has sent, as the next block
    /// would, for the live side to have the events for the live half's
    /// nodes without waiting for the ahead side's next block.
    pub(crate) fn receive_now(&mut self, shared: &Shared) {
        rt::flush_denormals(|| self.receive(shared));
    }

    /// Whether `event` is for a node in the live half of the graph, which
    /// the live side applies.
    fn forwards(&self, event: &Event) -> bool {
        let graph = self.graph.as_deref();
        match *event {
            Event::Param { node, .. } | Event::Note { node, .. } => {
                graph.is_some_and(|graph| graph.find(node).is_none() && graph.renders_live(node))
            }
            Event::Transport(_) => false,
        }
    }

    /// Whether the live side has room for another graph or tempo map, and
    /// what comes with it.
    fn room_to_live(&self, shared: &Shared) -> bool {
        self.to_live.slots() > LIVE_DEBT && shared.live_debt.load(Acquire) < LIVE_DEBT
    }

    /// Hands the live side a graph or a tempo map, which `room_to_live`
    /// said it has room for.
    fn hand_live(&mut self, shared: &Shared, sent: ToLive) {
        shared.live_debt.fetch_add(1, Relaxed);
        let pushed = self.to_live.push(sent);
        debug_assert!(pushed.is_ok(), "the queue to the live side has room");
    }

    /// Logs `applied` at engine time `time`, while an output recording is
    /// on.
    fn output_log(&mut self, time: u64, applied: output_recording::Applied) {
        if let Some(log) = &mut self.log {
            log.log(time, applied);
        }
    }

    /// Starts keeping a record of what it applies, for a test.
    #[cfg(test)]
    pub(crate) fn keep_log(&mut self) {
        self.applied = Some(Vec::with_capacity(LOG_CAPACITY));
    }

    /// Writes the taps with `key` into `writer`'s queues from now on, in
    /// place of the queues it had for them, or into none.
    fn take_writer(&mut self, key: NodeKey, writer: Option<CoreWriter>) {
        if let Some(writer) = &writer {
            writer.ring.from.store(self.time, Release);
        }
        let at = self.writers.iter().position(|w| w.key == key);
        let gone = match (at, writer) {
            (Some(at), Some(writer)) => Some(std::mem::replace(&mut self.writers[at], writer)),
            (Some(at), None) => Some(self.writers.swap_remove(at)),
            // The handle sends no more than there's room for.
            (None, Some(writer)) if self.writers.len() < MAX_WRITERS => {
                self.writers.push(writer);
                None
            }
            (None, writer) => writer,
        };
        if let Some(gone) = gone {
            gone.ring.until.store(self.time, Release);
            // `receive` checked there is room.
            let returned = self.garbage.push(CoreGarbage::Writer(gone));
            debug_assert!(returned.is_ok(), "the garbage queue has room");
        }
    }

    /// Installs `graph` at its time: its nodes take over from the old
    /// graph's that rendered ahead, the live half's among them, and the
    /// live half goes to the live side, which installs it at the same time,
    /// once the old live half has played up to there. If anything that
    /// rendered live renders ahead in `graph`, it waits there for the live
    /// side to hand it over ([`ToLive::Graph`]).
    fn install(&mut self, shared: &Shared, mut graph: Box<PreparedGraph>) {
        let mut live = graph.live_half.take();
        let mut old = self.graph.take();
        let drain = old.as_deref().is_some_and(|old| old.turns_ahead(&graph));
        let halves = live.is_some() || old.as_deref().is_some_and(|old| !old.live_ids.is_empty());
        if let Some(old) = old.as_deref_mut() {
            take_over(&mut graph, live.as_deref_mut(), old);
        }
        // Its loads have all carried on, for the control thread to read,
        // unless the live side has some to carry on still.
        if live.is_none() && !drain {
            graph.loads.set_installed();
        }
        self.graph = Some(graph);
        if let Some(old) = old {
            // `receive` checked there is room.
            let returned = self.garbage.push(CoreGarbage::Graph(old));
            debug_assert!(returned.is_ok(), "the garbage queue has room");
        }
        if halves {
            let at = self.time;
            self.hand_live(shared, ToLive::Graph { at, live, drain });
            self.draining = drain;
        }
        self.counters.swaps.fetch_add(1, Relaxed);
    }

    /// Has the nodes that rendered live up to now in `old`, the live half
    /// of the graph it replaced, take over in its graph and in `live`, the
    /// new live half, if it has one, and goes on from here: for the live
    /// side, at the time it waited at.
    pub(crate) fn hand_over_live(
        &mut self,
        live: Option<&mut PreparedGraph>,
        old: Option<&mut PreparedGraph>,
    ) {
        if let Some(graph) = self.graph.as_deref_mut() {
            if let Some(old) = old {
                take_over(graph, live, old);
            }
            graph.loads.set_installed();
        }
        self.draining = false;
    }

    /// Takes back `event`, for engine time `time`, from the live side,
    /// which held it for a node that isn't in the live half from then on.
    pub(crate) fn take_back(&mut self, time: u64, event: Event) {
        self.schedule(time, event);
    }

    fn schedule(&mut self, time: u64, event: Event) {
        hold(&mut self.pending, &self.counters, time, event);
    }
}

/// How much a test's log of what a side of the engine applied can hold.
#[cfg(test)]
pub(crate) const LOG_CAPACITY: usize = 1 << 15;

/// Records in `log`, if a test keeps one, that a side of the engine applied
/// `applied` on engine time `time`.
#[cfg(test)]
pub(crate) fn log(log: &mut Option<Vec<(u64, Applied)>>, time: u64, applied: Applied) {
    if let Some(log) = log {
        assert!(
            log.len() < log.capacity(),
            "a test applies no more than this"
        );
        log.push((time, applied));
    }
}

/// Whether `event` is for a node that can be live in `graph`.
fn can_be_live(graph: &PreparedGraph, event: &Event) -> bool {
    match *event {
        Event::Param { node, .. } | Event::Note { node, .. } => graph.can_be_live(node),
        Event::Transport(_) => false,
    }
}

/// Puts what `graph` rendered ahead in the block of `frames` frames from
/// `start` into the rings: its output and cue output, and each writer's
/// record taps and notes. Returns how many notes there wasn't room for.
fn keep(
    shared: &Shared,
    graph: &PreparedGraph,
    writers: &mut [CoreWriter],
    start: u64,
    frames: usize,
) -> u64 {
    // SAFETY: the core renders ahead only while what it has rendered past
    // the live side, and this block, fit in a ring, so the live side has
    // played these places, and it doesn't know the frames are rendered
    // until `rendered` says so. This thread holds the core.
    unsafe {
        if let Some(output) = graph.own_output(frames) {
            shared.output.write(start, frames, Some(output));
        }
        if let Some(cue) = graph.own_cue(frames) {
            shared.cue.write(start, frames, Some(cue));
        }
        for writer in writers.iter() {
            let tapped = graph.own_record(writer.key, frames);
            writer.ring.sound.write(start, frames, tapped);
        }
    }
    let mut dropped = 0;
    for writer in writers {
        for note in graph.own_record_notes(writer.key).unwrap_or_default() {
            let at = start + note.offset as u64;
            if writer.notes.push((at, note.kind)).is_err() {
                dropped += 1;
            }
        }
    }
    dropped
}

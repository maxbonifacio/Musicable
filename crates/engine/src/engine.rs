//! The engine, as the audio thread runs it, and the handle the control thread
//! drives it with.
//!
//! The engine renders on two sides: the ahead side has the graph and renders
//! its blocks, ahead of the device when it can (see `ahead.rs`), and the live
//! side, here, is the audio thread's, which plays what the ahead side
//! rendered, or renders a block itself when there's nothing to play yet.

use std::collections::BTreeMap;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};

use musicable_model::time::TempoMap;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::ahead::{
    self, AheadWork, Core, CoreWriter, Course, Drive, LIVE_DEBT, Lead, LiveQueue, Pending, Shared,
    ToLive,
};
use crate::graph::take_over;
use crate::output_recording::{Applied, Callback, EventLog, OutputRecording, OutputTap};
use crate::preview::{Preview, Previews};
use crate::timeline::Timeline;
use crate::{
    AheadWaker, AudioBuffer, AudioMut, AudioRef, Block, EngineConfig, Event, MidiMessage,
    NodeEvent, NodeKey, ParamId, PreparedGraph, ReferenceRenderer, Renderer, rt,
};

/// Messages the control thread can have in flight to each side.
const QUEUE_CAPACITY: usize = 1024;
/// Old graphs and tempo maps the engine can hand back before it holds off
/// further swaps.
const GARBAGE_CAPACITY: usize = 16;
/// Events the engine holds until they are due, and the most one block takes,
/// besides the notes the graph's own nodes play.
pub(crate) const EVENT_CAPACITY: usize = 1024;
/// The most [`TapWriter`]s the engine writes to at once.
pub const MAX_WRITERS: usize = 256;

/// Where the engine writes what one of the graph's record taps carries
/// ([`Graph::add_record`](crate::Graph::add_record)), or the notes a note
/// record has ([`Graph::add_note_record`](crate::Graph::add_note_record)),
/// block by block, as it plays: a buffer a recorder reads, as it reads an
/// audio input's or a MIDI input's. It's called on the audio thread, so it
/// must be real-time safe.
///
/// What the engine renders ahead ([`EngineHandle::set_lead`]) comes as it
/// plays too, at its engine time, from a queue that holds a record tap in
/// two channels: a mono one in both.
pub trait TapWriter: Send {
    /// Takes the frames the engine played from engine time `time` on: the
    /// next ones, after those of the block before. A block without
    /// channels is silence, for where the graph hasn't the tap, or there's
    /// no graph yet.
    fn write(&mut self, time: u64, audio: AudioRef<'_>);

    /// Takes the notes the node the note record records played in the
    /// block of `frames` frames the engine played from engine time `time`
    /// on, in order, each at its offset into the block: none where the
    /// graph hasn't the record, or there's no graph yet. It comes after
    /// [`write`](Self::write), for every block. A writer of a tap of sound
    /// can pass over it.
    fn write_notes(&mut self, time: u64, frames: usize, notes: &[NodeEvent]) {
        let _ = (time, frames, notes);
    }
}

/// What the control thread sends the ahead side.
pub(crate) enum CoreMessage {
    /// `time: None` means as soon as possible.
    Event {
        time: Option<u64>,
        event: Event,
    },
    /// A parameter value sent again after a swap. A time already past means
    /// the start of the next block, and isn't late.
    Resend {
        time: Option<u64>,
        event: Event,
    },
    Swap(Box<PreparedGraph>),
    /// A tempo map for the ahead side, and the same for the live side.
    Tempo(Box<TempoMap>, Box<TempoMap>),
    /// The ahead side's end of the queues for the writer of the record taps
    /// with this key, or `None` to stop writing them.
    Writer(NodeKey, Option<CoreWriter>),
    /// A log of what it applies, for an output recording, or `None` to
    /// stop logging.
    Log(Option<Box<EventLog>>),
}

/// What the control thread sends the live side.
enum LiveMessage {
    /// A preview to play, or `None` to stop the one playing.
    Preview(Option<Box<Preview>>),
    /// A writer for the record taps with this key, with its end of their
    /// queues for a key that's new.
    Writer(NodeKey, Box<dyn TapWriter>, Option<LiveQueue>),
    /// Stop writing the record taps with this key.
    RemoveWriter(NodeKey),
    /// Keep what it plays, and log what it applies, for an output
    /// recording, or `None` to stop.
    Record(Option<Box<OutputTap>>),
}

/// What the ahead side hands back to be dropped off the audio thread.
pub(crate) enum CoreGarbage {
    Graph(Box<PreparedGraph>),
    Tempo(Box<TempoMap>),
    Writer(CoreWriter),
    Log(Box<EventLog>),
}

/// What the live side hands back.
enum LiveGarbage {
    Preview(Box<Preview>),
    Writer(Box<dyn TapWriter>),
    Queue(LiveQueue),
    /// A graph's live half, and a tempo map the live side's replaced.
    Graph(Box<PreparedGraph>),
    Tempo(Box<TempoMap>),
    Record(Box<OutputTap>),
}

#[derive(Default)]
pub(crate) struct Counters {
    time: AtomicU64,
    position: AtomicU64,
    playing: AtomicBool,
    pub(crate) swaps: AtomicU64,
    latency: AtomicU64,
    pub(crate) events_late: AtomicU64,
    pub(crate) events_dropped: AtomicU64,
    pub(crate) events_unmatched: AtomicU64,
    xruns: AtomicU64,
    /// The preview playing's id, or 0, and the file frame it's at.
    preview: AtomicU64,
    preview_frame: AtomicU64,
    /// The previews' gain, as an `f32`'s bits, which the control thread
    /// sets.
    preview_gain: AtomicU32,
    /// The engine time the ahead side has rendered to.
    pub(crate) furthest: AtomicU64,
    /// The lead the control thread asks for, in frames.
    pub(crate) lead_setting: AtomicU64,
    lead: AtomicU64,
    lead_target: AtomicU64,
    short_blocks: AtomicU64,
}

/// The engine's counters, as of its last device callback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames played since the engine started. This is the clock
    /// [`EngineHandle::send_at`] times refer to.
    pub time: u64,
    /// The song position, in samples: where the next frame plays from.
    pub position: u64,
    pub playing: bool,
    /// Graphs installed.
    pub swaps: u64,
    /// How many frames late the output plays, after delay compensation
    /// ([`PreparedGraph::latency`]), in the graph that rendered the frame
    /// playing. The audio device adds its own output latency to this.
    pub latency: u64,
    /// Events applied later than the time they were sent for.
    pub events_late: u64,
    /// Events dropped because the engine was already holding as many as it
    /// can, notes a node played beyond the most a block takes (see
    /// [`Notes::CAPACITY`](crate::Notes::CAPACITY)), and modulation a node
    /// sent beyond its ports' room ([`Controls::send`](crate::Controls::send)).
    pub events_dropped: u64,
    /// Parameter and note events whose node key wasn't in the running graph.
    pub events_unmatched: u64,
    /// Device underruns and overruns, as reported by the audio backend.
    pub xruns: u64,
    /// The preview playing, if one is: see [`EngineHandle::preview`].
    pub preview: Option<PreviewAt>,
    /// How many frames the engine has rendered ahead of [`time`](Self::time)
    /// ([`EngineHandle::set_lead`]).
    pub lead: u64,
    /// The lead it aims for now: the one set while the transport plays, 512
    /// frames while it stands still, and none while anything in the graph
    /// is played live ([`Node::live`](crate::Node::live)), or the lead is
    /// off.
    pub lead_target: u64,
    /// Blocks the audio thread rendered itself, while the engine rendered
    /// ahead, because the lead ran out: what a callback that drops out has
    /// been doing.
    pub short_blocks: u64,
    /// Frames rendered since the engine started, played and ahead: as of
    /// now, not the last callback, as the renderer's threads render ahead
    /// between callbacks, so it can be more than [`time`](Self::time) and
    /// [`lead`](Self::lead) add up to.
    pub rendered: u64,
}

/// Where the preview playing is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewAt {
    /// The id it was made with: see [`Preview::new`].
    pub id: u64,
    /// The file frame it's at.
    pub frame: u64,
}

impl Counters {
    fn snapshot(&self) -> Stats {
        Stats {
            time: self.time.load(Relaxed),
            position: self.position.load(Relaxed),
            playing: self.playing.load(Relaxed),
            swaps: self.swaps.load(Relaxed),
            latency: self.latency.load(Relaxed),
            events_late: self.events_late.load(Relaxed),
            events_dropped: self.events_dropped.load(Relaxed),
            events_unmatched: self.events_unmatched.load(Relaxed),
            xruns: self.xruns.load(Relaxed),
            preview: match self.preview.load(Relaxed) {
                0 => None,
                id => Some(PreviewAt {
                    id,
                    frame: self.preview_frame.load(Relaxed),
                }),
            },
            lead: self.lead.load(Relaxed),
            lead_target: self.lead_target.load(Relaxed),
            short_blocks: self.short_blocks.load(Relaxed),
            rendered: self.furthest.load(Relaxed),
        }
    }
}

/// The engine's message queue is full. Holds what couldn't be sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueFull<T>(pub T);

/// Why [`EngineHandle::swap_graph`] gave a graph back.
#[derive(Debug)]
pub enum SwapError {
    /// The graph was prepared for a different sample rate or block size.
    ConfigMismatch(Box<PreparedGraph>),
    /// The message queue is full. Try again after the engine renders.
    QueueFull(Box<PreparedGraph>),
}

/// Renders the running graph for the audio thread.
///
/// The engine applies what the control thread sends through its
/// [`EngineHandle`]: graph swaps and tempo maps between blocks, parameter
/// changes and transport commands at the exact frame they are due. It
/// renders whatever number of frames the device asks for, in blocks of at
/// most [`EngineConfig::max_block`], ending a block early wherever the
/// transport changes, a loop jumps back or recording punches in or out. It
/// publishes the transport's course on a [`Timeline`], for recording.
///
/// With a lead set ([`EngineHandle::set_lead`]), it renders ahead of the
/// device while nothing in the graph is played live, and plays what it
/// rendered as the device asks for it: then what it's sent applies at the
/// furthest frame it has rendered.
///
/// Drop the engine off the audio thread, since that frees its graph.
pub struct Engine {
    config: EngineConfig,
    shared: Box<Shared>,
    live: Live,
}

/// The audio thread's side of the engine.
struct Live {
    config: EngineConfig,
    /// The engine time of the next frame to play.
    time: u64,
    messages: Consumer<LiveMessage>,
    garbage: Producer<LiveGarbage>,
    counters: Arc<Counters>,
    previews: Previews,
    /// The writers of the graph's record taps.
    writers: Vec<LiveWriter>,
    /// The course of what the ahead side rendered, from each place it
    /// doesn't run on.
    course: Consumer<Course>,
    /// The course where it's playing.
    current: Course,
    /// Wakes the renderer's threads to render ahead, if it has any.
    waker: Option<AheadWaker>,
    /// Room for what it plays from the rings, a stretch at a time: the
    /// output, the cue output and a writer's tap.
    output: AudioBuffer,
    cue: AudioBuffer,
    tap: AudioBuffer,
    /// Room for a writer's notes for a stretch.
    notes: Vec<NodeEvent>,
    /// The live half of the graph, which renders here, as the device asks,
    /// once the ahead side has rendered the same frames, and what renders
    /// it.
    half: Option<Box<PreparedGraph>>,
    renderer: Box<dyn Renderer>,
    /// What the ahead side sends the live half: see [`ToLive`].
    to_live: Consumer<ToLive>,
    /// The events for the live half's nodes not yet due, by time and then
    /// in arrival order.
    pending: Vec<Pending>,
    /// Events for nodes a new live half hasn't, for the ahead side to take
    /// back once the live side next has it, as it was busy rendering.
    orphans: Vec<Pending>,
    /// The tempo map, as of the live side's time.
    tempo: Box<TempoMap>,
    /// What it's replaced and couldn't hand back yet, as the garbage queue
    /// was full: at most [`LIVE_DEBT`], as the ahead side sends no more.
    stash: Vec<LiveGarbage>,
    /// Where it keeps what it plays and logs what it applies, while an
    /// output recording is on.
    recording: Option<Box<OutputTap>>,
    /// What it applied, and at what time, once a test asks for it.
    #[cfg(test)]
    pub(crate) applied: Option<Vec<(u64, ahead::Applied)>>,
}

/// The writer of the record taps with a key, and its end of their queues.
struct LiveWriter {
    key: NodeKey,
    writer: Box<dyn TapWriter>,
    queue: LiveQueue,
}

/// What the engine played in a stretch of a device callback, for the
/// caller to write to the device.
struct Played<'a> {
    /// The output, if there's a graph yet.
    output: Option<AudioRef<'a>>,
    cue: Option<AudioRef<'a>>,
    /// The graph that rendered it, if it rendered it as the device asked:
    /// for its taps. Its live half, if it has one, rendered here.
    graph: Option<&'a PreparedGraph>,
    live: Option<&'a PreparedGraph>,
    /// What the previews played, if one did.
    preview: Option<AudioRef<'a>>,
}

impl Engine {
    /// An engine with the [`ReferenceRenderer`], and its handle. The engine
    /// plays silence until a graph is swapped in. Its transport is stopped at
    /// the start of the song, at 120 BPM in 4/4.
    pub fn new(config: EngineConfig) -> (Engine, EngineHandle) {
        Self::with_renderer(config, Box::new(ReferenceRenderer))
    }

    /// # Panics
    ///
    /// If the sample rate or the block size in `config` is zero.
    pub fn with_renderer(
        config: EngineConfig,
        renderer: Box<dyn Renderer>,
    ) -> (Engine, EngineHandle) {
        assert!(
            config.sample_rate > 0 && config.max_block > 0,
            "invalid engine config {config:?}"
        );
        let (core_tx, core_rx) = RingBuffer::new(QUEUE_CAPACITY);
        let (live_tx, live_rx) = RingBuffer::new(QUEUE_CAPACITY);
        let (core_garbage_tx, core_garbage_rx) = RingBuffer::new(GARBAGE_CAPACITY);
        let (live_garbage_tx, live_garbage_rx) = RingBuffer::new(GARBAGE_CAPACITY);
        let (course_tx, course_rx) = RingBuffer::new(ahead::COURSE_CAPACITY);
        let (to_live_tx, to_live_rx) = RingBuffer::new(ahead::TO_LIVE_CAPACITY);
        let mut renderer = renderer;
        let live_renderer = renderer.live_renderer();
        let counters = Arc::new(Counters::default());
        counters.preview_gain.store(1.0f32.to_bits(), Relaxed);
        let (timeline, timeline_reader) = crate::timeline::timeline();
        let core = Core::new(
            config,
            renderer,
            core_rx,
            core_garbage_tx,
            counters.clone(),
            timeline,
            course_tx,
            to_live_tx,
        );
        let shared = Box::new(Shared::new(core));
        let work = AheadWork::new(&shared);
        let waker = shared
            .try_lock()
            .expect("nothing else has the core yet")
            .core()
            .wake_with(work);
        let buffer = || AudioBuffer::new(2, config.max_block);
        let engine = Engine {
            config,
            live: Live {
                config,
                time: 0,
                messages: live_rx,
                garbage: live_garbage_tx,
                counters: counters.clone(),
                previews: Previews::new(config),
                writers: Vec::with_capacity(MAX_WRITERS),
                course: course_rx,
                current: Course::default(),
                waker,
                output: buffer(),
                cue: buffer(),
                tap: buffer(),
                notes: Vec::with_capacity(LiveQueue::NOTES),
                half: None,
                renderer: live_renderer,
                to_live: to_live_rx,
                pending: Vec::with_capacity(EVENT_CAPACITY),
                orphans: Vec::with_capacity(EVENT_CAPACITY),
                tempo: Box::default(),
                stash: Vec::with_capacity(LIVE_DEBT),
                recording: None,
                #[cfg(test)]
                applied: None,
            },
            shared,
        };
        let handle = EngineHandle {
            config,
            core: core_tx,
            live: live_tx,
            core_garbage: core_garbage_rx,
            live_garbage: live_garbage_rx,
            counters,
            params: BTreeMap::new(),
            timeline: Some(timeline_reader),
            writers: Vec::new(),
            sent: None,
        };
        (engine, handle)
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn stats(&self) -> Stats {
        self.live.counters.snapshot()
    }

    /// For the audio backend to count a device underrun or overrun.
    pub fn record_xrun(&self) {
        self.live.counters.xruns.fetch_add(1, Relaxed);
    }

    /// Frames played since the engine started: the engine time of the next
    /// frame it plays. An audio backend lines its device inputs up with it.
    pub fn time(&self) -> u64 {
        self.live.time
    }

    /// Renders `output.frames()` frames into `output`: the graph's output,
    /// with its cue output added (see [`Graph::set_cue`](crate::Graph::set_cue)),
    /// and the preview playing, if one is. Real-time safe.
    ///
    /// With no frames, it takes in what was sent, as the next block would,
    /// so [`Stats`] says what plays next: the graph's latency, say. This
    /// goes for every `process` call.
    pub fn process(&mut self, output: &mut AudioMut<'_>) {
        let frames = output.frames();
        self.process_with_input(AudioRef::empty(frames), &[], output);
    }

    /// [`process`](Self::process), with `input` holding the audio devices'
    /// inputs for the same frames, which nodes find in
    /// [`ProcessContext::input`](crate::ProcessContext::input), and `midi`
    /// the messages from the MIDI inputs that play in them, in order of
    /// time, which nodes find in
    /// [`ProcessContext::midi`](crate::ProcessContext::midi). A message for
    /// a time before the first frame plays at the first, and one for a time
    /// after the last at the last. Frames the engine rendered ahead had no
    /// inputs: it renders ahead only while no node plays them
    /// ([`Node::live`](crate::Node::live)), and their messages go nowhere.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// If `input` isn't as long as `output`.
    pub fn process_with_input(
        &mut self,
        input: AudioRef<'_>,
        midi: &[MidiMessage],
        output: &mut AudioMut<'_>,
    ) {
        rt::scope(|| {
            let time = self.live.time;
            self.run(input, midi, output.frames(), |start, frames, played| {
                let mut part = output.range_mut(start..start + frames);
                match played.output {
                    Some(audio) => part.copy_from(audio),
                    None => part.clear(),
                }
                for audio in [played.cue, played.preview].into_iter().flatten() {
                    part.add_from(audio);
                }
            });
            if let Some(callback) = self.live.callback(time, output.frames(), output.channels()) {
                let recording = self.live.recording.as_mut().expect("a recording");
                recording.planar(callback, output.view());
            }
        });
    }

    /// Renders `output.frames()` frames for an offline render, such as an
    /// export: the graph's output into `output`, without its cue output or
    /// a preview, and
    /// each of its taps into the buffer of `taps` with the same number (see
    /// [`Graph::add_tap`](crate::Graph::add_tap)). Taps the graph hasn't
    /// got, and everything before the first graph arrives, are silent, as
    /// are the taps in frames the engine rendered ahead: offline renders
    /// leave the lead off ([`EngineHandle::set_lead`]). Real-time safe.
    ///
    /// # Panics
    ///
    /// If a buffer in `taps` isn't as long as `output`.
    pub fn process_with_taps(&mut self, output: &mut AudioMut<'_>, taps: &mut [AudioMut<'_>]) {
        let frames = output.frames();
        assert!(
            taps.iter().all(|tap| tap.frames() == frames),
            "the taps aren't all {frames} frames long"
        );
        rt::scope(|| {
            self.run(
                AudioRef::empty(frames),
                &[],
                frames,
                |start, frames, played| {
                    let range = start..start + frames;
                    match played.output {
                        Some(audio) => output.range_mut(range.clone()).copy_from(audio),
                        None => output.range_mut(range.clone()).clear(),
                    }
                    for (n, tap) in taps.iter_mut().enumerate() {
                        let mut part = tap.range_mut(range.clone());
                        match live_first(played.live, played.graph, |g| g.own_tap(n, frames)) {
                            Some(source) => part.copy_from(source),
                            None => part.clear(),
                        }
                    }
                },
            );
        });
    }

    /// Renders into an interleaved device buffer with `channels` channels.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// If `output.len()` isn't a multiple of `channels`.
    pub fn process_interleaved(&mut self, output: &mut [f32], channels: usize) {
        let frames = output.len() / channels.max(1);
        self.process_interleaved_with_input(AudioRef::empty(frames), &[], output, channels);
    }

    /// [`process_interleaved`](Self::process_interleaved), with `input`
    /// holding the audio devices' inputs for the same frames and `midi` the
    /// MIDI inputs' messages, as
    /// [`process_with_input`](Self::process_with_input) has them.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// If `output.len()` isn't a multiple of `channels`, or `input` isn't
    /// as many frames long as `output`.
    pub fn process_interleaved_with_input(
        &mut self,
        input: AudioRef<'_>,
        midi: &[MidiMessage],
        output: &mut [f32],
        channels: usize,
    ) {
        assert!(
            channels > 0 && output.len().is_multiple_of(channels),
            "{} samples don't split into {channels} channels",
            output.len()
        );
        rt::scope(|| {
            let frames = output.len() / channels;
            let time = self.live.time;
            self.run(input, midi, frames, |start, frames, played| {
                let part = &mut output[start * channels..(start + frames) * channels];
                match played.output {
                    Some(audio) => audio.write_interleaved(part, channels),
                    None => part.fill(0.0),
                }
                for audio in [played.cue, played.preview].into_iter().flatten() {
                    audio.add_interleaved(part, channels);
                }
            });
            if let Some(callback) = self.live.callback(time, frames, channels) {
                let recording = self.live.recording.as_mut().expect("a recording");
                recording.interleaved(callback, output);
            }
        });
    }

    /// Plays `frames` frames, with `input` holding the device inputs for
    /// them and `midi` the MIDI inputs' messages: what the ahead side has
    /// rendered, and a block at a time what it hasn't, rendered now, with
    /// the graph's live half, if it has one, rendered as each stretch
    /// plays. It hands each stretch's offset and length to `write`, with
    /// what played in it. The record taps go to their writers. Then it
    /// keeps the lead up. With no frames, the ahead side only takes in
    /// what was sent.
    fn run<'a>(
        &mut self,
        input: AudioRef<'a>,
        mut midi: &'a [MidiMessage],
        frames: usize,
        mut write: impl FnMut(usize, usize, Played<'_>),
    ) {
        assert_eq!(
            input.frames(),
            frames,
            "the device inputs are {} frames long, the output {frames}",
            input.frames()
        );
        let shared = &*self.shared;
        // A panic on a worker rendering ahead comes out here, as one in a
        // node rendered here would.
        if let Some(payload) = shared.failure.take() {
            panic::resume_unwind(payload);
        }
        let live = &mut self.live;
        live.receive();
        match frames {
            // The ahead side takes in what was sent, as a block would, so
            // the counters say what plays next.
            0 => live.take_in(shared),
            frames => shared.period.store(frames, Relaxed),
        }
        // The events for the live half's nodes sent since the last
        // callback, unless a thread rendering ahead takes them in first,
        // and those it couldn't hand back.
        if (live.half.is_some() || !live.orphans.is_empty())
            && let Some(mut token) = shared.try_lock()
        {
            let core = token.core();
            live.give_back(core);
            if live.half.is_some() {
                core.receive_now(shared);
            }
        }
        let gain = f32::from_bits(live.counters.preview_gain.load(Relaxed));
        let mut done = 0;
        // Until it renders a block itself, which takes in what was sent.
        let mut receive = true;
        while done < frames {
            // Pairs with the ahead side's store: the rings hold what it
            // rendered, and the queue to the live side what it sent the live
            // half for those frames, which it takes first.
            let rendered = shared.rendered.load(Acquire);
            live.take_to_live(shared, None);
            let left = frames - done;
            let played = match rendered > live.time {
                true => {
                    let most = left.min((rendered - live.time) as usize);
                    live.play_ahead(
                        shared,
                        input,
                        &mut midi,
                        done,
                        most,
                        left,
                        gain,
                        |n, played| write(done, n, played),
                    )
                }
                false => {
                    let played = live.play_live(
                        shared,
                        input,
                        &mut midi,
                        done,
                        left,
                        receive,
                        gain,
                        |n, played| write(done, n, played),
                    );
                    receive &= played == 0;
                    played
                }
            };
            done += played;
        }
        live.finish(shared);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // No thread may render the ahead side once it's gone.
        if let Some(waker) = &self.live.waker {
            waker.close();
        }
    }
}

#[cfg(test)]
impl Engine {
    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// Runs `f` on the ahead side, once no other thread renders it.
    pub(crate) fn with_core<R>(&mut self, f: impl FnOnce(&mut Core, &Shared) -> R) -> R {
        let shared = &*self.shared;
        let mut token = shared.lock(self.live.waker.as_ref());
        f(token.core(), shared)
    }

    /// Starts keeping a record of the events the live side applies to the
    /// live half's nodes, at the times it applies them.
    pub(crate) fn keep_live_log(&mut self) {
        self.live.applied = Some(Vec::with_capacity(ahead::LOG_CAPACITY));
    }

    /// What the live side applied, as [`keep_live_log`](Self::keep_live_log)
    /// has it.
    pub(crate) fn live_log(&self) -> Vec<(u64, ahead::Applied)> {
        self.live.applied.clone().expect("a log")
    }
}

/// The messages at the front of `midi` that play in a stretch ending at
/// engine time `end`, and those after: all of them if the stretch ends the
/// callback, as one timed after its last frame plays on its last frame.
pub(crate) fn messages(
    midi: &[MidiMessage],
    end: u64,
    last: bool,
) -> (&[MidiMessage], &[MidiMessage]) {
    let due = match last {
        true => midi.len(),
        false => midi.partition_point(|m| m.time < end),
    };
    midi.split_at(due)
}

/// What `own` finds in the live half, if there's one and it has it, or
/// else in `graph`, the rest that rendered with it: the output, a tap and
/// the like come from the live half first, wherever the rest of a stretch
/// comes from.
fn live_first<'a, T>(
    half: Option<&'a PreparedGraph>,
    graph: Option<&'a PreparedGraph>,
    own: impl Fn(&'a PreparedGraph) -> Option<T>,
) -> Option<T> {
    half.and_then(&own).or_else(|| graph.and_then(&own))
}

impl Live {
    /// The note of a callback of `frames` frames in `channels` channels
    /// from engine time `time`, which just played, while an output
    /// recording is on.
    fn callback(&self, time: u64, frames: usize, channels: usize) -> Option<Callback> {
        self.recording.as_ref()?;
        let counters = &self.counters;
        Some(Callback {
            time,
            frames: frames as u32,
            channels: channels as u16,
            kept: true,
            xruns: counters.xruns.load(Relaxed),
            short_blocks: counters.short_blocks.load(Relaxed),
            lead: counters.lead.load(Relaxed),
        })
    }

    /// Takes everything the control thread has sent the live side.
    fn receive(&mut self) {
        loop {
            match self.messages.peek() {
                // Room for the two previews a new one can let go of, or a
                // writer and its queues.
                Ok(_) if self.garbage.slots() < 2 => break,
                Ok(_) => {}
                Err(_) => break,
            }
            let gone = match self.messages.pop() {
                Ok(LiveMessage::Preview(preview)) => {
                    let gone = self.previews.replace(preview);
                    gone.map(|gone| gone.map(LiveGarbage::Preview))
                }
                Ok(LiveMessage::Writer(key, writer, queue)) => self.add_writer(key, writer, queue),
                Ok(LiveMessage::Record(tap)) => {
                    let gone = std::mem::replace(&mut self.recording, tap);
                    if let Some(gone) = &gone {
                        gone.log.end();
                    }
                    [gone.map(LiveGarbage::Record), None]
                }
                Ok(LiveMessage::RemoveWriter(key)) => {
                    match self.writers.iter().position(|w| w.key == key) {
                        Some(at) => {
                            let LiveWriter { writer, queue, .. } = self.writers.swap_remove(at);
                            [
                                Some(LiveGarbage::Writer(writer)),
                                Some(LiveGarbage::Queue(queue)),
                            ]
                        }
                        None => [None, None],
                    }
                }
                Err(_) => break,
            };
            for gone in gone.into_iter().flatten() {
                // `receive` checked there is room.
                let returned = self.garbage.push(gone);
                debug_assert!(returned.is_ok(), "the garbage queue has room");
            }
        }
    }

    /// Has `writer` write the record taps with `key`, in place of the
    /// writer it had for them, and from `queue` if it comes with one.
    /// Returns what it let go of.
    fn add_writer(
        &mut self,
        key: NodeKey,
        writer: Box<dyn TapWriter>,
        queue: Option<LiveQueue>,
    ) -> [Option<LiveGarbage>; 2] {
        match self.writers.iter_mut().find(|w| w.key == key) {
            Some(had) => {
                let writer = std::mem::replace(&mut had.writer, writer);
                let queue = queue.map(|queue| std::mem::replace(&mut had.queue, queue));
                [
                    Some(LiveGarbage::Writer(writer)),
                    queue.map(LiveGarbage::Queue),
                ]
            }
            // The handle sends no more than there's room for, and a queue
            // with a writer for a new key.
            None => match queue {
                Some(queue) if self.writers.len() < MAX_WRITERS => {
                    self.writers.push(LiveWriter { key, writer, queue });
                    [None, None]
                }
                queue => [
                    Some(LiveGarbage::Writer(writer)),
                    queue.map(LiveGarbage::Queue),
                ],
            },
        }
    }

    /// Takes what the ahead side has sent it up to its time: each graph's
    /// live half and each tempo map, which it installs on its frame, and
    /// the events for the live half's nodes, which wait for theirs. With
    /// `core`, it holds the ahead side already.
    fn take_to_live(&mut self, shared: &Shared, mut core: Option<&mut Core>) {
        if let Some(core) = core.as_deref_mut() {
            self.give_back(core);
        }
        loop {
            match self.to_live.peek() {
                Ok(ToLive::Graph { at, .. } | ToLive::Tempo { at, .. }) if *at > self.time => break,
                Ok(_) => {}
                Err(_) => break,
            }
            match self.to_live.pop() {
                Ok(ToLive::Graph { live, drain, .. }) => {
                    self.install(shared, core.as_deref_mut(), live, drain);
                }
                Ok(ToLive::Tempo { tempo, .. }) => {
                    let old = std::mem::replace(&mut self.tempo, tempo);
                    self.discard(shared, LiveGarbage::Tempo(old));
                }
                // An event for now applies on the next frame it plays.
                Ok(ToLive::Event { time, event }) => {
                    self.schedule(time.unwrap_or(self.time), event)
                }
                Err(_) => break,
            }
        }
    }

    /// Installs `new`, a graph's live half, if it has one, in place of the
    /// one it has, at its time: its nodes take over from the old live
    /// half's, as the ahead side's took over from the rest. With `drain`,
    /// the ahead side waits here for the nodes of the old half that render
    /// ahead in the new graph to take over there, which they do now.
    fn install(
        &mut self,
        shared: &Shared,
        core: Option<&mut Core>,
        mut new: Option<Box<PreparedGraph>>,
        drain: bool,
    ) {
        let mut old = self.half.take();
        // The events it holds for nodes the new half hasn't are the ahead
        // side's now: they render there from here on, if anywhere. They're
        // for this frame, sent for the frame playing as the graph came, so
        // the ahead side waits for them here when it can.
        let half = new.as_deref();
        let orphaned = |p: &Pending| match p.event {
            Event::Param { node, .. } | Event::Note { node, .. } => {
                half.is_none_or(|half| half.find(node).is_none())
            }
            Event::Transport(_) => false,
        };
        // It waits for the ahead side only to drain it: otherwise they
        // wait until it has the ahead side, so a thread rendering a block
        // ahead doesn't hold up the callback.
        let orphans = &mut self.orphans;
        self.pending.retain(|p| {
            let keep = !orphaned(p);
            if !keep && orphans.len() < orphans.capacity() {
                orphans.push(*p);
            } else if !keep {
                self.counters.events_dropped.fetch_add(1, Relaxed);
            }
            keep
        });
        let mut token = None;
        let core = match (core, drain) {
            (Some(core), _) => Some(core),
            (None, true) => Some(token.insert(shared.lock(self.waker.as_ref())).core()),
            (None, false) if self.orphans.is_empty() => None,
            (None, false) => shared.try_lock().map(|t| token.insert(t).core()),
        };
        if let Some(core) = core {
            self.give_back(core);
            if drain {
                core.hand_over_live(new.as_deref_mut(), old.as_deref_mut());
            }
        }
        if !drain && let Some(new) = new.as_deref_mut() {
            if let Some(old) = old.as_deref_mut() {
                take_over(new, None, old);
            }
            // Its loads have all carried on, for the control thread to read.
            new.loads.set_installed();
        }
        self.renderer.set_live_half(new.is_some());
        self.half = new;
        match old {
            Some(old) => self.discard(shared, LiveGarbage::Graph(old)),
            // It hands back nothing for it.
            None => {
                shared.live_debt.fetch_sub(1, Release);
            }
        }
    }

    /// Has the ahead side take back the events for nodes a live half it
    /// installed hasn't.
    fn give_back(&mut self, core: &mut Core) {
        for p in self.orphans.drain(..) {
            core.take_back(p.time, p.event);
        }
    }

    /// Hands back a live half or a tempo map it's replaced, or keeps it
    /// until the garbage queue has room.
    fn discard(&mut self, shared: &Shared, gone: LiveGarbage) {
        match self.garbage.push(gone) {
            Ok(()) => {
                shared.live_debt.fetch_sub(1, Release);
            }
            Err(rtrb::PushError::Full(gone)) => {
                // The ahead side sends no more than it can keep, so this
                // doesn't allocate.
                debug_assert!(self.stash.len() < self.stash.capacity());
                self.stash.push(gone);
            }
        }
    }

    /// Hands back what it kept, as far as the garbage queue has room.
    fn hand_back(&mut self, shared: &Shared) {
        while !self.garbage.is_full()
            && let Some(gone) = self.stash.pop()
        {
            let returned = self.garbage.push(gone);
            debug_assert!(returned.is_ok(), "the garbage queue has room");
            shared.live_debt.fetch_sub(1, Release);
        }
    }

    /// Holds `event` for a node in the live half until engine time `time`,
    /// after the others for that time, unless it holds as many as it can.
    fn schedule(&mut self, time: u64, event: Event) {
        ahead::hold(&mut self.pending, &self.counters, time, event);
    }

    /// The engine time of the next graph or tempo map from the ahead side,
    /// which a stretch ends at, if one's queued.
    fn next_change(&self) -> Option<u64> {
        match self.to_live.peek() {
            Ok(ToLive::Graph { at, .. } | ToLive::Tempo { at, .. }) => Some(*at),
            _ => None,
        }
    }

    /// Renders the live half, if there is one, for `frames` frames from its
    /// time, with the device inputs `input` and the MIDI messages `midi`
    /// for them, and the events for its nodes due in them, in the
    /// transport's course there.
    fn render_half(&mut self, input: AudioRef<'_>, midi: &[MidiMessage], frames: usize) {
        let start = self.time;
        let end = start + frames as u64;
        let due = self.pending.partition_point(|p| p.time < end);
        let (mut late, mut unmatched) = (0, 0);
        for p in self.pending.drain(..due) {
            if let Event::Transport(_) = p.event {
                continue;
            }
            #[cfg(test)]
            ahead::log(
                &mut self.applied,
                p.time.max(start),
                ahead::Applied::Event(p.event),
            );
            if let Some(recording) = &mut self.recording {
                (recording.log).log(p.time.max(start), Applied::Event(p.event));
            }
            let (was_late, found) = p.route(self.half.as_deref_mut(), start);
            late += u64::from(was_late);
            unmatched += u64::from(!found);
        }
        if late > 0 {
            self.counters.events_late.fetch_add(late, Relaxed);
        }
        if unmatched > 0 {
            self.counters.events_unmatched.fetch_add(unmatched, Relaxed);
        }
        let Some(half) = self.half.as_deref_mut() else {
            return;
        };
        let block = Block {
            frames,
            time: start,
            transport: self.current.transport_at(start),
            tempo: &self.tempo,
            input,
            midi,
            ahead: false,
        };
        let renderer = &mut *self.renderer;
        // As on the ahead side, so the notes its nodes play come out the
        // same.
        let dropped = rt::flush_denormals(|| half.run(renderer, &block));
        if dropped > 0 {
            self.counters.events_dropped.fetch_add(dropped, Relaxed);
        }
    }

    /// Plays up to `most` of the frames the ahead side has rendered, as far
    /// as their course runs on, the live side's next graph or tempo map
    /// waits and a block at most, from frame `offset` of the callback, with
    /// `left` frames to its end. It renders the live half for them with the
    /// device inputs there and the MIDI messages at the front of `midi`
    /// due in them, and hands them to `write`. Returns how many it played.
    #[allow(clippy::too_many_arguments)]
    fn play_ahead<'a>(
        &mut self,
        shared: &Shared,
        input: AudioRef<'a>,
        midi: &mut &'a [MidiMessage],
        offset: usize,
        most: usize,
        left: usize,
        gain: f32,
        write: impl FnOnce(usize, Played<'_>),
    ) -> usize {
        let time = self.time;
        while let Ok(next) = self.course.peek()
            && next.segment.time <= time
        {
            self.current = *next;
            let _ = self.course.pop();
        }
        let mut frames = most.min(self.config.max_block);
        if let Ok(next) = self.course.peek() {
            frames = frames.min((next.segment.time - time) as usize);
        }
        if let Some(at) = self.next_change() {
            frames = frames.min((at - time) as usize);
        }
        // Without a live half, the messages go nowhere: no node played
        // them as they rendered.
        let (messages, later) = messages(midi, time + frames as u64, frames == left);
        *midi = later;
        self.render_half(input.range(offset..offset + frames), messages, frames);
        let Course { output, cue, .. } = self.current;
        // SAFETY: the ahead side has rendered these frames, as `run` saw,
        // and they aren't played yet.
        unsafe {
            if output > 0 {
                shared.output.read(time, &mut self.output.view_mut(frames));
            }
            if cue > 0 {
                shared.cue.read(time, &mut self.cue.view_mut(frames));
            }
        }
        let half = self.half.as_deref();
        // A ring holds a mono output in both channels.
        let output = live_first(half, None, |half| half.own_output(frames))
            .or_else(|| (output > 0).then(|| self.output.view(frames).first(output.into())));
        let cue = live_first(half, None, |half| half.own_cue(frames))
            .or_else(|| (cue > 0).then(|| self.cue.view(frames).first(cue.into())));
        let preview = self.previews.play(frames, gain);
        write(
            frames,
            Played {
                output,
                cue,
                graph: None,
                live: half,
                preview,
            },
        );
        for LiveWriter { key, writer, queue } in &mut self.writers {
            match live_first(half, None, |half| half.own_record(*key, frames)) {
                Some(tapped) => writer.write(time, tapped),
                // SAFETY: as above.
                None => unsafe { queue.play_sound(writer.as_mut(), time, frames, &mut self.tap) },
            }
            match live_first(half, None, |half| half.own_record_notes(*key)) {
                Some(notes) => writer.write_notes(time, frames, notes),
                None => queue.play_notes(writer.as_mut(), time, frames, &mut self.notes),
            }
        }
        self.played(shared, frames);
        frames
    }

    /// Renders the next block itself, of at most `max` frames, with the
    /// device inputs from frame `offset` of `input` and the MIDI messages
    /// at the front of `midi` due in it, having the ahead side take in
    /// what was sent first if `receive`, and the live half after it, and
    /// hands it to `write`. Returns its length: none if a thread rendering
    /// ahead finished a block while this waited for the core, which plays
    /// first.
    #[allow(clippy::too_many_arguments)]
    fn play_live<'a>(
        &mut self,
        shared: &Shared,
        input: AudioRef<'a>,
        midi: &mut &'a [MidiMessage],
        offset: usize,
        max: usize,
        receive: bool,
        gain: f32,
        write: impl FnOnce(usize, Played<'_>),
    ) -> usize {
        let mut token = shared.lock(self.waker.as_ref());
        if shared.rendered.load(Acquire) > self.time {
            return 0;
        }
        // The ahead side should have rendered this: its lead ran out.
        if shared.primed.load(Relaxed) && shared.target.load(Relaxed) > 0 {
            self.counters.short_blocks.fetch_add(1, Relaxed);
        }
        let core = token.core();
        let frames = loop {
            // The ahead side takes the device's MIDI messages for the block
            // as the live half does.
            let mut taken = *midi;
            let drive = Drive::Live {
                input,
                midi: &mut taken,
                offset,
                max,
                receive,
            };
            if let Some(frames) = core.render(shared, drive) {
                break frames;
            }
            // It waits here for what renders ahead from here on, which the
            // live side hands it now, for each graph it took in.
            self.take_to_live(shared, Some(core));
        };
        self.current = core.last;
        // The graph or the tempo map the ahead side took in for the block.
        self.take_to_live(shared, Some(core));
        let (messages, later) = messages(midi, self.time + frames as u64, frames == max);
        *midi = later;
        self.render_half(input.range(offset..offset + frames), messages, frames);
        let graph = core.graph.as_deref();
        let half = self.half.as_deref();
        let output = live_first(half, graph, |g| g.own_output(frames));
        let cue = live_first(half, graph, |g| g.own_cue(frames));
        let preview = self.previews.play(frames, gain);
        write(
            frames,
            Played {
                output,
                cue,
                graph,
                live: half,
                preview,
            },
        );
        let start = self.time;
        for LiveWriter { key, writer, .. } in &mut self.writers {
            let tapped = live_first(half, graph, |g| g.own_record(*key, frames));
            writer.write(start, tapped.unwrap_or(AudioRef::empty(frames)));
            let notes = live_first(half, graph, |g| g.own_record_notes(*key));
            writer.write_notes(start, frames, notes.unwrap_or_default());
        }
        self.played(shared, frames);
        frames
    }

    /// Has the ahead side take in what the control thread sent, for a
    /// callback of no frames, and goes on from where it stands, unless it
    /// has rendered ahead.
    fn take_in(&mut self, shared: &Shared) {
        let mut token = shared.lock(self.waker.as_ref());
        let core = token.core();
        core.take_in(shared);
        if shared.rendered.load(Acquire) == self.time {
            self.current = core.course();
        }
        self.take_to_live(shared, Some(core));
    }

    /// Moves on past `frames` frames it has played.
    fn played(&mut self, shared: &Shared, frames: usize) {
        self.time += frames as u64;
        // Pairs with the ahead side's load: it may write their places in
        // the rings again.
        shared.played.store(self.time, Release);
    }

    /// Ends a callback: hands back the previews that finished and what it
    /// replaced, publishes the counters, and has the ahead side render on
    /// if its lead is short.
    fn finish(&mut self, shared: &Shared) {
        self.hand_back(shared);
        while !self.garbage.is_full()
            && let Some(finished) = self.previews.take_finished()
        {
            let returned = self.garbage.push(LiveGarbage::Preview(finished));
            debug_assert!(returned.is_ok(), "the garbage queue has room");
        }
        let target = shared.target.load(Relaxed);
        if target > 0 && shared.lead() < target {
            match &self.waker {
                Some(waker) => waker.wake(),
                None => shared.render_ahead_here(),
            }
        }
        let counters = &self.counters;
        let (preview, frame) = self.previews.at().unwrap_or((0, 0));
        counters.preview.store(preview, Relaxed);
        counters.preview_frame.store(frame, Relaxed);
        let segment = self.current.segment;
        counters.time.store(self.time, Relaxed);
        counters
            .position
            .store(segment.position_at(self.time), Relaxed);
        counters.playing.store(segment.playing, Relaxed);
        counters.latency.store(segment.latency, Relaxed);
        counters.lead.store(shared.lead(), Relaxed);
        counters
            .lead_target
            .store(shared.target.load(Relaxed), Relaxed);
    }
}

/// The control thread's side of an [`Engine`]: sends it graphs and events,
/// and drops the graphs it is done with.
pub struct EngineHandle {
    config: EngineConfig,
    /// To the ahead side.
    core: Producer<CoreMessage>,
    /// To the live side.
    live: Producer<LiveMessage>,
    core_garbage: Consumer<CoreGarbage>,
    live_garbage: Consumer<LiveGarbage>,
    counters: Arc<Counters>,
    /// For each parameter, the value that applies last of those sent. Sent
    /// again after each swap, since a node that is new in the graph missed
    /// anything sent while the graph was being compiled.
    params: BTreeMap<(NodeKey, ParamId), LastValue>,
    timeline: Option<Timeline>,
    /// The keys of the writers sent, and not removed since.
    writers: Vec<NodeKey>,
    /// The events sent while an output recording is on.
    sent: Option<Vec<Sent>>,
}

/// An event sent while an output recording was on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sent {
    /// The engine time playing as it was sent: see [`Stats::time`].
    pub time: u64,
    /// The time it was sent for, or `None` for as soon as possible.
    pub at: Option<u64>,
    pub event: Event,
}

#[derive(Debug, Clone, Copy)]
struct LastValue {
    /// As sent: `None` for as soon as possible.
    time: Option<u64>,
    /// When it applies, as far as the control thread can tell.
    applies: u64,
    value: f64,
}

impl EngineHandle {
    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }

    /// Has the engine render ahead of the audio device by up to `lead`,
    /// from its next block on, all but each graph's live half
    /// ([`Graph::prepare`](crate::Graph::prepare)): what's played live
    /// ([`Node::live`](crate::Node::live)), as a track that monitors its
    /// input is, and what it reaches, which render as the device asks for
    /// each block. The rest renders in blocks of up to
    /// [`AHEAD_BLOCK`](crate::AHEAD_BLOCK) frames, on the renderer's own
    /// threads when it has them, so a device callback only copies what was
    /// rendered, and renders the live half on top. While the transport
    /// stands still, it keeps 512 frames ahead, so Play starts soon.
    ///
    /// What it's sent applies at the furthest frame it has rendered, at
    /// most the lead ahead, since what it rendered can't be taken back:
    /// transport commands, tempo maps, graphs and events sent for as soon
    /// as possible, but for events for the live half's nodes, which apply
    /// as they do with the lead off. [`Lead::Off`], the default, renders
    /// each block as the device asks for it.
    pub fn set_lead(&self, lead: Lead) {
        let frames = lead.frames(self.config.sample_rate);
        self.counters.lead_setting.store(frames, Relaxed);
    }

    /// The engine's [`Timeline`], for a recorder: where in the song each
    /// frame it renders plays, and whether it records. There's one, and the
    /// first call takes it. Read it every so often, or the engine drops
    /// what it publishes.
    pub fn take_timeline(&mut self) -> Option<Timeline> {
        self.timeline.take()
    }

    /// Starts recording what the engine hands the audio device, from its
    /// next callback on, and what each side of it applies, for a thread to
    /// read from the [`OutputRecording`] it returns, every few milliseconds
    /// or it loses frames: see [`output_recording`](crate::output_recording).
    /// It also keeps the events sent from now on, which
    /// [`stop_recording_output`](Self::stop_recording_output) returns. A
    /// recording already on stops. `None` if the message queues are full.
    pub fn record_output(&mut self) -> Option<OutputRecording> {
        if self.core.slots() < 1 || self.live.slots() < 1 {
            return None;
        }
        let (tap, log, reader) = crate::output_recording::output_recording(self.config.sample_rate);
        let sent = self
            .core
            .push(CoreMessage::Log(Some(Box::new(log))))
            .is_ok()
            && self
                .live
                .push(LiveMessage::Record(Some(Box::new(tap))))
                .is_ok();
        debug_assert!(sent, "both queues had room");
        self.sent = Some(Vec::new());
        Some(reader)
    }

    /// Stops the output recording, if one is on, from the engine's next
    /// callback, and returns the events sent while it was. Its
    /// [`OutputRecording`] says when the engine has stopped writing to it.
    /// `None` if none is on, or the message queues are full.
    pub fn stop_recording_output(&mut self) -> Option<Vec<Sent>> {
        if self.sent.is_none() || self.core.slots() < 1 || self.live.slots() < 1 {
            return None;
        }
        let sent = self.core.push(CoreMessage::Log(None)).is_ok()
            && self.live.push(LiveMessage::Record(None)).is_ok();
        debug_assert!(sent, "both queues had room");
        self.sent.take()
    }

    /// Sends an event to apply at the start of the engine's next block: the
    /// next `process` call's, or past what it has rendered ahead.
    pub fn send(&mut self, event: Event) -> Result<(), QueueFull<Event>> {
        self.push_event(None, event)
    }

    /// Sends an event to apply at engine time `time` (see [`Stats::time`]).
    /// An event for a time already past, or already rendered ahead,
    /// applies at the start of the next block and counts as late.
    pub fn send_at(&mut self, time: u64, event: Event) -> Result<(), QueueFull<Event>> {
        self.push_event(Some(time), event)
    }

    fn push_event(&mut self, time: Option<u64>, event: Event) -> Result<(), QueueFull<Event>> {
        if self.core.push(CoreMessage::Event { time, event }).is_err() {
            return Err(QueueFull(event));
        }
        if let Some(sent) = &mut self.sent {
            sent.push(Sent {
                time: self.counters.time.load(Relaxed),
                at: time,
                event,
            });
        }
        if let Event::Param { node, param, value } = event {
            // The value sent last isn't always the one that applies last: one
            // sent for now lands before one sent earlier for later.
            let now = self.counters.furthest.load(Relaxed);
            let applies = time.map_or(now, |t| t.max(now));
            let sent = LastValue {
                time,
                applies,
                value,
            };
            self.params
                .entry((node, param))
                .and_modify(|last| {
                    if applies >= last.applies {
                        *last = sent;
                    }
                })
                .or_insert(sent);
        }
        Ok(())
    }

    /// Queues `graph` to replace the running graph at the start of the
    /// engine's next block. Nodes take over the state of the old nodes with
    /// the same keys, and each of their parameters gets the value sent for
    /// it again: the one that applies last, if several were sent.
    pub fn swap_graph(&mut self, graph: PreparedGraph) -> Result<(), SwapError> {
        if graph.config() != self.config {
            return Err(SwapError::ConfigMismatch(Box::new(graph)));
        }
        let resend: Vec<CoreMessage> = self
            .params
            .iter()
            .filter(|((node, _), _)| graph.contains(*node))
            .map(|(&(node, param), last)| CoreMessage::Resend {
                time: last.time,
                event: Event::Param {
                    node,
                    param,
                    value: last.value,
                },
            })
            .collect();
        if self.core.slots() < 1 + resend.len() {
            return Err(SwapError::QueueFull(Box::new(graph)));
        }
        for message in std::iter::once(CoreMessage::Swap(Box::new(graph))).chain(resend) {
            let sent = self.core.push(message);
            debug_assert!(sent.is_ok(), "slots were checked");
        }
        Ok(())
    }

    /// Forgets the parameter values sent so far, so swaps stop sending them
    /// again: for when the graphs to come belong to another project, whose
    /// nodes have their own values.
    pub fn forget_params(&mut self) {
        self.params.clear();
    }

    /// Replaces the engine's tempo map at the start of its next block. The
    /// playhead keeps its musical position, so changing the tempo during
    /// playback doesn't jump, and the loop region keeps its place in bars
    /// and beats.
    pub fn set_tempo_map(&mut self, tempo: TempoMap) -> Result<(), QueueFull<TempoMap>> {
        if self.core.is_full() {
            return Err(QueueFull(tempo));
        }
        let live = Box::new(tempo.clone());
        let sent = self.core.push(CoreMessage::Tempo(Box::new(tempo), live));
        debug_assert!(sent.is_ok(), "the queue had room");
        Ok(())
    }

    /// Plays `preview` from the start of the engine's next `process` call,
    /// fading out the preview playing, if one is: see [`Preview`].
    pub fn preview(&mut self, preview: Preview) -> Result<(), QueueFull<Box<Preview>>> {
        match self
            .live
            .push(LiveMessage::Preview(Some(Box::new(preview))))
        {
            Ok(()) => Ok(()),
            Err(rtrb::PushError::Full(LiveMessage::Preview(Some(preview)))) => {
                Err(QueueFull(preview))
            }
            Err(_) => unreachable!("the message pushed was a preview"),
        }
    }

    /// Fades out the preview playing, if one is.
    pub fn stop_preview(&mut self) -> Result<(), QueueFull<()>> {
        self.live
            .push(LiveMessage::Preview(None))
            .map_err(|_| QueueFull(()))
    }

    /// Plays previews at `gain` from the engine's next `process` call on:
    /// 1.0 plays them as they are. The engine ramps to it over a block.
    pub fn set_preview_gain(&self, gain: f32) {
        self.counters.preview_gain.store(gain.to_bits(), Relaxed);
    }

    /// Has the engine write the record taps with `key` to `writer`, in
    /// place of the writer it had for them, from the start of its next
    /// `process` call: see [`TapWriter`]. A writer for a key that's new
    /// gets silence for what the engine had already rendered ahead. Gives
    /// the writer back if the message queue is full, or the engine writes
    /// to as many writers as it can already.
    pub fn add_writer(
        &mut self,
        key: NodeKey,
        writer: Box<dyn TapWriter>,
    ) -> Result<(), Box<dyn TapWriter>> {
        if self.writers.contains(&key) {
            return match self.live.push(LiveMessage::Writer(key, writer, None)) {
                Ok(()) => Ok(()),
                Err(rtrb::PushError::Full(LiveMessage::Writer(_, writer, _))) => Err(writer),
                Err(_) => unreachable!("the message pushed was a writer"),
            };
        }
        if self.writers.len() >= MAX_WRITERS || self.core.is_full() || self.live.is_full() {
            return Err(writer);
        }
        let (core, live) = ahead::writer_queues(key, self.config);
        let sent = (self.core.push(CoreMessage::Writer(key, Some(core))).is_ok())
            && (self.live)
                .push(LiveMessage::Writer(key, writer, Some(live)))
                .is_ok();
        debug_assert!(sent, "both queues had room");
        self.writers.push(key);
        Ok(())
    }

    /// Has the engine stop writing the record taps with `key`, and drop
    /// their writer.
    pub fn remove_writer(&mut self, key: NodeKey) -> Result<(), QueueFull<NodeKey>> {
        if !self.writers.contains(&key) {
            return Ok(());
        }
        if self.core.is_full() || self.live.is_full() {
            return Err(QueueFull(key));
        }
        let sent = self.core.push(CoreMessage::Writer(key, None)).is_ok()
            && self.live.push(LiveMessage::RemoveWriter(key)).is_ok();
        debug_assert!(sent, "both queues had room");
        self.writers.retain(|&k| k != key);
        Ok(())
    }

    /// The keys of the writers the engine has, or will have once it takes
    /// what was sent, in the order they were added.
    pub fn writers(&self) -> &[NodeKey] {
        &self.writers
    }

    /// Drops the graphs, tempo maps, previews and writers the engine has
    /// replaced or finished with, and returns how many there were. Call it
    /// regularly, for example once per UI frame: the engine holds off swaps
    /// while too many are waiting.
    pub fn collect_garbage(&mut self) -> usize {
        let mut dropped = 0;
        while let Ok(garbage) = self.core_garbage.pop() {
            match garbage {
                CoreGarbage::Graph(graph) => drop(graph),
                CoreGarbage::Tempo(tempo) => drop(tempo),
                // A writer's end of its queues isn't the writer.
                CoreGarbage::Writer(queues) => {
                    drop(queues);
                    continue;
                }
                CoreGarbage::Log(log) => drop(log),
            }
            dropped += 1;
        }
        while let Ok(garbage) = self.live_garbage.pop() {
            match garbage {
                LiveGarbage::Preview(preview) => drop(preview),
                LiveGarbage::Writer(writer) => drop(writer),
                LiveGarbage::Queue(queue) => {
                    drop(queue);
                    continue;
                }
                // The other halves of these came back from the ahead side.
                LiveGarbage::Graph(graph) => {
                    drop(graph);
                    continue;
                }
                LiveGarbage::Tempo(tempo) => {
                    drop(tempo);
                    continue;
                }
                LiveGarbage::Record(tap) => drop(tap),
            }
            dropped += 1;
        }
        dropped
    }

    /// Sends the ahead side `message` as it is, for a test to apply to
    /// this engine what another applied.
    #[cfg(test)]
    pub(crate) fn send_message(&mut self, message: CoreMessage) {
        assert!(self.core.push(message).is_ok(), "the queue has room");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread::{self, ThreadId};

    use musicable_model::time::{BeatRange, Beats, TimeSignature};

    use super::*;
    use crate::nodes::Constant;
    use crate::output_recording::AppliedAt;
    use crate::{Graph, Inputs, Node, NodeEventKind, ParamId, ProcessContext, TransportCommand};

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    /// Counts frames: state that has to survive a swap.
    struct Counter(f32);

    impl Node for Counter {
        fn channels(&self) -> usize {
            1
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<Counter>() {
                self.0 = previous.0;
            }
        }

        fn process(
            &mut self,
            _ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            for sample in output.channel_mut(0) {
                *sample = self.0;
                self.0 += 1.0;
            }
        }
    }

    /// Outputs the song position while playing, and -1 while stopped.
    struct Playhead;

    impl Node for Playhead {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            for (i, sample) in output.channel_mut(0).iter_mut().enumerate() {
                *sample = match ctx.transport.playing {
                    true => (ctx.transport.position + i as u64) as f32,
                    false => -1.0,
                };
            }
        }
    }

    /// Outputs 1 while the transport records, and 0 otherwise.
    struct Recording;

    impl Node for Recording {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            let recording = if ctx.transport.recording { 1.0 } else { 0.0 };
            output.channel_mut(0).fill(recording);
        }
    }

    /// Multiplies its input by a gain that jumps to each new value on the
    /// frame it's sent for: when events land, without the ramp a real gain
    /// stage makes.
    struct Level(f32);

    impl Level {
        const GAIN: ParamId = ParamId(0);
    }

    impl Node for Level {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
            for input in inputs.iter() {
                output.add_from(input);
            }
            let mut events = ctx.events.iter().peekable();
            for (frame, sample) in output.channel_mut(0).iter_mut().enumerate() {
                while let Some(event) = events.next_if(|e| e.offset == frame) {
                    if let NodeEventKind::Param { param, value } = event.kind
                        && param == Self::GAIN
                    {
                        self.0 = value as f32;
                    }
                }
                *sample *= self.0;
            }
        }
    }

    /// Outputs the device inputs' channel `0`, or silence without one.
    struct Echo(usize);

    impl Node for Echo {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            assert_eq!(ctx.input.frames(), ctx.frames);
            match ctx.input.channels() > self.0 {
                true => output
                    .channel_mut(0)
                    .copy_from_slice(ctx.input.channel(self.0)),
                false => output.clear(),
            }
        }
    }

    /// Outputs each MIDI message's key at the frame it plays.
    struct Keys;

    impl Node for Keys {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
            let out = output.channel_mut(0);
            for message in ctx.midi {
                let at = message.time.saturating_sub(ctx.time) as usize;
                out[at.min(ctx.frames - 1)] += f32::from(message.bytes[1]);
            }
        }
    }

    /// Reports the thread it is dropped on.
    struct DropProbe(mpsc::Sender<ThreadId>);

    impl Node for DropProbe {
        fn process(
            &mut self,
            _ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
        }
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.send(thread::current().id());
        }
    }

    fn single(key: u64, node: impl Node) -> PreparedGraph {
        let mut g = Graph::new();
        let id = g.add(NodeKey(key), node);
        g.set_output(id).unwrap();
        g.prepare(&CONFIG).unwrap()
    }

    /// A constant 1.0 through a [`Level`] with key 1.
    fn gain_stage() -> PreparedGraph {
        let mut g = Graph::new();
        let one = g.add(NodeKey(0), Constant::new(1.0).with_channels(1));
        let gain = g.add(NodeKey(1), Level(1.0));
        g.connect(one, gain).unwrap();
        g.set_output(gain).unwrap();
        g.prepare(&CONFIG).unwrap()
    }

    fn gain(value: f64) -> Event {
        Event::Param {
            node: NodeKey(1),
            param: Level::GAIN,
            value,
        }
    }

    /// Renders `frames` mono frames in device callbacks of `sizes`, repeated.
    fn render(engine: &mut Engine, frames: usize, sizes: &[usize]) -> Vec<f32> {
        let mut out = vec![0.0; frames];
        let mut done = 0;
        for &size in sizes.iter().cycle() {
            let size = size.min(frames - done);
            engine.process(&mut AudioMut::from_planar(&mut out[done..done + size], 1));
            done += size;
            if done == frames {
                break;
            }
        }
        out
    }

    #[test]
    fn an_output_recording_keeps_what_played_and_when_events_applied() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(gain_stage()).unwrap();
        render(&mut engine, 100, &[100]);
        let mut reader = handle.record_output().unwrap();
        render(&mut engine, 64, &[64]);
        handle.send(gain(0.5)).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        engine.record_xrun();
        let played = render(&mut engine, 64, &[32]);
        let sent = handle.stop_recording_output().unwrap();
        // Past the stop, nothing is kept.
        render(&mut engine, 64, &[64]);
        let (mut samples, mut callbacks, mut applied) = (Vec::new(), Vec::new(), Vec::new());
        let read = reader.read(&mut samples, &mut callbacks, &mut applied);
        assert!(read.ended, "{read:?}");
        let times: Vec<(u64, u32, u64)> = callbacks
            .iter()
            .map(|c| (c.time, c.frames, c.xruns))
            .collect();
        assert_eq!(times, [(100, 64, 0), (164, 32, 1), (196, 32, 1)]);
        assert!(callbacks.iter().all(|c| c.kept && c.channels == 1));
        assert_eq!(samples.len(), 128);
        assert_eq!(&samples[64..], &played[..]);
        assert!(samples[..64].iter().all(|&s| s == 1.0));
        assert!(played.iter().all(|&s| s == 0.5));
        // Each applied at the start of the next block, which played next.
        assert_eq!(
            applied,
            [
                AppliedAt {
                    time: 164,
                    applied: Applied::Event(Event::Transport(TransportCommand::Play))
                },
                AppliedAt {
                    time: 164,
                    applied: Applied::Event(gain(0.5))
                },
            ]
        );
        assert_eq!(
            sent.iter().map(|s| (s.time, s.event)).collect::<Vec<_>>(),
            [
                (164, gain(0.5)),
                (164, Event::Transport(TransportCommand::Play))
            ]
        );
        assert!(handle.stop_recording_output().is_none());
        handle.collect_garbage();
    }

    #[test]
    fn plays_silence_without_a_graph() {
        let (mut engine, handle) = Engine::new(CONFIG);
        let mut out = vec![1.0; 2 * 300];
        engine.process_interleaved(&mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(handle.stats().time, 300);
    }

    #[test]
    fn offline_renders_take_the_taps_and_leave_the_cue_out() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let mut g = Graph::new();
        let counter = g.add(NodeKey(0), Counter(0.0));
        let half = g.add(NodeKey(1), Level(0.5));
        let click = g.add(NodeKey(2), Constant::new(1000.0).with_channels(1));
        g.connect(counter, half).unwrap();
        g.set_output(half).unwrap();
        g.set_cue(click).unwrap();
        g.add_tap(counter).unwrap();
        let frames = 300;
        let mut output = vec![-1.0; frames];
        let mut taps = [vec![-1.0; frames], vec![-1.0; frames]];
        let render = |engine: &mut Engine, output: &mut [f32], taps: &mut [Vec<f32>; 2]| {
            let [first, second] = taps;
            let mut taps = [
                AudioMut::from_planar(first, 1),
                AudioMut::from_planar(second, 1),
            ];
            engine.process_with_taps(&mut AudioMut::from_planar(output, 1), &mut taps);
        };
        // Silence, before the graph.
        render(&mut engine, &mut output, &mut taps);
        assert!(
            output
                .iter()
                .chain(taps.iter().flatten())
                .all(|&s| s == 0.0)
        );
        handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
        render(&mut engine, &mut output, &mut taps);
        // The counter counts from the graph's first frame.
        let count = |scale: f32| -> Vec<f32> { (0..300).map(|i| i as f32 * scale).collect() };
        assert_eq!(output, count(0.5), "without the cue");
        assert_eq!(taps[0], count(1.0));
        assert!(taps[1].iter().all(|&s| s == 0.0), "no second tap");
    }

    /// Hands on each block it's written: its time and length, and its first
    /// channel, with silence as zeros.
    struct Kept {
        blocks: rtrb::Producer<(u64, usize)>,
        samples: rtrb::Producer<f32>,
    }

    impl TapWriter for Kept {
        fn write(&mut self, time: u64, audio: AudioRef<'_>) {
            self.blocks.push((time, audio.frames())).unwrap();
            for i in 0..audio.frames() {
                let sample = if audio.channels() > 0 {
                    audio.channel(0)[i]
                } else {
                    0.0
                };
                self.samples.push(sample).unwrap();
            }
        }
    }

    type Blocks = rtrb::Consumer<(u64, usize)>;

    /// A [`Kept`] with room for a test's blocks, and what it hands on.
    fn kept() -> (Box<dyn TapWriter>, Blocks, rtrb::Consumer<f32>) {
        let (blocks, blocks_rx) = RingBuffer::new(64);
        let (samples, samples_rx) = RingBuffer::new(1024);
        (Box::new(Kept { blocks, samples }), blocks_rx, samples_rx)
    }

    fn drain<T>(queue: &mut rtrb::Consumer<T>) -> Vec<T> {
        std::iter::from_fn(|| queue.pop().ok()).collect()
    }

    #[test]
    fn record_taps_go_to_their_writers_block_by_block() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let (writer, mut blocks, mut samples) = kept();
        let key = NodeKey(7);
        assert!(handle.add_writer(key, writer).is_ok());
        assert_eq!(handle.writers(), [key]);
        // Silence, before the graph.
        render(&mut engine, 100, &[100]);
        let mut g = Graph::new();
        let counter = g.add(NodeKey(0), Counter(0.0));
        let half = g.add(NodeKey(1), Level(0.5));
        g.connect(counter, half).unwrap();
        g.set_output(half).unwrap();
        assert_eq!(g.add_record(key, counter), Ok(0));
        assert_eq!(g.add_record(key, half), Ok(0), "one tap for a key");
        let g = g.prepare(&CONFIG).unwrap();
        assert!(g.records().eq([key]));
        handle.swap_graph(g).unwrap();
        render(&mut engine, 200, &[7, 64, 129]);
        // Each block from where the one before ended.
        let mut time = 0;
        for (at, frames) in drain(&mut blocks) {
            assert_eq!(at, time);
            time += frames as u64;
        }
        assert_eq!(time, 300);
        let written = drain(&mut samples);
        let count: Vec<f32> = (0..200).map(|i| i as f32).collect();
        assert_eq!(written[..100], [0.0; 100]);
        assert_eq!(written[100..], count);
        // Another writer for the key takes over, and the first one is
        // dropped on the control thread, as is the last one once removed.
        let (writer, mut other, _) = kept();
        assert!(handle.add_writer(key, writer).is_ok());
        render(&mut engine, 10, &[10]);
        assert!(blocks.is_empty());
        assert_eq!(drain(&mut other), [(300, 10)]);
        assert!(!blocks.is_abandoned());
        assert_eq!(handle.collect_garbage(), 1);
        assert!(blocks.is_abandoned());
        handle.remove_writer(key).unwrap();
        assert!(handle.writers().is_empty());
        render(&mut engine, 10, &[10]);
        assert_eq!(handle.collect_garbage(), 1);
        assert!(other.is_abandoned());
    }

    #[test]
    fn callbacks_of_any_size_render_the_same_audio() {
        for sizes in [&[1][..], &[7, 64, 65, 200, 3], &[1000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Counter(0.0))).unwrap();
            let out = render(&mut engine, 2000, sizes);
            assert!(
                out.iter().enumerate().all(|(i, &s)| s == i as f32),
                "{sizes:?}"
            );
        }
    }

    #[test]
    fn nodes_get_each_frame_of_the_device_inputs_with_its_own_frame() {
        for sizes in [&[1][..], &[7, 64, 65, 200, 3], &[1000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Echo(1))).unwrap();
            // A loop splits blocks where it jumps back, at frames 12,000 and
            // 24,000, wherever the callbacks fall.
            handle
                .send(looping(Beats::from_ratio(1, 2), Beats::new(1)))
                .unwrap();
            handle.send(play()).unwrap();
            let frames = 40_000;
            // Channel 1 counts frames, channel 0 is noise.
            let mut input = vec![0.5; 2 * frames];
            for (i, sample) in input[frames..].iter_mut().enumerate() {
                *sample = i as f32;
            }
            let mut out = vec![0.0; frames];
            let mut done = 0;
            for &size in sizes.iter().cycle() {
                let size = size.min(frames - done);
                let planar: Vec<f32> = (0..2)
                    .flat_map(|c| input[c * frames + done..][..size].iter().copied())
                    .collect();
                assert_eq!(engine.time(), done as u64);
                engine.process_with_input(
                    AudioRef::from_planar(&planar, 2),
                    &[],
                    &mut AudioMut::from_planar(&mut out[done..done + size], 1),
                );
                done += size;
                if done == frames {
                    break;
                }
            }
            assert!(
                out.iter().enumerate().all(|(i, &s)| s == i as f32),
                "{sizes:?}"
            );
        }
    }

    #[test]
    fn midi_messages_come_in_the_blocks_they_play_in() {
        for sizes in [&[1][..], &[7, 64, 65, 200, 3], &[1000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Keys)).unwrap();
            let frames = 1000;
            let messages: Vec<MidiMessage> = (0..100u8)
                .map(|i| MidiMessage {
                    time: 9 * u64::from(i) + 3,
                    input: 0,
                    bytes: [0x90, i + 1, 100],
                })
                .collect();
            let mut out = vec![0.0; frames];
            let mut done = 0;
            for &size in sizes.iter().cycle() {
                let size = size.min(frames - done);
                let callback = done as u64..(done + size) as u64;
                let midi: Vec<MidiMessage> = (messages.iter())
                    .filter(|m| callback.contains(&m.time))
                    .copied()
                    .collect();
                engine.process_with_input(
                    AudioRef::empty(size),
                    &midi,
                    &mut AudioMut::from_planar(&mut out[done..done + size], 1),
                );
                done += size;
                if done == frames {
                    break;
                }
            }
            for message in &messages {
                let key = f32::from(message.bytes[1]);
                assert_eq!(out[message.time as usize], key, "{sizes:?}");
            }
            let played = out.iter().filter(|&&s| s != 0.0).count();
            assert_eq!(played, messages.len(), "{sizes:?}");
        }
    }

    #[test]
    fn midi_messages_timed_outside_the_callback_play_at_its_ends() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Keys)).unwrap();
        let mut out = vec![0.0; 100];
        engine.process(&mut AudioMut::from_planar(&mut out, 1));
        let message = |time, key| MidiMessage {
            time,
            input: 0,
            bytes: [0x90, key, 100],
        };
        // Late, on time and early. The callback is two blocks long.
        let midi = [message(50, 7), message(130, 9), message(400, 11)];
        engine.process_with_input(
            AudioRef::empty(100),
            &midi,
            &mut AudioMut::from_planar(&mut out, 1),
        );
        assert_eq!((out[0], out[30], out[99]), (7.0, 9.0, 11.0));
        assert_eq!(out.iter().filter(|&&s| s != 0.0).count(), 3);
    }

    #[test]
    fn interleaved_output_takes_device_inputs_too() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Echo(0))).unwrap();
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = vec![0.0; 2 * 100];
        engine.process_interleaved_with_input(AudioRef::from_planar(&input, 1), &[], &mut out, 2);
        for (frame, samples) in out.chunks(2).enumerate() {
            assert_eq!(samples, [frame as f32; 2]);
        }
        // Without inputs, nodes see none.
        engine.process_interleaved(&mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    #[should_panic(expected = "the device inputs are 10 frames long, the output 20")]
    fn device_inputs_cover_the_output_exactly() {
        let (mut engine, _handle) = Engine::new(CONFIG);
        let input = [0.0; 10];
        let mut out = [0.0; 20];
        engine.process_with_input(
            AudioRef::from_planar(&input, 1),
            &[],
            &mut AudioMut::from_planar(&mut out, 1),
        );
    }

    #[test]
    fn interleaved_output_matches_planar() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Counter(0.0))).unwrap();
        let mut out = vec![0.0; 3 * 100];
        engine.process_interleaved(&mut out, 3);
        for (frame, samples) in out.chunks(3).enumerate() {
            // A mono output feeds every device channel.
            assert_eq!(samples, [frame as f32; 3]);
        }
    }

    #[test]
    fn the_cue_output_plays_on_top_of_the_output() {
        let mut g = Graph::new();
        let output = g.add(NodeKey(0), Counter(0.0));
        let cue = g.add(NodeKey(1), Constant::new(0.5));
        g.set_output(output).unwrap();
        g.set_cue(cue).unwrap();
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
        let out = render(&mut engine, 300, &[7, 200]);
        assert!(out.iter().enumerate().all(|(i, &s)| s == i as f32 + 0.5));
        let mut out = vec![0.0; 2 * 100];
        engine.process_interleaved(&mut out, 2);
        for (frame, samples) in out.chunks(2).enumerate() {
            assert_eq!(samples, [(300 + frame) as f32 + 0.5; 2]);
        }
    }

    #[test]
    fn parameter_events_land_on_their_frame() {
        for sizes in [&[1][..], &[37], &[64, 13, 200]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(gain_stage()).unwrap();
            handle.send_at(100, gain(0.5)).unwrap();
            handle.send_at(150, gain(0.0)).unwrap();
            let out = render(&mut engine, 300, sizes);
            assert_eq!(out[..100], [1.0; 100], "{sizes:?}");
            assert_eq!(out[100..150], [0.5; 50], "{sizes:?}");
            assert_eq!(out[150..], [0.0; 150], "{sizes:?}");
            assert_eq!(handle.stats().events_late, 0);
        }
    }

    #[test]
    fn events_sent_now_apply_at_the_next_block() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(gain_stage()).unwrap();
        let first = render(&mut engine, 10, &[10]);
        handle.send(gain(0.25)).unwrap();
        let second = render(&mut engine, 10, &[10]);
        assert_eq!(first, [1.0; 10]);
        assert_eq!(second, [0.25; 10]);
        assert_eq!(handle.stats().events_late, 0);
    }

    #[test]
    fn late_events_apply_at_once_and_count() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(gain_stage()).unwrap();
        render(&mut engine, 100, &[100]);
        handle.send_at(50, gain(0.5)).unwrap();
        assert_eq!(render(&mut engine, 10, &[10]), [0.5; 10]);
        assert_eq!(handle.stats().events_late, 1);
    }

    #[test]
    fn transport_changes_land_on_their_frame() {
        for sizes in [&[1][..], &[7], &[64]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Playhead)).unwrap();
            for (time, command) in [
                (10, TransportCommand::Play),
                // Frame 1000, at 120 BPM and 48 kHz.
                (20, TransportCommand::Locate(Beats::from_ratio(1, 24))),
                (30, TransportCommand::Stop),
                (35, TransportCommand::Play),
            ] {
                handle.send_at(time, Event::Transport(command)).unwrap();
            }
            let out = render(&mut engine, 40, sizes);
            let expected: Vec<f32> = (0..40)
                .map(|i| match i {
                    0..10 => -1.0,
                    10..20 => (i - 10) as f32,
                    20..30 => (1000 + i - 20) as f32,
                    30..35 => -1.0,
                    _ => (1010 + i - 35) as f32,
                })
                .collect();
            assert_eq!(out, expected, "{sizes:?}");
        }
    }

    fn play() -> Event {
        Event::Transport(TransportCommand::Play)
    }

    fn looping(start: Beats, end: Beats) -> Event {
        Event::Transport(TransportCommand::Loop(Some(BeatRange::new(start, end))))
    }

    #[test]
    fn loops_jump_back_on_their_frame() {
        for sizes in [&[1][..], &[7, 64, 1000], &[5000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Playhead)).unwrap();
            // Frames 12,000 to 24,000.
            handle
                .send(looping(Beats::from_ratio(1, 2), Beats::new(1)))
                .unwrap();
            handle.send(play()).unwrap();
            let out = render(&mut engine, 40_000, sizes);
            let expected: Vec<f32> = (0..40_000)
                .map(|i| match i {
                    0..24_000 => i,
                    _ => 12_000 + (i - 24_000) % 12_000,
                } as f32)
                .collect();
            assert_eq!(out, expected, "{sizes:?}");
        }
    }

    #[test]
    fn looping_starts_and_stops_on_its_frame() {
        for sizes in [&[1][..], &[7], &[64, 3000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(single(0, Playhead)).unwrap();
            handle.send(play()).unwrap();
            // Frames 0 to 2000, while the playhead is inside.
            let region = looping(Beats::ZERO, Beats::from_ratio(1, 12));
            handle.send_at(1000, region).unwrap();
            let off = Event::Transport(TransportCommand::Loop(None));
            handle.send_at(5000, off).unwrap();
            let out = render(&mut engine, 8000, sizes);
            let expected: Vec<f32> = (0..8000)
                .map(|i| match i {
                    0..2000 => i,
                    2000..4000 => i - 2000,
                    _ => i - 4000,
                } as f32)
                .collect();
            assert_eq!(out, expected, "{sizes:?}");
        }
    }

    #[test]
    fn recording_punches_in_and_out_on_its_frames_and_the_timeline_follows() {
        for sizes in [&[1][..], &[7, 64, 1000], &[5000]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            let mut timeline = handle.take_timeline().unwrap();
            assert!(handle.take_timeline().is_none());
            handle.swap_graph(single(0, Recording)).unwrap();
            // Looping over frames 0 to 18,000, recording from 6,000 to 12,000.
            let quarter = |n| Some(Beats::from_ratio(n, 4));
            let punch = TransportCommand::Punch {
                start: quarter(1),
                end: quarter(2),
            };
            handle.send(Event::Transport(punch)).unwrap();
            handle
                .send(looping(Beats::ZERO, Beats::from_ratio(3, 4)))
                .unwrap();
            let record = Event::Transport(TransportCommand::Record(true));
            handle.send(record).unwrap();
            handle.send(play()).unwrap();
            let out = render(&mut engine, 40_000, sizes);
            let expected: Vec<f32> = (0..40_000)
                .map(|i| match i % 18_000 {
                    6_000..12_000 => 1.0,
                    _ => 0.0,
                })
                .collect();
            assert_eq!(out, expected, "{sizes:?}");

            let mut segments = Vec::new();
            let read = timeline.read(&mut segments);
            assert_eq!((read.until, read.lost), (40_000, false));
            let course: Vec<(u64, u64, bool)> = segments
                .iter()
                .map(|s| {
                    assert!(s.playing && s.record && s.latency == 0);
                    (s.time, s.position, s.recording)
                })
                .collect();
            assert_eq!(
                course,
                [
                    (0, 0, false),
                    (6_000, 6_000, true),
                    (12_000, 12_000, false),
                    (18_000, 0, false),
                    (24_000, 6_000, true),
                    (30_000, 12_000, false),
                    (36_000, 0, false),
                ],
                "{sizes:?}"
            );

            // Stopping stops recording.
            handle
                .send(Event::Transport(TransportCommand::Stop))
                .unwrap();
            render(&mut engine, 10, sizes);
            segments.clear();
            timeline.read(&mut segments);
            assert_eq!(segments.len(), 1);
            assert!(!segments[0].playing && !segments[0].record);
        }
    }

    #[test]
    fn a_playhead_past_the_loop_end_plays_on() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Playhead)).unwrap();
        let locate = TransportCommand::Locate(Beats::new(2));
        handle.send(Event::Transport(locate)).unwrap();
        handle.send(looping(Beats::ZERO, Beats::new(1))).unwrap();
        handle.send(play()).unwrap();
        let out = render(&mut engine, 100, &[64]);
        let expected: Vec<f32> = (48_000..48_100).map(|i| i as f32).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn a_new_tempo_keeps_the_musical_position() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Playhead)).unwrap();
        handle.send(play()).unwrap();
        // A beat and a half at 120 BPM.
        render(&mut engine, 36_000, &[1000]);
        handle
            .set_tempo_map(TempoMap::new(60.0, TimeSignature::FOUR_FOUR))
            .unwrap();
        // At 60 BPM a beat and a half is twice as many frames.
        let out = render(&mut engine, 10, &[10]);
        let expected: Vec<f32> = (72_000..72_010).map(|i| i as f32).collect();
        assert_eq!(out, expected);
        assert_eq!(handle.stats().position, 72_010);
        assert_eq!(handle.collect_garbage(), 1, "the old map comes back");
    }

    #[test]
    fn tempo_maps_wait_for_garbage_to_be_collected() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let slow = || TempoMap::new(60.0, TimeSignature::FOUR_FOUR);
        for _ in 0..GARBAGE_CAPACITY {
            handle.set_tempo_map(slow()).unwrap();
            render(&mut engine, 1, &[1]);
        }
        // The garbage queue is full, so this map waits, with the locate
        // behind it.
        handle
            .set_tempo_map(TempoMap::new(90.0, TimeSignature::FOUR_FOUR))
            .unwrap();
        let locate = TransportCommand::Locate(Beats::new(1));
        handle.send(Event::Transport(locate)).unwrap();
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().position, 0);

        assert_eq!(handle.collect_garbage(), GARBAGE_CAPACITY);
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().position, 32_000);
    }

    #[test]
    fn stats_report_the_transport() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let locate = TransportCommand::Locate(Beats::new(1));
        handle.send(Event::Transport(locate)).unwrap();
        render(&mut engine, 10, &[10]);
        let stats = handle.stats();
        assert_eq!((stats.position, stats.playing), (24_000, false));
        handle.send(play()).unwrap();
        render(&mut engine, 10, &[10]);
        let stats = handle.stats();
        assert_eq!((stats.position, stats.playing), (24_010, true));
    }

    #[test]
    fn swapped_in_nodes_take_over_state() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(7, Counter(0.0))).unwrap();
        let before = render(&mut engine, 50, &[50]);
        handle.swap_graph(single(7, Counter(0.0))).unwrap();
        let after = render(&mut engine, 50, &[50]);
        assert_eq!(before.last(), Some(&49.0));
        assert_eq!(after.first(), Some(&50.0));

        // A node with a new key starts fresh.
        handle.swap_graph(single(8, Counter(0.0))).unwrap();
        assert_eq!(render(&mut engine, 2, &[2]), [0.0, 1.0]);
        assert_eq!(handle.stats().swaps, 3);
        assert_eq!(handle.collect_garbage(), 2);
    }

    #[test]
    fn old_graphs_are_dropped_on_the_control_thread() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        let (probe, dropped) = mpsc::channel();
        handle.swap_graph(single(0, DropProbe(probe))).unwrap();
        render(&mut engine, 10, &[10]);
        handle.swap_graph(single(0, Constant::new(0.0))).unwrap();

        let audio = thread::spawn(move || {
            render(&mut engine, 10, &[10]);
            (engine, thread::current().id())
        });
        let (_engine, audio_thread) = audio.join().unwrap();
        assert!(dropped.try_recv().is_err(), "the old graph is waiting");
        assert_eq!(handle.collect_garbage(), 1);
        let dropped_on = dropped.try_recv().unwrap();
        assert_eq!(dropped_on, thread::current().id());
        assert_ne!(dropped_on, audio_thread);
    }

    #[test]
    fn swaps_wait_for_garbage_to_be_collected() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        for _ in 0..=GARBAGE_CAPACITY {
            handle.swap_graph(single(0, Counter(0.0))).unwrap();
            render(&mut engine, 1, &[1]);
        }
        assert_eq!(handle.stats().swaps, GARBAGE_CAPACITY as u64 + 1);

        // The garbage queue is full, so this one waits, with the events behind it.
        handle.swap_graph(single(0, Counter(0.0))).unwrap();
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().swaps, GARBAGE_CAPACITY as u64 + 1);

        assert_eq!(handle.collect_garbage(), GARBAGE_CAPACITY);
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().swaps, GARBAGE_CAPACITY as u64 + 2);
    }

    #[test]
    fn graphs_for_another_config_are_refused() {
        let (_engine, mut handle) = Engine::new(CONFIG);
        let mut g = Graph::new();
        let id = g.add(NodeKey(0), Constant::new(0.0));
        g.set_output(id).unwrap();
        let other = EngineConfig {
            max_block: 128,
            ..CONFIG
        };
        let refused = handle.swap_graph(g.prepare(&other).unwrap());
        assert!(matches!(refused, Err(SwapError::ConfigMismatch(_))));
    }

    #[test]
    fn a_full_queue_hands_the_event_back() {
        let (_engine, mut handle) = Engine::new(CONFIG);
        for _ in 0..QUEUE_CAPACITY {
            handle.send(gain(1.0)).unwrap();
        }
        assert_eq!(handle.send(gain(0.5)), Err(QueueFull(gain(0.5))));
        assert!(matches!(
            handle.swap_graph(gain_stage()),
            Err(SwapError::QueueFull(_))
        ));
        let tempo = TempoMap::default();
        assert_eq!(handle.set_tempo_map(tempo.clone()), Err(QueueFull(tempo)));
    }

    #[test]
    fn events_beyond_the_store_are_dropped_and_counted() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        for i in 0..EVENT_CAPACITY {
            handle.send_at(1_000_000 + i as u64, gain(1.0)).unwrap();
        }
        render(&mut engine, 1, &[1]);
        handle.send(gain(1.0)).unwrap();
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().events_dropped, 1);
    }

    #[test]
    fn events_for_missing_nodes_are_counted() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.send(gain(0.5)).unwrap();
        render(&mut engine, 1, &[1]);
        handle.swap_graph(single(0, Constant::new(1.0))).unwrap();
        handle.send(gain(0.5)).unwrap();
        render(&mut engine, 1, &[1]);
        assert_eq!(handle.stats().events_unmatched, 2);
    }

    #[test]
    fn values_sent_during_a_compile_reach_new_nodes() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(single(0, Constant::new(1.0))).unwrap();
        render(&mut engine, 1, &[1]);
        // The gain stage is on its way, compiled before this change.
        handle.send(gain(0.5)).unwrap();
        render(&mut engine, 1, &[1]);
        handle.swap_graph(gain_stage()).unwrap();
        assert_eq!(render(&mut engine, 4, &[4]), [0.5; 4]);
    }

    #[test]
    fn swaps_send_again_the_value_that_applies_last() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(gain_stage()).unwrap();
        handle.send_at(100, gain(0.5)).unwrap();
        // Sent later, but applies first.
        handle.send(gain(0.25)).unwrap();
        render(&mut engine, 200, &[200]);
        // The new gain stage starts at unity and gets 0.5 again, on time.
        handle.swap_graph(gain_stage()).unwrap();
        assert_eq!(render(&mut engine, 4, &[4]), [0.5; 4]);
        assert_eq!(handle.stats().events_late, 0);
    }

    #[test]
    fn forgotten_values_are_not_sent_again() {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(gain_stage()).unwrap();
        handle.send(gain(0.5)).unwrap();
        render(&mut engine, 4, &[4]);
        // Another project's graph, with its own value.
        handle.forget_params();
        handle.swap_graph(gain_stage()).unwrap();
        assert_eq!(render(&mut engine, 4, &[4]), [1.0; 4]);
    }

    #[test]
    fn the_backend_reports_xruns() {
        let (engine, handle) = Engine::new(CONFIG);
        engine.record_xrun();
        assert_eq!(handle.stats().xruns, 1);
        assert_eq!(engine.stats(), handle.stats());
    }
}

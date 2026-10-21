//! The engine, as the audio thread runs it, and the handle the control thread
//! drives it with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use musicable_model::time::TempoMap;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::graph::Routed;
use crate::transport::Transport;
use crate::{
    AudioBuffer, AudioMut, AudioRef, Block, EngineConfig, Event, NodeEvent, NodeEventKind, NodeKey,
    ParamId, PreparedGraph, ReferenceRenderer, Renderer, rt,
};

/// Messages the control thread can have in flight.
const QUEUE_CAPACITY: usize = 1024;
/// Old graphs and tempo maps the engine can hand back before it holds off
/// further swaps.
const GARBAGE_CAPACITY: usize = 16;
/// Events the engine holds until they are due, and the most one block takes.
pub(crate) const EVENT_CAPACITY: usize = 1024;

enum Message {
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
    Tempo(Box<TempoMap>),
}

/// What the engine hands back to be dropped off the audio thread.
enum Garbage {
    Graph(Box<PreparedGraph>),
    Tempo(Box<TempoMap>),
}

struct Pending {
    time: u64,
    event: Event,
}

#[derive(Default)]
struct Counters {
    time: AtomicU64,
    position: AtomicU64,
    playing: AtomicBool,
    swaps: AtomicU64,
    events_late: AtomicU64,
    events_dropped: AtomicU64,
    events_unmatched: AtomicU64,
    xruns: AtomicU64,
}

/// The engine's counters, as of its last block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames rendered since the engine started. This is the clock
    /// [`EngineHandle::send_at`] times refer to.
    pub time: u64,
    /// The song position, in samples: where the next frame plays from.
    pub position: u64,
    pub playing: bool,
    /// Graphs installed.
    pub swaps: u64,
    /// Events applied later than the time they were sent for.
    pub events_late: u64,
    /// Events dropped because the engine was already holding as many as it
    /// can.
    pub events_dropped: u64,
    /// Parameter events whose node key wasn't in the running graph.
    pub events_unmatched: u64,
    /// Device underruns and overruns, as reported by the audio backend.
    pub xruns: u64,
}

impl Counters {
    fn snapshot(&self) -> Stats {
        Stats {
            time: self.time.load(Relaxed),
            position: self.position.load(Relaxed),
            playing: self.playing.load(Relaxed),
            swaps: self.swaps.load(Relaxed),
            events_late: self.events_late.load(Relaxed),
            events_dropped: self.events_dropped.load(Relaxed),
            events_unmatched: self.events_unmatched.load(Relaxed),
            xruns: self.xruns.load(Relaxed),
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

/// Renders the running graph on the audio thread.
///
/// The engine applies what the control thread sends through its
/// [`EngineHandle`]: graph swaps and tempo maps between blocks, parameter
/// changes and transport commands at the exact frame they are due. It renders
/// whatever number of frames the device asks for, in blocks of at most
/// [`EngineConfig::max_block`], ending a block early wherever the transport
/// changes or a loop jumps back.
///
/// Drop the engine off the audio thread, since that frees its graph.
pub struct Engine {
    config: EngineConfig,
    renderer: Box<dyn Renderer>,
    graph: Option<Box<PreparedGraph>>,
    transport: Transport,
    time: u64,
    messages: Consumer<Message>,
    garbage: Producer<Garbage>,
    /// Events not yet due, by time and then in arrival order.
    pending: Vec<Pending>,
    /// Scratch for the node events of one block.
    routed: Vec<Routed>,
    /// What plays before the first graph arrives.
    silence: AudioBuffer,
    counters: Arc<Counters>,
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
        let (message_tx, message_rx) = RingBuffer::new(QUEUE_CAPACITY);
        let (garbage_tx, garbage_rx) = RingBuffer::new(GARBAGE_CAPACITY);
        let counters = Arc::new(Counters::default());
        let engine = Engine {
            config,
            renderer,
            graph: None,
            transport: Transport::new(config.sample_rate),
            time: 0,
            messages: message_rx,
            garbage: garbage_tx,
            pending: Vec::with_capacity(EVENT_CAPACITY),
            routed: Vec::with_capacity(EVENT_CAPACITY),
            silence: AudioBuffer::new(1, config.max_block),
            counters: counters.clone(),
        };
        let handle = EngineHandle {
            config,
            messages: message_tx,
            garbage: garbage_rx,
            counters,
            params: BTreeMap::new(),
        };
        (engine, handle)
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }

    /// For the audio backend to count a device underrun or overrun.
    pub fn record_xrun(&self) {
        self.counters.xruns.fetch_add(1, Relaxed);
    }

    /// Renders `output.frames()` frames into `output`. Real-time safe.
    pub fn process(&mut self, output: &mut AudioMut<'_>) {
        rt::scope(|| {
            self.run(output.frames(), |start, block| {
                let mut part = output.range_mut(start..start + block.frames());
                part.copy_from(block);
            });
        });
    }

    /// Renders into an interleaved device buffer with `channels` channels.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// If `output.len()` isn't a multiple of `channels`.
    pub fn process_interleaved(&mut self, output: &mut [f32], channels: usize) {
        assert!(
            channels > 0 && output.len() % channels == 0,
            "{} samples don't split into {channels} channels",
            output.len()
        );
        rt::scope(|| {
            self.run(output.len() / channels, |start, block| {
                let part = &mut output[start * channels..(start + block.frames()) * channels];
                block.write_interleaved(part, channels);
            });
        });
    }

    /// Renders `frames` frames block by block, handing each block's output
    /// and its offset to `write`.
    fn run(&mut self, frames: usize, mut write: impl FnMut(usize, AudioRef<'_>)) {
        self.receive();
        let mut done = 0;
        while done < frames {
            let block = self.render(frames - done);
            match &self.graph {
                Some(graph) => write(done, graph.output(block)),
                None => write(done, self.silence.view(block)),
            }
            done += block;
        }
        let transport = self.transport.state();
        self.counters.time.store(self.time, Relaxed);
        self.counters.position.store(transport.position, Relaxed);
        self.counters.playing.store(transport.playing, Relaxed);
    }

    /// Takes everything the control thread has sent.
    fn receive(&mut self) {
        loop {
            match self.messages.peek() {
                // Hold a swap until there is room to hand back what it replaces.
                Ok(Message::Swap(_)) if self.graph.is_some() && self.garbage.is_full() => break,
                Ok(Message::Tempo(_)) if self.garbage.is_full() => break,
                Ok(_) => {}
                Err(_) => break,
            }
            match self.messages.pop() {
                Ok(Message::Swap(graph)) => self.install(graph),
                Ok(Message::Tempo(tempo)) => {
                    let old = self.transport.set_tempo(tempo);
                    // `receive` checked there is room.
                    let returned = self.garbage.push(Garbage::Tempo(old));
                    debug_assert!(returned.is_ok(), "the garbage queue has room");
                }
                Ok(Message::Event { time, event }) => {
                    self.schedule(time.unwrap_or(self.time), event)
                }
                Ok(Message::Resend { time, event }) => {
                    self.schedule(time.map_or(self.time, |t| t.max(self.time)), event)
                }
                Err(_) => break,
            }
        }
    }

    fn install(&mut self, mut graph: Box<PreparedGraph>) {
        if let Some(old) = self.graph.as_deref_mut() {
            graph.adopt_from(old);
        }
        if let Some(old) = self.graph.replace(graph) {
            // `receive` checked there is room.
            let returned = self.garbage.push(Garbage::Graph(old));
            debug_assert!(returned.is_ok(), "the garbage queue has room");
        }
        self.counters.swaps.fetch_add(1, Relaxed);
    }

    fn schedule(&mut self, time: u64, event: Event) {
        if self.pending.len() >= EVENT_CAPACITY {
            self.counters.events_dropped.fetch_add(1, Relaxed);
            return;
        }
        let at = self.pending.partition_point(|p| p.time <= time);
        self.pending.insert(at, Pending { time, event });
    }

    /// Renders the next block, of at most `max` frames, and returns its
    /// length.
    fn render(&mut self, max: usize) -> usize {
        let start = self.time;
        // Transport changes due by now apply before the block. The block ends
        // at the next change, and where the loop jumps back, so each block has
        // one transport state.
        let due_now = self.pending.partition_point(|p| p.time <= start);
        for i in 0..due_now {
            if let Event::Transport(command) = self.pending[i].event {
                self.transport.apply(command);
            }
        }
        self.transport.wrap();
        let mut end = start + max.min(self.config.max_block) as u64;
        if let Some(frames) = self.transport.frames_to_loop_end() {
            end = end.min(start + frames);
        }
        for p in &self.pending[due_now..] {
            if p.time >= end {
                break;
            }
            if matches!(p.event, Event::Transport(_)) {
                end = p.time;
                break;
            }
        }

        let due = self.pending.partition_point(|p| p.time < end);
        let graph = self.graph.as_deref_mut();
        let mut unmatched = 0;
        for p in self.pending.drain(..due) {
            if p.time < start {
                self.counters.events_late.fetch_add(1, Relaxed);
            }
            // Transport changes due now were applied above, and later ones
            // ended the block.
            if let Event::Param { node, param, value } = p.event {
                match graph.as_ref().and_then(|g| g.find(node)) {
                    Some(node) => self.routed.push(Routed {
                        node,
                        order: self.routed.len() as u32,
                        event: NodeEvent {
                            offset: (p.time.max(start) - start) as usize,
                            kind: NodeEventKind::Param { param, value },
                        },
                    }),
                    None => unmatched += 1,
                }
            }
        }
        if unmatched > 0 {
            self.counters.events_unmatched.fetch_add(unmatched, Relaxed);
        }

        let frames = (end - start) as usize;
        if let Some(graph) = graph {
            graph.load_events(&mut self.routed);
            let block = Block {
                frames,
                time: start,
                transport: self.transport.state(),
                tempo: self.transport.tempo(),
            };
            self.renderer.render(graph, &block);
            graph.clear_events(&self.routed);
        }
        self.routed.clear();
        self.time = end;
        self.transport.advance(frames as u64);
        frames
    }
}

/// The control thread's side of an [`Engine`]: sends it graphs and events,
/// and drops the graphs it is done with.
pub struct EngineHandle {
    config: EngineConfig,
    messages: Producer<Message>,
    garbage: Consumer<Garbage>,
    counters: Arc<Counters>,
    /// For each parameter, the value that applies last of those sent. Sent
    /// again after each swap, since a node that is new in the graph missed
    /// anything sent while the graph was being compiled.
    params: BTreeMap<(NodeKey, ParamId), LastValue>,
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

    /// Sends an event to apply at the start of the engine's next `process`
    /// call.
    pub fn send(&mut self, event: Event) -> Result<(), QueueFull<Event>> {
        self.push_event(None, event)
    }

    /// Sends an event to apply at engine time `time` (see [`Stats::time`]).
    /// An event for a time already past applies at the start of the next
    /// `process` call and counts as late.
    pub fn send_at(&mut self, time: u64, event: Event) -> Result<(), QueueFull<Event>> {
        self.push_event(Some(time), event)
    }

    fn push_event(&mut self, time: Option<u64>, event: Event) -> Result<(), QueueFull<Event>> {
        if self.messages.push(Message::Event { time, event }).is_err() {
            return Err(QueueFull(event));
        }
        if let Event::Param { node, param, value } = event {
            // The value sent last isn't always the one that applies last: one
            // sent for now lands before one sent earlier for later.
            let now = self.counters.time.load(Relaxed);
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
    /// engine's next `process` call. Nodes take over the state of the old
    /// nodes with the same keys, and each of their parameters gets the value
    /// sent for it again: the one that applies last, if several were sent.
    pub fn swap_graph(&mut self, graph: PreparedGraph) -> Result<(), SwapError> {
        if graph.config() != self.config {
            return Err(SwapError::ConfigMismatch(Box::new(graph)));
        }
        let resend: Vec<Message> = self
            .params
            .iter()
            .filter(|((node, _), _)| graph.contains(*node))
            .map(|(&(node, param), last)| Message::Resend {
                time: last.time,
                event: Event::Param {
                    node,
                    param,
                    value: last.value,
                },
            })
            .collect();
        if self.messages.slots() < 1 + resend.len() {
            return Err(SwapError::QueueFull(Box::new(graph)));
        }
        for message in std::iter::once(Message::Swap(Box::new(graph))).chain(resend) {
            let sent = self.messages.push(message);
            debug_assert!(sent.is_ok(), "slots were checked");
        }
        Ok(())
    }

    /// Replaces the engine's tempo map at the start of its next `process`
    /// call. The playhead keeps its musical position, so changing the tempo
    /// during playback doesn't jump, and the loop region keeps its place in
    /// bars and beats.
    pub fn set_tempo_map(&mut self, tempo: TempoMap) -> Result<(), QueueFull<TempoMap>> {
        if self.messages.is_full() {
            return Err(QueueFull(tempo));
        }
        let sent = self.messages.push(Message::Tempo(Box::new(tempo)));
        debug_assert!(sent.is_ok(), "the queue had room");
        Ok(())
    }

    /// Drops the graphs and tempo maps the engine has replaced, and returns
    /// how many there were. Call it regularly, for example once per UI frame:
    /// the engine holds off swaps while too many are waiting.
    pub fn collect_garbage(&mut self) -> usize {
        let mut dropped = 0;
        while let Ok(garbage) = self.garbage.pop() {
            match garbage {
                Garbage::Graph(graph) => drop(graph),
                Garbage::Tempo(tempo) => drop(tempo),
            }
            dropped += 1;
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread::{self, ThreadId};

    use musicable_model::time::{BeatRange, Beats, TimeSignature};

    use super::*;
    use crate::nodes::{Constant, Gain};
    use crate::{Graph, Inputs, Node, ProcessContext, TransportCommand};

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

    /// A constant 1.0 through a gain stage with key 1.
    fn gain_stage() -> PreparedGraph {
        let mut g = Graph::new();
        let one = g.add(NodeKey(0), Constant::new(1.0).with_channels(1));
        let gain = g.add(NodeKey(1), Gain::new(1.0).with_channels(1));
        g.connect(one, gain).unwrap();
        g.set_output(gain).unwrap();
        g.prepare(&CONFIG).unwrap()
    }

    fn gain(value: f64) -> Event {
        Event::Param {
            node: NodeKey(1),
            param: Gain::GAIN,
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
    fn plays_silence_without_a_graph() {
        let (mut engine, handle) = Engine::new(CONFIG);
        let mut out = vec![1.0; 2 * 300];
        engine.process_interleaved(&mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(handle.stats().time, 300);
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
    fn parameter_events_land_on_their_frame() {
        for sizes in [&[1][..], &[37], &[64, 13, 200]] {
            let (mut engine, mut handle) = Engine::new(CONFIG);
            handle.swap_graph(gain_stage()).unwrap();
            handle.send_at(100, gain(0.5)).unwrap();
            handle
                .send_at(
                    150,
                    Event::Param {
                        node: NodeKey(1),
                        param: Gain::MUTE,
                        value: 1.0,
                    },
                )
                .unwrap();
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
    fn the_backend_reports_xruns() {
        let (engine, handle) = Engine::new(CONFIG);
        engine.record_xrun();
        assert_eq!(handle.stats().xruns, 1);
        assert_eq!(engine.stats(), handle.stats());
    }
}

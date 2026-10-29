//! Rendering ahead, against rendering each block as the device asks for it.
//!
//! An engine with a lead plays just what an engine without one plays, when
//! the other is given what it applied at the times it applied it and renders
//! the same blocks: the random tests below have an engine without a lead
//! replay what one with a lead logged ([`Applied`]), and compare everything
//! they played, sample for sample. The others check what it applies when,
//! that live sources turn it off, and the threads that render it.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use musicable_model::Monitor;
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::{Config, TestRunner};

use super::*;
use crate::media::{AudioData, Source};
use crate::nodes::{Constant, Delay, InputMonitor, Latent, Spin};
use crate::{
    AudioBuffer, Engine, EngineHandle, Graph, Inputs, Node, NodeId, NoteEvent, NoteId, Notes,
    ParallelOptions, ParallelRenderer, ParamId, Preview, ProcessContext, ReferenceRenderer, Stats,
    TapWriter, TransportCommand,
};

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 1024,
};

/// The key of the tests' writers, and of the record taps they write.
const WRITER: NodeKey = NodeKey(1000);

/// Counts frames: state that has to carry on through a swap.
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

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
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

    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        for (i, sample) in output.channel_mut(0).iter_mut().enumerate() {
            *sample = match ctx.transport.playing {
                true => (ctx.transport.position + i as u64) as f32,
                false => -1.0,
            };
        }
    }
}

/// Multiplies what comes in by a gain that jumps to each new value on the
/// frame it's sent for.
struct Level(f32);

impl Level {
    const GAIN: ParamId = ParamId(0);
}

impl Node for Level {
    fn channels(&self) -> usize {
        1
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
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

/// Plays a note on each frame of engine time that's a multiple of its
/// period, and lets it go on the next, to the nodes its notes go to.
struct Ticks(u64);

impl Node for Ticks {
    fn channels(&self) -> usize {
        1
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        for i in 0..ctx.frames {
            let time = ctx.time + i as u64;
            let note = NoteId(time / self.0);
            let _ = match time % self.0 {
                0 => notes.send(
                    i,
                    NoteEvent::On {
                        note,
                        key: (time % 128) as u8,
                        velocity: 1.0,
                    },
                ),
                1 => notes.send(
                    i,
                    NoteEvent::Off {
                        note,
                        velocity: 0.5,
                    },
                ),
                _ => true,
            };
        }
    }

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
    }
}

/// Keeps what it's written, a frame at a time in two channels, as a
/// recorder would: a mono block in both, and silence as zeros. And the
/// notes, at their engine times. Each block has to follow the one before.
struct Kept {
    next: u64,
    frames: rtrb::Producer<[f32; 2]>,
    notes: rtrb::Producer<(u64, NodeEventKind)>,
}

impl TapWriter for Kept {
    fn write(&mut self, time: u64, audio: AudioRef<'_>) {
        assert_eq!(time, self.next, "a block from where the last ended");
        self.next += audio.frames() as u64;
        for i in 0..audio.frames() {
            let sample = |c: usize| match audio.channels() {
                0 => 0.0,
                channels => audio.channel(c.min(channels - 1))[i],
            };
            assert!(self.frames.push([sample(0), sample(1)]).is_ok());
        }
    }

    fn write_notes(&mut self, time: u64, frames: usize, notes: &[NodeEvent]) {
        assert_eq!(
            time + frames as u64,
            self.next,
            "the notes of the frames written"
        );
        for note in notes {
            assert!(note.offset < frames);
            let at = time + note.offset as u64;
            assert!(self.notes.push((at, note.kind)).is_ok());
        }
    }
}

/// What a [`Kept`] keeps.
struct Keeping {
    frames: rtrb::Consumer<[f32; 2]>,
    notes: rtrb::Consumer<(u64, NodeEventKind)>,
}

impl Keeping {
    fn frames(&mut self) -> Vec<[f32; 2]> {
        std::iter::from_fn(|| self.frames.pop().ok()).collect()
    }

    fn notes(&mut self) -> Vec<(u64, NodeEventKind)> {
        std::iter::from_fn(|| self.notes.pop().ok()).collect()
    }
}

/// A writer whose first block is from engine time `from`, and what it
/// keeps.
fn kept(from: u64) -> (Box<dyn TapWriter>, Keeping) {
    let (frames, frames_rx) = RingBuffer::new(1 << 17);
    let (notes, notes_rx) = RingBuffer::new(1 << 15);
    let writer = Kept {
        next: from,
        frames,
        notes,
    };
    let keeping = Keeping {
        frames: frames_rx,
        notes: notes_rx,
    };
    (Box::new(writer), keeping)
}

/// An engine rendering up to `lead` ahead, with the reference renderer.
fn with_lead(lead: Lead) -> (Engine, EngineHandle) {
    let (engine, handle) = Engine::new(CONFIG);
    handle.set_lead(lead);
    (engine, handle)
}

/// An engine rendering up to `lead` ahead, with two workers.
fn parallel(lead: Lead) -> (Engine, EngineHandle) {
    let renderer = ParallelRenderer::new(workers());
    let (engine, handle) = Engine::with_renderer(CONFIG, Box::new(renderer));
    handle.set_lead(lead);
    (engine, handle)
}

fn workers() -> ParallelOptions {
    ParallelOptions {
        threads: 2,
        spin: Duration::ZERO,
        inline_below: None,
        max_nodes: 64,
        realtime: false,
    }
}

/// `node`, alone, with key 0, as the output.
fn single(node: impl Node) -> PreparedGraph {
    let mut g = Graph::new();
    let id = g.add(NodeKey(0), node);
    g.set_output(id).unwrap();
    g.prepare(&CONFIG).unwrap()
}

/// A constant 1.0 through a [`Level`] with key 1.
fn gain_stage() -> PreparedGraph {
    let mut g = Graph::new();
    let one = g.add(NodeKey(0), Constant::new(1.0).with_channels(1));
    let level = g.add(NodeKey(1), Level(1.0));
    g.connect(one, level).unwrap();
    g.set_output(level).unwrap();
    g.prepare(&CONFIG).unwrap()
}

fn gain(value: f64) -> Event {
    Event::Param {
        node: NodeKey(1),
        param: Level::GAIN,
        value,
    }
}

/// A [`Counter`] as the output, with a record tap for [`WRITER`], and
/// [`Ticks`] every 100 frames playing into it, with a note record for the
/// writer too.
fn recorded() -> PreparedGraph {
    recorded_every(100)
}

/// The same, with a note every `period` frames.
fn recorded_every(period: u64) -> PreparedGraph {
    let mut g = Graph::new();
    let counter = g.add(NodeKey(0), Counter(0.0));
    let ticks = g.add(NodeKey(1), Ticks(period));
    g.send_notes(ticks, counter).unwrap();
    g.add_record(WRITER, counter).unwrap();
    g.add_note_record(WRITER, ticks).unwrap();
    g.set_output(counter).unwrap();
    g.prepare(&CONFIG).unwrap()
}

/// A track that monitors the first device input, with key 0, as the output:
/// it plays the input while it's armed and the transport stands still.
fn track() -> PreparedGraph {
    single(InputMonitor::new(Monitor::Auto, false).with_input(0, 1))
}

fn armed(armed: bool) -> Event {
    Event::Param {
        node: NodeKey(0),
        param: InputMonitor::ARMED,
        value: f64::from(u8::from(armed)),
    }
}

fn play() -> Event {
    Event::Transport(TransportCommand::Play)
}

/// The first device input on engine time `t`, which is never silent.
fn input(t: u64) -> f32 {
    ((t % 251) + 1) as f32 / 256.0
}

/// Plays `frames` frames in device callbacks of `size`, and returns the
/// first channel, with [`input`] as the device inputs.
fn render(engine: &mut Engine, frames: usize, size: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    let mut device = AudioBuffer::new(1, size);
    let mut done = 0;
    while done < frames {
        let n = size.min(frames - done);
        let time = engine.time();
        for (i, x) in device.view_mut(n).channel_mut(0).iter_mut().enumerate() {
            *x = input(time + i as u64);
        }
        let mut output = AudioMut::from_planar(&mut out[done..done + n], 1);
        engine.process_with_input(device.view(n), &[], &mut output);
        done += n;
    }
    out
}

/// Waits up to a few seconds for `done`, and says whether it came.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        if Instant::now() > until {
            return false;
        }
        thread::yield_now();
    }
    true
}

#[test]
fn leads_are_whole_frames() {
    assert_eq!(
        Lead::ALL.map(|lead| lead.frames(48_000)),
        [0, 960, 2400, 9600]
    );
    assert_eq!(Lead::Normal.frames(44_100), 2205);
    assert_eq!(Lead::Short.frames(22_050), 441);
    assert_eq!(Lead::Long.frames(1001), 201, "rounded up");
}

#[test]
fn the_lead_fills_a_block_while_stopped_and_all_of_it_while_playing() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle.swap_graph(single(Counter(0.0))).unwrap();
    let mut out = render(&mut engine, 256, 256);
    let stats = handle.stats();
    assert_eq!((stats.lead_target, stats.lead), (512, 512));
    handle.send(play()).unwrap();
    out.extend(render(&mut engine, 256 * 20, 256));
    let stats = handle.stats();
    assert_eq!(stats.lead_target, 2400);
    let lead = 2400..2400 + AHEAD_BLOCK as u64;
    assert!(lead.contains(&stats.lead), "{stats:?}");
    assert_eq!(stats.short_blocks, 0);
    // It played what it rendered ahead as it would have played it live.
    assert!(out.iter().enumerate().all(|(i, &s)| s == i as f32));
}

#[test]
fn a_callback_of_no_frames_takes_in_what_was_sent() {
    for lead in [Lead::Off, Lead::Normal] {
        let (mut engine, mut handle) = with_lead(lead);
        handle.swap_graph(single(Latent(Delay::new(100)))).unwrap();
        let locate = TransportCommand::Locate(Beats::new(1));
        handle.send(Event::Transport(locate)).unwrap();
        engine.process(&mut AudioMut::from_planar(&mut [], 1));
        let stats = handle.stats();
        // A beat at 120 BPM.
        let expected = (0, 1, 100, 24_000);
        let got = (stats.time, stats.swaps, stats.latency, stats.position);
        assert_eq!(got, expected, "{lead:?}");
    }
}

#[test]
fn events_sent_for_now_apply_at_the_furthest_frame_rendered() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle.swap_graph(gain_stage()).unwrap();
    handle.send(play()).unwrap();
    let mut out = render(&mut engine, 256 * 12, 256);
    let stats = handle.stats();
    assert!(stats.lead >= 2400);
    let furthest = (stats.time + stats.lead) as usize;
    handle.send(gain(0.5)).unwrap();
    out.extend(render(&mut engine, 256 * 20, 256));
    assert!(out[..furthest].iter().all(|&s| s == 1.0));
    assert!(out[furthest..].iter().all(|&s| s == 0.5));
    assert_eq!(handle.stats().events_late, 0);
}

#[test]
fn transport_commands_sent_for_now_apply_at_the_furthest_frame_too() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle.swap_graph(single(Playhead)).unwrap();
    handle.send(play()).unwrap();
    let mut out = render(&mut engine, 256 * 12, 256);
    let stats = handle.stats();
    let furthest = stats.time + stats.lead;
    let locate = TransportCommand::Locate(Beats::new(4));
    handle.send(Event::Transport(locate)).unwrap();
    out.extend(render(&mut engine, 256, 256));
    // The frame playing hasn't got there yet.
    let stats = handle.stats();
    assert!(stats.time < furthest);
    assert_eq!(stats.position, stats.time);
    out.extend(render(&mut engine, 256 * 20, 256));
    // 120 BPM at 48 kHz.
    let beat_4 = 4 * 24_000;
    for (i, &s) in out.iter().enumerate() {
        let t = i as u64;
        let position = if t < furthest {
            t
        } else {
            beat_4 + t - furthest
        };
        assert_eq!(s, position as f32, "frame {i}");
    }
    let stats = handle.stats();
    assert_eq!(stats.position, beat_4 + stats.time - furthest);
}

#[test]
fn graphs_swap_in_at_the_furthest_frame() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    let constant = |value| single(Constant::new(value).with_channels(1));
    handle.swap_graph(constant(1.0)).unwrap();
    handle.send(play()).unwrap();
    let mut out = render(&mut engine, 256 * 12, 256);
    let stats = handle.stats();
    let furthest = (stats.time + stats.lead) as usize;
    handle.swap_graph(constant(2.0)).unwrap();
    out.extend(render(&mut engine, 256 * 20, 256));
    assert!(out[..furthest].iter().all(|&s| s == 1.0));
    assert!(out[furthest..].iter().all(|&s| s == 2.0));
    assert_eq!(handle.stats().swaps, 2);
    assert_eq!(handle.collect_garbage(), 1);
}

#[test]
fn events_sent_for_a_time_land_on_it_unless_it_was_rendered_already() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle.swap_graph(gain_stage()).unwrap();
    handle.send(play()).unwrap();
    let mut out = render(&mut engine, 256 * 12, 256);
    let stats = handle.stats();
    let furthest = stats.time + stats.lead;
    // One beyond the furthest frame lands on its frame. One before it was
    // rendered already, so it applies there, and is late.
    handle.send_at(furthest + 1000, gain(0.25)).unwrap();
    handle.send_at(stats.time + 10, gain(0.5)).unwrap();
    out.extend(render(&mut engine, 256 * 30, 256));
    let (furthest, at) = (furthest as usize, furthest as usize + 1000);
    assert!(out[..furthest].iter().all(|&s| s == 1.0));
    assert!(out[furthest..at].iter().all(|&s| s == 0.5));
    assert!(out[at..].iter().all(|&s| s == 0.25));
    assert_eq!(handle.stats().events_late, 1);
}

#[test]
fn arming_a_track_renders_it_live_from_the_frame_it_arms_until_its_input_fades_out() {
    // One engine with a lead, the other without, each played the same.
    let mut engines = [Lead::Normal, Lead::Off].map(|lead| {
        let (engine, mut handle) = with_lead(lead);
        handle.swap_graph(track()).unwrap();
        (engine, handle, Vec::new())
    });
    let render_both = |engines: &mut [(Engine, EngineHandle, Vec<f32>)], frames| {
        for (engine, _, out) in engines {
            out.extend(render(engine, frames, 256));
        }
    };
    render_both(&mut engines, 256 * 4);
    let stats = engines[0].1.stats();
    assert_eq!((stats.lead_target, stats.lead), (512, 512));
    // Beyond what it has rendered.
    let armed_at = stats.time + stats.lead + 100;
    for (_, handle, _) in &mut engines {
        handle.send_at(armed_at, armed(true)).unwrap();
    }
    render_both(&mut engines, 256 * 8);
    let stats = engines[0].1.stats();
    assert_eq!((stats.lead_target, stats.lead), (0, 0));
    // Disarmed, it renders live until the input has faded out.
    let disarmed_at = stats.time + 100;
    for (_, handle, _) in &mut engines {
        handle.send_at(disarmed_at, armed(false)).unwrap();
    }
    render_both(&mut engines, 256 * 2);
    assert_eq!(engines[0].1.stats().lead_target, 0, "fading out");
    render_both(&mut engines, 256 * 8);
    let stats = engines[0].1.stats();
    assert_eq!((stats.lead_target, stats.lead), (512, 512));
    assert_eq!(stats.events_late, 0);
    let [(_, _, out), (_, _, expected)] = &engines;
    assert_eq!(out, expected);
    // Silent until it arms, then the input, as it fades in and out.
    let (armed_at, disarmed_at) = (armed_at as usize, disarmed_at as usize);
    let fade = 480;
    assert!(out[..armed_at].iter().all(|&s| s == 0.0));
    for (t, &s) in out
        .iter()
        .enumerate()
        .take(disarmed_at)
        .skip(armed_at + fade)
    {
        assert_eq!(s, input(t as u64), "frame {t}");
    }
    assert!(out[disarmed_at + fade..].iter().all(|&s| s == 0.0));
}

#[test]
fn blocks_rendered_where_the_lead_ran_out_count_as_short() {
    // Callbacks longer than the lead use it up, every one after the first.
    let (mut engine, mut handle) = with_lead(Lead::Short);
    handle.swap_graph(single(Counter(0.0))).unwrap();
    handle.send(play()).unwrap();
    let out = render(&mut engine, 2048 * 5, 2048);
    assert!(out.iter().enumerate().all(|(i, &s)| s == i as f32));
    assert_eq!(handle.stats().short_blocks, 4);
    // Shorter ones don't.
    let (mut engine, mut handle) = with_lead(Lead::Short);
    handle.swap_graph(single(Counter(0.0))).unwrap();
    handle.send(play()).unwrap();
    render(&mut engine, 256 * 40, 256);
    assert_eq!(handle.stats().short_blocks, 0);
}

#[test]
fn record_taps_reach_their_writers_as_they_play() {
    let sizes = [7, 256, 1000, 64, 2048, 333];
    let [(frames, notes), expected] = [Lead::Normal, Lead::Off].map(|lead| {
        let (mut engine, mut handle) = with_lead(lead);
        let (writer, mut keeping) = kept(0);
        assert!(handle.add_writer(WRITER, writer).is_ok());
        handle.swap_graph(recorded()).unwrap();
        handle.send(play()).unwrap();
        for &size in sizes.iter().cycle().take(30) {
            render(&mut engine, size, size);
            handle.collect_garbage();
        }
        (keeping.frames(), keeping.notes())
    });
    assert_eq!((&frames, &notes), (&expected.0, &expected.1));
    // The counter, in both channels, from the graph's first frame, and a
    // note on every 100th.
    assert!(frames.iter().enumerate().all(|(i, &f)| f == [i as f32; 2]));
    assert_eq!(notes.len(), 2 * frames.len().div_ceil(100));
    let on = NoteEvent::On {
        note: NoteId(1),
        key: 100,
        velocity: 1.0,
    };
    assert_eq!(notes[2], (100, NodeEventKind::Note(on)));
}

#[test]
fn notes_too_many_for_a_writers_queue_hold_the_lead_back_rather_than_drop() {
    let (mut engine, mut handle) = with_lead(Lead::Long);
    let (writer, mut keeping) = kept(0);
    assert!(handle.add_writer(WRITER, writer).is_ok());
    // A note on and off every 4 frames: more in a long lead than the queue
    // holds.
    handle.swap_graph(recorded_every(4)).unwrap();
    handle.send(play()).unwrap();
    let mut notes = Vec::new();
    for _ in 0..40 {
        render(&mut engine, 256, 256);
        handle.collect_garbage();
        notes.extend(keeping.notes());
    }
    let stats = handle.stats();
    assert_eq!(stats.events_dropped, 0);
    assert!(stats.lead < Lead::Long.frames(48_000) / 2, "{stats:?}");
    assert_eq!(notes.len(), 2 * 40 * 256 / 4);
    for (k, pair) in notes.chunks(2).enumerate() {
        let (at, note) = (4 * k as u64, NoteId(k as u64));
        let on = NoteEvent::On {
            note,
            key: (at % 128) as u8,
            velocity: 1.0,
        };
        let off = NoteEvent::Off {
            note,
            velocity: 0.5,
        };
        let expected = [
            (at, NodeEventKind::Note(on)),
            (at + 1, NodeEventKind::Note(off)),
        ];
        assert_eq!(pair, expected);
    }
}

#[test]
fn a_writer_added_while_rendering_ahead_gets_silence_for_what_was_rendered_already() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle.swap_graph(recorded()).unwrap();
    handle.send(play()).unwrap();
    render(&mut engine, 256 * 12, 256);
    let stats = handle.stats();
    let furthest = stats.time + stats.lead;
    let (writer, mut keeping) = kept(stats.time);
    assert!(handle.add_writer(WRITER, writer).is_ok());
    render(&mut engine, 256 * 20, 256);
    let frames = keeping.frames();
    assert_eq!(frames.len(), 256 * 20);
    let silent = (furthest - stats.time) as usize;
    assert!(frames[..silent].iter().all(|&f| f == [0.0; 2]));
    for (i, &f) in frames.iter().enumerate().skip(silent) {
        assert_eq!(f, [(stats.time as usize + i) as f32; 2]);
    }
    let first = keeping.notes()[0].0;
    assert_eq!(first, furthest.next_multiple_of(100));
}

#[test]
fn a_writer_that_takes_over_carries_on_where_the_last_left_off() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    let (first, mut first_kept) = kept(0);
    assert!(handle.add_writer(WRITER, first).is_ok());
    handle.swap_graph(recorded()).unwrap();
    handle.send(play()).unwrap();
    render(&mut engine, 256 * 12, 256);
    let time = handle.stats().time;
    let (second, mut second_kept) = kept(time);
    assert!(handle.add_writer(WRITER, second).is_ok());
    render(&mut engine, 256 * 12, 256);
    let mut frames = first_kept.frames();
    assert_eq!(frames.len() as u64, time);
    frames.extend(second_kept.frames());
    assert!(frames.iter().enumerate().all(|(i, &f)| f == [i as f32; 2]));
    assert_eq!(handle.collect_garbage(), 1, "the first writer");
}

#[test]
fn previews_play_from_the_next_callback_whatever_the_lead() {
    let samples: Vec<f32> = (1..=3000).map(|i| i as f32 / 4096.0).collect();
    let [out, expected] = [Lead::Normal, Lead::Off].map(|lead| {
        let (mut engine, mut handle) = with_lead(lead);
        handle.swap_graph(single(Counter(0.0))).unwrap();
        handle.send(play()).unwrap();
        let mut out = render(&mut engine, 256 * 12, 256);
        let source = Source::Whole(Arc::new(AudioData::new(48_000, vec![samples.clone()])));
        handle.preview(Preview::new(1, source, CONFIG)).unwrap();
        out.extend(render(&mut engine, 256 * 16, 256));
        out
    });
    assert_eq!(out, expected);
    let at = 256 * 12;
    assert_eq!(out[at - 1], (at - 1) as f32);
    assert_eq!(out[at], at as f32 + samples[0]);
}

#[test]
fn outputs_with_more_channels_than_two_render_live() {
    let (mut engine, mut handle) = with_lead(Lead::Normal);
    handle
        .swap_graph(single(Constant::new(1.0).with_channels(3)))
        .unwrap();
    render(&mut engine, 256 * 4, 256);
    let stats = handle.stats();
    assert_eq!((stats.lead_target, stats.lead), (0, 0));
    // And cue outputs.
    let mut g = Graph::new();
    let output = g.add(NodeKey(0), Constant::new(1.0));
    let cue = g.add(NodeKey(1), Constant::new(1.0).with_channels(3));
    g.set_output(output).unwrap();
    g.set_cue(cue).unwrap();
    handle.swap_graph(g.prepare(&CONFIG).unwrap()).unwrap();
    render(&mut engine, 256 * 4, 256);
    assert_eq!(handle.stats().lead_target, 0);
    handle.swap_graph(single(Constant::new(1.0))).unwrap();
    render(&mut engine, 256 * 4, 256);
    let stats = handle.stats();
    assert_eq!((stats.lead_target, stats.lead), (512, 512));
}

#[test]
fn a_loop_too_short_for_the_queues_renders_on_as_the_device_asks() {
    // About 8 frames, at 120 BPM: more turns than a long lead can queue.
    let short = BeatRange::new(Beats::ZERO, Beats::from_ticks(108));
    let [out, expected] = [Lead::Long, Lead::Off].map(|lead| {
        let (mut engine, mut handle) = with_lead(lead);
        handle.swap_graph(single(Playhead)).unwrap();
        let looping = TransportCommand::Loop(Some(short));
        handle.send(Event::Transport(looping)).unwrap();
        handle.send(play()).unwrap();
        render(&mut engine, 256 * 60, 256)
    });
    assert_eq!(out, expected);
    assert!(out.iter().all(|&s| (0.0..9.0).contains(&s)));
}

#[test]
fn workers_render_ahead_between_callbacks() {
    let (mut engine, mut handle) = parallel(Lead::Normal);
    handle.swap_graph(single(Counter(0.0))).unwrap();
    handle.send(play()).unwrap();
    let mut out = render(&mut engine, 256, 256);
    // With no callback since, and between each callback and the next, as a
    // device paces them.
    for _ in 0..40 {
        assert!(eventually(|| engine.shared().lead() >= 2400));
        out.extend(render(&mut engine, 256, 256));
    }
    assert!(out.iter().enumerate().all(|(i, &s)| s == i as f32));
    assert_eq!(handle.stats().short_blocks, 0);
}

/// Panics in the first block it renders from engine time 1024 on.
#[derive(Default)]
struct PanicsLater {
    panicked: bool,
}

impl Node for PanicsLater {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        if ctx.time >= 1024 && !self.panicked {
            self.panicked = true;
            rt::allowed(|| panic!("a node failed"));
        }
        output.clear();
    }
}

#[test]
fn a_panic_rendering_ahead_reaches_the_audio_thread() {
    let (mut engine, mut handle) = parallel(Lead::Normal);
    handle.swap_graph(single(PanicsLater::default())).unwrap();
    handle.send(play()).unwrap();
    render(&mut engine, 256, 256);
    // A worker renders on to frame 1024, and fails there.
    assert!(eventually(|| engine.shared().failure.is_set()));
    let failed = panic::catch_unwind(AssertUnwindSafe(|| render(&mut engine, 256, 256)));
    let payload = failed.expect_err("the callback should have failed");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"a node failed"));
}

#[test]
fn dropping_the_engine_waits_for_its_workers_to_stop_rendering_ahead() {
    for _ in 0..20 {
        let (mut engine, mut handle) = parallel(Lead::Long);
        let spin = Spin {
            render: Duration::from_micros(20),
            ..Spin::default()
        };
        handle.swap_graph(single(spin)).unwrap();
        handle.send(play()).unwrap();
        render(&mut engine, 64, 64);
        drop(engine);
        handle.collect_garbage();
    }
}

#[test]
fn a_thread_that_fails_to_take_the_core_leaves_it_held() {
    let (engine, _handle) = with_lead(Lead::Normal);
    let shared = engine.shared();
    let held = shared.try_lock().expect("the core, which no thread holds");
    assert!(shared.try_lock().is_none());
    // The attempt that failed let nothing go.
    assert!(shared.try_lock().is_none());
    drop(held);
    assert!(shared.try_lock().is_some());
}

/// What a random graph's node is.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Probe {
        seed: u8,
        channels: usize,
    },
    /// So delay compensation delays the paths that meet its path.
    Latent {
        frames: usize,
        channels: usize,
    },
    /// A track monitoring the device inputs: a live source while it can
    /// play them.
    Monitor {
        monitor: Monitor,
        armed: bool,
        first: usize,
        channels: usize,
    },
}

impl Kind {
    fn node(self) -> Box<dyn Node> {
        match self {
            Kind::Probe { seed, channels } => Box::new(Probe::new(seed, channels)),
            Kind::Latent { frames, channels } => {
                Box::new(Latent(Delay::new(frames).with_channels(channels)))
            }
            Kind::Monitor {
                monitor,
                armed,
                first,
                channels,
            } => Box::new(InputMonitor::new(monitor, armed).with_input(first, channels)),
        }
    }
}

/// A node whose output depends on everything a block gives it but the
/// device inputs: its inputs, its events, the block's time, length and
/// transport, the tempo, and its own history, which it takes over from the
/// probe it replaces. A difference in any of them carries on through its
/// output. It only uses arithmetic that IEEE 754 rounds exactly.
struct Probe {
    channels: usize,
    seed: f32,
    state: f32,
}

impl Probe {
    fn new(seed: u8, channels: usize) -> Self {
        Self {
            channels,
            seed: f32::from(seed) / 64.0,
            state: 0.0,
        }
    }
}

impl Node for Probe {
    fn channels(&self) -> usize {
        self.channels
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Probe>() {
            self.state = previous.state;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        let transport = ctx.transport;
        let flags = [
            transport.playing,
            transport.record,
            transport.recording,
            transport.looping.is_some(),
        ];
        let flags = (flags.iter().enumerate()).fold(0, |sum, (i, &on)| sum + (u8::from(on) << i));
        let block = self.seed
            + (ctx.time % 1000) as f32 * 1e-3
            + ctx.frames as f32 * 1e-2
            + (transport.position % 1000) as f32 * 1e-4
            + f32::from(flags)
            + (transport.jumps % 16) as f32 * 0.125
            + ctx.tempo.bpm_at(Beats::ZERO) as f32 * 1e-3
            + ctx.tempo.tempos().len() as f32 * 0.25;
        let mut events = ctx.events.iter().peekable();
        for frame in 0..ctx.frames {
            while let Some(event) = events.next_if(|e| e.offset == frame) {
                if let NodeEventKind::Param { param, value } = event.kind {
                    self.state += (param.0 as f32 + 1.0) * value as f32;
                }
            }
            for c in 0..self.channels {
                let mut acc = self.state + block;
                for (i, input) in inputs.iter().enumerate() {
                    let x = input.channel(c % input.channels())[frame];
                    acc = acc * 0.5 + x * (i as f32 + 1.5);
                }
                let y = acc / (1.0 + acc.abs());
                output.channel_mut(c)[frame] = y;
                self.state = 0.9 * self.state + 0.1 * y;
            }
        }
    }
}

/// A random graph. Each node is fed by earlier ones, and keyed by its
/// place, so the node in the same place in the next graph takes over from
/// it.
#[derive(Debug, Clone)]
struct Spec {
    nodes: Vec<(Kind, Vec<usize>)>,
    output: usize,
    cue: Option<usize>,
    /// The node the writer's record tap taps, if there's one.
    record: Option<usize>,
    /// How often a [`Ticks`] node plays into the first node, with a note
    /// record for the writer, if there's one.
    ticks: Option<u64>,
}

/// The key of a random graph's [`Ticks`].
const TICKS: NodeKey = NodeKey(100);

impl Spec {
    fn build(&self, config: EngineConfig) -> PreparedGraph {
        let mut g = Graph::new();
        let ids: Vec<NodeId> = (self.nodes.iter().enumerate())
            .map(|(i, (kind, _))| g.add_boxed(NodeKey(i as u64), kind.node()))
            .collect();
        for (i, (_, inputs)) in self.nodes.iter().enumerate() {
            for &input in inputs {
                g.connect(ids[input], ids[i]).unwrap();
            }
        }
        g.set_output(ids[self.output]).unwrap();
        if let Some(cue) = self.cue {
            g.set_cue(ids[cue]).unwrap();
        }
        if let Some(record) = self.record {
            g.add_record(WRITER, ids[record]).unwrap();
        }
        if let Some(period) = self.ticks {
            let ticks = g.add(TICKS, Ticks(period));
            g.send_notes(ticks, ids[0]).unwrap();
            g.add_note_record(WRITER, ticks).unwrap();
        }
        g.prepare(&config).unwrap()
    }
}

/// What happens next in a random run.
#[derive(Debug, Clone)]
enum Step {
    /// A device callback this long, into a planar buffer or an interleaved
    /// one.
    Callback {
        frames: usize,
        planar: bool,
    },
    Send(Event),
    /// An event for this many frames after the engine's time.
    SendAt(u64, Event),
    Swap(Spec),
    Tempo(TempoMap),
    Lead(Lead),
    /// Waits a little while for the workers to fill the lead.
    Wait,
}

/// A random run: an engine's sample rate, block size, lead and device
/// channels, the graph it starts with, and what happens.
#[derive(Debug, Clone)]
struct Run {
    sample_rate: u32,
    max_block: usize,
    lead: Lead,
    channels: usize,
    graph: Spec,
    steps: Vec<Step>,
}

impl Run {
    fn config(&self) -> EngineConfig {
        EngineConfig {
            sample_rate: self.sample_rate,
            max_block: self.max_block,
        }
    }
}

fn kind() -> impl Strategy<Value = Kind> {
    let channels = prop_oneof![3 => Just(1usize), 3 => Just(2), 1 => Just(3)];
    let monitor = prop_oneof![
        1 => Just(Monitor::In),
        2 => Just(Monitor::Auto),
        2 => Just(Monitor::Off),
    ];
    prop_oneof![
        8 => (any::<u8>(), channels).prop_map(|(seed, channels)| Kind::Probe { seed, channels }),
        1 => (0..40usize, 1..=2usize)
            .prop_map(|(frames, channels)| Kind::Latent { frames, channels }),
        2 => (monitor, prop::bool::weighted(0.25), 0..2usize, 1..=2usize).prop_map(
            |(monitor, armed, first, channels)| Kind::Monitor {
                monitor,
                armed,
                first,
                channels,
            }
        ),
    ]
}

fn spec() -> impl Strategy<Value = Spec> {
    (1..=6usize)
        .prop_flat_map(|n| {
            let inputs = prop::collection::vec(any::<Index>(), 0..=3);
            (
                prop::collection::vec((kind(), inputs), n),
                any::<Index>(),
                prop::option::of(any::<Index>()),
                prop::option::of(any::<Index>()),
                prop::option::of(16..=64u64),
            )
        })
        .prop_map(|(nodes, output, cue, record, ticks)| {
            let n = nodes.len();
            let nodes = (nodes.into_iter().enumerate())
                .map(|(i, (kind, inputs))| {
                    let inputs = match i {
                        0 => Vec::new(),
                        _ => inputs.iter().map(|input| input.index(i)).collect(),
                    };
                    (kind, inputs)
                })
                .collect();
            Spec {
                nodes,
                output: output.index(n),
                cue: cue.map(|cue| cue.index(n)),
                record: record.map(|record| record.index(n)),
                ticks,
            }
        })
}

/// Sixteenth notes, up to four beats in.
fn beats() -> impl Strategy<Value = Beats> {
    (0i64..64).prop_map(|sixteenths| Beats::from_ratio(sixteenths, 16))
}

fn command() -> impl Strategy<Value = TransportCommand> {
    let looped = (beats(), 1i64..32).prop_map(|(start, length)| {
        let end = start + Beats::from_ratio(length, 16);
        TransportCommand::Loop(Some(BeatRange::new(start, end)))
    });
    let punch = (prop::option::of(beats()), prop::option::of(beats()))
        .prop_map(|(start, end)| TransportCommand::Punch { start, end });
    prop_oneof![
        3 => Just(TransportCommand::Play),
        2 => Just(TransportCommand::Stop),
        2 => beats().prop_map(TransportCommand::Locate),
        2 => looped,
        1 => Just(TransportCommand::Loop(None)),
        1 => any::<bool>().prop_map(TransportCommand::Record),
        1 => punch,
    ]
}

/// Parameters for the first nodes, some of which a graph won't have, and
/// transport commands.
fn event() -> impl Strategy<Value = Event> {
    let value = prop::sample::select(vec![0.0, 0.5, 1.0, 2.0, -0.75]);
    prop_oneof![
        3 => (0..8u64, 0..3u64, value).prop_map(|(node, param, value)| Event::Param {
            node: NodeKey(node),
            param: ParamId(param),
            value,
        }),
        2 => command().prop_map(Event::Transport),
    ]
}

fn tempo() -> impl Strategy<Value = TempoMap> {
    let change = prop::option::of((1i64..8, 60.0f64..600.0));
    (60.0f64..600.0, any::<bool>(), change).prop_map(|(bpm, waltz, change)| {
        let signature = match waltz {
            true => TimeSignature::new(3, 4).unwrap(),
            false => TimeSignature::FOUR_FOUR,
        };
        let mut tempo = TempoMap::new(bpm, signature);
        if let Some((at, bpm)) = change {
            tempo.insert_tempo(Beats::new(at), bpm);
        }
        tempo
    })
}

fn step() -> impl Strategy<Value = Step> {
    let frames = prop_oneof![2 => 1..=16usize, 3 => 17..=200usize, 2 => 201..=600usize];
    prop_oneof![
        12 => (frames, any::<bool>())
            .prop_map(|(frames, planar)| Step::Callback { frames, planar }),
        3 => event().prop_map(Step::Send),
        3 => (0..2000u64, event()).prop_map(|(after, event)| Step::SendAt(after, event)),
        1 => spec().prop_map(Step::Swap),
        1 => tempo().prop_map(Step::Tempo),
        1 => prop::sample::select(Lead::ALL.to_vec()).prop_map(Step::Lead),
        2 => Just(Step::Wait),
    ]
}

fn run() -> impl Strategy<Value = Run> {
    (
        // Low rates, so the leads are short and a run soon fills them, and
        // one where the leads are long enough for the longest blocks.
        prop::sample::select(vec![8000, 8000, 48_000]),
        prop::sample::select(vec![32usize, 64, 128, 512, 1024, 1024]),
        prop::sample::select(vec![Lead::Short, Lead::Normal, Lead::Long]),
        1..=3usize,
        spec(),
        // Half the runs start playing, since a lead fills only then.
        any::<bool>(),
        prop::collection::vec(step(), 1..=60),
    )
        .prop_map(
            |(sample_rate, max_block, lead, channels, graph, play, mut steps)| {
                if play {
                    let play = Event::Transport(TransportCommand::Play);
                    steps.insert(0, Step::Send(play));
                }
                Run {
                    sample_rate,
                    max_block,
                    lead,
                    channels,
                    graph,
                    steps,
                }
            },
        )
}

/// The device inputs' channel `c` on engine time `t`.
fn device_input(t: u64, c: usize) -> f32 {
    ((t * (c as u64 + 3)) % 101) as f32 / 128.0 - 0.375
}

/// Plays a device callback of `frames` frames with `channels` channels on
/// `engine`, into a planar buffer or an interleaved one, with two channels
/// of [`device_input`] from the engine's time on, and puts what it played
/// on the end of `output`, interleaved.
fn callback(
    engine: &mut Engine,
    frames: usize,
    channels: usize,
    planar: bool,
    output: &mut Vec<f32>,
) {
    let mut input = AudioBuffer::new(2, frames);
    let time = engine.time();
    for c in 0..2 {
        let mut view = input.view_mut(frames);
        for (i, x) in view.channel_mut(c).iter_mut().enumerate() {
            *x = device_input(time + i as u64, c);
        }
    }
    let start = output.len();
    output.resize(start + frames * channels, 0.0);
    let out = &mut output[start..];
    match planar {
        true => {
            let mut buffer = AudioBuffer::new(channels, frames);
            engine.process_with_input(input.view(frames), &[], &mut buffer.view_mut(frames));
            buffer.view(frames).write_interleaved(out, channels);
        }
        false => engine.process_interleaved_with_input(input.view(frames), &[], out, channels),
    }
}

/// Waits a little while for the workers to fill the lead.
fn wait(engine: &Engine) {
    let shared = engine.shared();
    let until = Instant::now() + Duration::from_millis(20);
    while Instant::now() < until {
        let target = shared.target.load(Relaxed);
        if target == 0 || shared.lead() >= target {
            return;
        }
        thread::yield_now();
    }
}

/// What an engine played: its output, interleaved, what its writer kept, and
/// its timeline.
#[derive(Debug, Default)]
struct Played {
    output: Vec<f32>,
    frames: Vec<[f32; 2]>,
    notes: Vec<(u64, NodeEventKind)>,
    segments: Vec<Segment>,
}

/// What an engine rendering ahead did in a run.
struct Ahead {
    played: Played,
    applied: Vec<(u64, Applied)>,
    /// Its stats, as of its last block.
    stats: Stats,
    /// The graphs and the tempo maps it was sent, in order.
    graphs: Vec<Spec>,
    tempos: Vec<TempoMap>,
}

/// Plays `run` on an engine with its lead and `renderer`, and `workers` of
/// its own.
fn ahead(run: &Run, renderer: Box<dyn Renderer>, workers: bool) -> Ahead {
    let config = run.config();
    let (mut engine, mut handle) = Engine::with_renderer(config, renderer);
    engine.with_core(|core, _| core.keep_log());
    let mut timeline = handle.take_timeline().unwrap();
    let (writer, mut keeping) = kept(0);
    assert!(handle.add_writer(WRITER, writer).is_ok());
    handle.set_lead(run.lead);
    handle.swap_graph(run.graph.build(config)).unwrap();
    let mut graphs = vec![run.graph.clone()];
    let mut tempos = Vec::new();
    let mut played = Played::default();
    for step in &run.steps {
        match step {
            &Step::Callback { frames, planar } => {
                callback(
                    &mut engine,
                    frames,
                    run.channels,
                    planar,
                    &mut played.output,
                );
                handle.collect_garbage();
                timeline.read(&mut played.segments);
            }
            &Step::Send(event) => {
                let _ = handle.send(event);
            }
            &Step::SendAt(after, event) => {
                let _ = handle.send_at(engine.time() + after, event);
            }
            Step::Swap(spec) => {
                if handle.swap_graph(spec.build(config)).is_ok() {
                    graphs.push(spec.clone());
                }
            }
            Step::Tempo(tempo) => {
                if handle.set_tempo_map(tempo.clone()).is_ok() {
                    tempos.push(tempo.clone());
                }
            }
            &Step::Lead(lead) => handle.set_lead(lead),
            Step::Wait if workers => wait(&engine),
            Step::Wait => {}
        }
    }
    // With no thread rendering it, for a log that ends where its counters
    // and its timeline do.
    let (applied, stats) = engine.with_core(|core, _| {
        timeline.read(&mut played.segments);
        (core.applied.clone().expect("a log"), handle.stats())
    });
    played.frames = keeping.frames();
    played.notes = keeping.notes();
    Ahead {
        played,
        applied,
        stats,
        graphs,
        tempos,
    }
}

/// What an engine without a lead did, replaying another's run.
struct Replayed {
    played: Played,
    applied: Vec<(u64, Applied)>,
    stats: Stats,
}

/// Has an engine without a lead apply what `ahead` applied, at the times it
/// applied it, and render each block it rendered, as a device callback of
/// its own.
fn replay(run: &Run, ahead: &Ahead) -> Result<Replayed, TestCaseError> {
    let config = run.config();
    let (mut engine, mut handle) = Engine::new(config);
    engine.with_core(|core, _| core.keep_log());
    let mut timeline = handle.take_timeline().unwrap();
    let (writer, mut keeping) = kept(0);
    assert!(handle.add_writer(WRITER, writer).is_ok());
    let mut graphs = ahead.graphs.iter();
    let mut tempos = ahead.tempos.iter();
    let mut played = Played::default();
    for &(time, applied) in &ahead.applied {
        prop_assert_eq!(engine.time(), time);
        match applied {
            Applied::Event { time, event } => {
                handle.send_message(CoreMessage::Event { time, event });
            }
            Applied::Resend { time, event } => {
                handle.send_message(CoreMessage::Resend { time, event });
            }
            Applied::Swap => {
                let graph = graphs.next().expect("a graph for each swap");
                handle.send_message(CoreMessage::Swap(Box::new(graph.build(config))));
            }
            Applied::Tempo => {
                let tempo = tempos.next().expect("a tempo map for each");
                handle.send_message(CoreMessage::Tempo(Box::new(tempo.clone())));
            }
            Applied::Block(frames) => {
                callback(&mut engine, frames, run.channels, false, &mut played.output);
            }
            Applied::Held => {
                // As the other did, it takes what was sent and applies the
                // transport changes due, and renders nothing: it can't
                // render ahead without a lead.
                let held = engine.with_core(|core, shared| core.render(shared, Drive::Ahead));
                prop_assert_eq!(held, None);
            }
        }
        handle.collect_garbage();
        timeline.read(&mut played.segments);
    }
    let applied = engine.with_core(|core, _| core.applied.clone().expect("a log"));
    played.frames = keeping.frames();
    played.notes = keeping.notes();
    Ok(Replayed {
        played,
        applied,
        stats: handle.stats(),
    })
}

/// Where `a` first differs from the start of `b`, bit for bit.
fn difference(a: &[f32], b: &[f32]) -> Option<usize> {
    let b = &b[..a.len().min(b.len())];
    (a.iter().zip(b))
        .position(|(x, y)| x.to_bits() != y.to_bits())
        .or((b.len() < a.len()).then_some(b.len()))
}

/// Plays `run` with a lead and replays it without, and checks they played
/// the same.
fn check(run: &Run, renderer: Box<dyn Renderer>, workers: bool) -> Result<(), TestCaseError> {
    let ahead = ahead(run, renderer, workers);
    let Replayed {
        played: reference,
        applied,
        stats,
    } = replay(run, &ahead)?;
    prop_assert!(
        applied == ahead.applied,
        "the replay applied something else"
    );
    let played = &ahead.played;
    let frames = played.output.len() / run.channels;
    prop_assert_eq!(ahead.stats.time, frames as u64);
    if let Some(at) = difference(&played.output, &reference.output) {
        let (frame, channel) = (at / run.channels, at % run.channels);
        let (got, expected) = (played.output[at], reference.output.get(at));
        prop_assert!(
            false,
            "frame {frame} differs in channel {channel}: {got} for {expected:?}"
        );
    }
    let kept = (
        played.frames.as_flattened(),
        reference.frames.as_flattened(),
    );
    if let Some(at) = difference(kept.0, kept.1) {
        prop_assert!(false, "the writer's frame {} differs", at / 2);
    }
    prop_assert_eq!(played.frames.len(), frames);
    let notes: Vec<_> = (reference.notes.iter())
        .filter(|&&(at, _)| at < frames as u64)
        .copied()
        .collect();
    prop_assert_eq!(&played.notes, &notes);
    prop_assert_eq!(&played.segments, &reference.segments);
    // Where the song is, as of the frame it played last.
    if frames > 0 {
        let last = (frames - 1) as u64;
        let segment = (reference.segments.iter())
            .rfind(|s| s.time <= last)
            .expect("a segment for every frame");
        let time = frames as u64;
        let at = (
            ahead.stats.position,
            ahead.stats.playing,
            ahead.stats.latency,
        );
        let expected = (segment.position_at(time), segment.playing, segment.latency);
        prop_assert_eq!(at, expected);
    }
    let counts = |s: &Stats| (s.swaps, s.events_late, s.events_dropped, s.events_unmatched);
    prop_assert_eq!(counts(&ahead.stats), counts(&stats));
    Ok(())
}

/// Cases to try, unless `PROPTEST_CASES` says.
fn cases(cases: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(cases)
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    })
}

#[test]
#[cfg_attr(miri, ignore)]
fn rendering_ahead_plays_what_rendering_live_plays() {
    let result = runner(cases(64)).run(&run(), |run| {
        check(&run, Box::new(ReferenceRenderer), false)
    });
    if let Err(error) = result {
        panic!("{error}");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn rendering_ahead_on_workers_plays_what_rendering_live_plays() {
    let result = runner(cases(24)).run(&run(), |run| {
        let renderer = ParallelRenderer::new(workers());
        check(&run, Box::new(renderer), true)
    });
    if let Err(error) = result {
        panic!("{error}");
    }
}

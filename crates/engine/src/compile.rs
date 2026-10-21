//! Compiling the project model into a graph, off the audio thread.
//!
//! Each track becomes a channel strip feeding the master bus: its test tone,
//! if it has one, then its gain stage, with built-in slots for native stages
//! along the way (see [`Slot`]). Empty slots add no nodes. The metronome
//! feeds the cue bus, which the engine plays alongside the master, not
//! through it. Node keys come from track ids and slots (see [`keys`]), so a
//! track's nodes keep their identity when the project is recompiled.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use musicable_model::{ClipType, Project, Track};

use crate::nodes::{Gain, Metronome, Sine, Sum};
use crate::{EngineConfig, Graph, GraphError, Node, NodeId, NodeKey, PreparedGraph};

/// The keys [`compile`] gives model objects' nodes.
///
/// A track's nodes have its id in the low 32 bits of the key and their role
/// in the 16 bits above: 1 for the gain stage, 2 for the test tone and 3 to 6
/// for the built-in slots. The slot in front of a device also has the
/// device's position in the top 16 bits. Keys with role 0 belong to no track.
pub mod keys {
    use musicable_model::TrackId;

    use super::Slot;
    use crate::NodeKey;

    /// The master bus.
    pub const MASTER: NodeKey = NodeKey(0);

    /// The [`Metronome`](crate::nodes::Metronome).
    pub const METRONOME: NodeKey = NodeKey(1);

    /// The cue bus: the graph's [cue output](crate::Graph::set_cue), which
    /// the metronome feeds.
    pub const CUE: NodeKey = NodeKey(2);

    /// A track's gain stage, a [`Gain`](crate::nodes::Gain).
    pub fn track_gain(track: TrackId) -> NodeKey {
        track_key(1, track)
    }

    /// A track's test tone, a [`Sine`](crate::nodes::Sine).
    pub fn track_tone(track: TrackId) -> NodeKey {
        track_key(2, track)
    }

    /// The native stage in one of a track's built-in slots.
    pub fn track_slot(track: TrackId, slot: Slot) -> NodeKey {
        let role = match slot {
            Slot::PreFader => 3,
            Slot::PostFader => 4,
            Slot::BusFeed => 5,
            Slot::BeforeDevice(device) => 6 | u64::from(device) << 16,
        };
        track_key(role, track)
    }

    fn track_key(role: u64, track: TrackId) -> NodeKey {
        NodeKey(role << 32 | u64::from(track.0))
    }
}

/// The fixed points in a channel strip where a native stage, such as a clip
/// type or a console stage, can sit. They belong to the channel, not to its
/// device list. In signal order:
///
/// ```text
/// source → [before device 0] → device 0 → … → [pre-fader] → fader → [post-fader] → [bus feed] → bus
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// In front of the track's device `n`, counting from 0. Tracks have no
    /// devices yet, so these don't occur in Tier 0.
    BeforeDevice(u16),
    /// After the devices, in front of the fader.
    PreFader,
    /// Right after the fader.
    PostFader,
    /// Where the channel feeds its bus.
    BusFeed,
}

/// How a bus sums its inputs. Every bus, including the master, sums through
/// the kind the compiler picks, so console mode can swap in a modelled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summing {
    /// Plain floating-point addition.
    Plain,
}

impl Summing {
    fn node(self) -> Box<dyn Node> {
        match self {
            Self::Plain => Box::new(Sum::default()),
        }
    }
}

/// Builds the graph for `project`.
pub fn compile(project: &Project) -> Graph {
    compile_with(project, native_stage)
}

/// Like [`compile`], with `stages` choosing the native stage for each slot of
/// each track. `None` leaves the slot empty.
pub(crate) fn compile_with(
    project: &Project,
    mut stages: impl FnMut(&Track, Slot) -> Option<Box<dyn Node>>,
) -> Graph {
    let mut graph = Graph::new();
    let master = graph.add_boxed(keys::MASTER, Summing::Plain.node());
    // The cue bus isn't part of the mix, so it sums plainly whatever the
    // summing.
    let cue = graph.add(keys::CUE, Sum::default());
    let metronome = Metronome::new().with_enabled(project.metronome);
    let metronome = graph.add(keys::METRONOME, metronome);
    connect(&mut graph, metronome, cue);
    for track in &project.tracks {
        let mut strip = Strip {
            graph: &mut graph,
            tail: None,
        };
        let mut slot = |strip: &mut Strip<'_>, at: Slot| {
            if let Some(stage) = stages(track, at) {
                strip.push(keys::track_slot(track.id, at), stage);
            }
        };
        if let Some(tone) = track.tone {
            let sine = Sine::new(tone.frequency, tone.level);
            strip.push(keys::track_tone(track.id), Box::new(sine));
        }
        // Devices go here, each with a `Slot::BeforeDevice` in front of it.
        slot(&mut strip, Slot::PreFader);
        let fader = Gain::new(track.gain).with_muted(track.muted);
        strip.push(keys::track_gain(track.id), Box::new(fader));
        slot(&mut strip, Slot::PostFader);
        slot(&mut strip, Slot::BusFeed);
        strip.feed(master);
    }
    graph.set_output(master).expect("the master was just added");
    graph.set_cue(cue).expect("the cue bus was just added");
    graph
}

/// The native stage the model puts in `slot`: none yet. Clip types (Tier 1)
/// and console mode (Tier 2) will fill slots from `Track::clip_type` and
/// `Track::console`.
fn native_stage(track: &Track, _slot: Slot) -> Option<Box<dyn Node>> {
    match track.clip_type {
        ClipType::Off => None,
    }
}

/// A channel strip being built, front to back.
struct Strip<'a> {
    graph: &'a mut Graph,
    /// The last node so far, which feeds the next one.
    tail: Option<NodeId>,
}

impl Strip<'_> {
    fn push(&mut self, key: NodeKey, node: Box<dyn Node>) {
        let id = self.graph.add_boxed(key, node);
        if let Some(tail) = self.tail {
            connect(self.graph, tail, id);
        }
        self.tail = Some(id);
    }

    /// Feeds the strip into `bus`.
    fn feed(self, bus: NodeId) {
        if let Some(tail) = self.tail {
            connect(self.graph, tail, bus);
        }
    }
}

fn connect(graph: &mut Graph, from: NodeId, to: NodeId) {
    graph
        .connect(from, to)
        .expect("a new node can't form a loop");
}

/// Builds and prepares graphs on a thread of its own, so a node that's slow
/// to [prepare](Node::prepare), such as a plugin loading, never holds up the
/// thread that asked for it.
///
/// Each job is a closure that builds a graph, usually [`compile`] on a
/// snapshot of the project, and the [`EngineConfig`] to prepare it for. When
/// several jobs are waiting, the compiler runs only the newest, so results
/// can lag edits but never pile up.
pub struct Compiler {
    jobs: Option<Sender<Job>>,
    results: Receiver<Compiled>,
    thread: Option<JoinHandle<()>>,
    /// The number of the last job queued.
    last_job: u64,
    /// The number of the last job done.
    last_done: u64,
}

struct Job {
    number: u64,
    config: EngineConfig,
    build: Box<dyn FnOnce() -> Graph + Send>,
}

/// A job the [`Compiler`] has done.
#[derive(Debug)]
pub struct Compiled {
    /// The job's number, from [`Compiler::compile`]. The jobs before it that
    /// it overtook have no results.
    pub job: u64,
    /// The graph, prepared for the job's config.
    pub graph: Result<PreparedGraph, GraphError>,
}

impl Compiler {
    /// Starts the compiler's thread, `musicable-compiler`.
    ///
    /// # Panics
    ///
    /// If the operating system can't start a thread.
    pub fn spawn() -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (result_tx, results) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("musicable-compiler".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    let graph = (job.build)().prepare(&job.config);
                    let compiled = Compiled {
                        job: job.number,
                        graph,
                    };
                    if result_tx.send(compiled).is_err() {
                        break;
                    }
                }
            })
            .expect("failed to spawn the compiler thread");
        Self {
            jobs: Some(jobs),
            results,
            thread: Some(thread),
            last_job: 0,
            last_done: 0,
        }
    }

    /// Queues a job: `build` runs on the compiler's thread, and the graph it
    /// builds is prepared for `config`. Returns the job's number. Jobs are
    /// numbered from 1, in the order they're queued.
    pub fn compile(
        &mut self,
        config: EngineConfig,
        build: impl FnOnce() -> Graph + Send + 'static,
    ) -> u64 {
        self.last_job += 1;
        let job = Job {
            number: self.last_job,
            config,
            build: Box::new(build),
        };
        if let Some(jobs) = &self.jobs {
            // Only fails if the thread is gone, which `try_recv` and `recv`
            // report.
            let _ = jobs.send(job);
        }
        self.last_job
    }

    /// Queues a job to compile `project`, as it is now, for `config`.
    pub fn compile_project(&mut self, config: EngineConfig, project: &Project) -> u64 {
        let project = project.clone();
        self.compile(config, move || compile(&project))
    }

    /// The next job done, if there is one.
    ///
    /// # Panics
    ///
    /// If a job panicked on the compiler's thread, with that panic.
    pub fn try_recv(&mut self) -> Option<Compiled> {
        match self.results.try_recv() {
            Ok(compiled) => Some(self.done(compiled)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => self.failed(),
        }
    }

    /// Waits for the next job to be done.
    ///
    /// # Panics
    ///
    /// If a job panicked on the compiler's thread, with that panic, or if
    /// there's no job to wait for.
    pub fn recv(&mut self) -> Compiled {
        // The newest job always runs, so once it's done, nothing more comes.
        assert!(self.last_done < self.last_job, "there's no job to wait for");
        match self.results.recv() {
            Ok(compiled) => self.done(compiled),
            Err(_) => self.failed(),
        }
    }

    fn done(&mut self, compiled: Compiled) -> Compiled {
        self.last_done = compiled.job;
        compiled
    }

    /// Hands on the panic that stopped the compiler's thread.
    #[cold]
    fn failed(&mut self) -> ! {
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            std::panic::resume_unwind(panic);
        }
        panic!("the compiler's thread has stopped");
    }
}

impl Drop for Compiler {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            // A panic in a job nobody waited for goes with it.
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for Compiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compiler")
            .field("last_job", &self.last_job)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::any::{Any, TypeId};
    use std::collections::BTreeSet;

    use musicable_model::{TestTone, TrackId, TrackKind};

    use super::*;
    use crate::{
        AudioMut, Block, Engine, Event, Inputs, ProcessContext, ReferenceRenderer, Renderer,
        TransportCommand, TransportState,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 128,
    };

    fn project() -> Project {
        let mut p = Project::new();
        let drums = p.add_track("Drums", TrackKind::Audio);
        p.track_mut(drums).unwrap().tone = Some(TestTone {
            frequency: 220.0,
            level: 0.5,
        });
        let bass = p.add_track("Bass", TrackKind::Midi);
        p.track_mut(bass).unwrap().tone = Some(TestTone {
            frequency: 55.0,
            level: 0.5,
        });
        p.add_track("Reverb", TrackKind::Return);
        p
    }

    /// Renders `frames` more frames of interleaved stereo, in uneven device
    /// callbacks.
    fn play(engine: &mut Engine, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; 2 * frames];
        let mut done = 0;
        for size in [441, 64, 7].into_iter().cycle() {
            let size = size.min(frames - done);
            engine.process_interleaved(&mut out[2 * done..2 * (done + size)], 2);
            done += size;
            if done == frames {
                break;
            }
        }
        out
    }

    /// Plays `graph` from the start.
    fn render(graph: PreparedGraph, frames: usize) -> Vec<f32> {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        play(&mut engine, frames)
    }

    /// Each node's key, type and input keys, in key order, and the output
    /// and cue nodes' keys.
    #[allow(clippy::type_complexity)]
    fn shape(
        graph: &PreparedGraph,
    ) -> (
        Vec<(NodeKey, TypeId, Vec<NodeKey>)>,
        NodeKey,
        Option<NodeKey>,
    ) {
        let key = |id: NodeId| graph.by_key.iter().find(|&&(_, n)| n == id).unwrap().0;
        let nodes = graph
            .by_key
            .iter()
            .map(|&(k, id)| {
                let node: &dyn Node = graph.nodes[id.0].as_ref();
                let node: &dyn Any = node;
                let inputs = graph.inputs[id.0].iter().map(|&i| key(i)).collect();
                (k, node.type_id(), inputs)
            })
            .collect();
        (nodes, key(graph.output), graph.cue.map(key))
    }

    fn no_stages(_: &Track, _: Slot) -> Option<Box<dyn Node>> {
        None
    }

    /// Halves the drums after their fader: a test stage.
    fn half_drums(track: &Track, slot: Slot) -> Option<Box<dyn Node>> {
        let here = track.name == "Drums" && slot == Slot::PostFader;
        here.then(|| Box::new(Gain::new(0.5)) as Box<dyn Node>)
    }

    /// Counts the frames it has rendered: state a swap must carry over.
    struct Ramp(f32);

    impl Node for Ramp {
        fn channels(&self) -> usize {
            1
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<Ramp>() {
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

    #[test]
    fn tracks_get_stable_keys() {
        let p = project();
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        // Master, cue, metronome, three gain stages and two tones.
        assert_eq!(graph.len(), 8);
        assert!(graph.contains(keys::MASTER));
        assert!(graph.contains(keys::CUE));
        assert!(graph.contains(keys::METRONOME));
        for track in &p.tracks {
            assert!(graph.contains(keys::track_gain(track.id)));
            assert_eq!(
                graph.contains(keys::track_tone(track.id)),
                track.tone.is_some()
            );
        }
    }

    #[test]
    fn every_node_in_a_strip_has_its_own_key() {
        let slots = [
            Slot::BeforeDevice(0),
            Slot::BeforeDevice(1),
            Slot::BeforeDevice(u16::MAX),
            Slot::PreFader,
            Slot::PostFader,
            Slot::BusFeed,
        ];
        let mut seen = BTreeSet::from([keys::MASTER, keys::METRONOME, keys::CUE]);
        for track in [TrackId(0), TrackId(1), TrackId(u32::MAX)] {
            let strip = slots.map(|slot| keys::track_slot(track, slot));
            for key in strip
                .into_iter()
                .chain([keys::track_gain(track), keys::track_tone(track)])
            {
                assert!(seen.insert(key), "{key:?} is taken");
            }
        }
    }

    #[test]
    fn empty_slots_leave_the_graph_as_it_was() {
        let mut p = project();
        p.tracks[0].gain = 0.8;
        p.tracks[1].muted = true;
        // What compile() built before channel strips: each track's tone feeds
        // its gain stage, which feeds the master, and the metronome feeds the
        // cue bus.
        let mut before = Graph::new();
        let master = before.add(keys::MASTER, Sum::default());
        let cue = before.add(keys::CUE, Sum::default());
        let metronome = before.add(keys::METRONOME, Metronome::new().with_enabled(false));
        before.connect(metronome, cue).unwrap();
        before.set_cue(cue).unwrap();
        for track in &p.tracks {
            let gain = Gain::new(track.gain).with_muted(track.muted);
            let gain = before.add(keys::track_gain(track.id), gain);
            if let Some(tone) = track.tone {
                let sine = Sine::new(tone.frequency, tone.level);
                let source = before.add(keys::track_tone(track.id), sine);
                before.connect(source, gain).unwrap();
            }
            before.connect(gain, master).unwrap();
        }
        before.set_output(master).unwrap();
        let before = before.prepare(&CONFIG).unwrap();

        let now = compile(&p).prepare(&CONFIG).unwrap();
        assert_eq!(shape(&now), shape(&before));
        assert!(render(now, 2000) == render(before, 2000));
    }

    #[test]
    fn a_slot_switches_on_and_off_during_playback_without_a_glitch() {
        let p = project();
        let off = || compile_with(&p, no_stages).prepare(&CONFIG).unwrap();
        let on = || compile_with(&p, half_drums).prepare(&CONFIG).unwrap();
        assert_eq!(on().len(), off().len() + 1);
        assert!(on().contains(keys::track_slot(p.tracks[0].id, Slot::PostFader)));

        // Off, on and off again, swapping between device callbacks.
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(off()).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        let mut out = play(&mut engine, 1000);
        handle.swap_graph(on()).unwrap();
        out.extend(play(&mut engine, 1000));
        handle.swap_graph(off()).unwrap();
        out.extend(play(&mut engine, 1000));

        // Each part is exactly what its graph would have played there had it
        // been running all along. Frame 1000 starts at sample 2000.
        let always_off = render(off(), 3000);
        let always_on = render(on(), 3000);
        assert!(out[..2000] == always_off[..2000]);
        assert!(out[2000..4000] == always_on[2000..4000]);
        assert!(out[4000..] == always_off[4000..]);
        assert!(always_on[2000..4000] != always_off[2000..4000]);
    }

    #[test]
    fn stages_keep_their_state_across_swaps() {
        let mut p = Project::new();
        p.add_track("Ramp", TrackKind::Audio);
        let ramp_in = |at: Slot| {
            move |_: &Track, slot: Slot| (slot == at).then(|| Box::new(Ramp(0.0)) as Box<dyn Node>)
        };
        let graph = |at: Slot| compile_with(&p, ramp_in(at)).prepare(&CONFIG).unwrap();
        let left = |out: Vec<f32>| -> Vec<f32> { out.into_iter().step_by(2).collect() };
        let count = |from: u16| -> Vec<f32> { (from..from + 100).map(f32::from).collect() };

        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph(Slot::BusFeed)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(0));
        // Recompiled, the stage in the same slot carries on.
        handle.swap_graph(graph(Slot::BusFeed)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(100));
        // In another slot it's another node, so it starts again.
        handle.swap_graph(graph(Slot::PreFader)).unwrap();
        assert_eq!(left(play(&mut engine, 100)), count(0));
    }

    #[test]
    fn muted_tracks_are_silent() {
        let mut p = project();
        let all = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        let bass = p.tracks[1].id;
        p.track_mut(bass).unwrap().muted = true;
        let drums_only = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        p.tracks[0].muted = true;
        let none = render(compile(&p).prepare(&CONFIG).unwrap(), 256);
        assert!(all.iter().any(|&s| s != 0.0));
        assert_ne!(all, drums_only);
        assert!(drums_only.iter().any(|&s| s != 0.0));
        assert!(none.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_compiler_returns_the_newest_project() {
        let mut compiler = Compiler::spawn();
        let mut p = Project::new();
        for i in 0..5 {
            p.add_track(format!("Track {i}"), TrackKind::Audio);
            assert_eq!(compiler.compile_project(CONFIG, &p), i + 1);
        }
        // Snapshots that queued up while it was busy are skipped.
        let mut jobs = Vec::new();
        let last = loop {
            let compiled = compiler.recv();
            jobs.push(compiled.job);
            if compiled.job == 5 {
                break compiled.graph.unwrap();
            }
        };
        // The master, the cue, the metronome and five gain stages.
        assert_eq!(last.len(), 8);
        assert!(jobs.is_sorted());
        assert!(compiler.try_recv().is_none());
    }

    #[test]
    #[should_panic = "there's no job to wait for"]
    fn waiting_when_every_job_is_done_panics_instead_of_hanging() {
        let mut compiler = Compiler::spawn();
        compiler.compile_project(CONFIG, &Project::new());
        compiler.recv();
        compiler.recv();
    }

    #[test]
    fn each_job_has_its_own_config() {
        let mut compiler = Compiler::spawn();
        let other = EngineConfig {
            sample_rate: 44_100,
            max_block: 32,
        };
        let p = project();
        compiler.compile_project(CONFIG, &p);
        let first = compiler.recv();
        compiler.compile_project(other, &p);
        let second = compiler.recv();
        assert_eq!((first.job, second.job), (1, 2));
        assert_eq!(first.graph.unwrap().config(), CONFIG);
        assert_eq!(second.graph.unwrap().config(), other);
    }

    #[test]
    fn a_job_that_panics_panics_where_its_result_is_taken() {
        let mut compiler = Compiler::spawn();
        compiler.compile(CONFIG, || panic!("a plugin failed to load"));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compiler.recv()));
        let payload = caught.expect_err("the job panicked");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"a plugin failed to load")
        );
    }

    #[test]
    fn the_metronome_plays_alongside_the_master_not_through_it() {
        let mut p = project();
        p.metronome = true;
        p.tracks.iter_mut().for_each(|t| t.muted = true);
        let mut graph = compile(&p).prepare(&CONFIG).unwrap();
        let block = Block {
            frames: 64,
            time: 0,
            transport: TransportState {
                playing: true,
                ..TransportState::default()
            },
            tempo: &p.tempo,
        };
        ReferenceRenderer.render(&mut graph, &block);
        // The downbeat's click is on the cue output, and not in the mix.
        assert_eq!(graph.cue(64).unwrap().channel(0)[0], 0.5);
        assert!(graph.output(64).iter().flatten().all(|&s| s == 0.0));
    }

    #[test]
    fn the_metronome_follows_the_project() {
        let mut p = project();
        p.tracks.iter_mut().for_each(|t| t.muted = true);
        let clicks = |p: &Project| render(compile(p).prepare(&CONFIG).unwrap(), 1000);
        assert!(clicks(&p).iter().all(|&s| s == 0.0));
        p.metronome = true;
        // The downbeat, at the first frame, at the metronome's level.
        assert_eq!(clicks(&p)[..2], [0.5, 0.5]);

        // Switched off and on again while playing.
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle
            .swap_graph(compile(&p).prepare(&CONFIG).unwrap())
            .unwrap();
        let enabled = |value| Event::Param {
            node: keys::METRONOME,
            param: Metronome::ENABLED,
            value,
        };
        handle.send(enabled(0.0)).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        assert!(play(&mut engine, 24_000).iter().all(|&s| s == 0.0));
        handle.send(enabled(1.0)).unwrap();
        // The second beat.
        assert_eq!(play(&mut engine, 1)[..2], [0.25, 0.25]);
    }
}

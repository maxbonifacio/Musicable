//! Compiling the project model into a graph, off the audio thread.
//!
//! Each track becomes a channel strip feeding the master bus: its test tone,
//! if it has one, then its gain stage, with built-in slots for native stages
//! along the way (see [`Slot`]). Empty slots add no nodes. The metronome
//! feeds the master too. Node keys come from track ids and slots (see
//! [`keys`]), so a track's nodes keep their identity when the project is
//! recompiled.

use std::sync::mpsc::{self, Receiver, Sender};
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
    // Straight into the master for now. It belongs on a cue output, once
    // there is one.
    let metronome = Metronome::new().with_enabled(project.metronome);
    let metronome = graph.add(keys::METRONOME, metronome);
    connect(&mut graph, metronome, master);
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

/// Compiles and prepares projects on a background thread.
///
/// Send it snapshots of the model as it changes. When several are waiting it
/// compiles only the newest, so results can lag edits but never pile up.
pub struct Compiler {
    jobs: Option<Sender<Project>>,
    results: Receiver<Result<PreparedGraph, GraphError>>,
    thread: Option<JoinHandle<()>>,
}

impl Compiler {
    pub fn spawn(config: EngineConfig) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Project>();
        let (result_tx, results) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("musicable-compiler".into())
            .spawn(move || {
                while let Ok(mut project) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        project = newer;
                    }
                    if result_tx.send(compile(&project).prepare(&config)).is_err() {
                        break;
                    }
                }
            })
            .expect("failed to spawn the compiler thread");
        Self {
            jobs: Some(jobs),
            results,
            thread: Some(thread),
        }
    }

    /// Queues a snapshot of the model to compile.
    pub fn compile(&self, project: Project) {
        if let Some(jobs) = &self.jobs {
            // Only fails if the thread is gone, which `recv` reports.
            let _ = jobs.send(project);
        }
    }

    /// The next finished graph, if there is one.
    pub fn try_recv(&self) -> Option<Result<PreparedGraph, GraphError>> {
        self.results.try_recv().ok()
    }

    /// Waits for the next finished graph. `None` if the compiler thread has
    /// stopped.
    pub fn recv(&self) -> Option<Result<PreparedGraph, GraphError>> {
        self.results.recv().ok()
    }
}

impl Drop for Compiler {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::{Any, TypeId};
    use std::collections::BTreeSet;

    use musicable_model::{TestTone, TrackId, TrackKind};

    use super::*;
    use crate::{AudioMut, Engine, Event, Inputs, ProcessContext, TransportCommand};

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
    /// node's key.
    fn shape(graph: &PreparedGraph) -> (Vec<(NodeKey, TypeId, Vec<NodeKey>)>, NodeKey) {
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
        (nodes, key(graph.output))
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
        // Master, metronome, three gain stages and two tones.
        assert_eq!(graph.len(), 7);
        assert!(graph.contains(keys::MASTER));
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
        let mut seen = BTreeSet::from([keys::MASTER, keys::METRONOME]);
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
        // its gain stage, which feeds the master, after the metronome.
        let mut before = Graph::new();
        let master = before.add(keys::MASTER, Sum::default());
        let metronome = before.add(keys::METRONOME, Metronome::new().with_enabled(false));
        before.connect(metronome, master).unwrap();
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
        let compiler = Compiler::spawn(CONFIG);
        let mut p = Project::new();
        for i in 0..5 {
            p.add_track(format!("Track {i}"), TrackKind::Audio);
            compiler.compile(p.clone());
        }
        // Snapshots that queued up while it was busy are skipped.
        let mut last = None;
        while last.as_ref().is_none_or(|g: &PreparedGraph| g.len() < 7) {
            last = Some(compiler.recv().unwrap().unwrap());
        }
        assert_eq!(last.unwrap().len(), 7);
        assert!(compiler.try_recv().is_none());
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

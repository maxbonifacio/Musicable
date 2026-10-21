//! Compiling the project model into a graph, off the audio thread.
//!
//! Each track becomes a gain stage feeding the master bus, with its test tone,
//! if it has one, feeding the gain stage. Node keys come from track ids (see
//! [`keys`]), so a track's nodes keep their identity when the project is
//! recompiled.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use musicable_model::Project;

use crate::nodes::{Gain, Sine, Sum};
use crate::{EngineConfig, Graph, GraphError, PreparedGraph};

/// The keys [`compile`] gives model objects' nodes.
pub mod keys {
    use musicable_model::TrackId;

    use crate::NodeKey;

    /// The master bus.
    pub const MASTER: NodeKey = NodeKey(0);

    /// A track's gain stage, a [`Gain`](crate::nodes::Gain).
    pub fn track_gain(track: TrackId) -> NodeKey {
        NodeKey(1 << 32 | u64::from(track.0))
    }

    /// A track's test tone, a [`Sine`](crate::nodes::Sine).
    pub fn track_tone(track: TrackId) -> NodeKey {
        NodeKey(2 << 32 | u64::from(track.0))
    }
}

/// Builds the graph for `project`.
pub fn compile(project: &Project) -> Graph {
    let mut graph = Graph::new();
    let master = graph.add(keys::MASTER, Sum::default());
    for track in &project.tracks {
        let gain = graph.add(
            keys::track_gain(track.id),
            Gain::new(track.gain).with_muted(track.muted),
        );
        if let Some(tone) = track.tone {
            let source = graph.add(
                keys::track_tone(track.id),
                Sine::new(tone.frequency, tone.level),
            );
            graph
                .connect(source, gain)
                .expect("a new node can't form a loop");
        }
        graph
            .connect(gain, master)
            .expect("a new node can't form a loop");
    }
    graph.set_output(master).expect("the master was just added");
    graph
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
    use musicable_model::{TestTone, TrackKind};

    use super::*;
    use crate::{AudioBuffer, Engine, Event, TransportCommand};

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

    fn render(graph: PreparedGraph, frames: usize) -> Vec<f32> {
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        let mut out = AudioBuffer::new(2, frames);
        engine.process(&mut out.view_mut(frames));
        out.view(frames).channel(0).to_vec()
    }

    #[test]
    fn tracks_get_stable_keys() {
        let p = project();
        let graph = compile(&p).prepare(&CONFIG).unwrap();
        // Master, three gain stages and two tones.
        assert_eq!(graph.len(), 6);
        assert!(graph.contains(keys::MASTER));
        for track in &p.tracks {
            assert!(graph.contains(keys::track_gain(track.id)));
            assert_eq!(
                graph.contains(keys::track_tone(track.id)),
                track.tone.is_some()
            );
        }
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
        while last.as_ref().is_none_or(|g: &PreparedGraph| g.len() < 6) {
            last = Some(compiler.recv().unwrap().unwrap());
        }
        assert_eq!(last.unwrap().len(), 6);
        assert!(compiler.try_recv().is_none());
    }
}

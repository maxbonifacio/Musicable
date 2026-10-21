//! Graphs compile on a thread of their own, so a node that's slow to
//! prepare, as a plugin loading can be, never holds up the window. What the
//! user does meanwhile reaches the engine with the graph.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use musicable_app::{Session, demo_project};
use musicable_commands::{KeyChord, Keymap};
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Inputs, Node, NodeKey, ProcessContext, compile, rt,
};
use musicable_model::Project;
use musicable_model::time::Beats;

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// How long a gate stays shut if the test never opens it, so a session that
/// waits for the graph fails the test instead of hanging it.
const GIVE_UP: Duration = Duration::from_secs(10);

/// Holds up the prepare of every [`Slow`] node until the test opens it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    fn wait(&self) {
        let open = self.open.lock().unwrap();
        let _ = self
            .opened
            .wait_timeout_while(open, GIVE_UP, |open| !*open)
            .unwrap();
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    fn shut(&self) {
        *self.open.lock().unwrap() = false;
    }
}

/// A node that takes until the gate opens to prepare, then plays silence.
struct Slow(Arc<Gate>);

impl Node for Slow {
    fn prepare(&mut self, _: &EngineConfig) {
        self.0.wait();
    }

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
    }
}

/// A session whose graphs each have a [`Slow`] node, and its engine.
fn slow_session(project: Project, gate: &Arc<Gate>) -> (Engine, Session) {
    let (engine, handle) = Engine::new(CONFIG);
    let gate = gate.clone();
    let session =
        Session::with_compiler(project, Keymap::default_keymap(), handle, move |project| {
            let mut graph = compile(project);
            graph.add(NodeKey(u64::MAX), Slow(gate.clone()));
            graph
        });
    (engine, session)
}

/// Renders `frames` frames as the audio thread would, then ticks the session
/// as the UI would. Returns the left channel.
fn render(engine: &mut Engine, session: &mut Session, frames: usize) -> Vec<f32> {
    let mut output = vec![0.0; 2 * frames];
    engine.process_interleaved(&mut output, 2);
    session.tick();
    output.into_iter().step_by(2).collect()
}

fn press(session: &mut Session, chord: &str) {
    let chord: KeyChord = chord.parse().unwrap();
    assert!(session.key_pressed(chord), "{chord} runs a command");
}

fn clicks(samples: &[f32]) -> bool {
    samples.iter().any(|sample| sample.abs() > 0.1)
}

#[test]
fn a_slow_graph_never_holds_up_the_window() {
    assert!(rt::checker_installed());
    let gate = Arc::new(Gate::default());
    let started = Instant::now();
    let (mut engine, mut session) = slow_session(Project::new(), &gate);

    // What the window does while the graph compiles: the metronome on, play,
    // a new tempo, and frame after frame of ticks while the device plays.
    press(&mut session, "O");
    press(&mut session, "Space");
    session.set_tempo(90.0);
    for _ in 0..20 {
        render(&mut engine, &mut session, 256);
    }
    assert!(
        started.elapsed() < GIVE_UP / 2,
        "the session waited for the graph"
    );
    assert!(session.compiling());
    // The project changed at once. The engine waits for the graph: it plays
    // silence, and the transport hasn't started.
    assert!(session.project().metronome);
    assert_eq!(session.project().tempo.bpm_at(Beats::ZERO), 90.0);
    let stats = session.stats();
    assert_eq!((stats.swaps, stats.playing, stats.position), (0, false, 0));

    // Once it's ready, everything the window did reaches the engine, in
    // order: it plays from the start, at 90 BPM, with the metronome on.
    gate.open();
    session.wait_for_graph();
    assert!(!session.compiling());
    let out = render(&mut engine, &mut session, 100);
    assert!(clicks(&out));
    let beat = 60.0 / 90.0 * 48_000.0;
    let out = render(&mut engine, &mut session, beat as usize);
    assert!(clicks(&out[beat as usize - 100..]), "a beat at 90 BPM");
    let stats = session.stats();
    assert_eq!((stats.swaps, stats.playing), (1, true));
    assert_eq!(stats.events_late, 0);
    assert_eq!(stats.events_unmatched, 0);
}

#[test]
fn a_project_opened_while_the_last_one_compiles_replaces_it() {
    let gate = Arc::new(Gate::default());
    let (mut engine, mut session) = slow_session(Project::new(), &gate);
    session.open(demo_project(), None);
    render(&mut engine, &mut session, 256);
    gate.open();
    session.wait_for_graph();
    press(&mut session, "Space");
    let out = render(&mut engine, &mut session, 256);

    // Only the demo's graph reached the engine, and its metronome clicks.
    let stats = session.stats();
    assert_eq!((stats.swaps, stats.playing), (1, true));
    assert!(clicks(&out));
    assert_eq!(session.project(), &demo_project());
}

#[test]
fn a_new_engine_waits_for_its_graph_before_it_plays_on() {
    let gate = Arc::new(Gate::default());
    gate.open();
    let (mut engine, mut session) = slow_session(Project::new(), &gate);
    session.wait_for_graph();
    press(&mut session, "Space");
    render(&mut engine, &mut session, 24_000);
    assert_eq!(session.playhead(), Beats::new(1));

    // The new engine's graph is slow. Until it comes, the new engine stays
    // where it starts, stopped.
    gate.shut();
    let (new_engine, handle) = Engine::new(CONFIG);
    drop(session.replace_engine(handle));
    engine = new_engine;
    render(&mut engine, &mut session, 1000);
    let stats = session.stats();
    assert_eq!((stats.playing, stats.position), (false, 0));

    // Then it goes on from beat 1, playing.
    gate.open();
    session.wait_for_graph();
    render(&mut engine, &mut session, 100);
    let stats = session.stats();
    assert!(stats.playing);
    assert_eq!(stats.position, 24_000 + 100);
}

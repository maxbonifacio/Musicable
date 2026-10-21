//! What the app shell (M7 in the Tier 0 plan) asks of the session: carrying
//! out file commands, opening projects, a tempo field, and moving to a new
//! engine when the audio device changes.

use musicable_app::{Session, demo_project};
use musicable_commands::{Keymap, Request, file};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_model::Project;
use musicable_model::time::{BeatRange, Beats};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// A session, and an engine that renders when told to, standing in for the
/// audio device.
struct Rig {
    engine: Engine,
    session: Session,
}

impl Rig {
    fn new(project: Project) -> Self {
        let (engine, handle) = Engine::new(CONFIG);
        let session = Session::new(project, Keymap::default_keymap(), handle);
        Self { engine, session }
    }

    fn press(&mut self, chord: &str) -> bool {
        self.session.key_pressed(chord.parse().unwrap())
    }

    /// Renders `frames` frames as the audio thread would, then ticks the
    /// session as the UI would. Returns the left channel.
    fn render(&mut self, frames: u64) -> Vec<f32> {
        let mut output = vec![0.0; 2 * frames as usize];
        self.engine.process_interleaved(&mut output, 2);
        self.session.tick();
        output.into_iter().step_by(2).collect()
    }

    fn position(&self) -> u64 {
        self.session.stats().position
    }
}

fn clicks(samples: &[f32]) -> bool {
    samples.iter().any(|sample| sample.abs() > 0.1)
}

#[test]
fn the_demo_is_the_one_in_the_project_format_fixtures() {
    let saved = include_str!("../../io/tests/fixtures/v1/demo.musicable");
    let saved = musicable_io::project::from_str(saved).unwrap();
    assert_eq!(demo_project(), saved);
}

#[test]
fn file_commands_ask_the_app() {
    let mut rig = Rig::new(Project::new());
    assert!(rig.press("Ctrl+S"));
    assert!(rig.press("Ctrl+Shift+S"));
    rig.session.run(&file::NEW_DEMO).unwrap();
    assert!(rig.press("Ctrl+O"));
    assert_eq!(
        rig.session.take_requests(),
        [
            Request::Save,
            Request::SaveAs,
            Request::NewDemo,
            Request::Open
        ]
    );
    assert_eq!(rig.session.take_requests(), []);
}

#[test]
fn opening_a_project_stops_at_its_start() {
    let mut rig = Rig::new(Project::new());
    assert!(rig.press("O"));
    assert!(rig.press("Space"));
    rig.render(10_000);
    assert!(rig.session.is_modified());

    rig.session.open(demo_project());
    rig.render(100);
    let stats = rig.session.stats();
    assert!(!stats.playing);
    assert_eq!(stats.position, 0);
    assert_eq!(stats.swaps, 2);
    assert_eq!(rig.session.project(), &demo_project());
    assert!(!rig.session.is_modified());

    // The demo's metronome clicks, at the demo's tempo: 124 BPM.
    assert!(rig.press("Space"));
    let frames_per_beat: f64 = 60.0 / 124.0 * 48_000.0;
    let out = rig.render(frames_per_beat as u64 + 100);
    assert!(clicks(&out[..100]));
    assert!(clicks(&out[frames_per_beat.ceil() as usize..]));
}

#[test]
fn a_tempo_field_sets_the_tempo_where_the_playhead_is() {
    let mut rig = Rig::new(Project::new());
    assert!(rig.press("Space"));
    // One beat at 120 BPM, then stop there.
    rig.render(24_000);
    assert!(rig.press("Space"));
    rig.render(100);
    assert_eq!(rig.session.playhead(), Beats::new(1));

    // At 60 BPM, the playhead keeps its place in beats.
    rig.session.set_tempo(60.0);
    rig.render(100);
    assert_eq!(rig.session.project().tempo.bpm_at(Beats::ZERO), 60.0);
    assert_eq!(rig.position(), 48_000);
    assert_eq!(rig.session.playhead(), Beats::new(1));
    assert!(rig.session.is_modified());
}

/// A new device can need an engine with another sample rate or block size.
/// The new engine takes over where the old one was.
#[test]
fn a_new_engine_takes_over_where_the_old_one_was() {
    let mut project = Project::new();
    project.tempo.set_tempo(100.0);
    project.loop_region = BeatRange::new(Beats::new(2), Beats::new(3));
    project.looping = true;
    project.metronome = true;
    let mut rig = Rig::new(project);

    // Halfway through beat 3, at 48 kHz: a beat is 28,800 frames.
    assert!(rig.press("Space"));
    rig.render(72_000);
    assert_eq!(rig.session.playhead(), Beats::from_ratio(5, 2));

    let config = EngineConfig {
        sample_rate: 44_100,
        max_block: 128,
    };
    let (engine, handle) = Engine::new(config);
    let old = rig.session.replace_engine(handle);
    let old_engine = std::mem::replace(&mut rig.engine, engine);
    drop((old_engine, old));
    assert_eq!(rig.session.engine_config(), config);

    // At 44.1 kHz a beat is 26,460 frames. The new engine plays from the
    // same place, and loops back to beat 2 at the end of beat 3, with a
    // click.
    rig.render(100);
    assert!(rig.session.stats().playing);
    assert_eq!(rig.position(), 66_150 + 100);
    let out = rig.render(13_130 + 100);
    assert_eq!(rig.position(), 52_920 + 100);
    assert!(clicks(&out[13_130..]));
    assert_eq!(rig.session.stats().events_late, 0);
    assert!(!rig.session.is_modified());
}

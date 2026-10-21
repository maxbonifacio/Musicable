//! The done test for commands, keymaps and transactions (M6 in the Tier 0
//! plan): key presses go through the keymap to commands, and an engine,
//! rendering offline, plays what they changed.

use musicable_app::Session;
use musicable_commands::{KeyChord, Keymap, KeymapFile};
use musicable_engine::{Engine, EngineConfig, Stats, rt};
use musicable_model::Project;
use musicable_model::time::{BeatRange, Beats};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// Frames to the beat at 120 BPM, the default tempo.
const BEAT: u64 = 24_000;

/// A session, and an engine that renders when told to, standing in for the
/// audio device.
struct Rig {
    engine: Engine,
    session: Session,
}

impl Rig {
    fn new(project: Project) -> Self {
        Self::with_keymap(project, Keymap::default_keymap())
    }

    fn with_keymap(project: Project, keymap: Keymap) -> Self {
        let (engine, handle) = Engine::new(CONFIG);
        let session = Session::new(project, keymap, handle);
        Self { engine, session }
    }

    /// Presses `chord`. Returns whether it ran a command.
    fn press(&mut self, chord: &str) -> bool {
        let chord: KeyChord = chord.parse().unwrap();
        self.session.key_pressed(chord)
    }

    /// Renders `frames` frames as the audio thread would, then ticks the
    /// session as the UI would. Returns the left channel.
    fn render(&mut self, frames: u64) -> Vec<f32> {
        let mut output = vec![0.0; 2 * frames as usize];
        self.engine.process_interleaved(&mut output, 2);
        self.session.tick();
        output.into_iter().step_by(2).collect()
    }

    fn stats(&self) -> Stats {
        self.session.stats()
    }

    /// Checks that every event reached the engine in time, and found its
    /// node.
    fn assert_clean(&self) {
        let stats = self.stats();
        assert_eq!(stats.events_late, 0);
        assert_eq!(stats.events_dropped, 0);
        assert_eq!(stats.events_unmatched, 0);
    }
}

fn clicks(samples: &[f32]) -> bool {
    samples.iter().any(|sample| sample.abs() > 0.1)
}

#[test]
fn key_presses_drive_the_engine() {
    assert!(rt::checker_installed());
    let mut project = Project::new();
    project.loop_region = BeatRange::new(Beats::new(2), Beats::new(3));
    let mut rig = Rig::new(project);

    // O turns the metronome on, and Space plays: a click on the first beat.
    assert!(rig.press("O"));
    assert!(rig.press("Space"));
    let out = rig.render(BEAT);
    assert!(clicks(&out[..100]));
    assert!(rig.stats().playing);
    assert_eq!(rig.stats().position, BEAT);

    // O again turns it off.
    assert!(rig.press("O"));
    assert!(!clicks(&rig.render(BEAT)));

    // Ctrl+L loops over beats 2 to 3, where the playhead is, and again stops
    // looping.
    assert!(rig.press("Ctrl+L"));
    rig.render(BEAT + 100);
    assert_eq!(rig.stats().position, 2 * BEAT + 100);
    assert!(rig.press("Ctrl+L"));
    rig.render(BEAT);
    assert_eq!(rig.stats().position, 3 * BEAT + 100);

    // Ctrl+Period stops, Home goes to the start, Shift+Space plays and Space
    // stops.
    assert!(rig.press("Ctrl+Period"));
    rig.render(100);
    assert!(!rig.stats().playing);
    assert_eq!(rig.stats().position, 3 * BEAT + 100);
    assert!(rig.press("Home"));
    assert!(rig.press("Shift+Space"));
    rig.render(4800);
    assert!(rig.press("Space"));
    rig.render(100);
    assert!(!rig.stats().playing);
    assert_eq!(rig.stats().position, 4800);

    // Ctrl+Shift+Up raises the tempo to 121 BPM. The playhead keeps its place
    // in beats, which now come sooner. Ctrl+Shift+Down undoes it.
    assert!(rig.press("Ctrl+Shift+Up"));
    rig.render(100);
    assert_eq!(rig.session.project().tempo.bpm_at(Beats::ZERO), 121.0);
    assert_eq!(rig.stats().position, 4760);
    assert!(rig.press("Ctrl+Shift+Down"));
    rig.render(100);
    assert_eq!(rig.session.project().tempo.bpm_at(Beats::ZERO), 120.0);
    assert_eq!(rig.stats().position, 4800);

    // A chord bound to nothing runs nothing.
    assert!(!rig.press("Ctrl+Shift+F24"));
    assert_eq!(rig.stats().swaps, 1);
    rig.assert_clean();
}

#[test]
fn opening_a_project_sets_up_the_engine() {
    let mut project = Project::new();
    project.tempo.set_tempo(60.0);
    project.loop_region = BeatRange::new(Beats::ZERO, Beats::new(1));
    project.looping = true;
    project.metronome = true;
    let mut rig = Rig::new(project);

    // At 60 BPM a beat is 2 × 24,000 frames. The playhead goes round the
    // one-beat loop, clicking each time it starts it.
    assert!(rig.press("Space"));
    let out = rig.render(2 * BEAT + 100);
    assert!(clicks(&out[..100]));
    assert!(!clicks(&out[BEAT as usize..][..100]));
    assert!(clicks(&out[2 * BEAT as usize..]));
    assert_eq!(rig.stats().position, 100);
    rig.assert_clean();
}

#[test]
fn a_keymap_file_remaps_the_keys() {
    let file = r#"
        [bindings]
        "transport.play_stop" = ["P"]
    "#;
    let (changes, problems) = KeymapFile::parse(file).unwrap();
    assert_eq!(problems, []);
    let mut keymap = Keymap::default_keymap();
    changes.apply_to(&mut keymap);

    let mut rig = Rig::with_keymap(Project::new(), keymap);
    assert!(!rig.press("Space"));
    rig.render(100);
    assert!(!rig.stats().playing);
    assert!(rig.press("P"));
    rig.render(100);
    assert!(rig.stats().playing);
}

/// While the engine doesn't render, as when the audio device is stopped, its
/// queue fills up. The session holds on to what doesn't fit, and sends it in
/// order once there is room.
#[test]
fn nothing_is_lost_while_the_engine_falls_behind() {
    let mut rig = Rig::new(Project::new());
    rig.render(100);
    // An odd number of toggles, two queues' worth (1024 each) and one.
    for _ in 0..2049 {
        assert!(rig.press("O"));
    }
    assert!(rig.session.project().metronome);
    for _ in 0..4 {
        rig.render(100);
    }
    assert!(rig.press("Space"));
    assert!(clicks(&rig.render(100)));
    rig.assert_clean();
}

//! Musicable's own devices in the app: the device commands add them, their
//! parameters reach the engine as they change, undo takes the changes back,
//! and they keep their settings when the graph is built again.

use musicable_app::Session;
use musicable_commands::{Args, CommandId, Keymap, Value, device, edit, track, transport};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_model::native::utility;
use musicable_model::{DeviceId, Project, TestTone, TrackKind};

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
        let mut session = Session::new(project, None, Keymap::default_keymap(), handle);
        session.wait_for_graph();
        Self { engine, session }
    }

    /// Runs the command `id` with `args`, and waits for the graph it needs,
    /// if any, as the app's next frames would.
    fn run(&mut self, id: &CommandId, args: Args) -> Value {
        let value = self.session.run_with(id, &args).unwrap();
        self.session.wait_for_graph();
        value
    }

    /// Sets parameter `param` of `device` to `value`.
    fn set(&mut self, device: DeviceId, param: usize, value: f64) {
        let args = Args::new()
            .with("device", device)
            .with("param", param as i64)
            .with("value", value);
        self.run(&device::SET_PARAM, args);
    }

    /// Renders a fifth of a second as the audio thread would, ticking the
    /// session as the UI would, and returns the peak of each channel over
    /// the last half of it, when any change has long settled.
    fn peaks(&mut self) -> [f32; 2] {
        let frames = 9_600;
        let mut output = vec![0.0; 2 * frames];
        for block in output.chunks_mut(2 * 480) {
            self.engine.process_interleaved(block, 2);
            self.session.tick();
        }
        let mut peaks = [0.0_f32; 2];
        for frame in output[frames..].chunks(2) {
            for (peak, sample) in peaks.iter_mut().zip(frame) {
                *peak = peak.max(sample.abs());
            }
        }
        let stats = self.session.stats();
        assert_eq!((stats.events_late, stats.events_dropped), (0, 0));
        assert_eq!(stats.events_unmatched, 0);
        peaks
    }
}

#[track_caller]
fn near(peaks: [f32; 2], expected: [f32; 2]) {
    for (peak, expected) in peaks.into_iter().zip(expected) {
        assert!(
            (peak - expected).abs() < 0.01,
            "{peaks:?}, expected {expected:?}"
        );
    }
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn a_utility_on_a_track_follows_its_controls() {
    let mut project = Project::new();
    let lead = project.add_track("Lead", TrackKind::Audio);
    project.track_mut(lead).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.5,
    });
    let mut rig = Rig::new(project);
    rig.session.run(&transport::PLAY).unwrap();
    near(rig.peaks(), [0.5, 0.5]);

    let added = rig.run(
        &device::ADD,
        Args::new().with("kind", "utility").with("track", lead),
    );
    let Value::Device(utility) = added else {
        panic!("a device, not {added:?}");
    };
    // A new utility leaves the signal as it is.
    near(rig.peaks(), [0.5, 0.5]);
    rig.set(utility, utility::GAIN, 0.5);
    near(rig.peaks(), [0.25, 0.25]);
    rig.set(utility, utility::PAN, 1.0);
    near(rig.peaks(), [0.0, 0.25]);

    // Building the graph again, for a new track, keeps the settings.
    rig.run(&track::NEW_AUDIO_TRACK, Args::new());
    near(rig.peaks(), [0.0, 0.25]);

    // Undo takes the changes back, one at a time.
    rig.run(&edit::UNDO, Args::new());
    assert_eq!(rig.session.undo_name(), Some("Change Utility"));
    rig.run(&edit::UNDO, Args::new());
    near(rig.peaks(), [0.25, 0.25]);
    rig.run(&edit::UNDO, Args::new());
    near(rig.peaks(), [0.5, 0.5]);

    rig.run(&device::DELETE, Args::new().with("device", utility));
    near(rig.peaks(), [0.5, 0.5]);
    assert_eq!(rig.session.undo_name(), Some("Delete Utility"));
}

//! The mixer in the app: changes from the mixer's controls reach the engine,
//! the meters move, the track commands add tracks, a wire from one track
//! keys a compressor on another, opening another project starts it afresh,
//! and a mix plays the same with the parallel scheduler as with the
//! reference renderer.

use musicable_app::Session;
use musicable_app::null_test::{self, Player, Render};
use musicable_commands::mixer::Change;
use musicable_commands::{Args, CommandId, Keymap, Value, wire};
use musicable_engine::{Engine, EngineConfig, ParallelOptions, rt};
use musicable_model::native::{NativeDevice, NativeKind, compressor};
use musicable_model::{Chain, ClipType, DeviceKind, Project, TestTone, TrackId, TrackKind};

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
    /// Opens `project`, and waits for its graph, as the app's first frames
    /// would.
    fn new(project: Project) -> Self {
        let (engine, handle) = Engine::new(CONFIG);
        let mut session = Session::new(project, None, Keymap::default_keymap(), handle);
        session.wait_for_graph();
        Self { engine, session }
    }

    fn press(&mut self, chord: &str) -> bool {
        self.session.key_pressed(chord.parse().unwrap())
    }

    /// Makes `change`, and waits for the graph it needs, if any, as the
    /// app's next frames would.
    fn change(&mut self, change: Change) {
        self.session.change_mixer(change).unwrap();
        self.session.wait_for_graph();
    }

    /// Runs the command `id` with `args`, and waits for the graph it needs,
    /// if any.
    fn run(&mut self, id: &CommandId, args: Args) -> Value {
        let value = self.session.run_with(id, &args).unwrap();
        self.session.wait_for_graph();
        value
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

/// Adds an audio track with a test tone at level 0.5.
fn tone_track(project: &mut Project, name: &str, frequency: f64) -> TrackId {
    let id = project.add_track(name, TrackKind::Audio);
    project.track_mut(id).unwrap().tone = Some(TestTone {
        frequency,
        level: 0.5,
    });
    id
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
fn the_mixer_drives_the_engine() {
    let mut project = Project::new();
    let lead = tone_track(&mut project, "Lead", 440.0);
    let pad = tone_track(&mut project, "Pad", 330.0);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let mut rig = Rig::new(project);
    assert!(rig.press("Space"));

    rig.change(Change::Muted(pad, true));
    near(rig.peaks(), [0.5, 0.5]);
    rig.change(Change::Gain(lead, 0.5));
    near(rig.peaks(), [0.25, 0.25]);
    rig.change(Change::Pan(lead, 1.0));
    near(rig.peaks(), [0.0, 0.25]);

    // A send after the fader takes the panned signal to the reverb.
    rig.change(Change::SendLevel {
        track: lead,
        to: reverb,
        level: 1.0,
    });
    near(rig.peaks(), [0.0, 0.5]);
    // Soloing the reverb leaves what the send puts in.
    rig.change(Change::Solo {
        track: reverb,
        solo: true,
        exclusive: true,
    });
    near(rig.peaks(), [0.0, 0.25]);
    // Unsoloed, and the master's hard clip stage cuts what the gain pushes
    // over full scale.
    rig.change(Change::Solo {
        track: reverb,
        solo: false,
        exclusive: false,
    });
    rig.change(Change::Gain(lead, 4.0));
    rig.change(Change::MasterClipType(ClipType::Hard));
    near(rig.peaks(), [0.0, 1.0]);
    rig.change(Change::MasterGain(0.25));
    near(rig.peaks(), [0.0, 1.0]);
    rig.change(Change::MasterClipType(ClipType::Off));
    near(rig.peaks(), [0.0, 1.0]);

    // The meters moved, and the muted track's fell back.
    let meters = rig.session.meters();
    let level = |id| meters.track(id).unwrap().level();
    assert!(level(lead).peak[1] > 1.9, "{:?}", level(lead));
    assert!(level(reverb).rms[1] > 0.5, "{:?}", level(reverb));
    assert!(level(pad).rms[0] < 0.01, "{:?}", level(pad));
    assert!(meters.master().level().peak[1] > 0.9);
}

#[test]
fn a_muted_kick_keys_a_compressor_on_the_bass() {
    let mut project = Project::new();
    let kick = tone_track(&mut project, "Kick", 55.0);
    let bass = tone_track(&mut project, "Bass", 110.0);
    // Loud after its fader, where the wire takes it, but muted.
    let track = project.track_mut(kick).unwrap();
    (track.gain, track.muted) = (4.0, true);
    let mut native = NativeDevice::new(NativeKind::Compressor);
    native.values[compressor::THRESHOLD] = 0.0;
    native.values[compressor::ATTACK] = 0.1;
    native.values[compressor::RELEASE] = 5.0;
    let device = project.new_device(DeviceKind::Native(native));
    let id = device.id;
    project
        .devices_mut(Chain::Track(bass))
        .unwrap()
        .push(device);
    let mut rig = Rig::new(project);
    assert!(rig.press("Space"));
    // The bass alone stays under the threshold.
    near(rig.peaks(), [0.5, 0.5]);

    let from_kick = Args::new()
        .with("track", kick)
        .with("tap", "post_fader")
        .with("device", id);
    let Value::Wire(key) = rig.run(&wire::CONNECT, from_kick) else {
        panic!("a wire");
    };
    let [left, right] = rig.peaks();
    assert!(left < 0.4 && right < 0.4, "ducked: {left}, {right}");
    // Turned down, the key is under the threshold too.
    let gain = Args::new().with("wire", key).with("gain", 0.25);
    rig.run(&wire::SET_GAIN, gain);
    near(rig.peaks(), [0.5, 0.5]);
}

#[test]
fn deleting_a_soloed_track_lets_the_rest_play() {
    let mut project = Project::new();
    let lead = tone_track(&mut project, "Lead", 440.0);
    tone_track(&mut project, "Pad", 330.0);
    let mut rig = Rig::new(project);
    assert!(rig.press("Space"));
    rig.change(Change::Solo {
        track: lead,
        solo: true,
        exclusive: true,
    });
    near(rig.peaks(), [0.5, 0.5]);
    rig.change(Change::Muted(lead, true));
    near(rig.peaks(), [0.0, 0.0]);
    // The pad's output stage was muted by the solo. The new graph has it
    // open, and the engine mustn't send the old value again.
    rig.change(Change::Delete(lead));
    near(rig.peaks(), [0.5, 0.5]);
}

#[test]
fn the_track_commands_add_tracks_with_meters() {
    let mut rig = Rig::new(Project::new());
    for chord in ["Ctrl+T", "Ctrl+Shift+T", "Ctrl+Alt+T"] {
        assert!(rig.press(chord), "{chord}");
    }
    let project = rig.session.project();
    let kinds: Vec<TrackKind> = project.tracks.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [TrackKind::Audio, TrackKind::Midi, TrackKind::Return]
    );
    for track in &project.tracks {
        assert!(rig.session.meters().track(track.id).is_some());
    }
    assert!(rig.session.is_modified());
}

#[test]
fn another_project_starts_afresh() {
    let mut first = Project::new();
    let id = tone_track(&mut first, "Lead", 440.0);
    let mut rig = Rig::new(first);
    assert!(rig.press("Space"));
    near(rig.peaks(), [0.5, 0.5]);
    rig.change(Change::Gain(id, 0.0));
    rig.change(Change::Muted(id, true));
    near(rig.peaks(), [0.0, 0.0]);

    // A track with the same id, at unity and unmuted.
    let mut second = Project::new();
    assert_eq!(tone_track(&mut second, "Other", 330.0), id);
    rig.session.open(second, None);
    rig.session.wait_for_graph();
    assert!(rig.press("Space"));
    near(rig.peaks(), [0.5, 0.5]);
}

/// The mixer fixture: groups in groups, sends before and after the fader,
/// devices and clip stages, with its solo on and off.
#[test]
fn a_mix_plays_the_same_with_the_parallel_scheduler() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../io/tests/fixtures/v2/mixer.musicable"
    );
    let soloed = musicable_io::project::load(path).unwrap();
    let mut unsoloed = soloed.clone();
    unsoloed.tracks.iter_mut().for_each(|t| t.solo = false);
    for project in [soloed, unsoloed] {
        let shared = Render::Parallel(ParallelOptions {
            threads: 3,
            inline_below: None,
            realtime: false,
            ..ParallelOptions::default()
        });
        let renders = [Render::Reference, Render::App, shared];
        let mut players: Vec<Player> = renders
            .iter()
            .map(|render| Player::new(project.clone(), None, render, CONFIG))
            .collect();
        let differences = null_test::compare(&mut players, u64::from(CONFIG.sample_rate));
        assert_eq!(differences, [None, None, None]);
        assert!(players[0].peak() > 0.05, "{}", players[0].peak());
    }
}

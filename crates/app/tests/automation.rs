//! Automation in the app: lanes drawn with the automation commands play the
//! same with every renderer, come back from the project's file as they were
//! saved, and go and come back with undo and redo, in the engine too.

use std::fs;
use std::path::PathBuf;

use musicable_app::Session;
use musicable_app::null_test::{self, Player, Render};
use musicable_commands::{Args, Keymap, automation, edit, transport};
use musicable_engine::{Engine, EngineConfig, ParallelOptions, rt};
use musicable_io::project;
use musicable_model::param::ParamAddress;
use musicable_model::time::Beats;
use musicable_model::{Chain, DeviceKind, Project, TestTone, TrackId, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// Four beats at 120 BPM, which the lanes all fit in.
const SONG: u64 = 96_000;

/// A low tone, and a high one through a test gain that sends to a return,
/// without automation.
fn mix() -> Project {
    let mut project = Project::new();
    let tone = |project: &mut Project, name: &str, frequency| {
        let id = project.add_track(name, TrackKind::Audio);
        project.track_mut(id).unwrap().tone = Some(TestTone {
            frequency,
            level: 0.5,
        });
        id
    };
    tone(&mut project, "Low", 110.0);
    let high = tone(&mut project, "High", 880.0);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let send = project.new_send(reverb);
    let gain = project.new_device(DeviceKind::TestGain { gain: 1.0 });
    let track = project.track_mut(high).unwrap();
    track.sends.push(send);
    track.devices.push(gain);
    project
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

/// The lanes the tests draw on [`mix`], as the arguments of
/// [`automation::SET_POINTS`]: the low tone fading in and stepping down,
/// and panning across, the send swelling, the test gain ducking and the
/// master's volume coming down. All of them hold still from beat 2.
fn lanes(project: &Project) -> Vec<Args> {
    let (low, high) = (TrackId(0), TrackId(1));
    let track = project.track(high).unwrap();
    let (send, gain) = (track.sends[0].id, track.devices[0].id);
    let lane = |param: ParamAddress, points: &[(Beats, f64)]| {
        let (at, values): (Vec<Beats>, Vec<f64>) = points.iter().copied().unzip();
        Args::new()
            .with("param", param)
            .with("at", at)
            .with("values", values)
    };
    vec![
        lane(
            ParamAddress::Volume(Chain::Track(low)),
            &[(Beats::ZERO, 0.0), (beats(1, 1), 1.0), (beats(1, 1), 0.5)],
        ),
        lane(
            ParamAddress::Pan(Chain::Track(low)),
            &[(Beats::ZERO, -1.0), (beats(2, 1), 0.5)],
        ),
        lane(
            ParamAddress::Send(send),
            &[(Beats::ZERO, 0.0), (beats(2, 1), 1.0)],
        ),
        lane(
            ParamAddress::Device(gain, 0),
            &[(beats(1, 2), 1.0), (beats(3, 2), 0.25)],
        ),
        lane(
            ParamAddress::Volume(Chain::Master),
            &[(beats(1, 1), 1.0), (beats(2, 1), 0.5)],
        ),
    ]
}

/// A session on `project`, with [`lanes`] drawn in it as someone would with
/// the automation commands, and the engine it sends to.
fn drawn(project: Project) -> (Engine, Session) {
    let (engine, handle) = Engine::new(CONFIG);
    let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
    for args in lanes(session.project()) {
        session.run_with(&automation::SET_POINTS, &args).unwrap();
    }
    (engine, session)
}

/// What `project` plays from its start for [`SONG`] frames, with the
/// reference renderer.
fn played(project: &Project) -> Vec<f32> {
    let mut player = Player::new(project.clone(), None, &Render::Reference, CONFIG);
    let mut output = Vec::new();
    let mut done = 0;
    while done < SONG {
        let block = (SONG - done).min(CONFIG.max_block as u64) as usize;
        output.extend_from_slice(player.render(block));
        done += block as u64;
    }
    output
}

/// The peak of each channel `engine` plays from `at`, over the last half of
/// a fifth of a second, when it has settled.
fn peaks_from(engine: &mut Engine, session: &mut Session, at: Beats) -> [f32; 2] {
    session.run(&transport::STOP).unwrap();
    let locate = Args::new().with("at", at);
    session.run_with(&transport::LOCATE, &locate).unwrap();
    session.run(&transport::PLAY).unwrap();
    session.wait_for_graph();
    let frames = 9_600;
    let mut output = vec![0.0; 2 * frames];
    for block in output.chunks_mut(2 * 240) {
        engine.process_interleaved(block, 2);
        session.tick();
    }
    let mut peaks = [0.0_f32; 2];
    for frame in output[frames..].chunks(2) {
        for (peak, sample) in peaks.iter_mut().zip(frame) {
            *peak = peak.max(sample.abs());
        }
    }
    peaks
}

/// The same peaks, from a part of `output` that `played` played.
fn peaks_in(output: &[f32], at: Beats) -> [f32; 2] {
    // At 120 BPM.
    let start = 2 * (at.to_f64() * 24_000.0) as usize;
    let mut peaks = [0.0_f32; 2];
    for frame in output[start + 2 * 4_800..start + 2 * 9_600].chunks(2) {
        for (peak, sample) in peaks.iter_mut().zip(frame) {
            *peak = peak.max(sample.abs());
        }
    }
    peaks
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
fn automation_plays_the_same_with_every_renderer() {
    let (_engine, mut session) = drawn(mix());
    let project = session.saved_project();
    assert_eq!(project.automation.len(), 5);
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
    let differences = null_test::compare(&mut players, SONG);
    assert_eq!(differences, [None, None, None]);
    assert!(players[0].peak() > 0.05, "{}", players[0].peak());
}

#[test]
fn automation_survives_saving_opening_undo_and_redo() {
    let plain = mix();
    let (mut engine, mut session) = drawn(plain.clone());
    let automated = session.saved_project();
    let expected = played(&automated);
    let unautomated = played(&plain);
    assert_ne!(expected, unautomated, "the automation changes what plays");

    // Saved and opened again, it plays the same, to the bit.
    let dir = std::env::temp_dir().join(format!("musicable-app-automation-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("Automation.musicable");
    project::save(&path, &automated).unwrap();
    let opened = project::load(&path).unwrap();
    assert_eq!(opened.automation, automated.automation);
    assert_eq!(played(&opened), expected);

    // The session's own engine plays the lanes where they hold still.
    let still = beats(5, 2);
    let held = peaks_in(&expected, still);
    near(peaks_from(&mut engine, &mut session, still), held);

    // Undo takes the lanes away, one at a time, in the engine too, and redo
    // puts them back as they were.
    for _ in 0..5 {
        assert_eq!(session.undo_name(), Some("Edit Automation"));
        session.run(&edit::UNDO).unwrap();
    }
    assert!(session.project().automation.is_empty());
    assert_eq!(played(&session.saved_project()), unautomated);
    let without = peaks_in(&unautomated, still);
    near(peaks_from(&mut engine, &mut session, still), without);
    for _ in 0..5 {
        session.run(&edit::REDO).unwrap();
    }
    assert_eq!(played(&session.saved_project()), expected);
    near(peaks_from(&mut engine, &mut session, still), held);
    let _ = fs::remove_dir_all(&dir);
}

//! Musical time's done test (M3 in the Tier 0 plan): an offline render of a
//! looping region with the metronome on has its clicks on exactly the
//! expected frames, at several tempos, time signatures and sample rates.
//!
//! The expected frames are worked out here in integer arithmetic, apart from
//! the engine's floating-point tempo map.

use std::collections::BTreeMap;

use musicable_engine::{Engine, EngineConfig, Event, TransportCommand, compile, rt};
use musicable_model::Project;
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

/// Device callback sizes, in turn. Drivers don't always deliver the period
/// that was asked for.
const CALLBACKS: [usize; 5] = [441, 64, 970, 128, 7];

#[derive(Debug)]
struct Case {
    sample_rate: u32,
    /// Numerator and denominator, so frames can be worked out exactly.
    bpm: (u64, u64),
    signature: (u8, u8),
    loop_region: BeatRange,
}

impl Case {
    fn signature(&self) -> TimeSignature {
        TimeSignature::new(self.signature.0, self.signature.1).unwrap()
    }

    /// The frame `at` plays on: the one nearest to
    /// `ticks × 60 × rate / (ticks per beat × bpm)`, halfway rounding up.
    fn frame(&self, at: Beats) -> u64 {
        let (numerator, denominator) = self.bpm;
        let ticks = u128::try_from(at.ticks()).unwrap();
        let n = ticks * 60 * u128::from(self.sample_rate) * u128::from(denominator);
        let d = Beats::TICKS_PER_BEAT as u128 * u128::from(numerator);
        ((2 * n + d) / (2 * d)) as u64
    }

    /// Enough frames to go round the loop three times.
    fn frames(&self) -> usize {
        let (start, end) = (
            self.frame(self.loop_region.start),
            self.frame(self.loop_region.end),
        );
        (end + 2 * (end - start)) as usize
    }
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

/// Where the clicks should be, in output frames, and which are downbeats.
fn expected_clicks(case: &Case) -> Vec<(usize, bool)> {
    let numerator = u32::from(case.signature.0);
    // Every beat of the signature up to the loop end, by the frame it falls
    // on.
    let mut clicks = BTreeMap::new();
    let mut at = Beats::ZERO;
    for beat in 0.. {
        if at >= case.loop_region.end {
            break;
        }
        clicks.insert(case.frame(at), beat % numerator == 0);
        at += case.signature().beat_length();
    }
    // Follow the playhead from the start of the song, round the loop.
    let (start, end) = (
        case.frame(case.loop_region.start),
        case.frame(case.loop_region.end),
    );
    let mut position = 0;
    let mut expected = Vec::new();
    for frame in 0..case.frames() {
        if position == end {
            position = start;
        }
        if let Some(&downbeat) = clicks.get(&position) {
            expected.push((frame, downbeat));
        }
        position += 1;
    }
    expected
}

/// Plays the case's project offline and finds the clicks: they start out of
/// silence, and downbeats start louder.
fn rendered_clicks(case: &Case) -> Vec<(usize, bool)> {
    let mut project = Project::new();
    let (numerator, denominator) = case.bpm;
    project.tempo = TempoMap::new(numerator as f64 / denominator as f64, case.signature());
    project.loop_region = case.loop_region;
    project.looping = true;
    project.metronome = true;

    let config = EngineConfig {
        sample_rate: case.sample_rate,
        max_block: 256,
    };
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(compile(&project).prepare(&config).unwrap())
        .unwrap();
    handle.set_tempo_map(project.tempo.clone()).unwrap();
    let region = project.looping.then_some(project.loop_region);
    for command in [TransportCommand::Loop(region), TransportCommand::Play] {
        handle.send(Event::Transport(command)).unwrap();
    }

    let frames = case.frames();
    let mut out = vec![0.0; 2 * frames];
    let mut done = 0;
    for &size in CALLBACKS.iter().cycle() {
        let size = size.min(frames - done);
        engine.process_interleaved(&mut out[2 * done..2 * (done + size)], 2);
        done += size;
        if done == frames {
            break;
        }
    }
    assert_eq!(handle.stats().events_late, 0);

    let left: Vec<f32> = out.into_iter().step_by(2).collect();
    let starts: Vec<(usize, f32)> = (0..frames)
        .filter(|&i| left[i] != 0.0 && (i == 0 || left[i - 1] == 0.0))
        .map(|i| (i, left[i]))
        .collect();
    let loudest = starts.iter().map(|&(_, s)| s).fold(0.0, f32::max);
    starts.into_iter().map(|(i, s)| (i, s == loudest)).collect()
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn clicks_land_on_their_frames_while_looping() {
    let cases = [
        // A whole number of frames per beat.
        Case {
            sample_rate: 48_000,
            bpm: (120, 1),
            signature: (4, 4),
            loop_region: BeatRange::new(Beats::new(4), Beats::new(12)),
        },
        // 20,671.875 frames per beat, so some beats fall halfway between
        // frames.
        Case {
            sample_rate: 44_100,
            bpm: (128, 1),
            signature: (6, 8),
            loop_region: BeatRange::new(Beats::new(3), Beats::new(9)),
        },
        Case {
            sample_rate: 44_100,
            bpm: (133, 1),
            signature: (3, 4),
            loop_region: BeatRange::new(Beats::new(3), Beats::new(9)),
        },
        Case {
            sample_rate: 96_000,
            bpm: (175, 2),
            signature: (7, 8),
            loop_region: BeatRange::new(Beats::ZERO, Beats::new(7)),
        },
        Case {
            sample_rate: 192_000,
            bpm: (174, 1),
            signature: (5, 4),
            loop_region: BeatRange::new(Beats::new(5), Beats::new(10)),
        },
        // A loop that starts and ends between beats.
        Case {
            sample_rate: 48_000,
            bpm: (100, 1),
            signature: (4, 4),
            loop_region: BeatRange::new(beats(3, 2), beats(11, 2)),
        },
    ];
    for case in &cases {
        let expected = expected_clicks(case);
        let bars = expected.iter().filter(|&&(_, downbeat)| downbeat).count();
        assert!(bars >= 4, "{case:?} should go round the loop");
        assert_eq!(rendered_clicks(case), expected, "{case:?}");
    }
}

#[test]
fn clicks_follow_tempo_changes() {
    // 120 BPM for two bars, then 90: 24,000 frames to the beat, then 32,000.
    let mut project = Project::new();
    project.tempo.insert_tempo(Beats::new(8), 90.0);
    project.metronome = true;
    let config = EngineConfig {
        sample_rate: 48_000,
        max_block: 512,
    };
    let (mut engine, mut handle) = Engine::new(config);
    handle
        .swap_graph(compile(&project).prepare(&config).unwrap())
        .unwrap();
    handle.set_tempo_map(project.tempo.clone()).unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut out = vec![0.0; 2 * 330_000];
    for part in out.chunks_mut(2 * 500) {
        engine.process_interleaved(part, 2);
    }
    let left: Vec<f32> = out.into_iter().step_by(2).collect();
    let starts: Vec<usize> = (0..left.len())
        .filter(|&i| left[i] != 0.0 && (i == 0 || left[i - 1] == 0.0))
        .collect();
    let expected: Vec<usize> = (0..=8)
        .map(|beat| beat * 24_000)
        .chain((1..=4).map(|beat| 192_000 + beat * 32_000))
        .collect();
    assert_eq!(starts, expected);
}

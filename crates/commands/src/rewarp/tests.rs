use std::sync::Arc;

use musicable_analysis::{Analysis, Onset, Steady, Tracker};
use musicable_model::clip::{AudioClip, ClipContent, ClipId, Offset};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{Warp, WarpMarker};
use musicable_model::{Project, TrackKind};

use super::*;
use crate::editor::AudioFile;
use crate::testing::Setup;
use crate::{edit, warp as warping};

const RATE: u32 = 48_000;
/// Frames to the beat at 100 BPM.
const AT_100: u64 = 28_800;

/// What analysis finds in a file `frames` long with a beat on each of
/// `beats`, at `bpm` and drifting by `drift_ms`: a pulse the trackers
/// agree on, with a downbeat on every fourth from the first.
fn pulse(frames: u64, beats: Vec<u64>, bpm: f64, drift_ms: f64) -> Analysis {
    let onsets = (beats.iter())
        .map(|&frame| Onset {
            frame,
            strength: 1.0,
            percussiveness: 1.0,
        })
        .collect();
    let steady = Steady {
        bpm,
        origin: beats[0] as f64,
        error_ms: 0.0,
        drift_ms,
    };
    Analysis {
        downbeats: (0..beats.len()).step_by(4).collect(),
        tracker: Tracker::Network,
        agreement: Some(1.0),
        steady: Some(steady),
        events: beats.len(),
        pulse: true,
        beats,
        ..Analysis::with_onsets(RATE, frames, onsets)
    }
}

/// Sixteen beats at 100 BPM from the first frame, and how many frames.
fn steady_file() -> (u64, Analysis) {
    let beats = (0..16).map(|k| k * AT_100).collect();
    (16 * AT_100, pulse(16 * AT_100, beats, 100.0, 0.0))
}

/// A 120 BPM song with Drums, an audio track whose clip, Song, plays
/// "Song.wav", `frames` long, warped by `warp`, from beat `start` of the
/// song for `length` beats, from beat `offset` of its own time, in the
/// Sample Editor. The editor has `analysis` for the file, if it's given.
fn song(
    frames: u64,
    analysis: Option<Analysis>,
    warp: Warp,
    start: f64,
    length: f64,
    offset: f64,
) -> (Setup, ClipId) {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let mut audio = AudioClip::new("Song.wav");
    audio.offset = Offset::Beats(Beats::from_f64(offset));
    audio.warp = Some(warp);
    let start = Beats::from_f64(start);
    let range = BeatRange::new(start, start + Beats::from_f64(length));
    let clip = project.new_clip("Song", range, ClipContent::Audio(audio));
    let id = clip.id;
    project.track_mut(drums).unwrap().clips.push(clip);
    let mut setup = Setup::new(project);
    let editor = setup.document.editor_mut();
    let file = AudioFile {
        sample_rate: RATE,
        frames,
        channels: 2,
        tempo: None,
    };
    editor.files.insert("Song.wav".into(), file);
    if let Some(analysis) = analysis {
        editor
            .analyses
            .insert("Song.wav".into(), Arc::new(analysis));
    }
    let args = Args::new().with("clip", id);
    setup.run_with(&warping::OPEN, args).unwrap();
    (setup, id)
}

fn audio_of(setup: &Setup, clip: ClipId) -> (Clip, AudioClip) {
    let (c, a) = warping::audio(setup.document.project(), clip).unwrap();
    (c.clone(), a.clone())
}

/// The clip's markers, as (frame, beat), and its warp's tempo.
fn markers(setup: &Setup, clip: ClipId) -> (Vec<(u64, f64)>, f64) {
    let warp = audio_of(setup, clip).1.warp.unwrap();
    let markers = warp.markers.iter().map(|m| (m.frame, m.beat.to_f64()));
    (markers.collect(), warp.tempo)
}

/// The song position where the file's `frame` plays.
fn plays(setup: &Setup, clip: ClipId, frame: u64) -> f64 {
    let (c, a) = audio_of(setup, clip);
    let own = a.warp.as_ref().unwrap().beat_at(frame as f64, RATE);
    c.start.to_f64() + own - a.offset.beats().unwrap().to_f64()
}

/// The frames of the file the clip plays from and to.
fn played(setup: &Setup, clip: ClipId) -> (f64, f64) {
    let (c, a) = audio_of(setup, clip);
    let warp = a.warp.as_ref().unwrap();
    let offset = a.offset.beats().unwrap();
    let frame = |own: Beats| warp.frame_at(own.to_f64(), RATE);
    (frame(offset), frame(offset + c.length))
}

fn at_frame(id: &CommandId, clip: ClipId, frame: u64) -> (CommandId, Args) {
    let args = Args::new().with("clip", clip).with("frame", frame as i64);
    (id.clone(), args)
}

fn run(setup: &mut Setup, (id, args): (CommandId, Args)) -> Result<Value, CommandError> {
    setup.run_with(&id, args)
}

#[test]
fn set_1_1_1_here_puts_the_downbeat_on_the_nearest_bar_line() {
    // Warped as if the first beat were the downbeat, from beat 4, where it
    // plays; the downbeat is the next.
    let (frames, analysis) = steady_file();
    let warp = Warp::new(0, Beats::ZERO, 100.0);
    let (mut setup, clip) = song(frames, Some(analysis), warp, 4.0, 16.0, 0.0);
    let select = Args::new().with("markers", vec![Beats::ZERO]);
    setup.run_with(&warping::SELECT_MARKERS, select).unwrap();
    assert_eq!(plays(&setup, clip, AT_100), 5.0);
    run(&mut setup, at_frame(&SET_DOWNBEAT, clip, AT_100)).unwrap();
    // It's beat 0 of the clip's own time now, and plays on beat 4, the bar
    // line nearest: the clip moved a beat earlier with its audio.
    assert_eq!(markers(&setup, clip), (vec![(0, -1.0)], 100.0));
    assert_eq!(plays(&setup, clip, AT_100), 4.0);
    let (c, a) = audio_of(&setup, clip);
    assert_eq!((c.start, c.length), (Beats::new(3), Beats::new(16)));
    assert_eq!(a.offset, Offset::Beats(Beats::new(-1)));
    // The marker stays selected, at its new beat.
    let shown = &setup.document.editor().sample_editor;
    assert_eq!(
        shown.markers.iter().copied().collect::<Vec<_>>(),
        [Beats::new(-1)]
    );
    assert_eq!(setup.committed_names(), ["Set 1.1.1 Here"]);
    setup.run(&edit::UNDO);
    assert_eq!(markers(&setup, clip), (vec![(0, 0.0)], 100.0));
    assert_eq!(audio_of(&setup, clip).0.start, Beats::new(4));
}

#[test]
fn set_1_1_1_here_at_the_song_s_start_cuts_the_audio_before_it() {
    let (frames, analysis) = steady_file();
    let warp = Warp::new(0, Beats::ZERO, 100.0);
    let (mut setup, clip) = song(frames, Some(analysis), warp, 0.0, 16.0, 0.0);
    run(&mut setup, at_frame(&SET_DOWNBEAT, clip, AT_100)).unwrap();
    assert_eq!(plays(&setup, clip, AT_100), 0.0);
    let (c, a) = audio_of(&setup, clip);
    assert_eq!((c.start, c.length), (Beats::ZERO, Beats::new(15)));
    assert_eq!(a.offset, Offset::Beats(Beats::ZERO));
    // It plays from the downbeat, to where it did.
    assert_eq!(played(&setup, clip), (AT_100 as f64, 16.0 * AT_100 as f64));
}

/// A drifting file: 17 beats speeding up from 100 BPM, by 0.6% a beat,
/// with its seventh beat 20 ms late, and where each beat is.
fn drifting_file() -> (u64, Analysis, Vec<u64>) {
    let mut beats = Vec::new();
    let mut at = 0.0;
    for k in 0..17 {
        beats.push(at as u64);
        at += AT_100 as f64 * (1.0 - 0.006 * f64::from(k));
    }
    beats[6] += 960;
    let frames = at as u64 + AT_100;
    let span = (beats[16] - beats[0]) as f64 / 16.0;
    let analysis = pulse(frames, beats.clone(), 60.0 * f64::from(RATE) / span, 30.0);
    (frames, analysis, beats)
}

#[test]
fn warp_from_here_warps_the_rest_by_analysis_keeping_the_markers_before() {
    let (frames, analysis, beats) = drifting_file();
    let warp = Warp {
        markers: vec![
            WarpMarker {
                frame: 0,
                beat: Beats::ZERO,
            },
            WarpMarker {
                frame: beats[12],
                beat: Beats::new(13),
            },
        ],
        ..Warp::new(0, Beats::ZERO, 100.0)
    };
    let (mut setup, clip) = song(frames, Some(analysis), warp, 4.0, 16.0, 0.0);
    let before = played(&setup, clip);
    // From beat 8, where there's no marker: one pins it where it plays.
    let here = Beats::from_f64(beats[8] as f64 * 13.0 / beats[12] as f64);
    run(&mut setup, at_frame(&FROM_HERE, clip, beats[8])).unwrap();
    // After it, a marker on each downbeat, as nothing between strays.
    let beat = |k: f64| here.to_f64() + k;
    let (markers, _) = markers(&setup, clip);
    let expected = [
        (0, 0.0),
        (beats[8], beat(0.0)),
        (beats[12], beat(4.0)),
        (beats[16], beat(8.0)),
    ];
    assert_eq!(markers.len(), expected.len(), "{markers:?}");
    for (marker, expected) in markers.iter().zip(expected) {
        assert_eq!(marker.0, expected.0);
        assert!((marker.1 - expected.1).abs() < 1e-5, "{markers:?}");
    }
    // It plays the same stretch of its file.
    let after = played(&setup, clip);
    assert!((after.0 - before.0).abs() < 0.5 && (after.1 - before.1).abs() < 0.5);
    assert_eq!(setup.committed_names(), ["Warp From Here"]);
}

#[test]
fn warp_from_here_straight_or_at_the_song_s_tempo_plays_the_rest_at_one_tempo() {
    let (frames, analysis) = steady_file();
    // Warped wrong after beat 4.
    let warp = Warp {
        markers: vec![
            WarpMarker {
                frame: 0,
                beat: Beats::ZERO,
            },
            WarpMarker {
                frame: 4 * AT_100,
                beat: Beats::new(4),
            },
            WarpMarker {
                frame: 8 * AT_100,
                beat: Beats::from_f64(8.5),
            },
        ],
        ..Warp::new(0, Beats::ZERO, 100.0)
    };
    let (mut setup, clip) = song(frames, Some(analysis.clone()), warp.clone(), 0.0, 16.5, 0.0);
    let before = played(&setup, clip);
    // At the tempo analysis found, with a marker at the file's end.
    run(&mut setup, at_frame(&FROM_HERE_STRAIGHT, clip, 4 * AT_100)).unwrap();
    let straight = vec![(0, 0.0), (4 * AT_100, 4.0), (frames, 16.0)];
    assert_eq!(markers(&setup, clip).0, straight);
    // Warp From Here does the same for a file at a steady tempo.
    setup.run(&edit::UNDO);
    run(&mut setup, at_frame(&FROM_HERE, clip, 4 * AT_100)).unwrap();
    assert_eq!(markers(&setup, clip).0, straight);
    // At the song's 120 BPM, the file's own speed: 14.4 beats for the 12.
    setup.run(&edit::UNDO);
    run(
        &mut setup,
        at_frame(&SONG_TEMPO_FROM_HERE, clip, 4 * AT_100),
    )
    .unwrap();
    let (at_120, _) = markers(&setup, clip);
    assert_eq!(at_120[..2], [(0, 0.0), (4 * AT_100, 4.0)]);
    assert_eq!(at_120[2].0, frames);
    assert!((at_120[2].1 - 18.4).abs() < 1e-5, "{at_120:?}");
    // The clip plays the same stretch of its file.
    let after = played(&setup, clip);
    assert!((after.0 - before.0).abs() < 0.5 && (after.1 - before.1).abs() < 0.5);

    // Without analysis, Straight keeps the clip's tempo there, and Warp
    // From Here can't.
    let (mut setup, clip) = song(frames, None, warp, 0.0, 16.5, 0.0);
    let error = run(&mut setup, at_frame(&FROM_HERE, clip, 4 * AT_100)).unwrap_err();
    assert!(
        error.to_string().contains("hasn't been analysed"),
        "{error}"
    );
    run(&mut setup, at_frame(&FROM_HERE_STRAIGHT, clip, 4 * AT_100)).unwrap();
    assert_eq!(
        markers(&setup, clip).0,
        [(0, 0.0), (4 * AT_100, 4.0), (frames, 16.0)]
    );
}

#[test]
fn warp_from_here_reads_the_tempo_at_the_clip_s_octave_or_the_song_s() {
    // At 200 BPM, the file's beats are two of the clip's.
    let (frames, analysis) = steady_file();
    let warp = Warp::new(0, Beats::ZERO, 200.0);
    let (mut setup, clip) = song(frames, Some(analysis), warp, 0.0, 32.0, 0.0);
    run(&mut setup, at_frame(&FROM_HERE, clip, 0)).unwrap();
    assert_eq!(markers(&setup, clip), (vec![(0, 0.0)], 200.0));
    // Nearest the song's 120, it's 100.
    run(&mut setup, at_frame(&FROM_HERE_AT_SONG_TEMPO, clip, 0)).unwrap();
    assert_eq!(markers(&setup, clip), (vec![(0, 0.0)], 100.0));
    // The clip plays all of the file, as it did.
    assert_eq!(audio_of(&setup, clip).0.length, Beats::new(16));
}

#[test]
fn warp_selection_as_a_loop_fits_it_into_whole_bars() {
    // Sixteen beats at 100 BPM, warped at 110, so 17.6 beats long, from
    // beat 1.
    let (frames, analysis) = steady_file();
    let warp = Warp::new(0, Beats::ZERO, 110.0);
    let (mut setup, clip) = song(frames, Some(analysis), warp, 1.0, 17.6, 0.0);
    assert!(!setup.applies(&SELECTION_AS_LOOP), "no time is selected");
    let select = Args::new()
        .with("start", Beats::ZERO)
        .with("end", Beats::from_f64(17.6));
    setup.run_with(&warping::SELECT_TIME, select).unwrap();
    assert!(setup.applies(&SELECTION_AS_LOOP));
    setup.run(&SELECTION_AS_LOOP);
    // Four bars from the bar line nearest, at the tempo that gives.
    let (looped, tempo) = markers(&setup, clip);
    assert_eq!(looped, [(0, 0.0)]);
    assert!((tempo - 100.0).abs() < 1e-9, "{tempo}");
    let (c, a) = audio_of(&setup, clip);
    assert_eq!((c.start, c.length), (Beats::ZERO, Beats::new(16)));
    assert_eq!(a.offset, Offset::Beats(Beats::ZERO));
    // The loop is selected.
    let shown = &setup.document.editor().sample_editor;
    assert_eq!(
        shown.time,
        Some(BeatRange::new(Beats::ZERO, Beats::new(16)))
    );
    // A stretch given, a bar and a bit of it, is one bar.
    let range = Args::new()
        .with("clip", clip)
        .with("start", Beats::new(4))
        .with("end", Beats::from_f64(9.0));
    setup.run_with(&SELECTION_AS_LOOP, range).unwrap();
    let (c, _) = audio_of(&setup, clip);
    assert_eq!((c.start, c.length), (Beats::new(4), Beats::new(4)));
    assert_eq!(markers(&setup, clip).0, [(4 * AT_100, 0.0)]);
    assert!((markers(&setup, clip).1 - 80.0).abs() < 1e-9);
}

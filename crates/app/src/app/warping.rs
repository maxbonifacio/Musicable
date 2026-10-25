//! Warping in the app's window: warped clips taking the transients analysis
//! finds in their files, files auto-warped as their analyses land, and
//! clips in Granular taking the pitch marks it finds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use musicable_analysis::{Analysis, PitchMarks, PitchRun};
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent, Offset};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{Warp, WarpMode};
use musicable_model::{Project, TrackKind};

use super::App;
use super::testing::{TempDir, window};
use crate::analyses::Analyses;

/// A frame rate, and the frames of a beat at 120 BPM.
const RATE: u32 = 48_000;
const BEAT: u64 = 24_000;

/// A click on each of `beats` beats at 120 BPM, from the first frame.
fn clicks(beats: u64) -> Vec<f32> {
    clicks_apart(beats, BEAT)
}

/// A click on each of `beats` beats, `apart` frames apart, from the first
/// frame.
fn clicks_apart(beats: u64, apart: u64) -> Vec<f32> {
    let mut samples = vec![0.0; (beats * apart) as usize];
    for beat in 0..beats {
        let click = samples[(beat * apart) as usize..].iter_mut().take(400);
        for (i, sample) in click.enumerate() {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            *sample = 0.8 * (-(i as f32) / 60.0).exp() * sign;
        }
    }
    samples
}

/// A second of a tone with a pitch, of 150 Hz: 320 frames a period.
fn tone() -> Vec<f32> {
    (0..RATE)
        .map(|i| {
            let phase = std::f64::consts::TAU * 150.0 * f64::from(i) / f64::from(RATE);
            let harmonics = [1.0, 0.5, 0.25].into_iter().enumerate();
            let sum: f64 = harmonics
                .map(|(h, a)| a * (phase * (h + 1) as f64).sin())
                .sum();
            (0.4 * sum) as f32
        })
        .collect()
}

/// The frames of the transients of the song's warped clip, once it has
/// taken them.
fn transients(harness: &Harness<'static, App>) -> Option<Vec<u64>> {
    let track = &harness.state().session.project().tracks[0];
    let ClipContent::Audio(audio) = &track.clips[0].content else {
        panic!("the clip is an audio clip");
    };
    let warp = audio.warp.as_ref()?;
    warp.onsets.as_ref()?;
    Some(warp.transients().map(|o| o.frame).collect())
}

#[test]
fn warped_clips_take_the_transients_analysis_finds_in_their_files() {
    let config = TempDir::new("warping-config");
    let folder = TempDir::new("warping-song");
    let file = folder.0.join("Clicks.wav");
    save_wav(&file, RATE, SampleFormat::Float32, &[&clicks(8)]).unwrap();
    // Warped by a Musicable from before transients, so it has none yet.
    let mut song = Project::new();
    let track = song.add_track("Drums", TrackKind::Audio);
    let mut audio = AudioClip::new("Clicks.wav");
    audio.offset = Offset::Beats(Beats::ZERO);
    audio.warp = Some(Warp::new(0, Beats::ZERO, 120.0));
    let range = BeatRange::new(Beats::ZERO, Beats::new(8));
    let clip = song.new_clip("Clicks", range, ClipContent::Audio(audio));
    song.track_mut(track).unwrap().clips.push(clip);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));
    // Analysing as the app does, on the library's queue, without the beat
    // tracker or a cache.
    let queue = harness.state().session.library().queue().cloned();
    let queue = queue.expect("the library has a queue");
    harness.state_mut().analyses = Analyses::new(queue, None, None, None);
    harness.state_mut().files_found = None;
    let deadline = Instant::now() + Duration::from_secs(60);
    while transients(&harness).is_none() {
        assert!(Instant::now() < deadline, "Clicks.wav wasn't analysed");
        std::thread::sleep(Duration::from_millis(5));
        harness.run();
    }
    // One at each click, to within a couple of milliseconds.
    let frames = transients(&harness).unwrap();
    assert_eq!(frames.len(), 8, "{frames:?}");
    for (beat, &frame) in (0..).zip(&frames) {
        assert!(frame.abs_diff(beat * BEAT) < 100, "{frames:?}");
    }
    // It's a change the project follows, not one to undo or save.
    let session = &harness.state().session;
    assert_eq!(session.undo_name(), None);
    assert!(!session.is_modified());
}

#[test]
fn a_dropped_loop_warps_to_its_bars_once_its_analysis_lands() {
    let config = TempDir::new("auto-warp-config");
    let folder = TempDir::new("auto-warp-files");
    // Two bars at 100 BPM, 28,800 frames to the beat.
    let file = folder.0.join("Loop.wav");
    save_wav(
        &file,
        RATE,
        SampleFormat::Float32,
        &[&clicks_apart(8, 28_800)],
    )
    .unwrap();
    let mut harness = window(&config.0, None);
    let queue = harness.state().session.library().queue().cloned();
    let queue = queue.expect("the library has a queue");
    harness.state_mut().analyses = Analyses::new(queue, None, None, None);
    harness.run();
    (harness.state_mut()).insert_audio(vec![file], None, Some(Beats::ZERO));
    // It comes in at once, unwarped: 9.6 beats of the song's 120 BPM.
    let clip = |harness: &Harness<'static, App>| {
        let project = harness.state().session.project();
        let track = project.tracks.iter().find(|t| !t.clips.is_empty());
        track.expect("a track has the clip").clips[0].clone()
    };
    assert_eq!(clip(&harness).end(), Beats::from_f64(9.6));
    let warped = |harness: &Harness<'static, App>| match clip(harness).content {
        ClipContent::Audio(audio) => audio.warp,
        ClipContent::Midi(_) => panic!("an audio clip"),
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while warped(&harness).is_none() {
        assert!(Instant::now() < deadline, "Loop.wav wasn't auto-warped");
        std::thread::sleep(Duration::from_millis(5));
        harness.run();
    }
    // The song was empty, so it takes the loop's tempo, and the loop its
    // eight beats, as part of the step that dropped it.
    let session = &harness.state().session;
    assert!((session.project().tempo.bpm_at(Beats::ZERO) - 100.0).abs() < 0.01);
    let range = clip(&harness).range();
    assert_eq!(range, BeatRange::new(Beats::ZERO, Beats::new(8)));
    assert_eq!(session.undo_name(), Some("Insert Audio"));
}

#[test]
fn granular_clips_that_follow_the_pitch_take_the_pitch_marks_analysis_finds() {
    let config = TempDir::new("granular-config");
    let folder = TempDir::new("granular-song");
    for name in ["Tone.wav", "Pad.wav"] {
        save_wav(
            &folder.0.join(name),
            RATE,
            SampleFormat::Float32,
            &[&tone()],
        )
        .unwrap();
    }
    // In Granular, the tone following its pitch, and the pad not.
    let mut song = Project::new();
    let track = song.add_track("Granular", TrackKind::Audio);
    for (beat, file, follow_pitch) in [(0, "Tone.wav", true), (2, "Pad.wav", false)] {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(Beats::ZERO);
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        warp.mode = WarpMode::Granular;
        warp.granular.follow_pitch = follow_pitch;
        audio.warp = Some(warp);
        let range = BeatRange::new(Beats::new(beat), Beats::new(beat + 2));
        let clip = song.new_clip(file, range, ClipContent::Audio(audio));
        song.track_mut(track).unwrap().clips.push(clip);
    }
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();

    // Until the tone's marks are in, its grains are on the plain lattice;
    // once they are, a new graph fits them to the marks. The pad's grains
    // don't follow them, so its marks need no new graph.
    let mut harness = window(&config.0, Some(path.clone()));
    let analysis = |pitch: PitchMarks| {
        let mut analysis = Analysis::with_onsets(RATE, RATE.into(), Vec::new());
        analysis.pitch = pitch;
        Arc::new(analysis)
    };
    let marks = PitchMarks {
        runs: vec![PitchRun {
            start: 0,
            periods: vec![320 * 64; 140],
        }],
    };
    let session = &mut harness.state_mut().session;
    let analyses = &mut session.editor_mut().analyses;
    analyses.insert("Pad.wav".into(), analysis(marks.clone()));
    assert!(!session.take_pitch_marks());
    let analyses = &mut session.editor_mut().analyses;
    analyses.insert("Tone.wav".into(), analysis(marks));
    assert!(session.take_pitch_marks());
    assert!(!session.take_pitch_marks(), "they're kept already");
    drop(harness);

    // Opened again and analysed as the app does, the tone's marks reach
    // the graphs as they land.
    let mut harness = window(&config.0, Some(path));
    let queue = harness.state().session.library().queue().cloned();
    let queue = queue.expect("the library has a queue");
    harness.state_mut().analyses = Analyses::new(queue, None, None, None);
    harness.state_mut().files_found = None;
    let analysed = |harness: &Harness<'static, App>| {
        let analyses = &harness.state().session.editor().analyses;
        analyses.contains_key("Tone.wav") && analyses.contains_key("Pad.wav")
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !analysed(&harness) {
        assert!(Instant::now() < deadline, "the files weren't analysed");
        std::thread::sleep(Duration::from_millis(5));
        harness.run();
    }
    let runs = &harness.state().session.editor().analyses["Tone.wav"]
        .pitch
        .runs;
    assert_eq!(runs.len(), 1, "one run with a pitch");
    assert!(!harness.state_mut().session.take_pitch_marks());
}

//! Warping in the app's window: warped clips taking the transients analysis
//! finds in their files.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent, Offset};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::Warp;
use musicable_model::{Project, TrackKind};

use super::App;
use super::testing::{TempDir, window};
use crate::analyses::Analyses;

/// A frame rate, and the frames of a beat at 120 BPM.
const RATE: u32 = 48_000;
const BEAT: u64 = 24_000;

/// A click on each of `beats` beats at 120 BPM, from the first frame.
fn clicks(beats: u64) -> Vec<f32> {
    let mut samples = vec![0.0; (beats * BEAT) as usize];
    for beat in 0..beats {
        let click = samples[(beat * BEAT) as usize..].iter_mut().take(400);
        for (i, sample) in click.enumerate() {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            *sample = 0.8 * (-(i as f32) / 60.0).exp() * sign;
        }
    }
    samples
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

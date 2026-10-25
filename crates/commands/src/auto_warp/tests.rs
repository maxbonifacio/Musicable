use musicable_analysis::{Analysis, Loop, Onset, Steady, Tracker};
use musicable_model::clip::{AudioClip, Clip, ClipContent, ClipId, Offset};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::{Project, TrackId, TrackKind};

use super::*;
use crate::arrange::INSERT_AUDIO;
use crate::editor::AudioFile;
use crate::testing::Setup;
use crate::{Args, edit, warp};

const RATE: u32 = 48_000;
/// Frames to the beat at 100 BPM.
const AT_100: u64 = 28_800;

/// What analysis finds in a file `frames` long with a beat on each of
/// `beats`, steady at `bpm`: a pulse the network and the classic tracker
/// agree on, with a downbeat on every fourth from beat `downbeat`.
fn pulse(frames: u64, beats: Vec<u64>, bpm: f64, downbeat: usize) -> Analysis {
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
        drift_ms: 0.0,
    };
    Analysis {
        downbeats: (downbeat..beats.len()).step_by(4).collect(),
        tracker: Tracker::Network,
        agreement: Some(1.0),
        steady: Some(steady),
        events: beats.len(),
        pulse: true,
        beats,
        ..Analysis::with_onsets(RATE, frames, onsets)
    }
}

/// `count` beats, `apart` frames apart, from frame `from`.
fn steady_beats(from: u64, count: u64, apart: u64) -> Vec<u64> {
    (0..count).map(|k| from + k * apart).collect()
}

/// A loop of `bars` bars of `meter` beats at `bpm`, which must give whole
/// frames to the beat at 48 kHz, and how many frames it lasts.
fn a_loop(bars: u64, meter: u32, bpm: f64) -> (u64, Analysis) {
    let apart = (60.0 * f64::from(RATE) / bpm) as u64;
    let frames = bars * u64::from(meter) * apart;
    let beats = steady_beats(0, bars * u64::from(meter), apart);
    let mut analysis = pulse(frames, beats, bpm, 0);
    analysis.meter = meter;
    analysis.looped = Some(Loop {
        bars: bars as u32,
        bpm,
    });
    (frames, analysis)
}

/// A song at 100 BPM, steady, with a pickup: 33 beats from frame 1,000,
/// the second the first downbeat. It isn't a loop.
fn with_pickup() -> (u64, Analysis) {
    let beats = steady_beats(1_000, 33, AT_100);
    let frames = 1_000 + 33 * AT_100;
    (frames, pulse(frames, beats, 100.0, 1))
}

/// A 120 BPM song with Drums, an audio track, and Keys, a MIDI track whose
/// clip from beat 0 to 4 keeps the song from being empty, unless `empty`.
fn song(empty: bool) -> (Setup, TrackId) {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    if !empty {
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let content = ClipContent::Midi(Default::default());
        let clip = project.new_clip("Chords", range, content);
        project.track_mut(keys).unwrap().clips.push(clip);
    }
    (Setup::new(project), drums)
}

/// Gives the editor the header of `name`, a 48 kHz file `frames` long,
/// and what analysis found in it, if anything.
fn file(setup: &mut Setup, name: &str, frames: u64, analysis: Option<Analysis>) {
    let editor = setup.document.editor_mut();
    let header = AudioFile {
        sample_rate: RATE,
        frames,
        channels: 2,
        tempo: None,
    };
    editor.files.insert(name.into(), header);
    if let Some(analysis) = analysis {
        editor.analyses.insert(name.into(), Arc::new(analysis));
    }
}

/// Drops `files` on `track` at beat `at`, and returns the new clips on the
/// track, in order.
fn drop(setup: &mut Setup, track: TrackId, at: f64, files: &[&str]) -> Vec<ClipId> {
    let ids = |setup: &Setup| -> Vec<ClipId> {
        let clips = &setup.document.project().track(track).unwrap().clips;
        clips.iter().map(|c| c.id).collect()
    };
    let before = ids(setup);
    let files: Vec<String> = files.iter().map(|&f| f.to_owned()).collect();
    let args = Args::new()
        .with("files", files)
        .with("track", track)
        .with("at", Beats::from_f64(at));
    setup.run_with(&INSERT_AUDIO, args).unwrap();
    ids(setup)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect()
}

fn clip(setup: &Setup, id: ClipId) -> (&Clip, &AudioClip) {
    warp::audio(setup.document.project(), id).unwrap()
}

/// The clip's warp markers, as frames and beats of its own time, and its
/// tempo, if it's warped.
fn markers(setup: &Setup, id: ClipId) -> Option<(Vec<(u64, f64)>, f64)> {
    let warp = clip(setup, id).1.warp.as_ref()?;
    let markers = warp.markers.iter().map(|m| (m.frame, m.beat.to_f64()));
    Some((markers.collect(), warp.tempo))
}

/// Where the clip starts and ends in the song, in beats.
fn span(setup: &Setup, id: ClipId) -> (f64, f64) {
    let (c, _) = clip(setup, id);
    (c.start.to_f64(), c.end().to_f64())
}

/// The song position where the file's `frame` plays, in beats, for a
/// warped clip.
fn plays(setup: &Setup, id: ClipId, frame: u64) -> f64 {
    let (c, a) = clip(setup, id);
    let own = a.warp.as_ref().unwrap().beat_at(frame as f64, RATE);
    c.start.to_f64() + own - a.offset.beats().unwrap().to_f64()
}

fn steps(setup: &Setup) -> Vec<String> {
    let steps = setup.document.steps();
    (0..steps.len())
        .filter_map(|i| steps.name(i).map(str::to_owned))
        .collect()
}

#[test]
fn analysis_says_what_kind_of_file_it_is_when_it_s_sure() {
    let (_, looped) = a_loop(4, 4, 100.0);
    assert_eq!(kind(&looped), Some(Kind::Loop));
    let (_, steady) = with_pickup();
    assert_eq!(kind(&steady), Some(Kind::Steady));
    let mut drifting = steady.clone();
    drifting.steady.as_mut().unwrap().drift_ms = 30.0;
    assert_eq!(kind(&drifting), Some(Kind::Drifting));
    let one_shot = Analysis::with_onsets(RATE, 48_000, Vec::new());
    assert_eq!(kind(&one_shot), Some(Kind::OneShot));
    // Unsure where the trackers disagree, or the beat slips.
    let mut unsure = steady.clone();
    unsure.agreement = Some(0.5);
    assert_eq!(kind(&unsure), None);
    let mut slips = steady.clone();
    slips.breaks = 1;
    assert_eq!(kind(&slips), None);
    // Fast analysis has nothing to check against.
    let mut fast = steady;
    fast.tracker = Tracker::Classic;
    fast.agreement = None;
    assert_eq!(kind(&fast), Some(Kind::Steady));
}

#[test]
fn a_file_s_tempo_is_read_at_the_octave_nearest_the_song_s() {
    assert_eq!(least_stretch(85.0, 170.0), 2.0);
    assert_eq!(least_stretch(170.0, 85.0), 0.5);
    assert_eq!(least_stretch(100.0, 120.0), 1.0);
    assert_eq!(least_stretch(120.0, 120.0), 1.0);
    // 160 is nearer 120 than 80 is, as a ratio, but 87.5 is nearer than 175.
    assert_eq!(least_stretch(160.0, 120.0), 1.0);
    assert_eq!(least_stretch(175.0, 120.0), 0.5);
}

#[test]
fn a_loop_dropped_into_a_song_warps_to_its_whole_bars() {
    let (mut setup, drums) = song(false);
    let (frames, analysis) = a_loop(4, 4, 100.0);
    file(&mut setup, "Loop.wav", frames, Some(analysis));
    let [loop_clip] = drop(&mut setup, drums, 8.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    // One marker at its start, at 100 BPM: its 16 beats, from where it was
    // dropped.
    assert_eq!(markers(&setup, loop_clip), Some((vec![(0, 0.0)], 100.0)));
    assert_eq!(span(&setup, loop_clip), (8.0, 24.0));
    let (_, audio) = clip(&setup, loop_clip);
    assert_eq!(audio.offset, Offset::Beats(Beats::ZERO));
    let warp = audio.warp.as_ref().unwrap();
    assert_eq!(warp.transients().count(), 16, "it takes its transients");
    // As one step: undo takes the clip away.
    assert_eq!(steps(&setup), ["Insert Audio"]);
    setup.run(&edit::UNDO);
    assert!(
        setup
            .document
            .project()
            .track(drums)
            .unwrap()
            .clips
            .is_empty()
    );
}

#[test]
fn a_loop_at_half_the_song_s_tempo_plays_at_its_own_speed_as_half_time() {
    let (mut setup, drums) = song(false);
    let mut tempo = setup.document.project().tempo.clone();
    tempo.set_tempo(160.0);
    setup.document.transact("Tempo", |tx| tx.set_tempo(tempo));
    // Two bars at 80 BPM, 36,000 frames to the beat.
    let (frames, analysis) = a_loop(2, 4, 80.0);
    file(&mut setup, "Slow.wav", frames, Some(analysis));
    let [slow] = drop(&mut setup, drums, 0.0, &["Slow.wav"])[..] else {
        panic!("one clip");
    };
    // Read at 160: its eight beats are 16 of the song's, six seconds.
    assert_eq!(markers(&setup, slow), Some((vec![(0, 0.0)], 160.0)));
    assert_eq!(span(&setup, slow), (0.0, 16.0));
}

#[test]
fn the_first_file_with_a_pulse_in_an_empty_song_gives_it_its_tempo_and_meter() {
    let (mut setup, drums) = song(true);
    // Four bars of 3/4 at 90 BPM.
    let (frames, analysis) = a_loop(4, 3, 90.0);
    file(&mut setup, "Waltz.wav", frames, Some(analysis));
    let [waltz] = drop(&mut setup, drums, 0.0, &["Waltz.wav"])[..] else {
        panic!("one clip");
    };
    let tempo = &setup.document.project().tempo;
    assert_eq!(tempo.bpm_at(Beats::ZERO), 90.0);
    assert_eq!(
        tempo.signature_at(Beats::ZERO),
        TimeSignature::new(3, 4).unwrap()
    );
    // So it isn't stretched: its 12 beats are the song's.
    assert_eq!(markers(&setup, waltz), Some((vec![(0, 0.0)], 90.0)));
    assert_eq!(span(&setup, waltz), (0.0, 12.0));
    // Undo takes the tempo back with the clip.
    setup.run(&edit::UNDO);
    let tempo = &setup.document.project().tempo;
    assert_eq!(*tempo, TempoMap::default());
    // In a song with a clip already, the file stretches to the song.
    let (mut setup, drums) = song(false);
    let (frames, analysis) = a_loop(4, 3, 90.0);
    file(&mut setup, "Waltz.wav", frames, Some(analysis));
    let [waltz] = drop(&mut setup, drums, 0.0, &["Waltz.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(setup.document.project().tempo, TempoMap::default());
    assert_eq!(markers(&setup, waltz), Some((vec![(0, 0.0)], 90.0)));
}

#[test]
fn a_steady_file_s_first_downbeat_lands_on_the_nearest_bar_line_with_its_pickup_before_it() {
    let (mut setup, drums) = song(false);
    let (frames, analysis) = with_pickup();
    file(&mut setup, "Song.wav", frames, Some(analysis.clone()));
    let [song_clip] = drop(&mut setup, drums, 8.0, &["Song.wav"])[..] else {
        panic!("one clip");
    };
    // One marker, on the first downbeat, at 100 BPM.
    let downbeat = 1_000 + AT_100;
    assert_eq!(
        markers(&setup, song_clip),
        Some((vec![(downbeat, 0.0)], 100.0))
    );
    // It plays on beat 8, the bar line it was dropped on, and the pickup's
    // beat before it.
    assert_eq!(plays(&setup, song_clip, downbeat), 8.0);
    let (start, end) = span(&setup, song_clip);
    let pickup = (1_000 + AT_100) as f64 / AT_100 as f64;
    assert!((start - (8.0 - pickup)).abs() < 1e-5, "{start}");
    assert!((end - (8.0 + 32.0)).abs() < 1e-5, "{end}");
    // Dropped just before a bar line, its downbeat lands on that one.
    let (mut setup, drums) = song(false);
    file(&mut setup, "Song.wav", frames, Some(analysis.clone()));
    let [song_clip] = drop(&mut setup, drums, 10.5, &["Song.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(plays(&setup, song_clip, downbeat), 12.0);
    // At the song's start, the pickup doesn't fit before the first bar
    // line, so the downbeat goes on the next.
    let (mut setup, drums) = song(false);
    file(&mut setup, "Song.wav", frames, Some(analysis));
    let [song_clip] = drop(&mut setup, drums, 0.0, &["Song.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(plays(&setup, song_clip, downbeat), 4.0);
}

#[test]
fn a_file_that_starts_on_its_downbeat_at_the_song_s_start_loses_the_moment_before_it() {
    let (mut setup, drums) = song(false);
    // Its first beat, a downbeat, 200 frames in: about 4 ms.
    let beats = steady_beats(200, 32, AT_100);
    let frames = 200 + 32 * AT_100;
    file(
        &mut setup,
        "Song.wav",
        frames,
        Some(pulse(frames, beats, 100.0, 0)),
    );
    let [song_clip] = drop(&mut setup, drums, 0.0, &["Song.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(plays(&setup, song_clip, 200), 0.0);
    let (c, a) = clip(&setup, song_clip);
    assert_eq!(c.start, Beats::ZERO);
    // The clip starts on its own beat 0, the downbeat.
    assert_eq!(a.offset, Offset::Beats(Beats::ZERO));
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
    let mut analysis = pulse(frames, beats.clone(), 60.0 * f64::from(RATE) / span, 0);
    analysis.steady.as_mut().unwrap().drift_ms = 30.0;
    (frames, analysis, beats)
}

#[test]
fn a_drifting_file_gets_a_marker_on_every_downbeat_and_each_beat_that_strays() {
    let (mut setup, drums) = song(false);
    let (frames, analysis, beats) = drifting_file();
    file(&mut setup, "Band.wav", frames, Some(analysis));
    let [band] = drop(&mut setup, drums, 4.0, &["Band.wav"])[..] else {
        panic!("one clip");
    };
    // Between the downbeats, the beats keep within 10 ms of where the
    // markers put them, but for the late one.
    let at = |k: usize| (beats[k], k as f64);
    let (markers, _) = markers(&setup, band).unwrap();
    assert_eq!(markers, [at(0), at(4), at(6), at(8), at(12), at(16)]);
    // Each marked beat plays on the song's beat.
    for k in [0, 4, 6, 8, 12, 16] {
        assert_eq!(plays(&setup, band, beats[k]), 4.0 + k as f64);
    }
}

#[test]
fn a_one_shot_plays_its_hit_where_it_s_dropped() {
    let (mut setup, drums) = song(false);
    // Its hit 100 ms in.
    let analysis = Analysis {
        sync: Some(4_800),
        ..Analysis::with_onsets(RATE, 48_000, Vec::new())
    };
    file(&mut setup, "Hit.wav", 48_000, Some(analysis));
    let [hit] = drop(&mut setup, drums, 8.0, &["Hit.wav"])[..] else {
        panic!("one clip");
    };
    // Unwarped, from a fifth of a beat earlier, two beats long.
    assert_eq!(markers(&setup, hit), None);
    assert_eq!(span(&setup, hit), (7.8, 9.8));
    assert_eq!(clip(&setup, hit).1.offset, Offset::Frames(0));
    // At the song's start, it starts that far into its file.
    let [first] = drop(&mut setup, drums, 0.0, &["Hit.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(span(&setup, first), (0.0, 1.8));
    assert_eq!(clip(&setup, first).1.offset, Offset::Frames(4_800));
}

#[test]
fn files_analysis_is_unsure_of_and_rules_switched_off_come_in_as_they_are() {
    let (mut setup, drums) = song(false);
    let (frames, mut analysis) = with_pickup();
    analysis.agreement = Some(0.5);
    file(&mut setup, "Unsure.wav", frames, Some(analysis));
    let (loop_frames, looped) = a_loop(4, 4, 100.0);
    file(&mut setup, "Loop.wav", loop_frames, Some(looped));
    let hit = Analysis {
        sync: Some(4_800),
        ..Analysis::with_onsets(RATE, 48_000, Vec::new())
    };
    file(&mut setup, "Hit.wav", 48_000, Some(hit));
    let [unsure] = drop(&mut setup, drums, 8.0, &["Unsure.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(markers(&setup, unsure), None);
    // All of the file from where it was dropped, at its own speed.
    let seconds = frames as f64 / f64::from(RATE);
    let (start, end) = span(&setup, unsure);
    assert_eq!(start, 8.0);
    assert!((end - (8.0 + seconds * 2.0)).abs() < 1e-5, "{end}");
    let behaviour = &mut setup.document.editor_mut().behaviour;
    behaviour.warp_loops = false;
    behaviour.sync_one_shots = false;
    let [looped] = drop(&mut setup, drums, 64.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(markers(&setup, looped), None);
    let [hit] = drop(&mut setup, drums, 96.0, &["Hit.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(span(&setup, hit), (96.0, 98.0));
}

#[test]
fn files_dropped_one_after_another_stay_one_after_another() {
    let (mut setup, drums) = song(false);
    let (long, a) = a_loop(4, 4, 100.0);
    let (short, b) = a_loop(2, 4, 100.0);
    file(&mut setup, "A.wav", long, Some(a));
    file(&mut setup, "B.wav", short, Some(b));
    let [a, b] = drop(&mut setup, drums, 0.0, &["A.wav", "B.wav"])[..] else {
        panic!("two clips");
    };
    assert_eq!(span(&setup, a), (0.0, 16.0));
    assert_eq!(span(&setup, b), (16.0, 24.0));
    assert_eq!(steps(&setup), ["Insert Audio"]);
}

#[test]
fn a_drop_waits_for_its_analysis_and_warps_as_part_of_its_step() {
    let (mut setup, drums) = song(true);
    let (frames, analysis) = a_loop(4, 4, 100.0);
    file(&mut setup, "Loop.wav", frames, None);
    let [loop_clip] = drop(&mut setup, drums, 0.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    // Unwarped, all of it at its own speed, until its analysis lands.
    assert_eq!(markers(&setup, loop_clip), None);
    assert_eq!(span(&setup, loop_clip), (0.0, 19.2));
    assert_eq!(setup.document.editor().auto_warp.len(), 1);
    assert!(
        !apply_pending(&mut setup.document, |_| true),
        "still analysing"
    );
    assert_eq!(setup.document.editor().auto_warp.len(), 1);
    setup
        .document
        .editor_mut()
        .analyses
        .insert("Loop.wav".into(), Arc::new(analysis));
    assert!(apply_pending(&mut setup.document, |_| false));
    assert!(setup.document.editor().auto_warp.is_empty());
    // The song was empty, so it takes the loop's tempo.
    assert_eq!(setup.document.project().tempo.bpm_at(Beats::ZERO), 100.0);
    assert_eq!(markers(&setup, loop_clip), Some((vec![(0, 0.0)], 100.0)));
    assert_eq!(span(&setup, loop_clip), (0.0, 16.0));
    assert_eq!(steps(&setup), ["Insert Audio"]);
    setup.run(&edit::UNDO);
    assert!(
        setup
            .document
            .project()
            .track(drums)
            .unwrap()
            .clips
            .is_empty()
    );
    assert_eq!(setup.document.project().tempo, TempoMap::default());
}

#[test]
fn a_drop_whose_analysis_lands_after_another_step_warps_as_a_step_of_its_own() {
    let (mut setup, drums) = song(true);
    // At 150 BPM: 12.8 beats unwarped, 16 warped.
    let (frames, analysis) = a_loop(4, 4, 150.0);
    file(&mut setup, "Loop.wav", frames, None);
    let [loop_clip] = drop(&mut setup, drums, 8.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    assert_eq!(span(&setup, loop_clip), (8.0, 20.8));
    setup
        .document
        .transact("Rename Track", |tx| tx.set_track_name(drums, "Loops"));
    setup
        .document
        .editor_mut()
        .analyses
        .insert("Loop.wav".into(), Arc::new(analysis.clone()));
    assert!(apply_pending(&mut setup.document, |_| false));
    assert_eq!(span(&setup, loop_clip), (8.0, 24.0));
    assert_eq!(steps(&setup), ["Insert Audio", "Rename Track", "Auto-Warp"]);
    // The song was empty, and still has no other clip: it takes the tempo.
    assert_eq!(setup.document.project().tempo.bpm_at(Beats::ZERO), 150.0);
    setup.run(&edit::UNDO);
    assert_eq!(markers(&setup, loop_clip), None);
    assert_eq!(setup.document.project().tempo, TempoMap::default());

    // A clip put where it would grow to stays as it is, as does one moved,
    // and the tempo set since stays.
    let (mut setup, drums) = song(true);
    file(&mut setup, "Loop.wav", frames, None);
    file(&mut setup, "After.wav", 48_000, None);
    let [grows] = drop(&mut setup, drums, 8.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    let [moves] = drop(&mut setup, drums, 40.0, &["Loop.wav"])[..] else {
        panic!("one clip");
    };
    drop(&mut setup, drums, 21.0, &["After.wav"]);
    let timing = Timing {
        start: Beats::new(44),
        ..clip(&setup, moves).0.timing()
    };
    setup
        .document
        .transact("Move", |tx| tx.set_clip_timing(moves, timing));
    let mut tempo = TempoMap::default();
    tempo.set_tempo(130.0);
    setup
        .document
        .transact("Tempo", |tx| tx.set_tempo(tempo.clone()));
    setup
        .document
        .editor_mut()
        .analyses
        .insert("Loop.wav".into(), Arc::new(analysis));
    let before = steps(&setup);
    assert!(!apply_pending(&mut setup.document, |_| false));
    assert_eq!(steps(&setup), before);
    assert_eq!(markers(&setup, grows), None);
    assert_eq!(markers(&setup, moves), None);
    assert_eq!(setup.document.project().tempo, tempo);
    assert!(setup.document.editor().auto_warp.is_empty());
}

#[test]
fn warp_follows_the_file_s_rule_or_else_its_best_guess_at_its_tempo() {
    let (mut setup, drums) = song(false);
    let (frames, analysis) = with_pickup();
    file(&mut setup, "Song.wav", frames, None);
    let [song_clip] = drop(&mut setup, drums, 4.0, &["Song.wav"])[..] else {
        panic!("one clip");
    };
    setup.document.editor_mut().auto_warp.clear();
    setup
        .document
        .editor_mut()
        .analyses
        .insert("Song.wav".into(), Arc::new(analysis.clone()));
    let on = || Args::new().with("on", true).with("clip", song_clip);
    setup.run_with(&warp::SET_WARP, on()).unwrap();
    // Its rule's marker, with the clip where it was, playing what it did.
    let downbeat = 1_000 + AT_100;
    assert_eq!(
        markers(&setup, song_clip),
        Some((vec![(downbeat, 0.0)], 100.0))
    );
    let (c, a) = clip(&setup, song_clip);
    assert_eq!(c.start, Beats::new(4));
    let first = a
        .warp
        .as_ref()
        .unwrap()
        .frame_at(a.offset.beats().unwrap().to_f64(), RATE);
    assert!(first.abs() < 0.5, "{first}");
    // Unsure, one marker at the clip's start, at the tempo found.
    setup.run(&edit::UNDO);
    let mut unsure = analysis;
    unsure.agreement = Some(0.5);
    setup
        .document
        .editor_mut()
        .analyses
        .insert("Song.wav".into(), Arc::new(unsure));
    setup.run_with(&warp::SET_WARP, on()).unwrap();
    assert_eq!(markers(&setup, song_clip), Some((vec![(0, 0.0)], 100.0)));
}

#[test]
fn the_tempo_guess_is_analysis_s_or_the_file_s_own_where_they_agree() {
    let (mut setup, _) = song(false);
    let guess = |setup: &Setup, name: &str| {
        let audio = AudioClip::new(name);
        let clip = Clip {
            id: ClipId(99),
            name: "Clip".into(),
            start: Beats::ZERO,
            length: Beats::new(4),
            content: ClipContent::Audio(audio.clone()),
        };
        let document = &setup.document;
        tempo_guess(document.project(), document.editor(), &clip, &audio)
    };
    let found = |bpm: f64| {
        let beats = steady_beats(0, 16, (60.0 * f64::from(RATE) / bpm) as u64);
        let frames = beats[15] + 1;
        pulse(frames, beats, bpm, 0)
    };
    // The name's, where it's near what analysis found.
    file(&mut setup, "Beat 120 BPM.wav", 480_000, Some(found(119.6)));
    assert_eq!(guess(&setup, "Beat 120 BPM.wav"), 120.0);
    // Analysis's, where it isn't.
    file(&mut setup, "Break 120 BPM.wav", 480_000, Some(found(93.0)));
    assert_eq!(guess(&setup, "Break 120 BPM.wav"), 93.0);
    // Either alone, at the octave nearest the song's.
    file(&mut setup, "Slow.wav", 480_000, Some(found(60.0)));
    assert_eq!(guess(&setup, "Slow.wav"), 120.0);
    file(&mut setup, "Pad 63 BPM.wav", 480_000, None);
    assert_eq!(guess(&setup, "Pad 63 BPM.wav"), 126.0);
    // Or else the song's.
    file(&mut setup, "Vocal.wav", 480_000, None);
    assert_eq!(guess(&setup, "Vocal.wav"), 120.0);
}

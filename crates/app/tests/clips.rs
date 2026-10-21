//! Clips through the app: a saved project whose audio clips play WAV, AIFF
//! and FLAC files from its folder, one of them long enough to stream, and
//! whose MIDI clip plays a test instrument. It finds its files, loops, and
//! plays the same with every renderer, before and after saving, as
//! `musicable null-test` checks it.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use musicable_app::null_test::{self, Player, Render};
use musicable_engine::{EngineConfig, ParallelOptions, rt};
use musicable_io::audio::{SampleFormat, save_wav, write_aiff};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent, MidiClip, Point};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackId, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 128,
};

/// More frames than the app loads whole, so the file streams.
const LONG: usize = (1 << 21) + 10_000;

/// A test signal: a sine a channel, with a click every 1,000 frames.
fn signal(channels: usize, frames: usize, rate: u32) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            let frequency = 150.0 * (c + 1) as f64;
            (0..frames)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    let click = if i.is_multiple_of(1_000) { 0.25 } else { 0.0 };
                    (std::f64::consts::TAU * frequency * t).sin() as f32 * 0.5 + click
                })
                .collect()
        })
        .collect()
}

fn planar(channels: &[Vec<f32>]) -> Vec<&[f32]> {
    channels.iter().map(Vec::as_slice).collect()
}

/// A folder holding the project's audio files, made fresh.
fn folder() -> PathBuf {
    let folder = std::env::temp_dir().join(format!("musicable-clips-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    let audio = folder.join("Audio");
    fs::create_dir_all(&audio).unwrap();
    let drums = signal(2, 60_000, 48_000);
    save_wav(
        &audio.join("Drums.wav"),
        48_000,
        SampleFormat::Int24,
        &planar(&drums),
    )
    .unwrap();
    let bass = signal(1, 30_000, 44_100);
    let mut file = BufWriter::new(File::create(audio.join("Bass.aif")).unwrap());
    write_aiff(&mut file, 44_100, SampleFormat::Int16, &planar(&bass)).unwrap();
    drop(file);
    let long = signal(1, LONG, 44_100);
    save_wav(
        &audio.join("Long.wav"),
        44_100,
        SampleFormat::Int16,
        &planar(&long),
    )
    .unwrap();
    let flac =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../io/tests/audio/ffmpeg-stereo-16.flac");
    fs::copy(flac, audio.join("Strings.flac")).unwrap();
    folder
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

fn audio_clip(project: &mut Project, track: TrackId, from: Beats, to: Beats, audio: AudioClip) {
    let clip = project.new_clip("", BeatRange::new(from, to), ClipContent::Audio(audio));
    project.track_mut(track).unwrap().clips.push(clip);
}

/// Four beats that loop: drums crossfading into a stream from near the end
/// of a long file, a bass line and strings, and chords on a test
/// instrument.
fn song() -> Project {
    let mut p = Project::new();
    p.looping = true;
    p.loop_region = BeatRange::new(Beats::ZERO, beats(4, 1));
    let drums = p.add_track("Drums", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Drums.wav");
    audio.fade_out = beats(1, 2);
    audio_clip(&mut p, drums, beats(0, 1), beats(2, 1), audio);
    let mut audio = AudioClip::new("Audio/Long.wav");
    audio.offset = LONG as u64 - 100_000;
    audio.fade_in = beats(1, 2);
    audio_clip(&mut p, drums, beats(3, 2), beats(4, 1), audio);

    let bass = p.add_track("Bass", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Bass.aif");
    audio.gain = 0.7;
    audio_clip(&mut p, bass, beats(1, 2), beats(3, 1), audio);
    let strings = p.add_track("Strings", TrackKind::Audio);
    audio_clip(
        &mut p,
        strings,
        beats(5, 2),
        beats(4, 1),
        AudioClip::new("Audio/Strings.flac"),
    );

    let keys = p.add_track("Keys", TrackKind::Midi);
    let mut chords = MidiClip::default();
    for (i, key) in [60, 64, 67].into_iter().enumerate() {
        let note = chords.add_note(beats(i as i64, 2), beats(3, 2), key, 0.6);
        note.expression.pitch_bend = vec![Point::new(beats(1, 2), 1.0)];
        note.expression.pressure = vec![Point::new(Beats::ZERO, 0.5)];
    }
    let clip = p.new_clip(
        "Chords",
        BeatRange::new(Beats::ZERO, beats(4, 1)),
        ClipContent::Midi(chords),
    );
    let instrument = p.new_device(DeviceKind::TestInstrument { level: 0.2 });
    let track = p.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    p
}

#[test]
fn a_saved_project_with_clips_plays_the_same_with_every_renderer() {
    let folder = folder();
    let path = folder.join("Song.musicable");
    project::save(&path, &song()).unwrap();
    let saved = project::load(&path).unwrap();
    assert_eq!(saved, song());

    let renders = [
        Render::Reference,
        Render::App,
        Render::Parallel(ParallelOptions {
            threads: 4,
            inline_below: None,
            realtime: false,
            ..ParallelOptions::default()
        }),
    ];
    let mut players: Vec<Player> = renders
        .iter()
        .map(|render| Player::new(saved.clone(), Some(&folder), render, CONFIG))
        .collect();
    // Twice round the loop and some more: at 120 BPM, four beats are two
    // seconds.
    let differences = null_test::compare(&mut players, 5 * u64::from(CONFIG.sample_rate));
    assert_eq!(differences, [None, None, None]);
    for player in &mut players {
        assert_eq!(player.take_problems(), Vec::<String>::new());
        assert!(player.stats().position < 4 * 24_000, "it loops");
    }
    assert!(players[0].peak() > 0.5, "peak {}", players[0].peak());

    // With a file missing, its clips are silent, and it says so.
    fs::remove_file(folder.join("Audio/Bass.aif")).unwrap();
    let mut missing = Player::new(saved, Some(&folder), &Render::Reference, CONFIG);
    missing.render(128);
    let problems = missing.take_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("Bass.aif"), "{problems:?}");
    fs::remove_dir_all(&folder).unwrap();
}

//! Writes a project with clips, and the audio files they play, to listen to
//! clips in the app:
//!
//! ```text
//! cargo run --release -p musicable-app --example clips_demo -- FOLDER
//! musicable "FOLDER/Clips demo.musicable"
//! ```
//!
//! Sixteen bars at 100 BPM that loop: a drum loop in WAV, played by a clip
//! a bar, a bass line in AIFF at 44.1 kHz, resampled, a pad from a file long
//! enough to stream from disk, fading in and out, and chords on the test
//! instrument, swelling with pressure and bending at the end of each bar.
//! `musicable null-test` plays it too.

use std::f64::consts::TAU;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;
use std::process::ExitCode;

use musicable_io::audio::{SampleFormat, save_wav, write_aiff};
use musicable_io::project;
use musicable_model::clip::{AudioClip, ClipContent, MidiClip, Offset, Point};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackKind};

const BPM: f64 = 100.0;
const BARS: i64 = 16;

/// Seconds to `beats` beats.
fn seconds(beats: f64) -> f64 {
    beats * 60.0 / BPM
}

/// The equal-tempered frequency of MIDI note `key`.
fn pitch(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// Noise from a seeded generator, the same every run.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// A bar of drums in stereo: kick on the beats, snare on two and four, and
/// hats between, panned a little right.
fn drums(rate: u32) -> [Vec<f32>; 2] {
    let frames = (seconds(4.0) * f64::from(rate)) as usize;
    let mut noise = Noise(7);
    let beat = seconds(1.0);
    let mut phase = 0.0;
    let (left, right) = (0..frames)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let (n, into) = ((t / beat) as usize, t % beat);
            let half = into % (beat / 2.0);
            phase += TAU * (45.0 + 110.0 * (-into * 35.0).exp()) / f64::from(rate);
            let kick = phase.sin() * (-into * 9.0).exp() * 0.28;
            let white = noise.next();
            let snare = if n % 2 == 1 {
                (white * 0.12 + (TAU * 185.0 * into).sin() * 0.1) * (-into * 18.0).exp()
            } else {
                0.0
            };
            let hat = if into >= beat / 2.0 {
                white * 0.06 * (-half * 70.0).exp()
            } else {
                0.0
            };
            (
                (kick + snare + hat * 0.6) as f32,
                (kick + snare + hat) as f32,
            )
        })
        .unzip();
    [left, right]
}

/// Four bars of bass, mono: a saw of six harmonics, a note a beat.
fn bass(rate: u32) -> Vec<f32> {
    let line: [u8; 16] = [
        45, 45, 57, 45, 41, 41, 53, 43, 48, 48, 60, 47, 43, 43, 55, 50,
    ];
    let frames = (seconds(16.0) * f64::from(rate)) as usize;
    (0..frames)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let (n, into) = ((t / seconds(1.0)) as usize, t % seconds(1.0));
            let f = pitch(line[n % 16]);
            let saw: f64 = (1..=6)
                .map(|h| (TAU * f * h as f64 * t).sin() / h as f64)
                .sum();
            let envelope = (into * 400.0).min(1.0) * (-into * 2.5).exp();
            (saw * envelope * 0.12) as f32
        })
        .collect()
}

/// A pad in stereo, longer than the app loads whole: an A minor chord,
/// slowly beating, a little wider in each channel.
fn pad(rate: u32, seconds: f64) -> [Vec<f32>; 2] {
    let frames = (seconds * f64::from(rate)) as usize;
    let chord = [57, 60, 64, 69].map(pitch);
    let channel = |detune: f64| -> Vec<f32> {
        (0..frames)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let sum: f64 = chord.iter().map(|f| (TAU * f * detune * t).sin()).sum();
                let swell = 0.75 + 0.25 * (TAU * 0.1 * t).sin();
                (sum * swell * 0.025) as f32
            })
            .collect()
    };
    [channel(0.998), channel(1.002)]
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

fn song() -> Project {
    let mut p = Project::new();
    p.tempo.set_tempo(BPM);
    p.looping = true;
    p.loop_region = BeatRange::new(Beats::ZERO, beats(4 * BARS, 1));
    let bar = |n: i64| beats(4 * n, 1);

    let drums = p.add_track("Drums", TrackKind::Audio);
    for n in 0..BARS {
        let mut audio = AudioClip::new("Audio/Drums.wav");
        // The last bar fades out into the loop.
        if n == BARS - 1 {
            audio.fade_out = beats(2, 1);
        }
        let range = BeatRange::new(bar(n), bar(n + 1));
        let clip = p.new_clip(format!("Drums {}", n + 1), range, ClipContent::Audio(audio));
        p.track_mut(drums).unwrap().clips.push(clip);
    }

    let bass = p.add_track("Bass", TrackKind::Audio);
    for n in (4..BARS).step_by(4) {
        let mut audio = AudioClip::new("Audio/Bass.aif");
        audio.fade_out = beats(1, 8);
        let range = BeatRange::new(bar(n), bar(n + 4));
        let clip = p.new_clip("Bass", range, ClipContent::Audio(audio));
        p.track_mut(bass).unwrap().clips.push(clip);
    }

    let pad = p.add_track("Pad", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Pad.wav");
    // From ten seconds into the file.
    audio.offset = Offset::Frames(10 * 48_000);
    audio.gain = 0.8;
    audio.fade_in = bar(2);
    audio.fade_out = bar(2);
    let clip = p.new_clip(
        "Pad",
        BeatRange::new(Beats::ZERO, bar(BARS)),
        ClipContent::Audio(audio),
    );
    p.track_mut(pad).unwrap().clips.push(clip);

    let keys = p.add_track("Keys", TrackKind::Midi);
    let chords: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]];
    let mut notes = MidiClip::default();
    for n in 0..BARS {
        let chord = chords[n as usize % 4];
        for (k, key) in chord.into_iter().enumerate() {
            let note = notes.add_note(bar(n), beats(4, 1), key + 12, 0.5);
            note.expression.pressure = vec![
                Point::new(Beats::ZERO, 0.1),
                Point::new(beats(1, 1), 0.4),
                Point::new(beats(2, 1), 0.8),
            ];
            note.expression.timbre = vec![Point::new(Beats::ZERO, n as f32 / BARS as f32)];
            // The top note bends up a tone at the end of the bar.
            if k == 2 {
                note.expression.pitch_bend = (0..8)
                    .map(|step| Point::new(beats(24 + step, 8), step as f32 / 4.0))
                    .collect();
            }
        }
    }
    let range = BeatRange::new(bar(2), bar(BARS));
    let mut clip = p.new_clip("Chords", range, ClipContent::Midi(notes));
    if let ClipContent::Midi(midi) = &mut clip.content {
        midi.offset = bar(2);
    }
    let instrument = p.new_device(DeviceKind::TestInstrument { level: 0.05 });
    let track = p.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    p
}

fn main() -> ExitCode {
    let Some(folder) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: clips_demo FOLDER");
        return ExitCode::FAILURE;
    };
    let written = (|| -> Result<PathBuf, Box<dyn std::error::Error>> {
        let audio = folder.join("Audio");
        fs::create_dir_all(&audio)?;
        let [left, right] = drums(48_000);
        save_wav(
            &audio.join("Drums.wav"),
            48_000,
            SampleFormat::Int24,
            &[&left, &right],
        )?;
        let mut file = BufWriter::new(File::create(audio.join("Bass.aif"))?);
        write_aiff(&mut file, 44_100, SampleFormat::Int16, &[&bass(44_100)])?;
        drop(file);
        // Ten seconds, then as long as the song.
        let [left, right] = pad(48_000, 10.0 + seconds(4.0 * BARS as f64) + 1.0);
        save_wav(
            &audio.join("Pad.wav"),
            48_000,
            SampleFormat::Int16,
            &[&left, &right],
        )?;
        let path = folder.join("Clips demo.musicable");
        project::save(&path, &song())?;
        Ok(path)
    })();
    match written {
        Ok(path) => {
            println!("Wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Couldn't write the demo to {}: {error}", folder.display());
            ExitCode::FAILURE
        }
    }
}

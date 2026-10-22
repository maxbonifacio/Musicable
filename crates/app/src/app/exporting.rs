//! The export window, as the app drives it: Ctrl+Shift+R opens it on the
//! song, with the settings the last export kept, and an export started from
//! it renders on its own thread while the window shows how it's going, then
//! says how it went.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use musicable_io::audio::Decoder;
use musicable_io::project;
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackKind};

use super::testing::{TempDir, chord, press, window};
use crate::export::{Bits, ExportSettings, FileFormat};
use crate::settings::{SETTINGS_FILE, Settings};

/// Two beats of a chord on a test instrument.
fn song() -> Project {
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let mut chord = MidiClip::default();
    for key in [60, 64, 67] {
        chord.add_note(Beats::ZERO, Beats::new(1), key, 0.8);
    }
    let range = BeatRange::new(Beats::ZERO, Beats::new(2));
    let clip = song.new_clip("Chord", range, ClipContent::Midi(chord));
    let instrument = song.new_device(DeviceKind::TestInstrument { level: 0.3 });
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    song
}

#[test]
fn ctrl_shift_r_opens_the_export_window_and_exports_in_the_background() {
    let dir = TempDir::new("exporting");
    let path = dir.0.join("Song.musicable");
    project::save(&path, &song()).unwrap();
    let mut harness = window(&dir.0, Some(path));
    press(&mut harness, chord("Ctrl+Shift+R"));
    harness.get_by_label("Export Audio");
    harness.get_by_label("Export...");
    harness.get_by_label("1. 1. 1 to 1. 3. 1, 0:01.000");

    // Where the files go is the file dialog's to say, which a test can't
    // answer: it exports where the dialog would have said.
    let settings = ExportSettings {
        format: FileFormat::Flac,
        bits: Bits::Int16,
        ..ExportSettings::default()
    };
    let file = dir.0.join("Mix.flac");
    let range = BeatRange::new(Beats::ZERO, Beats::new(2));
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .export_to(&file, range, settings, &BTreeSet::new(), &ctx);
    assert!(harness.state().export.is_some());
    let started = Instant::now();
    while harness.state().export.is_some() {
        assert!(started.elapsed() < Duration::from_secs(60), "it never ends");
        harness.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    harness.run();
    let message = format!("Exported {}.", file.display());
    assert_eq!(harness.state().messages.last(), Some(&message));
    harness.get_by_label_contains("Mix.flac: peak -");
    let info = Decoder::open(&file).unwrap().info();
    // The chord rings out before the song ends, so the tail adds nothing.
    assert_eq!((info.sample_rate, info.frames), (48_000, 48_000));

    // The next export starts with these settings, also after a restart.
    let kept = Settings::read(&dir.0.join(SETTINGS_FILE)).unwrap();
    assert_eq!(kept.export, settings);
}

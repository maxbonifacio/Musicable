//! The done test for the arrangement view (N6 in the Tier 1 plan): the Tier
//! 1 demo's arranging steps, done with the mouse and the keys in the app's
//! window, undone and redone, then saved and opened again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{DroppedFile, Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_io::audio::{SampleFormat, save_wav};
use musicable_io::project;
use musicable_model::clip::ClipContent;
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackKind};
use musicable_ui::Theme;

use super::App;
use super::testing::{TempDir, chord, press, window};

/// A file dropped on the window.
#[derive(Debug)]
struct Dropped(PathBuf);

impl DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("the app reads dropped files from their paths".to_owned())
    }
}

fn song<'a>(harness: &'a Harness<'static, App>) -> &'a Project {
    harness.state().session.project()
}

/// The clips on the track called `track`, by name, start and end in beats,
/// in order.
fn clips(harness: &Harness<'static, App>, track: &str) -> Vec<(String, f64, f64)> {
    let song = song(harness);
    let track = song.tracks.iter().find(|t| t.name == track).unwrap();
    let mut clips: Vec<_> = track
        .clips
        .iter()
        .map(|c| (c.name.clone(), c.start.to_f64(), c.end().to_f64()))
        .collect();
    clips.sort_by(|a, b| a.1.total_cmp(&b.1));
    clips
}

fn clip(name: &str, start: f64, end: f64) -> (String, f64, f64) {
    (name.to_owned(), start, end)
}

/// Where the timeline shows `at`.
fn x_of(harness: &Harness<'static, App>, at: f64) -> f32 {
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().session.editor().viewport;
    lanes.left() + viewport.x_of(Beats::from_f64(at))
}

/// The row of the track called `track`.
fn row(harness: &Harness<'static, App>, track: &str) -> Rect {
    harness.get_by_label(&format!("Track {track}")).rect()
}

/// In the empty part of `track`'s lane, or in the title bar or the body of
/// a clip there, at `at`.
fn lane(harness: &Harness<'static, App>, track: &str, at: f64) -> Pos2 {
    pos2(x_of(harness, at), row(harness, track).center().y)
}

fn title(harness: &Harness<'static, App>, track: &str, at: f64) -> Pos2 {
    pos2(x_of(harness, at), row(harness, track).top() + 6.0)
}

fn body(harness: &Harness<'static, App>, track: &str, at: f64) -> Pos2 {
    pos2(x_of(harness, at), row(harness, track).bottom() - 6.0)
}

/// In the ruler's strip, where the loop brace is drawn, at `at`.
fn strip(harness: &Harness<'static, App>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    let y = ruler.top() + 0.75 * Theme::default().ruler_strip;
    pos2(x_of(harness, at), y)
}

/// In the ruler, under its strip, at `at`.
fn ruler(harness: &Harness<'static, App>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    pos2(x_of(harness, at), ruler.bottom() - 6.0)
}

fn button(harness: &mut Harness<'static, App>, at: Pos2, button: PointerButton, pressed: bool) {
    harness.event(Event::PointerButton {
        pos: at,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
    harness.run();
}

fn click_with(harness: &mut Harness<'static, App>, at: Pos2, pointer: PointerButton) {
    harness.hover_at(at);
    harness.run();
    button(harness, at, pointer, true);
    button(harness, at, pointer, false);
}

fn click(harness: &mut Harness<'static, App>, at: Pos2) {
    click_with(harness, at, PointerButton::Primary);
}

/// Drags from `from` to `to`, in a few moves.
fn drag(harness: &mut Harness<'static, App>, from: Pos2, to: Pos2) {
    harness.hover_at(from);
    harness.run();
    button(harness, from, PointerButton::Primary, true);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    button(harness, to, PointerButton::Primary, false);
}

/// Runs the window until the thread that reads audio files has read the
/// waveform of `file`.
fn wait_for_waveform(harness: &mut Harness<'static, App>, file: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !harness.state().audio_files.waveforms().contains_key(file) {
        assert!(Instant::now() < deadline, "{file}'s waveform wasn't read");
        std::thread::sleep(Duration::from_millis(2));
        harness.run();
    }
}

/// A saved song with an audio track and a MIDI track and no clips yet, and
/// a take to put on the audio track: four seconds, two bars at 120 BPM.
fn song_folder() -> (TempDir, PathBuf, PathBuf) {
    let folder = TempDir::new("arranging-song");
    let take = folder.0.join("Take.wav");
    let samples: Vec<f32> = (0..4 * 48_000)
        .map(|i| (i as f32 * 0.05).sin() * 0.5)
        .collect();
    save_wav(&take, 48_000, SampleFormat::Float32, &[&samples]).unwrap();
    let mut song = Project::new();
    song.add_track("Vocals", TrackKind::Audio);
    song.add_track("Keys", TrackKind::Midi);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    (folder, path, take)
}

#[test]
fn the_demos_arranging_steps_work_with_the_mouse_and_the_keys() {
    let config = TempDir::new("arranging-config");
    let (_folder, path, take) = song_folder();
    let mut harness = window(&config.0, Some(path.clone()));
    let opened = song(&harness).clone();

    // The take, dropped at the start of the audio track's lane, lasts as
    // long as its file, and plays it from the project's folder.
    harness.hover_at(lane(&harness, "Vocals", 0.2));
    harness.run();
    harness
        .input_mut()
        .dropped_files
        .push(Arc::new(Dropped(take.clone())));
    harness.run();
    assert_eq!(clips(&harness, "Vocals"), [clip("Take", 0.0, 8.0)]);
    let track = &song(&harness).tracks[0];
    let ClipContent::Audio(audio) = &track.clips[0].content else {
        panic!("the take is an audio clip");
    };
    assert_eq!(audio.file, "Take.wav");
    wait_for_waveform(&mut harness, "Take.wav");

    // A click in the MIDI track's lane puts the start marker there, and
    // Ctrl+Shift+M a bar-long MIDI clip.
    let at = lane(&harness, "Keys", 8.2);
    click(&mut harness, at);
    press(&mut harness, chord("Ctrl+Shift+M"));
    assert_eq!(clips(&harness, "Keys"), [clip("MIDI Clip", 8.0, 12.0)]);

    // Dragging the take's title moves it a bar later, on the grid, and
    // Ctrl+D puts a copy after it.
    let (from, to) = (
        title(&harness, "Vocals", 1.0),
        title(&harness, "Vocals", 5.0),
    );
    drag(&mut harness, from, to);
    assert_eq!(clips(&harness, "Vocals"), [clip("Take", 4.0, 12.0)]);
    press(&mut harness, chord("Ctrl+D"));
    let two = [clip("Take", 4.0, 12.0), clip("Take", 12.0, 20.0)];
    assert_eq!(clips(&harness, "Vocals"), two);

    // A click in the copy's body selects it and marks where: Ctrl+E splits
    // it there. Then the second half goes with Delete.
    let at = body(&harness, "Vocals", 16.2);
    click(&mut harness, at);
    press(&mut harness, chord("Ctrl+E"));
    let three = [
        clip("Take", 4.0, 12.0),
        clip("Take", 12.0, 16.0),
        clip("Take", 16.0, 20.0),
    ];
    assert_eq!(clips(&harness, "Vocals"), three);
    let at = title(&harness, "Vocals", 18.0);
    click(&mut harness, at);
    press(&mut harness, chord("Delete"));
    let two = [clip("Take", 4.0, 12.0), clip("Take", 12.0, 16.0)];
    assert_eq!(clips(&harness, "Vocals"), two);

    // The first half's end is trimmed back by its edge, to the grid, which
    // is every two beats at this zoom, and the first take fades in over a
    // beat by its handle.
    let y = row(&harness, "Vocals").center().y;
    let from = pos2(x_of(&harness, 16.0) - 2.0, y);
    let to = pos2(x_of(&harness, 14.0) - 2.0, y);
    drag(&mut harness, from, to);
    let trimmed = [clip("Take", 4.0, 12.0), clip("Take", 12.0, 14.0)];
    assert_eq!(clips(&harness, "Vocals"), trimmed);
    let theme = Theme::default();
    let handle = pos2(
        x_of(&harness, 4.0) + theme.edge_grab + theme.fade_handle / 2.0,
        row(&harness, "Vocals").top() + 1.0 + theme.clip_title + theme.fade_handle / 2.0,
    );
    let beat = x_of(&harness, 5.0) - x_of(&harness, 4.0);
    drag(&mut harness, handle, handle + vec2(beat, 0.0));
    let track = &song(&harness).tracks[0];
    let first = track
        .clips
        .iter()
        .find(|c| c.start == Beats::new(4))
        .unwrap();
    let ClipContent::Audio(audio) = &first.content else {
        panic!("the take is an audio clip");
    };
    let fade = audio.fade_in.to_f64();
    assert!((fade - 1.0).abs() < 0.05, "{fade}");

    // A new loop brace is drawn in the ruler's strip, away from the one
    // there, which turns looping on, and Ctrl+L turns it off and on again.
    let (from, to) = (strip(&harness, 20.0), strip(&harness, 28.0));
    drag(&mut harness, from, to);
    let looped = BeatRange::new(Beats::new(20), Beats::new(28));
    assert_eq!(song(&harness).loop_region, looped);
    assert!(song(&harness).looping);
    press(&mut harness, chord("Ctrl+L"));
    assert!(!song(&harness).looping);
    press(&mut harness, chord("Ctrl+L"));
    assert!(song(&harness).looping);

    // A locator from the ruler's context menu, where it was opened.
    let at = ruler(&harness, 8.2);
    click_with(&mut harness, at, PointerButton::Secondary);
    harness.get_by_label("Add Locator").click();
    harness.run();
    let locators: Vec<_> = song(&harness).locators.iter().map(|l| l.at).collect();
    assert_eq!(locators, [Beats::new(8)]);

    // The zoom keys, and back.
    let zoom = harness.state().session.editor().viewport.zoom;
    press(&mut harness, chord("Equals"));
    assert!(harness.state().session.editor().viewport.zoom > zoom);
    press(&mut harness, chord("Minus"));
    let zoomed_back = harness.state().session.editor().viewport.zoom;
    assert!((zoomed_back - zoom).abs() < 1e-9, "{zoomed_back} {zoom}");

    // Every step undoes, one at a time, back to the song as it was opened,
    // and redoes again.
    let arranged = song(&harness).clone();
    let steps = 12;
    for _ in 0..steps {
        press(&mut harness, chord("Ctrl+Z"));
    }
    // Undo leaves the counters that give out ids alone, so an id is never
    // given out twice.
    let mut undone = song(&harness).clone();
    undone.next_clip_id = opened.next_clip_id;
    undone.next_locator_id = opened.next_locator_id;
    assert_eq!(undone, opened);
    assert_eq!(harness.state().session.undo_name(), None);
    for _ in 0..steps {
        press(&mut harness, chord("Ctrl+Y"));
    }
    assert_eq!(*song(&harness), arranged);
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );

    // Ctrl+S saves it, and it opens again as it was.
    press(&mut harness, chord("Ctrl+S"));
    assert!(!harness.state().session.is_modified());
    drop(harness);
    let harness = window(&config.0, Some(path));
    assert_eq!(*song(&harness), arranged);
}

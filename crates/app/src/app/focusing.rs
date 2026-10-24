//! The done test for the focus and panels' own keys (milestone 2 of the Tier
//! 2 UI foundations): a click shows which panel has the focus, Copy and
//! Delete act on that panel's selection, and a key bound only in the piano
//! roll works there and nowhere else.
//!
//! It drives the app's window as eframe would, without an audio device.

use eframe::egui::{Event, Modifiers, PointerButton, Pos2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::Focus;
use musicable_io::project;
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackKind};
use musicable_ui::PIANO_ROLL;

use super::App;
use super::testing::{TempDir, chord, press, window};

/// A bar-long MIDI clip with a C3 and an E3.
fn song() -> Project {
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let mut riff = MidiClip::default();
    riff.add_note(Beats::ZERO, Beats::new(1), 60, 0.8);
    riff.add_note(Beats::new(1), Beats::new(1), 64, 0.8);
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    let clip = song.new_clip("Riff", range, ClipContent::Midi(riff));
    song.track_mut(keys).unwrap().clips.push(clip);
    song
}

fn focus(harness: &Harness<'static, App>) -> Focus {
    harness.state().session.editor().focus
}

/// The clips on the track, and the keys of the first one's notes.
fn song_now(harness: &Harness<'static, App>) -> (usize, Vec<u8>) {
    let clips = &harness.state().session.project().tracks[0].clips;
    let keys = match clips.first().map(|clip| &clip.content) {
        Some(ClipContent::Midi(midi)) => midi.notes.iter().map(|note| note.key).collect(),
        _ => Vec::new(),
    };
    (clips.len(), keys)
}

/// Moves the pointer to `at`, waits a third of a second, so the clicks
/// before don't make these a double click, and clicks `times` times.
fn click_at(harness: &mut Harness<'static, App>, at: Pos2, times: usize) {
    harness.hover_at(at);
    for _ in 0..20 {
        harness.step();
    }
    for _ in 0..times {
        for pressed in [true, false] {
            harness.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
    }
    harness.run();
}

/// In the title bar of the clip on the track, at `at` beats, or in its
/// body.
fn clip_title(harness: &Harness<'static, App>, at: f64) -> Pos2 {
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().session.editor().viewport;
    let row = harness.get_by_label("Track Keys").rect();
    pos2(
        lanes.left() + viewport.x_of(Beats::from_f64(at)),
        row.top() + 6.0,
    )
}

fn clip_body(harness: &Harness<'static, App>, at: f64) -> Pos2 {
    let row = harness.get_by_label("Track Keys").rect();
    pos2(clip_title(harness, at).x, row.bottom() - 6.0)
}

fn click(harness: &mut Harness<'static, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

#[test]
fn the_keys_and_the_edit_commands_act_in_the_panel_clicked() {
    let dir = TempDir::new("focusing");
    let path = dir.0.join("Song.musicable");
    project::save(&path, &song()).unwrap();
    let mut harness = window(&dir.0, Some(path));
    assert_eq!(focus(&harness), Focus::ARRANGEMENT);

    // A double-click opens the clip in the piano roll, which takes the
    // focus, and a click there selects a note. Up is the piano roll's own
    // key: it transposes the note.
    let at = clip_body(&harness, 1.0);
    click_at(&mut harness, at, 2);
    assert!(harness.state().shell.panel_shown(PIANO_ROLL));
    assert_eq!(focus(&harness), Focus::PIANO_ROLL);
    let note = harness.get_by_label_contains("Note C3").rect().center();
    click_at(&mut harness, note, 1);
    press(&mut harness, chord("Up"));
    assert_eq!(song_now(&harness), (1, vec![61, 64]));

    // A click in the arrangement gives it the focus. Up does nothing there,
    // and Copy takes the clip.
    let at = clip_title(&harness, 1.0);
    click_at(&mut harness, at, 1);
    assert_eq!(focus(&harness), Focus::ARRANGEMENT);
    press(&mut harness, chord("Up"));
    assert_eq!(song_now(&harness), (1, vec![61, 64]));
    press(&mut harness, chord("Ctrl+C"));
    let editor = harness.state().session.editor();
    assert_eq!(editor.clipboard.as_ref().map(|c| c.clips.len()), Some(1));
    assert!(editor.piano_roll.clipboard.is_none());

    // Clicking the piano roll's tab gives it the focus back: Copy and
    // Delete take the note selected there, and leave the clip.
    click(&mut harness, "Piano Roll panel");
    assert_eq!(focus(&harness), Focus::PIANO_ROLL);
    press(&mut harness, chord("Ctrl+C"));
    let roll = &harness.state().session.editor().piano_roll;
    assert_eq!(roll.clipboard.as_ref().map(|c| c.notes.len()), Some(1));
    press(&mut harness, chord("Delete"));
    assert_eq!(song_now(&harness), (1, vec![64]));

    // The devices panel has nothing Delete takes.
    click(&mut harness, "Devices panel");
    assert_eq!(focus(&harness), Focus::DEVICES);
    press(&mut harness, chord("Delete"));
    assert_eq!(song_now(&harness), (1, vec![64]));

    // In the arrangement, Delete takes the clip.
    let at = clip_title(&harness, 1.0);
    click_at(&mut harness, at, 1);
    assert_eq!(focus(&harness), Focus::ARRANGEMENT);
    press(&mut harness, chord("Delete"));
    assert_eq!(song_now(&harness), (0, vec![]));
}

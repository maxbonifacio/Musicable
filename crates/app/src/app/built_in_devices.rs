//! Musicable's own devices in the app's window: the browser lists the ones
//! that play, a double-click puts one on the selected track, its controls
//! change it a step at a time in the undo history, and it comes back from
//! the project's file as it was saved. One added while the piano roll is in
//! the devices' place brings them forward.
//!
//! It drives the app's window as eframe would, without an audio device.

use eframe::egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_io::project;
use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::native::{NativeDevice, NativeKind, eq};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackKind};
use musicable_ui::{DEVICES, PIANO_ROLL};

use super::App;
use super::testing::{TempDir, chord, press, window};

/// Moves the pointer to `at`, waits a third of a second, so the clicks
/// before don't make these a double or triple click, and clicks `times`
/// times.
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

/// The built-in devices on the track called `track`, in order.
fn natives(project: &Project, track: &str) -> Vec<NativeDevice> {
    let track = project.tracks.iter().find(|t| t.name == track).unwrap();
    (track.devices.iter())
        .filter_map(|d| d.kind.native().cloned())
        .collect()
}

#[test]
fn an_eq_goes_on_a_track_and_its_controls_change_it() {
    let folder = TempDir::new("built-in-song");
    let config = TempDir::new("built-in-config");
    let mut song = Project::new();
    song.add_track("Keys", TrackKind::Audio);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path.clone()));

    // The browser lists the built-in devices that play, and a double-click
    // puts one on the selected track.
    let header = harness.get_by_label("Track Keys").rect();
    click_at(
        &mut harness,
        pos2(header.left() + 12.0, header.center().y),
        1,
    );
    harness.get_by_label("Add Device...").click();
    harness.run();
    for kind in NativeKind::ALL {
        let listed = harness.query_by_label(kind.name()).is_some();
        assert_eq!(
            listed,
            musicable_engine::devices::BUILT.contains(&kind),
            "{kind:?}"
        );
    }
    let at = harness.get_by_label("EQ").rect().center();
    click_at(&mut harness, at, 2);
    let state = harness.state();
    assert_eq!(
        natives(state.session.project(), "Keys"),
        [NativeDevice::new(NativeKind::Eq)]
    );
    assert_eq!(state.session.undo_name(), Some("Add EQ"));

    // Dragging a control is one step in the undo history.
    let freq = 2 * eq::PER_BAND + eq::FREQ;
    let bar = harness.get_by_label("EQ Band 3 Freq").rect().center();
    harness.hover_at(bar);
    harness.run();
    harness.drag_at(bar);
    harness.run();
    for x in 1..=4 {
        harness.hover_at(bar + vec2(10.0 * x as f32, 0.0));
        harness.run();
    }
    harness.drop_at(bar + vec2(40.0, 0.0));
    harness.run();
    let moved = natives(harness.state().session.project(), "Keys")[0].values[freq];
    assert!(moved > 250.0, "{moved}");
    assert_eq!(harness.state().session.undo_name(), Some("Change EQ"));
    press(&mut harness, chord("Ctrl+Z"));
    let back = natives(harness.state().session.project(), "Keys")[0].values[freq];
    assert_eq!(back, 250.0);
    assert_eq!(harness.state().session.undo_name(), Some("Add EQ"));
    press(&mut harness, chord("Ctrl+Y"));

    // Ctrl+S saves it, and it opens again as it was.
    press(&mut harness, chord("Ctrl+S"));
    assert!(!harness.state().session.is_modified());
    assert!(
        harness.state().messages.is_empty(),
        "{:?}",
        harness.state().messages
    );
    drop(harness);
    let harness = window(&config.0, Some(path));
    let eq = &natives(harness.state().session.project(), "Keys")[0];
    assert_eq!(eq.values[freq], moved);
}

#[test]
fn a_device_added_with_the_piano_roll_open_comes_into_sight() {
    let folder = TempDir::new("piano-roll-device-song");
    let config = TempDir::new("piano-roll-device-config");
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    let riff = song.new_clip("Riff", range, ClipContent::Midi(MidiClip::default()));
    song.track_mut(keys).unwrap().clips.push(riff);
    let path = folder.0.join("Song.musicable");
    project::save(&path, &song).unwrap();
    let mut harness = window(&config.0, Some(path));

    // A double-click on the clip opens it in the piano roll, in the
    // devices' place.
    let lanes = harness.get_by_label("Arrangement").rect();
    let row = harness.get_by_label("Track Keys").rect();
    let x = harness
        .state()
        .session
        .editor()
        .viewport
        .x_of(Beats::new(1));
    click_at(&mut harness, pos2(lanes.left() + x, row.bottom() - 6.0), 2);
    let shell = &harness.state().shell;
    assert!(shell.panel_shown(PIANO_ROLL) && !shell.panel_shown(DEVICES));

    // A synth double-clicked in the browser goes on the clip's track, and
    // the devices come forward with it.
    press(&mut harness, chord("Ctrl+Alt+B"));
    harness.get_by_label("Devices").click();
    harness.run();
    let at = harness.get_by_label("Synth").rect().center();
    click_at(&mut harness, at, 2);
    assert_eq!(
        natives(harness.state().session.project(), "Keys"),
        [NativeDevice::new(NativeKind::Synth)]
    );
    let shell = &harness.state().shell;
    assert!(shell.panel_shown(DEVICES) && !shell.panel_shown(PIANO_ROLL));
    harness.get_by_label("Device Synth");
}

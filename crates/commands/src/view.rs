//! The view commands: what the window shows, the arrangement's and the
//! piano roll's zoom and scrolling, the grid, and which of the two has the
//! focus.
//!
//! Commands can't show a window, so the command palette and the mixer ask
//! the app to, with a [`Request`]. The zoom, the scrolling and the grid are
//! in the [`Editor`](crate::Editor). Zooming acts on the view with the focus:
//! the arrangement, or the piano roll.

use musicable_model::TrackId;
use musicable_model::time::Beats;

use crate::editor::{Editor, Focus, PianoRoll, Step, Viewport};
use crate::{
    Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value, When,
};

/// Shows the command palette, which finds any command by name and runs it.
pub const COMMAND_PALETTE: CommandId = CommandId::new("view.command_palette");
/// Shows or hides the mixer, under the arrangement.
pub const TOGGLE_MIXER: CommandId = CommandId::new("view.toggle_mixer");
/// Shows or hides the devices of the selected track, or the master's, under
/// the arrangement: Live's Detail View.
pub const TOGGLE_DEVICES: CommandId = CommandId::new("view.toggle_devices");
/// Shows or hides the browser, beside the arrangement.
pub const TOGGLE_BROWSER: CommandId = CommandId::new("view.toggle_browser");
/// Shows or hides the piano roll, in a tab beside the devices: Live's clip
/// view.
pub const TOGGLE_PIANO_ROLL: CommandId = CommandId::new("view.toggle_piano_roll");
/// Shows or hides the History panel, which lists the steps of the undo
/// history, in a tab beside the browser.
pub const TOGGLE_HISTORY: CommandId = CommandId::new("view.toggle_history");
/// Brings the piano roll in front of the devices, or the devices in front of
/// the piano roll: Live's Toggle Between Device/Clip View.
pub const SWITCH_DEVICES_PIANO_ROLL: CommandId = CommandId::new("view.switch_devices_piano_roll");
/// Moves the panel with the focus to a new window of its own, as its tab's
/// menu does. The arrangement stays in the main window.
pub const MOVE_TO_NEW_WINDOW: CommandId = CommandId::new("view.move_to_new_window");
/// Sends the panel with the focus back to where it was in the main window,
/// from the window of its own it's in.
pub const MOVE_TO_MAIN_WINDOW: CommandId = CommandId::new("view.move_to_main_window");
/// Zooms the arrangement in, or the piano roll while it has the focus,
/// around the start marker.
pub const ZOOM_IN: CommandId = CommandId::new("view.zoom_in");
/// Zooms the arrangement out, or the piano roll, around the start marker.
pub const ZOOM_OUT: CommandId = CommandId::new("view.zoom_out");
/// Zooms the arrangement to show the selection across it, or the piano roll
/// to show its selected notes.
pub const ZOOM_TO_SELECTION: CommandId = CommandId::new("view.zoom_to_selection");
/// Turns on or off scrolling the arrangement to keep the playhead in sight.
pub const TOGGLE_FOLLOW: CommandId = CommandId::new("view.toggle_follow");
/// Shows the selected tracks' takes in lanes under them, or hides them.
pub const TOGGLE_TAKE_LANES: CommandId = CommandId::new("view.toggle_take_lanes");
/// Brings the grid's lines closer together.
pub const NARROW_GRID: CommandId = CommandId::new("view.narrow_grid");
/// Takes the grid's lines further apart.
pub const WIDEN_GRID: CommandId = CommandId::new("view.widen_grid");
/// Turns triplet grid lines on or off.
pub const TRIPLET_GRID: CommandId = CommandId::new("view.triplet_grid");
/// Turns snapping to the grid on or off.
pub const TOGGLE_SNAP: CommandId = CommandId::new("view.toggle_snap");
/// Switches the grid between following the zoom and keeping its step.
pub const FIXED_GRID: CommandId = CommandId::new("view.fixed_grid");
/// Sets the arrangement's zoom and scrolling.
pub const SET_VIEWPORT: CommandId = CommandId::new("view.set_viewport");
/// Sets the height of a track in the arrangement.
pub const SET_TRACK_HEIGHT: CommandId = CommandId::new("view.set_track_height");
/// Sets the piano roll's zoom, scrolling and key height.
pub const SET_PIANO_ROLL_VIEWPORT: CommandId = CommandId::new("view.set_piano_roll_viewport");
/// Gives a panel the focus, where the edit commands and the keys act.
pub const SET_FOCUS: CommandId = CommandId::new("view.set_focus");

/// How much Zoom In and Zoom Out zoom by.
const ZOOM_STEP: f64 = 2.0;
/// The lowest a track gets, in points.
pub const MIN_TRACK_HEIGHT: f32 = 24.0;
/// The highest a track gets, in points.
pub const MAX_TRACK_HEIGHT: f32 = 400.0;

type Handler = fn(&mut Context<'_>);

/// The view commands that take no arguments, with their titles, when they
/// apply and what they run.
const COMMANDS: [(CommandId, &str, When, Handler); 9] = [
    (ZOOM_IN, "Zoom In", When::Always, |ctx| zoom(ctx, ZOOM_STEP)),
    (ZOOM_OUT, "Zoom Out", When::Always, |ctx| {
        zoom(ctx, 1.0 / ZOOM_STEP)
    }),
    (
        ZOOM_TO_SELECTION,
        "Zoom to Selection",
        When::CanCopy,
        zoom_to_selection,
    ),
    (TOGGLE_FOLLOW, "Follow", When::Always, |ctx| {
        let editor = ctx.document.editor_mut();
        editor.follow = !editor.follow;
    }),
    (NARROW_GRID, "Narrow Grid", When::Always, |ctx| {
        widen_grid(ctx.document.editor_mut(), -1)
    }),
    (WIDEN_GRID, "Widen Grid", When::Always, |ctx| {
        widen_grid(ctx.document.editor_mut(), 1)
    }),
    (TRIPLET_GRID, "Triplet Grid", When::Always, |ctx| {
        let grid = &mut ctx.document.editor_mut().grid;
        grid.triplet = !grid.triplet;
    }),
    (TOGGLE_SNAP, "Snap to Grid", When::Always, |ctx| {
        let grid = &mut ctx.document.editor_mut().grid;
        grid.snap = !grid.snap;
    }),
    (FIXED_GRID, "Fixed Grid", When::Always, fix_grid),
];

/// The view commands that ask the app for something.
const REQUESTS: [(CommandId, &str, Request); 9] = [
    (
        COMMAND_PALETTE,
        "Command Palette...",
        Request::CommandPalette,
    ),
    (TOGGLE_MIXER, "Mixer", Request::TogglePanel("mixer")),
    (TOGGLE_DEVICES, "Devices", Request::TogglePanel("devices")),
    (TOGGLE_BROWSER, "Browser", Request::TogglePanel("browser")),
    (
        TOGGLE_PIANO_ROLL,
        "Piano Roll",
        Request::TogglePanel("piano_roll"),
    ),
    (TOGGLE_HISTORY, "History", Request::TogglePanel("history")),
    (
        SWITCH_DEVICES_PIANO_ROLL,
        "Switch Devices and Piano Roll",
        Request::SwitchDevicesPianoRoll,
    ),
    (
        MOVE_TO_NEW_WINDOW,
        "Move Panel to New Window",
        Request::MoveToNewWindow,
    ),
    (
        MOVE_TO_MAIN_WINDOW,
        "Move Panel to Main Window",
        Request::MoveToMainWindow,
    ),
];

/// Registers the view commands.
pub fn register(registry: &mut Registry) {
    crate::register_requests(registry, &REQUESTS);
    for (id, title, when, run) in COMMANDS {
        registry.add(Command::new(id, title).when(when), move |ctx, _| {
            run(ctx);
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(SET_VIEWPORT, "Set Viewport").with_params(&[
            Param::required("start", Type::Beats, "The song position at the left edge."),
            Param::required("zoom", Type::Number, "How many points a beat takes."),
            Param::required("top", Type::Number, "How far down the tracks are scrolled."),
            Param::required(
                "width",
                Type::Number,
                "How wide the timeline is, in points.",
            ),
        ]),
        |ctx, args| {
            let zoom: f64 = args.get("zoom")?;
            let top: f64 = args.get("top")?;
            let width: f64 = args.get("width")?;
            if zoom <= 0.0 || width < 0.0 {
                return Err(CommandError::invalid(
                    "the zoom and the width can't be negative",
                ));
            }
            ctx.document.editor_mut().viewport = Viewport {
                start: Beats::max(args.get("start")?, Beats::ZERO),
                zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
                top: top.max(0.0) as f32,
                width: width as f32,
            };
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(TOGGLE_TAKE_LANES, "Show/Hide Take Lanes").with_params(&[Param::optional(
            "tracks",
            Type::TRACKS,
            "The tracks: the selected tracks, or the track the panels show, without it.",
        )]),
        |ctx, args| {
            let editor = ctx.document.editor();
            let tracks: Vec<TrackId> = match args.opt("tracks")? {
                Some(tracks) => tracks,
                None if !editor.selection.tracks.is_empty() => {
                    editor.selection.tracks.iter().copied().collect()
                }
                None => editor.selected_track.into_iter().collect(),
            };
            let project = ctx.document.project();
            let tracks: Vec<TrackId> = tracks
                .into_iter()
                .filter(|&id| project.track(id).is_some_and(|t| !t.kind.is_bus()))
                .collect();
            let lanes = &mut ctx.document.editor_mut().take_lanes;
            // All shown, or else none: the first track decides, as Live's
            // track header buttons do for a selection.
            let show = tracks.first().is_some_and(|id| !lanes.contains(id));
            for track in tracks {
                if show {
                    lanes.insert(track);
                } else {
                    lanes.remove(&track);
                }
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_PIANO_ROLL_VIEWPORT, "Set Piano Roll Viewport").with_params(&[
            Param::required("start", Type::Beats, "The song position at the left edge."),
            Param::required("zoom", Type::Number, "How many points a beat takes."),
            Param::required(
                "top",
                Type::Number,
                "How far down the keys are scrolled from the top of the highest.",
            ),
            Param::required(
                "width",
                Type::Number,
                "How wide the notes are shown, in points.",
            ),
            Param::required(
                "height",
                Type::Number,
                "How tall the rows of keys are on screen together, in points.",
            ),
            Param::optional("key_height", Type::Number, "How tall each key's row is."),
        ]),
        |ctx, args| {
            let zoom: f64 = args.get("zoom")?;
            let top: f64 = args.get("top")?;
            let width: f64 = args.get("width")?;
            let height: f64 = args.get("height")?;
            let key_height: Option<f64> = args.opt("key_height")?;
            if zoom <= 0.0 || width < 0.0 || height < 0.0 {
                return Err(CommandError::invalid(
                    "the zoom, the width and the height can't be negative",
                ));
            }
            let roll = &mut ctx.document.editor_mut().piano_roll;
            if let Some(key_height) = key_height {
                let (min, max) = (PianoRoll::MIN_KEY_HEIGHT, PianoRoll::MAX_KEY_HEIGHT);
                roll.key_height = (key_height as f32).clamp(min, max);
            }
            roll.height = height as f32;
            let rows = roll.key_height * (f32::from(musicable_model::clip::Note::MAX_KEY) + 1.0);
            roll.viewport = Viewport {
                start: Beats::max(args.get("start")?, Beats::ZERO),
                zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
                top: (top as f32).clamp(0.0, (rows - roll.height).max(0.0)),
                width: width as f32,
            };
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_FOCUS, "Set Focus").with_params(&[Param::required(
            "panel",
            Type::Text,
            "The panel's id, such as \"arrangement\" or \"piano_roll\".",
        )]),
        |ctx, args| {
            let id: String = args.get("panel")?;
            let editor = ctx.document.editor_mut();
            let focus = Focus::on(&id)
                .ok_or_else(|| CommandError::invalid(format!("there's no panel {id}")))?;
            if focus == Focus::PIANO_ROLL && editor.piano_roll.clip.is_none() {
                return Err(CommandError::invalid("the piano roll shows no clip"));
            }
            editor.focus = focus;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_TRACK_HEIGHT, "Set Track Height").with_params(&[
            Param::required("track", Type::Track, "The track."),
            Param::required("height", Type::Number, "Its height, in points."),
        ]),
        |ctx, args| {
            let track: TrackId = args.get("track")?;
            let height: f64 = args.get("height")?;
            let (project, editor) = ctx.document.parts();
            if project.track(track).is_none() {
                return Err(CommandError::invalid(format!(
                    "there's no track {}",
                    track.0
                )));
            }
            let height = (height as f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
            editor.track_heights.insert(track, height);
            Ok(Value::None)
        },
    );
}

/// Zooms the view with the focus by `factor` around the start marker, or
/// the middle of the view if the start marker is out of sight.
fn zoom(ctx: &mut Context<'_>, factor: f64) {
    let editor = ctx.document.editor_mut();
    let start_marker = editor.start_marker;
    let viewport = match editor.focus {
        Focus::PIANO_ROLL => &mut editor.piano_roll.viewport,
        _ => &mut editor.viewport,
    };
    let at = match viewport.visible().contains(start_marker) {
        true => start_marker,
        false => viewport.beats_at(viewport.width / 2.0),
    };
    viewport.zoom_around(factor, at);
}

fn zoom_to_selection(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    match editor.focus {
        Focus::PIANO_ROLL => {
            if let Some((range, low, high)) = crate::notes::selected_range(project, editor) {
                editor.piano_roll.show_notes(range, low, high);
            }
        }
        _ => {
            if let Some(range) = editor.selected_range(project) {
                editor.viewport.show(range);
            }
        }
    }
}

/// Widens the grid by `by` steps, or narrows it: a fixed grid's step, or
/// the adaptive grid's density.
fn widen_grid(editor: &mut Editor, by: i8) {
    let grid = &mut editor.grid;
    match &mut grid.fixed {
        Some(step) => *step = Step::new(step.get().saturating_add(by)),
        None => grid.density = grid.density.saturating_add(by).clamp(-2, 2),
    }
}

/// Fixes the grid at the step it has at the zoom now, or lets it follow the
/// zoom again.
fn fix_grid(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    let grid = &mut editor.grid;
    grid.fixed = match grid.fixed {
        Some(_) => None,
        None => {
            let viewport = &editor.viewport;
            let bar = project.tempo.signature_at(viewport.start.max(Beats::ZERO));
            Some(grid.step(viewport.zoom, bar.bar_length()))
        }
    };
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use musicable_model::clip::{ClipContent, MidiClip};
    use musicable_model::time::BeatRange;
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::testing::Setup;
    use crate::{Args, Key, KeyChord, Keymap, Modifiers};

    #[test]
    fn the_lanes_key_shows_and_hides_the_selected_tracks_takes() {
        let keymap = Keymap::default_keymap();
        let chord = KeyChord::new(Key::U, Modifiers::CTRL | Modifiers::ALT);
        assert_eq!(keymap.lookup(chord), Some(&TOGGLE_TAKE_LANES));

        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        setup.select_tracks(&[keys, vocal]);
        setup.run(&TOGGLE_TAKE_LANES);
        let lanes = |setup: &Setup| setup.document.editor().take_lanes.clone();
        assert_eq!(lanes(&setup), BTreeSet::from([vocal, keys]));
        setup.select_tracks(&[]);
        setup.document.editor_mut().selected_track = Some(keys);
        setup.run(&TOGGLE_TAKE_LANES);
        assert_eq!(lanes(&setup), BTreeSet::from([vocal]));
        // Showing them isn't a change to the project.
        assert_eq!(setup.document.take_committed(), []);
        // They go with their track.
        setup.document.transact("Delete", |tx| {
            tx.remove_track(vocal);
        });
        assert!(lanes(&setup).is_empty());
    }

    #[test]
    fn zoom_keeps_the_start_marker_in_place() {
        let mut setup = Setup::new(Project::new());
        setup.document.editor_mut().start_marker = Beats::new(8);
        let x = setup.document.editor().viewport.x_of(Beats::new(8));
        setup.run(&ZOOM_IN);
        let viewport = setup.document.editor().viewport;
        assert_eq!(viewport.zoom, Viewport::default().zoom * 2.0);
        assert!((viewport.x_of(Beats::new(8)) - x).abs() < 0.01);
        setup.run(&ZOOM_OUT);
        setup.run(&ZOOM_OUT);
        assert_eq!(
            setup.document.editor().viewport.zoom,
            Viewport::default().zoom / 2.0
        );
        // None of it is a change to the project.
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn zoom_to_selection_fits_it_across() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::new(64), Beats::new(96));
        let clip = project.new_clip("K", range, ClipContent::Midi(MidiClip::default()));
        let id = clip.id;
        project.track_mut(keys).unwrap().clips.push(clip);
        let mut setup = Setup::new(project);
        assert!(!setup.applies(&ZOOM_TO_SELECTION));
        setup.select_clips(&[id]);
        setup.run(&ZOOM_TO_SELECTION);
        let visible = setup.document.editor().viewport.visible();
        assert!(visible.start < range.start && range.end < visible.end);
        assert!(visible.len() < Beats::new(40), "{visible:?}");
    }

    #[test]
    fn the_grid_keys_change_it_as_live_s_do() {
        let mut setup = Setup::new(Project::new());
        setup.run(&NARROW_GRID);
        setup.run(&NARROW_GRID);
        setup.run(&NARROW_GRID);
        assert_eq!(setup.document.editor().grid.density, -2);
        setup.run(&WIDEN_GRID);
        setup.run(&TRIPLET_GRID);
        setup.run(&TOGGLE_SNAP);
        let grid = setup.document.editor().grid;
        assert_eq!((grid.density, grid.triplet, grid.snap), (-1, true, false));
        // Fixing it keeps the step it has at this zoom: at 15.6 points a
        // beat, density -1 makes it quarter-note triplets.
        setup.run(&FIXED_GRID);
        assert_eq!(setup.document.editor().grid.fixed, Some(Step::QUARTER));
        setup.run(&WIDEN_GRID);
        assert_eq!(setup.document.editor().grid.fixed, Some(Step::new(-1)));
        setup.run(&FIXED_GRID);
        assert_eq!(setup.document.editor().grid.fixed, None);

        let keymap = crate::Keymap::default_keymap();
        let chords: Vec<String> = [
            NARROW_GRID,
            WIDEN_GRID,
            TRIPLET_GRID,
            TOGGLE_SNAP,
            FIXED_GRID,
        ]
        .iter()
        .flat_map(|id| keymap.chords_for(id).map(|c| c.to_string()))
        .collect();
        assert_eq!(chords, ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5"]);
    }

    #[test]
    fn the_viewport_and_track_heights_are_set_by_the_arrangement() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        let args = Args::new()
            .with("start", Beats::new(4))
            .with("zoom", 1e9)
            .with("top", -5.0)
            .with("width", 640.0);
        setup.run_with(&SET_VIEWPORT, args).unwrap();
        let viewport = setup.document.editor().viewport;
        assert_eq!(viewport.start, Beats::new(4));
        assert_eq!(viewport.zoom, Viewport::MAX_ZOOM);
        assert_eq!((viewport.top, viewport.width), (0.0, 640.0));

        let height = |h: f64| Args::new().with("track", keys).with("height", h);
        setup.run_with(&SET_TRACK_HEIGHT, height(1.0)).unwrap();
        assert_eq!(
            setup.document.editor().track_heights[&keys],
            MIN_TRACK_HEIGHT
        );
        setup.run_with(&SET_TRACK_HEIGHT, height(90.0)).unwrap();
        assert_eq!(setup.document.editor().track_heights[&keys], 90.0);
        setup.run(&TOGGLE_FOLLOW);
        assert!(!setup.document.editor().follow);
        setup.run(&TOGGLE_MIXER);
        setup.run(&TOGGLE_DEVICES);
        setup.run(&TOGGLE_BROWSER);
        setup.run(&TOGGLE_PIANO_ROLL);
        setup.run(&SWITCH_DEVICES_PIANO_ROLL);
        setup.run(&MOVE_TO_NEW_WINDOW);
        setup.run(&MOVE_TO_MAIN_WINDOW);
        assert_eq!(
            setup.requests,
            [
                Request::TogglePanel("mixer"),
                Request::TogglePanel("devices"),
                Request::TogglePanel("browser"),
                Request::TogglePanel("piano_roll"),
                Request::SwitchDevicesPianoRoll,
                Request::MoveToNewWindow,
                Request::MoveToMainWindow,
            ]
        );
    }

    /// Keys, a MIDI track with a clip from beat 8 to 16 holding notes on D3
    /// (62) from 1 to 2 in its own time, and on A3 (69) from 3 to 4.
    fn with_notes() -> (Setup, musicable_model::clip::ClipId) {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut midi = MidiClip::default();
        for (start, key) in [(1, 62), (3, 69)] {
            midi.add_note(Beats::new(start), Beats::new(1), key, 0.5);
        }
        let range = BeatRange::new(Beats::new(8), Beats::new(16));
        let clip = project.new_clip("K", range, ClipContent::Midi(midi));
        let id = clip.id;
        project.track_mut(keys).unwrap().clips.push(clip);
        (Setup::new(project), id)
    }

    #[test]
    fn zooming_acts_on_the_view_with_the_focus() {
        let (mut setup, clip) = with_notes();
        let args = Args::new()
            .with("notes", vec![musicable_model::clip::NoteId(1)])
            .with("clip", clip);
        setup.run_with(&crate::notes::SELECT, args).unwrap();
        let arrangement = setup.document.editor().viewport;
        let roll = setup.document.editor().piano_roll.viewport;
        setup.run(&ZOOM_IN);
        let editor = setup.document.editor();
        assert_eq!(editor.viewport, arrangement);
        assert_eq!(editor.piano_roll.viewport.zoom, roll.zoom * 2.0);
        // A3 alone, from song position 11 to 12, across the piano roll, with
        // its key in the middle.
        assert!(setup.applies(&ZOOM_TO_SELECTION));
        setup.run(&ZOOM_TO_SELECTION);
        let roll = &setup.document.editor().piano_roll;
        let visible = roll.viewport.visible();
        assert!(visible.start < Beats::new(11) && Beats::new(12) < visible.end);
        assert!(visible.len() < Beats::new(2), "{visible:?}");
        let middle = roll.viewport.top + roll.height / 2.0;
        let a3 = (127.0 - 69.0) * roll.key_height;
        assert!((a3..a3 + roll.key_height).contains(&middle), "{middle}");

        let focus = |panel: &str| Args::new().with("panel", panel);
        setup.run_with(&SET_FOCUS, focus("arrangement")).unwrap();
        setup.run(&ZOOM_IN);
        assert_eq!(
            setup.document.editor().viewport.zoom,
            arrangement.zoom * 2.0
        );
        // The mixer zooms the arrangement, which it shows the tracks of.
        setup.run_with(&SET_FOCUS, focus("mixer")).unwrap();
        setup.run(&ZOOM_IN);
        assert_eq!(
            setup.document.editor().viewport.zoom,
            arrangement.zoom * 4.0
        );
        assert_eq!(
            setup.run_with(&SET_FOCUS, focus("later")),
            Err(CommandError::invalid("there's no panel later"))
        );
        setup.document.transact("Delete", |tx| {
            tx.remove_clip(clip);
        });
        assert_eq!(
            setup.run_with(&SET_FOCUS, focus("piano_roll")),
            Err(CommandError::invalid("the piano roll shows no clip"))
        );
    }

    #[test]
    fn the_piano_roll_sets_its_viewport() {
        let mut setup = Setup::new(Project::new());
        let args = Args::new()
            .with("start", Beats::new(-4))
            .with("zoom", 80.0)
            .with("top", 1e6)
            .with("width", 700.0)
            .with("height", 300.0)
            .with("key_height", 100.0);
        setup.run_with(&SET_PIANO_ROLL_VIEWPORT, args).unwrap();
        let roll = &setup.document.editor().piano_roll;
        assert_eq!(roll.key_height, PianoRoll::MAX_KEY_HEIGHT);
        assert_eq!(roll.height, 300.0);
        let viewport = roll.viewport;
        assert_eq!((viewport.start, viewport.zoom), (Beats::ZERO, 80.0));
        // Scrolled down to the lowest key, and no further.
        assert_eq!(viewport.top, 128.0 * PianoRoll::MAX_KEY_HEIGHT - 300.0);
        assert_eq!(setup.document.take_committed(), []);
    }
}

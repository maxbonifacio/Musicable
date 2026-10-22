//! The view commands: what the window shows, and the arrangement's zoom,
//! scrolling and grid.
//!
//! Commands can't show a window, so the command palette and the mixer ask
//! the app to, with a [`Request`]. The zoom, the scrolling and the grid are
//! the arrangement's, in the [`Editor`](crate::Editor).

use musicable_model::TrackId;
use musicable_model::time::Beats;

use crate::editor::{Editor, Step, Viewport};
use crate::{Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value, When};

/// Shows the command palette, which finds any command by name and runs it.
pub const COMMAND_PALETTE: CommandId = CommandId::new("view.command_palette");
/// Shows or hides the mixer, under the arrangement.
pub const TOGGLE_MIXER: CommandId = CommandId::new("view.toggle_mixer");
/// Zooms the arrangement in, around the start marker.
pub const ZOOM_IN: CommandId = CommandId::new("view.zoom_in");
/// Zooms the arrangement out, around the start marker.
pub const ZOOM_OUT: CommandId = CommandId::new("view.zoom_out");
/// Zooms the arrangement to show the selection across it.
pub const ZOOM_TO_SELECTION: CommandId = CommandId::new("view.zoom_to_selection");
/// Turns on or off scrolling the arrangement to keep the playhead in sight.
pub const TOGGLE_FOLLOW: CommandId = CommandId::new("view.toggle_follow");
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
        When::ClipsOrTimeSelected,
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
const REQUESTS: [(CommandId, &str, Request); 2] = [
    (
        COMMAND_PALETTE,
        "Command Palette...",
        Request::CommandPalette,
    ),
    (TOGGLE_MIXER, "Mixer", Request::TogglePanel("mixer")),
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
            Param::required("width", Type::Number, "How wide the timeline is, in points."),
        ]),
        |ctx, args| {
            let zoom: f64 = args.get("zoom")?;
            let top: f64 = args.get("top")?;
            let width: f64 = args.get("width")?;
            if zoom <= 0.0 || width < 0.0 {
                return Err(CommandError::invalid("the zoom and the width can't be negative"));
            }
            ctx.document.editor_mut().viewport = Viewport {
                start: args.get("start")?,
                zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
                top: top.max(0.0) as f32,
                width: width as f32,
            };
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
                return Err(CommandError::invalid(format!("there's no track {}", track.0)));
            }
            let height = (height as f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
            editor.track_heights.insert(track, height);
            Ok(Value::None)
        },
    );
}

/// Zooms by `factor` around the start marker, or the middle of the timeline
/// if the start marker is out of sight.
fn zoom(ctx: &mut Context<'_>, factor: f64) {
    let editor = ctx.document.editor_mut();
    let viewport = &mut editor.viewport;
    let at = match viewport.visible().contains(editor.start_marker) {
        true => editor.start_marker,
        false => viewport.beats_at(viewport.width / 2.0),
    };
    viewport.zoom_around(factor, at);
}

fn zoom_to_selection(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    if let Some(range) = editor.selected_range(project) {
        editor.viewport.show(range);
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
    use musicable_model::clip::{ClipContent, MidiClip};
    use musicable_model::time::BeatRange;
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::Args;
    use crate::testing::Setup;

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
        let chords: Vec<String> = [NARROW_GRID, WIDEN_GRID, TRIPLET_GRID, TOGGLE_SNAP, FIXED_GRID]
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
        assert_eq!(setup.document.editor().track_heights[&keys], MIN_TRACK_HEIGHT);
        setup.run_with(&SET_TRACK_HEIGHT, height(90.0)).unwrap();
        assert_eq!(setup.document.editor().track_heights[&keys], 90.0);
        setup.run(&TOGGLE_FOLLOW);
        assert!(!setup.document.editor().follow);
        setup.run(&TOGGLE_MIXER);
        assert_eq!(setup.requests, [Request::TogglePanel("mixer")]);
    }
}

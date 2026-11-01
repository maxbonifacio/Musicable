//! The view commands: what the window shows, the zoom and scrolling of the
//! arrangement, the piano roll and the Sample Editor, the grid, and which
//! panel has the focus.
//!
//! Commands can't show a window, so the command palette and the mixer ask
//! the app to, with a [`Request`]. The zoom, the scrolling and the grid are
//! in the [`Editor`](crate::Editor). Zooming acts on the view with the focus:
//! the arrangement, the piano roll or the Sample Editor.

use musicable_model::time::Beats;
use musicable_model::{Chain, TrackId};

use crate::editor::{Editor, Focus, FollowMode, PianoRoll, Step, Viewport};
use crate::navigation::{Panel, Record, song_range};
use crate::panel;
use crate::{
    Args, Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value, When,
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
/// view for MIDI clips.
pub const TOGGLE_PIANO_ROLL: CommandId = CommandId::new("view.toggle_piano_roll");
/// Shows or hides the Sample Editor, in a tab beside the devices: Live's
/// clip view for audio clips.
pub const TOGGLE_SAMPLE_EDITOR: CommandId = CommandId::new("view.toggle_sample_editor");
/// Shows or hides the editor that fits the clip selected or opened last:
/// the piano roll, or the Sample Editor for an audio clip.
pub const TOGGLE_CLIP_EDITOR: CommandId = CommandId::new("view.toggle_clip_editor");
/// Shows or hides the History panel, which lists the steps of the undo
/// history, in a tab beside the browser.
pub const TOGGLE_HISTORY: CommandId = CommandId::new("view.toggle_history");
/// Shows or hides the overview of the whole song over the arrangement.
pub const TOGGLE_OVERVIEW: CommandId = CommandId::new("view.toggle_overview");
/// Shows or hides the Routing panel, which lists every connection in the
/// project, in a tab beside the mixer: Live's In/Out section, for the whole
/// project.
pub const TOGGLE_ROUTING: CommandId = CommandId::new("view.toggle_routing");
/// Shows or hides the Performance panel, which lists what each device and
/// each track's mixer take of the CPU, busiest first, in a tab beside the
/// mixer.
pub const TOGGLE_PERFORMANCE: CommandId = CommandId::new("view.toggle_performance");
/// Brings the editor that fits the clip selected or opened last, the piano
/// roll or the Sample Editor, in front of the devices, or the devices in
/// front of it: Live's Toggle Between Device/Clip View.
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
/// Zooms the arrangement to show the selection across it, the piano roll to
/// show its selected notes, or the Sample Editor its selected warp markers.
pub const ZOOM_TO_SELECTION: CommandId = CommandId::new("view.zoom_to_selection");
/// Goes back to the view before the last zoom or scroll of the view with
/// the focus: Live's Zoom Back.
pub const ZOOM_BACK: CommandId = CommandId::new("view.zoom_back");
/// Goes forward again to the view Zoom Back went back from.
pub const ZOOM_FORWARD: CommandId = CommandId::new("view.zoom_forward");
/// Zooms and scrolls the arrangement to show the whole song across it, or
/// the piano roll or the Sample Editor their whole clip, while they have
/// the focus. It leaves the tracks' heights alone.
pub const FIT_SONG_WIDTH: CommandId = CommandId::new("view.fit_song_width");
/// Makes the arrangement's tracks fill its height together, and scrolls to
/// the top. It leaves the zoom alone.
pub const FIT_TRACK_HEIGHTS: CommandId = CommandId::new("view.fit_track_heights");
/// Makes the selected tracks, or the track the panels show, the next size
/// taller.
pub const TRACKS_TALLER: CommandId = CommandId::new("view.tracks_taller");
/// Makes the selected tracks, or the track the panels show, the next size
/// shorter.
pub const TRACKS_SHORTER: CommandId = CommandId::new("view.tracks_shorter");
/// Gives tracks their default height back: the selected tracks, or the
/// track the panels show.
pub const RESET_TRACK_HEIGHT: CommandId = CommandId::new("view.reset_track_height");
/// Makes the selected tracks, or every track with none selected, the
/// Small size.
pub const SMALL_TRACKS: CommandId = CommandId::new("view.small_tracks");
/// Makes the selected tracks, or every track, the Medium size.
pub const MEDIUM_TRACKS: CommandId = CommandId::new("view.medium_tracks");
/// Makes the selected tracks, or every track, the Large size.
pub const LARGE_TRACKS: CommandId = CommandId::new("view.large_tracks");
/// Makes the selected tracks, or every track, the Huge size.
pub const HUGE_TRACKS: CommandId = CommandId::new("view.huge_tracks");
/// Makes the selected tracks fill the arrangement's height together, and
/// every other track as short as tracks go.
pub const FIT_SELECTED_TRACKS: CommandId = CommandId::new("view.fit_selected_tracks");
/// Turns on or off making the selected track Large while it's selected.
pub const TOGGLE_AUTO_ZOOM: CommandId = CommandId::new("view.toggle_auto_zoom");
/// Scrolls the view with the focus to the playhead, and lets Follow follow
/// it again.
pub const SCROLL_TO_PLAYHEAD: CommandId = CommandId::new("view.scroll_to_playhead");
/// Tells the commands the sizes the arrangement draws its lanes at, for
/// fitting tracks to its height.
pub const SET_LANE_SIZES: CommandId = CommandId::new("view.set_lane_sizes");
/// Turns on or off scrolling the panel with the focus to keep the playhead
/// in sight: the arrangement, the piano roll or the Sample Editor.
pub const TOGGLE_FOLLOW: CommandId = CommandId::new("view.toggle_follow");
/// Switches Follow between turning pages and scrolling continuously, in
/// every panel.
pub const SWITCH_FOLLOW_MODE: CommandId = CommandId::new("view.switch_follow_mode");
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
/// Sets the height of a track in the arrangement, or of the master's row.
pub const SET_TRACK_HEIGHT: CommandId = CommandId::new("view.set_track_height");
/// Sets the piano roll's zoom, scrolling and key height.
pub const SET_PIANO_ROLL_VIEWPORT: CommandId = CommandId::new("view.set_piano_roll_viewport");
/// Sets the Sample Editor's zoom and scrolling.
pub const SET_SAMPLE_EDITOR_VIEWPORT: CommandId = CommandId::new("view.set_sample_editor_viewport");
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
const COMMANDS: [(CommandId, &str, When, Handler); 20] = [
    (ZOOM_IN, "Zoom In", When::Always, |ctx| zoom(ctx, ZOOM_STEP)),
    (ZOOM_OUT, "Zoom Out", When::Always, |ctx| {
        zoom(ctx, 1.0 / ZOOM_STEP)
    }),
    (
        ZOOM_TO_SELECTION,
        "Zoom to Selection",
        When::CanZoomToSelection,
        zoom_to_selection,
    ),
    (ZOOM_BACK, "Zoom Back", When::CanZoomBack, |ctx| {
        step_view(ctx, false)
    }),
    (ZOOM_FORWARD, "Zoom Forward", When::CanZoomForward, |ctx| {
        step_view(ctx, true)
    }),
    (FIT_SONG_WIDTH, "Fit Song Width", When::Always, fit_width),
    (
        FIT_TRACK_HEIGHTS,
        "Fit Track Heights",
        When::Always,
        fit_heights,
    ),
    (TRACKS_TALLER, "Make Tracks Taller", When::Always, |ctx| {
        step_heights(ctx, true)
    }),
    (TRACKS_SHORTER, "Make Tracks Shorter", When::Always, |ctx| {
        step_heights(ctx, false)
    }),
    (
        SCROLL_TO_PLAYHEAD,
        "Scroll to Playhead",
        When::Always,
        scroll_to_playhead,
    ),
    (TOGGLE_FOLLOW, "Follow", When::Always, |ctx| {
        let editor = ctx.document.editor_mut();
        let history = editor.navigation.of_mut(Panel::of(editor.focus));
        history.follow = !history.follow;
        // Turned on, it follows the playhead from the next frame.
        history.suspended = false;
    }),
    (TOGGLE_AUTO_ZOOM, "Auto Zoom Tracks", When::Always, |ctx| {
        let editor = ctx.document.editor_mut();
        editor.behaviour.auto_zoom_tracks = !editor.behaviour.auto_zoom_tracks;
        editor.sized_by_hand = None;
        ctx.requests.push(Request::SaveBehaviour);
    }),
    (
        SWITCH_FOLLOW_MODE,
        "Switch Follow Mode",
        When::Always,
        |ctx| {
            let behaviour = &mut ctx.document.editor_mut().behaviour;
            behaviour.follow_mode = match behaviour.follow_mode {
                FollowMode::Page => FollowMode::Continuous,
                FollowMode::Continuous => FollowMode::Page,
            };
            ctx.requests.push(Request::SaveBehaviour);
        },
    ),
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
    (TOGGLE_CLIP_EDITOR, "Clip Editor", When::Always, |ctx| {
        let panel = ctx.document.editor().clip_editor;
        ctx.requests.push(Request::TogglePanel(panel));
    }),
    (
        SWITCH_DEVICES_PIANO_ROLL,
        "Switch Devices and Clip Editor",
        When::Always,
        |ctx| {
            let panel = ctx.document.editor().clip_editor;
            ctx.requests.push(Request::SwitchDevicesClipEditor(panel));
        },
    ),
];

/// The view commands that ask the app for something.
const REQUESTS: [(CommandId, &str, Request); 12] = [
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
    (
        TOGGLE_SAMPLE_EDITOR,
        "Sample Editor",
        Request::TogglePanel(panel::SAMPLE_EDITOR),
    ),
    (TOGGLE_HISTORY, "History", Request::TogglePanel("history")),
    (TOGGLE_OVERVIEW, "Overview", Request::ToggleOverview),
    (
        TOGGLE_ROUTING,
        "Routing",
        Request::TogglePanel(panel::ROUTING),
    ),
    (
        TOGGLE_PERFORMANCE,
        "Performance",
        Request::TogglePanel(panel::PERFORMANCE),
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

/// The height presets' commands, Small to Huge, in the order of
/// [`Behaviour::presets`](crate::editor::Behaviour::presets).
pub const PRESETS: [(CommandId, &str); 4] = [
    (SMALL_TRACKS, "Small Tracks"),
    (MEDIUM_TRACKS, "Medium Tracks"),
    (LARGE_TRACKS, "Large Tracks"),
    (HUGE_TRACKS, "Huge Tracks"),
];

/// The tracks a height preset run with `args` sizes: those it names, or
/// every track with `all`, or else the selected tracks, or every track if
/// none is selected.
fn preset_tracks(ctx: &Context<'_>, args: &Args) -> Result<Vec<TrackId>, CommandError> {
    let project = ctx.document.project();
    let every = || project.tracks.iter().map(|t| t.id).collect();
    if args.opt("all")? == Some(true) {
        return Ok(every());
    }
    if let Some(tracks) = args.opt::<Vec<TrackId>>("tracks")? {
        if let Some(id) = tracks.iter().find(|&&id| project.track(id).is_none()) {
            return Err(CommandError::invalid(format!("there's no track {}", id.0)));
        }
        return Ok(tracks);
    }
    let selected = &ctx.document.editor().selection.tracks;
    Ok(match selected.is_empty() {
        true => every(),
        false => selected.iter().copied().collect(),
    })
}

/// The `history` argument the view-setting commands take: how the change
/// counts in the view's history and for Follow.
const HISTORY: Param = Param::optional(
    "history",
    Type::Choice(Record::NAMES),
    "How it counts: \"step\", a step of the view's history, which Zoom Back goes back over, \
     as without it; \"gesture\", part of a gesture going on, which is a step once a call \
     without it ends the gesture; \"follow\", Follow moving it; or \"none\", movement the \
     user didn't make.",
);

/// How the view-setting command run with `args` counts.
fn record(args: &Args) -> Result<Record, CommandError> {
    let name: Option<String> = args.opt("history")?;
    Ok(name
        .map_or(Some(Record::Step), |name| Record::from_name(&name))
        .unwrap_or_default())
}

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
            HISTORY,
        ]),
        |ctx, args| {
            let playing = ctx.transport.is_playing();
            set_viewport(ctx.document.editor_mut(), args, playing)?;
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
            let tracks: Vec<TrackId> = match args.opt("tracks")? {
                Some(tracks) => tracks,
                None => crate::track::chosen(ctx.document.editor()),
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
            HISTORY,
        ]),
        |ctx, args| {
            let playing = ctx.transport.is_playing();
            set_piano_roll_viewport(ctx.document.editor_mut(), args, playing)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_SAMPLE_EDITOR_VIEWPORT, "Set Sample Editor Viewport").with_params(&[
            Param::required("start", Type::Beats, "The song position at the left edge."),
            Param::required("zoom", Type::Number, "How many points a beat takes."),
            Param::required(
                "width",
                Type::Number,
                "How wide the waveform is shown, in points.",
            ),
            HISTORY,
        ]),
        |ctx, args| {
            let zoom: f64 = args.get("zoom")?;
            let width: f64 = args.get("width")?;
            if zoom <= 0.0 || width < 0.0 {
                return Err(CommandError::invalid(
                    "the zoom and the width can't be negative",
                ));
            }
            let start = Beats::max(args.get("start")?, Beats::ZERO);
            let record = record(args)?;
            let playing = ctx.transport.is_playing();
            let editor = ctx.document.editor_mut();
            editor.navigate(Panel::SampleEditor, record, playing, |editor| {
                editor.sample_editor.viewport = Viewport {
                    start,
                    zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
                    top: 0.0,
                    width: width as f32,
                };
            });
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
            if focus == Focus::SAMPLE_EDITOR && editor.sample_editor.clip.is_none() {
                return Err(CommandError::invalid("the Sample Editor shows no clip"));
            }
            editor.focus = focus;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_TRACK_HEIGHT, "Set Track Height").with_params(&[
            Param::optional(
                "track",
                Type::Track,
                "The track: the one the panels show without it.",
            ),
            Param::optional("master", Type::Bool, "The master's row instead."),
            Param::optional(
                "all",
                Type::Bool,
                "Every track instead, as Alt-dragging an edge does.",
            ),
            Param::required("height", Type::Number, "Its height, in points."),
            HISTORY,
        ]),
        |ctx, args| {
            let height: f64 = args.get("height")?;
            let height = (height as f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
            let chains = chains(ctx, args, false)?;
            let record = record(args)?;
            let playing = ctx.transport.is_playing();
            let editor = ctx.document.editor_mut();
            editor.navigate(Panel::Arrangement, record, playing, |editor| {
                for chain in chains {
                    editor.set_height(chain, Some(height));
                }
            });
            Ok(Value::None)
        },
    );
    for (index, (id, title)) in PRESETS.into_iter().enumerate() {
        registry.add(
            Command::new(id, title).with_params(&[
                Param::optional(
                    "tracks",
                    Type::TRACKS,
                    "The tracks: the selected tracks without it, or every track if none is.",
                ),
                Param::optional("all", Type::Bool, "Every track, as with Alt held."),
            ]),
            move |ctx, args| {
                let tracks = preset_tracks(ctx, args)?;
                let playing = ctx.transport.is_playing();
                let editor = ctx.document.editor_mut();
                let height = editor.behaviour.presets()[index];
                editor.navigate(Panel::Arrangement, Record::Step, playing, |editor| {
                    for id in tracks {
                        editor.set_height(Chain::Track(id), Some(height));
                    }
                });
                Ok(Value::None)
            },
        );
    }
    registry.add(
        Command::new(FIT_SELECTED_TRACKS, "Fit Selected Tracks")
            .when(When::TracksSelected)
            .with_params(&[Param::optional(
                "tracks",
                Type::TRACKS,
                "The tracks to fit: the selected tracks without it.",
            )]),
        |ctx, args| {
            let tracks: Vec<TrackId> = match args.opt("tracks")? {
                Some(tracks) => tracks,
                None => ctx
                    .document
                    .editor()
                    .selection
                    .tracks
                    .iter()
                    .copied()
                    .collect(),
            };
            let project = ctx.document.project();
            if let Some(id) = tracks.iter().find(|&&id| project.track(id).is_none()) {
                return Err(CommandError::invalid(format!("there's no track {}", id.0)));
            }
            if tracks.is_empty() {
                return Err(CommandError::invalid("no track is selected"));
            }
            let playing = ctx.transport.is_playing();
            let (project, editor) = ctx.document.parts();
            let mut fits = true;
            editor.navigate(Panel::Arrangement, Record::Step, playing, |editor| {
                fits = editor.fit_selected_tracks(project, &tracks);
            });
            if !fits {
                ctx.requests.push(Request::Notice(
                    "The selected tracks don't fit at the smallest height. Scroll for the rest."
                        .into(),
                ));
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RESET_TRACK_HEIGHT, "Reset Track Height").with_params(&[
            Param::optional(
                "track",
                Type::Track,
                "The track: the selected tracks, or the one the panels show, without it.",
            ),
            Param::optional("master", Type::Bool, "The master's row instead."),
            Param::optional("all", Type::Bool, "Every track instead."),
        ]),
        |ctx, args| {
            let chains = chains(ctx, args, true)?;
            let playing = ctx.transport.is_playing();
            let editor = ctx.document.editor_mut();
            editor.navigate(Panel::Arrangement, Record::Step, playing, |editor| {
                for chain in chains {
                    editor.set_height(chain, None);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_LANE_SIZES, "Set Lane Sizes").with_params(&[
            Param::required(
                "height",
                Type::Number,
                "How tall the tracks' part of the arrangement is, over the master's, in points.",
            ),
            Param::optional(
                "track",
                Type::Number,
                "A track's height where it hasn't one of its own.",
            ),
            Param::optional("take", Type::Number, "A take lane's height."),
            Param::optional("automation", Type::Number, "An automation lane's height."),
        ]),
        |ctx, args| {
            let size = |name| -> Result<Option<f32>, CommandError> {
                match args.opt::<f64>(name)? {
                    Some(size) if size < 0.0 || !size.is_finite() => {
                        Err(CommandError::invalid("the sizes can't be negative"))
                    }
                    size => Ok(size.map(|size| size as f32)),
                }
            };
            let height = size("height")?.unwrap_or_default();
            let (track, take, automation) = (size("track")?, size("take")?, size("automation")?);
            let lanes = &mut ctx.document.editor_mut().lanes;
            lanes.height = height;
            lanes.track = track.unwrap_or(lanes.track);
            lanes.take = take.unwrap_or(lanes.take);
            lanes.automation = automation.unwrap_or(lanes.automation);
            Ok(Value::None)
        },
    );
}

/// Sets the arrangement's viewport as [`SET_VIEWPORT`] does with `args`,
/// while the song plays or not.
pub fn set_viewport(editor: &mut Editor, args: &Args, playing: bool) -> Result<(), CommandError> {
    let zoom: f64 = args.get("zoom")?;
    let top: f64 = args.get("top")?;
    let width: f64 = args.get("width")?;
    if zoom <= 0.0 || width < 0.0 {
        return Err(CommandError::invalid(
            "the zoom and the width can't be negative",
        ));
    }
    let start = Beats::max(args.get("start")?, Beats::ZERO);
    let record = record(args)?;
    editor.navigate(Panel::Arrangement, record, playing, |editor| {
        editor.viewport = Viewport {
            start,
            zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
            top: top.max(0.0) as f32,
            width: width as f32,
        };
    });
    Ok(())
}

/// Sets the piano roll's viewport as [`SET_PIANO_ROLL_VIEWPORT`] does with
/// `args`, while the song plays or not.
pub fn set_piano_roll_viewport(
    editor: &mut Editor,
    args: &Args,
    playing: bool,
) -> Result<(), CommandError> {
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
    let start = Beats::max(args.get("start")?, Beats::ZERO);
    let record = record(args)?;
    editor.navigate(Panel::PianoRoll, record, playing, |editor| {
        let roll = &mut editor.piano_roll;
        if let Some(key_height) = key_height {
            let (min, max) = (PianoRoll::MIN_KEY_HEIGHT, PianoRoll::MAX_KEY_HEIGHT);
            roll.key_height = (key_height as f32).clamp(min, max);
        }
        roll.height = height as f32;
        let rows = roll.key_height * (f32::from(musicable_model::clip::Note::MAX_KEY) + 1.0);
        roll.viewport = Viewport {
            start,
            zoom: zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM),
            top: (top as f32).clamp(0.0, (rows - roll.height).max(0.0)),
            width: width as f32,
        };
    });
    Ok(())
}

/// The rows the height commands run with `args` change: the track or the
/// master's row they name, every track, or else the track the panels show,
/// or, if `selected`, the selected tracks before it.
fn chains(ctx: &Context<'_>, args: &Args, selected: bool) -> Result<Vec<Chain>, CommandError> {
    let project = ctx.document.project();
    let editor = ctx.document.editor();
    if args.opt("all")? == Some(true) {
        return Ok(project.tracks.iter().map(|t| Chain::Track(t.id)).collect());
    }
    if args.opt("master")? == Some(true) {
        return Ok(vec![Chain::Master]);
    }
    if let Some(track) = args.opt::<TrackId>("track")? {
        if project.track(track).is_none() {
            return Err(CommandError::invalid(format!(
                "there's no track {}",
                track.0
            )));
        }
        return Ok(vec![Chain::Track(track)]);
    }
    let chosen = match selected {
        true => chosen_rows(editor),
        false => editor.selected_track.into_iter().collect(),
    };
    match chosen.is_empty() {
        true => Err(CommandError::invalid("no track is selected")),
        false => Ok(chosen),
    }
}

/// The rows the height keys change: the selected tracks, or else the track
/// the panels show, which can be the master.
fn chosen_rows(editor: &Editor) -> Vec<Chain> {
    match editor.selection.tracks.is_empty() {
        false => editor
            .selection
            .tracks
            .iter()
            .map(|&id| Chain::Track(id))
            .collect(),
        true => editor.selected_track.into_iter().collect(),
    }
}

/// Zooms the view with the focus by `factor` around the playhead while it
/// plays in sight, or else the start marker, or the middle of the view if
/// the start marker is out of sight too.
fn zoom(ctx: &mut Context<'_>, factor: f64) {
    let playing = ctx.transport.is_playing();
    let playhead = ctx.transport.playhead();
    let editor = ctx.document.editor_mut();
    let panel = Panel::of(editor.focus);
    let start_marker = editor.start_marker;
    editor.navigate(panel, Record::Step, playing, |editor| {
        let viewport = editor.viewport_mut(panel);
        let visible = viewport.visible();
        let at = if playing && visible.contains(playhead) {
            playhead
        } else if visible.contains(start_marker) {
            start_marker
        } else {
            viewport.beats_at(viewport.width / 2.0)
        };
        viewport.zoom_around(factor, at);
    });
}

fn zoom_to_selection(ctx: &mut Context<'_>) {
    let playing = ctx.transport.is_playing();
    let (project, editor) = ctx.document.parts();
    let panel = Panel::of(editor.focus);
    editor.navigate(panel, Record::Step, playing, |editor| match panel {
        Panel::PianoRoll => {
            if let Some((range, low, high)) = crate::notes::selected_range(project, editor) {
                editor.piano_roll.show_notes(range, low, high);
            }
        }
        Panel::SampleEditor => {
            if let Some(range) = crate::warp::selected_range(project, editor) {
                editor.sample_editor.viewport.show(range);
            }
        }
        Panel::Arrangement => {
            if let Some(range) = editor.selected_range(project) {
                editor.viewport.show(range);
            }
        }
    });
}

/// Goes back to the view before the last step of the view with the focus,
/// or forward again.
fn step_view(ctx: &mut Context<'_>, forward: bool) {
    let playing = ctx.transport.is_playing();
    let (project, editor) = ctx.document.parts();
    let panel = Panel::of(editor.focus);
    editor.step_view(project, panel, forward, playing);
}

/// Shows the whole song across the arrangement, or the whole clip across
/// the piano roll or the Sample Editor.
fn fit_width(ctx: &mut Context<'_>) {
    let playing = ctx.transport.is_playing();
    let (project, editor) = ctx.document.parts();
    let panel = Panel::of(editor.focus);
    let clip = |id: Option<musicable_model::clip::ClipId>| {
        id.and_then(|id| project.find_clip(id))
            .map(|(_, clip)| clip.range())
    };
    let range = match panel {
        Panel::Arrangement => Some(song_range(project)),
        Panel::PianoRoll => clip(editor.piano_roll.clip),
        Panel::SampleEditor => clip(editor.sample_editor.clip),
    };
    if let Some(range) = range {
        editor.navigate(panel, Record::Step, playing, |editor| {
            editor.viewport_mut(panel).show(range);
        });
    }
}

/// Makes the arrangement's tracks fill its height, and says how many are in
/// sight when they can't all be.
fn fit_heights(ctx: &mut Context<'_>) {
    let playing = ctx.transport.is_playing();
    let (project, editor) = ctx.document.parts();
    let mut fitted = (0, 0);
    editor.navigate(Panel::Arrangement, Record::Step, playing, |editor| {
        fitted = editor.fit_track_heights(project);
    });
    let (shown, count) = fitted;
    if shown < count {
        ctx.requests.push(Request::Notice(format!(
            "{shown} of {count} tracks are in sight at the smallest height. Scroll for the rest."
        )));
    }
}

/// Makes the selected tracks, or the track the panels show, the next of the
/// behaviour's sizes taller, or shorter.
fn step_heights(ctx: &mut Context<'_>, taller: bool) {
    let playing = ctx.transport.is_playing();
    let editor = ctx.document.editor_mut();
    let sizes = editor.behaviour.track_sizes();
    let chains = chosen_rows(editor);
    editor.navigate(Panel::Arrangement, Record::Step, playing, |editor| {
        for chain in chains {
            let height = editor.height_of(chain);
            let next = match taller {
                true => (sizes.iter().copied())
                    .find(|&size| size > height + 0.5)
                    .unwrap_or(MAX_TRACK_HEIGHT),
                false => (sizes.iter().rev().copied())
                    .find(|&size| size < height - 0.5)
                    .unwrap_or(MIN_TRACK_HEIGHT),
            };
            editor.set_height(chain, Some(next));
        }
    });
}

/// Scrolls the view with the focus so the playhead is a little way in from
/// its left edge, or at Continuous Follow's place, and lets Follow follow
/// again there.
fn scroll_to_playhead(ctx: &mut Context<'_>) {
    let playing = ctx.transport.is_playing();
    let playhead = ctx.transport.playhead();
    let editor = ctx.document.editor_mut();
    let panel = Panel::of(editor.focus);
    let fraction = match editor.behaviour.follow_mode {
        FollowMode::Page => 1.0 / 20.0,
        FollowMode::Continuous => editor.behaviour.follow_place.fraction(),
    };
    editor.navigate(panel, Record::Step, playing, |editor| {
        editor.viewport_mut(panel).place(playhead, fraction);
    });
    editor.navigation.of_mut(panel).suspended = false;
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
        setup.document.editor_mut().selected_track = Some(Chain::Track(keys));
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
        let track = Chain::Track(keys);
        setup.run_with(&SET_TRACK_HEIGHT, height(1.0)).unwrap();
        assert_eq!(
            setup.document.editor().track_heights[&track],
            MIN_TRACK_HEIGHT
        );
        setup.run_with(&SET_TRACK_HEIGHT, height(90.0)).unwrap();
        assert_eq!(setup.document.editor().track_heights[&track], 90.0);
        // The master's row, and without either, the selected track's.
        let master = Args::new().with("master", true).with("height", 1e4);
        setup.run_with(&SET_TRACK_HEIGHT, master).unwrap();
        let heights = &setup.document.editor().track_heights;
        assert_eq!(heights[&Chain::Master], MAX_TRACK_HEIGHT);
        let selected = Args::new().with("height", 70.0);
        assert_eq!(
            setup.run_with(&SET_TRACK_HEIGHT, selected.clone()),
            Err(CommandError::invalid("no track is selected"))
        );
        setup.document.editor_mut().selected_track = Some(track);
        setup.run_with(&SET_TRACK_HEIGHT, selected).unwrap();
        assert_eq!(setup.document.editor().track_heights[&track], 70.0);
        setup.run(&TOGGLE_FOLLOW);
        assert!(!setup.document.editor().follows(Panel::Arrangement));
        setup.run(&TOGGLE_MIXER);
        setup.run(&TOGGLE_DEVICES);
        setup.run(&TOGGLE_BROWSER);
        setup.run(&TOGGLE_PIANO_ROLL);
        setup.run(&TOGGLE_SAMPLE_EDITOR);
        setup.run(&SWITCH_DEVICES_PIANO_ROLL);
        setup.run(&TOGGLE_CLIP_EDITOR);
        // The keys for the clip editor show the one that fits the clip
        // selected last.
        setup.document.editor_mut().clip_editor = panel::SAMPLE_EDITOR;
        setup.run(&SWITCH_DEVICES_PIANO_ROLL);
        setup.run(&TOGGLE_CLIP_EDITOR);
        setup.run(&MOVE_TO_NEW_WINDOW);
        setup.run(&MOVE_TO_MAIN_WINDOW);
        assert_eq!(
            setup.requests,
            [
                Request::TogglePanel("mixer"),
                Request::TogglePanel("devices"),
                Request::TogglePanel("browser"),
                Request::TogglePanel("piano_roll"),
                Request::TogglePanel("sample_editor"),
                Request::SwitchDevicesClipEditor("piano_roll"),
                Request::TogglePanel("piano_roll"),
                Request::SwitchDevicesClipEditor("sample_editor"),
                Request::TogglePanel("sample_editor"),
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

    /// Bass and Keys, with clips from beat 0 to 32 and 16 to 64.
    fn two_tracks() -> (Setup, TrackId, TrackId) {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Midi);
        let keys = project.add_track("Keys", TrackKind::Midi);
        for (track, start, end) in [(bass, 0, 32), (keys, 16, 64)] {
            let range = BeatRange::new(Beats::new(start), Beats::new(end));
            let clip = project.new_clip("C", range, ClipContent::Midi(MidiClip::default()));
            project.track_mut(track).unwrap().clips.push(clip);
        }
        (Setup::new(project), bass, keys)
    }

    fn history(setup: &Setup) -> usize {
        setup.document.editor().navigation.arrangement.len()
    }

    #[test]
    fn w_and_h_pressed_again_change_nothing() {
        let (mut setup, ..) = two_tracks();
        setup.run(&FIT_SONG_WIDTH);
        let fitted = setup.document.editor().viewport;
        let visible = fitted.visible();
        assert!(visible.start == Beats::ZERO && Beats::new(64) <= visible.end);
        assert_eq!(history(&setup), 1);
        setup.run(&FIT_SONG_WIDTH);
        assert_eq!(setup.document.editor().viewport, fitted);
        assert_eq!(history(&setup), 1);
        setup.run(&FIT_TRACK_HEIGHTS);
        let heights = setup.document.editor().track_heights.clone();
        assert_eq!(history(&setup), 2);
        setup.run(&FIT_TRACK_HEIGHTS);
        assert_eq!(setup.document.editor().track_heights, heights);
        assert_eq!(history(&setup), 2);
        // Each undoes nothing of the other's, however they're mixed.
        setup.run(&FIT_SONG_WIDTH);
        setup.run(&FIT_TRACK_HEIGHTS);
        let editor = setup.document.editor();
        assert_eq!((editor.viewport, &editor.track_heights), (fitted, &heights));
        // Two tracks share the 480 points the tracks have.
        assert!(
            heights.values().all(|&h| (h - 240.0).abs() < 0.5),
            "{heights:?}"
        );
        // Nothing of it changes the project.
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn w_after_zooming_fits_the_song_again() {
        let (mut setup, ..) = two_tracks();
        setup.run(&FIT_SONG_WIDTH);
        let fitted = setup.document.editor().viewport;
        setup.run(&ZOOM_IN);
        setup.run(&ZOOM_IN);
        assert_ne!(setup.document.editor().viewport, fitted);
        setup.run(&FIT_SONG_WIDTH);
        assert_eq!(setup.document.editor().viewport, fitted);
    }

    #[test]
    fn x_goes_back_and_shift_x_forward() {
        let (mut setup, ..) = two_tracks();
        let before = setup.document.editor().viewport;
        assert!(!setup.applies(&ZOOM_BACK) && !setup.applies(&ZOOM_FORWARD));
        setup.run(&FIT_SONG_WIDTH);
        let fitted = setup.document.editor().viewport;
        setup.run(&ZOOM_BACK);
        assert_eq!(setup.document.editor().viewport, before);
        assert!(setup.applies(&ZOOM_FORWARD));
        setup.run(&ZOOM_FORWARD);
        assert_eq!(setup.document.editor().viewport, fitted);
        // A new step after going back leaves nothing to go forward to.
        setup.run(&ZOOM_BACK);
        setup.run(&ZOOM_IN);
        assert!(!setup.applies(&ZOOM_FORWARD));
        setup.run(&ZOOM_BACK);
        assert_eq!(setup.document.editor().viewport, before);
    }

    #[test]
    fn a_wheel_gesture_is_one_step_back() {
        let (mut setup, ..) = two_tracks();
        let before = setup.document.editor().viewport;
        let to = |start: i64, history: &str| {
            Args::new()
                .with("start", Beats::new(start))
                .with("zoom", before.zoom)
                .with("top", 0.0)
                .with("width", before.width)
                .with("history", history)
        };
        for start in [4, 8, 12] {
            setup.run_with(&SET_VIEWPORT, to(start, "gesture")).unwrap();
        }
        setup.run_with(&SET_VIEWPORT, to(12, "step")).unwrap();
        assert_eq!(history(&setup), 1);
        setup.run(&ZOOM_BACK);
        assert_eq!(setup.document.editor().viewport.start, before.start);
        // What the user didn't do isn't a step.
        setup.run_with(&SET_VIEWPORT, to(20, "none")).unwrap();
        assert_eq!(history(&setup), 0);
    }

    #[test]
    fn zooming_while_playing_keeps_the_playhead_still_and_holds_follow() {
        let (mut setup, ..) = two_tracks();
        setup.transport.playing = true;
        setup.transport.playhead = Beats::new(10);
        let x = setup.document.editor().viewport.x_of(Beats::new(10));
        setup.run(&ZOOM_IN);
        let editor = setup.document.editor();
        assert!((editor.viewport.x_of(Beats::new(10)) - x).abs() < 0.01);
        assert!(editor.navigation.arrangement.suspended);
        // Follow finding the playhead in sight follows again.
        let viewport = editor.viewport;
        let args = Args::new()
            .with("start", viewport.start)
            .with("zoom", viewport.zoom)
            .with("top", 0.0)
            .with("width", viewport.width)
            .with("history", "follow");
        setup.run_with(&SET_VIEWPORT, args).unwrap();
        assert!(!setup.document.editor().navigation.arrangement.suspended);
        // And so does playing from stopped.
        setup.run(&ZOOM_OUT);
        assert!(setup.document.editor().navigation.arrangement.suspended);
        setup.transport.playing = false;
        setup.run(&crate::transport::PLAY);
        assert!(!setup.document.editor().navigation.arrangement.suspended);
    }

    #[test]
    fn tracks_step_through_the_sizes_and_too_many_say_so() {
        let (mut setup, bass, keys) = two_tracks();
        setup.select_tracks(&[bass]);
        let height = |setup: &Setup, track| setup.document.editor().height_of(Chain::Track(track));
        setup.run(&TRACKS_TALLER);
        assert_eq!(height(&setup, bass), 120.0);
        setup.run(&TRACKS_TALLER);
        setup.run(&TRACKS_TALLER);
        assert_eq!(height(&setup, bass), MAX_TRACK_HEIGHT);
        for _ in 0..5 {
            setup.run(&TRACKS_SHORTER);
        }
        assert_eq!(height(&setup, bass), MIN_TRACK_HEIGHT);
        assert_eq!(height(&setup, keys), 60.0);
        setup
            .run_with(&RESET_TRACK_HEIGHT, Args::new().with("track", bass))
            .unwrap();
        assert_eq!(height(&setup, bass), 60.0);

        let mut project = Project::new();
        for n in 0..30 {
            project.add_track(format!("T{n}"), TrackKind::Audio);
        }
        let mut setup = Setup::new(project);
        setup.run(&FIT_TRACK_HEIGHTS);
        assert_eq!(
            setup.requests,
            [Request::Notice(
                "20 of 30 tracks are in sight at the smallest height. Scroll for the rest.".into()
            )]
        );
    }

    #[test]
    fn presets_size_the_selected_tracks_or_every_track() {
        let (mut setup, bass, keys) = two_tracks();
        let height = |setup: &Setup, track| setup.document.editor().height_of(Chain::Track(track));
        // With none selected, every track.
        setup.run(&SMALL_TRACKS);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (24.0, 24.0));
        setup.select_tracks(&[keys]);
        setup.run(&LARGE_TRACKS);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (24.0, 120.0));
        // Alt in a menu gives every track the size.
        let all = Args::new().with("all", true);
        setup.run_with(&HUGE_TRACKS, all).unwrap();
        assert_eq!((height(&setup, bass), height(&setup, keys)), (200.0, 200.0));
        // The sizes are the behaviour's.
        setup.document.editor_mut().behaviour.medium_tracks = 80;
        let tracks = Args::new().with("tracks", vec![bass]);
        setup.run_with(&MEDIUM_TRACKS, tracks).unwrap();
        assert_eq!((height(&setup, bass), height(&setup, keys)), (80.0, 200.0));
        // Each is a step back.
        setup.run(&ZOOM_BACK);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (200.0, 200.0));
        setup.run(&ZOOM_BACK);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (24.0, 120.0));
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn fit_selected_gives_the_selected_tracks_the_room() {
        let mut project = Project::new();
        let tracks: Vec<TrackId> = (0..30)
            .map(|n| project.add_track(format!("T{n}"), TrackKind::Audio))
            .collect();
        let mut setup = Setup::new(project);
        let height = |setup: &Setup, track| setup.document.editor().height_of(Chain::Track(track));
        assert!(!setup.applies(&FIT_SELECTED_TRACKS));
        // The other 28 take 672 points at the smallest, more than the
        // 480 there are: the two are the smallest too, and the first of
        // them comes to the top.
        setup.select_tracks(&[tracks[27], tracks[29]]);
        // Selecting them scrolled down to them.
        let top = setup.document.editor().viewport.top;
        setup.run(&FIT_SELECTED_TRACKS);
        assert_eq!(height(&setup, tracks[27]), MIN_TRACK_HEIGHT);
        assert_eq!(
            setup.document.editor().viewport.top,
            27.0 * MIN_TRACK_HEIGHT
        );
        assert_eq!(
            setup.requests,
            [Request::Notice(
                "The selected tracks don't fit at the smallest height. Scroll for the rest.".into()
            )]
        );
        setup.run(&ZOOM_BACK);
        assert_eq!(height(&setup, tracks[27]), 60.0);
        assert_eq!(setup.document.editor().viewport.top, top);

        // With four tracks, the other two take 48 points and the selected
        // ones share the rest.
        let mut project = Project::new();
        let tracks: Vec<TrackId> = (0..4)
            .map(|n| project.add_track(format!("T{n}"), TrackKind::Audio))
            .collect();
        let mut setup = Setup::new(project);
        let args = Args::new().with("tracks", vec![tracks[1], tracks[2]]);
        setup.run_with(&FIT_SELECTED_TRACKS, args).unwrap();
        assert_eq!(height(&setup, tracks[1]), (480.0 - 48.0) / 2.0);
        assert_eq!(height(&setup, tracks[3]), MIN_TRACK_HEIGHT);
        assert!(setup.requests.is_empty());
    }

    #[test]
    fn auto_zoom_makes_the_selected_track_large_and_saves_none_of_it() {
        let (mut setup, bass, keys) = two_tracks();
        let height = |setup: &Setup, track| setup.document.editor().height_of(Chain::Track(track));
        let show = |setup: &mut Setup, track| {
            setup.document.editor_mut().selected_track = Some(Chain::Track(track));
            // Any command lets Auto Zoom see the track selected.
            setup.run(&SCROLL_TO_PLAYHEAD);
        };
        setup.run(&TOGGLE_AUTO_ZOOM);
        assert!(setup.document.editor().behaviour.auto_zoom_tracks);
        assert_eq!(setup.requests, [Request::SaveBehaviour]);
        show(&mut setup, bass);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (120.0, 60.0));
        assert!(setup.document.editor().track_heights.is_empty());
        // Sizing the zoomed track by hand keeps the size given.
        let args = Args::new().with("track", bass).with("height", 50.0);
        setup.run_with(&SET_TRACK_HEIGHT, args).unwrap();
        assert_eq!(height(&setup, bass), 50.0);
        show(&mut setup, keys);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (50.0, 120.0));
        // Selected again, it's zoomed again.
        show(&mut setup, bass);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (120.0, 60.0));
        // Off, the heights are the user's.
        setup.run(&TOGGLE_AUTO_ZOOM);
        assert_eq!((height(&setup, bass), height(&setup, keys)), (50.0, 60.0));
        let heights = &setup.document.editor().track_heights;
        assert_eq!(heights.len(), 1);
    }

    #[test]
    fn each_panel_has_its_own_follow_and_they_share_the_mode() {
        let (mut setup, clip) = with_notes();
        let args = Args::new()
            .with("notes", vec![musicable_model::clip::NoteId(1)])
            .with("clip", clip);
        setup.run_with(&crate::notes::SELECT, args).unwrap();
        let focus = |panel: &str| Args::new().with("panel", panel);
        setup.run_with(&SET_FOCUS, focus("piano_roll")).unwrap();
        setup.run(&TOGGLE_FOLLOW);
        let editor = setup.document.editor();
        assert!(!editor.follows(Panel::PianoRoll));
        assert!(editor.follows(Panel::Arrangement) && editor.follows(Panel::SampleEditor));
        setup.run(&SWITCH_FOLLOW_MODE);
        let behaviour = setup.document.editor().behaviour;
        assert_eq!(behaviour.follow_mode, FollowMode::Continuous);
        assert_eq!(setup.requests, [Request::SaveBehaviour]);
        // Scroll to Playhead puts it at Continuous Follow's place, a third
        // of the way in, and follows again.
        setup.run_with(&SET_FOCUS, focus("arrangement")).unwrap();
        setup.transport.playing = true;
        setup.transport.playhead = Beats::new(100);
        setup.document.editor_mut().navigation.arrangement.suspended = true;
        setup.run(&SCROLL_TO_PLAYHEAD);
        let editor = setup.document.editor();
        let viewport = editor.viewport;
        let x = viewport.x_of(Beats::new(100));
        assert!((x - viewport.width / 3.0).abs() < 0.5, "{x}");
        assert!(!editor.navigation.arrangement.suspended);
        // A drag let go of in Continuous mode leaves Follow waiting.
        let args = Args::new()
            .with("start", viewport.start)
            .with("zoom", viewport.zoom)
            .with("top", 0.0)
            .with("width", viewport.width)
            .with("history", "suspend");
        setup.run_with(&SET_VIEWPORT, args).unwrap();
        let editor = setup.document.editor();
        assert!(editor.navigation.arrangement.suspended);
        assert_eq!(editor.navigation.arrangement.len(), 1);
    }
}

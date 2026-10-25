//! The arrangement in a window, its gestures carried out on a document as
//! the app carries them out.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::accesskit::Toggled;
use egui::{DroppedFile, Event, Modifiers, PointerButton, Pos2, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::{AudioFile, Selection};
use musicable_commands::{CommandError, Context, Document, Keymap, Named, Registry, Request};
use musicable_model::automation::Lane;
use musicable_model::clip::{AudioClip, ClipContent, ClipId, MidiClip, Offset, Timing, Warping};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::Warp;
use musicable_model::{LocatorId, Project, TakeId, TrackId, TrackKind};

use super::*;
use crate::shell::{Audio, KeymapStatus, Playback};
use crate::testing::Player;
use crate::{Levels, Plugins, Waveforms};

struct State {
    arrangement: Arrangement,
    document: Document,
    commands: Registry,
    keymap: Keymap,
    player: Player,
    levels: Levels,
    waveforms: Waveforms,
    theme: Theme,
    mouse: MouseMap,
    parts: Parts,
    /// What the arrangement asked for, and what the commands asked of the
    /// app.
    actions: Vec<Action>,
    requests: Vec<Request>,
    errors: Vec<CommandError>,
}

impl State {
    fn project(&self) -> &Project {
        self.document.project()
    }

    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// Carries out `action` as the app does.
    fn carry_out(&mut self, action: &Action) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.player,
            requests: &mut self.requests,
            library: &mut musicable_commands::NoLibrary,
        };
        let ran = match action {
            Action::Run(id) => self.commands.run_with(id, &Args::new(), &mut ctx),
            Action::RunWith(id, args) => self.commands.run_with(id, args, &mut ctx),
            Action::Mixer(change) => {
                musicable_commands::mixer::change(&mut self.document, change.clone());
                Ok(musicable_commands::Value::None)
            }
            _ => Ok(musicable_commands::Value::None),
        };
        if let Err(error) = ran {
            self.errors.push(error);
        }
    }
}

/// The ids of the test project's tracks and clips.
struct Ids {
    drums: TrackId,
    keys: TrackId,
    bass: TrackId,
    kick: ClipId,
    chords: ClipId,
    line: ClipId,
    verse: LocatorId,
}

/// Drums, an audio track with Kick over the first two bars; Keys, a MIDI
/// track with Chords over the next two; Bass, an audio track with Line over
/// the two after; and a reverb return. The locator Verse is at bar 5, and
/// Outro is a clip far out of sight.
fn project() -> (Project, Ids) {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let bass = project.add_track("Bass", TrackKind::Audio);
    project.add_track("Reverb", TrackKind::Return);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let audio = |file: &str| ClipContent::Audio(AudioClip::new(file));
    let kick = project.new_clip("Kick", beats(0, 8), audio("kick.wav"));
    let outro = project.new_clip("Outro", beats(1000, 1008), audio("kick.wav"));
    let mut notes = MidiClip::default();
    notes.add_note(Beats::ZERO, Beats::new(2), 60, 0.8);
    notes.add_note(Beats::new(2), Beats::new(2), 67, 0.8);
    let chords = project.new_clip("Chords", beats(8, 16), ClipContent::Midi(notes));
    let line = project.new_clip("Line", beats(16, 24), audio("bass.wav"));
    let ids = (kick.id, chords.id, line.id);
    project
        .track_mut(drums)
        .unwrap()
        .clips
        .extend([kick, outro]);
    project.track_mut(keys).unwrap().clips.push(chords);
    project.track_mut(bass).unwrap().clips.push(line);
    let verse = project.new_locator("Verse", Beats::new(16));
    let verse_id = verse.id;
    project.locators.push(verse);
    let ids = Ids {
        drums,
        keys,
        bass,
        kick: ids.0,
        chords: ids.1,
        line: ids.2,
        verse: verse_id,
    };
    (project, ids)
}

fn harness(project: Project) -> Harness<'static, State> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let mut document = Document::new(project);
    // A minute of each file, as if the app had read them.
    for file in ["kick.wav", "bass.wav"] {
        let info = AudioFile {
            sample_rate: 48_000,
            frames: 48_000 * 60,
            channels: 2,
            tempo: None,
        };
        document.editor_mut().files.insert(file.to_owned(), info);
    }
    let state = State {
        arrangement: Arrangement::default(),
        document,
        commands,
        keymap: Keymap::default_keymap(),
        player: Player::default(),
        levels: Levels::default(),
        waveforms: Waveforms::default(),
        theme: Theme::default(),
        mouse: MouseMap::live(),
        parts: Parts::default(),
        actions: Vec::new(),
        requests: Vec::new(),
        errors: Vec::new(),
    };
    // Frames as quick as a screen's, so two clicks make a double click, and
    // enough of them for egui to smooth out a turn of the wheel, which takes
    // a few.
    let mut harness = Harness::builder()
        .with_size([1000.0, 420.0])
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40)
        .build_ui_state(
            |ui, state: &mut State| {
                let mut actions = Vec::new();
                {
                    let view = View {
                        project: state.document.project(),
                        editor: state.document.editor(),
                        waveforms: &state.waveforms,
                        name: "Untitled",
                        modified: false,
                        commands: &state.commands,
                        keymap: &state.keymap,
                        default_keymap: &state.keymap,
                        history: state.document.steps(),
                        playback: Playback {
                            playing: state.player.playing,
                            record: state.player.recording,
                            playhead: state.player.playhead,
                            seconds: 0.0,
                        },
                        recordings: &[],
                        levels: &state.levels,
                        inputs: &[],
                        midi_inputs: &[],
                        audio: Audio {
                            summary: "",
                            running: true,
                            load: None,
                            dropouts: 0,
                            details: "",
                        },
                        keymap_file: KeymapStatus::default(),
                        messages: &[],
                        agent: None,
                        analysing: None,
                        rendering: None,
                        plugins: Plugins::default(),
                        native: &[],
                        library: crate::Library::default(),
                        workspaces: crate::Workspaces::default(),
                        monitors: &[],
                    };
                    let (theme, mouse, parts) = (&state.theme, &state.mouse, &state.parts);
                    state
                        .arrangement
                        .show(ui, &view, theme, mouse, parts, &mut actions);
                }
                state.parts.carry_out();
                for action in &actions {
                    state.carry_out(action);
                }
                state.actions.extend(actions);
            },
            state,
        );
    harness.run();
    harness
}

/// Where the timeline shows `at`.
fn x_of(harness: &Harness<'_, State>, at: f64) -> f32 {
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().editor().viewport;
    lanes.left() + viewport.x_of(Beats::from_f64(at))
}

/// Somewhere in `name`'s lane, at `at`.
fn in_lane(harness: &Harness<'_, State>, name: &str, at: f64) -> Pos2 {
    let row = harness.get_by_label(&format!("Track {name}")).rect();
    pos2(x_of(harness, at), row.center().y)
}

/// A point in the clip's title bar, `along` points from its start.
fn title(harness: &Harness<'_, State>, clip: &str, along: f32) -> Pos2 {
    let rect = harness.get_by_label(&format!("Clip {clip}")).rect();
    pos2(rect.left() + along, rect.top() + 4.0)
}

/// A point in the clip's body, `along` points from its start.
fn body(harness: &Harness<'_, State>, clip: &str, along: f32) -> Pos2 {
    let rect = harness.get_by_label(&format!("Clip {clip}")).rect();
    pos2(rect.left() + along, rect.bottom() - 6.0)
}

/// A point in the ruler's strip, at `at`.
fn strip(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    pos2(
        x_of(harness, at),
        ruler.top() + 0.75 * Theme::default().ruler_strip,
    )
}

/// A point in the ruler under its strip, at `at`.
fn ruler(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    pos2(x_of(harness, at), ruler.bottom() - 6.0)
}

fn press(harness: &mut Harness<'_, State>, at: Pos2, pressed: bool, mods: Modifiers) {
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: mods,
    });
    harness.run();
}

/// Clicks at `at` with `mods` held.
fn click(harness: &mut Harness<'_, State>, at: Pos2, mods: Modifiers) {
    harness.event(Event::ModifiersChanged(mods));
    harness.hover_at(at);
    harness.run();
    press(harness, at, true, mods);
    press(harness, at, false, mods);
    harness.event(Event::ModifiersChanged(Modifiers::NONE));
    harness.run();
}

/// Drags from `from` to `to` with `mods` held, in a few moves.
fn drag(harness: &mut Harness<'_, State>, from: Pos2, to: Pos2, mods: Modifiers) {
    harness.event(Event::ModifiersChanged(mods));
    harness.hover_at(from);
    harness.run();
    press(harness, from, true, mods);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    press(harness, to, false, mods);
    harness.event(Event::ModifiersChanged(Modifiers::NONE));
    harness.run();
}

/// The distance `beats` beats take on the timeline.
fn width(harness: &Harness<'_, State>, beats: f64) -> f32 {
    (beats * harness.state().editor().viewport.zoom) as f32
}

fn clip(harness: &Harness<'_, State>, id: ClipId) -> (TrackId, musicable_model::clip::Clip) {
    let (track, clip) = harness.state().project().find_clip(id).unwrap();
    (track.id, clip.clone())
}

/// The shapes the last frame painted, those in groups too.
fn painted(harness: &Harness<'_, State>) -> Vec<Shape> {
    fn add(shape: &Shape, shapes: &mut Vec<Shape>) {
        match shape {
            Shape::Vec(group) => group.iter().for_each(|shape| add(shape, shapes)),
            shape => shapes.push(shape.clone()),
        }
    }
    let mut shapes = Vec::new();
    for clipped in &harness.output().shapes {
        add(&clipped.shape, &mut shapes);
    }
    shapes
}

/// The rectangles the last frame filled with `colour` inside `area`.
fn filled(harness: &Harness<'_, State>, colour: Color32, area: Rect) -> Vec<Rect> {
    (painted(harness).into_iter())
        .filter_map(|shape| match shape {
            Shape::Rect(rect) if rect.fill == colour && area.contains_rect(rect.rect) => {
                Some(rect.rect)
            }
            _ => None,
        })
        .collect()
}

/// Whether the last frame drew a mesh in `colour` inside `area`, as
/// notes and waveforms are drawn.
fn meshed(harness: &Harness<'_, State>, colour: Color32, area: Rect) -> bool {
    painted(harness).iter().any(|shape| match shape {
        Shape::Mesh(mesh) => {
            (mesh.vertices.iter()).any(|v| v.color == colour && area.contains(v.pos))
        }
        _ => false,
    })
}

/// The row of the track or take with the label `label`, across the lanes.
fn across(harness: &Harness<'_, State>, label: &str) -> Rect {
    let lanes = harness.get_by_label("Arrangement").rect();
    let row = harness.get_by_label(label).rect();
    Rect::from_x_y_ranges(lanes.x_range(), row.y_range())
}

fn selected(harness: &Harness<'_, State>) -> Vec<ClipId> {
    harness
        .state()
        .editor()
        .selection
        .clips
        .iter()
        .copied()
        .collect()
}

#[test]
fn clips_tracks_and_locators_are_named() {
    let (project, _) = project();
    let harness = harness(project);
    for name in [
        "Clip Kick",
        "Clip Chords",
        "Clip Line",
        "Track Drums",
        "Track Keys",
        "Track Bass",
        "Track Reverb",
        "Locator Verse",
        "Mute track Drums",
        "Solo track Reverb",
        "Arm track Keys",
    ] {
        harness.get_by_label(name);
    }
    // Returns have no arm, and clips out of sight aren't there.
    assert!(harness.query_by_label("Arm track Reverb").is_none());
    assert!(harness.query_by_label("Clip Outro").is_none());
    // The timeline knows how wide it is, for zooming to fit.
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().editor().viewport;
    assert_eq!(viewport.width, lanes.width());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn clicks_select_clips_and_place_the_start_marker() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = title(&harness, "Kick", 30.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(selected(&harness), [ids.kick]);
    let at = title(&harness, "Line", 30.0);
    click(&mut harness, at, Modifiers::COMMAND);
    assert_eq!(selected(&harness), [ids.kick, ids.line]);
    // Clicking a clip's body selects it, and marks where it was clicked,
    // on the grid: at this zoom, every two beats.
    let at = pos2(x_of(&harness, 10.7), body(&harness, "Chords", 0.0).y);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(selected(&harness), [ids.chords]);
    assert_eq!(harness.state().editor().start_marker, Beats::new(10));
    // An empty lane clears the selection and selects the track.
    let at = in_lane(&harness, "Bass", 30.2);
    click(&mut harness, at, Modifiers::NONE);
    let editor = harness.state().editor();
    assert!(editor.selection.clips.is_empty());
    assert_eq!(editor.selected_track, Some(Chain::Track(ids.bass)));
    assert_eq!(editor.start_marker, Beats::new(30));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_title_moves_clips_on_the_grid_and_to_other_tracks() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Kick", 30.0);
    let to = from + vec2(width(&harness, 2.3), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let (track, kick) = clip(&harness, ids.kick);
    assert_eq!((track, kick.start), (ids.drums, Beats::new(2)));
    assert_eq!(selected(&harness), [ids.kick]);
    assert_eq!(harness.state().document.undo_name(), Some("Move Clips"));
    // Keys is a MIDI track, so Kick can't go there, but Bass is two down.
    let from = title(&harness, "Kick", 30.0);
    let to = in_lane(&harness, "Keys", 0.0);
    drag(&mut harness, from, pos2(from.x, to.y), Modifiers::NONE);
    assert_eq!(clip(&harness, ids.kick).0, ids.drums);
    let to = in_lane(&harness, "Bass", 0.0);
    drag(&mut harness, from, pos2(from.x, to.y), Modifiers::NONE);
    let (track, kick) = clip(&harness, ids.kick);
    assert_eq!((track, kick.start), (ids.bass, Beats::new(2)));
    // A drag is one step to undo.
    assert!(harness.state_mut().document.undo());
    assert_eq!(clip(&harness, ids.kick).0, ids.drums);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn ctrl_drag_copies_and_alt_drag_moves_off_the_grid() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Line", 30.0);
    let to = from + vec2(width(&harness, 8.0), 0.0);
    drag(&mut harness, from, to, Modifiers::COMMAND);
    let starts: Vec<_> = harness.state().project().tracks[2]
        .clips
        .iter()
        .map(|clip| clip.start)
        .collect();
    assert_eq!(starts, [Beats::new(16), Beats::new(24)]);
    // The copy is selected.
    let copy = harness.state().project().tracks[2].clips[1].id;
    assert_eq!(selected(&harness), [copy]);
    assert_eq!(harness.state().document.undo_name(), Some("Copy Clips"));

    let from = title(&harness, "Kick", 30.0);
    let to = from + vec2(width(&harness, 1.25), 0.0);
    drag(&mut harness, from, to, Modifiers::ALT);
    let start = clip(&harness, ids.kick).1.start.to_f64();
    assert!((start - 1.25).abs() < 0.01, "{start}");
}

#[test]
fn edges_trim_and_handles_fade() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let rect = harness.get_by_label("Clip Kick").rect();
    let from = pos2(rect.right() - 2.0, rect.center().y);
    let to = from - vec2(width(&harness, 2.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let kick = clip(&harness, ids.kick).1;
    assert_eq!((kick.start, kick.end()), (Beats::ZERO, Beats::new(6)));
    assert_eq!(harness.state().document.undo_name(), Some("Trim Clip"));
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = from + vec2(width(&harness, 2.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let kick = clip(&harness, ids.kick).1;
    assert_eq!((kick.start, kick.end()), (Beats::new(2), Beats::new(6)));
    let ClipContent::Audio(audio) = &kick.content else {
        panic!("Kick is audio");
    };
    // Two beats at 120 BPM is a second of the file.
    assert_eq!(audio.offset, Offset::Frames(48_000));

    // The fade-in's handle is at the top of the body, clear of the edge.
    let rect = harness.get_by_label("Clip Kick").rect();
    let theme = Theme::default();
    let handle = pos2(
        rect.left() + theme.edge_grab + theme.fade_handle / 2.0,
        rect.top() + theme.clip_title + theme.fade_handle / 2.0,
    );
    let to = handle + vec2(width(&harness, 1.0), 0.0);
    drag(&mut harness, handle, to, Modifiers::NONE);
    let ClipContent::Audio(audio) = &clip(&harness, ids.kick).1.content else {
        panic!("Kick is audio");
    };
    let fade = audio.fade_in.to_f64();
    assert!((fade - 1.0).abs() < 0.01, "{fade}");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn shift_dragging_a_warped_clip_s_edge_stretches_it() {
    let (mut project, ids) = project();
    // Kick and Line warped at 120 BPM, as the song is.
    for track in [ids.drums, ids.bass] {
        let clip = &mut project.track_mut(track).unwrap().clips[0];
        let ClipContent::Audio(audio) = &mut clip.content else {
            panic!("an audio clip");
        };
        let warping = Warping {
            offset: Offset::Beats(Beats::ZERO),
            warp: Some(Warp::new(0, Beats::ZERO, 120.0)),
        };
        assert!(audio.set_warping(warping));
    }
    let mut harness = harness(project);
    let tempo = |harness: &Harness<'_, State>, id: ClipId| {
        let ClipContent::Audio(audio) = clip(harness, id).1.content else {
            panic!("an audio clip");
        };
        audio.warp.unwrap().clip_tempo(48_000)
    };
    // Its end four beats later: the same audio over twelve beats.
    let rect = harness.get_by_label("Clip Kick").rect();
    let from = pos2(rect.right() - 2.0, rect.center().y);
    let to = pos2(x_of(&harness, 12.0) - 2.0, from.y);
    drag(&mut harness, from, to, Modifiers::SHIFT);
    let kick = clip(&harness, ids.kick).1;
    assert_eq!(kick.range(), BeatRange::new(Beats::ZERO, Beats::new(12)));
    assert!((tempo(&harness, ids.kick) - 180.0).abs() < 1e-6);
    assert_eq!(harness.state().document.undo_name(), Some("Set Clip Tempo"));
    // From its start, its end stays.
    let rect = harness.get_by_label("Clip Line").rect();
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = pos2(x_of(&harness, 14.0) + 2.0, from.y);
    drag(&mut harness, from, to, Modifiers::SHIFT);
    let line = clip(&harness, ids.line).1;
    assert_eq!(line.range(), BeatRange::new(Beats::new(14), Beats::new(24)));
    assert!((tempo(&harness, ids.line) - 150.0).abs() < 1e-6);
    // A clip that isn't warped trims.
    let rect = harness.get_by_label("Clip Chords").rect();
    let from = pos2(rect.right() - 2.0, rect.center().y);
    let to = from - vec2(width(&harness, 2.0), 0.0);
    drag(&mut harness, from, to, Modifiers::SHIFT);
    let chords = clip(&harness, ids.chords).1;
    assert_eq!(
        chords.range(),
        BeatRange::new(Beats::new(8), Beats::new(14))
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn a_trims_ghost_stops_where_the_audio_does() {
    let (mut project, ids) = project();
    // Line plays bass.wav from a second in, from beat 18.
    let timing = Timing {
        start: Beats::new(18),
        length: Beats::new(6),
        offset: Offset::Frames(48_000),
    };
    assert!(project.track_mut(ids.bass).unwrap().clips[0].set_timing(timing));
    let mut harness = harness(project);
    // Its start dragged four beats earlier, two before the file starts.
    let rect = harness.get_by_label("Clip Line").rect();
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = from - vec2(width(&harness, 4.0), 0.0);
    harness.hover_at(from);
    harness.run();
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    let ghosts = filled(
        &harness,
        Theme::default().ghost,
        across(&harness, "Track Bass"),
    );
    let [ghost] = ghosts[..] else {
        panic!("one ghost, not {ghosts:?}");
    };
    let starts = x_of(&harness, 16.0);
    assert!(
        (ghost.left() - starts).abs() < 0.5,
        "{} {starts}",
        ghost.left()
    );
    // And that's where the trim takes it.
    press(&mut harness, to, false, Modifiers::NONE);
    let line = clip(&harness, ids.line).1;
    assert_eq!(line.range(), BeatRange::new(Beats::new(16), Beats::new(24)));
    assert_eq!(line.content.offset(), Offset::Frames(0));
}

#[test]
fn dragging_over_lanes_selects_time_on_the_tracks_crossed() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = in_lane(&harness, "Drums", 26.3);
    let to = in_lane(&harness, "Keys", 34.4);
    drag(&mut harness, from, to, Modifiers::NONE);
    let editor = harness.state().editor();
    let time = editor.selection.time.as_ref().expect("a time selection");
    assert_eq!(time.range, BeatRange::new(Beats::new(26), Beats::new(34)));
    assert_eq!(
        time.tracks.iter().copied().collect::<Vec<_>>(),
        [ids.drums, ids.keys]
    );
    // Playing starts from it.
    assert_eq!(editor.start_marker, Beats::new(26));
}

#[test]
fn the_ruler_locates_and_its_strip_draws_moves_and_toggles_the_loop() {
    let (project, _) = project();
    let mut harness = harness(project);
    let at = ruler(&harness, 12.3);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().player.playhead, Beats::new(12));
    assert_eq!(harness.state().editor().start_marker, Beats::new(12));

    let (from, to) = (strip(&harness, 20.0), strip(&harness, 28.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let project = harness.state().project();
    assert!(project.looping);
    let region = BeatRange::new(Beats::new(20), Beats::new(28));
    assert_eq!(project.loop_region, region);

    let at = strip(&harness, 24.0);
    click(&mut harness, at, Modifiers::NONE);
    assert!(!harness.state().project().looping);
    // Its end moves on its own.
    let (from, to) = (strip(&harness, 28.0), strip(&harness, 32.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let region = BeatRange::new(Beats::new(20), Beats::new(32));
    assert_eq!(harness.state().project().loop_region, region);
    // And all of it, from the middle.
    let (from, to) = (strip(&harness, 24.0), strip(&harness, 28.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let region = BeatRange::new(Beats::new(24), Beats::new(36));
    assert_eq!(harness.state().project().loop_region, region);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn locators_move_and_a_double_click_renames_them() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = harness.get_by_label("Locator Verse").rect();
    let from = pos2(at.center().x, at.top() + 2.0);
    let to = from + vec2(width(&harness, 4.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let verse = harness.state().project().locator(ids.verse).unwrap();
    assert_eq!(verse.at, Beats::new(20));
    let at = harness.get_by_label("Locator Verse").rect();
    let at = pos2(at.center().x, at.top() + 2.0);
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
    let rename = Request::Rename(Named::Locator(ids.verse));
    assert_eq!(harness.state().requests, [rename]);
    // A click on a locator jumps there.
    assert_eq!(harness.state().player.playhead, Beats::new(20));
}

#[test]
fn a_double_click_opens_a_clip_in_its_editor() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let double_click = |harness: &mut Harness<'_, State>, at: Pos2| {
        harness.run_steps(30);
        harness.hover_at(at);
        harness.run();
        for pressed in [true, false, true, false] {
            harness.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        harness.run();
    };
    // An audio clip opens in the Sample Editor.
    let at = body(&harness, "Kick", 30.0);
    double_click(&mut harness, at);
    let sample_editor = Request::ShowPanel(crate::SAMPLE_EDITOR);
    assert_eq!(
        harness.state().requests,
        std::slice::from_ref(&sample_editor)
    );
    let editor = harness.state().editor();
    assert_eq!(editor.sample_editor.clip, Some(ids.kick));
    assert_eq!(editor.focus, musicable_commands::Focus::SAMPLE_EDITOR);
    // A MIDI clip in the piano roll.
    let at = body(&harness, "Chords", 30.0);
    double_click(&mut harness, at);
    assert_eq!(
        harness.state().requests,
        [sample_editor, Request::ShowPanel(crate::PIANO_ROLL)]
    );
    let editor = harness.state().editor();
    assert_eq!(editor.piano_roll.clip, Some(ids.chords));
    assert_eq!(editor.focus, musicable_commands::Focus::PIANO_ROLL);
}

#[test]
fn only_what_is_in_sight_is_laid_out() {
    // A hundred tracks of clips, as the arrangement has to stay smooth with.
    let mut project = Project::new();
    for t in 0..100 {
        let track = project.add_track(format!("T{t}"), TrackKind::Midi);
        for c in 0..20 {
            let range = BeatRange::new(Beats::new(8 * c), Beats::new(8 * c + 8));
            let content = ClipContent::Midi(MidiClip::default());
            let clip = project.new_clip(format!("C{t}.{c}"), range, content);
            project.track_mut(track).unwrap().clips.push(clip);
        }
    }
    let mut harness = harness(project);
    for name in ["Track T0", "Clip C0.0", "Clip C3.2"] {
        harness.get_by_label(name);
    }
    for name in ["Track T99", "Clip C99.0", "Clip C0.19"] {
        assert!(harness.query_by_label(name).is_none(), "{name}");
    }
    // Scrolled to the bottom, the last tracks are there instead.
    let at = in_lane(&harness, "T0", 4.0);
    harness.hover_at(at);
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -10_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    harness.get_by_label("Track T99");
    harness.get_by_label("Clip C99.0");
    assert!(harness.query_by_label("Track T0").is_none());
}

#[test]
fn the_wheel_scrolls_and_ctrl_wheel_zooms_around_the_pointer() {
    let (project, ids) = project();
    let mut harness = harness(project);
    for track in [ids.drums, ids.keys, ids.bass] {
        let heights = &mut harness.state_mut().document.editor_mut().track_heights;
        heights.insert(Chain::Track(track), 150.0);
    }
    harness.run();
    let at = in_lane(&harness, "Drums", 20.0);
    let wheel = |harness: &mut Harness<'_, State>, delta, mods| {
        harness.hover_at(at);
        harness.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: mods,
        });
        harness.run();
    };
    wheel(&mut harness, vec2(0.0, -50.0), Modifiers::NONE);
    assert_eq!(harness.state().editor().viewport.top, 50.0);
    let sideways = vec2(0.0, -width(&harness, 4.0));
    wheel(&mut harness, sideways, Modifiers::SHIFT);
    assert_eq!(harness.state().editor().viewport.start, Beats::new(4));
    let before = harness.state().editor().viewport;
    let lanes = harness.get_by_label("Arrangement").rect();
    let under = before.beats_at(at.x - lanes.left());
    wheel(&mut harness, vec2(0.0, 100.0), Modifiers::COMMAND);
    let viewport = harness.state().editor().viewport;
    assert!(viewport.zoom > before.zoom * 1.5, "{viewport:?}");
    // What was under the pointer stays there.
    let x = lanes.left() + viewport.x_of(under);
    assert!((x - at.x).abs() < 0.5, "{x} {}", at.x);
}

#[test]
fn header_buttons_mute_solo_and_arm() {
    let (project, ids) = project();
    let mut harness = harness(project);
    harness.get_by_label("Mute track Drums").click();
    harness.run();
    harness.get_by_label("Solo track Keys").click();
    harness.run();
    harness
        .get_by_label("Solo track Bass")
        .click_modifiers(Modifiers::COMMAND);
    harness.run();
    harness.get_by_label("Arm track Bass").click();
    harness.run();
    let project = harness.state().project();
    let track = |id| project.track(id).unwrap();
    assert!(track(ids.drums).muted);
    assert!(track(ids.keys).solo && track(ids.bass).solo);
    assert!(track(ids.bass).armed);
    let mute = harness.get_by_label("Mute track Drums");
    assert_eq!(mute.accesskit_node().toggled(), Some(Toggled::True));
}

#[test]
fn context_menus_offer_commands_for_what_was_clicked() {
    let (project, ids) = project();
    let mut harness = harness(project);
    harness.get_by_label("Clip Kick").click_secondary();
    harness.run();
    for item in [
        "Cut Ctrl+X",
        "Copy Ctrl+C",
        "Split Ctrl+E",
        "Quantize Ctrl+U",
        "Snap to Grid Ctrl+4",
    ] {
        harness.get_by_label(item);
    }
    harness.get_by_label("Delete Backspace, Delete").click();
    harness.run();
    assert!(harness.state().project().find_clip(ids.kick).is_none());

    // On a MIDI track's lane, a new clip where it was clicked.
    let at = in_lane(&harness, "Keys", 30.3);
    harness.event(Event::PointerMoved(at));
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    harness
        .get_by_label("Insert MIDI Clip Ctrl+Shift+M")
        .click();
    harness.run();
    let keys = &harness.state().project().tracks[1];
    let new = keys
        .clips
        .iter()
        .find(|clip| clip.id != ids.chords)
        .unwrap();
    assert_eq!(new.range(), BeatRange::new(Beats::new(30), Beats::new(34)));

    // A track's header has the track's commands.
    harness.get_by_label("Track Drums").click_secondary();
    harness.run();
    harness.get_by_label("Delete Tracks").click();
    harness.run();
    assert!(harness.state().project().track(ids.drums).is_none());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_tracks_edge_changes_its_height() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let header = harness.get_by_label("Track Drums").rect();
    let from = pos2(header.center().x, header.bottom() - 1.0);
    drag(&mut harness, from, from + vec2(0.0, 40.0), Modifiers::NONE);
    let height = harness.state().editor().track_heights[&Chain::Track(ids.drums)];
    assert_eq!(height, Theme::default().track_height + 40.0);
}

#[test]
fn dragging_the_masters_top_edge_changes_its_height() {
    let (project, _) = project();
    let mut harness = harness(project);
    let theme = Theme::default();
    let lanes = harness.get_by_label("Arrangement").rect();
    let edge = |harness: &Harness<'_, State>| {
        let header = harness.get_by_label("Master track").rect();
        pos2(header.center().x, header.top() + 1.0)
    };
    let height =
        |harness: &Harness<'_, State>| harness.state().editor().track_heights[&Chain::Master];
    // It's pinned along the bottom, so up makes it taller.
    let from = edge(&harness);
    drag(&mut harness, from, from - vec2(0.0, 30.0), Modifiers::NONE);
    assert_eq!(height(&harness), theme.track_height + 30.0);
    let master = harness.get_by_label("Master track").rect();
    assert_eq!(master.height(), theme.track_height + 30.0);
    assert_eq!(master.bottom(), lanes.bottom());
    // As far up as the master's part goes, half the arrangement.
    let from = edge(&harness);
    drag(
        &mut harness,
        from,
        pos2(from.x, lanes.top()),
        Modifiers::NONE,
    );
    assert_eq!(height(&harness), lanes.height() / 2.0);
    let master = harness.get_by_label("Master track").rect();
    assert_eq!(master.top(), lanes.bottom() - lanes.height() / 2.0);
    // And down, no shorter than a track can be.
    let from = edge(&harness);
    drag(
        &mut harness,
        from,
        pos2(from.x, lanes.bottom()),
        Modifiers::NONE,
    );
    assert_eq!(height(&harness), views::MIN_TRACK_HEIGHT);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn time_is_selected_on_the_master_and_across_to_it() {
    let (project, ids) = project();
    let reverb = project.tracks[3].id;
    let mut harness = harness(project);
    let theme = Theme::default();
    let master = across(&harness, "Master track");
    let on_master = |harness: &Harness<'_, State>, at| pos2(x_of(harness, at), master.center().y);
    // On the master alone, which is selected, and playing starts there.
    let (from, to) = (on_master(&harness, 4.3), on_master(&harness, 8.4));
    drag(&mut harness, from, to, Modifiers::NONE);
    let editor = harness.state().editor();
    let time = editor.selection.time.clone().expect("a time selection");
    assert_eq!(time.range, BeatRange::new(Beats::new(4), Beats::new(8)));
    assert!(time.tracks.is_empty() && time.master);
    assert_eq!(editor.selected_track, Some(Chain::Master));
    assert_eq!(editor.start_marker, Beats::new(4));
    // It's shaded over the master's row.
    let shaded = filled(&harness, theme.time_selection, master);
    let spans: Vec<_> = shaded.iter().map(|r| (r.left(), r.right())).collect();
    let [(left, right)] = spans[..] else {
        panic!("{spans:?}");
    };
    assert!((left - x_of(&harness, 4.0)).abs() < 0.5 && (right - x_of(&harness, 8.0)).abs() < 0.5);
    // From a track down to the master, it's on the tracks from there down,
    // and on the master.
    let (from, to) = (in_lane(&harness, "Bass", 2.0), on_master(&harness, 6.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let time = harness.state().editor().selection.time.clone().unwrap();
    assert_eq!(time.range, BeatRange::new(Beats::new(2), Beats::new(6)));
    assert_eq!(
        time.tracks.into_iter().collect::<Vec<_>>(),
        [ids.bass, reverb]
    );
    assert!(time.master);
    // And up from the master.
    let (from, to) = (on_master(&harness, 10.0), in_lane(&harness, "Keys", 4.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let time = harness.state().editor().selection.time.clone().unwrap();
    assert_eq!(time.range, BeatRange::new(Beats::new(4), Beats::new(10)));
    let tracks: Vec<_> = time.tracks.into_iter().collect();
    assert_eq!(tracks, [ids.keys, ids.bass, reverb]);
    assert!(time.master);
    // A right-click on the master's lane keeps it, as it would on a
    // track's.
    let at = on_master(&harness, 2.0);
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
    let time = harness.state().editor().selection.time.clone();
    assert!(time.is_some_and(|time| time.master && time.tracks.len() == 3));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn escape_cancels_a_drag() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Kick", 30.0);
    harness.hover_at(from);
    harness.run();
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=3 {
        harness.hover_at(from + vec2(20.0 * step as f32, 0.0));
        harness.run();
    }
    harness.key_press(egui::Key::Escape);
    harness.run();
    press(&mut harness, from + vec2(60.0, 0.0), false, Modifiers::NONE);
    assert_eq!(clip(&harness, ids.kick).1.start, Beats::ZERO);
}

#[test]
fn follow_turns_the_page_while_playing() {
    let (project, _) = project();
    let mut harness = harness(project);
    let player = &mut harness.state_mut().player;
    player.playing = true;
    player.playhead = Beats::new(80);
    harness.run();
    let viewport = harness.state().editor().viewport;
    assert!(viewport.visible().contains(Beats::new(80)), "{viewport:?}");
    // Scrolling away while it plays stops it following, until the
    // playhead's in sight again.
    let at = in_lane(&harness, "Drums", 80.0);
    harness.hover_at(at);
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -2000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::SHIFT,
    });
    harness.run();
    let away = harness.state().editor().viewport;
    assert!(away.start > Beats::new(90), "{away:?}");
    harness.state_mut().player.playhead = Beats::new(90);
    harness.run();
    assert_eq!(harness.state().editor().viewport, away);
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read in the tests".to_owned())
    }
}

#[test]
fn files_dropped_on_an_audio_track_go_there() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = in_lane(&harness, "Bass", 40.3);
    harness.hover_at(at);
    harness.run();
    let file = PathBuf::from("/music/take.wav");
    harness
        .input_mut()
        .dropped_files
        .push(Arc::new(Dropped(file.clone())));
    harness.run();
    let drop = Action::DropFiles {
        files: vec![file],
        track: Some(ids.bass),
        at: Some(Beats::new(40)),
    };
    assert!(harness.state().actions.contains(&drop));
}

/// Bass, with two takes of its two bars: Take 1, which plays bass.wav from
/// its start and which the Line clip is the comp of, as recording leaves
/// it, and Take 2, which plays it from a second in.
fn with_takes() -> (Project, Ids, [TakeId; 2]) {
    let (mut project, ids) = project();
    let range = BeatRange::new(Beats::new(16), Beats::new(24));
    let mut takes = Vec::new();
    for (name, offset) in [("Take 1", 0), ("Take 2", 48_000)] {
        let mut audio = AudioClip::new("bass.wav");
        audio.offset = Offset::Frames(offset);
        let clip = project.new_clip("Bass", range, ClipContent::Audio(audio));
        let mut take = project.new_take(name);
        take.clips.push(clip);
        takes.push(take);
    }
    let ids_of = [takes[0].id, takes[1].id];
    project.track_mut(ids.bass).unwrap().takes = takes;
    (project, ids, ids_of)
}

/// Keys, with two takes of its two bars: Take 1, with each note of Chords
/// two keys higher, and Take 2, which Chords is the comp of, as recording
/// leaves it.
fn with_midi_takes() -> (Project, Ids) {
    let (mut project, ids) = project();
    let (_, chords) = project.find_clip(ids.chords).unwrap();
    let (range, played) = (chords.range(), chords.content.clone());
    let mut higher = played.clone();
    let ClipContent::Midi(midi) = &mut higher else {
        panic!("Chords is MIDI");
    };
    for note in &mut midi.notes {
        note.key += 2;
    }
    let mut takes = Vec::new();
    for (name, content) in [("Take 1", higher), ("Take 2", played)] {
        let clip = project.new_clip("Chords", range, content);
        let mut take = project.new_take(name);
        take.clips.push(clip);
        takes.push(take);
    }
    project.track_mut(ids.keys).unwrap().takes = takes;
    (project, ids)
}

/// Somewhere in the lane of the take called `name`, at `at`.
fn in_take(harness: &Harness<'_, State>, name: &str, at: f64) -> Pos2 {
    let row = harness.get_by_label(name).rect();
    pos2(x_of(harness, at), row.center().y)
}

/// The track's clips, as where they are and where in their files they
/// start.
fn played(harness: &Harness<'_, State>, track: TrackId) -> Vec<(BeatRange, u64)> {
    let track = harness.state().project().track(track).unwrap();
    let mut clips: Vec<_> = track
        .clips
        .iter()
        .map(|clip| match &clip.content {
            ClipContent::Audio(audio) => (clip.range(), audio.offset.frames().unwrap_or(0)),
            ClipContent::Midi(_) => (clip.range(), 0),
        })
        .collect();
    clips.sort_by_key(|(range, _)| range.start);
    clips
}

#[test]
fn takes_show_in_lanes_and_dragging_or_clicking_comps_them() {
    let (project, ids, [first, second]) = with_takes();
    let mut harness = harness(project);
    // Hidden until the track shows them.
    assert!(harness.query_by_label("Take 1 of Bass").is_none());
    harness.get_by_label("Track Bass").click_secondary();
    harness.run();
    harness
        .get_by_label("Show/Hide Take Lanes Ctrl+Alt+U")
        .click();
    harness.run();
    let bass = harness.get_by_label("Track Bass").rect();
    let (one, two) = (
        harness.get_by_label("Take 1 of Bass").rect(),
        harness.get_by_label("Take 2 of Bass").rect(),
    );
    let reverb = harness.get_by_label("Track Reverb").rect();
    assert_eq!((one.top(), one.height()), (bass.bottom(), 36.0));
    assert_eq!((two.top(), reverb.top()), (one.bottom(), two.bottom()));
    let bars = |a, b| BeatRange::new(Beats::new(a), Beats::new(b));
    assert_eq!(played(&harness, ids.bass), [(bars(16, 24), 0)]);

    // A drag across Take 2 copies what's under it, on the grid, to the
    // track.
    let from = in_take(&harness, "Take 2 of Bass", 18.1);
    let to = in_take(&harness, "Take 2 of Bass", 19.9);
    drag(&mut harness, from, to, Modifiers::NONE);
    let comped = [
        (bars(16, 18), 0),
        // Two beats into Take 2 at 120 BPM: a second past its start.
        (bars(18, 20), 96_000),
        (bars(20, 24), 96_000),
    ];
    assert_eq!(played(&harness, ids.bass), comped);
    assert_eq!(harness.state().document.undo_name(), Some("Comp"));

    // A click takes a take's whole clip.
    let at = in_take(&harness, "Take 1 of Bass", 21.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(played(&harness, ids.bass), [(bars(16, 24), 0)]);

    // A double click on a take's name renames it, and its menu deletes it.
    // Long enough after the last click that it isn't a triple click.
    for _ in 0..40 {
        harness.step();
    }
    let header = harness.get_by_label("Take 2 of Bass").rect().center();
    harness.hover_at(header);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: header,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
    let rename = Request::Rename(Named::Take(second));
    assert_eq!(harness.state().requests, [rename]);
    harness.get_by_label("Take 1 of Bass").click_secondary();
    harness.run();
    harness.get_by_label("Delete Take").click();
    harness.run();
    assert!(harness.state().project().find_take(first).is_none());
    assert!(harness.query_by_label("Take 1 of Bass").is_none());
    assert!(harness.query_by_label("Take 2 of Bass").is_some());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn midi_takes_show_their_notes_and_where_the_track_plays_them() {
    let (project, ids) = with_midi_takes();
    let mut harness = harness(project);
    harness.get_by_label("Track Keys").click_secondary();
    harness.run();
    harness
        .get_by_label("Show/Hide Take Lanes Ctrl+Alt+U")
        .click();
    harness.run();
    let theme = Theme::default();
    let keys = harness.state().project().track_index(ids.keys).unwrap();
    let chords = (x_of(&harness, 8.0), x_of(&harness, 16.0));
    for (take, plays) in [("Take 1 of Keys", false), ("Take 2 of Keys", true)] {
        let lane = across(&harness, take);
        assert!(meshed(&harness, theme.notes, lane), "{take}'s notes");
        // In the track's colour where the track plays it.
        let coloured = filled(&harness, theme.clip(keys), lane);
        let spans: Vec<_> = coloured.iter().map(|r| (r.left(), r.right())).collect();
        match plays {
            false => assert!(spans.is_empty(), "{take}: {spans:?}"),
            true => {
                let [(left, right)] = spans[..] else {
                    panic!("{take}: {spans:?}");
                };
                assert!((left - chords.0).abs() < 0.5 && (right - chords.1).abs() < 0.5);
            }
        }
    }
}

/// Where `value` of a parameter like `spec` is on the automation lane
/// called `label`, at `at`.
fn on_lane(
    harness: &Harness<'_, State>,
    label: &str,
    spec: &ParamSpec,
    at: f64,
    value: f64,
) -> Pos2 {
    let row = harness.get_by_label(label).rect();
    let margin = Theme::default().point + 2.0;
    let unit = spec.scale.to_unit(value, spec.min, spec.max) as f32;
    let y = row.bottom() - margin - unit * (row.height() - 2.0 * margin);
    pos2(x_of(harness, at), y)
}

/// The points of the automation of `param`, as where they are and what
/// they're set to.
fn points(harness: &Harness<'_, State>, param: ParamAddress) -> Vec<(Beats, f64)> {
    let lane = harness.state().project().lane(param);
    let points = lane.map_or(&[][..], |lane| &lane.points[..]);
    points.iter().map(|point| (point.at, point.value)).collect()
}

/// Lets long enough go by since the last click that the next isn't a
/// double click.
fn pause(harness: &mut Harness<'_, State>) {
    for _ in 0..40 {
        harness.step();
    }
}

fn double_click(harness: &mut Harness<'_, State>, at: Pos2) {
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

#[track_caller]
fn near(value: f64, expected: f64) {
    assert!(
        (value - expected).abs() < 1e-4,
        "{value}, expected {expected}"
    );
}

#[test]
fn automation_lanes_show_under_their_tracks_and_edit_as_breakpoints() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let volume = ParamAddress::Volume(Chain::Track(ids.drums));
    let spec = ParamSpec::volume();
    let unit = |value| spec.scale.to_unit(value, spec.min, spec.max);
    let lane = "Volume of Drums";
    assert!(harness.query_by_label(lane).is_none());
    harness
        .get_by_label("Show automation of track Drums")
        .click();
    harness.run();

    // The volume's lane shows under the track, at the volume's own value
    // while it has no automation, and the next track under it.
    let drums = harness.get_by_label("Track Drums").rect();
    let row = harness.get_by_label(lane).rect();
    let height = Theme::default().automation_height;
    assert_eq!((row.top(), row.height()), (drums.bottom(), height));
    assert_eq!(
        harness.get_by_label("Track Keys").rect().top(),
        row.bottom()
    );
    assert_eq!(
        harness.get_by_label(lane).value().as_deref(),
        Some("0.0 dB")
    );
    let button = harness.get_by_label("Show automation of track Drums");
    assert_eq!(button.accesskit_node().toggled(), Some(Toggled::True));

    // A click on the line puts a point on it, on the grid, which changes
    // nothing until it moves. A double click off the line puts one where
    // it was clicked.
    let at = on_lane(&harness, lane, &spec, 4.3, 1.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(points(&harness, volume), [(Beats::new(4), 1.0)]);
    let undo =
        |harness: &Harness<'_, State>| harness.state().document.undo_name().map(str::to_owned);
    assert_eq!(undo(&harness).as_deref(), Some("Add Automation Point"));
    pause(&mut harness);
    let at = on_lane(&harness, lane, &spec, 12.2, 0.0);
    double_click(&mut harness, at);
    let added = [(Beats::new(4), 1.0), (Beats::new(12), 0.0)];
    assert_eq!(points(&harness, volume), added);
    harness.get_by_label("Point 2 of Volume of Drums");

    // Dragging a point moves it on the grid, and up or down.
    pause(&mut harness);
    let half = spec.scale.from_unit(0.5, spec.min, spec.max);
    let from = on_lane(&harness, lane, &spec, 12.0, 0.0);
    let to = on_lane(&harness, lane, &spec, 8.2, half);
    drag(&mut harness, from, to, Modifiers::NONE);
    let moved = points(&harness, volume);
    assert_eq!((moved[0], moved[1].0), (added[0], Beats::new(8)));
    near(moved[1].1, half);
    assert_eq!(undo(&harness).as_deref(), Some("Edit Automation"));

    // Dragging the line between two points moves them both.
    let middle = (unit(1.0) + 0.5) / 2.0;
    let middle = spec.scale.from_unit(middle, spec.min, spec.max);
    let from = on_lane(&harness, lane, &spec, 6.0, middle);
    let travel = row.height() - 2.0 * (Theme::default().point + 2.0);
    drag(
        &mut harness,
        from,
        from + vec2(0.0, travel / 4.0),
        Modifiers::NONE,
    );
    let lowered = points(&harness, volume);
    assert_eq!((lowered[0].0, lowered[1].0), (Beats::new(4), Beats::new(8)));
    near(unit(lowered[0].1), unit(1.0) - 0.25);
    near(unit(lowered[1].1), 0.25);

    // A click on a point deletes it. Deleting the last deletes the
    // automation, which leaves the volume where it had it at the playhead.
    pause(&mut harness);
    let at = on_lane(&harness, lane, &spec, 8.0, lowered[1].1);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(points(&harness, volume), [lowered[0]]);
    pause(&mut harness);
    let at = on_lane(&harness, lane, &spec, 4.0, lowered[0].1);
    click(&mut harness, at, Modifiers::NONE);
    assert!(harness.state().project().lane(volume).is_none());
    assert_eq!(undo(&harness).as_deref(), Some("Clear Automation"));
    let gain = harness.state().project().track(ids.drums).unwrap().gain;
    near(f64::from(gain), lowered[0].1);

    // The button hides the lanes again.
    harness
        .get_by_label("Show automation of track Drums")
        .click();
    harness.run();
    assert!(harness.query_by_label(lane).is_none());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn a_dragged_point_stays_where_it_was_let_go() {
    let (mut project, ids) = project();
    let volume = ParamAddress::Volume(Chain::Track(ids.drums));
    let set = vec![
        Point::new(Beats::new(4), 1.0),
        Point::new(Beats::new(12), 0.0),
    ];
    project.automation.push(Lane::new(volume, set));
    let mut harness = harness(project);
    harness
        .get_by_label("Show automation of track Drums")
        .click();
    harness.run();
    let (lane, spec) = ("Volume of Drums", ParamSpec::volume());
    let area = across(&harness, lane);
    let drawn = |harness: &Harness<'_, State>| -> Vec<Pos2> {
        (painted(harness).into_iter())
            .filter_map(|shape| match shape {
                Shape::Circle(circle) if area.contains(circle.center) => Some(circle.center),
                _ => None,
            })
            .collect()
    };

    // The frame that lets go draws the point where the drag left it, though
    // the app moves it only after the frame, and so do the frames after.
    let half = spec.scale.from_unit(0.5, spec.min, spec.max);
    let from = on_lane(&harness, lane, &spec, 12.0, 0.0);
    let to = on_lane(&harness, lane, &spec, 8.0, half);
    harness.hover_at(from);
    harness.run();
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    let dragged = drawn(&harness);
    assert_eq!(dragged.len(), 2);
    assert!(
        (dragged[1] - to).length() < 1.0,
        "{dragged:?}, dragged to {to:?}"
    );
    harness.event(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    assert_eq!(drawn(&harness), dragged);
    harness.run();
    assert_eq!(drawn(&harness), dragged);
    assert_eq!(points(&harness, volume)[1].0, Beats::new(8));
}

#[test]
fn a_drag_on_an_automation_lane_draws_a_step_of_the_grid_at_a_time() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let volume = ParamAddress::Volume(Chain::Track(ids.drums));
    let spec = ParamSpec::volume();
    let at_unit = |unit| spec.scale.from_unit(unit, spec.min, spec.max);
    let lane = "Volume of Drums";
    // The track's menu shows its automation too.
    harness.get_by_label("Track Drums").click_secondary();
    harness.run();
    harness.get_by_label("Show/Hide Automation A").click();
    harness.run();

    // Each step of the grid the drag crosses takes the value where it
    // crossed the step's middle, with a step from the volume's own value at
    // either end.
    let quarter = at_unit(0.25);
    let from = on_lane(&harness, lane, &spec, 2.5, quarter);
    let to = on_lane(&harness, lane, &spec, 7.5, quarter);
    drag(&mut harness, from, to, Modifiers::NONE);
    let drawn = points(&harness, volume);
    let at: Vec<Beats> = drawn.iter().map(|&(at, _)| at).collect();
    assert_eq!(at, [2, 2, 8, 8].map(Beats::new));
    for (&(_, value), expected) in drawn.iter().zip([1.0, quarter, quarter, 1.0]) {
        near(value, expected);
    }
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Edit Automation")
    );

    // With Alt, it draws freely, off the grid: here, a ramp up.
    let from = on_lane(&harness, lane, &spec, 10.0, 0.0);
    let to = on_lane(&harness, lane, &spec, 14.0, spec.max);
    drag(&mut harness, from, to, Modifiers::ALT);
    let project = harness.state().project();
    let automated = project.lane(volume).unwrap();
    let value = |at: f64| automated.value_at(Beats::from_f64(at), spec.scale).unwrap();
    for (at, unit) in [(11.0, 0.25), (12.0, 0.5), (13.0, 0.75)] {
        near(value(at), at_unit(unit));
    }
    // What was there before and after stays.
    near(value(5.0), quarter);
    near(value(9.0), 1.0);
    near(value(15.0), 1.0);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn an_automation_lanes_header_chooses_adds_and_hides_lanes() {
    let (mut project, ids) = project();
    // Drums sends to the reverb, and plays through a test gain.
    let reverb = project.tracks[3].id;
    let send = project.new_send(reverb);
    let gain = project.new_device(DeviceKind::TestGain { gain: 1.0 });
    let (send_level, gain_param) = (
        ParamAddress::Send(send.id),
        ParamAddress::Device(gain.id, 0),
    );
    let track = project.track_mut(ids.drums).unwrap();
    track.sends.push(send);
    track.devices.push(gain);
    let mut harness = harness(project);
    let chain = Chain::Track(ids.drums);
    let shown = |harness: &Harness<'_, State>| {
        let automation = &harness.state().editor().automation;
        automation.get(&chain).cloned().unwrap_or_default()
    };
    harness
        .get_by_label("Show automation of track Drums")
        .click();
    harness.run();
    assert_eq!(shown(&harness), [ParamAddress::Volume(chain)]);

    // The chooser offers the track's volume, pan and sends, then its
    // devices' parameters, and shows the one chosen in the lane's place.
    harness
        .get_by_label("Choose the parameter of Volume of Drums")
        .click();
    harness.run();
    harness.get_by_label("Pan");
    harness.get_by_label("Send to Reverb").click();
    harness.run();
    assert_eq!(shown(&harness), [send_level]);
    harness
        .get_by_label("Choose the parameter of Send to Reverb of Drums")
        .click();
    harness.run();
    harness.get_by_label("Test Gain").hover();
    harness.run();
    harness.get_by_label("Gain").click();
    harness.run();
    assert_eq!(shown(&harness), [gain_param]);
    harness.get_by_label("Test Gain: Gain of Drums");

    // Add shows a lane for the next parameter without one, under the
    // others, and Hide hides a lane.
    harness
        .get_by_label("Add a lane under Test Gain: Gain of Drums")
        .click();
    harness.run();
    assert_eq!(shown(&harness), [gain_param, ParamAddress::Volume(chain)]);
    harness
        .get_by_label("Hide Test Gain: Gain of Drums")
        .click();
    harness.run();
    assert_eq!(shown(&harness), [ParamAddress::Volume(chain)]);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn automation_menus_clear_and_re_enable_a_lane() {
    let (mut project, ids) = project();
    let volume = ParamAddress::Volume(Chain::Track(ids.drums));
    let points = vec![
        Point::new(Beats::ZERO, 0.5),
        Point::new(Beats::new(8), 0.25),
    ];
    let mut lane = musicable_model::automation::Lane::new(volume, points);
    lane.overridden = true;
    project.automation.push(lane);
    let mut harness = harness(project);
    harness
        .get_by_label("Show automation of track Drums")
        .click();
    harness.run();
    // Someone moved the volume by hand, so the lane says so, and its menu
    // has the volume follow its automation again.
    let header = harness.get_by_label("Volume of Drums");
    assert_eq!(header.value().as_deref(), Some("Overridden"));
    header.click_secondary();
    harness.run();
    harness.get_by_label("Re-Enable Automation").click();
    harness.run();
    let project = harness.state().project();
    assert!(!project.lane(volume).unwrap().overridden);
    let header = harness.get_by_label("Volume of Drums");
    assert_eq!(header.value().as_deref(), Some("-6.0 dB"));
    // Clear deletes it.
    harness.get_by_label("Volume of Drums").click_secondary();
    harness.run();
    assert!(harness.query_by_label("Re-Enable Automation").is_none());
    harness.get_by_label("Clear Automation").click();
    harness.run();
    assert!(harness.state().project().lane(volume).is_none());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_master_track_is_pinned_under_the_tracks() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let theme = Theme::default();
    let lanes = harness.get_by_label("Arrangement").rect();
    let master = harness.get_by_label("Master track").rect();
    let reverb = harness.get_by_label("Track Reverb").rect();
    // Along the bottom, with room for a new track over it.
    assert_eq!(master.bottom(), lanes.bottom());
    assert_eq!(master.height(), theme.track_height);
    assert!(master.top() > reverb.bottom());

    // Clicking its header selects it, alone, as the track the devices
    // panel shows.
    let kick = title(&harness, "Kick", 10.0);
    click(&mut harness, kick, Modifiers::NONE);
    click(&mut harness, master.center(), Modifiers::NONE);
    let editor = harness.state().editor();
    assert_eq!(editor.selected_track, Some(Chain::Master));
    assert_eq!(editor.selection, Selection::default());
    let row = harness.get_by_label("Master track");
    assert_eq!(row.accesskit_node().toggled(), Some(Toggled::True));
    // Clicking a track's header selects that track instead, and clicking
    // the master's lane selects the master again, and puts the start
    // marker there.
    let drums = harness.get_by_label("Track Drums").rect().center();
    click(&mut harness, drums, Modifiers::NONE);
    let selected = harness.state().editor().selected_track;
    assert_eq!(selected, Some(Chain::Track(ids.drums)));
    let row = harness.get_by_label("Master track");
    assert_eq!(row.accesskit_node().toggled(), Some(Toggled::False));
    let lane = pos2(x_of(&harness, 4.0), master.center().y);
    click(&mut harness, lane, Modifiers::NONE);
    let editor = harness.state().editor();
    assert_eq!(editor.selected_track, Some(Chain::Master));
    assert_eq!(editor.start_marker, Beats::new(4));
    // A drag there selects time on it.
    drag(&mut harness, lane, lane + vec2(80.0, 0.0), Modifiers::NONE);
    let time = harness.state().editor().selection.time.clone();
    assert!(time.is_some_and(|time| time.master && time.tracks.is_empty()));

    // Its A button shows its automation lanes, under its row, and the row
    // goes up to make room.
    let button = harness.get_by_label("Show automation of the master track");
    assert_eq!(button.accesskit_node().toggled(), Some(Toggled::False));
    button.click();
    harness.run();
    let lanes_shown = &harness.state().editor().automation;
    let volume = ParamAddress::Volume(Chain::Master);
    assert_eq!(lanes_shown.get(&Chain::Master), Some(&vec![volume]));
    let master = harness.get_by_label("Master track").rect();
    let lane = harness.get_by_label("Volume of Master").rect();
    assert_eq!(master.bottom(), lane.top());
    assert_eq!(lane.bottom(), lanes.bottom());
    let button = harness.get_by_label("Show automation of the master track");
    assert_eq!(button.accesskit_node().toggled(), Some(Toggled::True));

    // The tracks scroll over it, and it stays where it is.
    for track in [ids.drums, ids.keys, ids.bass] {
        let heights = &mut harness.state_mut().document.editor_mut().track_heights;
        heights.insert(Chain::Track(track), 150.0);
    }
    harness.run();
    // Out of sight, under the master.
    assert!(harness.query_by_label("Track Reverb").is_none());
    harness.hover_at(in_lane(&harness, "Drums", 4.0));
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -10_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    assert!(harness.state().editor().viewport.top > 0.0);
    let reverb = harness.get_by_label("Track Reverb").rect();
    assert!(
        reverb.bottom() <= master.top(),
        "{reverb:?} over {master:?}"
    );
    assert_eq!(harness.get_by_label("Master track").rect(), master);

    // Its lane's menu acts on the master's.
    harness.get_by_label("Volume of Master").click_secondary();
    harness.run();
    harness.get_by_label("Hide Automation Lane").click();
    harness.run();
    assert!(harness.query_by_label("Volume of Master").is_none());
    assert!(
        !harness
            .state()
            .editor()
            .automation
            .contains_key(&Chain::Master)
    );
    // And its header's shows and hides its lanes.
    harness.get_by_label("Master track").click_secondary();
    harness.run();
    harness.get_by_label("Show/Hide Automation A").click();
    harness.run();
    harness.get_by_label("Volume of Master");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_masters_lanes_scroll_on_their_own_when_they_dont_fit() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let lanes = harness.get_by_label("Arrangement").rect();
    let params = [
        ParamAddress::Volume(Chain::Master),
        ParamAddress::Pan(Chain::Master),
    ];
    let state = harness.state_mut();
    let shown = &mut state.document.editor_mut().automation;
    shown.insert(Chain::Master, params.to_vec());
    shown.insert(
        Chain::Track(ids.drums),
        vec![ParamAddress::Volume(Chain::Track(ids.drums))],
    );
    // Taller lanes than half the arrangement has room for.
    state.theme.automation_height = lanes.height() / 2.0;
    harness.run();
    let master = harness.get_by_label("Master track").rect();
    // Half the lanes, with the master's row at the top, and its second
    // lane out of sight.
    let top = lanes.bottom() - lanes.height() / 2.0;
    assert_eq!(master.top(), top);
    harness.get_by_label("Volume of Master");
    assert!(harness.query_by_label("Pan of Master").is_none());
    // The wheel over them scrolls them, not the tracks.
    harness.hover_at(pos2(x_of(&harness, 4.0), master.center().y));
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -10_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    assert_eq!(harness.state().editor().viewport.top, 0.0);
    let pan = harness.get_by_label("Pan of Master").rect();
    assert_eq!(pan.bottom(), lanes.bottom());
    // The tracks' lanes stay over the master's part.
    let drums = harness.get_by_label("Volume of Drums").rect();
    assert!(drums.bottom() <= top, "{drums:?}");
    assert!(harness.state().errors.is_empty());
}

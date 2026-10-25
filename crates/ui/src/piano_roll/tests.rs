//! The piano roll in a window, its gestures carried out on a document as
//! the app carries them out.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use musicable_commands::editor::Viewport;
use musicable_commands::{
    CommandError, Context, Document, Keymap, NoLibrary, Registry, Request, Value,
};
use musicable_model::clip::{AudioClip, ClipContent, ClipId, MidiClip, Note, NoteId, Point};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackKind};

use super::*;
use crate::shell::{Audio, KeymapStatus, Playback};
use crate::testing::Player;
use crate::{Levels, Plugins, Waveforms};

struct State {
    piano_roll: PianoRoll,
    document: Document,
    commands: Registry,
    keymap: Keymap,
    player: Player,
    levels: Levels,
    waveforms: Waveforms,
    theme: Theme,
    mouse: MouseMap,
    /// What the piano roll asked for, and what the commands asked of the
    /// app.
    actions: Vec<Action>,
    requests: Vec<Request>,
    errors: Vec<CommandError>,
}

impl State {
    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// Carries out `action` as the app does.
    fn carry_out(&mut self, action: &Action) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.player,
            requests: &mut self.requests,
            library: &mut NoLibrary,
        };
        let ran = match action {
            Action::Run(id) => self.commands.run_with(id, &Args::new(), &mut ctx),
            Action::RunWith(id, args) => self.commands.run_with(id, args, &mut ctx),
            _ => Ok(Value::None),
        };
        if let Err(error) = ran {
            self.errors.push(error);
        }
    }

    /// The notes of the clip the piano roll shows, by id.
    fn notes(&self) -> Vec<Note> {
        let id = self
            .editor()
            .piano_roll
            .clip
            .expect("a clip in the piano roll");
        let (_, clip) = self.document.project().find_clip(id).unwrap();
        let ClipContent::Midi(midi) = &clip.content else {
            panic!("the piano roll shows a MIDI clip");
        };
        let mut notes = midi.notes.clone();
        notes.sort_by_key(|note| note.id);
        notes
    }

    fn note(&self, id: NoteId) -> Note {
        let notes = self.notes();
        notes.into_iter().find(|note| note.id == id).unwrap()
    }

    fn selected(&self) -> Vec<NoteId> {
        let roll = &self.editor().piano_roll;
        roll.notes.iter().copied().collect()
    }

    /// How the clip the piano roll shows moves `controller`, as (beat in
    /// its own time, value).
    fn curve(&self, controller: Controller) -> Vec<(f64, f32)> {
        let id = self.editor().piano_roll.clip.unwrap();
        let (_, clip) = self.document.project().find_clip(id).unwrap();
        let ClipContent::Midi(midi) = &clip.content else {
            panic!("the piano roll shows a MIDI clip");
        };
        let points = midi.controller(controller).iter();
        points.map(|p| (p.at.to_f64(), p.value)).collect()
    }
}

/// Keys, a MIDI track with Chords, a clip from beat 4 to beat 12 whose own
/// time starts at its start, and Vox, an audio track. Chords' notes, in its
/// own time: C3 (60) from 0 for a beat, at velocity 0.5; E3 (64) from 1 for
/// a beat, at 0.75; and G3 (67) from 2 for two beats, at 1.
fn project() -> (Project, ClipId, [NoteId; 3]) {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    let vox = project.add_track("Vox", TrackKind::Audio);
    let mut midi = MidiClip::default();
    let notes = [(0, 1, 60, 0.5), (1, 1, 64, 0.75), (2, 2, 67, 1.0)].map(
        |(start, length, key, velocity)| {
            let note = midi.add_note(Beats::new(start), Beats::new(length), key, velocity);
            note.id
        },
    );
    let range = BeatRange::new(Beats::new(4), Beats::new(12));
    let chords = project.new_clip("Chords", range, ClipContent::Midi(midi));
    let words = project.new_clip("Words", range, ClipContent::Audio(AudioClip::new("v.wav")));
    let id = chords.id;
    project.track_mut(keys).unwrap().clips.push(chords);
    project.track_mut(vox).unwrap().clips.push(words);
    (project, id, notes)
}

/// The project, with the clip with id `clip` moving `controller` by
/// `points`: (beat in its own time, value).
fn moving(
    mut project: Project,
    clip: ClipId,
    controller: Controller,
    points: &[(f64, f32)],
) -> Project {
    let points = points
        .iter()
        .map(|&(at, value)| Point::new(Beats::from_f64(at), value))
        .collect();
    let clips = project.tracks.iter_mut().flat_map(|t| &mut t.clips);
    let clip = clips.into_iter().find(|c| c.id == clip).unwrap();
    let ClipContent::Midi(midi) = &mut clip.content else {
        panic!("a MIDI clip");
    };
    midi.set_controller(controller, points);
    project
}

/// Shows `controller` in the piano roll's lane.
fn show_lane(harness: &mut Harness<'_, State>, controller: Controller) {
    let roll = &mut harness.state_mut().document.editor_mut().piano_roll;
    roll.lane = Lane::Controller(controller);
    harness.run();
}

/// The piano roll showing `clip` in a window, with the focus, a beat a
/// hundred points wide from the song's third beat, so the grid's step is a
/// sixteenth, and E3 in the middle.
fn harness(project: Project, clip: Option<ClipId>) -> Harness<'static, State> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let mut document = Document::new(project);
    if let Some(id) = clip {
        let (project, editor) = document.parts();
        let (_, clip) = project.find_clip(id).unwrap();
        editor.open_in_piano_roll(clip);
        let roll = &mut editor.piano_roll;
        roll.viewport = Viewport {
            start: Beats::new(3),
            zoom: 100.0,
            width: 844.0,
            top: 603.0,
        };
    }
    let state = State {
        piano_roll: PianoRoll::default(),
        document,
        commands,
        keymap: Keymap::default_keymap(),
        player: Player::default(),
        levels: Levels::default(),
        waveforms: Waveforms::default(),
        theme: Theme::default(),
        mouse: MouseMap::live(),
        actions: Vec::new(),
        requests: Vec::new(),
        errors: Vec::new(),
    };
    // Frames as quick as a screen's, so two clicks make a double click.
    let mut harness = Harness::builder()
        .with_size([900.0, 400.0])
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
                        plugins: Plugins::default(),
                        native: &[],
                        library: crate::Library::default(),
                        workspaces: crate::Workspaces::default(),
                        monitors: &[],
                    };
                    let (theme, mouse) = (&state.theme, &state.mouse);
                    state.piano_roll.show(ui, &view, theme, mouse, &mut actions);
                }
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

/// Where the piano roll shows song position `at` on `key`'s row, halfway
/// down it.
fn at(harness: &Harness<'_, State>, at: f64, key: u8) -> Pos2 {
    let rows = harness.get_by_label("Piano Roll").rect();
    let roll = &harness.state().editor().piano_roll;
    let x = rows.left() + roll.viewport.x_of(Beats::from_f64(at));
    let row = f32::from(Note::MAX_KEY - key) + 0.5;
    pos2(x, rows.top() - roll.viewport.top + row * roll.key_height)
}

/// Where the controller lane shows song position `beat` at `value`.
fn on_lane(harness: &Harness<'_, State>, beat: f64, value: f32) -> Pos2 {
    let lane = harness.get_by_label("Controller Lane").rect();
    let x = at(harness, beat, 60).x;
    let marker = Theme::default().marker;
    pos2(x, lane.bottom() - 1.0 - (lane.height() - marker) * value)
}

/// Where the note called `name` is drawn.
fn note(harness: &Harness<'_, State>, name: &str) -> Rect {
    harness.get_by_label(name).rect()
}

/// The distance `beats` beats take across the piano roll.
fn width(harness: &Harness<'_, State>, beats: f64) -> f32 {
    (beats * harness.state().editor().piano_roll.viewport.zoom) as f32
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

fn double_click(harness: &mut Harness<'_, State>, at: Pos2) {
    // Long enough after any click before that egui doesn't count this one
    // with it.
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

fn wheel(harness: &mut Harness<'_, State>, over: Pos2, delta: Vec2, mods: Modifiers) {
    harness.hover_at(over);
    harness.run();
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: mods,
    });
    harness.run();
}

#[test]
fn notes_are_named_where_they_are() {
    let (project, clip, _) = project();
    let harness = harness(project, Some(clip));
    for name in [
        "Piano Roll",
        "Keyboard",
        "Piano Roll Ruler",
        "Velocities",
        "Note C3 at 2. 1. 1",
        "Note E3 at 2. 2. 1",
        "Note G3 at 2. 3. 1",
    ] {
        harness.get_by_label(name);
    }
    // The notes are where their song positions and keys are.
    let c3 = note(&harness, "Note C3 at 2. 1. 1");
    let start = at(&harness, 4.0, 60);
    assert!((c3.left() - start.x).abs() < 1.0, "{c3:?} {start:?}");
    assert!(c3.contains(pos2(start.x + 50.0, start.y)));
    assert!((c3.width() - width(&harness, 1.0)).abs() < 1.0);
    // The rows know how big they are, for scrolling notes into sight.
    let rows = harness.get_by_label("Piano Roll").rect();
    let roll = &harness.state().editor().piano_roll;
    assert_eq!(roll.viewport.width, rows.width());
    assert_eq!(roll.height, rows.height());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn notes_scrolled_away_are_left_out() {
    let (project, clip, _) = project();
    let mut harness = harness(project, Some(clip));
    harness
        .state_mut()
        .document
        .editor_mut()
        .piano_roll
        .viewport
        .top = 0.0;
    harness.run();
    assert!(harness.query_by_label("Note C3 at 2. 1. 1").is_none());
    // Middle C is called C4 in some DAWs.
    let editor = harness.state_mut().document.editor_mut();
    editor.piano_roll.viewport.top = 603.0;
    editor.behaviour.middle_c = musicable_commands::editor::MiddleC::C4;
    harness.run();
    harness.get_by_label("Note C4 at 2. 1. 1");
}

#[test]
fn without_a_midi_clip_it_says_how_to_show_one() {
    let (project, _, _) = project();
    let harness = harness(project, None);
    harness.get_by_label("Piano Roll");
    assert!(harness.query_by_label("Keyboard").is_none());
    assert!(harness.state().actions.is_empty());
}

#[test]
fn clicks_select_notes_and_place_the_start_marker() {
    let (project, clip, [c3, e3, _]) = project();
    let mut harness = harness(project, Some(clip));
    harness.state_mut().document.editor_mut().focus = Focus::ARRANGEMENT;
    let on = note(&harness, "Note C3 at 2. 1. 1").center();
    click(&mut harness, on, Modifiers::NONE);
    assert_eq!(harness.state().selected(), [c3]);
    // Clicking gives the piano roll the focus, so the keys act on its notes.
    assert_eq!(harness.state().editor().focus, Focus::PIANO_ROLL);
    let on = note(&harness, "Note E3 at 2. 2. 1").center();
    click(&mut harness, on, Modifiers::COMMAND);
    assert_eq!(harness.state().selected(), [c3, e3]);
    click(&mut harness, on, Modifiers::SHIFT);
    assert_eq!(harness.state().selected(), [c3]);
    // An empty row clears the selection and marks where it was clicked,
    // on the grid.
    let empty = at(&harness, 9.1, 70);
    click(&mut harness, empty, Modifiers::NONE);
    let editor = harness.state().editor();
    assert!(editor.piano_roll.notes.is_empty());
    assert_eq!(editor.start_marker, Beats::new(9));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn double_clicks_add_and_delete_notes() {
    let (project, clip, notes) = project();
    let mut harness = harness(project, Some(clip));
    // A note starts on the grid's line before where it was clicked, and
    // lasts a step of it.
    let empty = at(&harness, 10.4, 72);
    double_click(&mut harness, empty);
    let state = harness.state();
    let added = state.notes().into_iter().find(|n| !notes.contains(&n.id));
    let added = added.expect("a new note");
    assert_eq!(added.key, 72);
    assert_eq!(added.start, Beats::from_f64(6.25));
    assert_eq!(added.length, Beats::from_f64(0.25));
    assert_eq!(added.velocity, notes::VELOCITY);
    assert_eq!(state.selected(), [added.id]);
    assert_eq!(state.document.undo_name(), Some("Add Note"));
    // Double-clicking a note deletes it.
    let on = note(&harness, "Note C4 at 3. 3. 2").center();
    double_click(&mut harness, on);
    assert_eq!(harness.state().notes().len(), 3);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_notes_moves_them_on_the_grid_and_to_other_keys() {
    let (project, clip, [c3, e3, g3]) = project();
    let mut harness = harness(project, Some(clip));
    let from = note(&harness, "Note C3 at 2. 1. 1").center();
    let to = from + vec2(width(&harness, 1.1), -2.0 * 12.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let moved = harness.state().note(c3);
    assert_eq!((moved.start, moved.key), (Beats::new(1), 62));
    assert_eq!(moved.length, Beats::new(1));
    assert_eq!(harness.state().selected(), [c3]);
    assert_eq!(harness.state().document.undo_name(), Some("Move Notes"));
    // Selected notes move together.
    let on = note(&harness, "Note E3 at 2. 2. 1").center();
    click(&mut harness, on, Modifiers::COMMAND);
    let from = note(&harness, "Note E3 at 2. 2. 1").center();
    let to = from + vec2(width(&harness, 2.0), 12.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let state = harness.state();
    let starts = [c3, e3, g3].map(|id| (state.note(id).start, state.note(id).key));
    assert_eq!(
        starts,
        [
            (Beats::new(3), 61),
            (Beats::new(3), 63),
            (Beats::new(2), 67)
        ]
    );
    // A drag is one step to undo.
    assert!(harness.state_mut().document.undo());
    assert_eq!(harness.state().note(e3).start, Beats::new(1));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn ctrl_drag_copies_notes_and_selects_the_copies() {
    let (project, clip, [c3, ..]) = project();
    let mut harness = harness(project, Some(clip));
    let from = note(&harness, "Note C3 at 2. 1. 1").center();
    let to = from + vec2(width(&harness, 4.0), 0.0);
    drag(&mut harness, from, to, Modifiers::COMMAND);
    let state = harness.state();
    assert_eq!(state.notes().len(), 4);
    assert_eq!(state.note(c3).start, Beats::ZERO);
    let copy = state.selected();
    assert_eq!(copy.len(), 1);
    assert_eq!(state.note(copy[0]).start, Beats::new(4));
    assert_eq!(state.document.undo_name(), Some("Copy Notes"));
}

#[test]
fn edges_trim_notes() {
    let (project, clip, [.., g3]) = project();
    let mut harness = harness(project, Some(clip));
    let rect = note(&harness, "Note G3 at 2. 3. 1");
    let from = pos2(rect.right() - 2.0, rect.center().y);
    let to = from - vec2(width(&harness, 1.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let g = harness.state().note(g3);
    assert_eq!((g.start, g.length), (Beats::new(2), Beats::new(1)));
    assert_eq!(harness.state().document.undo_name(), Some("Trim Notes"));
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = from + vec2(width(&harness, 0.5), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let g = harness.state().note(g3);
    assert_eq!((g.start, g.end()), (Beats::from_f64(2.5), Beats::new(3)));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn alt_drags_and_the_velocity_lane_change_velocities() {
    let (project, clip, [c3, e3, _]) = project();
    let mut harness = harness(project, Some(clip));
    let travel = Theme::default().piano_roll.velocity_drag;
    let from = note(&harness, "Note C3 at 2. 1. 1").center();
    drag(
        &mut harness,
        from,
        from - vec2(0.0, travel / 4.0),
        Modifiers::ALT,
    );
    let velocity = harness.state().note(c3).velocity;
    assert!((velocity - 0.75).abs() < 0.01, "{velocity}");
    assert_eq!(harness.state().document.undo_name(), Some("Set Velocity"));
    // In the lane, a drag across its height goes from the softest to the
    // hardest.
    let lane = harness.get_by_label("Velocities").rect();
    let x = at(&harness, 5.0, 64).x + 1.0;
    let from = pos2(x, lane.bottom() - 10.0);
    drag(
        &mut harness,
        from,
        from - vec2(0.0, lane.height() / 4.0),
        Modifiers::NONE,
    );
    let state = harness.state();
    assert!((state.note(e3).velocity - 1.0).abs() < 0.01);
    assert_eq!(state.selected(), [e3]);
    // Velocities stop at the hardest.
    assert!(state.note(e3).velocity <= 1.0);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_over_the_rows_selects_the_notes_in_the_rectangle() {
    let (project, clip, [c3, e3, _]) = project();
    let mut harness = harness(project, Some(clip));
    let from = at(&harness, 3.5, 66);
    let to = at(&harness, 5.5, 59);
    drag(&mut harness, from, to, Modifiers::NONE);
    assert_eq!(harness.state().selected(), [c3, e3]);
    // Selecting isn't a change to undo.
    assert_eq!(harness.state().document.undo_name(), None);
}

#[test]
fn draw_mode_draws_along_a_drag_and_clicks_delete() {
    let (project, clip, notes) = project();
    let mut harness = harness(project, Some(clip));
    harness.state_mut().document.editor_mut().piano_roll.draw = true;
    let from = at(&harness, 8.05, 72);
    let to = at(&harness, 9.05, 72);
    drag(&mut harness, from, to, Modifiers::NONE);
    let state = harness.state();
    let drawn: Vec<(Beats, Beats, u8)> = state
        .notes()
        .iter()
        .filter(|n| !notes.contains(&n.id))
        .map(|n| (n.start, n.length, n.key))
        .collect();
    let sixteenth = Beats::from_f64(0.25);
    let expected: Vec<_> = [4.0, 4.25, 4.5, 4.75, 5.0]
        .map(|start| (Beats::from_f64(start), sixteenth, 72))
        .into();
    assert_eq!(drawn, expected);
    // As one step to undo.
    assert_eq!(state.document.undo_name(), Some("Draw Notes"));
    // A click deletes a note, and adds one where there's none.
    let on = note(&harness, "Note C3 at 2. 1. 1").center();
    click(&mut harness, on, Modifiers::NONE);
    assert_eq!(harness.state().notes().len(), 7);
    let empty = at(&harness, 10.1, 48 + 24);
    click(&mut harness, empty, Modifiers::NONE);
    assert_eq!(harness.state().notes().len(), 8);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_keyboard_selects_the_notes_on_a_key() {
    let (project, clip, [c3, e3, _]) = project();
    let mut harness = harness(project, Some(clip));
    let keys = harness.get_by_label("Keyboard").rect();
    let key = |harness: &Harness<'_, State>, key| pos2(keys.center().x, at(harness, 4.0, key).y);
    let c = key(&harness, 60);
    click(&mut harness, c, Modifiers::NONE);
    assert_eq!(harness.state().selected(), [c3]);
    let e = key(&harness, 64);
    click(&mut harness, e, Modifiers::SHIFT);
    assert_eq!(harness.state().selected(), [c3, e3]);
}

#[test]
fn the_wheel_scrolls_and_zooms_the_keys_and_the_time() {
    let (project, clip, _) = project();
    let mut harness = harness(project, Some(clip));
    let over = at(&harness, 6.0, 64);
    wheel(&mut harness, over, vec2(0.0, -24.0), Modifiers::NONE);
    let roll = &harness.state().editor().piano_roll;
    assert_eq!(roll.viewport.top, 627.0);
    let sideways = vec2(0.0, -width(&harness, 1.0));
    wheel(&mut harness, over, sideways, Modifiers::SHIFT);
    assert_eq!(
        harness.state().editor().piano_roll.viewport.start,
        Beats::new(4)
    );
    // Over the keyboard, Ctrl and the wheel make the rows taller, keeping
    // the key under the pointer where it is.
    let keys = harness.get_by_label("Keyboard").rect();
    let over = pos2(keys.center().x, at(&harness, 6.0, 64).y);
    wheel(&mut harness, over, vec2(0.0, 100.0), Modifiers::COMMAND);
    let roll = &harness.state().editor().piano_roll;
    assert!(roll.key_height > 12.0, "{}", roll.key_height);
    let after = at(&harness, 6.0, 64);
    assert!((after.y - over.y).abs() < roll.key_height / 2.0);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_ruler_locates_and_dragging_it_zooms() {
    let (project, clip, _) = project();
    let mut harness = harness(project, Some(clip));
    let ruler = harness.get_by_label("Piano Roll Ruler").rect();
    let x = at(&harness, 6.05, 60).x;
    click(&mut harness, pos2(x, ruler.center().y), Modifiers::NONE);
    assert_eq!(harness.state().player.playhead, Beats::new(6));
    assert_eq!(harness.state().editor().start_marker, Beats::new(6));
    let from = pos2(x, ruler.center().y);
    drag(&mut harness, from, from + vec2(0.0, 40.0), Modifiers::NONE);
    let zoom = harness.state().editor().piano_roll.viewport.zoom;
    assert!(zoom > 100.0, "{zoom}");
    // The arrangement's zoom stays.
    assert_eq!(
        harness.state().editor().viewport.zoom,
        Viewport::default().zoom
    );
}

#[test]
fn context_menus_offer_commands_for_what_was_clicked() {
    let (project, clip, [c3, ..]) = project();
    let mut harness = harness(project, Some(clip));
    harness.state_mut().document.editor_mut().focus = Focus::ARRANGEMENT;
    harness.get_by_label("Note C3 at 2. 1. 1").click_secondary();
    harness.run();
    for item in [
        "Cut Ctrl+X",
        "Copy Ctrl+C",
        "Quantize Ctrl+U",
        "Snap to Grid Ctrl+4",
    ] {
        harness.get_by_label(item);
    }
    // The menu selects the note and gives the piano roll the focus, so the
    // edit commands act on it.
    assert_eq!(harness.state().selected(), [c3]);
    assert_eq!(harness.state().editor().focus, Focus::PIANO_ROLL);
    harness.get_by_label("Delete Backspace, Delete").click();
    harness.run();
    assert_eq!(harness.state().notes().len(), 2);
    // On an empty row, Draw Mode is ticked when it's on.
    let empty = at(&harness, 9.0, 70);
    harness.hover_at(empty);
    harness.run();
    harness.event(Event::PointerButton {
        pos: empty,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.event(Event::PointerButton {
        pos: empty,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    harness.get_by_label("Draw Mode B").click();
    harness.run();
    assert!(harness.state().editor().piano_roll.draw);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn escape_cancels_a_drag() {
    let (project, clip, [c3, ..]) = project();
    let mut harness = harness(project, Some(clip));
    let from = note(&harness, "Note C3 at 2. 1. 1").center();
    harness.hover_at(from);
    harness.run();
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=3 {
        harness.hover_at(from + vec2(40.0 * step as f32, 0.0));
        harness.run();
    }
    harness.key_press(Key::Escape);
    harness.run();
    press(
        &mut harness,
        from + vec2(120.0, 0.0),
        false,
        Modifiers::NONE,
    );
    assert_eq!(harness.state().note(c3).start, Beats::ZERO);
}

#[test]
fn the_lane_s_menu_chooses_what_it_shows() {
    let (project, clip, _) = project();
    let project = moving(project, clip, Controller::SUSTAIN, &[(0.0, 1.0)]);
    let mut harness = harness(project, Some(clip));
    let chooser = harness.get_by_role_and_label(Role::ComboBox, "Lane");
    assert_eq!(chooser.value().as_deref(), Some("Velocity"));
    chooser.click();
    harness.run();
    // The controllers the clip moves come first, then those keyboards and
    // pedals have, then the rest.
    for name in ["CC 64 Sustain", "CC 1 Mod Wheel", "CC 3", "CC 119"] {
        harness.get_by_role_and_label(Role::Button, name);
    }
    harness
        .get_by_role_and_label(Role::Button, "CC 1 Mod Wheel")
        .click();
    harness.run();
    let lane = harness.state().editor().piano_roll.lane;
    assert_eq!(lane, Lane::Controller(Controller::MOD_WHEEL));
    harness.get_by_label("Controller Lane");
    assert!(harness.query_by_label("Velocities").is_none());
    let chooser = harness.get_by_role_and_label(Role::ComboBox, "Lane");
    assert_eq!(chooser.value().as_deref(), Some("CC 1 Mod Wheel"));
    // Choosing isn't a change to undo.
    assert_eq!(harness.state().document.undo_name(), None);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_in_the_controller_lane_draws_a_step_of_the_grid_at_a_time() {
    let (project, clip, _) = project();
    let mut harness = harness(project, Some(clip));
    let wheel = Controller::MOD_WHEEL;
    show_lane(&mut harness, wheel);
    let from = on_lane(&harness, 5.05, 0.25);
    let to = on_lane(&harness, 6.05, 0.75);
    drag(&mut harness, from, to, Modifiers::NONE);
    let state = harness.state();
    let curve = state.curve(wheel);
    // A point at the start of each sixteenth, the grid's step, in the
    // clip's own time, rising with the drag, in MIDI's steps. Before, the
    // clip left the wheel alone, so it's left alone after.
    let at: Vec<f64> = curve.iter().map(|p| p.0).collect();
    assert_eq!(at, [1.0, 1.25, 1.5, 1.75, 2.0]);
    let values: Vec<f32> = curve.iter().map(|p| p.1).collect();
    assert_eq!(values[0], 32.0 / 127.0);
    assert_eq!(values[4], 95.0 / 127.0);
    assert!(values.is_sorted(), "{values:?}");
    for value in values {
        let steps = value * 127.0;
        assert!((steps - steps.round()).abs() < 1e-4, "{value}");
    }
    // As one step to undo.
    assert_eq!(state.document.undo_name(), Some("Draw Controller"));
    assert!(state.errors.is_empty());
}

#[test]
fn alt_drags_erase_and_clicks_draw_a_step() {
    let (project, clip, _) = project();
    let sustain = Controller::SUSTAIN;
    let pedal = [(0.0, 1.0), (1.0, 0.0), (2.0, 1.0), (3.0, 0.0)];
    let mut harness = harness(moving(project, clip, sustain, &pedal), Some(clip));
    show_lane(&mut harness, sustain);
    // From 0.5 to 2.75 in the clip's own time: the pedal stays down, as it
    // was before, until it comes up at 3.
    let from = on_lane(&harness, 4.55, 0.5);
    let to = on_lane(&harness, 6.55, 0.5);
    drag(&mut harness, from, to, Modifiers::ALT);
    assert_eq!(harness.state().curve(sustain), [(0.0, 1.0), (3.0, 0.0)]);
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Erase Controller")
    );
    // A switch is on from halfway up: a click there holds the pedal down a
    // sixteenth longer.
    let on = on_lane(&harness, 7.05, 0.6);
    click(&mut harness, on, Modifiers::NONE);
    assert_eq!(harness.state().curve(sustain), [(0.0, 1.0), (3.25, 0.0)]);
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Draw Controller")
    );
    // The lane's menu clears it.
    let lane = harness.get_by_label("Controller Lane").rect();
    let empty = pos2(on.x + 100.0, lane.center().y);
    harness.hover_at(empty);
    harness.run();
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos: empty,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
    harness.get_by_label("Clear Controller").click();
    harness.run();
    assert_eq!(harness.state().curve(sustain), []);
    assert!(harness.state().errors.is_empty());
}

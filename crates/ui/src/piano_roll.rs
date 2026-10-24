//! The piano roll: the notes of the MIDI clip it shows, on rows of keys
//! beside a keyboard, under a ruler and over a lane of their velocities or
//! a controller's curve, as Live's MIDI note editor shows them.
//!
//! Like the arrangement, it's drawn with egui's painter, only what's in
//! sight, and everything it does goes through commands. What it shows and
//! has selected, its zoom and its scrolling are the editor's
//! [`PianoRoll`](musicable_commands::editor::PianoRoll), outside the widget,
//! beside the arrangement's selection and zoom, so commands, scripts and
//! extensions see what the user sees. Gestures are looked up in the
//! [`MouseMap`], and a drag that changes notes shows where they'll go while
//! it lasts and runs one command when it ends, so it's one step in the undo
//! history. Context menus list commands from the registry. Across, it shows
//! song positions, as the arrangement does, so the start marker, the
//! playhead and the grid are the same in both.

mod menu;
mod paint;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use egui::{
    CursorIcon, Event, Id, MouseWheelUnit, Pos2, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2,
};
use musicable_commands::editor::{Lane, Viewport};
use musicable_commands::{
    Args, CommandId, Editor, Focus, Modifiers, controllers, notes, transport, view as views,
};
use musicable_model::Project;
use musicable_model::clip::{Clip, ClipContent, Controller, MidiClip, Note, NoteId};
use musicable_model::time::{BeatRange, Beats};

use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::widgets::position;

/// How much a point of the wheel, or of a drag up or down the ruler,
/// zooms: the zoom changes by `e` to the power of this many points.
const ZOOM_SPEED: f32 = 0.005;
const SCRUB_ZOOM_SPEED: f32 = 0.01;

/// The keys there are: MIDI notes 0 to 127.
const KEYS: f32 = Note::MAX_KEY as f32 + 1.0;

/// How wide a step of a controller's curve drawn off the grid is, in
/// points.
const FREE_STEP: f32 = 3.0;

/// The piano roll's own state: the gesture in progress, and the menu open.
#[derive(Debug, Default)]
pub(crate) struct PianoRoll {
    drag: Option<Drag>,
    /// What the open context menu was opened on.
    menu: Option<Spot>,
    /// Set when the user scrolls away from the playhead while it plays, so
    /// Follow doesn't scroll back until the playhead is in sight again.
    follow_paused: bool,
}

/// The parts of the piano roll that take the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Rows,
    Keys,
    Ruler,
    /// The lane under the rows.
    Lane,
}

/// What's under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Note {
        note: NoteId,
        part: Part,
    },
    /// A key's row, where it has no note.
    Row(u8),
    /// A key of the keyboard.
    Key(u8),
    Ruler,
    /// The velocities' lane, on a note's velocity or beside them.
    Velocity(Option<NoteId>),
    /// The lane while it shows a controller.
    Controller,
}

impl Hit {
    /// What the mouse map calls it, with Draw Mode on or off.
    fn target(self, draw: bool) -> Target {
        match self {
            Hit::Note {
                part: Part::Edge(_),
                ..
            } => Target::NoteEdge,
            Hit::Note { .. } if draw => Target::DrawNote,
            Hit::Note { .. } => Target::Note,
            Hit::Row(_) if draw => Target::DrawGrid,
            Hit::Row(_) => Target::NoteGrid,
            Hit::Key(_) => Target::PianoKey,
            Hit::Ruler => Target::Ruler,
            Hit::Velocity(_) => Target::VelocityLane,
            Hit::Controller => Target::ControllerLane,
        }
    }

    /// The note it's on, if it's on one.
    fn note(self) -> Option<NoteId> {
        match self {
            Hit::Note { note, .. } | Hit::Velocity(Some(note)) => Some(note),
            _ => None,
        }
    }
}

/// A part of a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Body,
    Edge(Side),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Start,
    End,
}

/// A drag in progress.
#[derive(Debug, Clone)]
struct Drag {
    area: Area,
    action: MouseAction,
    hit: Hit,
    /// Where it started, and where the pointer is now.
    from: Pos2,
    to: Pos2,
    /// The notes it moves, trims or changes the velocity of.
    notes: Vec<NoteId>,
    /// The viewport as the drag started.
    viewport: Viewport,
    /// Where Draw Mode adds notes, by the song positions they start at, on
    /// the keys the drag passed over there.
    drawn: BTreeMap<Beats, u8>,
    /// The controller the lane showed as the drag started, if it showed
    /// one, and the steps of its curve the drag drew or erased: by the song
    /// positions they start at, where they end and the value drawn there.
    controller: Option<Controller>,
    steps: BTreeMap<Beats, (Beats, f32)>,
    /// The step the pointer was last in, and the value there.
    last_step: Option<(Beats, f32)>,
    /// What the drag last asked for, for those that ask while they last.
    sent: Option<Plan>,
}

/// What a drag would do if it ended now.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    /// Moves the notes by some beats and some keys up, or copies them.
    Move {
        notes: Vec<NoteId>,
        by: Beats,
        keys: i64,
        copy: bool,
    },
    /// Moves the notes' starts, or their ends.
    Trim {
        notes: Vec<NoteId>,
        side: Side,
        by: Beats,
    },
    /// Makes the notes start harder, or softer.
    Velocity { notes: Vec<NoteId>, by: f32 },
    /// Selects the notes in a rectangle. Drags ask for it as they go.
    Area { notes: Vec<NoteId>, rect: Rect },
    /// Adds notes at these song positions, on these keys.
    Draw { notes: Vec<(Beats, u8)> },
    /// Draws over a stretch of song positions of a controller's curve,
    /// with a point at each of these.
    DrawController {
        controller: Controller,
        range: BeatRange,
        points: Vec<(Beats, f32)>,
    },
    /// Erases a stretch of a controller's curve.
    EraseController {
        controller: Controller,
        range: BeatRange,
    },
    /// Scrolls and zooms. Drags ask for it as they go.
    Viewport(Viewport),
}

/// Where a context menu was opened, and what on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    hit: Hit,
    /// The song position there, snapped to the grid.
    at: Beats,
}

/// Where the piano roll's parts are this frame.
#[derive(Debug, Clone)]
struct Layout {
    /// The ruler over the rows, and the corner beside it, over the
    /// keyboard.
    ruler: Rect,
    corner: Rect,
    keys: Rect,
    /// The rows of keys, where the notes are.
    rows: Rect,
    /// The lane under the rows, and its name beside it, which chooses what
    /// it shows.
    lane: Rect,
    label: Rect,
    /// The zoom and scrolling, with the rows' width this frame.
    viewport: Viewport,
    key_height: f32,
}

impl Layout {
    fn new(rect: Rect, theme: &Theme, viewport: Viewport, key_height: f32) -> Self {
        let sizes = theme.piano_roll;
        let split = (rect.left() + sizes.keys).min(rect.right());
        let ruler_bottom = (rect.top() + sizes.ruler).min(rect.bottom());
        let lane_top = (rect.bottom() - sizes.lane).max(ruler_bottom);
        let ruler = Rect::from_min_max(pos2(split, rect.top()), pos2(rect.right(), ruler_bottom));
        let corner = Rect::from_min_max(rect.left_top(), pos2(split, ruler_bottom));
        let keys = Rect::from_min_max(pos2(rect.left(), ruler_bottom), pos2(split, lane_top));
        let rows = Rect::from_min_max(pos2(split, ruler_bottom), pos2(rect.right(), lane_top));
        let lane = Rect::from_min_max(pos2(split, lane_top), rect.right_bottom());
        let label = Rect::from_min_max(pos2(rect.left(), lane_top), pos2(split, rect.bottom()));
        let lowest = (KEYS * key_height - rows.height()).max(0.0);
        let viewport = Viewport {
            width: rows.width(),
            top: viewport.top.clamp(0.0, lowest),
            ..viewport
        };
        Self {
            ruler,
            corner,
            keys,
            rows,
            lane,
            label,
            viewport,
            key_height,
        }
    }

    fn x_of(&self, at: Beats) -> f32 {
        self.rows.left() + self.viewport.x_of(at)
    }

    fn beats_at(&self, x: f32) -> Beats {
        self.viewport.beats_at(x - self.rows.left())
    }

    /// The top of `key`'s row.
    fn y_of(&self, key: u8) -> f32 {
        self.rows.top() - self.viewport.top + (KEYS - 1.0 - f32::from(key)) * self.key_height
    }

    /// The key whose row is at `y`, or the nearest one above or below them
    /// all.
    fn key_at(&self, y: f32) -> u8 {
        let row = ((y - self.rows.top() + self.viewport.top) / self.key_height).floor();
        (KEYS - 1.0 - row).clamp(0.0, KEYS - 1.0) as u8
    }

    /// The keys whose rows are in sight, from the lowest to the highest.
    fn visible_keys(&self) -> std::ops::RangeInclusive<u8> {
        self.key_at(self.rows.bottom() - 0.5)..=self.key_at(self.rows.top())
    }

    /// Where a note on `key` from song position `start` to `end` is drawn.
    fn note(&self, key: u8, start: Beats, end: Beats) -> Rect {
        let top = self.y_of(key);
        let (left, right) = (self.x_of(start), self.x_of(end));
        Rect::from_x_y_ranges(
            left..=right.max(left + 1.0),
            top + 0.5..=top + self.key_height - 0.5,
        )
    }

    /// Where the lane shows values from 0 to 1: the foot, at 0, and how far
    /// up from it 1 is, leaving room for a velocity's head at the top.
    fn span(&self, theme: &Theme) -> (f32, f32) {
        let lane = self.lane;
        (lane.bottom() - 1.0, (lane.height() - theme.marker).max(1.0))
    }
}

/// What the piano roll shows this frame, and where.
struct Scene<'s> {
    view: &'s View<'s>,
    theme: &'s Theme,
    mouse: &'s MouseMap,
    layout: Layout,
    /// The clip it shows, its notes, and its track's place among the
    /// tracks, for its colour.
    clip: &'s Clip,
    midi: &'s MidiClip,
    index: usize,
    /// Where the pointer is, if it's over the piano roll and nothing's in
    /// the way.
    pointer: Option<Pos2>,
}

impl Scene<'_> {
    fn project(&self) -> &Project {
        self.view.project
    }

    fn editor(&self) -> &Editor {
        self.view.editor
    }

    fn selected(&self, note: NoteId) -> bool {
        self.editor().piano_roll.notes.contains(&note)
    }

    /// Where `at`, in the clip's own time, plays in the song.
    fn song(&self, at: Beats) -> Beats {
        notes::song_position(self.clip, self.midi, at)
    }

    /// The moment in the clip's own time that plays at `at` in the song.
    fn own(&self, at: Beats) -> Beats {
        notes::clip_position(self.clip, self.midi, at)
    }

    /// Where `note` is drawn.
    fn note_rect(&self, note: &Note) -> Rect {
        let start = self.song(note.start);
        self.layout.note(note.key, start, start + note.length)
    }

    /// `at`, snapped to the grid unless `free` or snapping is off.
    fn snap(&self, at: Beats, free: bool) -> Beats {
        if free {
            return at;
        }
        let zoom = self.layout.viewport.zoom;
        self.editor().grid.snap(&self.project().tempo, at, zoom)
    }

    /// The song position at `x`, snapped to the grid.
    fn snapped(&self, x: f32) -> Beats {
        self.snap(self.layout.beats_at(x), false).max(Beats::ZERO)
    }

    /// Where a note drawn at `x` starts: on the grid's line at `x` or the
    /// last one before it, while snapping is on.
    fn drawn_at(&self, x: f32) -> Beats {
        let at = self.layout.beats_at(x).max(Beats::ZERO);
        let editor = self.editor();
        match editor.grid.snap {
            true => editor
                .grid
                .floor(&self.project().tempo, at, self.layout.viewport.zoom),
            false => at,
        }
    }

    /// How long a note drawn at song position `at` lasts: a step of the
    /// grid.
    fn step_at(&self, at: Beats) -> Beats {
        let zoom = self.layout.viewport.zoom;
        let tempo = &self.project().tempo;
        self.editor().grid.step_at(tempo, at.max(Beats::ZERO), zoom)
    }

    /// The controller the lane shows, if it shows one.
    fn controller(&self) -> Option<Controller> {
        match self.editor().piano_roll.lane {
            Lane::Controller(controller) => Some(controller),
            Lane::Velocity => None,
        }
    }

    /// The song position at `x` in the lane, kept within the lane and from
    /// the song's start.
    fn lane_at(&self, x: f32) -> Beats {
        let lane = self.layout.lane;
        let at = self.layout.beats_at(x.clamp(lane.left(), lane.right()));
        at.max(Beats::ZERO)
    }

    /// The step of a controller's curve that drawing at song position `at`
    /// fills: the grid's step there while snapping is on, or else a few
    /// points' worth.
    fn controller_step(&self, at: Beats) -> BeatRange {
        let viewport = &self.layout.viewport;
        let grid = self.editor().grid;
        if grid.snap {
            return grid.cell(&self.project().tempo, at, viewport.zoom);
        }
        let step = viewport.beats_in(FREE_STEP).ticks().max(1);
        let start = at.ticks() - at.ticks().rem_euclid(step);
        BeatRange::new(Beats::from_ticks(start), Beats::from_ticks(start + step))
    }

    /// The value drawn at `y` in the lane for `controller`.
    fn controller_value(&self, y: f32, controller: Controller) -> f32 {
        let (foot, height) = self.layout.span(self.theme);
        drawn_value((foot - y) / height, controller)
    }

    /// What's under `pos` in `area`.
    fn hit(&self, area: Area, pos: Pos2) -> Hit {
        let layout = &self.layout;
        match area {
            Area::Rows => self.hit_rows(pos),
            Area::Keys => Hit::Key(layout.key_at(pos.y)),
            Area::Ruler => Hit::Ruler,
            Area::Lane if self.controller().is_some() => Hit::Controller,
            Area::Lane => {
                // The note whose start is nearest, among those there.
                let grab = self.theme.piano_roll.edge_grab;
                let near = self.midi.notes.iter().filter_map(|note| {
                    let rect = self.note_rect(note);
                    let reach = rect.left() - grab..=rect.right().max(rect.left() + grab);
                    reach
                        .contains(&pos.x)
                        .then(|| ((rect.left() - pos.x).abs(), note.id))
                });
                let nearest = near.min_by(|a, b| a.0.total_cmp(&b.0));
                Hit::Velocity(nearest.map(|(_, id)| id))
            }
        }
    }

    fn hit_rows(&self, pos: Pos2) -> Hit {
        let key = self.layout.key_at(pos.y);
        // The last drawn is on top.
        for note in self.midi.notes.iter().rev().filter(|n| n.key == key) {
            let rect = self.note_rect(note);
            if pos.x < rect.left() - 1.0 || pos.x > rect.right() + 1.0 {
                continue;
            }
            let grab = self.theme.piano_roll.edge_grab.min(rect.width() / 3.0);
            let part = if pos.x <= rect.left() + grab {
                Part::Edge(Side::Start)
            } else if pos.x >= rect.right() - grab {
                Part::Edge(Side::End)
            } else {
                Part::Body
            };
            return Hit::Note {
                note: note.id,
                part,
            };
        }
        Hit::Row(key)
    }

    /// The notes with ids `ids`.
    fn notes<'n>(&'n self, ids: &'n [NoteId]) -> impl Iterator<Item = &'n Note> + 'n {
        ids.iter().filter_map(|&id| self.midi.note(id))
    }

    /// The notes on `key`.
    fn on_key(&self, key: u8) -> Vec<NoteId> {
        let notes = self.midi.notes.iter().filter(|n| n.key == key);
        notes.map(|n| n.id).collect()
    }

    /// What `drag` would do if it ended now.
    fn plan(&self, drag: &Drag) -> Option<Plan> {
        let layout = &self.layout;
        let moved = layout.viewport.beats_in(drag.to.x - drag.from.x);
        match (drag.action, drag.hit) {
            (MouseAction::Move | MouseAction::Copy, Hit::Note { note, .. }) => {
                let grabbed = self.song(self.midi.note(note)?.start);
                let earliest = self.notes(&drag.notes).map(|n| self.song(n.start)).min()?;
                let by =
                    (self.snap(grabbed + moved, false) - grabbed).max(-earliest.max(Beats::ZERO));
                let (low, high) = self
                    .notes(&drag.notes)
                    .fold((Note::MAX_KEY, 0), |(low, high), n| {
                        (low.min(n.key), high.max(n.key))
                    });
                let keys =
                    i64::from(layout.key_at(drag.to.y)) - i64::from(layout.key_at(drag.from.y));
                Some(Plan::Move {
                    notes: drag.notes.clone(),
                    by,
                    keys: keys.clamp(-i64::from(low), i64::from(Note::MAX_KEY - high)),
                    copy: drag.action == MouseAction::Copy,
                })
            }
            (
                MouseAction::Trim | MouseAction::TrimFree,
                Hit::Note {
                    note,
                    part: Part::Edge(side),
                },
            ) => {
                let grabbed = self.midi.note(note)?;
                let start = self.song(grabbed.start);
                let edge = match side {
                    Side::Start => start,
                    Side::End => start + grabbed.length,
                };
                let at = self.snap(edge + moved, drag.action == MouseAction::TrimFree);
                (at != edge).then(|| Plan::Trim {
                    notes: drag.notes.clone(),
                    side,
                    by: at - edge,
                })
            }
            (MouseAction::Velocity, _) => {
                let travel = match drag.area {
                    Area::Lane => layout.lane.height().max(1.0),
                    _ => self.theme.piano_roll.velocity_drag,
                };
                let by = -(drag.to.y - drag.from.y) / travel;
                (by != 0.0 && !drag.notes.is_empty()).then(|| Plan::Velocity {
                    notes: drag.notes.clone(),
                    by,
                })
            }
            (MouseAction::SelectArea, _) => {
                let rect = Rect::from_two_pos(drag.from, drag.to);
                let notes = self.midi.notes.iter();
                let inside = notes.filter(|n| self.note_rect(n).intersects(rect));
                Some(Plan::Area {
                    notes: inside.map(|n| n.id).collect(),
                    rect,
                })
            }
            (MouseAction::DrawNotes, _) => Some(Plan::Draw {
                notes: drag.drawn.iter().map(|(&at, &key)| (at, key)).collect(),
            }),
            (MouseAction::DrawController | MouseAction::EraseController, Hit::Controller) => {
                let controller = drag.controller?;
                let (&start, _) = drag.steps.first_key_value()?;
                let (_, &(end, _)) = drag.steps.last_key_value()?;
                let range = BeatRange::new(start, end);
                Some(match drag.action {
                    MouseAction::DrawController => Plan::DrawController {
                        controller,
                        range,
                        points: drag.steps.iter().map(|(&at, &(_, v))| (at, v)).collect(),
                    },
                    _ => Plan::EraseController { controller, range },
                })
            }
            (MouseAction::ScrubZoom, _) => {
                let mut viewport = drag.viewport;
                let left = layout.rows.left();
                let anchor = viewport.beats_at(drag.from.x - left);
                let zoom = f64::from(((drag.to.y - drag.from.y) * SCRUB_ZOOM_SPEED).exp());
                viewport.zoom =
                    (viewport.zoom * zoom).clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM);
                let room = Beats::from_f64(f64::from(drag.to.x - left) / viewport.zoom);
                viewport.start = (anchor - room).max(Beats::ZERO);
                Some(Plan::Viewport(viewport))
            }
            _ => None,
        }
    }
}

/// Asks the app to run the command `id` with `args`.
fn run(actions: &mut Vec<Action>, id: CommandId, args: Args) {
    actions.push(Action::RunWith(id, args));
}

/// The arguments that name the notes `notes` of the clip the piano roll
/// shows.
fn of(scene: &Scene<'_>, notes: &[NoteId]) -> Args {
    Args::new()
        .with("notes", notes.to_vec())
        .with("clip", scene.clip.id)
}

/// Asks the app for `plan`.
fn send(actions: &mut Vec<Action>, scene: &Scene<'_>, plan: &Plan) {
    match plan {
        Plan::Move {
            notes: ids,
            by,
            keys,
            copy,
        } => {
            let args = of(scene, ids)
                .with("by", *by)
                .with("keys", *keys)
                .with("copy", *copy);
            run(actions, notes::MOVE, args);
        }
        Plan::Trim {
            notes: ids,
            side,
            by,
        } => {
            let edge = match side {
                Side::Start => "start_by",
                Side::End => "end_by",
            };
            run(actions, notes::TRIM, of(scene, ids).with(edge, *by));
        }
        Plan::Velocity { notes: ids, by } => {
            run(actions, notes::SET_VELOCITY, of(scene, ids).with("by", *by));
        }
        Plan::Area { notes: ids, .. } => run(actions, notes::SELECT, of(scene, ids)),
        Plan::Draw { notes: drawn } => {
            let starts: Vec<Beats> = drawn.iter().map(|&(at, _)| scene.own(at)).collect();
            let keys: Vec<i64> = drawn.iter().map(|&(_, key)| i64::from(key)).collect();
            let args = Args::new()
                .with("starts", starts)
                .with("keys", keys)
                .with("clip", scene.clip.id);
            run(actions, notes::DRAW, args);
        }
        Plan::DrawController {
            controller,
            range,
            points,
        } => {
            let at: Vec<Beats> = points.iter().map(|&(at, _)| scene.own(at)).collect();
            let values: Vec<f64> = points.iter().map(|&(_, v)| f64::from(v)).collect();
            let args = stretch(scene, *controller, *range)
                .with("at", at)
                .with("values", values);
            run(actions, controllers::DRAW, args);
        }
        Plan::EraseController { controller, range } => {
            run(
                actions,
                controllers::ERASE,
                stretch(scene, *controller, *range),
            );
        }
        Plan::Viewport(viewport) => {
            let roll = &scene.editor().piano_roll;
            set_viewport(actions, viewport, roll.key_height, roll.height);
        }
    }
}

/// The arguments that name the stretch `range` of song positions of
/// `controller`'s curve in the clip the piano roll shows.
fn stretch(scene: &Scene<'_>, controller: Controller, range: BeatRange) -> Args {
    Args::new()
        .with("start", scene.own(range.start))
        .with("end", scene.own(range.end))
        .with("controller", i64::from(controller.number()))
        .with("clip", scene.clip.id)
}

/// `value`, the share of the lane's height up from its foot, as drawing
/// sets `controller` to: one of MIDI's 128 steps from 0 to 1, or off or on
/// for a switch.
fn drawn_value(value: f32, controller: Controller) -> f32 {
    let value = value.clamp(0.0, 1.0);
    match controller.is_switch() {
        true if Controller::is_on(value) => 1.0,
        true => 0.0,
        false => (value * 127.0).round() / 127.0,
    }
}

fn set_viewport(actions: &mut Vec<Action>, viewport: &Viewport, key_height: f32, height: f32) {
    let args = Args::new()
        .with("start", viewport.start)
        .with("zoom", viewport.zoom)
        .with("top", viewport.top)
        .with("width", viewport.width)
        .with("height", height)
        .with("key_height", key_height);
    run(actions, views::SET_PIANO_ROLL_VIEWPORT, args);
}

impl PianoRoll {
    /// Shows the piano roll in the rest of `ui`, and adds what the user
    /// asked for to `actions`.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        theme: &Theme,
        mouse: &MouseMap,
        actions: &mut Vec<Action>,
    ) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let editor = view.editor;
        let roll = &editor.piano_roll;
        let shown = roll.clip.and_then(|id| {
            let (track, clip) = view.project.find_clip(id)?;
            let ClipContent::Midi(midi) = &clip.content else {
                return None;
            };
            Some((clip, midi, view.project.track_index(track.id)?))
        });
        let Some((clip, midi, index)) = shown else {
            self.drag = None;
            paint::empty(&ui.painter_at(rect), rect, theme);
            let response = ui.interact(rect, ui.scope_id().with("piano roll"), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Piano Roll"));
            return;
        };
        let layout = Layout::new(rect, theme, roll.viewport, roll.key_height);
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|&pos| rect.contains(pos) && ui.rect_contains_pointer(rect));
        let mut scene = Scene {
            view,
            theme,
            mouse,
            layout,
            clip,
            midi,
            index,
            pointer,
        };
        // The rows' size, for the commands that zoom to fit and scroll
        // notes into sight, and the scrolling, kept within the keys.
        let mut viewport = scene.layout.viewport;
        let mut key_height = roll.key_height;
        let id = ui.scope_id().with("piano roll");
        let lane = match scene.controller() {
            Some(_) => "Controller Lane",
            None => "Velocities",
        };
        let areas = [
            (scene.layout.rows, Area::Rows, "Piano Roll"),
            (scene.layout.keys, Area::Keys, "Keyboard"),
            (scene.layout.ruler, Area::Ruler, "Piano Roll Ruler"),
            (scene.layout.lane, Area::Lane, lane),
        ];
        let responses: Vec<(Response, Area)> = areas
            .into_iter()
            .map(|(rect, area, name)| {
                let response = ui.interact(rect, id.with(name), Sense::click_and_drag());
                response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, name));
                (response, area)
            })
            .collect();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.drag = None;
        }
        for (response, area) in &responses {
            self.respond(ui, &scene, response, *area, actions);
        }
        self.wheel(ui, &scene, &mut viewport, &mut key_height);
        self.follow(&scene, &mut viewport);
        let height = scene.layout.rows.height();
        if viewport != roll.viewport || key_height != roll.key_height || height != roll.height {
            set_viewport(actions, &viewport, key_height, height);
            scene.layout = Layout::new(rect, theme, viewport, key_height);
        }
        let plan = self.drag.as_ref().and_then(|drag| scene.plan(drag));
        self.cursor(ui, &scene);

        let painter = ui.painter_at(rect);
        let layout = &scene.layout;
        paint::rows(&painter.with_clip_rect(layout.rows), &scene, plan.as_ref());
        paint::keys(&painter.with_clip_rect(layout.keys), &scene);
        paint::ruler(&painter.with_clip_rect(layout.ruler), &scene);
        paint::lane(&painter.with_clip_rect(layout.lane), &scene, plan.as_ref());
        paint::corner(&painter.with_clip_rect(layout.corner), &scene);
        let chooser = ui.interact(layout.label, id.with("lane"), Sense::click());
        paint::label(&painter.with_clip_rect(layout.label), &scene, &chooser);
        menu::lanes(&chooser, &scene, actions);
        name_for_screen_readers(ui, &scene, id);

        for (response, area) in &responses {
            if response.secondary_clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let spot = Spot {
                    hit: scene.hit(*area, pos),
                    at: scene.snapped(pos.x),
                };
                self.menu = Some(spot);
                focus(&scene, actions);
                select_for_menu(&scene, spot, actions);
            }
            response.context_menu(|ui| {
                if let Some(spot) = self.menu {
                    menu::show(ui, &scene, spot, actions);
                }
            });
        }
    }

    /// Handles the clicks and drags on `area`, whose response is
    /// `response`.
    fn respond(
        &mut self,
        ui: &Ui,
        scene: &Scene<'_>,
        response: &Response,
        area: Area,
        actions: &mut Vec<Action>,
    ) {
        let mods = modifiers(ui.input(|i| i.modifiers));
        if response.drag_started()
            && let Some(from) = ui.input(|i| i.pointer.press_origin())
        {
            focus(scene, actions);
            self.drag_started(scene, area, from, mods, actions);
        }
        if let Some(drag) = self.drag.as_mut().filter(|drag| drag.area == area) {
            if let Some(pos) = response.interact_pointer_pos() {
                drag.to = pos;
            }
            // Holding Ctrl part of the way through a drag of notes copies
            // them.
            if matches!(drag.action, MouseAction::Move | MouseAction::Copy)
                && let Some(action @ (MouseAction::Move | MouseAction::Copy)) =
                    scene.mouse.action(Target::Note, Gesture::Drag, mods)
            {
                drag.action = action;
            }
            match drag.action {
                MouseAction::DrawNotes => draw_along(scene, drag),
                MouseAction::DrawController | MouseAction::EraseController => {
                    controller_along(scene, drag);
                }
                _ => {}
            }
            // The selection and the scrolling change as the drag goes.
            if matches!(
                drag.action,
                MouseAction::SelectArea | MouseAction::ScrubZoom
            ) {
                let plan = scene.plan(drag);
                if plan.as_ref().map(sent_as) != drag.sent.as_ref().map(sent_as)
                    && let Some(plan) = &plan
                {
                    send(actions, scene, plan);
                }
                drag.sent = plan;
            }
            if response.drag_stopped() {
                let drag = self.drag.take().expect("a drag is in progress");
                drag_stopped(scene, &drag, actions);
            }
        }
        if response.drag_stopped() {
            self.drag = self.drag.take().filter(|drag| drag.area != area);
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let hit = scene.hit(area, pos);
        if response.double_clicked() {
            focus(scene, actions);
            if clicked(scene, hit, pos, Gesture::DoubleClick, mods, actions) {
                return;
            }
        }
        if response.clicked() {
            focus(scene, actions);
            clicked(scene, hit, pos, Gesture::Click, mods, actions);
        }
    }

    fn drag_started(
        &mut self,
        scene: &Scene<'_>,
        area: Area,
        from: Pos2,
        mods: Modifiers,
        actions: &mut Vec<Action>,
    ) {
        let hit = scene.hit(area, from);
        let draw = scene.editor().piano_roll.draw;
        let Some(action) = scene.mouse.action(hit.target(draw), Gesture::Drag, mods) else {
            self.drag = None;
            return;
        };
        let mut drag = Drag {
            area,
            action,
            hit,
            from,
            to: from,
            notes: Vec::new(),
            viewport: scene.layout.viewport,
            drawn: BTreeMap::new(),
            controller: scene.controller(),
            steps: BTreeMap::new(),
            last_step: None,
            sent: None,
        };
        let acts_on_notes = matches!(
            action,
            MouseAction::Move
                | MouseAction::Copy
                | MouseAction::Trim
                | MouseAction::TrimFree
                | MouseAction::Velocity
        );
        if let Some(note) = hit.note().filter(|_| acts_on_notes) {
            // A note that isn't selected changes alone, and is selected.
            let selected = &scene.editor().piano_roll.notes;
            drag.notes = if selected.contains(&note) {
                selected.iter().copied().collect()
            } else {
                run(actions, notes::SELECT, of(scene, &[note]));
                vec![note]
            };
        }
        match action {
            MouseAction::DrawNotes => draw_along(scene, &mut drag),
            MouseAction::DrawController | MouseAction::EraseController => {
                controller_along(scene, &mut drag);
            }
            _ => {}
        }
        self.drag = Some(drag);
    }

    /// Scrolls and zooms with the wheel, over the piano roll.
    fn wheel(&mut self, ui: &Ui, scene: &Scene<'_>, viewport: &mut Viewport, key_height: &mut f32) {
        let layout = &scene.layout;
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let parts = [layout.rows, layout.keys, layout.ruler, layout.lane];
        let Some(&over) = parts
            .iter()
            .find(|rect| rect.contains(pos) && ui.rect_contains_pointer(**rect))
        else {
            return;
        };
        let target = if over == layout.keys {
            Target::PianoKey
        } else {
            Target::Timeline
        };
        let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
        let x = pos.x.clamp(layout.rows.left(), layout.rows.right());
        let at = layout.viewport.beats_at(x - layout.rows.left());
        let mut wheel = Vec::new();
        let before = *viewport;
        ui.input(|i| {
            for event in &i.events {
                match event {
                    Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } => wheel.push((*unit, *delta, *modifiers)),
                    // A trackpad's pinch.
                    Event::Zoom(factor) => viewport.zoom_around(f64::from(*factor), at),
                    _ => {}
                }
            }
        });
        for (unit, delta, mods) in wheel {
            let delta = match unit {
                MouseWheelUnit::Point => delta,
                MouseWheelUnit::Line => delta * line,
                MouseWheelUnit::Page => delta * layout.rows.height(),
            };
            match scene.mouse.action(target, Gesture::Wheel, modifiers(mods)) {
                Some(MouseAction::Scroll) => {
                    viewport.top -= delta.y;
                    viewport.start -= viewport.beats_in(delta.x);
                }
                Some(MouseAction::ScrollSideways) => {
                    viewport.start -= viewport.beats_in(delta.x + delta.y);
                }
                Some(MouseAction::Zoom) => {
                    let factor = f64::from(((delta.x + delta.y) * ZOOM_SPEED).exp());
                    viewport.zoom_around(factor, at);
                }
                Some(MouseAction::ZoomKeys) => {
                    // The key under the pointer stays there.
                    let y = pos.y - layout.rows.top();
                    let (min, max) = (
                        musicable_commands::editor::PianoRoll::MIN_KEY_HEIGHT,
                        musicable_commands::editor::PianoRoll::MAX_KEY_HEIGHT,
                    );
                    let factor = ((delta.x + delta.y) * ZOOM_SPEED).exp();
                    let height = (*key_height * factor).clamp(min, max);
                    viewport.top = (viewport.top + y) * height / *key_height - y;
                    *key_height = height;
                }
                _ => {}
            }
        }
        viewport.start = viewport.start.max(Beats::ZERO);
        let lowest = (KEYS * *key_height - layout.rows.height()).max(0.0);
        viewport.top = viewport.top.clamp(0.0, lowest);
        let sideways = viewport.start != before.start || viewport.zoom != before.zoom;
        if sideways && scene.view.playback.playing {
            self.follow_paused = true;
        }
    }

    /// Scrolls to keep the playhead in sight while it plays, a page at a
    /// time, if Follow is on, as the arrangement does.
    fn follow(&mut self, scene: &Scene<'_>, viewport: &mut Viewport) {
        let playback = scene.view.playback;
        if !playback.playing || !scene.editor().follow {
            self.follow_paused = false;
            return;
        }
        let in_sight = viewport.visible().contains(playback.playhead);
        if self.follow_paused {
            self.follow_paused = !in_sight;
        } else if self.drag.is_none() {
            viewport.follow(playback.playhead);
        }
    }

    /// Shows what a press there would do: the edges trim, notes move.
    fn cursor(&self, ui: &Ui, scene: &Scene<'_>) {
        let icon = |action: Option<MouseAction>| match action {
            Some(MouseAction::Trim | MouseAction::TrimFree) => Some(CursorIcon::ResizeHorizontal),
            Some(MouseAction::Velocity) => Some(CursorIcon::ResizeVertical),
            Some(MouseAction::Copy) => Some(CursorIcon::Copy),
            Some(MouseAction::Move) => Some(CursorIcon::Grab),
            Some(MouseAction::DrawNotes | MouseAction::DrawController) => {
                Some(CursorIcon::Crosshair)
            }
            _ => None,
        };
        let shown = match &self.drag {
            Some(drag) => match drag.action {
                MouseAction::Move => Some(CursorIcon::Grabbing),
                action => icon(Some(action)),
            },
            None => {
                let Some(pos) = scene.pointer else {
                    return;
                };
                let layout = &scene.layout;
                let areas = [
                    (layout.rows, Area::Rows),
                    (layout.keys, Area::Keys),
                    (layout.lane, Area::Lane),
                ];
                let Some((_, area)) = areas.into_iter().find(|(rect, _)| rect.contains(pos)) else {
                    return;
                };
                let hit = scene.hit(area, pos);
                let mods = modifiers(ui.input(|i| i.modifiers));
                let draw = scene.editor().piano_roll.draw;
                icon(scene.mouse.action(hit.target(draw), Gesture::Drag, mods))
            }
        };
        if let Some(icon) = shown {
            ui.ctx().set_cursor_icon(icon);
        }
    }
}

/// What's asked for as a drag goes, without what's only drawn.
fn sent_as(plan: &Plan) -> Plan {
    match plan {
        Plan::Area { notes, .. } => Plan::Area {
            notes: notes.clone(),
            rect: Rect::NOTHING,
        },
        plan => plan.clone(),
    }
}

/// Gives the piano roll the focus, so the keys and the edit commands act on
/// its notes, if it hasn't got it.
fn focus(scene: &Scene<'_>, actions: &mut Vec<Action>) {
    if scene.editor().focus != Focus::PianoRoll {
        let args = Args::new().with("view", "piano_roll");
        run(actions, views::SET_FOCUS, args);
    }
}

/// Adds a step of Draw Mode's drag where the pointer is now, on the key
/// under it, and the steps it passed over since the last, on the same key.
fn draw_along(scene: &Scene<'_>, drag: &mut Drag) {
    let layout = &scene.layout;
    let x = drag.to.x.clamp(layout.rows.left(), layout.rows.right());
    let at = scene.drawn_at(x);
    let key = layout.key_at(drag.to.y);
    let first = drag.drawn.keys().next().copied().unwrap_or(at);
    let last = drag.drawn.keys().next_back().copied().unwrap_or(at);
    // Steps it went past without a frame in them: forwards or backwards.
    let skipped = if at > last {
        BeatRange::new(last, at)
    } else {
        BeatRange::new(at, first)
    };
    let zoom = layout.viewport.zoom;
    if !skipped.is_empty() {
        let lines = scene
            .editor()
            .grid
            .lines(&scene.project().tempo, skipped, zoom);
        for line in lines.into_iter().filter(|&line| line > skipped.start) {
            drag.drawn.entry(line).or_insert(key);
        }
    }
    drag.drawn.insert(at, key);
}

/// Adds the step of the controller's curve under the pointer to `drag`,
/// with the value at the pointer's height, and the steps it passed over
/// since the last frame, with the values on the way there.
fn controller_along(scene: &Scene<'_>, drag: &mut Drag) {
    let Some(controller) = drag.controller else {
        return;
    };
    let step = scene.controller_step(scene.lane_at(drag.to.x));
    let value = scene.controller_value(drag.to.y, controller);
    if let Some((last, was)) = drag.last_step.filter(|&(last, _)| last != step.start) {
        let (from, to) = (last.min(step.start), last.max(step.start));
        let mut at = scene.controller_step(from).end;
        while at < to {
            let skipped = scene.controller_step(at);
            if skipped.end <= at {
                break;
            }
            let along = ((at - last).to_f64() / (step.start - last).to_f64()) as f32;
            let between = drawn_value(was + (value - was) * along, controller);
            drag.steps.insert(at, (skipped.end, between));
            at = skipped.end;
        }
    }
    drag.steps.insert(step.start, (step.end, value));
    drag.last_step = Some((step.start, value));
}

fn drag_stopped(scene: &Scene<'_>, drag: &Drag, actions: &mut Vec<Action>) {
    let Some(plan) = scene.plan(drag) else {
        return;
    };
    match &plan {
        // Moving nowhere does nothing.
        Plan::Move { by, keys, .. } if *by == Beats::ZERO && *keys == 0 => {}
        Plan::Draw { notes } if notes.is_empty() => {}
        // These were asked for as the drag went.
        Plan::Area { .. } | Plan::Viewport(_) => {
            if drag.sent.as_ref().map(sent_as) != Some(sent_as(&plan)) {
                send(actions, scene, &plan);
            }
        }
        _ => send(actions, scene, &plan),
    }
}

/// Carries out a click or a double click on `hit` at `pos`. Returns whether
/// the mouse map has something for it.
fn clicked(
    scene: &Scene<'_>,
    hit: Hit,
    pos: Pos2,
    gesture: Gesture,
    mods: Modifiers,
    actions: &mut Vec<Action>,
) -> bool {
    let draw = scene.editor().piano_roll.draw;
    let Some(action) = scene.mouse.action(hit.target(draw), gesture, mods) else {
        return false;
    };
    let selected = &scene.editor().piano_roll.notes;
    match (action, hit) {
        (MouseAction::Select | MouseAction::ToggleSelect, Hit::Note { note, .. }) => {
            let toggle = action == MouseAction::ToggleSelect;
            run(
                actions,
                notes::SELECT,
                of(scene, &[note]).with("toggle", toggle),
            );
        }
        (MouseAction::DeleteNote, Hit::Note { note, .. }) => {
            run(actions, notes::DELETE, of(scene, &[note]));
        }
        (MouseAction::PlaceMarker, Hit::Row(_)) => {
            run(actions, notes::SELECT, of(scene, &[]));
            let at = scene.snapped(pos.x);
            run(
                actions,
                transport::SET_START_MARKER,
                Args::new().with("at", at),
            );
        }
        (MouseAction::AddNote, Hit::Row(key)) => {
            let at = scene.own(scene.drawn_at(pos.x));
            let args = Args::new()
                .with("start", at)
                .with("key", i64::from(key))
                .with("clip", scene.clip.id);
            run(actions, notes::ADD, args);
        }
        (MouseAction::SelectKey, Hit::Key(key)) => {
            run(actions, notes::SELECT, of(scene, &scene.on_key(key)));
        }
        (MouseAction::AddKey, Hit::Key(key)) => {
            let mut added = scene.on_key(key);
            added.retain(|note| !selected.contains(note));
            let args = of(scene, &added).with("toggle", true);
            run(actions, notes::SELECT, args);
        }
        (MouseAction::Locate, Hit::Ruler) => {
            let at = scene.snapped(pos.x);
            run(actions, transport::LOCATE, Args::new().with("at", at));
        }
        // A step of the curve, as a drag that goes nowhere draws.
        (MouseAction::DrawController | MouseAction::EraseController, Hit::Controller) => {
            let Some(controller) = scene.controller() else {
                return false;
            };
            let range = scene.controller_step(scene.lane_at(pos.x));
            let plan = match action {
                MouseAction::DrawController => Plan::DrawController {
                    controller,
                    range,
                    points: vec![(range.start, scene.controller_value(pos.y, controller))],
                },
                _ => Plan::EraseController { controller, range },
            };
            send(actions, scene, &plan);
        }
        _ => return false,
    }
    true
}

/// Selects the note a context menu opens on, as a click would, unless it's
/// selected already, so the menu's commands act on it.
fn select_for_menu(scene: &Scene<'_>, spot: Spot, actions: &mut Vec<Action>) {
    if let Some(note) = spot.hit.note()
        && !scene.selected(note)
    {
        run(actions, notes::SELECT, of(scene, &[note]));
    }
}

/// Names what's in sight for screen readers, and for the tests, which find
/// notes by these names: "Note C3 at 1. 1. 1".
fn name_for_screen_readers(ui: &Ui, scene: &Scene<'_>, id: Id) {
    let layout = &scene.layout;
    let middle_c = scene.editor().behaviour.middle_c;
    let tempo = &scene.project().tempo;
    let keys = layout.visible_keys();
    for note in &scene.midi.notes {
        let rect = scene.note_rect(note).intersect(layout.rows);
        if !keys.contains(&note.key) || !rect.is_positive() {
            continue;
        }
        let response = ui.interact(rect, id.with(("note", note.id)), Sense::hover());
        let selected = scene.selected(note.id);
        let name = format!(
            "Note {} at {}",
            middle_c.name(note.key),
            position(tempo, scene.song(note.start))
        );
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &name));
    }
}

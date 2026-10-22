//! The arrangement: the song's tracks over time, with their clips, under a
//! ruler, and each track's header on the right, as in Live.
//!
//! It's drawn with egui's painter, which egui-wgpu renders with wgpu, and
//! only what's in sight is drawn, so it stays smooth with a hundred tracks
//! of clips: `docs/app.md` has the numbers. Everything it does goes through
//! commands. Gestures are looked up in the [`MouseMap`], and a drag that
//! changes the project shows where things will go while it lasts and runs
//! one command when it ends, so it's one step in the undo history and the
//! engine rebuilds its graph once. Context menus list commands from the
//! registry, with their arguments taken from what was clicked.

mod menu;
mod paint;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use egui::{
    Color32, CursorIcon, Event, Id, Key, MouseWheelUnit, Pos2, Rect, Response, Role, Sense, Ui,
    WidgetInfo, pos2, vec2,
};
use musicable_commands::editor::Viewport;
use musicable_commands::mixer::Change;
use musicable_commands::{
    Args, CommandId, Editor, Modifiers, arrange, edit, locator, notes, take as takes, transport,
    view as views,
};
use musicable_model::clip::{Clip, ClipContent, ClipId};
use musicable_model::time::Beats;
use musicable_model::{LocatorId, Project, TakeId, TrackId, TrackKind};

use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::widgets::{Face, paint_toggle};

/// How much a point of the wheel, or of a drag up or down the ruler,
/// zooms: the zoom changes by `e` to the power of this many points.
const ZOOM_SPEED: f32 = 0.005;
const SCRUB_ZOOM_SPEED: f32 = 0.01;

/// The arrangement's own state: the gesture in progress, and the menu open.
#[derive(Debug, Default)]
pub(crate) struct Arrangement {
    drag: Option<Drag>,
    /// What the open context menu was opened on.
    menu: Option<Spot>,
    /// Set when the user scrolls away from the playhead while it plays, so
    /// Follow doesn't scroll back until the playhead is in sight again.
    follow_paused: bool,
}

/// The parts of the arrangement that take the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Lanes,
    Ruler,
    Headers,
}

/// What's under the mouse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Clip {
        track: TrackId,
        clip: ClipId,
        part: Part,
    },
    Lane(TrackId),
    /// Under the last track.
    Empty,
    Ruler,
    Strip,
    LoopBrace,
    LoopEdge(Side),
    Locator(LocatorId),
    Header(TrackId),
    TrackEdge(TrackId),
    /// A take's lane, on one of the take's clips or between them.
    Take {
        track: TrackId,
        take: TakeId,
        clip: Option<ClipId>,
    },
    /// The header beside a take's lane.
    TakeHeader {
        track: TrackId,
        take: TakeId,
    },
}

impl Hit {
    fn target(self) -> Target {
        match self {
            Hit::Clip { part, .. } => match part {
                Part::Title => Target::ClipTitle,
                Part::Body => Target::ClipBody,
                Part::Edge(_) => Target::ClipEdge,
                Part::Fade(_) => Target::FadeHandle,
            },
            Hit::Lane(_) => Target::Lane,
            Hit::Empty => Target::Empty,
            Hit::Ruler => Target::Ruler,
            Hit::Strip => Target::Strip,
            Hit::LoopBrace => Target::LoopBrace,
            Hit::LoopEdge(_) => Target::LoopEdge,
            Hit::Locator(_) => Target::Locator,
            Hit::Header(_) => Target::TrackHeader,
            Hit::TrackEdge(_) => Target::TrackEdge,
            Hit::Take { .. } => Target::TakeLane,
            Hit::TakeHeader { .. } => Target::TakeHeader,
        }
    }

    /// The track it's on, if it's on one.
    fn track(self) -> Option<TrackId> {
        match self {
            Hit::Clip { track, .. }
            | Hit::Lane(track)
            | Hit::Header(track)
            | Hit::TrackEdge(track)
            | Hit::Take { track, .. }
            | Hit::TakeHeader { track, .. } => Some(track),
            _ => None,
        }
    }
}

/// A part of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Title,
    Body,
    Edge(Side),
    /// The handle of the fade at that side.
    Fade(Side),
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
    /// The clips a drag of clips moves.
    clips: Vec<ClipId>,
    /// The viewport as the drag started.
    viewport: Viewport,
    /// The height of the track whose edge is dragged, as the drag started.
    height: f32,
    /// What the drag last asked for, for those that ask while they last.
    sent: Option<Plan>,
}

/// What a drag would do if it ended now.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    /// Moves the clips by some beats and some tracks down, or copies them.
    Move {
        clips: Vec<ClipId>,
        by: Beats,
        tracks: i64,
        copy: bool,
    },
    Trim {
        clip: ClipId,
        side: Side,
        at: Beats,
    },
    Fade {
        clip: ClipId,
        side: Side,
        length: Beats,
    },
    Loop {
        start: Beats,
        end: Beats,
        looping: bool,
    },
    Locator {
        locator: LocatorId,
        at: Beats,
    },
    /// A time selection. Drags ask for it as they go.
    Time {
        start: Beats,
        end: Beats,
        tracks: Vec<TrackId>,
    },
    /// Scrolls and zooms. Drags ask for it as they go.
    Viewport(Viewport),
    /// A track's height. Drags ask for it as they go.
    Height {
        track: TrackId,
        height: f32,
    },
    /// Copies a stretch of a take to its track.
    Comp {
        track: TrackId,
        take: TakeId,
        start: Beats,
        end: Beats,
    },
}

/// Where a context menu was opened, and what on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    hit: Hit,
    /// The song position clicked, snapped to the grid.
    at: Beats,
}

/// A track's row: where its lane and its header are.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Row {
    /// Its place in the project's tracks.
    index: usize,
    id: TrackId,
    top: f32,
    height: f32,
}

impl Row {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// A take's lane, under its track's.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TakeRow {
    /// Its track's place in the project's tracks, and its place in the
    /// track's takes.
    track_index: usize,
    index: usize,
    track: TrackId,
    take: TakeId,
    top: f32,
    height: f32,
}

impl TakeRow {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// Where the arrangement's parts are this frame.
#[derive(Debug, Clone)]
struct Layout {
    /// The ruler over the lanes, and the strip along its top.
    ruler: Rect,
    strip: Rect,
    lanes: Rect,
    headers: Rect,
    /// Over the headers, beside the ruler.
    corner: Rect,
    /// The zoom and scrolling, with the timeline's width this frame.
    viewport: Viewport,
    /// Every track's row, top to bottom.
    rows: Vec<Row>,
    /// The lanes of the takes of the tracks that show them, top to bottom.
    takes: Vec<TakeRow>,
    /// How tall the tracks and their take lanes are together.
    content: f32,
}

impl Layout {
    fn new(
        rect: Rect,
        theme: &Theme,
        project: &Project,
        editor: &Editor,
        viewport: Viewport,
    ) -> Self {
        let split = (rect.right() - theme.header_width).max(rect.left());
        let ruler_bottom = (rect.top() + theme.ruler_height).min(rect.bottom());
        let ruler = Rect::from_min_max(rect.left_top(), pos2(split, ruler_bottom));
        let strip = Rect::from_min_max(
            ruler.left_top(),
            pos2(split, (ruler.top() + theme.ruler_strip).min(ruler.bottom())),
        );
        let lanes = Rect::from_min_max(pos2(rect.left(), ruler_bottom), pos2(split, rect.bottom()));
        let headers = Rect::from_min_max(pos2(split, ruler_bottom), rect.right_bottom());
        let corner = Rect::from_min_max(pos2(split, rect.top()), pos2(rect.right(), ruler_bottom));
        let mut rows = Vec::with_capacity(project.tracks.len());
        let mut takes = Vec::new();
        let mut y = 0.0;
        for (index, track) in project.tracks.iter().enumerate() {
            let height = editor
                .track_heights
                .get(&track.id)
                .copied()
                .unwrap_or(theme.track_height);
            rows.push(Row {
                index,
                id: track.id,
                top: y,
                height,
            });
            y += height;
            if !editor.take_lanes.contains(&track.id) {
                continue;
            }
            for (take_index, take) in track.takes.iter().enumerate() {
                takes.push(TakeRow {
                    track_index: index,
                    index: take_index,
                    track: track.id,
                    take: take.id,
                    top: y,
                    height: theme.take_height,
                });
                y += theme.take_height;
            }
        }
        let viewport = Viewport {
            width: lanes.width(),
            top: viewport.top.clamp(0.0, lowest_top(y, theme, lanes)),
            ..viewport
        };
        for row in &mut rows {
            row.top += lanes.top() - viewport.top;
        }
        for take in &mut takes {
            take.top += lanes.top() - viewport.top;
        }
        Self {
            ruler,
            strip,
            lanes,
            headers,
            corner,
            viewport,
            rows,
            takes,
            content: y,
        }
    }

    /// Where the last track, or its last take lane, ends.
    fn bottom(&self) -> f32 {
        self.lanes.top() - self.viewport.top + self.content
    }

    fn x_of(&self, at: Beats) -> f32 {
        self.lanes.left() + self.viewport.x_of(at)
    }

    fn beats_at(&self, x: f32) -> Beats {
        self.viewport.beats_at(x - self.lanes.left())
    }

    /// The row at `y`, if a track is there.
    fn row_at(&self, y: f32) -> Option<&Row> {
        let i = self.rows.partition_point(|row| row.bottom() <= y);
        self.rows.get(i).filter(|row| row.top <= y)
    }

    /// The index of the track at `y`, or whose take lanes are there, or of
    /// the nearest one above or below the tracks. `None` without tracks.
    fn nearest_index(&self, y: f32) -> Option<usize> {
        let below = self.rows.partition_point(|row| row.top <= y);
        Some(below.saturating_sub(1).min(self.rows.len().checked_sub(1)?))
    }

    /// The take lane at `y`, if there's one there.
    fn take_at(&self, y: f32) -> Option<&TakeRow> {
        let i = self.takes.partition_point(|take| take.bottom() <= y);
        self.takes.get(i).filter(|take| take.top <= y)
    }

    /// The take lanes in sight, top to bottom.
    fn visible_takes(&self) -> impl Iterator<Item = &TakeRow> {
        let first = self
            .takes
            .partition_point(|take| take.bottom() <= self.lanes.top());
        self.takes[first..]
            .iter()
            .take_while(|take| take.top < self.lanes.bottom())
    }

    fn row(&self, id: TrackId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// The rows in sight, top to bottom.
    fn visible_rows(&self) -> impl Iterator<Item = &Row> {
        let first = self
            .rows
            .partition_point(|row| row.bottom() <= self.lanes.top());
        self.rows[first..]
            .iter()
            .take_while(|row| row.top < self.lanes.bottom())
    }

    /// Where `clip` is drawn on `row`, and its title bar and body.
    fn clip(&self, theme: &Theme, row: &Row, clip: &Clip) -> ClipRect {
        let rect = Rect::from_x_y_ranges(
            self.x_of(clip.start)..=self.x_of(clip.end()),
            row.top + 1.0..=row.bottom() - 1.0,
        );
        if rect.height() >= 2.0 * theme.clip_title {
            let (title, body) = rect.split_top_bottom_at_y(rect.top() + theme.clip_title);
            ClipRect {
                rect,
                title,
                body: Some(body),
            }
        } else {
            ClipRect {
                rect,
                title: rect,
                body: None,
            }
        }
    }

    /// The handle of the fade at `side` of the audio clip drawn at `at`.
    fn fade_handle(&self, theme: &Theme, at: &ClipRect, clip: &Clip, side: Side) -> Option<Rect> {
        let (ClipContent::Audio(audio), Some(body)) = (&clip.content, at.body) else {
            return None;
        };
        let size = theme.fade_handle;
        // Clear of the edges, where a drag trims.
        let room = theme.edge_grab;
        let rect = match side {
            Side::Start => {
                let x = self
                    .x_of(clip.start + audio.fade_in)
                    .max(at.rect.left() + room);
                Rect::from_min_size(pos2(x, body.top()), vec2(size, size))
            }
            Side::End => {
                let x = self
                    .x_of(clip.end() - audio.fade_out)
                    .min(at.rect.right() - room);
                Rect::from_min_size(pos2(x - size, body.top()), vec2(size, size))
            }
        };
        // Clips too narrow for both handles have none.
        (at.rect.width() > 2.0 * (room + size)).then_some(rect)
    }
}

/// Where a clip is drawn.
#[derive(Debug, Clone, Copy)]
struct ClipRect {
    rect: Rect,
    /// Its title bar, along its top. A clip too short for a body is all
    /// title bar.
    title: Rect,
    body: Option<Rect>,
}

/// What the arrangement shows this frame, and where.
struct Scene<'s> {
    view: &'s View<'s>,
    theme: &'s Theme,
    mouse: &'s MouseMap,
    layout: Layout,
    /// Where the pointer is, if it's over the arrangement and nothing's in
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

    /// `at`, snapped to the grid unless `free` or snapping is off.
    fn snap(&self, at: Beats, free: bool) -> Beats {
        if free {
            return at;
        }
        let editor = self.editor();
        editor
            .grid
            .snap(&self.project().tempo, at, self.layout.viewport.zoom)
    }

    /// The song position at `x`, snapped to the grid.
    fn snapped(&self, x: f32) -> Beats {
        self.snap(self.layout.beats_at(x), false).max(Beats::ZERO)
    }

    /// What's under `pos` in `area`.
    fn hit(&self, area: Area, pos: Pos2) -> Hit {
        match area {
            Area::Lanes => self.hit_lanes(pos),
            Area::Ruler => self.hit_ruler(pos),
            Area::Headers => match self.layout.row_at(pos.y) {
                Some(row) if pos.y >= row.bottom() - self.theme.height_grab => {
                    Hit::TrackEdge(row.id)
                }
                Some(row) => Hit::Header(row.id),
                None => match self.layout.take_at(pos.y) {
                    Some(lane) => Hit::TakeHeader {
                        track: lane.track,
                        take: lane.take,
                    },
                    None => Hit::Empty,
                },
            },
        }
    }

    fn hit_lanes(&self, pos: Pos2) -> Hit {
        if let Some(lane) = self.layout.take_at(pos.y) {
            let take = &self.project().tracks[lane.track_index].takes[lane.index];
            // The last drawn is on top.
            let clip = take.clips.iter().rev().find(|clip| {
                let x = self.layout.x_of(clip.start)..=self.layout.x_of(clip.end());
                x.contains(&pos.x)
            });
            return Hit::Take {
                track: lane.track,
                take: lane.take,
                clip: clip.map(|clip| clip.id),
            };
        }
        let Some(row) = self.layout.row_at(pos.y) else {
            return Hit::Empty;
        };
        let track = &self.project().tracks[row.index];
        // The last drawn is on top.
        for clip in track.clips.iter().rev() {
            let at = self.layout.clip(self.theme, row, clip);
            let grab = self.theme.edge_grab.min(at.rect.width() / 3.0);
            if pos.x < at.rect.left() - 1.0 || pos.x > at.rect.right() + 1.0 {
                continue;
            }
            let part = |part| Hit::Clip {
                track: track.id,
                clip: clip.id,
                part,
            };
            for side in [Side::Start, Side::End] {
                let handle = self.layout.fade_handle(self.theme, &at, clip, side);
                if handle.is_some_and(|handle| handle.expand(1.0).contains(pos)) {
                    return part(Part::Fade(side));
                }
            }
            if pos.x <= at.rect.left() + grab {
                return part(Part::Edge(Side::Start));
            }
            if pos.x >= at.rect.right() - grab {
                return part(Part::Edge(Side::End));
            }
            return match at.body {
                Some(body) if body.contains(pos) => part(Part::Body),
                _ => part(Part::Title),
            };
        }
        Hit::Lane(track.id)
    }

    fn hit_ruler(&self, pos: Pos2) -> Hit {
        let layout = &self.layout;
        if !layout.strip.contains(pos) {
            return Hit::Ruler;
        }
        let project = self.project();
        // The locators hang from the strip's top, over the loop brace.
        if pos.y < layout.strip.center().y {
            let marker = self.theme.marker;
            let near = |l: &&musicable_model::Locator| (layout.x_of(l.at) - pos.x).abs() <= marker;
            if let Some(locator) = project.locators.iter().rev().find(near) {
                return Hit::Locator(locator.id);
            }
        }
        let region = project.loop_region;
        let (start, end) = (layout.x_of(region.start), layout.x_of(region.end));
        let grab = self.theme.edge_grab.min((end - start) / 3.0);
        if (pos.x - start).abs() <= grab {
            Hit::LoopEdge(Side::Start)
        } else if (pos.x - end).abs() <= grab {
            Hit::LoopEdge(Side::End)
        } else if pos.x > start && pos.x < end {
            Hit::LoopBrace
        } else {
            Hit::Strip
        }
    }

    /// What `drag` would do if it ended now.
    fn plan(&self, drag: &Drag) -> Option<Plan> {
        let project = self.project();
        let layout = &self.layout;
        let moved = layout.viewport.beats_in(drag.to.x - drag.from.x);
        match (drag.action, drag.hit) {
            (
                MouseAction::Move | MouseAction::Copy | MouseAction::MoveFree,
                Hit::Clip { clip, .. },
            ) => {
                let (index, grabbed) = find(project, clip)?;
                let free = drag.action == MouseAction::MoveFree;
                let moving: Vec<(usize, &Clip)> = drag
                    .clips
                    .iter()
                    .filter_map(|&c| find(project, c))
                    .collect();
                let earliest = moving.iter().map(|(_, c)| c.start).min()?;
                let by = (self.snap(grabbed.start + moved, free) - grabbed.start).max(-earliest);
                let to = layout.nearest_index(drag.to.y)?;
                let mut tracks = to as i64 - index as i64;
                // As the command does: all of them move to other tracks, or
                // none.
                let lands = moving.iter().all(|&(i, c)| {
                    usize::try_from(i as i64 + tracks)
                        .ok()
                        .and_then(|i| project.tracks.get(i))
                        .is_some_and(|track| track.holds(&c.content))
                });
                if !lands {
                    tracks = 0;
                }
                Some(Plan::Move {
                    clips: drag.clips.clone(),
                    by,
                    tracks,
                    copy: drag.action == MouseAction::Copy,
                })
            }
            (
                MouseAction::Trim | MouseAction::TrimFree,
                Hit::Clip {
                    clip,
                    part: Part::Edge(side),
                    ..
                },
            ) => {
                let (_, c) = find(project, clip)?;
                let edge = match side {
                    Side::Start => c.start,
                    Side::End => c.end(),
                };
                let at = self.snap(edge + moved, drag.action == MouseAction::TrimFree);
                // A clip is never narrower than the room to grab its edges.
                let least = layout
                    .viewport
                    .beats_in(self.theme.edge_grab)
                    .max(Beats::from_ticks(1));
                let at = match side {
                    Side::Start => at.min(c.end() - least).max(Beats::ZERO),
                    Side::End => at.max(c.start + least),
                };
                (at != edge).then_some(Plan::Trim { clip, side, at })
            }
            (
                MouseAction::Fade,
                Hit::Clip {
                    clip,
                    part: Part::Fade(side),
                    ..
                },
            ) => {
                let (_, c) = find(project, clip)?;
                let ClipContent::Audio(audio) = &c.content else {
                    return None;
                };
                let (length, room) = match side {
                    Side::Start => (audio.fade_in + moved, c.length - audio.fade_out),
                    Side::End => (audio.fade_out - moved, c.length - audio.fade_in),
                };
                Some(Plan::Fade {
                    clip,
                    side,
                    length: length.clamp(Beats::ZERO, room.max(Beats::ZERO)),
                })
            }
            (MouseAction::SelectTime, hit) => {
                let tracks = &project.tracks;
                let from = match hit.track() {
                    Some(track) => project.track_index(track)?,
                    None => layout.nearest_index(drag.from.y)?,
                };
                let to = layout.nearest_index(drag.to.y)?;
                let (start, end) = (self.snapped(drag.from.x), self.snapped(drag.to.x));
                Some(Plan::Time {
                    start: start.min(end),
                    end: start.max(end),
                    tracks: tracks[from.min(to)..=from.max(to)]
                        .iter()
                        .map(|t| t.id)
                        .collect(),
                })
            }
            (MouseAction::ScrubZoom, _) => {
                let mut viewport = drag.viewport;
                let left = layout.lanes.left();
                let anchor = viewport.beats_at(drag.from.x - left);
                let zoom = f64::from(((drag.to.y - drag.from.y) * SCRUB_ZOOM_SPEED).exp());
                viewport.zoom =
                    (viewport.zoom * zoom).clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM);
                let room = Beats::from_f64(f64::from(drag.to.x - left) / viewport.zoom);
                viewport.start = (anchor - room).max(Beats::ZERO);
                Some(Plan::Viewport(viewport))
            }
            (MouseAction::DrawLoop, _) => {
                let (start, end) = (self.snapped(drag.from.x), self.snapped(drag.to.x));
                (start != end).then_some(Plan::Loop {
                    start: start.min(end),
                    end: start.max(end),
                    looping: true,
                })
            }
            (MouseAction::MoveLoop, _) => {
                let region = project.loop_region;
                let start = self.snap(region.start + moved, false).max(Beats::ZERO);
                (start != region.start).then_some(Plan::Loop {
                    start,
                    end: start + region.len(),
                    looping: project.looping,
                })
            }
            (MouseAction::ResizeLoop, Hit::LoopEdge(side)) => {
                let region = project.loop_region;
                let (start, end) = match side {
                    Side::Start => (self.snap(region.start + moved, false), region.end),
                    Side::End => (region.start, self.snap(region.end + moved, false)),
                };
                let (start, end) = (
                    start.min(end).max(Beats::ZERO),
                    start.max(end).max(Beats::ZERO),
                );
                (start != end && (start, end) != (region.start, region.end)).then_some(Plan::Loop {
                    start,
                    end,
                    looping: project.looping,
                })
            }
            (MouseAction::MoveLocator, Hit::Locator(id)) => {
                let locator = project.locator(id)?;
                let at = self.snap(locator.at + moved, false).max(Beats::ZERO);
                (at != locator.at).then_some(Plan::Locator { locator: id, at })
            }
            (MouseAction::ResizeTrack, Hit::TrackEdge(track)) => Some(Plan::Height {
                track,
                height: (drag.height + drag.to.y - drag.from.y)
                    .clamp(views::MIN_TRACK_HEIGHT, views::MAX_TRACK_HEIGHT),
            }),
            (MouseAction::Comp | MouseAction::CompFree, Hit::Take { track, take, .. }) => {
                let free = drag.action == MouseAction::CompFree;
                let at = |x| self.snap(layout.beats_at(x), free).max(Beats::ZERO);
                let (start, end) = (at(drag.from.x), at(drag.to.x));
                (start != end).then_some(Plan::Comp {
                    track,
                    take,
                    start: start.min(end),
                    end: start.max(end),
                })
            }
            _ => None,
        }
    }
}

/// The index of the track with the clip with id `id`, and the clip.
fn find(project: &Project, id: ClipId) -> Option<(usize, &Clip)> {
    let (track, clip) = project.find_clip(id)?;
    Some((project.track_index(track.id)?, clip))
}

/// Asks the app to run the command `id` with `args`.
fn run(actions: &mut Vec<Action>, id: CommandId, args: Args) {
    actions.push(Action::RunWith(id, args));
}

/// Asks the app for `plan`.
fn send(actions: &mut Vec<Action>, plan: &Plan) {
    match plan {
        Plan::Move {
            clips,
            by,
            tracks,
            copy,
        } => {
            let args = Args::new()
                .with("clips", clips.clone())
                .with("by", *by)
                .with("tracks", *tracks)
                .with("copy", *copy);
            run(actions, arrange::MOVE, args);
        }
        Plan::Trim { clip, side, at } => {
            let edge = match side {
                Side::Start => "start",
                Side::End => "end",
            };
            run(
                actions,
                arrange::TRIM,
                Args::new().with("clip", *clip).with(edge, *at),
            );
        }
        Plan::Fade { clip, side, length } => {
            let fade = match side {
                Side::Start => "fade_in",
                Side::End => "fade_out",
            };
            let args = Args::new().with("clip", *clip).with(fade, *length);
            run(actions, arrange::SET_FADES, args);
        }
        Plan::Loop {
            start,
            end,
            looping,
        } => {
            let args = Args::new()
                .with("start", *start)
                .with("end", *end)
                .with("looping", *looping);
            run(actions, transport::SET_LOOP, args);
        }
        Plan::Locator { locator, at } => {
            let args = Args::new().with("locator", *locator).with("at", *at);
            run(actions, locator::MOVE, args);
        }
        Plan::Time { start, end, tracks } => {
            let args = Args::new()
                .with("start", *start)
                .with("end", *end)
                .with("tracks", tracks.clone());
            run(actions, edit::SELECT_TIME, args);
        }
        Plan::Viewport(viewport) => set_viewport(actions, viewport),
        Plan::Height { track, height } => {
            let args = Args::new().with("track", *track).with("height", *height);
            run(actions, views::SET_TRACK_HEIGHT, args);
        }
        Plan::Comp {
            take, start, end, ..
        } => {
            let args = Args::new()
                .with("take", *take)
                .with("start", *start)
                .with("end", *end);
            run(actions, takes::COMP, args);
        }
    }
}

fn set_viewport(actions: &mut Vec<Action>, viewport: &Viewport) {
    let args = Args::new()
        .with("start", viewport.start)
        .with("zoom", viewport.zoom)
        .with("top", viewport.top)
        .with("width", viewport.width);
    run(actions, views::SET_VIEWPORT, args);
}

impl Arrangement {
    /// Shows the arrangement in the rest of `ui`, and adds what the user
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
        let layout = Layout::new(rect, theme, view.project, editor, editor.viewport);
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|&pos| rect.contains(pos) && ui.rect_contains_pointer(rect));
        let mut scene = Scene {
            view,
            theme,
            mouse,
            layout,
            pointer,
        };
        // The timeline's width, for the commands that zoom to fit, and the
        // scrolling, kept within the tracks.
        let mut viewport = scene.layout.viewport;
        let id = ui.scope_id().with("arrangement");
        let lanes = ui.interact(
            scene.layout.lanes,
            id.with("lanes"),
            Sense::click_and_drag(),
        );
        lanes.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Arrangement"));
        let ruler = ui.interact(
            scene.layout.ruler,
            id.with("ruler"),
            Sense::click_and_drag(),
        );
        ruler.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Ruler"));
        let headers = ui.interact(
            scene.layout.headers,
            id.with("headers"),
            Sense::click_and_drag(),
        );
        // Over the headers, so they take their clicks.
        let buttons = self.header_buttons(ui, &scene, actions);
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.drag = None;
        }
        for (response, area) in [
            (&lanes, Area::Lanes),
            (&ruler, Area::Ruler),
            (&headers, Area::Headers),
        ] {
            self.respond(ui, &scene, response, area, actions);
        }
        self.wheel(ui, &scene, &mut viewport);
        self.follow(&scene, &mut viewport);
        if viewport != editor.viewport {
            set_viewport(actions, &viewport);
            scene.layout = Layout::new(rect, theme, view.project, editor, viewport);
        }
        let plan = self.drag.as_ref().and_then(|drag| scene.plan(drag));
        self.cursor(ui, &scene, plan.as_ref());

        let painter = ui.painter_at(rect);
        paint::lanes(
            &painter.with_clip_rect(scene.layout.lanes),
            &scene,
            plan.as_ref(),
        );
        paint::ruler(
            &painter.with_clip_rect(scene.layout.ruler),
            &scene,
            plan.as_ref(),
        );
        paint::headers(&painter.with_clip_rect(scene.layout.headers), &scene);
        for button in &buttons {
            let (face, on, colour) = (button.face, button.on, button.colour);
            paint_toggle(ui, button.rect, &button.response, face, on, colour, theme);
        }
        paint::corner(&painter.with_clip_rect(scene.layout.corner), &scene);
        if ui.input(|i| !i.raw.hovered_files.is_empty()) {
            paint::drop_hint(&painter, &scene);
        }
        name_for_screen_readers(ui, &scene, id);

        for (response, area) in [
            (&lanes, Area::Lanes),
            (&ruler, Area::Ruler),
            (&headers, Area::Headers),
        ] {
            if response.secondary_clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let hit = scene.hit(area, pos);
                let at = match hit {
                    Hit::Locator(id) => scene.project().locator(id).map(|l| l.at),
                    _ => None,
                };
                let spot = Spot {
                    hit,
                    at: at.unwrap_or_else(|| scene.snapped(pos.x)),
                };
                self.menu = Some(spot);
                select_for_menu(&scene, spot, actions);
            }
            response.context_menu(|ui| {
                if let Some(spot) = self.menu {
                    menu::show(ui, &scene, spot, actions);
                }
            });
        }
        self.drop_files(ui, &scene, actions);
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
            self.drag_started(scene, area, from, mods, actions);
        }
        if let Some(drag) = self.drag.as_mut().filter(|drag| drag.area == area) {
            if let Some(pos) = response.interact_pointer_pos() {
                drag.to = pos;
            }
            // Holding Ctrl or Alt part of the way through a drag of clips
            // copies them or stops snapping.
            if let Hit::Clip {
                part: Part::Title, ..
            } = drag.hit
                && matches!(
                    drag.action,
                    MouseAction::Move | MouseAction::Copy | MouseAction::MoveFree
                )
                && let Some(
                    action @ (MouseAction::Move | MouseAction::Copy | MouseAction::MoveFree),
                ) = scene.mouse.action(Target::ClipTitle, Gesture::Drag, mods)
            {
                drag.action = action;
            }
            // Selections, scrolling and heights change as the drag goes.
            if matches!(
                drag.action,
                MouseAction::SelectTime | MouseAction::ScrubZoom | MouseAction::ResizeTrack
            ) {
                let plan = scene.plan(drag);
                if plan != drag.sent
                    && let Some(plan) = &plan
                {
                    send(actions, plan);
                }
                drag.sent = plan;
            }
            if response.drag_stopped() {
                let drag = self.drag.take().expect("a drag is in progress");
                self.drag_stopped(scene, &drag, actions);
            }
        }
        if response.drag_stopped() {
            self.drag = self.drag.take().filter(|drag| drag.area != area);
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let hit = scene.hit(area, pos);
        if response.double_clicked()
            && self.clicked(scene, hit, pos, Gesture::DoubleClick, mods, actions)
        {
            return;
        }
        if response.clicked() {
            self.clicked(scene, hit, pos, Gesture::Click, mods, actions);
        }
    }

    /// Carries out a click or a double click on `hit` at `pos`. Returns
    /// whether the mouse map has something for it.
    fn clicked(
        &mut self,
        scene: &Scene<'_>,
        hit: Hit,
        pos: Pos2,
        gesture: Gesture,
        mods: Modifiers,
        actions: &mut Vec<Action>,
    ) -> bool {
        let Some(action) = scene.mouse.action(hit.target(), gesture, mods) else {
            return false;
        };
        let at = scene.snapped(pos.x);
        let project = scene.project();
        let marker = |actions: &mut Vec<Action>| {
            run(
                actions,
                transport::SET_START_MARKER,
                Args::new().with("at", at),
            );
        };
        let select_time = |actions: &mut Vec<Action>, tracks: Vec<TrackId>| {
            let args = Args::new()
                .with("start", at)
                .with("end", at)
                .with("tracks", tracks);
            run(actions, edit::SELECT_TIME, args);
        };
        match (action, hit) {
            (MouseAction::Select | MouseAction::ToggleSelect, Hit::Clip { clip, .. }) => {
                let toggle = action == MouseAction::ToggleSelect;
                let args = Args::new().with("clips", vec![clip]).with("toggle", toggle);
                run(actions, edit::SELECT_CLIPS, args);
            }
            (MouseAction::SelectAndMark, Hit::Clip { clip, .. }) => {
                run(
                    actions,
                    edit::SELECT_CLIPS,
                    Args::new().with("clips", vec![clip]),
                );
                marker(actions);
            }
            (MouseAction::Select | MouseAction::ToggleSelect, Hit::Header(track)) => {
                let toggle = action == MouseAction::ToggleSelect;
                let args = Args::new()
                    .with("tracks", vec![track])
                    .with("toggle", toggle);
                run(actions, edit::SELECT_TRACKS, args);
            }
            (MouseAction::PlaceMarker, Hit::Lane(track)) => {
                select_time(actions, vec![track]);
                marker(actions);
            }
            (MouseAction::PlaceMarker, Hit::Empty) => {
                select_time(actions, Vec::new());
                marker(actions);
            }
            (MouseAction::Locate, Hit::Locator(id)) => {
                let at = project.locator(id).map_or(at, |locator| locator.at);
                run(actions, transport::LOCATE, Args::new().with("at", at));
            }
            (MouseAction::Locate, _) => {
                run(actions, transport::LOCATE, Args::new().with("at", at));
            }
            (MouseAction::ToggleLoop, _) => {
                let region = project.loop_region;
                let plan = Plan::Loop {
                    start: region.start,
                    end: region.end,
                    looping: !project.looping,
                };
                send(actions, &plan);
            }
            (MouseAction::Rename, Hit::Clip { clip, .. }) => {
                run(actions, edit::RENAME, Args::new().with("clip", clip));
            }
            // MIDI clips open in the piano roll.
            (MouseAction::Open, Hit::Clip { clip, .. }) => {
                let midi = find(project, clip)
                    .is_some_and(|(_, c)| matches!(c.content, ClipContent::Midi(_)));
                if !midi {
                    return false;
                }
                run(actions, notes::OPEN, Args::new().with("clip", clip));
            }
            (MouseAction::Rename, Hit::Header(track)) => {
                run(actions, edit::RENAME, Args::new().with("track", track));
            }
            (MouseAction::Rename, Hit::Locator(id)) => {
                run(actions, edit::RENAME, Args::new().with("locator", id));
            }
            (MouseAction::Rename, Hit::TakeHeader { take, .. }) => {
                run(actions, edit::RENAME, Args::new().with("take", take));
            }
            // A click takes the whole clip.
            (
                MouseAction::Comp | MouseAction::CompFree,
                Hit::Take {
                    track,
                    take,
                    clip: Some(clip),
                },
            ) => {
                let Some((_, t)) = project.find_take(take) else {
                    return false;
                };
                let Some(clip) = t.clips.iter().find(|c| c.id == clip) else {
                    return false;
                };
                let plan = Plan::Comp {
                    track,
                    take,
                    start: clip.start,
                    end: clip.end(),
                };
                send(actions, &plan);
            }
            _ => return false,
        }
        true
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
        let Some(action) = scene.mouse.action(hit.target(), Gesture::Drag, mods) else {
            self.drag = None;
            return;
        };
        let mut drag = Drag {
            area,
            action,
            hit,
            from,
            to: from,
            clips: Vec::new(),
            viewport: scene.layout.viewport,
            height: 0.0,
            sent: None,
        };
        match (action, hit) {
            (
                MouseAction::Move | MouseAction::Copy | MouseAction::MoveFree,
                Hit::Clip { clip, .. },
            ) => {
                // A clip that isn't selected moves alone, and is selected.
                let selected = &scene.editor().selection.clips;
                drag.clips = if selected.contains(&clip) {
                    selected.iter().copied().collect()
                } else {
                    run(
                        actions,
                        edit::SELECT_CLIPS,
                        Args::new().with("clips", vec![clip]),
                    );
                    vec![clip]
                };
            }
            (MouseAction::ResizeTrack, Hit::TrackEdge(track)) => {
                drag.height = scene.layout.row(track).map_or(0.0, |row| row.height);
            }
            _ => {}
        }
        self.drag = Some(drag);
    }

    fn drag_stopped(&mut self, scene: &Scene<'_>, drag: &Drag, actions: &mut Vec<Action>) {
        let Some(plan) = scene.plan(drag) else {
            return;
        };
        match &plan {
            // Moving nowhere does nothing.
            Plan::Move { by, tracks, .. } if *by == Beats::ZERO && *tracks == 0 => {}
            // The start marker goes to the start of a time selection, where
            // playing starts.
            Plan::Time { start, .. } => {
                send(actions, &plan);
                run(
                    actions,
                    transport::SET_START_MARKER,
                    Args::new().with("at", *start),
                );
            }
            // These were asked for as the drag went.
            Plan::Viewport(_) | Plan::Height { .. } => {
                if drag.sent.as_ref() != Some(&plan) {
                    send(actions, &plan);
                }
            }
            _ => send(actions, &plan),
        }
    }

    /// Mute, solo and arm in each header in sight, as buttons. Returns
    /// them, to be drawn over the headers.
    fn header_buttons(
        &mut self,
        ui: &mut Ui,
        scene: &Scene<'_>,
        actions: &mut Vec<Action>,
    ) -> Vec<Button> {
        let project = scene.project();
        let theme = scene.theme;
        let mods = modifiers(ui.input(|i| i.modifiers));
        let mut buttons = Vec::new();
        for row in scene.layout.visible_rows() {
            let track = &project.tracks[row.index];
            for (button, rect) in paint::header_buttons(scene, row, track) {
                let name = &track.name;
                let (face, on, colour, label) = match button {
                    HeaderButton::Mute => (
                        Face::Text("M"),
                        track.muted,
                        theme.mute,
                        format!("Mute track {name}"),
                    ),
                    HeaderButton::Solo => (
                        Face::Text("S"),
                        track.solo,
                        theme.solo,
                        format!("Solo track {name}"),
                    ),
                    HeaderButton::Arm => (
                        Face::Dot,
                        track.armed,
                        theme.arm,
                        format!("Arm track {name}"),
                    ),
                };
                let id = Id::unique(("header button", track.id, button));
                let response = ui.interact(rect, id, Sense::click());
                response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, &label));
                let clicked = response.clicked();
                buttons.push(Button {
                    rect,
                    response,
                    face,
                    on,
                    colour,
                });
                if !clicked {
                    continue;
                }
                let change = match button {
                    HeaderButton::Mute => Change::Muted(track.id, !on),
                    HeaderButton::Arm => Change::Armed(track.id, !on),
                    HeaderButton::Solo => {
                        let action = scene.mouse.action(Target::SoloButton, Gesture::Click, mods);
                        Change::Solo {
                            track: track.id,
                            solo: !on,
                            exclusive: !on && action != Some(MouseAction::AddSolo),
                        }
                    }
                };
                actions.push(Action::Mixer(change));
            }
        }
        buttons
    }

    /// Scrolls and zooms with the wheel, over the tracks and the ruler.
    fn wheel(&mut self, ui: &Ui, scene: &Scene<'_>, viewport: &mut Viewport) {
        let layout = &scene.layout;
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let over = [layout.lanes, layout.ruler, layout.headers]
            .into_iter()
            .any(|rect| ui.rect_contains_pointer(rect));
        if !over || !rect_union(layout).contains(pos) {
            return;
        }
        let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
        let x = pos.x.clamp(layout.lanes.left(), layout.lanes.right());
        let at = layout.viewport.beats_at(x - layout.lanes.left());
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
                MouseWheelUnit::Page => delta * layout.lanes.height(),
            };
            let target = Target::Timeline;
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
                _ => {}
            }
        }
        viewport.start = viewport.start.max(Beats::ZERO);
        let lowest = lowest_top(layout.content, scene.theme, layout.lanes);
        viewport.top = viewport.top.clamp(0.0, lowest);
        let sideways = viewport.start != before.start || viewport.zoom != before.zoom;
        if sideways && scene.view.playback.playing {
            self.follow_paused = true;
        }
    }

    /// Scrolls to keep the playhead in sight while it plays, a page at a
    /// time, if Follow is on.
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

    /// Shows what a press there would do: the edges trim, title bars move.
    fn cursor(&self, ui: &Ui, scene: &Scene<'_>, plan: Option<&Plan>) {
        let icon = |hit: Hit, action: Option<MouseAction>| match (hit, action) {
            (_, Some(MouseAction::Trim | MouseAction::TrimFree | MouseAction::Fade)) => {
                Some(CursorIcon::ResizeHorizontal)
            }
            (_, Some(MouseAction::ResizeLoop)) => Some(CursorIcon::ResizeHorizontal),
            (_, Some(MouseAction::ResizeTrack)) => Some(CursorIcon::ResizeVertical),
            (_, Some(MouseAction::Copy)) => Some(CursorIcon::Copy),
            (_, Some(MouseAction::Move | MouseAction::MoveFree | MouseAction::MoveLocator)) => {
                Some(CursorIcon::Grab)
            }
            (_, Some(MouseAction::MoveLoop)) => Some(CursorIcon::Grab),
            _ => None,
        };
        let shown = match &self.drag {
            Some(drag) => match (drag.action, plan) {
                (MouseAction::Move | MouseAction::MoveFree, _) => Some(CursorIcon::Grabbing),
                (MouseAction::Copy, _) => Some(CursorIcon::Copy),
                (action, _) => icon(drag.hit, Some(action)),
            },
            None => {
                let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
                    return;
                };
                let layout = &scene.layout;
                let area = [
                    (layout.lanes, Area::Lanes),
                    (layout.ruler, Area::Ruler),
                    (layout.headers, Area::Headers),
                ]
                .into_iter()
                .find(|(rect, _)| rect.contains(pos) && ui.rect_contains_pointer(*rect));
                let Some((_, area)) = area else {
                    return;
                };
                let hit = scene.hit(area, pos);
                let mods = modifiers(ui.input(|i| i.modifiers));
                icon(hit, scene.mouse.action(hit.target(), Gesture::Drag, mods))
            }
        };
        if let Some(icon) = shown {
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Asks the app to insert audio files dropped on the window: where
    /// they're dropped, if that's over a track.
    fn drop_files(&self, ui: &Ui, scene: &Scene<'_>, actions: &mut Vec<Action>) {
        let files: Vec<PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if files.is_empty() {
            return;
        }
        let pos = ui.input(|i| i.pointer.latest_pos());
        let (track, at) = match pos {
            Some(pos) if scene.layout.lanes.contains(pos) => {
                let row = scene.layout.row_at(pos.y);
                let audio = row
                    .map(|row| &scene.project().tracks[row.index])
                    .filter(|track| track.kind == TrackKind::Audio);
                (audio.map(|track| track.id), Some(scene.snapped(pos.x)))
            }
            _ => (None, None),
        };
        actions.push(Action::DropFiles { files, track, at });
    }
}

/// The buttons in a track's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HeaderButton {
    Mute,
    Solo,
    Arm,
}

/// A header's button, to be drawn once the headers are.
struct Button {
    rect: Rect,
    response: Response,
    face: Face,
    on: bool,
    colour: Color32,
}

/// How far down the tracks scroll, when they and their take lanes are
/// `content` tall: until a track's height is left under the last.
fn lowest_top(content: f32, theme: &Theme, lanes: Rect) -> f32 {
    (content + theme.track_height - lanes.height()).max(0.0)
}

/// The ruler, the lanes and the headers together.
fn rect_union(layout: &Layout) -> Rect {
    layout.ruler.union(layout.lanes).union(layout.headers)
}

/// Selects what a context menu opens on, as a click would, unless it's
/// selected already, so the menu's commands act on it.
fn select_for_menu(scene: &Scene<'_>, spot: Spot, actions: &mut Vec<Action>) {
    let selection = &scene.editor().selection;
    match spot.hit {
        Hit::Clip { clip, .. } if !selection.clips.contains(&clip) => {
            run(
                actions,
                edit::SELECT_CLIPS,
                Args::new().with("clips", vec![clip]),
            );
        }
        Hit::Header(track) | Hit::TrackEdge(track) if !selection.tracks.contains(&track) => {
            run(
                actions,
                edit::SELECT_TRACKS,
                Args::new().with("tracks", vec![track]),
            );
        }
        Hit::Lane(track) if selection.time.is_none() => {
            let args = Args::new()
                .with("start", spot.at)
                .with("end", spot.at)
                .with("tracks", vec![track]);
            run(actions, edit::SELECT_TIME, args);
            run(
                actions,
                transport::SET_START_MARKER,
                Args::new().with("at", spot.at),
            );
        }
        _ => {}
    }
}

/// Names what's in sight for screen readers, and for the tests, which find
/// clips, tracks and locators by these names.
fn name_for_screen_readers(ui: &Ui, scene: &Scene<'_>, id: Id) {
    let layout = &scene.layout;
    let project = scene.project();
    let selection = &scene.editor().selection;
    let visible = layout.viewport.visible();
    for row in layout.visible_rows() {
        let track = &project.tracks[row.index];
        let header = Rect::from_x_y_ranges(layout.headers.x_range(), row.top..=row.bottom());
        let response = ui.interact(
            header.intersect(layout.headers),
            id.with(("track", track.id)),
            Sense::hover(),
        );
        let selected = selection.tracks.contains(&track.id);
        response.widget_info(|| {
            WidgetInfo::selected(Role::Row, true, selected, format!("Track {}", track.name))
        });
        for clip in &track.clips {
            if clip.end() <= visible.start || clip.start >= visible.end {
                continue;
            }
            let rect = layout
                .clip(scene.theme, row, clip)
                .rect
                .intersect(layout.lanes);
            let response = ui.interact(rect, id.with(("clip", clip.id)), Sense::hover());
            let selected = selection.clips.contains(&clip.id);
            response.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, selected, format!("Clip {}", clip.name))
            });
        }
    }
    for lane in layout.visible_takes() {
        let track = &project.tracks[lane.track_index];
        let take = &track.takes[lane.index];
        let header = Rect::from_x_y_ranges(layout.headers.x_range(), lane.top..=lane.bottom());
        let response = ui.interact(
            header.intersect(layout.headers),
            id.with(("take", take.id)),
            Sense::hover(),
        );
        response.widget_info(|| {
            WidgetInfo::labeled(Role::Row, true, format!("{} of {}", take.name, track.name))
        });
    }
    for locator in &project.locators {
        if !visible.contains(locator.at) {
            continue;
        }
        let x = layout.x_of(locator.at);
        let marker = scene.theme.marker;
        let rect = Rect::from_x_y_ranges(x - marker..=x + marker, layout.strip.y_range());
        let response = ui.interact(rect, id.with(("locator", locator.id)), Sense::hover());
        response.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, format!("Locator {}", locator.name))
        });
    }
}

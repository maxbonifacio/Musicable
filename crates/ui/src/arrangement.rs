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
//!
//! The master track's row is pinned along the bottom, under the tracks,
//! and is selected, resized and has time selected on it as they do, for
//! the devices panel to show its devices. As it's pinned, its top edge is
//! the one a drag moves.
//!
//! A track can show automation lanes under its lanes, and the master's go
//! under its row. Their points are added, moved and deleted as Live's
//! breakpoints are, and a drag draws over them a step of the grid at a
//! time, as Live's Draw Mode does. Each edit sets the lane's points with
//! one command.

mod header;
mod menu;
mod paint;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{
    Color32, CursorIcon, Event, Id, Key, MouseWheelUnit, Popup, Pos2, Rangef, Rect, Response,
    RichText, Role, ScrollArea, Sense, SetOpenCommand, Ui, WidgetInfo, pos2, vec2,
};
use musicable_commands::editor::Viewport;
use musicable_commands::{
    Args, CommandId, Editor, Modifiers, arrange, automation, edit, locator, notes, param_text,
    take as takes, transport, view as views, warp,
};
use musicable_model::automation::Point;
use musicable_model::clip::{Clip, ClipContent, ClipId};
use musicable_model::param::{ParamAddress, ParamSpec, Scale};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::Warp;
use musicable_model::{Chain, DeviceKind, LocatorId, Project, TakeId, TrackId, TrackKind};

use crate::browser::{Destination, Dragged};
use crate::devices::describe;
use crate::mixer::{decibels, pan_text};
use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::parts::{Parts, TRACK_HEADER};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::widgets::{Face, paint_toggle};

pub(crate) use header::{HeaderPart, register};

/// How much a point of the wheel, or of a drag up or down the ruler,
/// zooms: the zoom changes by `e` to the power of this many points.
const ZOOM_SPEED: f32 = 0.005;
/// The most of the lanes' height the master's row and its automation lanes
/// take, as long as that leaves the tracks a track's height. When they need
/// more, they scroll on their own.
const MASTER_SHARE: f32 = 0.5;
const SCRUB_ZOOM_SPEED: f32 = 0.01;
/// The most points an automation lane names for screen readers, of those
/// in sight. A lane drawn freely can have thousands.
const MAX_NAMED_POINTS: usize = 64;

/// The arrangement's own state: the gesture in progress, and the menu open.
#[derive(Debug, Default)]
pub(crate) struct Arrangement {
    drag: Option<Drag>,
    /// What the drag that ended this frame asked for. The frame draws it as
    /// the drag left it, since the app only carries it out once the frame
    /// is drawn.
    landed: Option<Plan>,
    /// What the open context menu was opened on.
    menu: Option<Spot>,
    /// Set when the user scrolls away from the playhead while it plays, so
    /// Follow doesn't scroll back until the playhead is in sight again.
    follow_paused: bool,
    /// How far the master's row and lanes are scrolled up, when they don't
    /// all fit.
    master_scroll: f32,
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
    /// Under the last track, over the master's.
    Empty,
    /// The master track's lane, under the tracks.
    Master,
    /// The master track's header.
    MasterHeader,
    /// The master track's top edge, in its header: it's pinned along the
    /// bottom, so the edge over it is the one that moves.
    MasterEdge,
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
    /// An automation lane: on one of its points, near the line through
    /// them, or neither.
    Automation {
        chain: Chain,
        param: ParamAddress,
        part: Envelope,
    },
    /// The header beside an automation lane.
    AutomationHeader {
        chain: Chain,
        param: ParamAddress,
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
            Hit::Lane(_) | Hit::Master => Target::Lane,
            Hit::Empty => Target::Empty,
            Hit::MasterHeader => Target::TrackHeader,
            Hit::Ruler => Target::Ruler,
            Hit::Strip => Target::Strip,
            Hit::LoopBrace => Target::LoopBrace,
            Hit::LoopEdge(_) => Target::LoopEdge,
            Hit::Locator(_) => Target::Locator,
            Hit::Header(_) => Target::TrackHeader,
            Hit::TrackEdge(_) | Hit::MasterEdge => Target::TrackEdge,
            Hit::Take { .. } => Target::TakeLane,
            Hit::TakeHeader { .. } => Target::TakeHeader,
            Hit::Automation { part, .. } => match part {
                Envelope::Point(_) => Target::AutomationPoint,
                Envelope::Line { .. } => Target::AutomationLine,
                Envelope::Lane => Target::AutomationLane,
            },
            Hit::AutomationHeader { .. } => Target::AutomationHeader,
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
            | Hit::TakeHeader { track, .. }
            | Hit::Automation {
                chain: Chain::Track(track),
                ..
            }
            | Hit::AutomationHeader {
                chain: Chain::Track(track),
                ..
            } => Some(track),
            _ => None,
        }
    }

    /// Whether it's on the master track: its row or its automation lanes.
    fn on_master(self) -> bool {
        matches!(
            self,
            Hit::Master
                | Hit::MasterHeader
                | Hit::MasterEdge
                | Hit::Automation {
                    chain: Chain::Master,
                    ..
                }
                | Hit::AutomationHeader {
                    chain: Chain::Master,
                    ..
                }
        )
    }

    /// The automation lane it's on, if it's on one.
    fn lane(self) -> Option<ParamAddress> {
        match self {
            Hit::Automation { param, .. } | Hit::AutomationHeader { param, .. } => Some(param),
            _ => None,
        }
    }
}

/// A part of an automation lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Envelope {
    /// The point at this place in the lane's points.
    Point(usize),
    /// Near the line through the points at these places, next to each
    /// other: or the same point twice, for the level line before the first
    /// point or after the last. A lane without points has a level line at
    /// its parameter's own value, if that's known, with no points to move.
    Line { from: usize, to: usize },
    /// Neither.
    Lane,
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
    /// Whether it keeps off the grid now, as the modifiers held say, and
    /// whether it did as it started, which where it started keeps.
    free: bool,
    started_free: bool,
    /// Where it started, and where the pointer is now.
    from: Pos2,
    to: Pos2,
    /// The clips a drag of clips moves.
    clips: Vec<ClipId>,
    /// The viewport as the drag started.
    viewport: Viewport,
    /// The height of the track, or of the master's row, whose edge is
    /// dragged, as the drag started.
    height: f32,
    /// What the drag last asked for, for those that ask while they last.
    sent: Option<Plan>,
    /// What a drag that draws automation drew so far.
    drawn: Drawn,
}

/// What a drag drew on an automation lane: a value for each step of the
/// grid it crossed, by where the step starts, or drawn freely, off the
/// grid, a value for each place the pointer was.
#[derive(Debug, Clone, Default)]
struct Drawn {
    /// The values, and whether each holds for a step of the grid: a drag
    /// goes off the grid and back as Alt is pressed and let go.
    values: BTreeMap<Beats, (f64, bool)>,
    /// Where the pointer was when it last drew.
    last: Option<Pos2>,
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
    /// Stretches a warped clip, so its edge goes to `at`, at the tempo
    /// that gives it.
    Stretch {
        clip: ClipId,
        side: Side,
        at: Beats,
        bpm: f64,
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
    /// A time selection, on the tracks and maybe the master. Drags ask for
    /// it as they go.
    Time {
        start: Beats,
        end: Beats,
        tracks: Vec<TrackId>,
        master: bool,
    },
    /// Scrolls and zooms. Drags ask for it as they go.
    Viewport(Viewport),
    /// A track's height, or the master's row's. Drags ask for it as they
    /// go.
    Height {
        chain: Chain,
        height: f32,
    },
    /// Copies a stretch of a take to its track.
    Comp {
        track: TrackId,
        take: TakeId,
        start: Beats,
        end: Beats,
    },
    /// Sets the points of a parameter's automation.
    Points {
        param: ParamAddress,
        points: Vec<Point>,
    },
}

/// Where a context menu was opened, and what on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    hit: Hit,
    /// The song position clicked, snapped to the grid.
    at: Beats,
    /// The part of the track's header clicked, if it was one.
    part: Option<&'static str>,
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

/// The master track's row, under the tracks.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MasterRow {
    top: f32,
    height: f32,
}

impl MasterRow {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// An automation lane, under its track's lanes, or under the master's row
/// for the master's.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AutoRow {
    /// Its track's place in the project's tracks: `None` for the master's.
    track_index: Option<usize>,
    chain: Chain,
    param: ParamAddress,
    top: f32,
    height: f32,
}

impl AutoRow {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }

    /// The y coordinates its line goes between, from the parameter's least
    /// value to its greatest: clear of the lane's edges, so the points at
    /// either end show whole.
    fn travel(&self, theme: &Theme) -> (f32, f32) {
        let margin = (theme.point + 2.0).min(self.height / 2.0);
        (self.bottom() - margin, self.top + margin)
    }

    /// The y coordinate `unit` of the way up the travel.
    fn y_of(&self, theme: &Theme, unit: f64) -> f32 {
        let (bottom, top) = self.travel(theme);
        bottom + (top - bottom) * unit as f32
    }

    /// How far up the travel `y` is, from 0.0 to 1.0.
    fn unit_at(&self, theme: &Theme, y: f32) -> f64 {
        let (bottom, top) = self.travel(theme);
        f64::from((bottom - y) / (bottom - top).max(1.0)).clamp(0.0, 1.0)
    }

    /// How much of the travel `dy` points up is.
    fn units_in(&self, theme: &Theme, dy: f32) -> f64 {
        let (bottom, top) = self.travel(theme);
        f64::from(dy / (bottom - top).max(1.0))
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
    /// The tracks' automation lanes shown, top to bottom.
    automation: Vec<AutoRow>,
    /// How tall the tracks and their take and automation lanes are
    /// together.
    content: f32,
    /// Where the master's part of the lanes and the headers starts: it's
    /// pinned along their bottom, and the tracks scroll over it.
    master_top: f32,
    /// The master's row, and its automation lanes under it.
    master: MasterRow,
    master_automation: Vec<AutoRow>,
    /// How far the master's part is scrolled, and how far it can be.
    master_scroll: f32,
    master_overflow: f32,
}

impl Layout {
    fn new(
        rect: Rect,
        theme: &Theme,
        project: &Project,
        editor: &Editor,
        viewport: Viewport,
        master_scroll: f32,
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
        let mut automation = Vec::new();
        let mut y = 0.0;
        let lanes_of =
            |chain: Chain, track_index: Option<usize>, y: &mut f32, to: &mut Vec<AutoRow>| {
                for &param in editor.automation.get(&chain).into_iter().flatten() {
                    to.push(AutoRow {
                        track_index,
                        chain,
                        param,
                        top: *y,
                        height: theme.automation_height,
                    });
                    *y += theme.automation_height;
                }
            };
        let height_of = |chain: Chain| {
            let height = editor.track_heights.get(&chain).copied();
            height.unwrap_or(theme.track_height)
        };
        for (index, track) in project.tracks.iter().enumerate() {
            let height = height_of(Chain::Track(track.id));
            rows.push(Row {
                index,
                id: track.id,
                top: y,
                height,
            });
            y += height;
            if editor.take_lanes.contains(&track.id) {
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
            lanes_of(Chain::Track(track.id), Some(index), &mut y, &mut automation);
        }
        // The master's row and lanes, along the bottom, as much of them as
        // fits in their share. In a short arrangement the tracks come first.
        let mut master_automation = Vec::new();
        let row_height = height_of(Chain::Master);
        let mut master_content = row_height;
        lanes_of(
            Chain::Master,
            None,
            &mut master_content,
            &mut master_automation,
        );
        let master_height = master_content.min(master_room(lanes, theme));
        let master_top = lanes.bottom() - master_height;
        let master_overflow = master_content - master_height;
        let master_scroll = master_scroll.clamp(0.0, master_overflow);
        let master = MasterRow {
            top: master_top - master_scroll,
            height: row_height,
        };
        for lane in &mut master_automation {
            lane.top += master.top;
        }
        let tracks = Rect::from_x_y_ranges(lanes.x_range(), lanes.top()..=master_top);
        let viewport = Viewport {
            width: lanes.width(),
            top: viewport.top.clamp(0.0, lowest_top(y, theme, tracks)),
            ..viewport
        };
        for row in &mut rows {
            row.top += lanes.top() - viewport.top;
        }
        for take in &mut takes {
            take.top += lanes.top() - viewport.top;
        }
        for lane in &mut automation {
            lane.top += lanes.top() - viewport.top;
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
            automation,
            content: y,
            master_top,
            master,
            master_automation,
            master_scroll,
            master_overflow,
        }
    }

    /// Where the last track, or the last lane under it, ends.
    fn bottom(&self) -> f32 {
        self.lanes.top() - self.viewport.top + self.content
    }

    /// The tracks' part of the lanes, or of the headers, across `x`: over
    /// the master's.
    fn tracks_part(&self, x: Rangef) -> Rect {
        Rect::from_x_y_ranges(x, self.lanes.top()..=self.master_top)
    }

    /// The master's part of the lanes, or of the headers, across `x`.
    fn master_part(&self, x: Rangef) -> Rect {
        Rect::from_x_y_ranges(x, self.master_top..=self.lanes.bottom())
    }

    /// The part the lanes of `chain` are in, across `x`: the master's or
    /// the tracks'.
    fn part_of(&self, chain: Chain, x: Rangef) -> Rect {
        match chain {
            Chain::Master => self.master_part(x),
            Chain::Track(_) => self.tracks_part(x),
        }
    }

    /// Whether `y` is in the master's part.
    fn in_master(&self, y: f32) -> bool {
        y >= self.master_top
    }

    /// Whether the master's part is in sight: in a short arrangement, the
    /// tracks take all of it.
    fn master_shown(&self) -> bool {
        self.master_top < self.lanes.bottom()
    }

    fn x_of(&self, at: Beats) -> f32 {
        self.lanes.left() + self.viewport.x_of(at)
    }

    fn beats_at(&self, x: f32) -> Beats {
        self.viewport.beats_at(x - self.lanes.left())
    }

    /// The row at `y`, if a track is there.
    fn row_at(&self, y: f32) -> Option<&Row> {
        if self.in_master(y) {
            return None;
        }
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
        if self.in_master(y) {
            return None;
        }
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
            .take_while(|take| take.top < self.master_top)
    }

    /// The automation lane at `y`, if there's one there: a track's, or the
    /// master's in the master's part.
    fn automation_at(&self, y: f32) -> Option<&AutoRow> {
        let lanes = match self.in_master(y) {
            true => &self.master_automation,
            false => &self.automation,
        };
        let i = lanes.partition_point(|lane| lane.bottom() <= y);
        lanes.get(i).filter(|lane| lane.top <= y)
    }

    /// The automation lanes in sight: the tracks', top to bottom, then the
    /// master's.
    fn visible_automation(&self) -> impl Iterator<Item = &AutoRow> {
        let first = self
            .automation
            .partition_point(|lane| lane.bottom() <= self.lanes.top());
        let tracks = self.automation[first..]
            .iter()
            .take_while(|lane| lane.top < self.master_top);
        let master = self
            .master_automation
            .iter()
            .filter(|lane| lane.bottom() > self.master_top && lane.top < self.lanes.bottom());
        tracks.chain(master)
    }

    /// The automation lane of `param`, if it's shown.
    fn automation_lane(&self, param: ParamAddress) -> Option<&AutoRow> {
        (self.automation.iter())
            .chain(&self.master_automation)
            .find(|lane| lane.param == param)
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
            .take_while(|row| row.top < self.master_top)
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
    /// The parts of the tracks' headers, as the user has them.
    parts: &'s Parts,
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

    /// The song positions where `drag` started and where the pointer is
    /// now, each snapped to the grid unless the drag kept off it then.
    fn ends(&self, drag: &Drag) -> (Beats, Beats) {
        let at = |x, free| self.snap(self.layout.beats_at(x), free).max(Beats::ZERO);
        (at(drag.from.x, drag.started_free), at(drag.to.x, drag.free))
    }

    /// Where something dragged from the browser or the devices panel goes
    /// if it's let go at `pos`: on the track there, from the time there over
    /// the lanes, on a new track under the tracks, or on the master.
    fn destination(&self, pos: Pos2) -> Option<Destination> {
        let layout = &self.layout;
        let in_lanes = layout.lanes.contains(pos);
        if !in_lanes && !layout.headers.contains(pos) {
            return None;
        }
        if layout.in_master(pos.y) {
            return Some(Destination::Chain(Some(Chain::Master)));
        }
        let at = in_lanes.then(|| self.snapped(pos.x));
        let row = (layout.row_at(pos.y).map(|row| (row.index, row.id))).or_else(|| {
            layout
                .take_at(pos.y)
                .map(|take| (take.track_index, take.track))
        });
        Some(match row {
            Some((index, track)) => Destination::Track {
                track,
                audio: self.project().tracks[index].kind == TrackKind::Audio,
                at,
            },
            None => Destination::NewTrack { at },
        })
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
                None if self.layout.in_master(pos.y)
                    && pos.y < self.layout.master.top + self.theme.height_grab =>
                {
                    Hit::MasterEdge
                }
                None => match self.layout.take_at(pos.y) {
                    Some(lane) => Hit::TakeHeader {
                        track: lane.track,
                        take: lane.take,
                    },
                    None => match self.layout.automation_at(pos.y) {
                        Some(lane) => Hit::AutomationHeader {
                            chain: lane.chain,
                            param: lane.param,
                        },
                        None if self.layout.in_master(pos.y) => Hit::MasterHeader,
                        None => Hit::Empty,
                    },
                },
            },
        }
    }

    fn hit_lanes(&self, pos: Pos2) -> Hit {
        if self.layout.in_master(pos.y) {
            return match self.layout.automation_at(pos.y) {
                Some(lane) => Hit::Automation {
                    chain: lane.chain,
                    param: lane.param,
                    part: self.envelope_part(lane, pos),
                },
                None => Hit::Master,
            };
        }
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
        if let Some(lane) = self.layout.automation_at(pos.y) {
            return Hit::Automation {
                chain: lane.chain,
                param: lane.param,
                part: self.envelope_part(lane, pos),
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

    /// What's under `pos` on the automation lane `lane`: one of its points,
    /// the line through them, or neither.
    fn envelope_part(&self, lane: &AutoRow, pos: Pos2) -> Envelope {
        let layout = &self.layout;
        let grab = self.theme.point_grab;
        let spec = self.spec(lane.param);
        let y = |value: f64| lane.y_of(self.theme, spec.scale.to_unit(value, spec.min, spec.max));
        let project = self.project();
        let Some(automated) = project.lane(lane.param) else {
            let own = project.param_value(lane.param);
            return match own.is_some_and(|own| (y(own) - pos.y).abs() <= grab) {
                true => Envelope::Line { from: 0, to: 0 },
                false => Envelope::Lane,
            };
        };
        let points = &automated.points;
        let at = |point: &Point| pos2(layout.x_of(point.at), y(point.value));
        // The points within reach across, and the nearest of them that's
        // within reach.
        let first = points.partition_point(|p| layout.x_of(p.at) < pos.x - grab);
        let last = points.partition_point(|p| layout.x_of(p.at) <= pos.x + grab);
        let nearest = (first..last)
            .map(|i| (at(&points[i]).distance(pos), i))
            .filter(|&(distance, _)| distance <= grab)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, i)) = nearest {
            return Envelope::Point(i);
        }
        let Some(end) = points.len().checked_sub(1) else {
            return Envelope::Lane;
        };
        // The lines from the point before those to the one after, and the
        // level lines before the first point and after the last.
        let mut lines = Vec::new();
        if first == 0 {
            let start = pos2(layout.lanes.left(), y(points[0].value));
            lines.push((start, at(&points[0]), 0, 0));
        }
        for i in first.saturating_sub(1)..last.min(end) {
            lines.push((at(&points[i]), at(&points[i + 1]), i, i + 1));
        }
        if last > end {
            let finish = pos2(layout.lanes.right(), y(points[end].value));
            lines.push((at(&points[end]), finish, end, end));
        }
        lines
            .into_iter()
            .map(|(a, b, from, to)| (distance_to_segment(pos, a, b), from, to))
            .filter(|&(distance, ..)| distance <= grab)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Envelope::Lane, |(_, from, to)| Envelope::Line { from, to })
    }

    /// What the parameter at `param` is: see [`param_spec`].
    fn spec(&self, param: ParamAddress) -> ParamSpec {
        param_spec(self.view, param)
    }

    /// What the automation lane of `param` is called: see [`param_name`].
    fn lane_name(&self, param: ParamAddress) -> String {
        param_name(self.view, param)
    }

    /// An automation lane's name, with its channel's: "Volume of Drums".
    fn lane_label(&self, lane: &AutoRow) -> String {
        let channel = match lane.chain {
            Chain::Track(id) => self.project().track(id).map_or("", |t| t.name.as_str()),
            Chain::Master => "Master",
        };
        format!("{} of {channel}", self.lane_name(lane.param))
    }

    /// Where the line of the automation lane of `param` is at `at`: on its
    /// points, even where someone overrode it, or else at the parameter's
    /// own value, if that's known.
    fn line_value(&self, param: ParamAddress, at: Beats) -> Option<f64> {
        let project = self.project();
        match project.lane(param) {
            Some(lane) => lane.value_at(at, self.spec(param).scale),
            None => project.param_value(param),
        }
    }

    /// The value of `param` at the playhead, as its control would show it:
    /// what its automation gives it, unless someone overrode that, or else
    /// its own, if that's known.
    fn value_text(&self, param: ParamAddress) -> Option<String> {
        let project = self.project();
        let spec = self.spec(param);
        let value = match project.lane(param) {
            Some(lane) if !lane.overridden => {
                lane.value_at(self.view.playback.playhead, spec.scale)
            }
            _ => project.param_value(param),
        }?;
        if let Some(info) = project.param_info(param) {
            // Between points, a switch or a choice shows its nearest value.
            return Some(info.format(info.clamp(value)));
        }
        Some(match param {
            ParamAddress::Pan(_) => pan_text(value as f32),
            // Between points, the nearest.
            ParamAddress::Bypass(_) if value >= 0.5 => "On".to_owned(),
            ParamAddress::Bypass(_) => "Off".to_owned(),
            _ if spec.scale == Scale::Fader => decibels(value as f32),
            _ => format!("{value:.2}"),
        })
    }

    /// What the header of the automation lane of `param` says of it: its
    /// value at the playhead, or that someone overrode its automation.
    fn header_value(&self, param: ParamAddress) -> Option<String> {
        match self
            .project()
            .lane(param)
            .is_some_and(|lane| lane.overridden)
        {
            true => Some("Overridden".to_owned()),
            false => self.value_text(param),
        }
    }

    /// The parameters on `chain` an automation lane can show, with their
    /// names: see [`chain_params`].
    fn chain_params(&self, chain: Chain) -> ChainParams {
        chain_params(self.view, chain)
    }

    /// The parameter a new lane under `chain` shows: the first automated
    /// one without a lane in sight, or else the first without one.
    fn next_param(&self, chain: Chain) -> Option<ParamAddress> {
        let shown = self.editor().automation.get(&chain);
        let (own, devices) = self.chain_params(chain);
        let hidden: Vec<ParamAddress> = (own.into_iter())
            .chain(devices.into_iter().flat_map(|(_, params)| params))
            .map(|(param, _)| param)
            .filter(|param| shown.is_none_or(|shown| !shown.contains(param)))
            .collect();
        let automated = hidden.iter().find(|&&p| self.project().lane(p).is_some());
        automated.or(hidden.first()).copied()
    }

    /// The step of the grid `at` is in.
    fn cell(&self, at: Beats) -> BeatRange {
        let zoom = self.layout.viewport.zoom;
        self.editor().grid.cell(&self.project().tempo, at, zoom)
    }

    /// Whether `drag` draws a step of the grid at a time, now.
    fn draws_steps(&self, drag: &Drag) -> bool {
        !drag.free && self.editor().grid.snap
    }

    /// Draws on the automation lane `drag` started on, from where the
    /// pointer last drew to where it is now.
    fn draw(&self, drag: &mut Drag) {
        let Hit::Automation { param, .. } = drag.hit else {
            return;
        };
        let Some(lane) = self.layout.automation_lane(param) else {
            return;
        };
        let layout = &self.layout;
        let spec = self.spec(param);
        let (from, to) = (drag.drawn.last.unwrap_or(drag.to), drag.to);
        // The value along the way from `from` to `to`, at `x`.
        let value = |x: f32| {
            let along = match to.x - from.x {
                0.0 => 1.0,
                across => ((x - from.x) / across).clamp(0.0, 1.0),
            };
            let y = from.y + (to.y - from.y) * along;
            (spec.scale).from_unit(lane.unit_at(self.theme, y), spec.min, spec.max)
        };
        let beats = |x: f32| layout.beats_at(x).max(Beats::ZERO);
        let (left, right) = (from.x.min(to.x), from.x.max(to.x));
        let steps = self.draws_steps(drag);
        let values = &mut drag.drawn.values;
        if steps {
            // Each step crossed takes the value where the pointer crossed
            // its middle, or came nearest it, in place of what was drawn
            // in it off the grid.
            let end = beats(right);
            let mut at = beats(left);
            loop {
                let cell = self.cell(at);
                let middle = (layout.x_of(cell.start) + layout.x_of(cell.end)) / 2.0;
                values.retain(|&drawn, _| drawn <= cell.start || drawn >= cell.end);
                values.insert(cell.start, (value(middle.clamp(left, right)), true));
                if cell.end > end || cell.end <= at {
                    break;
                }
                at = cell.end;
            }
        } else {
            // What was drawn between goes, so going back draws over it.
            let (start, end) = (beats(left), beats(right));
            values.retain(|&at, _| at <= start || at >= end);
            values.insert(beats(from.x), (value(from.x), false));
            values.insert(beats(to.x), (value(to.x), false));
        }
        drag.drawn.last = Some(to);
    }

    /// The points of the automation of `param` once what `drawn` drew
    /// takes the place of what was there: a step at either end of it, from
    /// the line as it was, or the parameter's own value, to what was drawn
    /// and back.
    fn drawn_points(&self, param: ParamAddress, drawn: &Drawn) -> Option<Vec<Point>> {
        let (&start, _) = drawn.values.first_key_value()?;
        let (&last, &(_, step)) = drawn.values.last_key_value()?;
        let end = if step { self.cell(last).end } else { last };
        let project = self.project();
        let scale = self.spec(param).scale;
        let lane = project.lane(param);
        let own = project.param_value(param);
        let before = lane.map_or(own, |lane| lane.value_before(start, scale));
        let after = lane.map_or(own, |lane| lane.value_at(end, scale));
        let old = lane.map_or(&[][..], |lane| &lane.points[..]);
        let mut points: Vec<Point> = old.iter().filter(|p| p.at < start).copied().collect();
        points.extend(before.map(|value| Point::new(start, value)));
        let mut values = drawn.values.iter().peekable();
        while let Some((&at, &(value, step))) = values.next() {
            points.push(Point::new(at, value));
            if step {
                let next = values.peek().map_or(end, |&(&next, _)| next);
                points.push(Point::new(next, value));
            }
        }
        points.extend(after.map(|value| Point::new(end, value)));
        points.extend(old.iter().filter(|p| p.at > end));
        Some(simplified(points))
    }

    /// What `drag` would do if it ended now.
    /// `at`, for the clip `c`'s `side`, kept where the clip is never
    /// narrower than the room to grab its edges, and starts in the song.
    fn room(&self, c: &Clip, side: Side, at: Beats) -> Beats {
        let viewport = self.layout.viewport;
        let least = viewport
            .beats_in(self.theme.edge_grab)
            .max(Beats::from_ticks(1));
        match side {
            Side::Start => at.min(c.end() - least).max(Beats::ZERO),
            Side::End => at.max(c.start + least),
        }
    }

    /// Trimming the clip `clip`'s `side` by `moved`, on the grid unless
    /// `free`, as far as trimming takes it: an audio clip stops where its
    /// file does.
    fn trim(&self, clip: ClipId, side: Side, moved: Beats, free: bool) -> Option<Plan> {
        let project = self.project();
        let (_, c) = find(project, clip)?;
        let edge = match side {
            Side::Start => c.start,
            Side::End => c.end(),
        };
        let at = self.room(c, side, self.snap(edge + moved, free));
        let editor = self.editor();
        let at = match side {
            Side::Start => arrange::trimmed(project, editor, c, Some(at), None)?.start,
            Side::End => arrange::trimmed(project, editor, c, None, Some(at))?.end(),
        };
        (at != edge).then_some(Plan::Trim { clip, side, at })
    }

    /// Stretching the warped clip `clip`'s `side` by `moved`, on the grid
    /// unless `free`: its tempo changes so the audio it plays fills it, as
    /// far as its tempo's range goes. A clip that isn't warped trims
    /// instead.
    fn stretch(&self, clip: ClipId, side: Side, moved: Beats, free: bool) -> Option<Plan> {
        let (_, c) = find(self.project(), clip)?;
        let warped = match &c.content {
            ClipContent::Audio(audio) => audio.warp.as_ref().map(|warp| (audio, warp)),
            ClipContent::Midi(_) => None,
        };
        let Some((audio, warp)) = warped else {
            return self.trim(clip, side, moved, free);
        };
        let edge = match side {
            Side::Start => c.start,
            Side::End => c.end(),
        };
        let at = self.room(c, side, self.snap(edge + moved, free));
        let length = match side {
            Side::Start => c.end() - at,
            Side::End => at - c.start,
        };
        let tempo = warp.clip_tempo(self.editor().sample_rate_of(&audio.file));
        let factor = length.to_f64() / c.length.to_f64();
        let bpm = (tempo * factor).clamp(Warp::MIN_TEMPO, Warp::MAX_TEMPO);
        let length = Beats::from_f64(c.length.to_f64() * bpm / tempo);
        let at = match side {
            Side::Start => c.end() - length,
            Side::End => c.start + length,
        };
        (at != edge).then_some(Plan::Stretch {
            clip,
            side,
            at,
            bpm,
        })
    }

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
                let moving: Vec<(usize, &Clip)> = drag
                    .clips
                    .iter()
                    .filter_map(|&c| find(project, c))
                    .collect();
                let earliest = moving.iter().map(|(_, c)| c.start).min()?;
                let by =
                    (self.snap(grabbed.start + moved, drag.free) - grabbed.start).max(-earliest);
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
            ) => self.trim(clip, side, moved, drag.free),
            (
                MouseAction::Stretch,
                Hit::Clip {
                    clip,
                    part: Part::Edge(side),
                    ..
                },
            ) => self.stretch(clip, side, moved, drag.free),
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
                // The rows dragged over, by their places: the tracks', then
                // the master's, under them.
                let master = tracks.len();
                let from = match hit.track() {
                    Some(track) => project.track_index(track)?,
                    None if hit.on_master() => master,
                    None => layout.nearest_index(drag.from.y)?,
                };
                let to = match layout.in_master(drag.to.y) && layout.master_shown() {
                    true => master,
                    false => layout.nearest_index(drag.to.y).unwrap_or(master),
                };
                let (top, bottom) = (from.min(to), from.max(to));
                let (start, end) = self.ends(drag);
                Some(Plan::Time {
                    start: start.min(end),
                    end: start.max(end),
                    tracks: tracks[top..(bottom + 1).min(master)]
                        .iter()
                        .map(|t| t.id)
                        .collect(),
                    master: bottom == master,
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
                let (start, end) = self.ends(drag);
                (start != end).then_some(Plan::Loop {
                    start: start.min(end),
                    end: start.max(end),
                    looping: true,
                })
            }
            (MouseAction::MoveLoop, _) => {
                let region = project.loop_region;
                let start = self.snap(region.start + moved, drag.free).max(Beats::ZERO);
                (start != region.start).then_some(Plan::Loop {
                    start,
                    end: start + region.len(),
                    looping: project.looping,
                })
            }
            (MouseAction::ResizeLoop, Hit::LoopEdge(side)) => {
                let region = project.loop_region;
                let (start, end) = match side {
                    Side::Start => (self.snap(region.start + moved, drag.free), region.end),
                    Side::End => (region.start, self.snap(region.end + moved, drag.free)),
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
                let at = self.snap(locator.at + moved, drag.free).max(Beats::ZERO);
                (at != locator.at).then_some(Plan::Locator { locator: id, at })
            }
            (MouseAction::ResizeTrack, Hit::TrackEdge(track)) => Some(Plan::Height {
                chain: Chain::Track(track),
                height: (drag.height + drag.to.y - drag.from.y)
                    .clamp(views::MIN_TRACK_HEIGHT, views::MAX_TRACK_HEIGHT),
            }),
            // The master's top edge: up makes it taller, as far as the
            // master's part of the lanes goes.
            (MouseAction::ResizeTrack, Hit::MasterEdge) => {
                let (least, most) = (views::MIN_TRACK_HEIGHT, views::MAX_TRACK_HEIGHT);
                let room = master_room(layout.lanes, self.theme).clamp(least, most);
                Some(Plan::Height {
                    chain: Chain::Master,
                    height: (drag.height + drag.from.y - drag.to.y).clamp(least, room),
                })
            }
            (MouseAction::Comp | MouseAction::CompFree, Hit::Take { track, take, .. }) => {
                let (start, end) = self.ends(drag);
                (start != end).then_some(Plan::Comp {
                    track,
                    take,
                    start: start.min(end),
                    end: start.max(end),
                })
            }
            (
                MouseAction::MovePoint | MouseAction::MovePointFree,
                Hit::Automation {
                    param,
                    part: Envelope::Point(i),
                    ..
                },
            ) => {
                let lane = layout.automation_lane(param)?;
                let old = &project.lane(param)?.points;
                let point = *old.get(i)?;
                let spec = self.spec(param);
                // It stays between the points either side.
                let earliest = i
                    .checked_sub(1)
                    .map_or(Beats::ZERO, |before| old[before].at);
                let mut at = self.snap(point.at + moved, drag.free).max(earliest);
                if let Some(next) = old.get(i + 1) {
                    at = at.min(next.at);
                }
                let unit = spec.scale.to_unit(point.value, spec.min, spec.max)
                    + lane.units_in(self.theme, drag.from.y - drag.to.y);
                let mut points = old.clone();
                points[i] = Point::new(at, spec.scale.from_unit(unit, spec.min, spec.max));
                (points != *old).then_some(Plan::Points { param, points })
            }
            (
                MouseAction::MoveSegment,
                Hit::Automation {
                    param,
                    part: Envelope::Line { from, to },
                    ..
                },
            ) => {
                let lane = layout.automation_lane(param)?;
                let old = &project.lane(param)?.points;
                let spec = self.spec(param);
                let up = lane.units_in(self.theme, drag.from.y - drag.to.y);
                let mut points = old.clone();
                for point in points.get_mut(from..=to)? {
                    let unit = spec.scale.to_unit(point.value, spec.min, spec.max) + up;
                    point.value = spec.scale.from_unit(unit, spec.min, spec.max);
                }
                (points != *old).then_some(Plan::Points { param, points })
            }
            (MouseAction::Draw | MouseAction::DrawFree, Hit::Automation { param, .. }) => {
                let points = self.drawn_points(param, &drag.drawn)?;
                let old = project.lane(param).map(|lane| &lane.points);
                (old != Some(&points)).then_some(Plan::Points { param, points })
            }
            _ => None,
        }
    }
}

/// The parameters of a channel: its own, with their names, then each
/// device's and each modulator's, by its name.
pub(crate) type ChainParams = (
    Vec<(ParamAddress, String)>,
    Vec<(String, Vec<(ParamAddress, String)>)>,
);

/// What the parameter at `param` is: as the project knows it, or as its
/// plugin says, or else a parameter from 0 to 1.
pub(crate) fn param_spec(view: &View<'_>, param: ParamAddress) -> ParamSpec {
    if let Some(spec) = view.project.param_spec(param) {
        return spec;
    }
    let listed = match param {
        ParamAddress::Device(device, n) => (view.plugins.status.get(&device))
            .and_then(|status| status.params.iter().find(|p| p.id == n)),
        _ => None,
    };
    match (listed, param) {
        (Some(listed), _) => listed.spec.clone(),
        (None, ParamAddress::Device(_, n)) => unknown(format!("Parameter {n}")),
        (None, param) => unknown(param_text(param)),
    }
}

/// What the parameter at `param` is called, as its automation lane names
/// it: after its device's name for a device's, and its modulator's for a
/// modulator's.
pub(crate) fn param_name(view: &View<'_>, param: ParamAddress) -> String {
    let name = param_spec(view, param).name;
    let project = view.project;
    let modulator = param.modulator().and_then(|id| project.find_modulator(id));
    if let Some((_, _, modulator)) = modulator {
        return format!("{}: {name}", modulator.name);
    }
    // A wire's gain goes with the device it feeds.
    let device = match param {
        ParamAddress::Wire(wire) => project.wire(wire).map(|wire| wire.to.device()),
        param => param.device(),
    };
    let device = device.and_then(|device| project.find_device(device));
    match device {
        Some((_, _, device)) => format!("{}: {name}", describe(device).0),
        None => name,
    }
}

/// The parameters on `chain` an automation lane can show, with their
/// names: the channel's volume, pan and sends, then each device's
/// parameters, by device: its Bypass, then every one in a built-in
/// device's table, or the ones a plugin lists, and the gains of the wires
/// into it. Then each modulator's, by modulator.
pub(crate) fn chain_params(view: &View<'_>, chain: Chain) -> ChainParams {
    let project = view.project;
    let mut own = vec![
        (ParamAddress::Volume(chain), "Volume".to_owned()),
        (ParamAddress::Pan(chain), "Pan".to_owned()),
    ];
    if let Chain::Track(id) = chain
        && let Some(track) = project.track(id)
    {
        for send in &track.sends {
            let param = ParamAddress::Send(send.id);
            own.push((param, param_spec(view, param).name));
        }
    }
    let devices = project.devices(chain).unwrap_or_default();
    let devices = devices.iter().map(|device| {
        let bypass = ParamAddress::Bypass(device.id);
        let mut params = vec![(bypass, param_spec(view, bypass).name)];
        params.extend(match &device.kind {
            DeviceKind::Plugin(_) => {
                let status = view.plugins.status.get(&device.id);
                let listed = status.map_or(&[][..], |status| &status.params[..]);
                let mut params: Vec<(ParamAddress, String)> = (listed.iter())
                    .map(|p| (ParamAddress::Device(device.id, p.id), p.spec.name.clone()))
                    .collect();
                // Lanes of parameters it doesn't list, as before it
                // loads.
                for lane in &project.automation {
                    if let ParamAddress::Device(on, _) = lane.param
                        && on == device.id
                        && !params.iter().any(|&(param, _)| param == lane.param)
                    {
                        params.push((lane.param, param_spec(view, lane.param).name));
                    }
                }
                params
            }
            DeviceKind::Native(native) => (0..native.params().len() as u32)
                .map(|n| {
                    let param = ParamAddress::Device(device.id, n);
                    (param, param_spec(view, param).name)
                })
                .collect(),
            DeviceKind::TestGain { .. } | DeviceKind::TestInstrument { .. } => {
                let param = ParamAddress::Device(device.id, 0);
                vec![(param, param_spec(view, param).name)]
            }
        });
        // The gains of the wires into its side inputs.
        let ports = project.ports(device.id);
        let wires = ports.into_iter().flat_map(|port| project.wires_into(port));
        for param in wires
            .map(|wire| ParamAddress::Wire(wire.id))
            .collect::<Vec<_>>()
        {
            params.push((param, param_spec(view, param).name));
        }
        (describe(device).0.to_owned(), params)
    });
    let mut groups: Vec<(String, Vec<(ParamAddress, String)>)> = devices.collect();
    let modulators = project
        .modulation(chain)
        .map_or(&[][..], |m| &m.modulators[..]);
    for modulator in modulators {
        let params = (modulator.params().into_iter())
            .map(|n| ParamAddress::Modulator(modulator.id, n))
            .map(|param| (param, param_spec(view, param).name))
            .collect();
        groups.push((modulator.name.clone(), params));
    }
    (own, groups)
}

/// A parameter called `name`, from 0 to 1, for a lane whose parameter is
/// unknown, such as a plugin's before the plugin loads.
fn unknown(name: String) -> ParamSpec {
    ParamSpec {
        name,
        min: 0.0,
        max: 1.0,
        default: 0.0,
        scale: Scale::Linear,
    }
}

/// How far `pos` is from the line from `a` to `b`.
pub(crate) fn distance_to_segment(pos: Pos2, a: Pos2, b: Pos2) -> f32 {
    let along = b - a;
    let length = along.length_sq();
    let t = match length {
        0.0 => 0.0,
        _ => ((pos - a).dot(along) / length).clamp(0.0, 1.0),
    };
    (a + along * t).distance(pos)
}

/// `points` without those that change nothing: one just like the one
/// before it, and one in a level stretch between two of the same value.
pub(crate) fn simplified(points: Vec<Point>) -> Vec<Point> {
    let mut kept: Vec<Point> = Vec::with_capacity(points.len());
    for point in points {
        if kept.last() == Some(&point) {
            continue;
        }
        if let [.., before, middle] = kept[..]
            && before.value == middle.value
            && middle.value == point.value
        {
            kept.pop();
        }
        kept.push(point);
    }
    kept
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
        Plan::Stretch {
            clip, side, bpm, ..
        } => {
            let args = Args::new()
                .with("bpm", *bpm)
                .with("keep_end", *side == Side::Start)
                .with("clip", *clip);
            run(actions, warp::SET_TEMPO, args);
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
        Plan::Time {
            start,
            end,
            tracks,
            master,
        } => {
            let mut args = Args::new()
                .with("start", *start)
                .with("end", *end)
                .with("tracks", tracks.clone());
            if *master {
                args.set("master", true);
            }
            run(actions, edit::SELECT_TIME, args);
        }
        Plan::Viewport(viewport) => set_viewport(actions, viewport),
        Plan::Height { chain, height } => {
            let args = match chain {
                Chain::Track(track) => Args::new().with("track", *track),
                Chain::Master => Args::new().with("master", true),
            };
            run(
                actions,
                views::SET_TRACK_HEIGHT,
                args.with("height", *height),
            );
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
        Plan::Points { param, points } => {
            let at: Vec<Beats> = points.iter().map(|point| point.at).collect();
            let values: Vec<f64> = points.iter().map(|point| point.value).collect();
            let args = Args::new()
                .with("param", *param)
                .with("at", at)
                .with("values", values);
            run(actions, automation::SET_POINTS, args);
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
    /// Shows the arrangement in the rest of `ui`, with the tracks' headers
    /// made of the parts `parts` has, and adds what the user asked for to
    /// `actions`.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        theme: &Theme,
        mouse: &MouseMap,
        parts: &Parts,
        actions: &mut Vec<Action>,
    ) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let editor = view.editor;
        let master_scroll = self.master_scroll;
        let layout = Layout::new(
            rect,
            theme,
            view.project,
            editor,
            editor.viewport,
            master_scroll,
        );
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|&pos| rect.contains(pos) && ui.rect_contains_pointer(rect));
        let mut scene = Scene {
            view,
            theme,
            mouse,
            parts,
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
        let buttons = self.lane_buttons(ui, &scene, actions);
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
        }
        if viewport != editor.viewport || self.master_scroll != master_scroll {
            let scroll = self.master_scroll;
            scene.layout = Layout::new(rect, theme, view.project, editor, viewport, scroll);
        }
        self.master_scroll = scene.layout.master_scroll;
        let plan = self.drag.as_ref().and_then(|drag| scene.plan(drag));
        self.cursor(ui, &scene, plan.as_ref());
        let plan = plan.or(self.landed.take());
        // What's dragged from the browser or the devices panel, and where
        // it would go.
        let dropping = scene.pointer.and_then(|pos| {
            let dragged = egui::DragAndDrop::payload::<Dragged>(ui.ctx())?;
            let to = scene.destination(pos)?;
            dragged.dropped(&to).map(|action| (action, to))
        });

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
        let headers_painter = painter.with_clip_rect(scene.layout.headers);
        paint::headers(&headers_painter, &scene);
        // The headers' parts, in columns down the headers. Their buttons
        // go over the headers, so they take their clicks.
        let lineup = parts.shown(TRACK_HEADER, theme);
        let columns = header::columns(&scene, &lineup);
        header::paint(ui, &headers_painter, &scene, &columns, actions);
        paint::over_headers(&headers_painter, &scene);
        for button in &buttons {
            let (face, on, colour) = (button.face, button.on, button.colour);
            paint_toggle(ui, button.rect, &button.response, face, on, colour, theme);
        }
        paint::corner(&painter.with_clip_rect(scene.layout.corner), &scene);
        if ui.input(|i| !i.raw.hovered_files.is_empty()) {
            paint::drop_hint(&painter, &scene);
        }
        if let Some((_, to)) = &dropping {
            paint::destination(&painter, &scene, to);
        }
        name_for_screen_readers(ui, &scene, id);

        for (response, area) in [
            (&lanes, Area::Lanes),
            (&ruler, Area::Ruler),
            (&headers, Area::Headers),
        ] {
            // A header's buttons take their right-clicks, which open the
            // headers' menu all the same, unless another menu is open.
            let ctx = ui.ctx();
            let taken = area == Area::Headers
                && !response.secondary_clicked()
                && response.container_secondary_clicked()
                && (!Popup::is_any_open(ctx)
                    || Popup::is_id_open(ctx, Popup::default_response_id(response)));
            if (response.secondary_clicked() || taken)
                && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            {
                let hit = scene.hit(area, pos);
                let at = match hit {
                    Hit::Locator(id) => scene.project().locator(id).map(|l| l.at),
                    _ => None,
                };
                let part = match hit {
                    Hit::Header(_) | Hit::TrackEdge(_) | Hit::MasterHeader | Hit::MasterEdge => {
                        header::part_at(&columns, pos.x)
                    }
                    _ => None,
                };
                let spot = Spot {
                    hit,
                    at: at.unwrap_or_else(|| scene.snapped(pos.x)),
                    part,
                };
                self.menu = Some(spot);
                select_for_menu(&scene, spot, actions);
            }
            let menu = Popup::context_menu(response);
            let menu = match taken {
                true => menu.open_memory(SetOpenCommand::Bool(true)),
                false => menu,
            };
            menu.show(|ui| {
                if let Some(spot) = self.menu {
                    menu::show(ui, &scene, spot, actions);
                }
            });
        }
        self.drop_files(ui, &scene, actions);
        if let Some((action, _)) = dropping
            && ui.input(|i| i.pointer.any_released())
        {
            egui::DragAndDrop::clear_payload(ui.ctx());
            actions.push(action);
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
            self.drag_started(scene, area, from, mods, actions);
        }
        if let Some(drag) = self.drag.as_mut().filter(|drag| drag.area == area) {
            if let Some(pos) = response.interact_pointer_pos() {
                drag.to = pos;
            }
            // Pressing Ctrl or Alt part of the way through copies clips or
            // keeps off the grid, until it's let go.
            (drag.action, drag.free) = scene.mouse.during(drag.hit.target(), drag.action, mods);
            if matches!(drag.action, MouseAction::Draw | MouseAction::DrawFree) {
                scene.draw(drag);
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
                self.landed = self.drag_stopped(scene, &drag, actions);
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
            // The master is selected alone.
            (MouseAction::Select | MouseAction::ToggleSelect, Hit::MasterHeader) => {
                run(actions, edit::SELECT_MASTER, Args::new());
            }
            (MouseAction::PlaceMarker, Hit::Lane(track)) => {
                select_time(actions, vec![track]);
                marker(actions);
            }
            (MouseAction::PlaceMarker, Hit::Empty) => {
                select_time(actions, Vec::new());
                marker(actions);
            }
            (MouseAction::PlaceMarker, hit) if hit.on_master() => {
                run(actions, edit::SELECT_MASTER, Args::new());
                marker(actions);
            }
            (
                MouseAction::PlaceMarker,
                Hit::Automation {
                    chain: Chain::Track(track),
                    ..
                },
            ) => {
                select_time(actions, vec![track]);
                marker(actions);
            }
            (MouseAction::Select, Hit::AutomationHeader { chain, .. }) => match chain {
                Chain::Track(track) => {
                    let args = Args::new().with("tracks", vec![track]);
                    run(actions, edit::SELECT_TRACKS, args);
                }
                Chain::Master => run(actions, edit::SELECT_MASTER, Args::new()),
            },
            (MouseAction::AddPoint, Hit::Automation { param, part, .. }) => {
                let Some(lane) = scene.layout.automation_lane(param) else {
                    return false;
                };
                // On the line, a point on it, which changes nothing until
                // it's moved.
                let on_line = match part {
                    Envelope::Line { .. } => scene.line_value(param, at),
                    _ => None,
                };
                let spec = scene.spec(param);
                let value = on_line.unwrap_or_else(|| {
                    let unit = lane.unit_at(scene.theme, pos.y);
                    spec.scale.from_unit(unit, spec.min, spec.max)
                });
                let args = Args::new()
                    .with("param", param)
                    .with("at", at)
                    .with("value", value);
                run(actions, automation::ADD_POINT, args);
            }
            (
                MouseAction::DeletePoint,
                Hit::Automation {
                    param,
                    part: Envelope::Point(i),
                    ..
                },
            ) => {
                let Some(lane) = project.lane(param).filter(|lane| i < lane.points.len()) else {
                    return false;
                };
                let mut points = lane.points.clone();
                points.remove(i);
                // Without points, the parameter keeps the value it had at
                // the playhead.
                if points.is_empty() {
                    run(actions, automation::CLEAR, Args::new().with("param", param));
                } else {
                    send(actions, &Plan::Points { param, points });
                }
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
            // MIDI clips open in the piano roll, and audio clips in the
            // Sample Editor.
            (MouseAction::Open, Hit::Clip { clip, .. }) => {
                let Some((_, c)) = find(project, clip) else {
                    return false;
                };
                let open = match c.content {
                    ClipContent::Midi(_) => notes::OPEN,
                    ClipContent::Audio(_) => warp::OPEN,
                };
                run(actions, open, Args::new().with("clip", clip));
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
        let (action, free) = scene.mouse.during(hit.target(), action, mods);
        let mut drag = Drag {
            area,
            action,
            hit,
            free,
            started_free: free,
            from,
            to: from,
            clips: Vec::new(),
            viewport: scene.layout.viewport,
            height: 0.0,
            sent: None,
            drawn: Drawn::default(),
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
            (MouseAction::ResizeTrack, Hit::MasterEdge) => {
                drag.height = scene.layout.master.height;
            }
            (MouseAction::Draw | MouseAction::DrawFree, _) => scene.draw(&mut drag),
            _ => {}
        }
        self.drag = Some(drag);
    }

    /// Asks for what the drag `drag` planned, as it ends. Returns the plan.
    fn drag_stopped(
        &mut self,
        scene: &Scene<'_>,
        drag: &Drag,
        actions: &mut Vec<Action>,
    ) -> Option<Plan> {
        let plan = scene.plan(drag)?;
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
        Some(plan)
    }

    /// The buttons in the header of each automation lane in sight. Returns
    /// them, to be drawn over the headers.
    fn lane_buttons(
        &mut self,
        ui: &mut Ui,
        scene: &Scene<'_>,
        actions: &mut Vec<Action>,
    ) -> Vec<Button> {
        let theme = scene.theme;
        let mut buttons = Vec::new();
        for lane in scene.layout.visible_automation() {
            let name = scene.lane_label(lane);
            for (button, rect) in paint::lane_buttons(scene, lane) {
                let (face, label) = match button {
                    LaneButton::Choose => (Face::Menu, format!("Choose the parameter of {name}")),
                    LaneButton::Add => (Face::Text("+"), format!("Add a lane under {name}")),
                    LaneButton::Hide => (Face::Text("\u{d7}"), format!("Hide {name}")),
                };
                let id = Id::unique(("lane button", lane.param, button));
                let response = ui.interact(rect, id, Sense::click());
                response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                let param = Args::new().with("param", lane.param);
                match button {
                    LaneButton::Choose => {
                        Popup::menu(&response).show(|ui| choose_param(ui, scene, lane, actions));
                    }
                    LaneButton::Add if response.clicked() => {
                        if let Some(next) = scene.next_param(lane.chain) {
                            let args = Args::new().with("param", next);
                            run(actions, automation::SHOW_LANE, args);
                        }
                    }
                    LaneButton::Hide if response.clicked() => {
                        run(actions, automation::HIDE_LANE, param);
                    }
                    _ => {}
                }
                buttons.push(Button {
                    rect,
                    response,
                    face,
                    on: false,
                    colour: theme.automation,
                });
            }
        }
        buttons
    }

    /// Scrolls and zooms with the wheel, over the tracks and the ruler. Over
    /// the master's part, when its lanes don't all fit, it scrolls them.
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
        let master = layout.in_master(pos.y) && layout.master_overflow > 0.0;
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
                    match master {
                        true => self.master_scroll -= delta.y,
                        false => viewport.top -= delta.y,
                    }
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
        let tracks = layout.tracks_part(layout.lanes.x_range());
        let lowest = lowest_top(layout.content, scene.theme, tracks);
        viewport.top = viewport.top.clamp(0.0, lowest);
        self.master_scroll = self.master_scroll.clamp(0.0, layout.master_overflow);
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
            (
                _,
                Some(
                    MouseAction::Trim
                    | MouseAction::TrimFree
                    | MouseAction::Stretch
                    | MouseAction::Fade,
                ),
            ) => Some(CursorIcon::ResizeHorizontal),
            (_, Some(MouseAction::ResizeLoop)) => Some(CursorIcon::ResizeHorizontal),
            (_, Some(MouseAction::ResizeTrack)) => Some(CursorIcon::ResizeVertical),
            (_, Some(MouseAction::Copy)) => Some(CursorIcon::Copy),
            (_, Some(MouseAction::Move | MouseAction::MoveFree | MouseAction::MoveLocator)) => {
                Some(CursorIcon::Grab)
            }
            (_, Some(MouseAction::MoveLoop | MouseAction::MovePoint)) => Some(CursorIcon::Grab),
            (_, Some(MouseAction::MovePointFree)) => Some(CursorIcon::Grab),
            (_, Some(MouseAction::MoveSegment)) => Some(CursorIcon::ResizeVertical),
            (_, Some(MouseAction::Draw | MouseAction::DrawFree)) => Some(CursorIcon::Crosshair),
            _ => None,
        };
        let shown = match &self.drag {
            Some(drag) => match (drag.action, plan) {
                (
                    MouseAction::Move
                    | MouseAction::MoveFree
                    | MouseAction::MovePoint
                    | MouseAction::MovePointFree,
                    _,
                ) => Some(CursorIcon::Grabbing),
                (MouseAction::Copy, _) => Some(CursorIcon::Copy),
                (action, _) => icon(drag.hit, Some(action)),
            },
            // egui's while something's dragged from the browser or the
            // devices panel.
            None if egui::DragAndDrop::has_any_payload(ui.ctx()) => return,
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

/// The buttons in an automation lane's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LaneButton {
    /// Picks the parameter the lane shows.
    Choose,
    /// Shows another lane under the channel.
    Add,
    Hide,
}

/// The parameters `lane` can show, to pick from: its channel's own, then
/// each device's, with those that have automation in its colour.
fn choose_param(ui: &mut Ui, scene: &Scene<'_>, lane: &AutoRow, actions: &mut Vec<Action>) {
    let (own, devices) = scene.chain_params(lane.chain);
    let mut item = |ui: &mut Ui, param: ParamAddress, name: &str| {
        let mut text = RichText::new(name);
        if scene.project().lane(param).is_some() {
            text = text.color(scene.theme.automation);
        }
        if ui
            .add(egui::Button::selectable(param == lane.param, text))
            .clicked()
        {
            if param != lane.param {
                let args = Args::new()
                    .with("param", param)
                    .with("instead_of", lane.param);
                run(actions, automation::SHOW_LANE, args);
            }
            ui.close();
        }
    };
    for (param, name) in &own {
        item(ui, *param, name);
    }
    for (device, params) in &devices {
        ui.menu_button(device.as_str(), |ui| {
            ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (param, name) in params {
                    item(ui, *param, name);
                }
            });
        });
    }
}

/// An automation lane's button, to be drawn once the headers are.
struct Button {
    rect: Rect,
    response: Response,
    face: Face<'static>,
    on: bool,
    colour: Color32,
}

/// How far down the tracks scroll in `tracks`, their part of the lanes,
/// when they and their take lanes are `content` tall: until a track's
/// height is left under the last.
fn lowest_top(content: f32, theme: &Theme, tracks: Rect) -> f32 {
    (content + theme.track_height - tracks.height()).max(0.0)
}

/// The most of `lanes` the master's row and its automation lanes take:
/// their share, as long as that leaves the tracks a track's height.
fn master_room(lanes: Rect, theme: &Theme) -> f32 {
    (lanes.height() * MASTER_SHARE).min((lanes.height() - theme.track_height).max(0.0))
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
        // As on a track's lane, a time selection stays.
        Hit::Master if selection.time.is_some() => {}
        Hit::Master | Hit::MasterHeader | Hit::MasterEdge
            if scene.editor().selected_track != Some(Chain::Master) =>
        {
            run(actions, edit::SELECT_MASTER, Args::new());
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
    let headers = layout.headers.x_range();
    let lanes = layout.lanes.x_range();
    for row in layout.visible_rows() {
        let track = &project.tracks[row.index];
        let header = Rect::from_x_y_ranges(headers, row.top..=row.bottom());
        let response = ui.interact(
            header.intersect(layout.tracks_part(headers)),
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
                .intersect(layout.tracks_part(lanes));
            let response = ui.interact(rect, id.with(("clip", clip.id)), Sense::hover());
            let selected = selection.clips.contains(&clip.id);
            response.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, selected, format!("Clip {}", clip.name))
            });
        }
    }
    let master = layout.master;
    let header = Rect::from_x_y_ranges(headers, master.top..=master.bottom());
    let response = ui.interact(
        header.intersect(layout.master_part(headers)),
        id.with("master"),
        Sense::hover(),
    );
    let selected = scene.editor().selected_track == Some(Chain::Master);
    response.widget_info(|| WidgetInfo::selected(Role::Row, true, selected, "Master track"));
    for lane in layout.visible_takes() {
        let track = &project.tracks[lane.track_index];
        let take = &track.takes[lane.index];
        let header = Rect::from_x_y_ranges(headers, lane.top..=lane.bottom());
        let response = ui.interact(
            header.intersect(layout.tracks_part(headers)),
            id.with(("take", take.id)),
            Sense::hover(),
        );
        response.widget_info(|| {
            WidgetInfo::labeled(Role::Row, true, format!("{} of {}", take.name, track.name))
        });
    }
    for lane in layout.visible_automation() {
        let name = scene.lane_label(lane);
        let header = Rect::from_x_y_ranges(headers, lane.top..=lane.bottom());
        let response = ui.interact(
            header.intersect(layout.part_of(lane.chain, headers)),
            id.with(("automation", lane.param)),
            Sense::hover(),
        );
        let value = scene.header_value(lane.param);
        response.widget_info(|| WidgetInfo {
            current_text_value: value.clone(),
            ..WidgetInfo::labeled(Role::Row, true, &name)
        });
        // Its points in sight, while there are few enough to name.
        let Some(automated) = project.lane(lane.param) else {
            continue;
        };
        let points = &automated.points;
        let first = points.partition_point(|p| p.at < visible.start);
        let last = points.partition_point(|p| p.at <= visible.end);
        if last - first > MAX_NAMED_POINTS {
            continue;
        }
        let spec = scene.spec(lane.param);
        for (i, point) in points.iter().enumerate().take(last).skip(first) {
            let unit = spec.scale.to_unit(point.value, spec.min, spec.max);
            let at = pos2(layout.x_of(point.at), lane.y_of(scene.theme, unit));
            let rect = Rect::from_center_size(at, vec2(2.0, 2.0) * scene.theme.point);
            let response = ui.interact(
                rect.intersect(layout.part_of(lane.chain, lanes)),
                id.with(("point", lane.param, i)),
                Sense::hover(),
            );
            response.widget_info(|| {
                WidgetInfo::labeled(Role::Button, true, format!("Point {} of {name}", i + 1))
            });
        }
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

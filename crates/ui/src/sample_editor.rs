//! The Sample Editor: the audio clip it shows, its waveform against the
//! song's beats, as Live's Sample Editor shows it, with the clip's controls
//! beside it: Warp, its mode and the mode's own settings, its tempo, its
//! transpose and detune, and its gain. A ruler over the waveform shows the
//! song's bars and one under it the file's time, the warp markers' handles
//! are along the waveform's top, with ticks under them where a warped
//! clip's transients are, and under it all a lane shows the curve of one of
//! the clip's parameters: its gain, or one of its warp's settings, as the
//! lane's name beside it chooses. Near a transient, the top half shows a
//! pseudo marker, as Live's does: dragging or double-clicking it makes it a
//! warp marker.
//!
//! Like the piano roll, it's drawn with egui's painter, only what's in
//! sight, and everything it does goes through commands. What it shows and
//! has selected, its zoom and its scrolling are the editor's
//! [`SampleEditor`](musicable_commands::editor::SampleEditor), so commands,
//! scripts and agents see what the user sees. Gestures are looked up in
//! the [`MouseMap`], and a drag shows what it'll do while it lasts and runs
//! one command when it ends, so it's one step in the undo history. Across,
//! it shows song positions, as the arrangement does, so the start marker,
//! the playhead and the grid are the same in both: a warped clip's markers
//! sit on the beats they play at, and its audio stretches between them.

mod menu;
mod paint;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use egui::{
    Button, ComboBox, CursorIcon, DragValue, Event, Id, MouseWheelUnit, Pos2, Rect, Response,
    RichText, Role, ScrollArea, Sense, Ui, UiBuilder, WidgetInfo, pos2,
};
use musicable_commands::editor::Viewport;
use musicable_commands::{
    Args, CommandId, Editor, Focus, Modifiers, auto_warp, automation, transient, transport,
    view as views, warp, warp_mode,
};
use musicable_model::Project;
use musicable_model::automation::Point;
use musicable_model::clip::{AudioClip, Clip, ClipContent};
use musicable_model::param::{ClipParam, ParamAddress, ParamSpec};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::{
    Onset, Origin, Preserve, SliceSettings, SpectralSettings, TransientLoop, Warp, WarpMarker,
    WarpMode,
};

use crate::arrangement::{distance_to_segment, simplified};
use crate::mixer::{decibels, name_menu};
use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::waveform::Peaks;
use crate::widgets::position;

/// How much a point of the wheel, or of a drag up or down a ruler, zooms:
/// the zoom changes by `e` to the power of this many points.
const ZOOM_SPEED: f32 = 0.005;
const SCRUB_ZOOM_SPEED: f32 = 0.01;

/// The quietest the gain field goes before silence, in decibels.
const GAIN_FLOOR: f64 = -70.0;

/// The Sample Editor's own state: the gesture in progress.
#[derive(Debug, Default)]
pub(crate) struct SampleEditor {
    drag: Option<Drag>,
    /// Set when the user scrolls away from the playhead while it plays, so
    /// Follow doesn't scroll back until the playhead is in sight again.
    follow_paused: bool,
    /// Where the context menu that's open was opened.
    menu: Option<Spot>,
}

/// The parts of the Sample Editor that take the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Area {
    /// The ruler of the song's bars, over the waveform.
    Ruler,
    Wave,
    /// The ruler of the file's time, under the waveform.
    Time,
    /// The lane of the curve of one of the clip's parameters.
    Lane,
}

/// What's under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// The warp marker at this place in the warp's markers.
    Marker(usize),
    /// The transient at this frame of the file, near the pointer in the
    /// waveform's top half, away from the markers: its pseudo marker.
    Transient(u64),
    /// The waveform's top half, away from the markers and the transients.
    WaveTop,
    /// The waveform's bottom half.
    Wave,
    Ruler,
    Lane(Envelope),
}

impl Hit {
    /// What the mouse map calls it.
    fn target(self) -> Target {
        match self {
            Hit::Marker(_) => Target::WarpMarker,
            Hit::Transient(_) => Target::Transient,
            Hit::WaveTop => Target::WaveformTop,
            Hit::Wave => Target::Waveform,
            Hit::Ruler => Target::Ruler,
            Hit::Lane(Envelope::Point(_)) => Target::AutomationPoint,
            Hit::Lane(Envelope::Line { .. }) => Target::AutomationLine,
            Hit::Lane(Envelope::Lane) => Target::AutomationLane,
        }
    }
}

/// A part of the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Envelope {
    /// The point at this place in the curve's points.
    Point(usize),
    /// Near the line through the points at these places, next to each
    /// other: or the same point twice, for the level line before the first
    /// point or after the last, or, for a parameter in steps, the level
    /// line from a point to the next, which holds its value. A lane without
    /// points has a level line at the parameter's own value, with no points
    /// to move.
    Line {
        from: usize,
        to: usize,
    },
    Lane,
}

/// Where a context menu was opened: what's there, the song position there,
/// snapped to the grid, and the moment of the clip's own time and the frame
/// of the file that play there.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    hit: Hit,
    at: Beats,
    own: Beats,
    frame: Option<u64>,
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
    /// The places of the markers it moves.
    markers: Vec<usize>,
    /// The viewport as the drag started.
    viewport: Viewport,
    /// What the drag last asked for, for those that ask while they last.
    sent: Option<Plan>,
    /// What a drag that draws on the lane drew so far: a value for
    /// each step of the grid it crossed, by the song position the step
    /// starts at, or drawn freely, a value for each place the pointer was.
    drawn: BTreeMap<Beats, f64>,
    /// Where the pointer was when it last drew.
    last: Option<Pos2>,
}

/// What a drag would do if it ended now.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    /// Moves the markers at these places by some beats.
    Markers { markers: Vec<usize>, by: Beats },
    /// Moves the audio under the marker at this place, so this frame of the
    /// file plays there.
    Audio { marker: usize, frame: u64 },
    /// Adds a warp marker at the transient at this frame, and moves it by
    /// some beats: a pseudo marker dragged.
    Pseudo { frame: u64, by: Beats },
    /// Moves the transient at `frame` to the frame `to`.
    Transient { frame: u64, to: u64 },
    /// Selects the clip's own time that plays from song position `start` to
    /// `end`. Drags ask for it as they go.
    Time { start: Beats, end: Beats },
    /// Sets the points of the curve the lane shows, in the clip's own time.
    Points(Vec<Point>),
    /// Scrolls and zooms. Drags ask for it as they go.
    Viewport(Viewport),
}

/// Where the Sample Editor's parts are this frame.
#[derive(Debug, Clone)]
struct Layout {
    /// The column of the clip's controls, beside the rulers and the
    /// waveform, and the lane's name, beside the lane.
    controls: Rect,
    label: Rect,
    /// The ruler of the song's bars, the waveform, the strip along its top
    /// with the markers' handles, the ruler of the file's time and the
    /// lane, top to bottom.
    ruler: Rect,
    wave: Rect,
    handles: Rect,
    time: Rect,
    lane: Rect,
    /// The zoom and scrolling, with the waveform's width this frame.
    viewport: Viewport,
}

impl Layout {
    fn new(rect: Rect, theme: &Theme, viewport: Viewport) -> Self {
        let sizes = theme.sample_editor;
        let split = (rect.left() + sizes.controls).min(rect.right());
        let ruler_bottom = (rect.top() + sizes.ruler).min(rect.bottom());
        let lane_top = (rect.bottom() - sizes.lane).max(ruler_bottom);
        let time_top = (lane_top - sizes.ruler).max(ruler_bottom);
        let right = rect.right();
        let wave = Rect::from_min_max(pos2(split, ruler_bottom), pos2(right, time_top));
        let handles_bottom = (wave.top() + sizes.handle).min(wave.bottom());
        Self {
            controls: Rect::from_min_max(rect.left_top(), pos2(split, lane_top)),
            label: Rect::from_min_max(pos2(rect.left(), lane_top), pos2(split, rect.bottom())),
            ruler: Rect::from_min_max(pos2(split, rect.top()), pos2(right, ruler_bottom)),
            wave,
            handles: Rect::from_min_max(wave.left_top(), pos2(right, handles_bottom)),
            time: Rect::from_min_max(pos2(split, time_top), pos2(right, lane_top)),
            lane: Rect::from_min_max(pos2(split, lane_top), rect.right_bottom()),
            viewport: Viewport {
                width: wave.width(),
                ..viewport
            },
        }
    }

    fn x_of(&self, at: Beats) -> f32 {
        self.wave.left() + self.viewport.x_of(at)
    }

    /// Where song position `at`, in fractional beats, is across.
    fn x_of_beats(&self, at: f64) -> f32 {
        let from = at - self.viewport.start.to_f64();
        self.wave.left() + (from * self.viewport.zoom) as f32
    }

    fn beats_at(&self, x: f32) -> Beats {
        self.viewport.beats_at(x - self.wave.left())
    }

    /// The y coordinates the lane's line goes between, from the
    /// parameter's least to its most: clear of the lane's edges, so the
    /// points at either end show whole.
    fn travel(&self, theme: &Theme) -> (f32, f32) {
        let lane = self.lane;
        let margin = (theme.point + 2.0).min(lane.height() / 2.0);
        (lane.bottom() - margin, lane.top() + margin)
    }

    /// The y coordinate `unit` of the way up the lane's travel.
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

/// What the Sample Editor shows this frame, and where.
struct Scene<'s> {
    view: &'s View<'s>,
    theme: &'s Theme,
    mouse: &'s MouseMap,
    layout: Layout,
    /// The clip it shows, what it plays, and its track's place among the
    /// tracks, for its colour.
    clip: &'s Clip,
    audio: &'s AudioClip,
    index: usize,
    /// The file's sample rate, as the app read it, or else the engine's.
    rate: u32,
    /// The file's peaks, once the app has read them.
    peaks: Option<&'s Peaks>,
    /// Where the pointer is, if it's over the Sample Editor and nothing's
    /// in the way.
    pointer: Option<Pos2>,
}

impl Scene<'_> {
    fn project(&self) -> &Project {
        self.view.project
    }

    fn editor(&self) -> &Editor {
        self.view.editor
    }

    fn tempo(&self) -> &TempoMap {
        &self.project().tempo
    }

    fn warp(&self) -> Option<&Warp> {
        self.audio.warp.as_ref()
    }

    /// The warp's markers: none while the clip isn't warped.
    fn markers(&self) -> &[WarpMarker] {
        self.warp().map_or(&[], |warp| &warp.markers[..])
    }

    fn selected(&self, marker: &WarpMarker) -> bool {
        self.editor().sample_editor.markers.contains(&marker.beat)
    }

    /// The clip parameter whose curve the lane shows: the one chosen, or
    /// Gain while the clip isn't warped, as the others are its warp's.
    fn lane(&self) -> ClipParam {
        match self.warp() {
            Some(_) => self.editor().sample_editor.lane,
            None => ClipParam::Gain,
        }
    }

    /// The parameter the lane shows, as automation names it.
    fn param(&self) -> ParamAddress {
        ParamAddress::Clip(self.clip.id, self.lane())
    }

    fn spec(&self) -> ParamSpec {
        self.lane().spec()
    }

    /// Whether the clip's mode plays the parameter the lane shows.
    fn lane_plays(&self) -> bool {
        self.lane().plays_in(self.warp().map(|warp| warp.mode))
    }

    /// The curve's points, in the clip's own time.
    fn curve(&self) -> &[Point] {
        let lane = self.audio.lane(self.lane());
        lane.map_or(&[], |lane| &lane.points[..])
    }

    /// The parameter's own value, where it has no curve: the clip's gain,
    /// or its warp's setting.
    fn level(&self) -> f64 {
        let setting = self.warp().and_then(|warp| warp.param(self.lane()));
        setting.unwrap_or(f64::from(self.audio.gain))
    }

    /// `value`, to the nearest step for a parameter in steps.
    fn in_steps(&self, value: f64) -> f64 {
        match self.lane().stepped() {
            true => value.round(),
            false => value,
        }
    }

    /// The value the lane's name shows under it: what a drag on the lane,
    /// `drag`, which would do `plan`, moves a point or a line to or draws,
    /// or else the value of the point or the line under the pointer, or at
    /// its height.
    fn readout(&self, drag: Option<&Drag>, plan: Option<&Plan>) -> Option<f64> {
        let planned = |i: usize| match plan {
            Some(Plan::Points(points)) => points.get(i).map(|p| p.value),
            _ => None,
        };
        let curve = self.curve();
        let Some(drag) = drag else {
            let pos = (self.pointer).filter(|&pos| self.layout.lane.contains(pos))?;
            return Some(match self.envelope_part(pos) {
                Envelope::Point(i) => curve.get(i)?.value,
                Envelope::Line { .. } => self.line_value(self.own_of(self.layout.beats_at(pos.x))),
                Envelope::Lane => self.value_at(pos.y),
            });
        };
        match (drag.area, drag.action, drag.hit) {
            (
                Area::Lane,
                MouseAction::MovePoint | MouseAction::MovePointFree,
                Hit::Lane(Envelope::Point(i)),
            ) => planned(i).or(curve.get(i).map(|p| p.value)),
            (Area::Lane, MouseAction::MoveSegment, Hit::Lane(Envelope::Line { from, .. })) => {
                planned(from).or(Some(curve.get(from).map_or(self.level(), |p| p.value)))
            }
            (Area::Lane, MouseAction::Draw | MouseAction::DrawFree, _) => {
                Some(self.value_at(drag.to.y))
            }
            _ => None,
        }
    }

    /// The song position, in fractional beats, where `own`, a moment of the
    /// clip's own time, plays.
    fn song_of(&self, own: Beats) -> f64 {
        (self.clip).song_time_at(own.to_f64(), self.tempo(), self.rate)
    }

    /// The moment of the clip's own time that plays at song position `at`.
    fn own_of(&self, at: Beats) -> Beats {
        Beats::from_f64(self.clip.own_time_at(at, self.tempo(), self.rate))
    }

    /// Where a warp marker is across.
    fn marker_x(&self, marker: &WarpMarker) -> f32 {
        let at = warp::song_position(self.clip, self.audio, marker.beat);
        self.layout.x_of(at)
    }

    /// The frame of the file, fractional, that `audio`, the clip's audio or
    /// what a drag would make it, plays at `x`.
    fn frame_at(&self, audio: &AudioClip, x: f32) -> f64 {
        let at = self.layout.beats_at(x);
        audio.frame_at(
            self.clip.own_time_at(at, self.tempo(), self.rate),
            self.rate,
        )
    }

    /// Where the file's `frame`, fractional, plays across, as `audio`, the
    /// clip's audio or what a drag would make it, plays it.
    fn x_of_frame(&self, audio: &AudioClip, frame: f64) -> f32 {
        let own = audio.own_time_of(frame, self.rate);
        let at = self.clip.song_time_at(own, self.tempo(), self.rate);
        self.layout.x_of_beats(at)
    }

    /// How long the file is, once the app has read it.
    fn file_frames(&self) -> Option<u64> {
        let file = self.editor().files.get(&self.audio.file);
        file.map(|file| file.frames)
            .or(self.peaks.map(|peaks| peaks.frames()))
    }

    /// The transients of `audio`'s warp, the clip's or what a drag would
    /// make it, that play from `left` to `right` across, in order.
    fn transients<'a>(
        &self,
        audio: &'a AudioClip,
        left: f32,
        right: f32,
    ) -> impl Iterator<Item = &'a Onset> + use<'a> {
        let onsets = audio.warp.as_ref().and_then(|warp| warp.onsets.as_ref());
        let all = onsets.map_or(&[][..], |onsets| onsets.all());
        // Later frames play further right: the markers keep their order.
        let (first, last) = (self.frame_at(audio, left), self.frame_at(audio, right));
        let start = all.partition_point(|o| (o.frame as f64) < first);
        let end = all.partition_point(|o| (o.frame as f64) <= last).max(start);
        all[start..end]
            .iter()
            .filter(|o| o.origin != Origin::Hidden)
    }

    /// The clip's own time selected, from where to where across.
    fn time_selected(&self) -> Option<(f32, f32)> {
        let time = self.editor().sample_editor.time?;
        let x = |own: Beats| self.layout.x_of_beats(self.song_of(own));
        Some((x(time.start), x(time.end)))
    }

    /// `at`, snapped to the grid unless `free` or snapping is off.
    fn snap(&self, at: Beats, free: bool) -> Beats {
        if free {
            return at;
        }
        let zoom = self.layout.viewport.zoom;
        self.editor().grid.snap(self.tempo(), at, zoom)
    }

    /// The song position at `x`, snapped to the grid.
    fn snapped(&self, x: f32) -> Beats {
        self.snap(self.layout.beats_at(x), false).max(Beats::ZERO)
    }

    /// The lane's value at `y`.
    fn value_at(&self, y: f32) -> f64 {
        let spec = self.spec();
        let unit = self.layout.unit_at(self.theme, y);
        self.in_steps(spec.scale.from_unit(unit, spec.min, spec.max))
    }

    /// Where `value` is drawn on the lane.
    fn y_of(&self, value: f64) -> f32 {
        let spec = self.spec();
        let unit = spec.scale.to_unit(value, spec.min, spec.max);
        self.layout.y_of(self.theme, unit)
    }

    /// Where `point`, of the curve, is drawn.
    fn point_at(&self, point: &Point) -> Pos2 {
        pos2(
            self.layout.x_of_beats(self.song_of(point.at)),
            self.y_of(point.value),
        )
    }

    /// The curve's value at `own`, in the clip's own time, or else the
    /// parameter's own value.
    fn line_value(&self, own: Beats) -> f64 {
        let lane = self.audio.lane(self.lane());
        lane.map_or(self.level(), |lane| lane.value_at(own))
    }

    /// What's under `pos` in `area`.
    fn hit(&self, area: Area, pos: Pos2) -> Hit {
        match area {
            Area::Ruler | Area::Time => Hit::Ruler,
            Area::Wave => self.hit_wave(pos),
            Area::Lane => Hit::Lane(self.envelope_part(pos)),
        }
    }

    fn hit_wave(&self, pos: Pos2) -> Hit {
        let layout = &self.layout;
        let in_handles = layout.handles.contains(pos);
        if !in_handles && pos.y >= layout.wave.center().y {
            return Hit::Wave;
        }
        // The nearest marker within reach: wider on the handles.
        let sizes = self.theme.sample_editor;
        let reach = match in_handles {
            true => sizes.marker_grab.max(sizes.handle / 2.0),
            false => sizes.marker_grab,
        };
        let near = (self.markers().iter().enumerate())
            .map(|(i, marker)| ((self.marker_x(marker) - pos.x).abs(), i))
            .filter(|&(distance, _)| distance <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, i)) = near {
            return Hit::Marker(i);
        }
        // Away from them, the nearest transient within reach.
        let audio = self.audio;
        let near = (self.transients(audio, pos.x - reach, pos.x + reach))
            .map(|o| {
                (
                    (self.x_of_frame(audio, o.frame as f64) - pos.x).abs(),
                    o.frame,
                )
            })
            .filter(|&(distance, _)| distance <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match near {
            Some((_, frame)) => Hit::Transient(frame),
            None => Hit::WaveTop,
        }
    }

    /// What's under `pos` on the lane: one of its points, the line
    /// through them, or neither.
    fn envelope_part(&self, pos: Pos2) -> Envelope {
        let layout = &self.layout;
        let grab = self.theme.point_grab;
        let points = self.curve();
        let Some(end) = points.len().checked_sub(1) else {
            let level = self.y_of(self.level());
            return match (level - pos.y).abs() <= grab {
                true => Envelope::Line { from: 0, to: 0 },
                false => Envelope::Lane,
            };
        };
        let at: Vec<Pos2> = points.iter().map(|p| self.point_at(p)).collect();
        let nearest = (at.iter().enumerate())
            .map(|(i, at)| (at.distance(pos), i))
            .filter(|&(distance, _)| distance <= grab)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, i)) = nearest {
            return Envelope::Point(i);
        }
        // The lines between the points, and the level lines before the
        // first and after the last. In steps, each point's value holds
        // until the next, and the jumps between aren't lines to move.
        let mut lines = vec![(pos2(layout.lane.left(), at[0].y), at[0], 0, 0)];
        lines.extend((0..end).map(|i| match self.lane().stepped() {
            true => (at[i], pos2(at[i + 1].x, at[i].y), i, i),
            false => (at[i], at[i + 1], i, i + 1),
        }));
        lines.push((at[end], pos2(layout.lane.right(), at[end].y), end, end));
        lines
            .into_iter()
            .map(|(a, b, from, to)| (distance_to_segment(pos, a, b), from, to))
            .filter(|&(distance, ..)| distance <= grab)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Envelope::Lane, |(_, from, to)| Envelope::Line { from, to })
    }

    /// Whether `drag` draws a step of the grid at a time.
    fn draws_steps(&self, drag: &Drag) -> bool {
        drag.action == MouseAction::Draw && self.editor().grid.snap
    }

    /// Draws on the lane from where the pointer last drew to where it is
    /// now.
    fn draw(&self, drag: &mut Drag) {
        let layout = &self.layout;
        let (from, to) = (drag.last.unwrap_or(drag.to), drag.to);
        // The value along the way from `from` to `to`, at `x`.
        let value = |x: f32| {
            let along = match to.x - from.x {
                0.0 => 1.0,
                across => ((x - from.x) / across).clamp(0.0, 1.0),
            };
            self.value_at(from.y + (to.y - from.y) * along)
        };
        let lane = layout.lane;
        let beats = |x: f32| layout.beats_at(x.clamp(lane.left(), lane.right()));
        let (left, right) = (from.x.min(to.x), from.x.max(to.x));
        let steps = self.draws_steps(drag);
        let values = &mut drag.drawn;
        if steps {
            // Each step crossed takes the value where the pointer crossed
            // its middle, or came nearest it.
            let end = beats(right);
            let mut at = beats(left);
            let zoom = layout.viewport.zoom;
            loop {
                let cell = self.editor().grid.cell(self.tempo(), at, zoom);
                let middle = (layout.x_of(cell.start) + layout.x_of(cell.end)) / 2.0;
                values.insert(cell.start, value(middle.clamp(left, right)));
                if cell.end > end || cell.end <= at {
                    break;
                }
                at = cell.end;
            }
        } else {
            // What was drawn between goes, so going back draws over it.
            let (start, end) = (beats(left), beats(right));
            values.retain(|&at, _| at <= start || at >= end);
            values.insert(beats(from.x), value(from.x));
            values.insert(beats(to.x), value(to.x));
        }
        drag.last = Some(to);
    }

    /// The curve once what `drag` drew takes the place of what was there,
    /// in the clip's own time: a step at either end of it, from the line as
    /// it was to what was drawn and back.
    fn drawn_points(&self, drag: &Drag) -> Option<Vec<Point>> {
        let steps = self.draws_steps(drag);
        let (&start, _) = drag.drawn.first_key_value()?;
        let (&last, _) = drag.drawn.last_key_value()?;
        let zoom = self.layout.viewport.zoom;
        let end = match steps {
            true => self.editor().grid.cell(self.tempo(), last, zoom).end,
            false => last,
        };
        let own = |at: Beats| self.own_of(at);
        let (start_own, end_own) = (own(start), own(end));
        let old = self.curve();
        let lane = self.audio.lane(self.lane());
        let before = lane.map_or(self.level(), |lane| lane.value_before(start_own));
        let after = self.line_value(end_own);
        let mut points: Vec<Point> = old.iter().filter(|p| p.at < start_own).copied().collect();
        points.push(Point::new(start_own, before));
        let mut values = drag.drawn.iter().peekable();
        while let Some((&at, &value)) = values.next() {
            points.push(Point::new(own(at), value));
            if steps {
                let next = values.peek().map_or(end, |&(&next, _)| next);
                points.push(Point::new(own(next), value));
            }
        }
        points.push(Point::new(end_own, after));
        points.extend(old.iter().filter(|p| p.at > end_own));
        Some(simplified(points))
    }

    /// What `drag` would do if it ended now.
    fn plan(&self, drag: &Drag) -> Option<Plan> {
        let layout = &self.layout;
        let moved = layout.viewport.beats_in(drag.to.x - drag.from.x);
        match (drag.action, drag.hit) {
            (MouseAction::MoveMarker | MouseAction::MoveMarkerFree, Hit::Marker(i)) => {
                let warp = self.warp()?;
                let grabbed = warp::song_position(self.clip, self.audio, warp.markers.get(i)?.beat);
                let free = drag.action == MouseAction::MoveMarkerFree;
                let by = self.snap(grabbed + moved, free) - grabbed;
                let by = warp.markers_can_move(&drag.markers, by);
                (by != Beats::ZERO).then(|| Plan::Markers {
                    markers: drag.markers.clone(),
                    by,
                })
            }
            (MouseAction::MoveAudio, Hit::Marker(i)) => {
                // The audio goes with the pointer: the frame that played
                // as far before the marker as the pointer went plays at it.
                let warp = self.warp()?;
                let marker = *warp.markers.get(i)?;
                let beats = f64::from(drag.to.x - drag.from.x) / layout.viewport.zoom;
                let frame = warp
                    .frame_at(marker.beat.to_f64() - beats, self.rate)
                    .round();
                let after = i
                    .checked_sub(1)
                    .map_or(0, |before| warp.markers[before].frame + 1);
                let before = warp
                    .markers
                    .get(i + 1)
                    .map_or(u64::MAX, |next| next.frame - 1);
                let frame = (frame.max(0.0) as u64).clamp(after, before.max(after));
                (frame != marker.frame).then_some(Plan::Audio { marker: i, frame })
            }
            (MouseAction::MoveMarker | MouseAction::MoveMarkerFree, Hit::Transient(frame)) => {
                // A marker at the transient, moved as a marker is.
                let mut warp = self.warp()?.clone();
                let i = warp.insert_at_frame(frame, self.rate)?;
                let grabbed = warp::song_position(self.clip, self.audio, warp.markers[i].beat);
                let free = drag.action == MouseAction::MoveMarkerFree;
                let by = self.snap(grabbed + moved, free) - grabbed;
                let by = warp.markers_can_move(&[i], by);
                (by != Beats::ZERO).then_some(Plan::Pseudo { frame, by })
            }
            (MouseAction::MoveTransient, Hit::Transient(frame)) => {
                // It goes with the pointer, between the transients either
                // side, in the file.
                let (before, after) = self.warp()?.onsets.as_ref()?.room(frame)?;
                let x = self.x_of_frame(self.audio, frame as f64) + drag.to.x - drag.from.x;
                let to = self.frame_at(self.audio, x).round().max(0.0) as u64;
                let low = before.map_or(0, |before| before + 1);
                let end = self
                    .file_frames()
                    .map_or(u64::MAX, |frames| frames.saturating_sub(1));
                let high = after.map_or(end, |after| (after - 1).min(end));
                let to = to.clamp(low, high.max(low));
                (to != frame).then_some(Plan::Transient { frame, to })
            }
            (MouseAction::SelectTime, Hit::WaveTop | Hit::Wave | Hit::Transient(_)) => {
                let (start, end) = (self.snapped(drag.from.x), self.snapped(drag.to.x));
                Some(Plan::Time {
                    start: start.min(end),
                    end: start.max(end),
                })
            }
            (
                MouseAction::MovePoint | MouseAction::MovePointFree,
                Hit::Lane(Envelope::Point(i)),
            ) => {
                let old = self.curve();
                let point = *old.get(i)?;
                let free = drag.action == MouseAction::MovePointFree;
                // It stays between the points either side.
                let song = Beats::from_f64(self.song_of(point.at));
                let mut at = self.own_of(self.snap(song + moved, free).max(Beats::ZERO));
                if let Some(before) = i.checked_sub(1) {
                    at = at.max(old[before].at);
                }
                if let Some(next) = old.get(i + 1) {
                    at = at.min(next.at);
                }
                let spec = self.spec();
                let unit = spec.scale.to_unit(point.value, spec.min, spec.max)
                    + layout.units_in(self.theme, drag.from.y - drag.to.y);
                let value = self.in_steps(spec.scale.from_unit(unit, spec.min, spec.max));
                let mut points = old.to_vec();
                points[i] = Point::new(at, value);
                (points != old).then_some(Plan::Points(points))
            }
            (MouseAction::MoveSegment, Hit::Lane(Envelope::Line { from, to })) => {
                let old = self.curve();
                let spec = self.spec();
                let up = layout.units_in(self.theme, drag.from.y - drag.to.y);
                let mut points = old.to_vec();
                for point in points.get_mut(from..=to)? {
                    let unit = spec.scale.to_unit(point.value, spec.min, spec.max) + up;
                    point.value = self.in_steps(spec.scale.from_unit(unit, spec.min, spec.max));
                }
                (points != old).then_some(Plan::Points(points))
            }
            (MouseAction::Draw | MouseAction::DrawFree, Hit::Lane(_)) => {
                let points = self.drawn_points(drag)?;
                (points != self.curve()).then_some(Plan::Points(points))
            }
            (MouseAction::ScrubZoom, Hit::Ruler) => {
                let mut viewport = drag.viewport;
                let left = layout.wave.left();
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

    /// The clip's audio as `plan` would leave its warp, for drawing while a
    /// drag moves markers, the audio under them or transients.
    fn planned(&self, plan: Option<&Plan>) -> Option<AudioClip> {
        let mut warp = self.warp()?.clone();
        let moved = match plan? {
            Plan::Markers { markers, by } => warp.move_markers(markers, *by),
            &Plan::Audio { marker, frame } => warp.move_audio(marker, frame),
            &Plan::Pseudo { frame, by } => (warp.insert_at_frame(frame, self.rate))
                .is_some_and(|i| warp.move_markers(&[i], by)),
            &Plan::Transient { frame, to } => {
                (warp.onsets.as_mut()).is_some_and(|onsets| onsets.move_to(frame, to))
            }
            _ => false,
        };
        moved.then(|| AudioClip {
            warp: Some(warp),
            ..self.audio.clone()
        })
    }
}

/// Asks the app to run the command `id` with `args`.
fn run(actions: &mut Vec<Action>, id: CommandId, args: Args) {
    actions.push(Action::RunWith(id, args));
}

/// The arguments that name the clip the Sample Editor shows, and the
/// markers at `markers` in its warp, by their beats.
fn of(scene: &Scene<'_>, markers: &[usize]) -> Args {
    let all = scene.markers();
    let beats: Vec<Beats> = markers
        .iter()
        .filter_map(|&i| all.get(i))
        .map(|m| m.beat)
        .collect();
    Args::new()
        .with("markers", beats)
        .with("clip", scene.clip.id)
}

/// Asks the app for `plan`.
fn send(actions: &mut Vec<Action>, scene: &Scene<'_>, plan: &Plan) {
    match plan {
        Plan::Markers { markers, by } => {
            run(
                actions,
                warp::MOVE_MARKERS,
                of(scene, markers).with("by", *by),
            );
        }
        &Plan::Audio { marker, frame } => {
            let Some(at) = scene.markers().get(marker) else {
                return;
            };
            let args = Args::new()
                .with("marker", at.beat)
                .with("frame", frame as i64)
                .with("clip", scene.clip.id);
            run(actions, warp::MOVE_AUDIO, args);
        }
        &Plan::Pseudo { frame, by } => {
            let Some(warp) = scene.warp() else {
                return;
            };
            // Where Insert Warp Marker puts it, pinning the frame where it
            // plays.
            let beat = Beats::from_f64(warp.beat_at(frame as f64, scene.rate));
            let clip = scene.clip.id;
            let insert = Args::new()
                .with("frames", vec![frame as i64])
                .with("clip", clip);
            let moving = Args::new()
                .with("markers", vec![beat])
                .with("by", by)
                .with("clip", clip);
            actions.push(Action::RunBatch {
                name: "Insert Warp Marker".to_owned(),
                commands: vec![(warp::INSERT_MARKERS, insert), (warp::MOVE_MARKERS, moving)],
            });
        }
        &Plan::Transient { frame, to } => {
            let args = Args::new()
                .with("frame", frame as i64)
                .with("to", to as i64)
                .with("clip", scene.clip.id);
            run(actions, transient::MOVE, args);
        }
        &Plan::Time { start, end } => {
            let args = Args::new()
                .with("start", scene.own_of(start))
                .with("end", scene.own_of(end))
                .with("clip", scene.clip.id);
            run(actions, warp::SELECT_TIME, args);
        }
        Plan::Points(points) => set_points(actions, scene, points),
        Plan::Viewport(viewport) => set_viewport(actions, viewport),
    }
}

/// Asks the app to give the curve the lane shows `points`, or to clear it
/// without them, which keeps the value it had at the playhead.
fn set_points(actions: &mut Vec<Action>, scene: &Scene<'_>, points: &[Point]) {
    let param = scene.param();
    if points.is_empty() {
        run(actions, automation::CLEAR, Args::new().with("param", param));
        return;
    }
    let at: Vec<Beats> = points.iter().map(|p| p.at).collect();
    let values: Vec<f64> = points.iter().map(|p| p.value).collect();
    let args = Args::new()
        .with("param", param)
        .with("at", at)
        .with("values", values);
    run(actions, automation::SET_POINTS, args);
}

fn set_viewport(actions: &mut Vec<Action>, viewport: &Viewport) {
    let args = Args::new()
        .with("start", viewport.start)
        .with("zoom", viewport.zoom)
        .with("width", viewport.width);
    run(actions, views::SET_SAMPLE_EDITOR_VIEWPORT, args);
}

/// How the lane names `value` of `param`: "-6.0 dB", "+3.5 st", "1/8".
fn value_text(param: ClipParam, value: f64) -> String {
    match param {
        ClipParam::Gain => decibels(value as f32),
        ClipParam::Transpose => format!("{value:+.1} st"),
        ClipParam::Detune => format!("{value:+.0} ct"),
        ClipParam::TransientEnvelope => format!("{value:.0}"),
        ClipParam::Preserve => Preserve::at_step(value).name().to_owned(),
        ClipParam::TransientLoop => TransientLoop::at_step(value).name().to_owned(),
        ClipParam::Formants => format!("{value:.0}%"),
    }
}

/// A gain in decibels, for the gain field: silence at the floor.
fn gain_to_db(gain: f32) -> f64 {
    match gain > 0.0 {
        true => (20.0 * f64::from(gain).log10()).max(GAIN_FLOOR),
        false => GAIN_FLOOR,
    }
}

fn db_to_gain(db: f64) -> f64 {
    match db <= GAIN_FLOOR {
        true => 0.0,
        false => 10f64.powf(db / 20.0),
    }
}

impl SampleEditor {
    /// Shows the Sample Editor in the rest of `ui`, and adds what the user
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
        let shown = editor.sample_editor.clip.and_then(|id| {
            let (track, clip) = view.project.find_clip(id)?;
            let ClipContent::Audio(audio) = &clip.content else {
                return None;
            };
            Some((clip, audio, view.project.track_index(track.id)?))
        });
        let Some((clip, audio, index)) = shown else {
            self.drag = None;
            paint::empty(&ui.painter_at(rect), rect, theme);
            let id = ui.scope_id().with("sample editor");
            let response = ui.interact(rect, id, Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Sample Editor"));
            return;
        };
        let viewport = editor.sample_editor.viewport;
        let layout = Layout::new(rect, theme, viewport);
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|&pos| rect.contains(pos) && ui.rect_contains_pointer(rect));
        let mut scene = Scene {
            view,
            theme,
            mouse,
            layout,
            clip,
            audio,
            index,
            rate: editor.sample_rate_of(&audio.file),
            peaks: view.waveforms.get(&audio.file).map(|peaks| &**peaks),
            pointer,
        };
        let mut viewport = scene.layout.viewport;
        let id = ui.scope_id().with("sample editor");
        // The lane is named for what it shows: "Gain Lane".
        let lane = format!("{} Lane", scene.spec().name);
        let areas = [
            (scene.layout.ruler, Area::Ruler, "Sample Editor Ruler"),
            (scene.layout.wave, Area::Wave, "Sample Editor"),
            (scene.layout.time, Area::Time, "File Time"),
            (scene.layout.lane, Area::Lane, lane.as_str()),
        ];
        let responses: Vec<(Response, Area)> = areas
            .into_iter()
            .map(|(rect, area, name)| {
                let response = ui.interact(rect, id.with(area), Sense::click_and_drag());
                response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, name));
                (response, area)
            })
            .collect();
        // While the clip's warped, a click on the lane's name chooses what
        // it shows.
        let chooser = (scene.warp().is_some())
            .then(|| ui.interact(scene.layout.label, id.with("lane"), Sense::click()));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.drag = None;
        }
        for (response, area) in &responses {
            self.respond(ui, &scene, response, *area, actions);
        }
        self.wheel(ui, &scene, &mut viewport);
        self.follow(&scene, &mut viewport);
        if viewport != editor.sample_editor.viewport {
            set_viewport(actions, &viewport);
            scene.layout = Layout::new(rect, theme, viewport);
        }
        let plan = self.drag.as_ref().and_then(|drag| scene.plan(drag));
        self.cursor(ui, &scene);
        let pseudo = self.pseudo(&scene, plan.as_ref());

        let painter = ui.painter_at(rect);
        let layout = &scene.layout;
        paint::ruler(&painter.with_clip_rect(layout.ruler), &scene);
        let wave = painter.with_clip_rect(layout.wave);
        paint::wave(&wave, &scene, plan.as_ref(), pseudo);
        paint::time(&painter.with_clip_rect(layout.time), &scene, plan.as_ref());
        paint::lane(&painter.with_clip_rect(layout.lane), &scene, plan.as_ref());
        let label = painter.with_clip_rect(layout.label);
        let readout = scene.readout(self.drag.as_ref(), plan.as_ref());
        paint::label(&label, &scene, chooser.as_ref(), readout);
        if let Some(chooser) = &chooser {
            menu::lanes(chooser, &scene, actions);
        }
        paint::column(&painter.with_clip_rect(layout.controls), &scene);
        controls(ui, &scene, actions);
        name_for_screen_readers(ui, &scene, id);
        if let Some((response, _)) = responses.iter().find(|(_, area)| *area == Area::Wave) {
            self.context_menu(&scene, response, actions);
        }
    }

    /// Where to show a pseudo marker, by the frame of the file: at the
    /// transient under the pointer, or at the one a drag takes, until it
    /// becomes a warp marker.
    fn pseudo(&self, scene: &Scene<'_>, plan: Option<&Plan>) -> Option<u64> {
        match (&self.drag, plan) {
            (Some(_), Some(&Plan::Transient { to, .. })) => Some(to),
            (Some(_), Some(Plan::Pseudo { .. })) => None,
            (Some(drag), _) => match drag.hit {
                Hit::Transient(frame) => Some(frame),
                _ => None,
            },
            (None, _) => {
                let pos = (scene.pointer).filter(|&pos| scene.layout.wave.contains(pos))?;
                match scene.hit(Area::Wave, pos) {
                    Hit::Transient(frame) => Some(frame),
                    _ => None,
                }
            }
        }
    }

    /// Opens the waveform's context menu where it's secondary-clicked, as
    /// its `response` says, and shows it while it's open.
    fn context_menu(&mut self, scene: &Scene<'_>, response: &Response, actions: &mut Vec<Action>) {
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let frame = scene.frame_at(scene.audio, pos.x).round();
            let spot = Spot {
                hit: scene.hit(Area::Wave, pos),
                at: scene.snapped(pos.x),
                own: scene.own_of(scene.layout.beats_at(pos.x)),
                frame: (frame >= 0.0).then_some(frame as u64),
            };
            self.menu = Some(spot);
            focus(scene, actions);
            menu::select_for(scene, spot, actions);
        }
        response.context_menu(|ui| {
            if let Some(spot) = self.menu {
                menu::show(ui, scene, spot, actions);
            }
        });
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
            if matches!(drag.action, MouseAction::Draw | MouseAction::DrawFree) {
                scene.draw(drag);
            }
            // The scrolling and the time selected change as the drag goes.
            if matches!(
                drag.action,
                MouseAction::ScrubZoom | MouseAction::SelectTime
            ) {
                let plan = scene.plan(drag);
                if plan != drag.sent
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
            markers: Vec::new(),
            viewport: scene.layout.viewport,
            sent: None,
            drawn: BTreeMap::new(),
            last: None,
        };
        if let (Hit::Marker(i), MouseAction::MoveMarker | MouseAction::MoveMarkerFree) =
            (hit, action)
        {
            // A marker that isn't selected moves alone, and is selected.
            let markers = scene.markers();
            drag.markers = match markers.get(i).is_some_and(|m| scene.selected(m)) {
                true => (markers.iter().enumerate())
                    .filter(|(_, m)| scene.selected(m))
                    .map(|(i, _)| i)
                    .collect(),
                false => {
                    run(actions, warp::SELECT_MARKERS, of(scene, &[i]));
                    vec![i]
                }
            };
        }
        if matches!(action, MouseAction::Draw | MouseAction::DrawFree) {
            scene.draw(&mut drag);
        }
        self.drag = Some(drag);
    }

    /// Scrolls and zooms with the wheel, over the Sample Editor.
    fn wheel(&mut self, ui: &Ui, scene: &Scene<'_>, viewport: &mut Viewport) {
        let layout = &scene.layout;
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let parts = [layout.ruler, layout.wave, layout.time, layout.lane];
        if !(parts.iter()).any(|rect| rect.contains(pos) && ui.rect_contains_pointer(*rect)) {
            return;
        }
        let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
        let x = pos.x.clamp(layout.wave.left(), layout.wave.right());
        let at = layout.beats_at(x);
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
                MouseWheelUnit::Page => delta * layout.wave.width(),
            };
            let mods = modifiers(mods);
            match scene.mouse.action(Target::Timeline, Gesture::Wheel, mods) {
                // Nothing to scroll up and down: either way goes sideways.
                Some(MouseAction::Scroll | MouseAction::ScrollSideways) => {
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

    /// Shows what a press there would do: markers and points move.
    fn cursor(&self, ui: &Ui, scene: &Scene<'_>) {
        let icon = |action: Option<MouseAction>| match action {
            Some(
                MouseAction::MoveMarker
                | MouseAction::MoveMarkerFree
                | MouseAction::MoveAudio
                | MouseAction::MoveTransient,
            ) => Some(CursorIcon::ResizeHorizontal),
            Some(
                MouseAction::MovePoint | MouseAction::MovePointFree | MouseAction::MoveSegment,
            ) => Some(CursorIcon::Grab),
            Some(MouseAction::Draw | MouseAction::DrawFree) => Some(CursorIcon::Crosshair),
            _ => None,
        };
        let shown = match &self.drag {
            Some(drag) => icon(Some(drag.action)),
            None => {
                let Some(pos) = scene.pointer else {
                    return;
                };
                let layout = &scene.layout;
                let areas = [(layout.wave, Area::Wave), (layout.lane, Area::Lane)];
                let Some((_, area)) = areas.into_iter().find(|(rect, _)| rect.contains(pos)) else {
                    return;
                };
                let hit = scene.hit(area, pos);
                let mods = modifiers(ui.input(|i| i.modifiers));
                icon(scene.mouse.action(hit.target(), Gesture::Drag, mods))
            }
        };
        if let Some(icon) = shown {
            ui.ctx().set_cursor_icon(icon);
        }
    }
}

/// Gives the Sample Editor the focus, so the keys and the edit commands act
/// on its markers, if it hasn't got it.
fn focus(scene: &Scene<'_>, actions: &mut Vec<Action>) {
    if scene.editor().focus != Focus::SAMPLE_EDITOR {
        let args = Args::new().with("panel", "sample_editor");
        run(actions, views::SET_FOCUS, args);
    }
}

fn drag_stopped(scene: &Scene<'_>, drag: &Drag, actions: &mut Vec<Action>) {
    let Some(plan) = scene.plan(drag) else {
        return;
    };
    match &plan {
        // Asked for as the drag went.
        Plan::Viewport(_) | Plan::Time { .. } => {
            if drag.sent.as_ref() != Some(&plan) {
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
    let Some(action) = scene.mouse.action(hit.target(), gesture, mods) else {
        return false;
    };
    let at = scene.snapped(pos.x);
    let warped = scene.warp().is_some();
    match (action, hit) {
        (MouseAction::Select | MouseAction::ToggleSelect, Hit::Marker(i)) => {
            let toggle = action == MouseAction::ToggleSelect;
            let args = of(scene, &[i]).with("toggle", toggle);
            run(actions, warp::SELECT_MARKERS, args);
        }
        (MouseAction::DeleteMarker, Hit::Marker(i)) => {
            run(actions, warp::DELETE_MARKERS, of(scene, &[i]));
        }
        (MouseAction::AddMarker, Hit::Transient(frame)) => {
            // On the transient, pinning it where it plays.
            let args = Args::new()
                .with("frames", vec![frame as i64])
                .with("clip", scene.clip.id);
            run(actions, warp::INSERT_MARKERS, args);
        }
        (MouseAction::AddMarker, Hit::WaveTop | Hit::Wave) if warped => {
            let args = Args::new()
                .with("at", vec![scene.own_of(at)])
                .with("clip", scene.clip.id);
            run(actions, warp::INSERT_MARKERS, args);
        }
        (MouseAction::PlaceMarker, Hit::Marker(i)) => {
            let Some(marker) = scene.markers().get(i) else {
                return false;
            };
            let at = warp::song_position(scene.clip, scene.audio, marker.beat);
            let args = Args::new().with("at", at.max(Beats::ZERO));
            run(actions, transport::SET_START_MARKER, args);
        }
        (MouseAction::PlaceMarker, _) => {
            if warped {
                run(actions, warp::SELECT_MARKERS, of(scene, &[]));
            }
            run(
                actions,
                transport::SET_START_MARKER,
                Args::new().with("at", at),
            );
        }
        (MouseAction::Locate, _) => {
            run(actions, transport::LOCATE, Args::new().with("at", at));
        }
        (MouseAction::AddPoint, Hit::Lane(part)) => {
            let own = scene.own_of(at);
            // On the line, a point on it, which changes nothing until it's
            // moved.
            let value = match part {
                Envelope::Line { .. } => scene.line_value(own),
                _ => scene.value_at(pos.y),
            };
            let args = Args::new()
                .with("param", scene.param())
                .with("at", own)
                .with("value", value);
            run(actions, automation::ADD_POINT, args);
        }
        (MouseAction::DeletePoint, Hit::Lane(Envelope::Point(i))) => {
            let mut points = scene.curve().to_vec();
            if i >= points.len() {
                return false;
            }
            points.remove(i);
            set_points(actions, scene, &points);
        }
        _ => return false,
    }
    true
}

/// The clip's controls, in the column beside the waveform, as Live's: Warp
/// and its mode, the mode's own settings, its tempo, with Live's ×2 and ÷2,
/// its transpose and detune, and its gain. An unwarped clip's tempo is the
/// one Warp would warp it at, greyed, and so are Re-Pitch's transpose and
/// detune, as its pitch follows its speed. Where the column is too short
/// for them all, it scrolls.
fn controls(ui: &mut Ui, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let theme = scene.theme;
    let column = scene.layout.controls.shrink(theme.padding);
    ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
        ui.spacing_mut().item_spacing.y = theme.padding;
        // Under the clip's name.
        ui.add_space(theme.sample_editor.ruler);
        ScrollArea::vertical()
            .id_salt("sample editor controls")
            .show(ui, |ui| {
                warp_and_mode(ui, scene, actions);
                match scene.warp() {
                    Some(warp) => {
                        match warp.mode {
                            WarpMode::RePitch => {}
                            WarpMode::Slice => slice(ui, scene, warp, actions),
                            WarpMode::Spectral => spectral(ui, scene, warp, actions),
                        }
                        tempo(ui, scene, warp, actions);
                        pitch(ui, scene, warp, actions);
                    }
                    None => {
                        let (project, editor) = (scene.project(), scene.editor());
                        let mut bpm =
                            auto_warp::tempo_guess(project, editor, scene.clip, scene.audio);
                        let field = DragValue::new(&mut bpm).fixed_decimals(2).suffix(" BPM");
                        ui.add_enabled(false, field).on_disabled_hover_text(
                            "The tempo Warp warps the clip at: its file's, as found",
                        );
                    }
                }
                gain(ui, scene, actions);
            });
    });
}

/// Warp, and while the clip's warped, the menu of its modes.
fn warp_and_mode(ui: &mut Ui, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    let warped = scene.warp().is_some();
    ui.horizontal(|ui| {
        let hint = "Whether the clip follows the song's tempo";
        let warp_button = ui
            .add(Button::new("Warp").selected(warped))
            .on_hover_text(hint);
        if warp_button.clicked() {
            let args = Args::new().with("on", !warped).with("clip", clip);
            run(actions, warp::SET_WARP, args);
        }
        let Some(warp) = scene.warp() else {
            return;
        };
        let hint = "How the audio between the markers plays: Re-Pitch resamples it, so its \
                    pitch follows its speed, as on a turntable, Slice cuts it into slices at \
                    its transients, each played at the file's own speed, for drums and other \
                    percussive loops, and Spectral stretches it keeping its pitch, for full \
                    mixes, chords and voices";
        let (shown, modes) = (RichText::new(warp.mode.name()), &WarpMode::ALL);
        if let Some(mode) = menu(
            ui,
            "Warp Mode",
            shown,
            modes,
            warp.mode,
            WarpMode::name,
            hint,
        ) {
            let args = Args::new().with("mode", mode.key()).with("clip", clip);
            run(actions, warp_mode::SET_MODE, args);
        }
    });
}

/// Slice's own settings: where it cuts the audio, what a slice plays where
/// its audio runs out, and how much each slice fades.
fn slice(ui: &mut Ui, scene: &Scene<'_>, warp: &Warp, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    let settings = warp.slice;
    let small = |text: String| RichText::new(text).size(scene.theme.small_text);
    let preserve = settings.preserve;
    let shown = small(format!("Preserve: {}", preserve.name()));
    let hint = "Where Slice cuts the audio: at its transients, or on a grid of notes from the \
                clip's 1.1.1";
    let (options, name) = (&Preserve::ALL, Preserve::name);
    if let Some(picked) = menu(ui, "Preserve", shown, options, preserve, name, hint) {
        let args = Args::new()
            .with("preserve", picked.key())
            .with("clip", clip);
        run(actions, warp_mode::SET_PRESERVE, args);
    }
    let looping = settings.transient_loop;
    let shown = small(format!("Loop: {}", looping.name()));
    let hint = "Transient Loop Mode: what a slice plays where its audio runs out before the \
                next starts, as where the song plays the file slower than its own speed";
    let (options, name) = (&TransientLoop::ALL, TransientLoop::name);
    if let Some(picked) = menu(
        ui,
        "Transient Loop Mode",
        shown,
        options,
        looping,
        name,
        hint,
    ) {
        let args = Args::new().with("mode", picked.key()).with("clip", clip);
        run(actions, warp_mode::SET_TRANSIENT_LOOP, args);
    }
    ui.horizontal(|ui| {
        ui.label("Envelope");
        let mut envelope = settings.envelope;
        let field = ui
            .add(
                DragValue::new(&mut envelope)
                    .speed(0.5)
                    .range(0.0..=SliceSettings::ENVELOPE_MAX)
                    .fixed_decimals(0)
                    .update_while_editing(false),
            )
            .on_hover_text(
                "Transient Envelope: how much each slice fades as it plays, from 0, a very \
                 quick decay, to 100, no fade",
            );
        name_field(&field, "Transient Envelope", envelope);
        edited(&field, actions, warp_mode::SET_TRANSIENT_ENVELOPE, || {
            Args::new().with("envelope", envelope).with("clip", clip)
        });
    });
}

/// Spectral's own setting: how far it keeps the formants in place as it
/// transposes.
fn spectral(ui: &mut Ui, scene: &Scene<'_>, warp: &Warp, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    ui.horizontal(|ui| {
        ui.label("Formants");
        let mut formants = warp.spectral.formants;
        let field = ui
            .add(
                DragValue::new(&mut formants)
                    .speed(0.5)
                    .range(0.0..=SpectralSettings::FORMANTS_MAX)
                    .fixed_decimals(0)
                    .suffix("%")
                    .update_while_editing(false),
            )
            .on_hover_text(
                "How far the formants, which give a voice its character, stay in place as \
                 the clip transposes: at 100% a voice moved up or down sounds like the same \
                 voice, and at 0% they move with the pitch, as a record's do played faster",
            );
        name_field(&field, "Formants", formants);
        edited(&field, actions, warp_mode::SET_FORMANTS, || {
            Args::new().with("formants", formants).with("clip", clip)
        });
    });
}

/// The clip's tempo, to drag or type, with ÷2 and ×2.
fn tempo(ui: &mut Ui, scene: &Scene<'_>, warp: &Warp, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    let mut bpm = warp.clip_tempo(scene.rate);
    let field = ui
        .add(
            DragValue::new(&mut bpm)
                .speed(0.1)
                .range(Warp::MIN_TEMPO..=Warp::MAX_TEMPO)
                .fixed_decimals(2)
                .suffix(" BPM")
                .update_while_editing(false),
        )
        .on_hover_text("The clip's tempo: drag, or click and type");
    name_field(&field, "Clip Tempo", bpm);
    edited(&field, actions, warp::SET_TEMPO, || {
        Args::new().with("bpm", bpm).with("clip", clip)
    });
    ui.horizontal(|ui| {
        let halve = ui.small_button("\u{f7}2").on_hover_text("Halve the tempo");
        if halve.clicked() {
            actions.push(Action::Run(warp::HALVE_TEMPO));
        }
        let double = ui.small_button("\u{d7}2").on_hover_text("Double the tempo");
        if double.clicked() {
            actions.push(Action::Run(warp::DOUBLE_TEMPO));
        }
    });
}

/// The clip's transpose, in semitones, and its detune, in cents: greyed in
/// Re-Pitch, whose pitch follows its speed.
fn pitch(ui: &mut Ui, scene: &Scene<'_>, warp: &Warp, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    let transposes = warp.mode.transposes();
    let greyed = "Re-Pitch's pitch follows its speed: the other modes transpose";
    ui.horizontal(|ui| {
        let fields = [
            (
                ("Transpose", "st", warp_mode::SET_TRANSPOSE, "semitones"),
                warp.transpose,
                Warp::TRANSPOSE_MAX,
            ),
            (
                ("Detune", "ct", warp_mode::SET_DETUNE, "cents"),
                warp.detune,
                Warp::DETUNE_MAX,
            ),
        ];
        for ((name, unit, id, arg), value, max) in fields {
            let mut value = i64::from(value);
            let max = i64::from(max);
            let field = DragValue::new(&mut value)
                .speed(0.1)
                .range(-max..=max)
                .custom_formatter(|value, _| match value > 0.0 {
                    true => format!("+{value} {unit}"),
                    false => format!("{value} {unit}"),
                })
                .custom_parser(|text| text.trim().trim_end_matches(unit).trim().parse().ok())
                .update_while_editing(false);
            let hint = format!(
                "{name}: how far the audio's pitch moves, in {arg}, where its curve doesn't \
                 say otherwise"
            );
            let field = ui
                .add_enabled(transposes, field)
                .on_hover_text(hint)
                .on_disabled_hover_text(greyed);
            name_field(&field, name, value as f64);
            edited(&field, actions, id, || {
                Args::new().with(arg, value).with("clip", clip)
            });
        }
    });
}

/// The clip's gain, in decibels, to drag or type.
fn gain(ui: &mut Ui, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let clip = scene.clip.id;
    let mut db = gain_to_db(scene.audio.gain);
    let max = 20.0 * ParamSpec::CLIP_GAIN_MAX.log10();
    ui.horizontal(|ui| {
        let name = ui.label("Gain");
        let field = ui
            .add(
                DragValue::new(&mut db)
                    .speed(0.2)
                    .range(GAIN_FLOOR..=max)
                    .custom_formatter(|db, _| decibels(db_to_gain(db) as f32))
                    .custom_parser(|text| {
                        let text = text.trim().trim_end_matches("dB").trim();
                        match text {
                            "-inf" => Some(GAIN_FLOOR),
                            text => text.parse().ok(),
                        }
                    })
                    .update_while_editing(false),
            )
            .on_hover_text("The clip's gain, where its curve doesn't say otherwise")
            .labelled_by(name.id);
        edited(&field, actions, warp::SET_GAIN, || {
            Args::new().with("gain", db_to_gain(db)).with("clip", clip)
        });
    });
}

/// A menu of `options`, with what `name_of` calls each, showing `shown`
/// and with `hint` on it, and named `name` for screen readers. Returns the
/// option picked, if it isn't `current`.
fn menu<T: Copy + PartialEq>(
    ui: &mut Ui,
    name: &str,
    shown: RichText,
    options: &[T],
    current: T,
    name_of: impl Fn(T) -> &'static str,
    hint: &str,
) -> Option<T> {
    let mut picked = None;
    let menu = ComboBox::from_id_salt(name)
        .width(ui.available_width())
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for &option in options {
                let item = ui.selectable_label(option == current, name_of(option));
                if item.clicked() && option != current {
                    picked = Some(option);
                }
            }
        });
    name_menu(&menu.response, name.to_owned(), name_of(current));
    menu.response.on_hover_text(hint);
    picked
}

/// Names the field `field`, which shows `value`, for screen readers.
fn name_field(field: &Response, name: &str, value: f64) {
    let enabled = field.enabled();
    field.widget_info(|| WidgetInfo {
        label: Some(name.to_owned()),
        ..WidgetInfo::drag_value(enabled, value)
    });
}

/// Asks for the command `id`, with the arguments `args` gives, if `field`
/// changed, with each drag of it one step in the undo history.
fn edited(field: &Response, actions: &mut Vec<Action>, id: CommandId, args: impl FnOnce() -> Args) {
    if field.drag_started() {
        actions.push(Action::BeginGesture);
    }
    if field.changed() {
        run(actions, id, args());
    }
    if field.drag_stopped() {
        actions.push(Action::EndGesture);
    }
}

/// Names what's in sight for screen readers, and for the tests, which find
/// warp markers by these names: "Warp marker at 2. 1. 1".
fn name_for_screen_readers(ui: &Ui, scene: &Scene<'_>, id: Id) {
    let layout = &scene.layout;
    let tempo = scene.tempo();
    let size = scene.theme.sample_editor.handle;
    for (i, marker) in scene.markers().iter().enumerate() {
        let x = scene.marker_x(marker);
        let handle =
            Rect::from_x_y_ranges(x - size / 2.0..=x + size / 2.0, layout.handles.y_range());
        let rect = handle.intersect(layout.wave);
        if !rect.is_positive() {
            continue;
        }
        let response = ui.interact(rect, id.with(("marker", i)), Sense::hover());
        let at = warp::song_position(scene.clip, scene.audio, marker.beat);
        let name = format!("Warp marker at {}", position(tempo, at.max(Beats::ZERO)));
        let selected = scene.selected(marker);
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &name));
    }
}

//! The Sample Editor: the audio clip it shows, its waveform against the
//! song's beats, as Live's Sample Editor shows it, with the clip's controls
//! beside it: Warp, its mode, its tempo and its gain. A ruler over the
//! waveform shows the song's bars and one under it the file's time, the
//! warp markers' handles are along the waveform's top, and the lane of the
//! clip's gain is under it all.
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

mod paint;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use egui::{
    Button, CursorIcon, DragValue, Event, Id, MouseWheelUnit, Pos2, Rect, Response, RichText,
    Role, Sense, Ui, UiBuilder, WidgetInfo, pos2,
};
use musicable_commands::editor::Viewport;
use musicable_commands::{
    Args, CommandId, Editor, Focus, Modifiers, automation, transport, view as views, warp,
};
use musicable_model::Project;
use musicable_model::automation::Point;
use musicable_model::clip::{AudioClip, Clip, ClipContent};
use musicable_model::param::{ClipParam, ParamAddress, ParamSpec};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::{Warp, WarpMarker};

use crate::arrangement::{distance_to_segment, simplified};
use crate::mixer::decibels;
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
}

/// The parts of the Sample Editor that take the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    /// The ruler of the song's bars, over the waveform.
    Ruler,
    Wave,
    /// The ruler of the file's time, under the waveform.
    Time,
    /// The lane of the clip's gain.
    Lane,
}

/// What's under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// The warp marker at this place in the warp's markers.
    Marker(usize),
    /// The waveform's top half, away from the markers.
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
            Hit::WaveTop => Target::WaveformTop,
            Hit::Wave => Target::Waveform,
            Hit::Ruler => Target::Ruler,
            Hit::Lane(Envelope::Point(_)) => Target::AutomationPoint,
            Hit::Lane(Envelope::Line { .. }) => Target::AutomationLine,
            Hit::Lane(Envelope::Lane) => Target::AutomationLane,
        }
    }
}

/// A part of the gain lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Envelope {
    /// The point at this place in the curve's points.
    Point(usize),
    /// Near the line through the points at these places, next to each
    /// other: or the same point twice, for the level line before the first
    /// point or after the last. A lane without points has a level line at
    /// the clip's gain, with no points to move.
    Line { from: usize, to: usize },
    Lane,
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
    /// What a drag that draws on the gain lane drew so far: a value for
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
    /// Sets the points of the clip's gain curve, in its own time.
    Points(Vec<Point>),
    /// Scrolls and zooms. Drags ask for it as they go.
    Viewport(Viewport),
}

/// Where the Sample Editor's parts are this frame.
#[derive(Debug, Clone)]
struct Layout {
    /// The column of the clip's controls, beside the rulers and the
    /// waveform, and the gain lane's name, beside the lane.
    controls: Rect,
    label: Rect,
    /// The ruler of the song's bars, the waveform, the strip along its top
    /// with the markers' handles, the ruler of the file's time and the gain
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

    /// The y coordinates the gain lane's line goes between, from silence to
    /// the loudest: clear of the lane's edges, so the points at either end
    /// show whole.
    fn travel(&self, theme: &Theme) -> (f32, f32) {
        let lane = self.lane;
        let margin = (theme.point + 2.0).min(lane.height() / 2.0);
        (lane.bottom() - margin, lane.top() + margin)
    }

    /// The y coordinate `unit` of the way up the gain lane's travel.
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

    /// The clip's gain, as automation names it.
    fn param(&self) -> ParamAddress {
        ParamAddress::Clip(self.clip.id, ClipParam::Gain)
    }

    fn spec(&self) -> ParamSpec {
        ClipParam::Gain.spec()
    }

    /// The gain curve's points, in the clip's own time.
    fn curve(&self) -> &[Point] {
        let lane = self.audio.lane(ClipParam::Gain);
        lane.map_or(&[], |lane| &lane.points[..])
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
        audio.frame_at(self.clip.own_time_at(at, self.tempo(), self.rate), self.rate)
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

    /// The gain lane's value at `y`.
    fn value_at(&self, y: f32) -> f64 {
        let spec = self.spec();
        let unit = self.layout.unit_at(self.theme, y);
        spec.scale.from_unit(unit, spec.min, spec.max)
    }

    /// Where `value` is drawn on the gain lane.
    fn y_of(&self, value: f64) -> f32 {
        let spec = self.spec();
        let unit = spec.scale.to_unit(value, spec.min, spec.max);
        self.layout.y_of(self.theme, unit)
    }

    /// Where `point`, of the gain curve, is drawn.
    fn point_at(&self, point: &Point) -> Pos2 {
        pos2(self.layout.x_of_beats(self.song_of(point.at)), self.y_of(point.value))
    }

    /// The gain curve's value at `own`, in the clip's own time, or else the
    /// clip's gain.
    fn line_value(&self, own: Beats) -> f64 {
        let lane = self.audio.lane(ClipParam::Gain);
        lane.map_or(f64::from(self.audio.gain), |lane| lane.value_at(own))
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
        match near {
            Some((_, i)) => Hit::Marker(i),
            None => Hit::WaveTop,
        }
    }

    /// What's under `pos` on the gain lane: one of its points, the line
    /// through them, or neither.
    fn envelope_part(&self, pos: Pos2) -> Envelope {
        let layout = &self.layout;
        let grab = self.theme.point_grab;
        let points = self.curve();
        let Some(end) = points.len().checked_sub(1) else {
            let level = self.y_of(f64::from(self.audio.gain));
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
        // first and after the last.
        let mut lines = vec![(pos2(layout.lane.left(), at[0].y), at[0], 0, 0)];
        lines.extend((0..end).map(|i| (at[i], at[i + 1], i, i + 1)));
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

    /// Draws on the gain lane from where the pointer last drew to where it
    /// is now.
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

    /// The gain curve once what `drag` drew takes the place of what was
    /// there, in the clip's own time: a step at either end of it, from the
    /// line as it was to what was drawn and back.
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
        let scale = self.spec().scale;
        let lane = self.audio.lane(ClipParam::Gain);
        let gain = f64::from(self.audio.gain);
        let before = lane.map_or(gain, |lane| {
            musicable_model::automation::value_before(&lane.points, start_own, scale)
                .unwrap_or(gain)
        });
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
                let frame = warp.frame_at(marker.beat.to_f64() - beats, self.rate).round();
                let after = i.checked_sub(1).map_or(0, |before| warp.markers[before].frame + 1);
                let before = warp.markers.get(i + 1).map_or(u64::MAX, |next| next.frame - 1);
                let frame = (frame.max(0.0) as u64).clamp(after, before.max(after));
                (frame != marker.frame).then_some(Plan::Audio { marker: i, frame })
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
                let mut points = old.to_vec();
                points[i] = Point::new(at, spec.scale.from_unit(unit, spec.min, spec.max));
                (points != old).then_some(Plan::Points(points))
            }
            (MouseAction::MoveSegment, Hit::Lane(Envelope::Line { from, to })) => {
                let old = self.curve();
                let spec = self.spec();
                let up = layout.units_in(self.theme, drag.from.y - drag.to.y);
                let mut points = old.to_vec();
                for point in points.get_mut(from..=to)? {
                    let unit = spec.scale.to_unit(point.value, spec.min, spec.max) + up;
                    point.value = spec.scale.from_unit(unit, spec.min, spec.max);
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
    /// drag moves markers or the audio under them.
    fn planned(&self, plan: Option<&Plan>) -> Option<AudioClip> {
        let mut warp = self.warp()?.clone();
        let moved = match plan? {
            Plan::Markers { markers, by } => warp.move_markers(markers, *by),
            &Plan::Audio { marker, frame } => warp.move_audio(marker, frame),
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
    let beats: Vec<Beats> = markers.iter().filter_map(|&i| all.get(i)).map(|m| m.beat).collect();
    Args::new()
        .with("markers", beats)
        .with("clip", scene.clip.id)
}

/// Asks the app for `plan`.
fn send(actions: &mut Vec<Action>, scene: &Scene<'_>, plan: &Plan) {
    match plan {
        Plan::Markers { markers, by } => {
            run(actions, warp::MOVE_MARKERS, of(scene, markers).with("by", *by));
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
        Plan::Points(points) => set_points(actions, scene, points),
        Plan::Viewport(viewport) => set_viewport(actions, viewport),
    }
}

/// Asks the app to give the clip's gain curve `points`, or to clear it
/// without them, which keeps the gain it had at the playhead.
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
        let areas = [
            (scene.layout.ruler, Area::Ruler, "Sample Editor Ruler"),
            (scene.layout.wave, Area::Wave, "Sample Editor"),
            (scene.layout.time, Area::Time, "File Time"),
            (scene.layout.lane, Area::Lane, "Gain Lane"),
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
        self.wheel(ui, &scene, &mut viewport);
        self.follow(&scene, &mut viewport);
        if viewport != editor.sample_editor.viewport {
            set_viewport(actions, &viewport);
            scene.layout = Layout::new(rect, theme, viewport);
        }
        let plan = self.drag.as_ref().and_then(|drag| scene.plan(drag));
        self.cursor(ui, &scene);

        let painter = ui.painter_at(rect);
        let layout = &scene.layout;
        paint::ruler(&painter.with_clip_rect(layout.ruler), &scene);
        paint::wave(&painter.with_clip_rect(layout.wave), &scene, plan.as_ref());
        paint::time(&painter.with_clip_rect(layout.time), &scene, plan.as_ref());
        paint::lane(&painter.with_clip_rect(layout.lane), &scene, plan.as_ref());
        paint::label(&painter.with_clip_rect(layout.label), &scene);
        paint::column(&painter.with_clip_rect(layout.controls), &scene);
        controls(ui, &scene, actions);
        name_for_screen_readers(ui, &scene, id);
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
            // The scrolling changes as the drag goes.
            if drag.action == MouseAction::ScrubZoom {
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
            Some(MouseAction::MoveMarker | MouseAction::MoveMarkerFree | MouseAction::MoveAudio) => {
                Some(CursorIcon::ResizeHorizontal)
            }
            Some(MouseAction::MovePoint | MouseAction::MovePointFree | MouseAction::MoveSegment) => {
                Some(CursorIcon::Grab)
            }
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
        Plan::Viewport(_) => {
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
            run(actions, transport::SET_START_MARKER, Args::new().with("at", at));
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

/// The clip's controls, in the column beside the waveform: Warp, its mode
/// and its tempo, with Live's ×2 and ÷2, and its gain.
fn controls(ui: &mut Ui, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let theme = scene.theme;
    let column = scene.layout.controls.shrink(theme.padding);
    let clip = scene.clip.id;
    ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
        ui.spacing_mut().item_spacing.y = theme.padding;
        ui.add_space(theme.sample_editor.ruler);
        let warped = scene.warp().is_some();
        let hint = "Whether the clip follows the song's tempo";
        let warp_button = ui.add(Button::new("Warp").selected(warped)).on_hover_text(hint);
        if warp_button.clicked() {
            let args = Args::new().with("on", !warped).with("clip", clip);
            run(actions, warp::SET_WARP, args);
        }
        if let Some(warp) = scene.warp() {
            ui.label(RichText::new(format!("Mode: {}", warp.mode.name())).size(theme.small_text));
            let mut bpm = warp.clip_tempo(scene.rate);
            ui.horizontal(|ui| {
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
                // One drag is one step in the undo history.
                if field.drag_started() {
                    actions.push(Action::BeginGesture);
                }
                if field.changed() {
                    let args = Args::new().with("bpm", bpm).with("clip", clip);
                    run(actions, warp::SET_TEMPO, args);
                }
                if field.drag_stopped() {
                    actions.push(Action::EndGesture);
                }
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
        let mut db = gain_to_db(scene.audio.gain);
        let max = 20.0 * ParamSpec::CLIP_GAIN_MAX.log10();
        ui.horizontal(|ui| {
            ui.label("Gain");
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
                .on_hover_text("The clip's gain, where its curve doesn't say otherwise");
            if field.drag_started() {
                actions.push(Action::BeginGesture);
            }
            if field.changed() {
                let args = Args::new().with("gain", db_to_gain(db)).with("clip", clip);
                run(actions, warp::SET_GAIN, args);
            }
            if field.drag_stopped() {
                actions.push(Action::EndGesture);
            }
        });
    });
}

/// Names what's in sight for screen readers, and for the tests, which find
/// warp markers by these names: "Warp marker at 2. 1. 1".
fn name_for_screen_readers(ui: &Ui, scene: &Scene<'_>, id: Id) {
    let layout = &scene.layout;
    let tempo = scene.tempo();
    let size = scene.theme.sample_editor.handle;
    for (i, marker) in scene.markers().iter().enumerate() {
        let x = scene.marker_x(marker);
        let handle = Rect::from_x_y_ranges(x - size / 2.0..=x + size / 2.0, layout.handles.y_range());
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

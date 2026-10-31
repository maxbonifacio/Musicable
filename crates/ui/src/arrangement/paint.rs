//! Drawing the arrangement: the lanes with their clips, and the takes' and
//! automation lanes under the tracks that show them, the master's row and
//! its lanes along the bottom, the ruler over them, the tracks' headers,
//! and the corner between the ruler and the headers. Only what's in sight
//! is drawn, and the tracks only in their part, over the master's.

use egui::{
    Align2, CornerRadius, FontId, Mesh, Painter, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};
use musicable_commands::take::comped;
use musicable_model::Chain;
use musicable_model::automation::Point;
use musicable_model::clip::{AudioClip, Clip, ClipContent, MidiClip};
use musicable_model::time::Beats;

use super::{AutoRow, Envelope, Hit, LaneButton, Plan, Row, Scene, Side, TakeRow, find};
use crate::browser::Destination;
use crate::timeline::{self, Timeline};

/// The fewest keys a MIDI clip's notes are spread over, so a clip of a few
/// notes doesn't fill its height with them.
const MIN_KEYS: u8 = 12;

/// The lanes: each track's clips over the grid and the bars' shades, and
/// its takes' and its automation lanes under it if it shows them, with what
/// the tracks are recording, the loop region, the time selection, the start
/// marker and the playhead, and where `plan` would put things; and the
/// master's under them.
pub(super) fn lanes(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let project = scene.project();
    let editor = scene.editor();
    let lanes = layout.lanes;
    let whole = painter;
    let part = layout.tracks_part(lanes.x_range());
    let painter = &painter.with_clip_rect(part.intersect(painter.clip_rect()));
    painter.rect_filled(part, 0.0, theme.empty);
    for row in layout.visible_rows() {
        let selected = editor.selected_track == Some(Chain::Track(row.id))
            || editor.selection.tracks.contains(&row.id);
        let fill = if selected {
            theme.lane_selected
        } else if row.index % 2 == 1 {
            theme.lane_alternate
        } else {
            theme.lane
        };
        painter.rect_filled(lane(scene, row), 0.0, fill);
    }
    for take in layout.visible_takes() {
        let rect = Rect::from_x_y_ranges(lanes.x_range(), take.top..=take.bottom());
        painter.rect_filled(rect, 0.0, theme.take_lane);
    }
    for lane in layout.visible_automation() {
        let rect = Rect::from_x_y_ranges(lanes.x_range(), lane.top..=lane.bottom());
        painter.rect_filled(rect, 0.0, theme.automation_lane);
    }
    let bottom = layout.bottom().clamp(lanes.top(), part.bottom());
    let tracks = Rect::from_x_y_ranges(lanes.x_range(), lanes.top()..=bottom);
    timeline::bar_shades(painter, theme, &self::timeline(scene), tracks);
    if project.looping {
        let region = project.loop_region;
        let x = layout.x_of(region.start)..=layout.x_of(region.end);
        let shade = Rect::from_x_y_ranges(x, tracks.y_range());
        painter.rect_filled(shade, 0.0, theme.loop_shade);
    }
    timeline::grid(painter, theme, &self::timeline(scene), editor.grid, tracks);
    let bottoms = (layout.visible_rows().map(Row::bottom))
        .chain(layout.visible_takes().map(TakeRow::bottom))
        .chain(layout.visible_automation().map(AutoRow::bottom));
    for bottom in bottoms {
        let y = painter.round_to_pixel_center(bottom - 0.5);
        painter.hline(lanes.x_range(), y, Stroke::new(1.0, theme.divider));
    }
    clips(painter, scene);
    takes(painter, scene);
    recordings(painter, scene);
    time_selection(painter, scene);
    if let Some(plan) = plan {
        ghosts(painter, scene, plan);
    }
    let master = layout.master_part(lanes.x_range());
    let filled = self::master(
        &whole.with_clip_rect(master.intersect(whole.clip_rect())),
        scene,
    );
    // Each lane's in its own part.
    automation(whole, scene, plan);
    let start = whole.round_to_pixel_center(layout.x_of(editor.start_marker));
    let marker = Stroke::new(1.0, theme.start_marker);
    painter.vline(start, tracks.y_range(), marker);
    let in_master = filled.intersect(master);
    if in_master.height() > 0.0 {
        whole.vline(start, in_master.y_range(), marker);
    }
    if let Some(x) = playhead(scene) {
        let x = whole.round_to_pixel_center(x);
        whole.vline(x, lanes.y_range(), Stroke::new(1.0, theme.playhead));
    }
}

/// The master's part of the lanes: its row, lit while it's selected, and
/// its automation lanes under it, with the bars' shades, the grid and the
/// loop region over them, the time selection over its row if it's on the
/// master, and a line along its top that sets it apart from the tracks.
/// Returns where its row and lanes are.
fn master(painter: &Painter, scene: &Scene<'_>) -> Rect {
    let theme = scene.theme;
    let layout = &scene.layout;
    let editor = scene.editor();
    let part = layout.master_part(layout.lanes.x_range());
    painter.rect_filled(part, 0.0, theme.empty);
    let row = layout.master;
    let fill = match editor.selected_track == Some(Chain::Master) {
        true => theme.lane_selected,
        false => theme.lane,
    };
    let across = part.x_range();
    painter.rect_filled(
        Rect::from_x_y_ranges(across, row.top..=row.bottom()),
        0.0,
        fill,
    );
    for lane in &layout.master_automation {
        let rect = Rect::from_x_y_ranges(across, lane.top..=lane.bottom());
        painter.rect_filled(rect, 0.0, theme.automation_lane);
    }
    let bottom = (layout.master_automation.last()).map_or(row.bottom(), AutoRow::bottom);
    let filled = Rect::from_x_y_ranges(across, row.top..=bottom);
    timeline::bar_shades(painter, theme, &self::timeline(scene), filled);
    let project = scene.project();
    if project.looping {
        let region = project.loop_region;
        let x = layout.x_of(region.start)..=layout.x_of(region.end);
        let shade = Rect::from_x_y_ranges(x, filled.y_range());
        painter.rect_filled(shade, 0.0, theme.loop_shade);
    }
    timeline::grid(painter, theme, &self::timeline(scene), editor.grid, filled);
    let bottoms =
        std::iter::once(row.bottom()).chain(layout.master_automation.iter().map(AutoRow::bottom));
    for bottom in bottoms {
        let y = painter.round_to_pixel_center(bottom - 0.5);
        painter.hline(across, y, Stroke::new(1.0, theme.divider));
    }
    if let Some(time) = (editor.selection.time.as_ref()).filter(|time| time.master) {
        let x = layout.x_of(time.range.start)..=layout.x_of(time.range.end);
        let rect = Rect::from_x_y_ranges(x, row.top..=row.bottom());
        painter.rect_filled(rect, 0.0, theme.time_selection);
    }
    master_line(painter, scene, across);
    filled
}

/// The line along the top of the master's part, across `x`.
fn master_line(painter: &Painter, scene: &Scene<'_>, x: egui::Rangef) {
    let top = scene.layout.master_top;
    painter.line_segment(
        [pos2(x.min, top + 1.0), pos2(x.max, top + 1.0)],
        Stroke::new(2.0, scene.theme.divider),
    );
}

/// Where the playhead is drawn, if it is.
fn playhead(scene: &Scene<'_>) -> Option<f32> {
    let at = timeline::playhead(&scene.view.playback, scene.editor().start_marker)?;
    Some(scene.layout.x_of(at))
}

/// The song positions across the lanes.
fn timeline<'s>(scene: &'s Scene<'_>) -> Timeline<'s> {
    Timeline {
        tempo: &scene.project().tempo,
        viewport: scene.layout.viewport,
        left: scene.layout.lanes.left(),
    }
}

/// The lane of the track in `row`, across the timeline.
fn lane(scene: &Scene<'_>, row: &Row) -> Rect {
    Rect::from_x_y_ranges(scene.layout.lanes.x_range(), row.top..=row.bottom())
}

/// The time selection, over the clips on its tracks.
fn time_selection(painter: &Painter, scene: &Scene<'_>) {
    let Some(time) = &scene.editor().selection.time else {
        return;
    };
    let layout = &scene.layout;
    let x = layout.x_of(time.range.start)..=layout.x_of(time.range.end);
    for row in layout.visible_rows() {
        if time.tracks.contains(&row.id) {
            let rect = Rect::from_x_y_ranges(x.clone(), row.top..=row.bottom());
            painter.rect_filled(rect, 0.0, scene.theme.time_selection);
        }
    }
}

/// The clips in sight.
fn clips(painter: &Painter, scene: &Scene<'_>) {
    let visible = scene.layout.viewport.visible();
    for row in scene.layout.visible_rows() {
        let track = &scene.project().tracks[row.index];
        for clip in &track.clips {
            if clip.end() > visible.start && clip.start < visible.end {
                self::clip(painter, scene, row, clip);
            }
        }
    }
}

fn clip(painter: &Painter, scene: &Scene<'_>, row: &Row, clip: &Clip) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let at = layout.clip(theme, row, clip);
    let colour = theme.clip(row.index);
    painter.rect_filled(at.rect, theme.corner, colour);
    let painter = painter.with_clip_rect(at.rect.intersect(painter.clip_rect()));
    if let Some(body) = at.body {
        let corner = theme.corner as u8;
        let radius = CornerRadius {
            nw: 0,
            ne: 0,
            sw: corner,
            se: corner,
        };
        painter.rect_filled(body, radius, theme.clip_body);
        match &clip.content {
            ClipContent::Audio(audio) => {
                waveform(&painter, scene, body, clip, audio);
                fades(&painter, scene, body, clip, audio);
            }
            ClipContent::Midi(midi) => notes(&painter, scene, body, clip, midi),
        }
    }
    // The name stays in sight while the clip's start is scrolled away.
    let left = at.title.left().max(layout.lanes.left()) + theme.padding;
    painter.text(
        pos2(left, at.title.center().y),
        Align2::LEFT_CENTER,
        &clip.name,
        FontId::proportional(theme.small_text),
        theme.clip_text,
    );
    let selected = scene.editor().selection.clips.contains(&clip.id);
    let hovered = scene.pointer.is_some_and(|pos| at.rect.contains(pos));
    if selected || hovered {
        for side in [Side::Start, Side::End] {
            if let Some(handle) = layout.fade_handle(theme, &at, clip, side) {
                painter.rect(
                    handle,
                    0.0,
                    theme.fade,
                    Stroke::new(1.0, theme.clip_selected),
                    StrokeKind::Inside,
                );
            }
        }
    }
    if selected {
        let outline = Stroke::new(1.5, theme.clip_selected);
        painter.rect_stroke(at.rect, theme.corner, outline, StrokeKind::Inside);
    }
}

/// The clips of the takes in sight, in their track's colour where the
/// track plays them, and grey where it doesn't.
fn takes(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let visible = layout.viewport.visible();
    for lane in layout.visible_takes() {
        let track = &scene.project().tracks[lane.track_index];
        let take = &track.takes[lane.index];
        for clip in &take.clips {
            if clip.end() <= visible.start || clip.start >= visible.end {
                continue;
            }
            let rect = Rect::from_x_y_ranges(
                layout.x_of(clip.start)..=layout.x_of(clip.end()),
                lane.top + 1.0..=lane.bottom() - 1.0,
            );
            painter.rect_filled(rect, theme.corner, theme.take_clip);
            let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
            let colour = theme.clip(lane.track_index);
            for played in comped(scene.project(), scene.editor(), track, clip) {
                let x = layout.x_of(played.start)..=layout.x_of(played.end);
                painter.rect_filled(Rect::from_x_y_ranges(x, rect.y_range()), 0.0, colour);
            }
            match &clip.content {
                ClipContent::Audio(audio) => waveform(&painter, scene, rect, clip, audio),
                ClipContent::Midi(midi) => notes(&painter, scene, rect, clip, midi),
            }
        }
    }
}

/// The automation lanes in sight: each one's line through its points, as
/// `plan` would leave them while a drag edits it, grey where someone
/// overrode it, or a parameter's own value where it has no automation.
fn automation(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let project = scene.project();
    // The point under the pointer is filled in.
    let lanes = scene.layout.lanes;
    let hovered = (scene.pointer)
        .filter(|&pos| lanes.contains(pos))
        .map(|pos| scene.hit_lanes(pos));
    for lane in scene.layout.visible_automation() {
        let part = scene.layout.part_of(lane.chain, lanes.x_range());
        let painter = painter.with_clip_rect(
            Rect::from_x_y_ranges(lanes.x_range(), lane.top..=lane.bottom())
                .intersect(part)
                .intersect(painter.clip_rect()),
        );
        let planned = match plan {
            Some(Plan::Points { param, points }) if *param == lane.param => Some(&points[..]),
            _ => None,
        };
        let automated = project.lane(lane.param);
        let colour = match automated {
            Some(automated) if !automated.overridden => theme.automation,
            _ => theme.automation_off,
        };
        let points = planned.or(automated.map(|automated| &automated.points[..]));
        match points {
            Some(points) if !points.is_empty() => {
                let hovered = match hovered {
                    Some(Hit::Automation {
                        param,
                        part: Envelope::Point(i),
                        ..
                    }) if param == lane.param && planned.is_none() => Some(i),
                    _ => None,
                };
                envelope(&painter, scene, lane, points, colour, hovered);
            }
            // A dashed line at the parameter's own value, where it's known.
            _ => {
                let Some(own) = project.param_value(lane.param) else {
                    continue;
                };
                let spec = scene.spec(lane.param);
                let y = lane.y_of(theme, spec.scale.to_unit(own, spec.min, spec.max));
                let y = painter.round_to_pixel_center(y);
                let line = [pos2(lanes.left(), y), pos2(lanes.right(), y)];
                let dashes = Shape::dashed_line(&line, Stroke::new(1.0, colour), 4.0, 4.0);
                painter.extend(dashes);
            }
        }
    }
}

/// The line through `points` on `lane`, level before the first and after
/// the last, and the points in sight, with the one at `hovered` filled in.
fn envelope(
    painter: &Painter,
    scene: &Scene<'_>,
    lane: &AutoRow,
    points: &[Point],
    colour: egui::Color32,
    hovered: Option<usize>,
) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let spec = scene.spec(lane.param);
    let at = |point: &Point| {
        let unit = spec.scale.to_unit(point.value, spec.min, spec.max);
        pos2(layout.x_of(point.at), lane.y_of(theme, unit))
    };
    // Those in sight, and one either side, for the lines out of sight.
    let visible = layout.viewport.visible();
    let first = points.partition_point(|p| p.at < visible.start);
    let last = points.partition_point(|p| p.at <= visible.end);
    let (from, to) = (first.saturating_sub(1), (last + 1).min(points.len()));
    let mut line = Vec::with_capacity(to - from + 2);
    if from == 0 {
        line.push(pos2(layout.lanes.left(), at(&points[0]).y));
    }
    line.extend(points[from..to].iter().map(at));
    if to == points.len() {
        let end = at(&points[to - 1]).y;
        line.push(pos2(layout.lanes.right(), end));
    }
    painter.add(Shape::line(line, Stroke::new(1.5, colour)));
    for (i, point) in points.iter().enumerate().take(last).skip(first) {
        let fill = match hovered == Some(i) {
            true => colour,
            false => theme.automation_lane,
        };
        painter.circle(at(point), theme.point, fill, Stroke::new(1.5, colour));
    }
}

/// What the armed tracks have recorded so far, while they record, over
/// their lanes.
fn recordings(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    for recording in scene.view.recordings {
        let Some(row) = layout.row(recording.track) else {
            continue;
        };
        let range = recording.range;
        let rect = Rect::from_x_y_ranges(
            layout.x_of(range.start)..=layout.x_of(range.end),
            row.top + 1.0..=row.bottom() - 1.0,
        );
        let outline = Stroke::new(1.0, theme.arm);
        painter.rect(
            rect,
            theme.corner,
            theme.recording,
            outline,
            StrokeKind::Inside,
        );
    }
}

/// An audio clip's waveform in its `body`, a column for each point, from
/// the file's peaks once the app has read them.
fn waveform(painter: &Painter, scene: &Scene<'_>, body: Rect, clip: &Clip, audio: &AudioClip) {
    let Some(peaks) = scene.view.waveforms.get(&audio.file) else {
        return;
    };
    let layout = &scene.layout;
    let tempo = &scene.project().tempo;
    let rate = peaks.sample_rate();
    // The file's frame that plays at the point `x`: along the warp markers,
    // for a warped clip.
    let frame = |x: f32| audio.frame_at(clip.own_time_at(layout.beats_at(x), tempo, rate), rate);
    let left = body.left().max(layout.lanes.left()).floor();
    let right = body.right().min(layout.lanes.right()).ceil();
    let middle = body.center().y;
    let half = (body.height() / 2.0 - 1.0).max(0.0);
    let height = |sample: f32| (sample * audio.gain).clamp(-1.0, 1.0) * half;
    let mut mesh = Mesh::default();
    let mut x = left;
    let mut from = frame(x);
    while x < right {
        let to = frame(x + 1.0);
        if let Some(peak) = peaks.range(from, to) {
            // Silence still shows, as a line.
            let top = middle - height(peak.max);
            let bottom = (middle - height(peak.min)).max(top + 1.0);
            let column = Rect::from_x_y_ranges(x..=x + 1.0, top..=bottom);
            mesh.add_colored_rect(column, scene.theme.waveform);
        }
        x += 1.0;
        from = to;
    }
    painter.add(Shape::mesh(mesh));
}

/// An audio clip's fades: lines from silence at its edges, with what they
/// fade shaded.
fn fades(painter: &Painter, scene: &Scene<'_>, body: Rect, clip: &Clip, audio: &AudioClip) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let (top, bottom) = (body.top(), body.bottom());
    let shade = theme.fade.gamma_multiply(0.3);
    let line = Stroke::new(1.0, theme.fade);
    if audio.fade_in > Beats::ZERO {
        let (start, end) = (
            layout.x_of(clip.start),
            layout.x_of(clip.start + audio.fade_in),
        );
        let points = vec![pos2(start, top), pos2(end, top), pos2(start, bottom)];
        painter.add(Shape::convex_polygon(points, shade, Stroke::NONE));
        painter.line_segment([pos2(start, bottom), pos2(end, top)], line);
    }
    if audio.fade_out > Beats::ZERO {
        let (start, end) = (
            layout.x_of(clip.end() - audio.fade_out),
            layout.x_of(clip.end()),
        );
        let points = vec![pos2(start, top), pos2(end, top), pos2(end, bottom)];
        painter.add(Shape::convex_polygon(points, shade, Stroke::NONE));
        painter.line_segment([pos2(start, top), pos2(end, bottom)], line);
    }
}

/// A MIDI clip's notes in its `body`, from the lowest key it plays at the
/// bottom to the highest at the top.
fn notes(painter: &Painter, scene: &Scene<'_>, body: Rect, clip: &Clip, midi: &MidiClip) {
    let layout = &scene.layout;
    // The notes the clip plays: those that start in its stretch of its own
    // time.
    let end = midi.offset + clip.length;
    let playing = || {
        midi.notes
            .iter()
            .filter(move |note| note.start >= midi.offset && note.start < end)
    };
    let Some(low) = playing().map(|note| note.key).min() else {
        return;
    };
    let high = playing().map(|note| note.key).max().unwrap_or(low);
    let spread = high - low;
    let low = low.saturating_sub(MIN_KEYS.saturating_sub(spread) / 2);
    let keys = f32::from((high - low + 1).max(MIN_KEYS));
    let height = (body.height() - 2.0) / keys;
    let visible = layout.lanes.x_range();
    let mut mesh = Mesh::default();
    for note in playing() {
        let start = clip.start + (note.start - midi.offset);
        let stop = (start + note.length).min(clip.end());
        let (left, right) = (layout.x_of(start), layout.x_of(stop));
        if right < visible.min || left > visible.max {
            continue;
        }
        let y = body.bottom() - 1.0 - f32::from(note.key - low + 1) * height;
        let rect = Rect::from_x_y_ranges(left..=right.max(left + 1.0), y..=y + height.max(1.0));
        mesh.add_colored_rect(rect, scene.theme.notes);
    }
    painter.add(Shape::mesh(mesh));
}

/// Where the clips a drag moves, trims or fades will be.
fn ghosts(painter: &Painter, scene: &Scene<'_>, plan: &Plan) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let project = scene.project();
    let outline = Stroke::new(1.0, theme.clip_selected);
    let ghost = |rect: Rect| {
        painter.rect(rect, theme.corner, theme.ghost, outline, StrokeKind::Inside);
    };
    let span = |row: &Row, start: Beats, end: Beats| {
        Rect::from_x_y_ranges(
            layout.x_of(start)..=layout.x_of(end),
            row.top + 1.0..=row.bottom() - 1.0,
        )
    };
    match plan {
        Plan::Move {
            clips, by, tracks, ..
        } => {
            for &id in clips {
                let Some((index, clip)) = find(project, id) else {
                    continue;
                };
                let index = usize::try_from(index as i64 + tracks).ok();
                if let Some(row) = index.and_then(|index| layout.rows.get(index)) {
                    ghost(span(row, clip.start + *by, clip.end() + *by));
                }
            }
        }
        Plan::Trim { clip, side, at } | Plan::Stretch { clip, side, at, .. } => {
            let Some((index, clip)) = find(project, *clip) else {
                return;
            };
            let (start, end) = match side {
                Side::Start => (*at, clip.end()),
                Side::End => (clip.start, *at),
            };
            ghost(span(&layout.rows[index], start, end));
        }
        Plan::Fade { clip, side, length } => {
            let Some((index, clip)) = find(project, *clip) else {
                return;
            };
            let at = layout.clip(theme, &layout.rows[index], clip);
            let body = at.body.unwrap_or(at.rect);
            let (top, bottom) = (body.top(), body.bottom());
            let line = match side {
                Side::Start => {
                    let x = layout.x_of(clip.start);
                    [
                        pos2(x, bottom),
                        pos2(layout.x_of(clip.start + *length), top),
                    ]
                }
                Side::End => {
                    let x = layout.x_of(clip.end());
                    [
                        pos2(layout.x_of(clip.end() - *length), top),
                        pos2(x, bottom),
                    ]
                }
            };
            painter.line_segment(line, Stroke::new(1.5, theme.clip_selected));
        }
        Plan::Comp {
            track,
            take,
            start,
            end,
        } => {
            // On the take, and on the track where it'll go.
            if let Some(lane) = layout.takes.iter().find(|lane| lane.take == *take) {
                let x = layout.x_of(*start)..=layout.x_of(*end);
                ghost(Rect::from_x_y_ranges(
                    x,
                    lane.top + 1.0..=lane.bottom() - 1.0,
                ));
            }
            if let Some(row) = layout.row(*track) {
                ghost(span(row, *start, *end));
            }
        }
        _ => {}
    }
}

/// The ruler: the bar numbers, the loop brace, the locators, the start
/// marker and the playhead, and where `plan` would put the loop or a
/// locator.
pub(super) fn ruler(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let project = scene.project();
    let ruler = layout.ruler;
    let strip = layout.strip;
    painter.rect_filled(ruler, 0.0, theme.ruler);
    let (start, end, looping) = match plan {
        Some(&Plan::Loop {
            start,
            end,
            looping,
        }) => (start, end, looping),
        _ => (
            project.loop_region.start,
            project.loop_region.end,
            project.looping,
        ),
    };
    let colour = if looping {
        theme.loop_on
    } else {
        theme.loop_off
    };
    let brace = Rect::from_x_y_ranges(
        layout.x_of(start)..=layout.x_of(end),
        strip.center().y..=strip.bottom() - 1.0,
    );
    painter.rect_filled(brace, theme.corner, colour);
    timeline::bars(
        painter,
        theme,
        &self::timeline(scene),
        ruler,
        strip.bottom(),
    );
    for locator in &project.locators {
        let at = match plan {
            Some(&Plan::Locator { locator: id, at }) if id == locator.id => at,
            _ => locator.at,
        };
        let x = layout.x_of(at);
        if x < ruler.left() - theme.label_spacing || x > ruler.right() {
            continue;
        }
        let marker = theme.marker;
        let points = vec![
            pos2(x - marker, strip.top()),
            pos2(x + marker, strip.top()),
            pos2(x, strip.top() + marker),
        ];
        painter.add(Shape::convex_polygon(points, theme.locator, Stroke::NONE));
        painter.vline(x, strip.y_range(), Stroke::new(1.0, theme.locator));
        painter.text(
            pos2(x + marker, strip.top()),
            Align2::LEFT_TOP,
            &locator.name,
            FontId::proportional(theme.small_text),
            theme.locator,
        );
    }
    let x = layout.x_of(scene.editor().start_marker);
    timeline::start_marker(painter, theme, x, ruler);
    if let Some(x) = playhead(scene) {
        let x = painter.round_to_pixel_center(x);
        let below = strip.bottom()..=ruler.bottom();
        painter.vline(x, below, Stroke::new(1.0, theme.playhead));
    }
    let y = painter.round_to_pixel_center(ruler.bottom() - 0.5);
    painter.hline(ruler.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// The tracks' headers, for their parts to paint over: each track's, lit
/// while it's selected, over the room under the tracks, and the master's
/// under them.
pub(super) fn headers(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let editor = scene.editor();
    let headers = layout.headers;
    painter.rect_filled(headers, 0.0, theme.empty);
    let fill = |selected| match selected {
        true => theme.header_selected,
        false => theme.header,
    };
    let tracks = tracks_painter(painter, scene);
    for row in layout.visible_rows() {
        let rect = Rect::from_x_y_ranges(headers.x_range(), row.top..=row.bottom());
        let selected = editor.selection.tracks.contains(&row.id)
            || editor.selected_track == Some(Chain::Track(row.id));
        tracks.rect_filled(rect, 0.0, fill(selected));
    }
    let master = layout.master;
    let rect = Rect::from_x_y_ranges(headers.x_range(), master.top..=master.bottom());
    let selected = editor.selected_track == Some(Chain::Master);
    master_painter(painter, scene).rect_filled(rect, 0.0, fill(selected));
}

/// `painter`, painting only in the tracks' part of the headers.
pub(super) fn tracks_painter(painter: &Painter, scene: &Scene<'_>) -> Painter {
    let part = scene.layout.tracks_part(scene.layout.headers.x_range());
    painter.with_clip_rect(part.intersect(painter.clip_rect()))
}

/// `painter`, painting only in the master's part of the headers.
pub(super) fn master_painter(painter: &Painter, scene: &Scene<'_>) -> Painter {
    let part = scene.layout.master_part(scene.layout.headers.x_range());
    painter.with_clip_rect(part.intersect(painter.clip_rect()))
}

/// What goes over the tracks' headers once their parts are painted: a
/// line under each, the names of the takes in lanes under them, and the
/// automation lanes' headers. Their buttons are drawn after this.
pub(super) fn over_headers(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let headers = layout.headers;
    let whole = painter;
    let painter = &tracks_painter(painter, scene);
    for row in layout.visible_rows() {
        let y = painter.round_to_pixel_center(row.bottom() - 0.5);
        painter.hline(headers.x_range(), y, Stroke::new(1.0, theme.divider));
    }
    let master = master_painter(whole, scene);
    let y = master.round_to_pixel_center(layout.master.bottom() - 0.5);
    master.hline(headers.x_range(), y, Stroke::new(1.0, theme.divider));
    for lane in layout.visible_takes() {
        let take = &scene.project().tracks[lane.track_index].takes[lane.index];
        let rect = Rect::from_x_y_ranges(headers.x_range(), lane.top..=lane.bottom());
        painter.rect_filled(rect, 0.0, theme.take_lane);
        let strip = Rect::from_min_size(rect.left_top(), vec2(theme.colour_strip, rect.height()));
        painter.rect_filled(strip, 0.0, theme.clip(lane.track_index));
        // Set in from the track's name, as it's the track's.
        let name = Rect::from_min_max(
            pos2(strip.right() + 3.0 * theme.padding, rect.top()),
            pos2(rect.right() - theme.padding, rect.bottom()),
        );
        painter
            .with_clip_rect(name.intersect(painter.clip_rect()))
            .text(
                pos2(name.left(), name.center().y),
                Align2::LEFT_CENTER,
                &take.name,
                FontId::proportional(theme.small_text),
                theme.header_text,
            );
        let y = painter.round_to_pixel_center(lane.bottom() - 0.5);
        painter.hline(rect.x_range(), y, Stroke::new(1.0, theme.divider));
    }
    for lane in layout.visible_automation() {
        automation_header(whole, scene, lane);
    }
    master_line(&master, scene, headers.x_range());
    let x = whole.round_to_pixel_center(headers.left() + 0.5);
    whole.vline(x, headers.y_range(), Stroke::new(1.0, theme.divider));
}

/// An automation lane's header: its parameter's name, and its value at the
/// playhead, or that someone overrode its automation. Its buttons are
/// drawn after this, with the tracks'.
fn automation_header(painter: &Painter, scene: &Scene<'_>, lane: &AutoRow) {
    let theme = scene.theme;
    let headers = scene.layout.headers;
    let part = scene.layout.part_of(lane.chain, headers.x_range());
    let painter = &painter.with_clip_rect(part.intersect(painter.clip_rect()));
    let rect = Rect::from_x_y_ranges(headers.x_range(), lane.top..=lane.bottom());
    painter.rect_filled(rect, 0.0, theme.automation_lane);
    let strip = Rect::from_min_size(rect.left_top(), vec2(theme.colour_strip, rect.height()));
    let colour = lane.track_index.map_or(theme.master, |i| theme.clip(i));
    painter.rect_filled(strip, 0.0, colour);
    let buttons = lane_buttons(scene, lane);
    let right = buttons
        .first()
        .map_or(rect.right(), |(_, rect)| rect.left());
    // Set in from the track's name, as it's the track's.
    let room = Rect::from_min_max(
        pos2(
            strip.right() + 3.0 * theme.padding,
            rect.top() + theme.padding,
        ),
        pos2(right - theme.padding, rect.bottom()),
    );
    let painter = painter.with_clip_rect(room.intersect(painter.clip_rect()));
    let name = painter.text(
        room.left_top(),
        Align2::LEFT_TOP,
        scene.lane_name(lane.param),
        FontId::proportional(theme.small_text),
        theme.header_text,
    );
    let overridden = scene
        .project()
        .lane(lane.param)
        .is_some_and(|l| l.overridden);
    let colour = match overridden {
        true => theme.automation_off,
        false => theme.ruler_text,
    };
    if let Some(value) = scene.header_value(lane.param) {
        painter.text(
            pos2(room.left(), name.bottom() + theme.padding / 2.0),
            Align2::LEFT_TOP,
            value,
            FontId::proportional(theme.small_text),
            colour,
        );
    }
    let y = painter.round_to_pixel_center(lane.bottom() - 0.5);
    painter.with_clip_rect(rect.intersect(part)).hline(
        rect.x_range(),
        y,
        Stroke::new(1.0, theme.divider),
    );
}

/// Where the buttons go in an automation lane's header: along its top, at
/// its right. A lane partly scrolled away has none.
pub(super) fn lane_buttons(scene: &Scene<'_>, lane: &AutoRow) -> Vec<(LaneButton, Rect)> {
    let theme = scene.theme;
    let headers = scene.layout.headers;
    let part = scene.layout.part_of(lane.chain, headers.x_range());
    let size = theme.button;
    let mut right = headers.right() - theme.padding;
    let mut placed: Vec<(LaneButton, Rect)> =
        [LaneButton::Hide, LaneButton::Add, LaneButton::Choose]
            .into_iter()
            .map(|button| {
                let rect =
                    Rect::from_min_size(pos2(right - size.x, lane.top + theme.padding), size);
                right = rect.left() - theme.padding / 2.0;
                (button, rect)
            })
            .collect();
    placed.reverse();
    let fits = placed.iter().all(|(_, rect)| {
        part.contains_rect(*rect) && rect.bottom() <= lane.bottom() && rect.left() > headers.left()
    });
    if fits { placed } else { Vec::new() }
}

/// The corner over the headers: the grid's step, and whether clips snap
/// to it.
pub(super) fn corner(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let corner = layout.corner;
    painter.rect_filled(corner, 0.0, theme.ruler);
    let grid = scene.editor().grid;
    let tempo = &scene.project().tempo;
    let bar = tempo.bar_at(layout.viewport.start).bar;
    let length = tempo.bar_start(bar + 1) - tempo.bar_start(bar);
    let step = grid.step(layout.viewport.zoom, length).name(grid.triplet);
    let text = match (grid.snap, grid.fixed.is_some()) {
        (true, false) => format!("Grid {step}"),
        (true, true) => format!("Fixed grid {step}"),
        (false, _) => format!("Grid {step}, no snap"),
    };
    painter.text(
        corner.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(theme.small_text),
        theme.ruler_text,
    );
    let x = painter.round_to_pixel_center(corner.left() + 0.5);
    painter.vline(x, corner.y_range(), Stroke::new(1.0, theme.divider));
    let y = painter.round_to_pixel_center(corner.bottom() - 0.5);
    painter.hline(corner.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// Shows that files can be dropped while they're dragged over the window.
pub(super) fn drop_hint(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let lanes = scene.layout.lanes;
    let outline = Stroke::new(2.0, theme.start_marker);
    painter.rect_stroke(lanes.shrink(1.0), 0.0, outline, StrokeKind::Inside);
    painter.text(
        lanes.center(),
        Align2::CENTER_CENTER,
        "Drop audio files to insert them",
        FontId::proportional(theme.text),
        theme.header_text,
    );
}

/// Where something dragged from the browser or the devices panel would go:
/// its track's row, a new track's under the others, or the master's row,
/// with a line where a sample would start.
pub(super) fn destination(painter: &Painter, scene: &Scene<'_>, to: &Destination) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let across = layout.headers.left().min(layout.lanes.left())
        ..=layout.headers.right().max(layout.lanes.right());
    let x = egui::Rangef::new(*across.start(), *across.end());
    let part = match to {
        Destination::Chain(Some(Chain::Master)) => layout.master_part(x),
        _ => layout.tracks_part(x),
    };
    let painter = &painter.with_clip_rect(part.intersect(painter.clip_rect()));
    let outline = Stroke::new(2.0, theme.start_marker);
    let (rows, at) = match *to {
        Destination::Track { track, at, .. } => {
            let Some(row) = layout.rows.iter().find(|row| row.id == track) else {
                return;
            };
            (row.top..=row.bottom(), at)
        }
        Destination::NewTrack { at } => {
            let last_row = layout.rows.last().map(Row::bottom);
            let last_take = layout.takes.last().map(TakeRow::bottom);
            let top = (last_row.into_iter().chain(last_take))
                .reduce(f32::max)
                .unwrap_or(layout.lanes.top());
            let rows = top..=top + theme.track_height;
            let rect = Rect::from_x_y_ranges(across.clone(), rows.clone());
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "New track",
                FontId::proportional(theme.text),
                theme.header_text,
            );
            (rows, at)
        }
        Destination::Chain(Some(Chain::Master)) => {
            (layout.master.top..=layout.master.bottom(), None)
        }
        Destination::Chain(_) | Destination::Devices { .. } | Destination::Sampler(_) => return,
    };
    let rect = Rect::from_x_y_ranges(across, rows.clone());
    painter.rect_stroke(rect.shrink(1.0), 0.0, outline, StrokeKind::Inside);
    if let Some(at) = at {
        painter.vline(layout.x_of(at), rows, outline);
    }
}

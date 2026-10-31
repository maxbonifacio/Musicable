//! Drawing the Sample Editor: the waveform against the song's beats, with
//! the warp markers on it and their handles along its top, the transients'
//! ticks under them and the time selected, the ruler of the song's bars over
//! it and the ruler of the file's time under it, the lane of one of the
//! clip's parameters under them, and the column of the clip's controls
//! beside them. Only what's in sight is drawn.

use egui::{Align2, Color32, FontId, Mesh, Painter, Rect, Response, Shape, Stroke, pos2};
use musicable_commands::warp;
use musicable_model::automation::Point;
use musicable_model::clip::AudioClip;
use musicable_model::warp::WarpMarker;

use super::{Envelope, Plan, Scene, value_text};
use crate::theme::Theme;
use crate::timeline::{self, Timeline};
use crate::widgets::{clock, drop_down_mark};

/// The steps the ruler of the file's time can go in, in seconds: the
/// first that leaves room for its labels.
const TIME_STEPS: [f64; 19] = [
    0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0,
    120.0, 300.0, 600.0,
];

/// The song positions across the waveform, the rulers and the lane.
fn timeline<'s>(scene: &'s Scene<'_>) -> Timeline<'s> {
    Timeline {
        tempo: scene.tempo(),
        viewport: scene.layout.viewport,
        left: scene.layout.wave.left(),
    }
}

/// Where the playhead is drawn, if it is.
fn playhead(scene: &Scene<'_>) -> Option<f32> {
    let at = timeline::playhead(&scene.view.playback, scene.editor().start_marker)?;
    Some(scene.layout.x_of(at))
}

/// The start marker's line and the playhead's down `rect`.
fn lines(painter: &Painter, scene: &Scene<'_>, rect: Rect) {
    let theme = scene.theme;
    let x = painter.round_to_pixel_center(scene.layout.x_of(scene.editor().start_marker));
    painter.vline(x, rect.y_range(), Stroke::new(1.0, theme.start_marker));
    if let Some(x) = playhead(scene) {
        let x = painter.round_to_pixel_center(x);
        painter.vline(x, rect.y_range(), Stroke::new(1.0, theme.playhead));
    }
}

/// The clip's audio as `plan` would leave its warp, or as it is.
fn audio<'s>(scene: &'s Scene<'_>, planned: &'s Option<AudioClip>) -> &'s AudioClip {
    planned.as_ref().unwrap_or(scene.audio)
}

/// What the Sample Editor shows in `rect` while it has no audio clip to
/// show.
pub(super) fn empty(painter: &Painter, rect: Rect, theme: &Theme) {
    painter.rect_filled(rect, 0.0, theme.empty);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Select an audio clip to edit it here",
        FontId::proportional(theme.text),
        theme.ruler_text,
    );
}

/// The ruler over the waveform: the bar numbers, the start marker and the
/// playhead.
pub(super) fn ruler(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let ruler = layout.ruler;
    painter.rect_filled(ruler, 0.0, theme.ruler);
    timeline::bars(painter, theme, &timeline(scene), ruler, ruler.top());
    let x = layout.x_of(scene.editor().start_marker);
    timeline::start_marker(painter, theme, x, ruler);
    if let Some(x) = playhead(scene) {
        let x = painter.round_to_pixel_center(x);
        painter.vline(x, ruler.y_range(), Stroke::new(1.0, theme.playhead));
    }
    let y = painter.round_to_pixel_center(ruler.bottom() - 0.5);
    painter.hline(ruler.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// The waveform as `plan` would leave the warp: in the clip's colour where
/// the file is, as the arrangement draws it, over the grid, with the time
/// selected, shaded where the clip doesn't play, with the warp markers, the
/// transients, a pseudo marker at the file's frame `pseudo`, the start
/// marker and the playhead.
pub(super) fn wave(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>, pseudo: Option<u64>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let wave = layout.wave;
    let planned = scene.planned(plan);
    let audio = audio(scene, &planned);
    painter.rect_filled(wave, 0.0, theme.empty);
    // The file, from its first frame to its last.
    let (mut left, mut right) = (wave.left(), wave.right());
    if let Some(frames) = scene.file_frames() {
        left = scene.x_of_frame(audio, 0.0).max(left);
        right = scene.x_of_frame(audio, frames as f64).min(right);
    }
    if left < right {
        let file = Rect::from_x_y_ranges(left..=right, wave.y_range());
        painter.rect_filled(file, 0.0, theme.clip(scene.index));
        painter.rect_filled(file, 0.0, theme.clip_body);
    }
    timeline::grid(painter, theme, &timeline(scene), scene.drawn_grid(), wave);
    waveform(painter, scene, audio);
    time_selected(painter, scene);
    outside_clip(painter, scene, wave);
    if audio.warp.is_some() {
        painter.rect_filled(layout.handles, 0.0, theme.outside_clip);
    }
    transients(painter, scene, audio);
    markers(painter, scene, audio, plan);
    if let Some(frame) = pseudo {
        let x = scene.x_of_frame(audio, frame as f64);
        marker(painter, scene, x, theme.pseudo_marker);
    }
    lines(painter, scene, wave);
}

/// Shades the clip's own time selected, under the markers' handles.
fn time_selected(painter: &Painter, scene: &Scene<'_>) {
    let Some((start, end)) = scene.time_selected() else {
        return;
    };
    let layout = &scene.layout;
    let wave = layout.wave;
    let (left, right) = (start.max(wave.left()), end.min(wave.right()));
    if left < right {
        let shade = Rect::from_x_y_ranges(left..=right, layout.handles.bottom()..=wave.bottom());
        painter.rect_filled(shade, 0.0, scene.theme.time_selection);
    }
}

/// `audio`'s transients: a tick for each in sight, from the middle of the
/// strip of the markers' handles to the waveform's top, but those too near
/// the one before to tell apart.
fn transients(painter: &Painter, scene: &Scene<'_>, audio: &AudioClip) {
    let layout = &scene.layout;
    let (handles, wave) = (layout.handles, layout.wave);
    let top = handles.center().y;
    let bottom = (handles.bottom() + 2.0).min(wave.bottom());
    let stroke = Stroke::new(1.0, scene.theme.transient);
    let mut last = f32::NEG_INFINITY;
    for onset in scene.transients(audio, wave.left() - 1.0, wave.right() + 1.0) {
        let x = painter.round_to_pixel_center(scene.x_of_frame(audio, onset.frame as f64));
        if x - last >= 2.0 {
            painter.vline(x, top..=bottom, stroke);
            last = x;
        }
    }
}

/// The file's peaks, a column for each point, as `audio` plays them, at the
/// clip's gain, once the app has read them.
fn waveform(painter: &Painter, scene: &Scene<'_>, audio: &AudioClip) {
    let Some(peaks) = scene.peaks else {
        return;
    };
    let layout = &scene.layout;
    let body = Rect::from_x_y_ranges(
        layout.wave.x_range(),
        layout.handles.bottom()..=layout.wave.bottom(),
    );
    let middle = body.center().y;
    let half = (body.height() / 2.0 - 1.0).max(0.0);
    let height = |sample: f32| (sample * audio.gain).clamp(-1.0, 1.0) * half;
    let right = body.right().ceil();
    let mut mesh = Mesh::default();
    let mut x = body.left().floor();
    let mut from = scene.frame_at(audio, x);
    while x < right {
        let to = scene.frame_at(audio, x + 1.0);
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

/// Shades `rect` before the clip's start and after its end, where what's
/// there doesn't play.
fn outside_clip(painter: &Painter, scene: &Scene<'_>, rect: Rect) {
    let layout = &scene.layout;
    let x = |at| layout.x_of(at).clamp(rect.left(), rect.right());
    let (start, end) = (x(scene.clip.start), x(scene.clip.end()));
    for x in [rect.left()..=start, end..=rect.right()] {
        if x.start() < x.end() {
            let shade = Rect::from_x_y_ranges(x, rect.y_range());
            painter.rect_filled(shade, 0.0, scene.theme.outside_clip);
        }
    }
}

/// `audio`'s warp markers, as `plan` would leave them: a line down the
/// waveform for each, with a handle on the strip along its top, brighter
/// while it's selected.
fn markers(painter: &Painter, scene: &Scene<'_>, audio: &AudioClip, plan: Option<&Plan>) {
    let Some(warp) = &audio.warp else {
        return;
    };
    let theme = scene.theme;
    let layout = &scene.layout;
    let handles = layout.handles;
    let half = theme.sample_editor.handle / 2.0;
    // Moving markers or the audio under them keeps them in their order, so
    // the selection is the clip's markers' at the same places. A pseudo
    // marker dragged is a new marker, selected alone.
    let pseudo = match plan {
        Some(&Plan::Pseudo { frame, .. }) => Some(frame),
        _ => None,
    };
    let selected = |i: usize, marker: &WarpMarker| match pseudo {
        Some(frame) => marker.frame == frame,
        None => scene.markers().get(i).is_some_and(|m| scene.selected(m)),
    };
    for (i, marker) in warp.markers.iter().enumerate() {
        let x = layout.x_of(warp::song_position(scene.clip, audio, marker.beat));
        if x < handles.left() - half || x > handles.right() + half {
            continue;
        }
        let colour = match selected(i, marker) {
            true => theme.warp_marker_selected,
            false => theme.warp_marker,
        };
        self::marker(painter, scene, x, colour);
    }
}

/// A warp marker at `x` in `colour`: a line down the waveform, and a tag on
/// the strip along its top pointing down at it.
fn marker(painter: &Painter, scene: &Scene<'_>, x: f32, colour: Color32) {
    let layout = &scene.layout;
    let handles = layout.handles;
    let half = scene.theme.sample_editor.handle / 2.0;
    let x = painter.round_to_pixel_center(x);
    painter.vline(
        x,
        handles.bottom()..=layout.wave.bottom(),
        Stroke::new(1.0, colour),
    );
    let (top, bottom) = (handles.top() + 1.0, handles.bottom());
    let shoulder = bottom - half.min(handles.height() / 2.0);
    let tag = vec![
        pos2(x - half, top),
        pos2(x + half, top),
        pos2(x + half, shoulder),
        pos2(x, bottom),
        pos2(x - half, shoulder),
    ];
    painter.add(Shape::convex_polygon(tag, colour, Stroke::NONE));
}

/// The ruler under the waveform: the file's time, in steps that leave room
/// for their labels, from its first frame, where `audio`, as `plan` would
/// leave the warp, plays it.
pub(super) fn time(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let time = layout.time;
    painter.rect_filled(time, 0.0, theme.ruler);
    let y = painter.round_to_pixel_center(time.top() + 0.5);
    painter.hline(time.x_range(), y, Stroke::new(1.0, theme.divider));
    let planned = scene.planned(plan);
    let audio = audio(scene, &planned);
    let rate = f64::from(scene.rate);
    let seconds = |x: f32| scene.frame_at(audio, x) / rate;
    let (first, last) = (seconds(time.left()).max(0.0), seconds(time.right()));
    let last = (scene.file_frames()).map_or(last, |frames| last.min(frames as f64 / rate));
    if last.is_nan() || last <= first {
        return;
    }
    // From how far apart the seconds are across what's in sight.
    let per_second = (scene.x_of_frame(audio, last * rate) - scene.x_of_frame(audio, first * rate))
        / (last - first) as f32;
    let room = f64::from(theme.label_spacing / per_second.max(f32::EPSILON));
    let step = TIME_STEPS
        .into_iter()
        .find(|&step| step >= room)
        .unwrap_or(TIME_STEPS[TIME_STEPS.len() - 1]);
    let font = FontId::proportional(theme.small_text);
    let tick = Stroke::new(1.0, theme.grid_bar);
    let mut label_end = f32::NEG_INFINITY;
    let mut n = (first / step).ceil() as i64;
    while (n as f64) * step <= last {
        let at = n as f64 * step;
        let x = painter.round_to_pixel_center(scene.x_of_frame(audio, at * rate));
        painter.vline(x, time.top()..=time.top() + time.height() / 3.0, tick);
        // Where the audio's squeezed, labels that would crowd are left out.
        if x >= label_end {
            let pos = pos2(x + theme.padding / 2.0, time.bottom());
            let label = time_label(at, step);
            let drawn = painter.text(
                pos,
                Align2::LEFT_BOTTOM,
                label,
                font.clone(),
                theme.ruler_text,
            );
            label_end = drawn.right() + theme.padding;
        }
        n += 1;
    }
}

/// `seconds` into the file as the ruler of its time labels it, as finely
/// as its steps of `step` seconds need: `0:01`, `0:01.5`, `0:01.250`.
fn time_label(seconds: f64, step: f64) -> String {
    let decimals = match step {
        step if step >= 1.0 => 0,
        step if step >= 0.1 => 1,
        step if step >= 0.01 => 2,
        _ => 3,
    };
    let full = clock(seconds + 0.000_5);
    // `clock` gives milliseconds: as many of them as the step needs.
    match decimals {
        0 => full[..full.len() - 4].to_owned(),
        decimals => full[..full.len() - 3 + decimals].to_owned(),
    }
}

/// The lane, over the grid: the line through the points of the curve it
/// shows as `plan` would leave them, level before the first and after the
/// last, or a dashed line at the parameter's own value where it has no
/// curve, shaded where the clip doesn't play, and greyed where the clip's
/// mode doesn't play the parameter.
pub(super) fn lane(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let lane = layout.lane;
    painter.rect_filled(lane, 0.0, theme.automation_lane);
    timeline::grid(painter, theme, &timeline(scene), scene.drawn_grid(), lane);
    let planned = match plan {
        Some(Plan::Points(points)) => Some(&points[..]),
        _ => None,
    };
    let points = planned.unwrap_or(scene.curve());
    if points.is_empty() {
        let y = painter.round_to_pixel_center(scene.y_of(scene.level()));
        let line = [pos2(lane.left(), y), pos2(lane.right(), y)];
        let stroke = Stroke::new(1.0, theme.automation_off);
        painter.extend(Shape::dashed_line(&line, stroke, 4.0, 4.0));
    } else {
        // The point under the pointer is filled in.
        let hovered = (scene.pointer)
            .filter(|&pos| planned.is_none() && lane.contains(pos))
            .and_then(|pos| match scene.envelope_part(pos) {
                Envelope::Point(i) => Some(i),
                _ => None,
            });
        envelope(painter, scene, points, hovered);
    }
    outside_clip(painter, scene, lane);
    lines(painter, scene, lane);
    let y = painter.round_to_pixel_center(lane.top() + 0.5);
    painter.hline(lane.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// The line through `points`, level before the first and after the last,
/// and for a parameter in steps, level from each to the next, and the
/// points in sight, with the one at `hovered` filled in.
fn envelope(painter: &Painter, scene: &Scene<'_>, points: &[Point], hovered: Option<usize>) {
    let theme = scene.theme;
    let lane = scene.layout.lane;
    let colour = match scene.lane_plays() {
        true => theme.automation,
        false => theme.automation_off,
    };
    let at: Vec<_> = points.iter().map(|point| scene.point_at(point)).collect();
    let stepped = scene.lane().stepped();
    let mut line = Vec::with_capacity(2 * at.len() + 2);
    line.push(pos2(lane.left(), at[0].y));
    for (i, &pos) in at.iter().enumerate() {
        if stepped && i > 0 {
            line.push(pos2(pos.x, at[i - 1].y));
        }
        line.push(pos);
    }
    line.push(pos2(lane.right(), at[at.len() - 1].y));
    painter.add(Shape::line(line, Stroke::new(1.5, colour)));
    let reach = lane.x_range().expand(theme.point);
    for (i, &pos) in at.iter().enumerate() {
        if !reach.contains(pos.x) {
            continue;
        }
        let fill = match hovered == Some(i) {
            true => colour,
            false => theme.automation_lane,
        };
        painter.circle(pos, theme.point, fill, Stroke::new(1.5, colour));
    }
}

/// The name of what the lane shows, beside it: "Gain", or another of the
/// clip's parameters, with a triangle after it while a click on it,
/// `chooser`, chooses another. Under it, the mode that doesn't play it, if
/// the clip's mode doesn't, "Not in Re-Pitch", and the value `readout`, if
/// there's one to show.
pub(super) fn label(
    painter: &Painter,
    scene: &Scene<'_>,
    chooser: Option<&Response>,
    readout: Option<f64>,
) {
    let theme = scene.theme;
    let label = scene.layout.label;
    let fill = match chooser.is_some_and(Response::hovered) {
        true => theme.header,
        false => theme.ruler,
    };
    painter.rect_filled(label, 0.0, fill);
    let font = FontId::proportional(theme.small_text);
    let at = pos2(label.left() + theme.padding, label.top() + theme.padding);
    let name = scene.spec().name;
    let name = painter.text(at, Align2::LEFT_TOP, name, font.clone(), theme.ruler_text);
    if chooser.is_some() {
        drop_down_mark(painter, theme, name);
    }
    let mut bottom = name.bottom();
    let mut line = |text: String, colour: Color32| {
        let under = pos2(at.x, bottom + 2.0);
        bottom = (painter.text(under, Align2::LEFT_TOP, text, font.clone(), colour)).bottom();
    };
    if let Some(warp) = scene.warp().filter(|_| !scene.lane_plays()) {
        line(format!("Not in {}", warp.mode.name()), theme.automation_off);
    }
    if let Some(value) = readout {
        line(value_text(scene.lane(), value), theme.ruler_text);
    }
    let y = painter.round_to_pixel_center(label.top() + 0.5);
    painter.hline(label.x_range(), y, Stroke::new(1.0, theme.divider));
    let x = painter.round_to_pixel_center(label.right() - 0.5);
    painter.vline(x, label.y_range(), Stroke::new(1.0, theme.divider));
}

/// The column of the clip's controls, under the clip's name, in its
/// track's colour down its left.
pub(super) fn column(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let column = scene.layout.controls;
    painter.rect_filled(column, 0.0, theme.ruler);
    let strip = Rect::from_x_y_ranges(
        column.left()..=column.left() + theme.colour_strip,
        column.y_range(),
    );
    painter.rect_filled(strip, 0.0, theme.clip(scene.index));
    let name = pos2(
        strip.right() + theme.padding,
        column.top() + theme.sample_editor.ruler / 2.0,
    );
    painter.text(
        name,
        Align2::LEFT_CENTER,
        &scene.clip.name,
        FontId::proportional(theme.small_text),
        theme.ruler_text,
    );
    let x = painter.round_to_pixel_center(column.right() - 0.5);
    painter.vline(x, column.y_range(), Stroke::new(1.0, theme.divider));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_s_time_is_labelled_as_finely_as_its_steps_need() {
        assert_eq!(time_label(0.0, 1.0), "0:00");
        assert_eq!(time_label(61.0, 5.0), "1:01");
        assert_eq!(time_label(1.5, 0.5), "0:01.5");
        assert_eq!(time_label(1.25, 0.05), "0:01.25");
        assert_eq!(time_label(1.25, 0.005), "0:01.250");
        // Not a millisecond short, as a sum of steps can be.
        assert_eq!(time_label(0.3 - 1e-12, 0.1), "0:00.3");
    }
}

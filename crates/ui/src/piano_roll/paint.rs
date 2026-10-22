//! Drawing the piano roll: the rows of keys with the notes on them over the
//! grid, the keyboard beside them, the ruler over them, the lane of their
//! velocities or a controller's curve under them, and the corners. Only
//! what's in sight is drawn.

use egui::{Align2, FontId, Painter, Rect, Response, Shape, Stroke, StrokeKind, Vec2, pos2};
use musicable_commands::editor::Lane;
use musicable_model::clip::{self, Controller, Note, Point};
use musicable_model::time::{BeatRange, Beats};

use super::{Plan, Scene, Side};
use crate::theme::Theme;
use crate::timeline::{self, Timeline};

/// The least room a note needs for its name, in points.
const NAME_WIDTH: f32 = 28.0;

/// Whether `key` is one of the black keys: C#, D#, F#, G# and A#.
fn black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// The song positions across the rows.
fn timeline<'s>(scene: &'s Scene<'_>) -> Timeline<'s> {
    Timeline {
        tempo: &scene.project().tempo,
        viewport: scene.layout.viewport,
        left: scene.layout.rows.left(),
    }
}

/// Where the playhead is drawn, if it is.
fn playhead(scene: &Scene<'_>) -> Option<f32> {
    let at = timeline::playhead(&scene.view.playback, scene.editor().start_marker)?;
    Some(scene.layout.x_of(at))
}

/// What the piano roll shows in `rect` while it has no MIDI clip to show.
pub(super) fn empty(painter: &Painter, rect: Rect, theme: &Theme) {
    painter.rect_filled(rect, 0.0, theme.empty);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Select a MIDI clip to edit its notes here",
        FontId::proportional(theme.text),
        theme.ruler_text,
    );
}

/// The rows of keys, the black keys' darker, with the notes over the grid,
/// shaded where the clip doesn't play, the start marker and the playhead,
/// and where `plan` would put notes.
pub(super) fn rows(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let rows = layout.rows;
    painter.rect_filled(rows, 0.0, theme.white_row);
    for key in layout.visible_keys() {
        let top = layout.y_of(key);
        let row = Rect::from_x_y_ranges(rows.x_range(), top..=top + layout.key_height);
        if black(key) {
            painter.rect_filled(row, 0.0, theme.black_row);
        }
        // A line under each row, when they're tall enough to need one, and
        // a brighter one under each C, where an octave starts.
        let colour = match key % 12 {
            0 => theme.grid_beat,
            _ if layout.key_height >= 8.0 => theme.grid_step,
            _ => continue,
        };
        let y = painter.round_to_pixel_center(row.bottom() - 0.5);
        painter.hline(rows.x_range(), y, Stroke::new(1.0, colour));
    }
    timeline::grid(painter, theme, &timeline(scene), scene.editor().grid, rows);
    notes(painter, scene, plan);
    // Notes outside the clip don't play.
    outside_clip(painter, scene, rows);
    if let Some(plan) = plan {
        ghosts(painter, scene, plan);
    }
    let x = painter.round_to_pixel_center(layout.x_of(scene.editor().start_marker));
    painter.vline(x, rows.y_range(), Stroke::new(1.0, theme.start_marker));
    if let Some(x) = playhead(scene) {
        let x = painter.round_to_pixel_center(x);
        painter.vline(x, rows.y_range(), Stroke::new(1.0, theme.playhead));
    }
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

/// The velocity `note` would have if `plan` were carried out.
fn velocity(note: &Note, plan: Option<&Plan>) -> f32 {
    match plan {
        Some(Plan::Velocity { notes, by }) if notes.contains(&note.id) => {
            (note.velocity + by).clamp(0.0, 1.0)
        }
        _ => note.velocity,
    }
}

/// The notes in sight, in their track's colour, fainter the softer they
/// start, outlined while selected, with their names on them where there's
/// room.
fn notes(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let keys = layout.visible_keys();
    let visible = layout.rows.x_range();
    let colour = theme.clip(scene.index);
    let middle_c = scene.editor().behaviour.middle_c;
    for note in &scene.midi.notes {
        let rect = scene.note_rect(note);
        if !keys.contains(&note.key) || rect.right() < visible.min || rect.left() > visible.max {
            continue;
        }
        let fill = colour.gamma_multiply(0.35 + 0.65 * velocity(note, plan));
        let (width, outline) = match scene.selected(note.id) {
            true => (1.5, theme.clip_selected),
            false => (1.0, theme.divider),
        };
        painter.rect(
            rect,
            1.0,
            fill,
            Stroke::new(width, outline),
            StrokeKind::Inside,
        );
        if rect.width() >= NAME_WIDTH && rect.height() >= theme.small_text {
            // The name stays in sight while the note's start is scrolled
            // away.
            let left = rect.left().max(visible.min) + theme.padding;
            painter
                .with_clip_rect(rect.intersect(painter.clip_rect()))
                .text(
                    pos2(left, rect.center().y),
                    Align2::LEFT_CENTER,
                    middle_c.name(note.key),
                    FontId::proportional(theme.small_text),
                    theme.clip_text,
                );
        }
    }
}

/// Where the notes a drag moves, trims or draws will be, and the rectangle
/// that selects notes.
fn ghosts(painter: &Painter, scene: &Scene<'_>, plan: &Plan) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let outline = Stroke::new(1.0, theme.clip_selected);
    let ghost = |rect: Rect| {
        painter.rect(rect, 1.0, theme.ghost, outline, StrokeKind::Inside);
    };
    match plan {
        Plan::Move {
            notes, by, keys, ..
        } => {
            for note in scene.notes(notes) {
                let start = scene.song(note.start) + *by;
                let key = (i64::from(note.key) + keys).clamp(0, i64::from(Note::MAX_KEY));
                ghost(layout.note(key as u8, start, start + note.length));
            }
        }
        Plan::Trim { notes, side, by } => {
            // As Trim Notes trims them: never shorter than a tick.
            let tick = Beats::from_ticks(1);
            for note in scene.notes(notes) {
                let (start, end) = (scene.song(note.start), scene.song(note.end()));
                let (start, end) = match side {
                    Side::Start => ((start + *by).min(end - tick).max(Beats::ZERO), end),
                    Side::End => (start, (end + *by).max(start + tick)),
                };
                ghost(layout.note(note.key, start, end));
            }
        }
        Plan::Draw { notes } => {
            for &(at, key) in notes {
                ghost(layout.note(key, at, at + scene.step_at(at)));
            }
        }
        Plan::Area { rect, .. } => {
            let outline = Stroke::new(1.0, theme.start_marker);
            painter.rect(*rect, 0.0, theme.area, outline, StrokeKind::Inside);
        }
        Plan::Velocity { .. }
        | Plan::DrawController { .. }
        | Plan::EraseController { .. }
        | Plan::Viewport(_) => {}
    }
}

/// The keyboard beside the rows: the white keys and the black, with the
/// names of the Cs.
pub(super) fn keys(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let keys = layout.keys;
    painter.rect_filled(keys, 0.0, theme.white_key);
    let middle_c = scene.editor().behaviour.middle_c;
    for key in layout.visible_keys() {
        let top = layout.y_of(key);
        let row = Rect::from_x_y_ranges(keys.x_range(), top..=top + layout.key_height);
        if black(key) {
            // The black keys are shorter, as on a keyboard.
            let width = keys.width() * 0.6;
            let short = Rect::from_x_y_ranges(keys.left()..=keys.left() + width, row.y_range());
            painter.rect_filled(short, 0.0, theme.black_key);
            continue;
        }
        // Lines between the white keys where no black key is: under each C
        // and each F.
        if matches!(key % 12, 0 | 5) {
            let y = painter.round_to_pixel_center(row.bottom() - 0.5);
            painter.hline(keys.x_range(), y, Stroke::new(1.0, theme.key_text));
        }
        if key % 12 == 0 && layout.key_height >= theme.small_text - 2.0 {
            painter.text(
                pos2(keys.right() - theme.padding, row.center().y),
                Align2::RIGHT_CENTER,
                middle_c.name(key),
                FontId::proportional(theme.small_text),
                theme.key_text,
            );
        }
    }
    let x = painter.round_to_pixel_center(keys.right() - 0.5);
    painter.vline(x, keys.y_range(), Stroke::new(1.0, theme.divider));
}

/// The ruler over the rows: the bar numbers, the start marker and the
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

/// The lane under the rows, over the grid: the notes' velocities, or the
/// curve of the controller it shows, as `plan` would leave them.
pub(super) fn lane(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let lane = scene.layout.lane;
    painter.rect_filled(lane, 0.0, theme.empty);
    timeline::grid(painter, theme, &timeline(scene), scene.editor().grid, lane);
    match scene.controller() {
        Some(controller) => curve(painter, scene, controller, plan),
        None => velocities(painter, scene, plan),
    }
    let y = painter.round_to_pixel_center(lane.top() + 0.5);
    painter.hline(lane.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// The velocities: a stem for each note in sight, as tall as it starts
/// hard, at its start.
fn velocities(painter: &Painter, scene: &Scene<'_>, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let lane = layout.lane;
    let colour = theme.clip(scene.index);
    let (bottom, height) = layout.span(theme);
    // The selected on top.
    let mut notes: Vec<&Note> = scene.midi.notes.iter().collect();
    notes.sort_by_key(|note| scene.selected(note.id));
    for note in notes {
        let x = layout.x_of(scene.song(note.start));
        if x < lane.left() - theme.marker || x > lane.right() + theme.marker {
            continue;
        }
        let top = bottom - height * velocity(note, plan);
        let selected = scene.selected(note.id);
        let stem = if selected {
            theme.clip_selected
        } else {
            colour
        };
        let x = painter.round_to_pixel_center(x);
        painter.vline(x, top..=bottom, Stroke::new(1.0, stem));
        let head = Rect::from_center_size(pos2(x, top), Vec2::splat(theme.marker * 0.8));
        painter.rect_filled(head, 1.0, stem);
    }
}

/// `controller`'s curve as the clip moves it, or as `plan` would leave it:
/// each point's value held until the next point, filled under, from the
/// first point on, as the clip leaves the controller alone before it. It's
/// shaded where the clip doesn't play.
fn curve(painter: &Painter, scene: &Scene<'_>, controller: Controller, plan: Option<&Plan>) {
    let theme = scene.theme;
    let layout = &scene.layout;
    let lane = layout.lane;
    let curve = scene.midi.controller(controller);
    let own = |range: &BeatRange| BeatRange::new(scene.own(range.start), scene.own(range.end));
    let curve = match plan {
        Some(Plan::DrawController {
            controller: drawn,
            range,
            points,
        }) if *drawn == controller => {
            let points: Vec<Point> = points
                .iter()
                .map(|&(at, value)| Point::new(scene.own(at), value))
                .collect();
            clip::redraw(curve, own(range), &points)
        }
        Some(Plan::EraseController {
            controller: erased,
            range,
        }) if *erased == controller => clip::redraw(curve, own(range), &[]),
        _ => curve.to_vec(),
    };
    let colour = theme.clip(scene.index);
    let line = Stroke::new(1.5, colour);
    let fill = colour.gamma_multiply(0.3);
    let (foot, height) = layout.span(theme);
    let y = |value: f32| foot - height * value;
    let visible = lane.x_range();
    for (i, point) in curve.iter().enumerate() {
        let next = curve.get(i + 1);
        let left = layout.x_of(scene.song(point.at));
        let right = next.map_or(visible.max, |next| layout.x_of(scene.song(next.at)));
        if right < visible.min || left > visible.max {
            continue;
        }
        let (left, right) = (left.max(visible.min - 1.0), right.min(visible.max + 1.0));
        let top = y(point.value);
        if right > left {
            let under = Rect::from_min_max(pos2(left, top), pos2(right, foot));
            painter.rect_filled(under, 0.0, fill);
            painter.hline(left..=right, top, line);
        }
        // The step up or down to the next point's value.
        if let Some(next) = next {
            let to = y(next.value);
            painter.vline(right, top.min(to)..=top.max(to), line);
        }
    }
    outside_clip(painter, scene, lane);
}

/// The corner over the keyboard: the name of the clip the piano roll shows.
pub(super) fn corner(painter: &Painter, scene: &Scene<'_>) {
    let theme = scene.theme;
    let corner = scene.layout.corner;
    painter.rect_filled(corner, 0.0, theme.ruler);
    painter.text(
        pos2(corner.left() + theme.padding, corner.center().y),
        Align2::LEFT_CENTER,
        &scene.clip.name,
        FontId::proportional(theme.small_text),
        theme.ruler_text,
    );
    let y = painter.round_to_pixel_center(corner.bottom() - 0.5);
    painter.hline(corner.x_range(), y, Stroke::new(1.0, theme.divider));
}

/// The name of what the lane shows, beside it, under the keyboard, with a
/// triangle that says a click on it, `chooser`, chooses: "Velocity", or a
/// controller's number over its name.
pub(super) fn label(painter: &Painter, scene: &Scene<'_>, chooser: &Response) {
    let theme = scene.theme;
    let label = scene.layout.label;
    let fill = match chooser.hovered() {
        true => theme.header,
        false => theme.ruler,
    };
    painter.rect_filled(label, 0.0, fill);
    let (name, detail) = match scene.editor().piano_roll.lane {
        Lane::Velocity => ("Velocity".to_owned(), None),
        Lane::Controller(controller) => (format!("CC {}", controller.number()), controller.name()),
    };
    let font = FontId::proportional(theme.small_text);
    let at = pos2(label.left() + theme.padding, label.top() + theme.padding);
    let name = painter.text(at, Align2::LEFT_TOP, name, font.clone(), theme.ruler_text);
    if let Some(detail) = detail {
        let under = pos2(at.x, name.bottom() + 2.0);
        painter.text(under, Align2::LEFT_TOP, detail, font, theme.ruler_text);
    }
    // A triangle pointing down, after the name.
    let size = theme.small_text * 0.5;
    let left = name.right() + theme.padding;
    let top = name.center().y - size / 4.0;
    let triangle = vec![
        pos2(left, top),
        pos2(left + size, top),
        pos2(left + size / 2.0, top + size / 2.0),
    ];
    painter.add(Shape::convex_polygon(
        triangle,
        theme.ruler_text,
        Stroke::NONE,
    ));
    let y = painter.round_to_pixel_center(label.top() + 0.5);
    painter.hline(label.x_range(), y, Stroke::new(1.0, theme.divider));
}

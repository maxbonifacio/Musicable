//! Small widgets for the shell and the app's windows, and how they write
//! positions and times.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::Arc;

use egui::text::TextWrapping;
use egui::{
    Align, Align2, Button, Color32, FontId, FontSelection, Id, Key, Label, Modal, Modifiers,
    Painter, Rect, Response, RichText, Role, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    WidgetInfo, WidgetText, pos2, vec2,
};
use musicable_model::time::{Beats, TempoMap};

use crate::theme::Theme;

/// A position in bars, beats and sixteenths, counting from one as DAWs show
/// them: `5. 2. 3` is the third sixteenth of the second beat of bar 5.
pub fn position(tempo: &TempoMap, at: Beats) -> String {
    let at = tempo.bar_at(at);
    let sixteenth = at.offset.ticks() / (Beats::TICKS_PER_BEAT / 4);
    format!("{}. {}. {}", at.bar + 1, at.beat + 1, sixteenth + 1)
}

/// A time in minutes, seconds and milliseconds: `1:05.250`. It rounds down,
/// so it never shows a time that hasn't come yet.
pub fn clock(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0) as u64;
    format!(
        "{}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

/// The transport's drawn icons. egui's default fonts have no media symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    ToStart,
    Play,
    Stop,
    Record,
}

impl Icon {
    /// What a screen reader calls the button.
    fn label(self) -> &'static str {
        match self {
            Self::ToStart => "Go to Start",
            Self::Play => "Play",
            Self::Stop => "Stop",
            Self::Record => "Record",
        }
    }
}

/// A button showing `icon`, lit while `selected`.
pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, selected: bool, theme: &Theme) -> Response {
    let size = vec2(theme.icon_button, ui.spacing().interact_size.y + 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, icon.label()));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        let painter = ui.painter();
        painter.rect(
            rect.expand(visuals.expansion),
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
        let color = match (icon, selected) {
            (Icon::Record, true) => theme.arm,
            (_, true) => ui.visuals().selection.stroke.color,
            (_, false) => visuals.fg_stroke.color,
        };
        let c = rect.center();
        let r = theme.icon;
        match icon {
            Icon::ToStart => {
                let bar = Rect::from_min_max(pos2(c.x - r, c.y - r), pos2(c.x - r + 2.0, c.y + r));
                painter.rect_filled(bar, 0.0, color);
                let points = vec![
                    pos2(c.x + r, c.y - r),
                    pos2(c.x + r, c.y + r),
                    pos2(c.x - r + 2.5, c.y),
                ];
                painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
            }
            Icon::Play => {
                let points = vec![
                    pos2(c.x - r * 0.8, c.y - r),
                    pos2(c.x + r, c.y),
                    pos2(c.x - r * 0.8, c.y + r),
                ];
                painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
            }
            Icon::Stop => {
                let square = Rect::from_center_size(c, Vec2::splat(2.0 * r * 0.85));
                painter.rect_filled(square, 1.0, color);
            }
            Icon::Record => {
                painter.circle_filled(c, r * 0.9, color);
            }
        }
    }
    response
}

/// A horizontal meter `size` big, full at 1.0. It turns yellow above 70%
/// and red above 90%, where an audio thread starts to run late.
pub(crate) fn meter(ui: &mut Ui, value: Option<f32>, size: Vec2, theme: &Theme) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo {
        value: value.map(f64::from),
        ..WidgetInfo::labeled(Role::Meter, true, "CPU load")
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
        if let Some(value) = value {
            let value = value.clamp(0.0, 1.0);
            let color = if value > 0.9 {
                theme.meter_over
            } else if value > 0.7 {
                theme.meter_loud
            } else {
                theme.meter_quiet
            };
            let mut filled = rect;
            filled.set_width(rect.width() * value);
            painter.rect_filled(filled, 2.0, color);
        }
        painter.rect_stroke(
            rect,
            2.0,
            visuals.widgets.noninteractive.bg_stroke,
            StrokeKind::Inside,
        );
    }
    response
}

/// How a toggle button shows what it does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Face<'a> {
    Text(&'a str),
    /// A dot, as record buttons have.
    Dot,
    /// A triangle pointing down, for a button that opens a menu.
    Menu,
    /// A power symbol, a ring with a bar through its top, as the Bypass
    /// buttons of devices have.
    Power,
}

/// A button `width` wide and a row of the mixer high that's lit in `color`
/// while `on`, named `label` for screen readers.
pub(crate) fn toggle(
    ui: &mut Ui,
    face: Face<'_>,
    on: bool,
    color: Color32,
    label: &str,
    size: Vec2,
    theme: &Theme,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, label));
    paint_toggle(ui, rect, &response, face, on, color, theme);
    response
}

/// Paints a toggle button at `rect`, which `response` is for, lit in `color`
/// while `on`.
pub(crate) fn paint_toggle(
    ui: &Ui,
    rect: Rect,
    response: &Response,
    face: Face<'_>,
    on: bool,
    color: Color32,
    theme: &Theme,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let visuals = ui.style().interact(response);
    let (fill, ink) = if on {
        (color, theme.ink)
    } else {
        (visuals.weak_bg_fill, visuals.text_color())
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        theme.corner,
        fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    match face {
        Face::Text(text) => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(theme.button_text),
                ink,
            );
        }
        Face::Dot => {
            painter.circle_filled(rect.center(), rect.height().min(rect.width()) * 0.22, ink);
        }
        Face::Menu => {
            let c = rect.center();
            let r = rect.height().min(rect.width()) * 0.22;
            let points = vec![
                pos2(c.x - r, c.y - r / 2.0),
                pos2(c.x + r, c.y - r / 2.0),
                pos2(c.x, c.y + r / 2.0 + 1.0),
            ];
            painter.add(Shape::convex_polygon(points, ink, Stroke::NONE));
        }
        Face::Power => {
            let c = rect.center();
            let r = rect.height().min(rect.width()) * 0.26;
            let stroke = Stroke::new(1.5, ink);
            // From the top's right round to its left, clockwise, leaving a
            // gap for the bar.
            let gap = 0.28 * PI;
            let steps = 24;
            let ring = (0..=steps).map(|i| {
                let angle = -FRAC_PI_2 + gap + (TAU - 2.0 * gap) * i as f32 / steps as f32;
                c + r * vec2(angle.cos(), angle.sin())
            });
            painter.add(Shape::line(ring.collect(), stroke));
            painter.line_segment([c - vec2(0.0, 1.2 * r), c - vec2(0.0, 0.2 * r)], stroke);
        }
    }
}

/// A small triangle pointing down, after a name drawn at `name`, that says
/// a click on the name opens a menu, as a lane's name beside it does.
pub(crate) fn drop_down_mark(painter: &Painter, theme: &Theme, name: Rect) {
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
}

/// What a panel says while it has nothing to show: `text`, weak, in the
/// middle of the room `ui` has left and wrapped to it. The label is only as
/// big as its text, so the pointer turns to the text cursor over the text,
/// as it does over other labels, and not over the whole panel.
pub(crate) fn placeholder(ui: &mut Ui, text: &str) -> Response {
    let room = ui.available_rect_before_wrap();
    let text = WidgetText::from(RichText::new(text).weak());
    let job = text.into_layout_job(ui.style(), FontSelection::Default, ui.text_valign());
    let mut job = Arc::unwrap_or_clone(job);
    job.wrap = TextWrapping::wrap_at_width(room.width());
    job.halign = Align::Center;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let rect = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), room);
    // From the top, if the room's too short for it, so it's never over what
    // the panel has above it.
    let rect = rect.translate(vec2(0.0, (room.top() - rect.top()).max(0.0)));
    ui.put(rect, Label::new(galley))
}

/// A box over the window that asks for a name: `title` over a field with
/// `text` in it, all of it selected to be typed over when the box is
/// `first` shown, and buttons to `confirm` it or cancel. Enter confirms,
/// and Escape or a click outside cancels. Returns whether the user
/// confirmed, once they have or have cancelled. `id` tells boxes apart.
pub(crate) fn name_box(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    confirm: &str,
    text: &mut String,
    first: bool,
) -> Option<bool> {
    checked_name_box(ctx, id, title, confirm, text, first, |_| None)
}

/// [`name_box`], which says under the field what `problem` finds wrong
/// with the name typed, and can't be confirmed while it finds something.
pub fn checked_name_box(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    confirm: &str,
    text: &mut String,
    first: bool,
    problem: impl Fn(&str) -> Option<String>,
) -> Option<bool> {
    let (enter, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    let mut done = None;
    // Whether the name can be confirmed.
    let modal = Modal::new(Id::unique(id)).show(ctx, |ui| {
        ui.set_width(ui.spacing().text_edit_width);
        let heading = ui.heading(title);
        let field = TextEdit::singleline(text).desired_width(f32::INFINITY);
        let field = ui.add(field).labelled_by(heading.id);
        if first {
            // The whole name, ready to be typed over.
            if let Some(mut state) = TextEdit::load_state(ui.ctx(), field.id) {
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(text.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), field.id);
            }
        }
        field.request_focus();
        let problem = problem(text);
        if let Some(problem) = &problem {
            ui.colored_label(ui.visuals().warn_fg_color, problem);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(problem.is_none(), Button::new(confirm))
                .clicked()
            {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() {
                done = Some(false);
            }
        });
        problem.is_none()
    });
    if enter && modal.inner {
        done = Some(true);
    } else if escape || modal.backdrop_response.clicked() {
        done = done.or(Some(false));
    }
    done
}

/// A box over the window that asks `question`, with `detail` under it,
/// and buttons to `confirm` or cancel. Escape or a click outside cancels.
/// Returns whether the user confirmed, once they have or have cancelled.
pub fn confirm_box(
    ctx: &egui::Context,
    question: &str,
    detail: &str,
    confirm: &str,
) -> Option<bool> {
    let modal = Modal::new(Id::unique(question)).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.heading(question);
        ui.label(detail);
        ui.add_space(10.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            if ui.button(confirm).clicked() {
                choice = Some(true);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(false);
            }
        });
        choice
    });
    if modal.inner.is_none() && modal.should_close() {
        return Some(false);
    }
    modal.inner
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use egui::CursorIcon;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use musicable_model::time::TimeSignature;

    use super::*;
    use crate::testing::cursor_at;

    const NOTHING: &str = "Nothing to show here yet.";

    #[test]
    fn a_placeholder_takes_the_text_cursor_only_over_its_words() {
        let room = Cell::new(Rect::NOTHING);
        let mut harness = Harness::builder().with_size([600.0, 300.0]).build_ui(|ui| {
            room.set(ui.available_rect_before_wrap());
            placeholder(ui, NOTHING);
        });
        let room = room.get();
        // In the middle of the room, no bigger than its words.
        let words = harness.get_by_label(NOTHING).rect();
        let off = words.center().distance(room.center());
        assert!(off < 1.0, "{words:?} {room:?}");
        assert!(words.width() < room.width() / 2.0, "{words:?}");
        assert!(words.height() < room.height() / 4.0, "{words:?}");
        assert_eq!(cursor_at(&mut harness, words.center()), CursorIcon::Text);
        let corner = room.left_top() + vec2(10.0, 10.0);
        assert_eq!(cursor_at(&mut harness, corner), CursorIcon::Default);
    }

    #[test]
    fn a_placeholder_too_tall_for_its_room_starts_at_the_top() {
        let room = Cell::new(Rect::NOTHING);
        let harness = Harness::builder().with_size([600.0, 300.0]).build_ui(|ui| {
            ui.allocate_ui(vec2(60.0, 10.0), |ui| {
                room.set(ui.available_rect_before_wrap());
                placeholder(ui, NOTHING);
            });
        });
        let room = room.get();
        // Wrapped to the room, so a few lines tall.
        let words = harness.get_by_label(NOTHING).rect();
        assert!(words.height() > 2.0 * room.height(), "{words:?} {room:?}");
        assert_eq!(words.top(), room.top());
    }

    #[test]
    fn positions_count_from_one() {
        let tempo = TempoMap::default();
        assert_eq!(position(&tempo, Beats::ZERO), "1. 1. 1");
        assert_eq!(position(&tempo, Beats::new(5)), "2. 2. 1");
        assert_eq!(position(&tempo, Beats::from_ratio(19, 4)), "2. 1. 4");
        // In 6/8 a bar has six eighth notes, each two sixteenths long.
        let mut tempo = TempoMap::default();
        tempo.set_signature(TimeSignature::new(6, 8).unwrap());
        assert_eq!(position(&tempo, Beats::from_ratio(7, 2)), "2. 2. 1");
        assert_eq!(position(&tempo, Beats::from_ratio(15, 4)), "2. 2. 2");
    }

    #[test]
    fn clocks_round_down() {
        assert_eq!(clock(0.0), "0:00.000");
        assert_eq!(clock(65.2509), "1:05.250");
        assert_eq!(clock(3599.9999), "59:59.999");
        assert_eq!(clock(-1.0), "0:00.000");
    }
}

//! Small widgets for the shell, and how it writes positions and times.

use egui::{
    Align2, Button, Color32, FontId, Id, Key, Modal, Modifiers, Rect, Response, Role, Sense, Shape,
    Stroke, StrokeKind, TextEdit, Ui, Vec2, WidgetInfo, pos2, vec2,
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

/// A horizontal meter, full at 1.0. It turns yellow above 70% and red above
/// 90%, where an audio thread starts to run late.
pub(crate) fn meter(ui: &mut Ui, value: Option<f32>, theme: &Theme) -> Response {
    let (rect, response) = ui.allocate_exact_size(theme.cpu_meter, Sense::hover());
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
pub(crate) enum Face {
    Text(&'static str),
    /// A dot, as record buttons have.
    Dot,
    /// A triangle pointing down, for a button that opens a menu.
    Menu,
}

/// A button `width` wide and a row of the mixer high that's lit in `color`
/// while `on`, named `label` for screen readers.
pub(crate) fn toggle(
    ui: &mut Ui,
    face: Face,
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
    face: Face,
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
    }
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
pub(crate) fn checked_name_box(
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

#[cfg(test)]
mod tests {
    use musicable_model::time::TimeSignature;

    use super::*;

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

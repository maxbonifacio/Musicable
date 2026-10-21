//! Small widgets for the shell, and how it writes positions and times.

use egui::{
    Color32, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2,
    vec2,
};
use musicable_model::time::{Beats, TempoMap};

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
}

impl Icon {
    /// What a screen reader calls the button.
    fn label(self) -> &'static str {
        match self {
            Self::ToStart => "Go to Start",
            Self::Play => "Play",
            Self::Stop => "Stop",
        }
    }
}

/// A button showing `icon`, lit while `selected`.
pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, selected: bool) -> Response {
    let size = vec2(32.0, ui.spacing().interact_size.y + 4.0);
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
        let color = if selected {
            ui.visuals().selection.stroke.color
        } else {
            visuals.fg_stroke.color
        };
        let c = rect.center();
        let r = 5.5;
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
        }
    }
    response
}

/// A horizontal meter, full at 1.0. It turns orange above 70% and red above
/// 90%, where an audio thread starts to run late.
pub(crate) fn meter(ui: &mut Ui, value: Option<f32>) -> Response {
    let size = vec2(56.0, 10.0);
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
                Color32::from_rgb(230, 70, 60)
            } else if value > 0.7 {
                Color32::from_rgb(235, 160, 50)
            } else {
                Color32::from_rgb(90, 190, 110)
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

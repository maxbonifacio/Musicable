//! A theme's widgets and fonts as egui's style.

use std::sync::Arc;

use egui::style::{Selection, WidgetVisuals};
use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, Style, TextStyle, ThemePreference,
    Visuals,
};

use super::{FontSizes, Theme, WidgetState, Widgets};

impl Theme {
    /// egui's style for drawing its widgets as this theme says.
    pub fn style(&self) -> Style {
        let widgets = &self.widgets;
        let mut style = Style {
            visuals: visuals(widgets),
            text_styles: text_styles(&self.fonts.sizes),
            ..Style::default()
        };
        let spacing = &mut style.spacing;
        spacing.item_spacing = widgets.item_spacing;
        spacing.button_padding = widgets.button_padding;
        spacing.window_margin = margin(widgets.window_margin);
        spacing.menu_margin = margin(widgets.menu_margin);
        spacing.indent = widgets.indent;
        spacing.interact_size = widgets.interact_size;
        spacing.slider_width = widgets.slider_width;
        spacing.combo_width = widgets.combo_width;
        spacing.text_edit_width = widgets.text_edit_width;
        spacing.icon_width = widgets.icon_width;
        spacing.icon_spacing = widgets.icon_spacing;
        spacing.scroll.bar_width = widgets.scroll_bar;
        style
    }
}

/// Draws egui's widgets in `ctx` as `theme` says from the next frame on,
/// whether the system is in light or dark mode, and gives the window's title
/// bar the theme's darkness.
pub fn apply(ctx: &egui::Context, theme: &Theme) {
    let style = Arc::new(theme.style());
    let preference = if style.visuals.dark_mode {
        ThemePreference::Dark
    } else {
        ThemePreference::Light
    };
    ctx.options_mut(|options| {
        options.dark_style = Arc::clone(&style);
        options.light_style = style;
        options.theme_preference = preference;
    });
}

fn visuals(widgets: &Widgets) -> Visuals {
    // Light and dark visuals differ in more than their colours, such as how
    // they draw text, so start from the right ones.
    let mut visuals = if widgets.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.panel_fill = widgets.panel;
    visuals.window_fill = widgets.window;
    visuals.window_stroke = Stroke::new(widgets.window_outline_width, widgets.window_outline);
    visuals.window_shadow.color = widgets.shadow;
    visuals.popup_shadow.color = widgets.shadow;
    visuals.extreme_bg_color = widgets.field;
    visuals.faint_bg_color = widgets.stripe;
    visuals.code_bg_color = widgets.code;
    visuals.hyperlink_color = widgets.link;
    visuals.warn_fg_color = widgets.warning;
    visuals.error_fg_color = widgets.error;
    visuals.selection = Selection {
        bg_fill: widgets.selection,
        stroke: Stroke::new(1.0, widgets.selected_text),
    };
    visuals.text_cursor.stroke.color = widgets.cursor;
    let ime = &mut visuals.ime_composition;
    ime.active_underline_stroke.color = widgets.cursor;
    ime.inactive_underline_stroke.color = widgets.cursor.linear_multiply(0.5);
    visuals.weak_text_alpha = widgets.weak_text.clamp(0.0, 1.0);
    visuals.window_corner_radius = corner(widgets.window_corner);
    visuals.menu_corner_radius = corner(widgets.menu_corner);
    let states = &mut visuals.widgets;
    states.noninteractive = state(&widgets.passive);
    states.inactive = state(&widgets.resting);
    states.hovered = state(&widgets.hovered);
    states.active = state(&widgets.pressed);
    states.open = state(&widgets.open);
    visuals
}

fn state(state: &WidgetState) -> WidgetVisuals {
    WidgetVisuals {
        weak_bg_fill: state.background,
        // egui asks that this is never transparent.
        bg_fill: if state.fill == Color32::TRANSPARENT {
            state.background
        } else {
            state.fill
        },
        bg_stroke: Stroke::new(state.outline_width, state.outline),
        corner_radius: corner(state.corner),
        fg_stroke: Stroke::new(state.line_width, state.text),
        expansion: state.expansion,
    }
}

fn text_styles(sizes: &FontSizes) -> std::collections::BTreeMap<TextStyle, FontId> {
    [
        (TextStyle::Small, sizes.small, FontFamily::Proportional),
        (TextStyle::Body, sizes.body, FontFamily::Proportional),
        (TextStyle::Button, sizes.button, FontFamily::Proportional),
        (TextStyle::Heading, sizes.heading, FontFamily::Proportional),
        (TextStyle::Monospace, sizes.monospace, FontFamily::Monospace),
    ]
    .into_iter()
    .map(|(style, size, family)| (style, FontId::new(size.max(1.0), family)))
    .collect()
}

/// A theme's corner radius, which egui keeps in whole points, up to 255.
fn corner(radius: f32) -> CornerRadius {
    CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// A theme's margin, which egui keeps in whole points, up to 127.
fn margin(size: f32) -> Margin {
    Margin::same(size.round().clamp(0.0, 127.0) as i8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dark_theme_draws_egui_as_it_draws_itself() {
        let style = Theme::dark().style();
        let mut expected = Style::default();
        // egui adds a little light behind striped rows, which the dark
        // theme makes a plain colour that looks the same on its panels.
        expected.visuals.faint_bg_color = Color32::from_gray(32);
        assert_eq!(style.visuals, expected.visuals);
        assert_eq!(style.spacing, expected.spacing);
        assert_eq!(style.text_styles, expected.text_styles);
        assert!(Theme::dark().widgets.is_dark());
    }

    #[test]
    fn the_light_theme_is_light_everywhere() {
        let theme = Theme::light();
        assert!(!theme.widgets.is_dark());
        let ctx = egui::Context::default();
        apply(&ctx, &theme);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(ctx.global_style().visuals.panel_fill, theme.widgets.panel);
        // In light mode or dark, the system's choice doesn't matter.
        assert_eq!(
            ctx.style_of(egui::Theme::Dark),
            ctx.style_of(egui::Theme::Light)
        );
        apply(&ctx, &Theme::dark());
        assert_eq!(ctx.theme(), egui::Theme::Dark);
    }

    #[test]
    fn sizes_egui_cant_take_are_kept_in_its_range() {
        let mut theme = Theme::dark();
        theme.widgets.window_corner = 1000.0;
        theme.widgets.window_margin = 2.6;
        theme.widgets.weak_text = 3.0;
        theme.widgets.resting.fill = Color32::TRANSPARENT;
        theme.fonts.sizes.body = 0.0;
        let style = theme.style();
        assert_eq!(style.visuals.window_corner_radius, CornerRadius::same(255));
        assert_eq!(style.spacing.window_margin, Margin::same(3));
        assert_eq!(style.visuals.weak_text_alpha, 1.0);
        let resting = style.visuals.widgets.inactive;
        assert_eq!(resting.bg_fill, resting.weak_bg_fill);
        assert_eq!(style.text_styles[&TextStyle::Body].size, 1.0);
    }
}

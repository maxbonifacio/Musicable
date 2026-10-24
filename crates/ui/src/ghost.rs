//! The ghost of a panel's tab that the user drags out past the edge of its
//! window. egui_tiles draws a dragged tab's ghost by the pointer in the
//! window the tab is dragged in, where it would stop at the edge: out
//! there, the ghost goes in a little window of its own, which follows the
//! pointer over the desktop and the other windows.

use egui::{
    Align2, Context, Pos2, StrokeKind, TextStyle, Vec2, ViewportBuilder, ViewportClass, ViewportId,
    WindowLevel, pos2, vec2,
};

/// The ghost's window.
pub(crate) fn id() -> ViewportId {
    ViewportId::from_hash_of("tab ghost")
}

/// Shows the ghost of the tab called `title` around the pointer, which is
/// at `at` on the desktop, in pixels.
pub(crate) fn show(ctx: &Context, title: &str, at: [i32; 2]) {
    // Embedded in the main window, as the tests embed windows of their
    // own, it would be a window in there.
    if ctx.embed_viewports() {
        return;
    }
    let style = ctx.global_style();
    let font = TextStyle::Body.resolve(&style);
    let text = ctx.fonts_mut(|fonts| {
        fonts.layout_no_wrap(title.to_owned(), font.clone(), style.visuals.text_color())
    });
    let size = (text.size() + vec2(16.0, 10.0)).ceil();
    // Points on the ghost's monitor, once it has one, and until then on
    // the main window's, a guess at the monitor egui makes it on.
    let scale = (ctx.input_for(id(), |i| i.viewport().native_pixels_per_point))
        .or_else(|| ctx.input_for(ViewportId::ROOT, |i| i.viewport().native_pixels_per_point))
        .unwrap_or(1.0);
    let middle = pos2(at[0] as f32, at[1] as f32) / (scale * ctx.zoom_factor());
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        // Not to take the focus, nor the pointer, from the window the tab
        // is dragged from.
        .with_active(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_position(corner(middle, size))
        .with_inner_size(size);
    ctx.show_viewport_immediate(id(), builder, |ui, class| {
        if class != ViewportClass::Immediate {
            return;
        }
        let rect = ui.ctx().content_rect();
        let visuals = ui.visuals();
        let painter = ui.painter();
        let stroke = visuals.window_stroke;
        painter.rect(rect, 0.0, visuals.window_fill, stroke, StrokeKind::Inside);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            title,
            font.clone(),
            visuals.text_color(),
        );
    });
}

/// The top left corner of a ghost `size` big with its middle at `middle`,
/// as egui_tiles centres the ghost on the pointer.
fn corner(middle: Pos2, size: Vec2) -> Pos2 {
    (middle - size / 2.0).round()
}

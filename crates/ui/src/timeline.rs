//! What the arrangement and the piano roll draw alike along the song: the
//! grid's lines, the ruler's bar numbers, the start marker and the
//! playhead.

use egui::{
    Align2, Color32, ColorImage, FontId, Id, Mesh, Painter, Rect, Shape, Stroke, TextureHandle,
    TextureOptions, TextureWrapMode, pos2,
};
use musicable_commands::editor::{Grid, Viewport};
use musicable_model::time::{Beats, TempoMap};

use crate::shell::Playback;
use crate::theme::Theme;

/// The closest the grid's lines are drawn, in points. A fixed grid finer
/// than this at the zoom is drawn as the adaptive grid.
const MIN_LINE_SPACING: f64 = 3.0;
/// The closest the ruler's ticks are drawn, in points.
const MIN_TICK_SPACING: f32 = 6.0;
/// How long the dashes of the grid's lines are while nothing snaps to it,
/// and the gaps between them, in points.
const DASH: f32 = 3.0;
const GAP: f32 = 3.0;

/// Song positions across the screen: the viewport's, from `left`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeline<'t> {
    pub tempo: &'t TempoMap,
    pub viewport: Viewport,
    /// Where on screen the viewport starts.
    pub left: f32,
}

impl Timeline<'_> {
    pub fn x_of(&self, at: Beats) -> f32 {
        self.left + self.viewport.x_of(at)
    }
}

/// The lines of `grid` over `area`: brightest at bars, then at beats, and
/// dashed while nothing snaps to them, as in Live.
pub(crate) fn grid(
    painter: &Painter,
    theme: &Theme,
    timeline: &Timeline<'_>,
    grid: Grid,
    area: Rect,
) {
    let tempo = timeline.tempo;
    let zoom = timeline.viewport.zoom;
    let visible = timeline.viewport.visible();
    let bar = tempo.bar_at(visible.start).bar;
    let length = tempo.bar_start(bar + 1) - tempo.bar_start(bar);
    let mut grid = grid;
    if grid.spacing(zoom, length) < MIN_LINE_SPACING {
        grid.fixed = None;
    }
    let mut dashes = Mesh::default();
    for at in grid.lines(tempo, visible, zoom) {
        let position = tempo.bar_at(at);
        let colour = match (position.offset == Beats::ZERO, position.beat) {
            (true, 0) => theme.grid_bar,
            (true, _) => theme.grid_beat,
            _ => theme.grid_step,
        };
        let x = painter.round_to_pixel_center(timeline.x_of(at));
        if grid.snap {
            painter.vline(x, area.y_range(), Stroke::new(1.0, colour));
        } else {
            // The dashes repeat down the texture from the top of the
            // screen, so they line up from area to area.
            let line = Rect::from_x_y_ranges(x - 0.5..=x + 0.5, area.y_range());
            let period = DASH + GAP;
            let uv = Rect::from_x_y_ranges(0.0..=1.0, area.top() / period..=area.bottom() / period);
            dashes.add_rect_with_uv(line, uv, colour);
        }
    }
    if !dashes.is_empty() {
        dashes.texture_id = dash_texture(painter).id();
        painter.add(dashes);
    }
}

/// A dash and a gap, in white, a texel to a point, repeated down a dashed
/// line of the grid, tinted its colour: a rectangle for each line, rather
/// than one for each of thousands of dashes.
fn dash_texture(painter: &Painter) -> TextureHandle {
    let ctx = painter.ctx();
    let id = Id::unique("grid dashes");
    if let Some(texture) = ctx.data(|data| data.get_temp::<TextureHandle>(id)) {
        return texture;
    }
    let texels = (0..(DASH + GAP) as usize)
        .map(|at| match (at as f32) < DASH {
            true => Color32::WHITE,
            false => Color32::TRANSPARENT,
        })
        .collect();
    let image = ColorImage::new([1, (DASH + GAP) as usize], texels);
    let options = TextureOptions {
        wrap_mode: TextureWrapMode::Repeat,
        ..TextureOptions::NEAREST
    };
    let texture = ctx.load_texture("grid dashes", image, options);
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

/// Every other group of bars over `area` shaded, counting from the song's
/// start, so the groups stand apart: each as many bars, 1, 2, 4, 8 or more,
/// as fill the theme's `bar_shade_width` at the zoom. Groups start at the
/// ruler's numbers, and like them, take the length of the first bar in
/// sight across the view, so they don't change size along it when the time
/// signature does.
pub(crate) fn bar_shades(painter: &Painter, theme: &Theme, timeline: &Timeline<'_>, area: Rect) {
    let tempo = timeline.tempo;
    let visible = timeline.viewport.visible();
    let first = tempo.bar_at(visible.start).bar;
    let last = tempo.bar_at(visible.end).bar;
    let group = bars_in(tempo, timeline.viewport.zoom, first, theme.bar_shade_width);
    // The first group is left as it is, the second shaded, and so on.
    let mut bar = first - first.rem_euclid(2 * group) + group;
    while bar <= last {
        let start = timeline.x_of(tempo.bar_start(bar));
        let end = timeline.x_of(tempo.bar_start(bar + group));
        let shade = Rect::from_x_y_ranges(start..=end, area.y_range()).intersect(area);
        if shade.is_positive() {
            painter.rect_filled(shade, 0.0, theme.bar_shade);
        }
        bar += 2 * group;
    }
}

/// How many bars, 1, 2, 4, 8 or more, are at least `least` points wide at
/// `zoom`, as long as `bar` is.
pub(crate) fn bars_in(tempo: &TempoMap, zoom: f64, bar: i64, least: f32) -> i64 {
    let width = (tempo.bar_start(bar + 1) - tempo.bar_start(bar)).to_f64() * zoom;
    let mut bars: i64 = 1;
    while width * (bars as f64) < f64::from(least) && bars < 1 << 20 {
        bars *= 2;
    }
    bars
}

/// A ruler's bar numbers, below `top` in `ruler`, every bar or every few so
/// they don't crowd, with ticks at the bars and at the beats when there's
/// room.
pub(crate) fn bars(
    painter: &Painter,
    theme: &Theme,
    timeline: &Timeline<'_>,
    ruler: Rect,
    top: f32,
) {
    let tempo = timeline.tempo;
    let zoom = timeline.viewport.zoom;
    let visible = timeline.viewport.visible();
    let first = tempo.bar_at(visible.start).bar;
    let last = tempo.bar_at(visible.end).bar;
    // A label every bar, or every 2, 4, 8... bars.
    let every = bars_in(tempo, zoom, first, theme.label_spacing);
    let tick = Stroke::new(1.0, theme.grid_bar);
    let mut bar = first - first.rem_euclid(every);
    while bar <= last {
        let x = painter.round_to_pixel_center(timeline.x_of(tempo.bar_start(bar)));
        painter.vline(x, top..=ruler.bottom(), tick);
        painter.text(
            pos2(x + theme.padding, top),
            Align2::LEFT_TOP,
            (bar + 1).to_string(),
            FontId::proportional(theme.text),
            theme.ruler_text,
        );
        bar += every;
    }
    let beat = tempo.signature_at(visible.start).beat_length().to_f64() * zoom;
    if beat as f32 >= MIN_TICK_SPACING {
        let short = ruler.bottom() - (ruler.bottom() - top) / 4.0;
        let tick = Stroke::new(1.0, theme.grid_beat);
        for at in tempo.beats_in(visible).filter(|beat| beat.beat != 0) {
            let x = painter.round_to_pixel_center(timeline.x_of(at.at));
            painter.vline(x, short..=ruler.bottom(), tick);
        }
    }
}

/// The start marker's triangle at `x`, pointing down at the bottom of
/// `ruler`.
pub(crate) fn start_marker(painter: &Painter, theme: &Theme, x: f32, ruler: Rect) {
    let marker = theme.marker;
    let points = vec![
        pos2(x - marker, ruler.bottom() - marker),
        pos2(x + marker, ruler.bottom() - marker),
        pos2(x, ruler.bottom()),
    ];
    painter.add(Shape::convex_polygon(
        points,
        theme.start_marker,
        Stroke::NONE,
    ));
}

/// Where the playhead is drawn: while it plays, or where it stopped if that
/// isn't the start marker.
pub(crate) fn playhead(playback: &Playback, start_marker: Beats) -> Option<Beats> {
    let shown = playback.playing || playback.playhead != start_marker;
    shown.then_some(playback.playhead)
}

#[cfg(test)]
mod tests {
    use musicable_model::time::{TempoMap, TimeSignature};

    use super::*;

    #[test]
    fn groups_of_bars_double_as_the_zoom_narrows_them() {
        let tempo = TempoMap::default();
        // A bar of 4/4 is 4 beats: 64 points at 16 points a beat.
        assert_eq!(bars_in(&tempo, 16.0, 0, 64.0), 1);
        assert_eq!(bars_in(&tempo, 16.0, 0, 65.0), 2);
        assert_eq!(bars_in(&tempo, 16.0, 0, 96.0), 2);
        assert_eq!(bars_in(&tempo, 2.0, 0, 96.0), 16);
        assert_eq!(bars_in(&tempo, 400.0, 0, 96.0), 1);
        // Zoomed all the way out, they stop growing.
        assert_eq!(bars_in(&tempo, 1e-9, 0, 96.0), 1 << 20);
        // From bar 9, bars of 3/4 are 48 points.
        let mut tempo = tempo;
        tempo.insert_signature(8, TimeSignature::new(3, 4).unwrap());
        assert_eq!(bars_in(&tempo, 16.0, 7, 96.0), 2);
        assert_eq!(bars_in(&tempo, 16.0, 8, 96.0), 2);
        assert_eq!(bars_in(&tempo, 16.0, 8, 97.0), 4);
    }
}

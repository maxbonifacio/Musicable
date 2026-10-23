//! What the arrangement and the piano roll draw alike along the song: the
//! grid's lines, the ruler's bar numbers, the start marker and the
//! playhead.

use egui::{Align2, FontId, Painter, Rect, Shape, Stroke, pos2};
use musicable_commands::editor::{Grid, Viewport};
use musicable_model::time::{Beats, TempoMap};

use crate::shell::Playback;
use crate::theme::Theme;

/// The closest the grid's lines are drawn, in points. A fixed grid finer
/// than this at the zoom is drawn as the adaptive grid.
const MIN_LINE_SPACING: f64 = 3.0;
/// The closest the ruler's ticks are drawn, in points.
const MIN_TICK_SPACING: f32 = 6.0;

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

/// The lines of `grid` over `area`: brightest at bars, then at beats.
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
    for at in grid.lines(tempo, visible, zoom) {
        let position = tempo.bar_at(at);
        let colour = match (position.offset == Beats::ZERO, position.beat) {
            (true, 0) => theme.grid_bar,
            (true, _) => theme.grid_beat,
            _ => theme.grid_step,
        };
        let x = painter.round_to_pixel_center(timeline.x_of(at));
        painter.vline(x, area.y_range(), Stroke::new(1.0, colour));
    }
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
    let width = |bar: i64| (tempo.bar_start(bar + 1) - tempo.bar_start(bar)).to_f64() * zoom;
    // A label every bar, or every 2, 4, 8... bars.
    let mut every: i64 = 1;
    while (width(first) * every as f64) < f64::from(theme.label_spacing) && every < 1 << 20 {
        every *= 2;
    }
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

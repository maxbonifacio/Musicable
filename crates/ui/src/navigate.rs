//! What the timeline panels, the arrangement, the piano roll and the Sample
//! Editor, share in getting around: the wheel's gesture, which is one step
//! of a panel's view history, scrolling while a drag is held at an edge,
//! and Follow.

use std::time::Duration;

use egui::{Rect, Ui};
use musicable_commands::editor::Viewport;
use musicable_commands::navigation::Record;
use musicable_model::time::Beats;

use crate::shell::Playback;

/// How long after the wheel's or a pinch's last event its gesture ends,
/// and becomes a step of the view's history, in seconds.
const WHEEL_GESTURE: f64 = 0.3;
/// How far inside a panel's edges a drag scrolls it, in points.
pub(crate) const EDGE: f32 = 8.0;
/// How fast a drag at an edge scrolls, in points a second, and how much
/// faster for each point further past it, up to the most.
const EDGE_SPEED: f32 = 240.0;
const EDGE_ACCELERATION: f32 = 30.0;
const EDGE_MAX_SPEED: f32 = 8000.0;

/// The wheel's gesture in a panel, while it lasts: it ends a moment after
/// the wheel's, or a pinch's, last event.
#[derive(Debug, Default)]
pub(crate) struct Wheeling {
    /// When the wheel or a pinch last changed the view.
    last: Option<f64>,
}

impl Wheeling {
    /// How this frame's change to the view counts, and whether to send the
    /// view even if it didn't change, to end the wheel's gesture or to let
    /// Follow follow again: as part of the wheel's gesture if `wheeled`
    /// changed it this frame, as its end once the gesture is over, as
    /// Follow's if `followed`, or else as movement the user didn't make.
    pub fn record(&mut self, ui: &Ui, wheeled: bool, followed: bool) -> (Record, bool) {
        let now = ui.input(|i| i.time);
        if wheeled {
            self.last = Some(now);
        }
        let ended = !wheeled && self.last.is_some_and(|at| now - at >= WHEEL_GESTURE);
        if ended {
            self.last = None;
        } else if self.last.is_some() {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(WHEEL_GESTURE));
        }
        let record = match (wheeled, ended, followed) {
            (true, ..) => Record::Gesture,
            (_, true, _) => Record::Step,
            (.., true) => Record::Follow,
            _ => Record::None,
        };
        (record, ended || followed)
    }
}

/// How far to scroll this frame, in points, while a drag is `past` points
/// past the line [`EDGE`] inside an edge: none before it, and faster the
/// further past it, by time, so a still pointer scrolls as fast at any
/// frame rate.
pub(crate) fn edge_scroll(ui: &Ui, past: f32) -> Option<f32> {
    if past <= 0.0 {
        return None;
    }
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    ui.ctx().request_repaint();
    Some((EDGE_SPEED + EDGE_ACCELERATION * past).min(EDGE_MAX_SPEED) * dt)
}

/// Scrolls `viewport` sideways while a drag at `x` is held at the left or
/// the right edge of `area`, once it's gone that way from `from`, where it
/// started, so a drag that starts by an edge doesn't scroll at once.
/// Returns whether it scrolled.
pub(crate) fn edge_scroll_sideways(
    ui: &Ui,
    area: Rect,
    from: Option<f32>,
    x: f32,
    viewport: &mut Viewport,
) -> bool {
    let left = from.is_none_or(|from| x < from) && viewport.start > Beats::ZERO;
    let right = from.is_none_or(|from| x > from);
    if left && let Some(by) = edge_scroll(ui, area.left() + EDGE - x) {
        viewport.start = (viewport.start - viewport.beats_in(by)).max(Beats::ZERO);
        return true;
    }
    if right && let Some(by) = edge_scroll(ui, x - (area.right() - EDGE)) {
        viewport.start += viewport.beats_in(by);
        return true;
    }
    false
}

/// Scrolls `scroll`, how far down `area`'s rows are scrolled, from none to
/// `lowest`, while a drag at `y` is held at the top or the bottom edge of
/// `area`, once it's gone that way from `from`, where it started. Returns
/// whether it scrolled.
pub(crate) fn edge_scroll_down(
    ui: &Ui,
    area: Rect,
    from: Option<f32>,
    y: f32,
    scroll: &mut f32,
    lowest: f32,
) -> bool {
    let up = from.is_none_or(|from| y < from) && *scroll > 0.0;
    let down = from.is_none_or(|from| y > from) && *scroll < lowest;
    let by = match () {
        _ if up => edge_scroll(ui, area.top() + EDGE - y).map(|by| -by),
        _ => None,
    };
    let by = by.or_else(|| {
        down.then(|| edge_scroll(ui, y - (area.bottom() - EDGE)))
            .flatten()
    });
    if let Some(by) = by {
        *scroll = (*scroll + by).clamp(0.0, lowest.max(0.0));
    }
    by.is_some()
}

/// Turns a page of `viewport` when the playhead leaves it while it plays,
/// if Follow is on in the panel, unless `held`, as while a drag or the
/// wheel goes on. While the user has navigated away, as `suspended` says,
/// it waits for the playhead to play into sight. Returns whether Follow
/// moved the view, or follows again now the playhead is in sight.
pub(crate) fn follow(
    playback: &Playback,
    on: bool,
    suspended: bool,
    held: bool,
    viewport: &mut Viewport,
) -> bool {
    if !playback.playing || !on || held {
        return false;
    }
    let in_sight = viewport.visible().contains(playback.playhead);
    if suspended {
        return in_sight;
    }
    let before = *viewport;
    viewport.follow(playback.playhead);
    *viewport != before
}

/// Where a drag that started at `from`, when the view was `then`, started
/// in the view `now`, which may have scrolled under it: in a panel whose
/// timeline's left edge is at `left` and whose rows' top is at `top`.
pub(crate) fn origin(from: egui::Pos2, then: Viewport, now: Viewport, left: f32) -> egui::Pos2 {
    let x = match now.zoom == then.zoom {
        true => from.x + now.x_of(then.start),
        false => left + now.x_of(then.beats_at(from.x - left)),
    };
    egui::pos2(x, from.y + then.top - now.top)
}

/// How far a drag that started at `from_x` and is at `to_x` has moved
/// along the song, counting how far the view scrolled under it since it
/// was `then`.
pub(crate) fn moved(from_x: f32, to_x: f32, then: Viewport, now: Viewport, left: f32) -> Beats {
    match now.zoom == then.zoom {
        true => (now.start - then.start) + now.beats_in(to_x - from_x),
        false => now.beats_at(to_x - left) - then.beats_at(from_x - left),
    }
}

//! What the timeline panels, the arrangement, the piano roll and the Sample
//! Editor, share in getting around: the wheel's gesture, which is one step
//! of a panel's view history, scrolling while a drag is held at an edge,
//! and Follow.

use std::time::Duration;

use egui::{Rect, Ui};
use musicable_commands::Editor;
use musicable_commands::editor::{FollowMode, FollowPlace, Viewport};
use musicable_commands::navigation::{Panel, Record};
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

/// What a timeline panel keeps from frame to frame for getting around: the
/// wheel's gesture, which ends a moment after the wheel's, or a pinch's,
/// last event, whether a drag held the view last frame, and where the
/// playhead was.
#[derive(Debug, Default)]
pub(crate) struct Navigating {
    /// When the wheel or a pinch last changed the view.
    wheeled: Option<f64>,
    dragging: bool,
    heard: Option<Beats>,
}

impl Navigating {
    /// Has Follow keep the playhead in `viewport`, `panel`'s view in
    /// `editor`, unless a drag or the wheel holds it, as `dragging` and
    /// `wheeled`, whether the wheel changed it this frame, say. Returns how
    /// this frame's change to the view counts, and whether to send the view
    /// even if it didn't change: as part of the wheel's gesture if the
    /// wheel changed it, as its end once the gesture is over, as Follow's
    /// if Follow moved it or follows again, as a drag's end in Continuous
    /// mode, which leaves Follow waiting, or else as movement the user
    /// didn't make.
    pub fn frame(
        &mut self,
        ui: &Ui,
        playback: &Playback,
        editor: &Editor,
        panel: Panel,
        viewport: &mut Viewport,
        (wheeled, dragging): (bool, bool),
    ) -> (Record, bool) {
        let released = std::mem::replace(&mut self.dragging, dragging) && !dragging;
        let mut following = Following::of(editor, panel, wheeled || dragging);
        // A drag let go of in Continuous mode leaves the view where it was:
        // the playhead moved on from its place while the drag held it.
        let suspend = released && following.continuous(playback);
        following.held |= suspend;
        let followed = follow(playback, following, self.heard, viewport);
        self.heard = Some(playback.playhead);
        let now = ui.input(|i| i.time);
        if wheeled {
            self.wheeled = Some(now);
        }
        let ended = !wheeled && self.wheeled.is_some_and(|at| now - at >= WHEEL_GESTURE);
        if ended {
            self.wheeled = None;
        } else if self.wheeled.is_some() {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(WHEEL_GESTURE));
        }
        match (wheeled, ended, followed) {
            (true, ..) => (Record::Gesture, false),
            (_, true, _) => (Record::Step, true),
            (.., true) => (Record::Follow, true),
            _ if suspend => (Record::Suspend, true),
            _ => (Record::None, false),
        }
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

/// How a panel follows the playhead this frame: Follow's switch in it, the
/// mode and place they share, and the panel's Follow state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Following {
    pub on: bool,
    pub mode: FollowMode,
    pub place: FollowPlace,
    /// Whether the user navigated away while it played.
    pub suspended: bool,
    /// Whether a drag or the wheel is going on in the panel, which holds
    /// the view still.
    pub held: bool,
}

impl Following {
    /// How `editor` has `panel` follow, while `held` or not.
    pub fn of(editor: &Editor, panel: Panel, held: bool) -> Self {
        Self {
            on: editor.follows(panel),
            mode: editor.behaviour.follow_mode,
            place: editor.behaviour.follow_place,
            suspended: editor.navigation.of(panel).suspended,
            held,
        }
    }

    /// Whether the view is following the playhead in Continuous mode now,
    /// which moves it every frame.
    pub fn continuous(&self, playback: &Playback) -> bool {
        playback.playing && self.on && self.mode == FollowMode::Continuous && !self.suspended
    }
}

/// Keeps the playhead in sight in `viewport` while it plays, if Follow is
/// on in the panel, unless it's held: a page at a time, or keeping it at
/// its place as the song moves under it. While the user has navigated
/// away, it waits: in Page mode for the playhead to play into sight, and in
/// Continuous mode for it to play up to its place from the left of it,
/// from `before`, where it was last frame, so the view never jumps. Returns
/// whether Follow moved the view, or follows again now.
pub(crate) fn follow(
    playback: &Playback,
    following: Following,
    before: Option<Beats>,
    viewport: &mut Viewport,
) -> bool {
    if !playback.playing || !following.on || following.held {
        return false;
    }
    let playhead = playback.playhead;
    match following.mode {
        FollowMode::Page if following.suspended => viewport.visible().contains(playhead),
        FollowMode::Continuous if following.suspended => {
            let place = viewport.beats_at(viewport.width * following.place.fraction());
            before.is_some_and(|before| before < place && place <= playhead)
                && viewport.visible().contains(playhead)
        }
        mode => {
            let was = *viewport;
            match mode {
                FollowMode::Page => viewport.follow(playhead),
                FollowMode::Continuous => viewport.place(playhead, following.place.fraction()),
            }
            *viewport != was
        }
    }
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

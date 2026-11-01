//! The overview: the whole song in a thin strip over the arrangement's
//! ruler, as Live's, with where the tracks have clips, the loop, the
//! locators, the playhead, and a box round what's in sight.
//!
//! Clicking shows that part of the song, dragging scrolls along and zooms
//! as it goes up and down, dragging the box's start or end zooms to fit it,
//! and a double click shows the whole song, as the mouse map says. Each is
//! a step of the arrangement's view history, and pauses Follow while the
//! song plays, as other navigation does.

use egui::{
    Align2, CursorIcon, FontId, Painter, Pos2, Rect, Response, Role, Sense, Stroke, Ui, WidgetInfo,
    pos2,
};
use musicable_commands::editor::Viewport;
use musicable_commands::navigation::{Record, song_end};
use musicable_commands::{Args, transport, view as views};
use musicable_model::time::{BeatRange, Beats};

use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::timeline;

/// How tall the strip is, in points.
pub(super) const HEIGHT: f32 = 28.0;
/// How tall the arrangement has to be for the strip to show, in points,
/// so a short one keeps its room for the tracks.
pub(super) const MIN_ARRANGEMENT: f32 = 300.0;
/// How near the box's start or end a press takes it, in points.
const EDGE_GRAB: f32 = 4.0;
/// How much a point of a drag up or down zooms: the zoom changes by `e` to
/// the power of this many points, as on the ruler.
const ZOOM_SPEED: f32 = 0.01;
/// The thinnest a track's row in the strip is drawn, in points: with less
/// room, the tracks share one row.
const MIN_ROW: f32 = 2.0;

/// The overview's own state: the drag in progress.
#[derive(Debug, Default)]
pub(crate) struct Overview {
    drag: Option<Drag>,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    action: MouseAction,
    from: Pos2,
    /// The view as the drag started.
    viewport: Viewport,
    /// The song positions across the strip as the drag started, kept while
    /// it lasts, so the box stays under the pointer while a recording
    /// makes the song longer.
    scale: Scale,
    /// For moving an edge of the view, whether it's the end.
    end: bool,
}

/// Song positions across the strip.
#[derive(Debug, Clone, Copy)]
struct Scale {
    left: f32,
    width: f32,
    /// How far the strip reaches, from the song's start.
    end: Beats,
}

impl Scale {
    /// The strip across `rect`, for the song in `view`, which reaches past
    /// the view in sight and what's recording, if they go past its end.
    fn of(rect: Rect, view: &View<'_>, viewport: &Viewport) -> Self {
        let recorded = (view.recordings.iter()).map(|recording| recording.range.end);
        let end = (song_end(view.project).max(viewport.visible().end))
            .max(recorded.max().unwrap_or(Beats::ZERO));
        Self {
            left: rect.left(),
            width: rect.width().max(1.0),
            end: end.max(Beats::new(1)),
        }
    }

    fn x_of(&self, at: Beats) -> f32 {
        self.left + (at.to_f64() / self.end.to_f64()) as f32 * self.width
    }

    fn beats_at(&self, x: f32) -> Beats {
        self.beats_in(x - self.left).max(Beats::ZERO)
    }

    /// How many beats `points` of the strip span.
    fn beats_in(&self, points: f32) -> Beats {
        Beats::from_f64(f64::from(points / self.width) * self.end.to_f64())
    }
}

impl Overview {
    /// Takes the mouse over the strip at `rect`, changing `viewport`, the
    /// arrangement's view, and asking for what can't be done here. Returns
    /// how the change to the view counts, if it changed it.
    pub(super) fn interact(
        &mut self,
        ui: &Ui,
        rect: Rect,
        view: &View<'_>,
        mouse: &MouseMap,
        viewport: &mut Viewport,
        actions: &mut Vec<Action>,
    ) -> Option<Record> {
        let id = ui.scope_id().with("overview");
        let response = ui.interact(rect, id, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Overview"));
        let scale = Scale::of(rect, view, viewport);
        let mods = modifiers(ui.input(|i| i.modifiers));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.drag.take().is_some() {
            return None;
        }
        if response.drag_started()
            && let Some(from) = ui.input(|i| i.pointer.press_origin())
        {
            let (target, end) = target(&scale, viewport, from.x);
            self.drag = (mouse.action(target, Gesture::Drag, mods)).map(|action| Drag {
                action,
                from,
                viewport: *viewport,
                scale,
                end,
            });
        }
        if let Some(drag) = self.drag {
            let to = response.interact_pointer_pos().unwrap_or(drag.from);
            let before = *viewport;
            dragged(&drag, to, viewport);
            if response.drag_stopped() {
                self.drag = None;
                return Some(Record::Step);
            }
            return (*viewport != before).then_some(Record::Gesture);
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        self.clicked(&response, &scale, mouse, mods, viewport, actions)
    }

    /// Whether a drag in the strip holds the view.
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn clicked(
        &self,
        response: &Response,
        scale: &Scale,
        mouse: &MouseMap,
        mods: musicable_commands::Modifiers,
        viewport: &mut Viewport,
        actions: &mut Vec<Action>,
    ) -> Option<Record> {
        let gesture = match () {
            _ if response.double_clicked() => Gesture::DoubleClick,
            _ if response.clicked() => Gesture::Click,
            _ => return None,
        };
        let pos = response.interact_pointer_pos()?;
        let (target, _) = target(scale, viewport, pos.x);
        let at = scale.beats_at(pos.x);
        match mouse.action(target, gesture, mods)? {
            MouseAction::ShowHere => {
                viewport.start = (at - viewport.beats_in(viewport.width / 2.0)).max(Beats::ZERO);
                Some(Record::Step)
            }
            MouseAction::FitSong => {
                let focus = Args::new().with("panel", musicable_commands::panel::ARRANGEMENT);
                actions.push(Action::RunWith(views::SET_FOCUS, focus));
                actions.push(Action::Run(views::FIT_SONG_WIDTH));
                None
            }
            MouseAction::Locate => {
                let args = Args::new().with("at", at);
                actions.push(Action::RunWith(transport::LOCATE, args));
                None
            }
            _ => None,
        }
    }

    /// Paints the strip at `rect`, for the arrangement's view `viewport`,
    /// and the pointer over it.
    pub(super) fn paint(
        &self,
        ui: &Ui,
        painter: &Painter,
        rect: Rect,
        view: &View<'_>,
        theme: &Theme,
        viewport: &Viewport,
    ) {
        let scale = match self.drag {
            Some(drag) => drag.scale,
            None => Scale::of(rect, view, viewport),
        };
        let project = view.project;
        painter.rect_filled(rect, 0.0, theme.ruler);
        if project.looping {
            let region = project.loop_region;
            let shade = Rect::from_x_y_ranges(
                scale.x_of(region.start)..=scale.x_of(region.end),
                rect.y_range(),
            );
            painter.rect_filled(shade, 0.0, theme.loop_shade);
        }
        // Each track in a row of its own when there's room, else all in one.
        let room = rect.shrink2(egui::vec2(0.0, 3.0));
        let count = project.tracks.len().max(1);
        let own_rows = room.height() / count as f32 >= MIN_ROW;
        let row_height = match own_rows {
            true => room.height() / count as f32,
            false => room.height(),
        };
        for (index, track) in project.tracks.iter().enumerate() {
            let top = match own_rows {
                true => room.top() + index as f32 * row_height,
                false => room.top(),
            };
            let colour = theme.clip(index);
            for clip in &track.clips {
                let range = clip.range();
                let left = scale.x_of(range.start);
                let right = scale.x_of(range.end).max(left + 1.0);
                let block = Rect::from_x_y_ranges(left..=right, top..=top + row_height);
                painter.rect_filled(block, 0.0, colour);
            }
        }
        for recording in view.recordings {
            let index = project.track_index(recording.track).unwrap_or(0);
            let range: BeatRange = recording.range;
            let block = Rect::from_x_y_ranges(
                scale.x_of(range.start)..=scale.x_of(range.end),
                room.y_range(),
            );
            painter.rect_filled(block, 0.0, theme.clip(index).gamma_multiply(0.6));
        }
        for locator in &project.locators {
            let x = painter.round_to_pixel_center(scale.x_of(locator.at));
            painter.vline(x, rect.y_range(), Stroke::new(1.0, theme.locator));
            painter.text(
                pos2(x + 2.0, rect.top()),
                Align2::LEFT_TOP,
                &locator.name,
                FontId::proportional(theme.small_text),
                theme.locator,
            );
        }
        if let Some(at) = timeline::playhead(&view.playback, view.editor.start_marker) {
            let x = painter.round_to_pixel_center(scale.x_of(at));
            painter.vline(x, rect.y_range(), Stroke::new(1.0, theme.playhead));
        }
        let visible = viewport.visible();
        let left = scale.x_of(visible.start).max(rect.left());
        let right = scale.x_of(visible.end).min(rect.right()).max(left + 2.0);
        let sight = Rect::from_x_y_ranges(left..=right, rect.top() + 1.0..=rect.bottom() - 1.0);
        painter.rect_filled(sight, theme.corner, theme.ruler_text.gamma_multiply(0.12));
        painter.rect_stroke(
            sight,
            theme.corner,
            Stroke::new(1.5, theme.ruler_text),
            egui::StrokeKind::Inside,
        );
        let y = painter.round_to_pixel_center(rect.bottom() - 0.5);
        painter.hline(rect.x_range(), y, Stroke::new(1.0, theme.divider));
        // The pointer shows what a drag would do.
        let hovered = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|&pos| rect.contains(pos));
        let edge = match self.drag {
            Some(drag) => drag.action == MouseAction::ResizeView,
            None => {
                hovered.is_some_and(|pos| target(&scale, viewport, pos.x).0 == Target::OverviewEdge)
            }
        };
        if edge {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
}

/// What's at `x` in the strip: the box's start or end, and which, or the
/// rest of it.
fn target(scale: &Scale, viewport: &Viewport, x: f32) -> (Target, bool) {
    let visible = viewport.visible();
    let (start, end) = (scale.x_of(visible.start), scale.x_of(visible.end));
    // A box too narrow to grab inside moves rather than resizes.
    let narrow = end - start < EDGE_GRAB * 4.0;
    match () {
        _ if narrow => (Target::Overview, false),
        _ if (x - start).abs() <= EDGE_GRAB => (Target::OverviewEdge, false),
        _ if (x - end).abs() <= EDGE_GRAB => (Target::OverviewEdge, true),
        _ => (Target::Overview, false),
    }
}

/// Changes `viewport` as `drag` does with the pointer at `to`.
fn dragged(drag: &Drag, to: Pos2, viewport: &mut Viewport) {
    let then = drag.viewport;
    let scale = drag.scale;
    match drag.action {
        MouseAction::MoveView => {
            // Down zooms in, around the middle of what was in sight, as on
            // the ruler; along scrolls by the song's beats under the strip.
            let zoom = then.zoom * f64::from(((to.y - drag.from.y) * ZOOM_SPEED).exp());
            let zoom = zoom.clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM);
            let middle = then.start + then.beats_in(then.width / 2.0);
            let middle = middle + scale.beats_in(to.x - drag.from.x);
            viewport.zoom = zoom;
            viewport.start = (middle - viewport.beats_in(viewport.width / 2.0)).max(Beats::ZERO);
        }
        MouseAction::ResizeView => {
            let visible = then.visible();
            let at = scale.beats_at(to.x);
            let least = then.beats_in(1.0).max(Beats::from_ticks(1));
            let (start, end) = match drag.end {
                true => (visible.start, at.max(visible.start + least)),
                false => (at.min(visible.end - least), visible.end),
            };
            let length = (end - start).to_f64().max(f64::MIN_POSITIVE);
            viewport.zoom =
                (f64::from(then.width) / length).clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM);
            viewport.start = start.max(Beats::ZERO);
        }
        _ => {}
    }
}

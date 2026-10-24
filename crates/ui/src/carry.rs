//! Drags from the browser between windows. While the user drags, the system
//! sends the pointer only to the window the drag started in, so the window
//! under the pointer never sees it: a sample dragged from a browser in a
//! window of its own couldn't go on a track in the main window. [`Carry`]
//! passes the pointer on to the window it's over, as if it moved there, and
//! lets go there, with what's dragged.

use egui::{
    Context, DragAndDrop, Event, Plugin, Pos2, RawInput, Rect, Ui, ViewportId, ViewportIdMap,
    ViewportInfo,
};

use crate::browser::Dragged;

/// The egui plugin that carries drags from the browser between windows.
#[derive(Default)]
pub(crate) struct Carry {
    /// The pointer's moves for windows other than the one it's in, as
    /// they'd have seen them, for their next pass.
    waiting: ViewportIdMap<Vec<Event>>,
    /// The window the pointer was last passed on to.
    over: Option<ViewportId>,
    /// What was let go of over another window, and which: for that
    /// window's next pass, as the window the drag started in drops it at
    /// the end of its own, which may come first.
    dropped: Option<(ViewportId, Dragged)>,
}

impl Plugin for Carry {
    fn debug_name(&self) -> &'static str {
        "Carry"
    }

    fn input_hook(&mut self, ctx: &Context, input: &mut RawInput) {
        self.waiting
            .retain(|id, _| input.viewports.contains_key(id));
        match DragAndDrop::payload::<Dragged>(ctx) {
            Some(dragged) => self.carry(ctx, input, &dragged),
            // Dropped, or Escape pressed.
            None => {
                if let Some(left) = self.over.take() {
                    self.send(ctx, left, Event::PointerGone);
                }
            }
        }
        if let Some(events) = self.waiting.remove(&input.viewport_id) {
            input.events.extend(events);
        }
    }

    fn on_begin_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx();
        let here = ctx.viewport_id();
        if self.dropped.as_ref().is_some_and(|(to, _)| *to == here)
            && let Some((_, dragged)) = self.dropped.take()
        {
            DragAndDrop::set_payload(ctx, dragged);
        }
    }
}

impl Carry {
    /// Passes the pointer's moves in `input`, from the window the drag of
    /// `dragged` is in, on to the window under the pointer, if it's not
    /// over the window it's in, and lets go there.
    fn carry(&mut self, ctx: &Context, input: &RawInput, dragged: &Dragged) {
        let from = input.viewport_id;
        let zoom = ctx.zoom_factor();
        let Some(source) = input.viewports.get(&from) else {
            return;
        };
        for event in &input.events {
            let (pos, release) = match *event {
                Event::PointerMoved(pos) => (pos, None),
                Event::PointerButton {
                    pos,
                    button,
                    pressed: false,
                    modifiers,
                } => (pos, Some((button, modifiers))),
                _ => continue,
            };
            let to =
                desktop(pos, source, zoom).and_then(|at| under(at, from, &input.viewports, zoom));
            let target = to.map(|(id, _)| id);
            if self.over != target
                && let Some(left) = self.over.take()
            {
                self.send(ctx, left, Event::PointerGone);
            }
            let Some((target, at)) = to else {
                continue;
            };
            self.over = Some(target);
            self.send(ctx, target, Event::PointerMoved(at));
            if let Some((button, modifiers)) = release {
                let release = Event::PointerButton {
                    pos: at,
                    button,
                    pressed: false,
                    modifiers,
                };
                self.send(ctx, target, release);
                self.dropped = Some((target, dragged.clone()));
                self.over = None;
            }
        }
    }

    /// Hands `event` to the window `to` in its next pass.
    fn send(&mut self, ctx: &Context, to: ViewportId, event: Event) {
        self.waiting.entry(to).or_default().push(event);
        ctx.request_repaint_of(to);
    }
}

/// Where `pos`, in the points of the window egui describes as `window`, is
/// on the desktop, in pixels, egui zooming by `zoom`.
fn desktop(pos: Pos2, window: &ViewportInfo, zoom: f32) -> Option<Pos2> {
    let inner = window.inner_rect?;
    Some((inner.min + pos.to_vec2()) * (window.native_pixels_per_point? * zoom))
}

/// Where the desktop's pixel `at` is in the window egui describes as
/// `window`, in its points, if it's in it.
fn inside(at: Pos2, window: &ViewportInfo, zoom: f32) -> Option<Pos2> {
    let inner = window.inner_rect?;
    let pos = at / (window.native_pixels_per_point? * zoom) - inner.min.to_vec2();
    let shown = window.minimized != Some(true);
    (shown && Rect::from_min_size(Pos2::ZERO, inner.size()).contains(pos)).then_some(pos)
}

/// The window other than `from` that the desktop's pixel `at` is in, of
/// `windows`, and where in it, unless it's in `from` itself. Windows of
/// their own go before the main window, which they're more likely to be
/// in front of.
fn under(
    at: Pos2,
    from: ViewportId,
    windows: &ViewportIdMap<ViewportInfo>,
    zoom: f32,
) -> Option<(ViewportId, Pos2)> {
    if (windows.get(&from)).is_some_and(|window| inside(at, window, zoom).is_some()) {
        return None;
    }
    let mut others: Vec<_> = (windows.iter()).filter(|(id, _)| **id != from).collect();
    others.sort_by_key(|(id, _)| **id == ViewportId::ROOT);
    (others.into_iter()).find_map(|(id, window)| Some((*id, inside(at, window, zoom)?)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui::{PointerButton, pos2, vec2};

    use super::*;

    /// The window of its own.
    fn window() -> ViewportId {
        ViewportId::from_hash_of("window")
    }

    /// The main window at the desktop's corner, 1000 by 800 points at 100%,
    /// and one of its own to its right, 600 by 400 points at 150%.
    fn windows() -> ViewportIdMap<ViewportInfo> {
        let at = |min: Pos2, size, scale| ViewportInfo {
            inner_rect: Some(Rect::from_min_size(min, size)),
            native_pixels_per_point: Some(scale),
            ..ViewportInfo::default()
        };
        let mut windows = ViewportIdMap::default();
        windows.insert(ViewportId::ROOT, at(Pos2::ZERO, vec2(1000.0, 800.0), 1.0));
        // At 1200, 150 in pixels.
        windows.insert(window(), at(pos2(800.0, 100.0), vec2(600.0, 400.0), 1.5));
        windows
    }

    /// Runs a pass of the window `id`, with `events`, and returns what
    /// `inside` reads from it once the pass has begun.
    fn pass<R>(
        ctx: &Context,
        id: ViewportId,
        events: Vec<Event>,
        inside: impl FnOnce(&Context) -> R,
    ) -> R {
        let input = RawInput {
            viewport_id: id,
            viewports: windows(),
            events,
            ..RawInput::default()
        };
        let mut inside = Some(inside);
        let mut read = None;
        let output = ctx.run_ui(input, |ui| {
            if let Some(inside) = inside.take() {
                read = Some(inside(ui.ctx()));
            }
        });
        output.drop_without_applying_deltas();
        read.expect("the pass ran")
    }

    fn sample() -> Dragged {
        Dragged::Sample(PathBuf::from("kick.wav"))
    }

    fn release(pos: Pos2) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_sample_let_go_of_over_another_window_is_dropped_there() {
        let ctx = Context::default();
        ctx.add_plugin(Carry::default());
        // Dragged in the window of its own...
        pass(&ctx, window(), Vec::new(), |ctx| {
            DragAndDrop::set_payload(ctx, sample());
        });
        // ...to the left of it, over the main window, at 600, 300 in
        // pixels, then let go of there.
        let out = pos2(-400.0, 100.0);
        pass(&ctx, window(), vec![Event::PointerMoved(out)], |_| ());
        let seen = |ctx: &Context| {
            let payload = DragAndDrop::payload::<Dragged>(ctx).map(|d| (*d).clone());
            let pointer = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
            (pointer, payload)
        };
        assert_eq!(
            pass(&ctx, ViewportId::ROOT, Vec::new(), seen),
            ((Some(pos2(600.0, 300.0)), false), Some(sample()))
        );
        // The window it's in sees the release, and egui drops what was
        // dragged at the end of its pass, before the main window's.
        pass(&ctx, window(), vec![release(out)], |_| ());
        assert!(DragAndDrop::payload::<Dragged>(&ctx).is_none());
        // The main window has it back for the release.
        assert_eq!(
            pass(&ctx, ViewportId::ROOT, Vec::new(), seen),
            ((Some(pos2(600.0, 300.0)), true), Some(sample()))
        );
        // Only the once.
        let after = pass(&ctx, ViewportId::ROOT, Vec::new(), seen);
        assert_eq!(after.1, None);
    }

    #[test]
    fn the_pointer_leaves_a_window_it_was_passed_on_to() {
        let ctx = Context::default();
        ctx.add_plugin(Carry::default());
        pass(&ctx, ViewportId::ROOT, Vec::new(), |ctx| {
            DragAndDrop::set_payload(ctx, sample());
        });
        // From the main window over the other, at 1500, 300 in pixels.
        let over = pos2(1500.0, 300.0);
        pass(
            &ctx,
            ViewportId::ROOT,
            vec![Event::PointerMoved(over)],
            |_| (),
        );
        let pointer = |ctx: &Context| ctx.input(|i| i.pointer.latest_pos());
        assert_eq!(
            pass(&ctx, window(), Vec::new(), pointer),
            Some(pos2(200.0, 100.0))
        );
        // Back in the main window, it's gone from the other.
        let back = pos2(500.0, 300.0);
        pass(
            &ctx,
            ViewportId::ROOT,
            vec![Event::PointerMoved(back)],
            |_| (),
        );
        assert_eq!(pass(&ctx, window(), Vec::new(), pointer), None);
        // Nothing's carried with nothing dragged.
        DragAndDrop::clear_payload(&ctx);
        pass(
            &ctx,
            ViewportId::ROOT,
            vec![Event::PointerMoved(over)],
            |_| (),
        );
        assert_eq!(pass(&ctx, window(), Vec::new(), pointer), None);
    }

    #[test]
    fn a_point_is_found_in_the_window_its_in() {
        let windows = windows();
        assert_eq!(
            under(pos2(1500.0, 300.0), ViewportId::ROOT, &windows, 1.0),
            Some((window(), pos2(200.0, 100.0)))
        );
        assert_eq!(
            under(pos2(600.0, 300.0), window(), &windows, 1.0),
            Some((ViewportId::ROOT, pos2(600.0, 300.0)))
        );
        // In the window it's from, or in none.
        assert_eq!(under(pos2(1500.0, 300.0), window(), &windows, 1.0), None);
        assert_eq!(
            under(pos2(5000.0, 300.0), ViewportId::ROOT, &windows, 1.0),
            None
        );
        // egui's zoom makes points bigger.
        let zoomed = windows.get(&window()).unwrap();
        assert_eq!(
            desktop(pos2(100.0, 0.0), zoomed, 2.0),
            Some(pos2(2700.0, 300.0))
        );
    }
}

//! The windows the panels are in: the main window, and the windows of their
//! own that the user moves panels out to. Each has its own tree of panels,
//! and its own place on the desktop, which the workspace keeps: where it
//! is, how big, and on which monitor. A window whose monitor is gone opens
//! on the main monitor.
//!
//! Places are in pixels on the desktop, and sizes in pixels at 100%
//! scaling, so a window on a monitor with other scaling keeps its size
//! there. egui works in points, its pixels per point being the monitor's
//! scaling times egui's zoom, so the shell turns them into points for the
//! window it moves, as the system scales that window when it gets there.

use egui::{Pos2, Vec2, ViewportBuilder, ViewportCommand, ViewportId, ViewportInfo, pos2, vec2};

use crate::dock::Dock;
use crate::keys::KeyPresses;
use crate::workspace::WindowLayout;

/// How long a window that the user moves or resizes stays put before its
/// place is saved, in seconds, so it isn't saved at every step.
pub(crate) const SETTLE: f64 = 1.0;

/// A monitor, as the app lists them for the shell: where windows can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    /// Where its top left corner is on the desktop, in pixels.
    pub at: [i32; 2],
    /// How big it is, in pixels.
    pub size: [u32; 2],
    /// Whether it's the main monitor, where a window whose monitor is gone
    /// opens.
    pub primary: bool,
}

/// How much of a window's title bar has to be on a monitor for the window
/// to go there, in pixels across and down.
const ON_A_MONITOR: [i64; 2] = [100, 16];
/// How tall a window's title bar is taken to be, in pixels.
const TITLE_BAR: i64 = 32;

/// Whether a window whose top left corner is at `at`, `width` pixels wide,
/// has enough of its title bar on one of `monitors` to be dragged. With no
/// monitors listed, it's taken to have.
pub fn on_a_monitor(at: [i32; 2], width: u32, monitors: &[Monitor]) -> bool {
    let [x, y] = at.map(i64::from);
    let right = x + i64::from(width);
    let needed = [ON_A_MONITOR[0].min(i64::from(width)), ON_A_MONITOR[1]];
    monitors.is_empty()
        || monitors.iter().any(|monitor| {
            let [left, top] = monitor.at.map(i64::from);
            let [across, down] = monitor.size.map(i64::from);
            let wide = right.min(left + across) - x.max(left);
            let high = (y + TITLE_BAR).min(top + down) - y.max(top);
            wide >= needed[0] && high >= needed[1]
        })
}

/// Where a window is, and how big: its place in its [`WindowLayout`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Place {
    /// Where its top left corner is on the desktop, in pixels.
    pub at: Option<[i32; 2]>,
    /// How big its inside is, in pixels at 100% scaling: its monitor's
    /// scaling makes it bigger.
    pub size: Option<[u32; 2]>,
    pub maximized: bool,
}

impl Place {
    /// Where the window egui describes as `viewport` is, now that egui
    /// zooms by `zoom`, when it was at `self` before. A minimized window
    /// stays where it was, and a maximized one keeps the place it goes back
    /// to, but for the monitor: one moved to another monitor while it was
    /// maximized goes back there.
    pub fn seen(self, viewport: &ViewportInfo, zoom: f32) -> Self {
        if viewport.minimized == Some(true) {
            return self;
        }
        // Pixels to an egui point, on the window's monitor.
        let scale = viewport.native_pixels_per_point.unwrap_or(1.0);
        let pixels = scale * zoom;
        let outer = viewport.outer_rect.map(|rect| rect * pixels);
        let at = outer.map(|rect| [rect.min.x.round() as i32, rect.min.y.round() as i32]);
        let size = (viewport.inner_rect).map(|rect| {
            let size = (rect.size() * zoom).round();
            [size.x.max(1.0) as u32, size.y.max(1.0) as u32]
        });
        if viewport.maximized != Some(true) {
            return Self {
                at: at.or(self.at),
                size: size.or(self.size),
                maximized: false,
            };
        }
        let mut back = Self {
            at: self.at.or(at),
            size: self.size.or(size),
            maximized: true,
        };
        if let (Some(outer), Some([x, y]), Some([width, height])) = (outer, back.at, back.size) {
            let middle = pos2(
                x as f32 + width as f32 * scale / 2.0,
                y as f32 + height as f32 * scale / 2.0,
            );
            if !outer.contains(middle) {
                let corner = (outer.min + vec2(40.0, 40.0) * scale).round();
                back.at = Some([corner.x as i32, corner.y as i32]);
            }
        }
        back
    }

    pub fn of(window: &WindowLayout) -> Self {
        Self {
            at: window.at,
            size: window.size,
            maximized: window.maximized,
        }
    }

    pub fn put_in(self, window: &mut WindowLayout) {
        window.at = self.at;
        window.size = self.size;
        window.maximized = self.maximized;
    }

    /// The place on one of `monitors`: where it is, if enough of its title
    /// bar is on one to drag it by, or else in the middle of the main
    /// monitor, no bigger than it.
    fn on(self, monitors: &[Monitor]) -> Self {
        let Some(at) = self.at else {
            return self;
        };
        let width = self.size.map_or(800, |[width, _]| width);
        let main = (monitors.iter().find(|monitor| monitor.primary)).or(monitors.first());
        let Some(main) = main.filter(|_| !on_a_monitor(at, width, monitors)) else {
            return self;
        };
        let size = self
            .size
            .map(|[width, height]| [width.min(main.size[0]), height.min(main.size[1])]);
        let [width, height] = size.unwrap_or([800, 600]);
        let middle = |start: i32, room: u32, length: u32| {
            start.saturating_add(i32::try_from(room.saturating_sub(length) / 2).unwrap_or(0))
        };
        Self {
            at: Some([
                middle(main.at[0], main.size[0], width),
                middle(main.at[1], main.size[1], height),
            ]),
            size,
            ..self
        }
    }
}

/// Where a window is, as the shell last saw it, and whether that's been
/// saved in the workspace.
#[derive(Debug, Clone, Default)]
pub(crate) struct Placement {
    /// Where the window is.
    pub place: Place,
    /// When it got there, in egui's time.
    since: f64,
    /// Where it was when the workspace was last handed out to be saved:
    /// `None` until the window has been seen since it was laid out.
    saved: Option<Place>,
    /// The time of the frame drawn last, in egui's time.
    now: f64,
}

impl Placement {
    /// A window that goes to `place`.
    pub fn at(place: Place) -> Self {
        Self {
            place,
            ..Self::default()
        }
    }

    /// Follows where the window is, as egui describes it in `viewport` at
    /// the time `now`, zooming by `zoom`.
    pub fn follow(&mut self, viewport: &ViewportInfo, zoom: f32, now: f64) {
        let place = self.place.seen(viewport, zoom);
        self.now = now;
        if place != self.place {
            self.place = place;
            self.since = now;
        }
        // Where the window is once it's laid out is where it was saved.
        self.saved.get_or_insert(place);
    }

    /// Whether the window moved since its place was saved. Unless `now`,
    /// only once it has stayed put a while, as a window the user moves or
    /// resizes is saved once it settles.
    pub fn moved(&self, now: bool) -> bool {
        let moved = self.saved.is_some_and(|saved| saved != self.place);
        moved && (now || self.now - self.since >= SETTLE)
    }

    /// Notes that the place is saved.
    pub fn saved(&mut self) {
        self.saved = Some(self.place);
    }

    /// Has the window go to `place`, from where the workspace has it.
    pub fn set(&mut self, place: Place) {
        self.place = place;
        self.saved = None;
    }

    /// Has the window stay where it is, as a workspace that doesn't say
    /// where it goes leaves it, with that place saved once it's seen.
    pub fn forget(&mut self) {
        self.saved = None;
    }
}

/// A window of the user's own, with panels they moved out of the main
/// window: an OS window of its own, as an egui viewport.
#[derive(Debug)]
pub(crate) struct Window {
    pub id: ViewportId,
    pub dock: Dock,
    pub placement: Placement,
    /// Where it's to go and how big, until it's there: the place the
    /// workspace has for it, or where the user dragged a tab to.
    goal: Option<Place>,
    /// How many frames it has been moved towards `goal`, which a window
    /// manager may not allow.
    tries: u8,
    /// The builder the window was made with, to hand egui the same each
    /// frame: egui moves a window whose builder moves it. `None` until it
    /// has been made.
    builder: Option<ViewportBuilder>,
    /// The panel with the focus while the user is in this window.
    pub focus: &'static str,
    pub keys: KeyPresses,
    /// Whether the window had the system's focus, as last seen.
    pub focused: bool,
    /// Whether to bring the window to the front next frame, as showing one
    /// of its panels does.
    pub raise: bool,
}

/// How many frames a window is moved towards where it's to go before the
/// shell gives up.
const TRIES: u8 = 20;

/// How near, in pixels, a window has to be to where it's to go, and to its
/// size, to be there: systems round.
const NEAR: i32 = 2;

impl Window {
    /// A window with the panels `dock` has, to go to `place`, as the
    /// viewport `id`. `focus` is the panel to give the focus to.
    pub fn new(id: ViewportId, dock: Dock, place: Place, focus: &'static str) -> Self {
        Self {
            id,
            dock,
            placement: Placement::at(place),
            goal: Some(place),
            tries: 0,
            builder: None,
            focus,
            keys: KeyPresses::new(),
            focused: false,
            raise: false,
        }
    }

    /// The builder for the window, called `title`, to hand egui this frame.
    /// The first time, the window goes to its place on one of `monitors`,
    /// in points as egui zooms by `zoom`, as if on a monitor whose pixels
    /// per point are `scale`. The system scales the window once it gets
    /// to its monitor, and [`follow_goal`](Self::follow_goal) puts it
    /// where it goes from there.
    pub fn builder(
        &mut self,
        title: &str,
        monitors: &[Monitor],
        scale: f32,
        zoom: f32,
    ) -> ViewportBuilder {
        if let Some(builder) = &mut self.builder {
            if builder.title.as_deref() != Some(title) {
                builder.title = Some(title.to_owned());
            }
            return builder.clone();
        }
        let goal = self.goal.map(|goal| goal.on(monitors));
        self.goal = goal;
        let mut builder = ViewportBuilder::default()
            .with_title(title)
            .with_min_inner_size([200.0, 120.0]);
        if let Some([width, height]) = goal.and_then(|goal| goal.size) {
            builder = builder.with_inner_size(vec2(width as f32, height as f32) / zoom);
        }
        if let Some([x, y]) = goal.and_then(|goal| goal.at) {
            builder = builder.with_position(pos2(x as f32, y as f32) / (scale * zoom));
        }
        self.builder = Some(builder.clone());
        builder
    }

    /// Moves the window towards where it's to go, as egui describes it in
    /// `viewport`, zooming by `zoom`: the place first, as the system scales
    /// the window for its monitor when it gets there, then the size, then
    /// maximized if it was. Returns the command to send the window, if any.
    pub fn follow_goal(&mut self, viewport: &ViewportInfo, zoom: f32) -> Option<ViewportCommand> {
        let goal = self.goal?;
        let scale = viewport.native_pixels_per_point?;
        let seen = Place::default().seen(viewport, zoom);
        let near = |a: Option<[i32; 2]>, b: [i32; 2]| {
            a.is_some_and(|a| (a[0] - b[0]).abs() <= NEAR && (a[1] - b[1]).abs() <= NEAR)
        };
        let size = |size: Option<[u32; 2]>| size.map(|[x, y]| [x as i32, y as i32]);
        self.tries += 1;
        let command = match (goal.at, size(goal.size)) {
            (Some(at), _) if !near(seen.at, at) => {
                let at = pos2(at[0] as f32, at[1] as f32) / (scale * zoom);
                Some(ViewportCommand::OuterPosition(at))
            }
            (_, Some(wanted)) if !near(size(seen.size), wanted) => {
                let points = vec2(wanted[0] as f32, wanted[1] as f32) / zoom;
                Some(ViewportCommand::InnerSize(points))
            }
            _ => None,
        };
        if command.is_some() && self.tries < TRIES {
            return command;
        }
        self.goal = None;
        goal.maximized.then_some(ViewportCommand::Maximized(true))
    }

    /// Where the window is going, if it's not there yet, or else where it
    /// is: for the workspace.
    pub fn place(&self) -> Place {
        self.goal.unwrap_or(self.placement.place)
    }

    /// Whether the window is still on its way to where it's to go.
    pub fn placing(&self) -> bool {
        self.goal.is_some()
    }

    /// How many panels it shows: none, and it closes.
    pub fn shown(&self) -> usize {
        self.dock.count_shown()
    }
}

/// Where a point `at` in the window egui describes as `viewport` is on the
/// desktop, in pixels, zooming by `zoom`. `None` without the window's place.
pub(crate) fn on_desktop(at: Pos2, viewport: &ViewportInfo, zoom: f32) -> Option<[i32; 2]> {
    let inner = viewport.inner_rect?;
    let pixels = viewport.native_pixels_per_point? * zoom;
    let at = ((inner.min + at.to_vec2()) * pixels).round();
    Some([at.x as i32, at.y as i32])
}

/// The size `size`, in points zoomed by `zoom`, in pixels at 100% scaling.
pub(crate) fn unscaled(size: Vec2, zoom: f32) -> [u32; 2] {
    let size = (size * zoom).round();
    [size.x.max(1.0) as u32, size.y.max(1.0) as u32]
}

#[cfg(test)]
mod tests {
    use egui::Rect;

    use super::*;

    const MAIN: Monitor = Monitor {
        at: [0, 0],
        size: [3840, 1600],
        primary: true,
    };
    const LEFT: Monitor = Monitor {
        at: [-1920, 200],
        size: [1920, 1080],
        primary: false,
    };

    #[test]
    fn a_window_keeps_the_place_it_goes_back_to() {
        let viewport = |min: [f32; 2], size: [f32; 2], maximized| egui::ViewportInfo {
            native_pixels_per_point: Some(1.5),
            outer_rect: Some(egui::Rect::from_min_size(min.into(), size.into())),
            inner_rect: Some(egui::Rect::from_min_size(
                egui::pos2(min[0] + 5.0, min[1] + 20.0),
                egui::vec2(size[0] - 10.0, size[1] - 25.0),
            )),
            maximized: Some(maximized),
            ..egui::ViewportInfo::default()
        };
        // Its corner in pixels, and its inside's size at 100% scaling.
        let place = Place::default().seen(&viewport([100.0, 50.0], [810.0, 625.0], false), 1.0);
        let normal = Place {
            at: Some([150, 75]),
            size: Some([800, 600]),
            maximized: false,
        };
        assert_eq!(place, normal);
        // Maximized on the same monitor, it goes back to where it was.
        let maximized = place.seen(&viewport([-5.0, -5.0], [1290.0, 700.0], true), 1.0);
        assert_eq!(
            maximized,
            Place {
                maximized: true,
                ..normal
            }
        );
        // Minimized, it's where it was.
        let minimized = egui::ViewportInfo {
            minimized: Some(true),
            ..viewport([-32000.0, -32000.0], [160.0, 28.0], false)
        };
        assert_eq!(maximized.seen(&minimized, 1.0), maximized);
        // Maximized on another monitor, it goes back to that one.
        let moved = maximized.seen(&viewport([1275.0, -5.0], [1290.0, 700.0], true), 1.0);
        assert_eq!(moved.at, Some([1973, 53]), "40 points in from its corner");
        assert_eq!(moved.size, normal.size);
        // egui's zoom makes points bigger, but not pixels.
        let zoomed = Place::default().seen(&viewport([100.0, 50.0], [810.0, 625.0], false), 2.0);
        assert_eq!(zoomed.at, Some([300, 150]));
        assert_eq!(zoomed.size, Some([1600, 1200]));
    }

    #[test]
    fn a_window_goes_where_its_title_bar_can_be_dragged() {
        let monitors = [MAIN, LEFT];
        assert!(on_a_monitor([100, 100], 800, &monitors));
        assert!(on_a_monitor([-1800, 300], 800, &monitors));
        // Maximized windows hang over their monitor's edges.
        assert!(on_a_monitor([-8, -8], 3856, &monitors));
        // Off the left monitor's top, which is lower than the main one's,
        // or with its title bar off the main one's.
        assert!(!on_a_monitor([-1800, 100], 800, &monitors));
        assert!(!on_a_monitor([100, -40], 800, &monitors));
        // Off every monitor, or only 40 pixels on.
        assert!(!on_a_monitor([5000, 100], 800, &monitors));
        assert!(!on_a_monitor([3800, 100], 800, &monitors), "40 pixels on");
        assert!(on_a_monitor([3700, 100], 800, &monitors));
        assert!(on_a_monitor([-5000, -5000], 800, &[]), "monitors unknown");
    }

    #[test]
    fn a_window_whose_monitor_is_gone_opens_in_the_middle_of_the_main_one() {
        let place = Place {
            at: Some([-1800, 300]),
            size: Some([1000, 600]),
            maximized: true,
        };
        assert_eq!(place.on(&[MAIN, LEFT]), place);
        let moved = place.on(&[MAIN]);
        assert_eq!(moved.at, Some([1420, 500]));
        assert_eq!((moved.size, moved.maximized), (place.size, true));
        // No bigger than the monitor.
        let big = Place {
            at: Some([-6000, 300]),
            size: Some([5000, 600]),
            ..place
        };
        assert_eq!(big.on(&[MAIN]).at, Some([0, 500]));
        assert_eq!(big.on(&[MAIN]).size, Some([3840, 600]));
    }

    /// A window as egui describes it: its inside at `inner` and its frame
    /// at `outer`, in points, on a monitor whose pixels per point are
    /// `scale`.
    fn viewport(inner: Rect, outer: Rect, scale: f32) -> ViewportInfo {
        ViewportInfo {
            inner_rect: Some(inner),
            outer_rect: Some(outer),
            native_pixels_per_point: Some(scale),
            ..ViewportInfo::default()
        }
    }

    #[test]
    fn a_window_is_moved_then_sized_where_it_goes() {
        let place = Place {
            at: Some([-1800, 300]),
            size: Some([800, 500]),
            maximized: true,
        };
        let dock = Dock::of_panel(crate::panels::MIXER);
        let mut window = Window::new(ViewportId::from_hash_of("test"), dock, place, "mixer");
        // Made on a monitor at 150%, egui zooming by 1.
        let points = Rect::from_min_size(pos2(100.0, 100.0), vec2(800.0, 500.0) / 1.5);
        let made = viewport(points, points, 1.5);
        assert_eq!(
            window.follow_goal(&made, 1.0),
            Some(ViewportCommand::OuterPosition(pos2(-1200.0, 200.0)))
        );
        // There, at 100%, the system scaled it.
        let there = Rect::from_min_size(pos2(-1800.0, 300.0), vec2(533.0, 333.0));
        let moved = viewport(there, there, 1.0);
        assert_eq!(
            window.follow_goal(&moved, 1.0),
            Some(ViewportCommand::InnerSize(vec2(800.0, 500.0)))
        );
        let sized = Rect::from_min_size(pos2(-1800.0, 300.0), vec2(800.0, 500.0));
        let done = viewport(sized, sized, 1.0);
        assert_eq!(
            window.follow_goal(&done, 1.0),
            Some(ViewportCommand::Maximized(true))
        );
        assert_eq!(window.follow_goal(&done, 1.0), None);
        window.placement.follow(&done, 1.0, 0.0);
        assert_eq!(window.place(), Place::default().seen(&done, 1.0));
    }

    #[test]
    fn a_window_that_cant_get_there_stays_where_it_is() {
        let place = Place {
            at: Some([500, 300]),
            size: None,
            maximized: false,
        };
        let dock = Dock::of_panel(crate::panels::MIXER);
        let mut window = Window::new(ViewportId::from_hash_of("stuck"), dock, place, "mixer");
        let stuck = Rect::from_min_size(pos2(10.0, 10.0), vec2(400.0, 300.0));
        let stuck = viewport(stuck, stuck, 1.0);
        let sent = (0..40)
            .filter_map(|_| window.follow_goal(&stuck, 1.0))
            .count();
        assert_eq!(sent, usize::from(TRIES) - 1);
        window.placement.follow(&stuck, 1.0, 0.0);
        assert_eq!(window.place().at, Some([10, 10]));
    }

    #[test]
    fn a_point_in_a_window_is_found_on_the_desktop() {
        let inner = Rect::from_min_size(pos2(-1000.0, 100.0), vec2(800.0, 600.0));
        let viewport = viewport(inner, inner, 1.25);
        assert_eq!(
            on_desktop(pos2(40.0, 20.0), &viewport, 1.0),
            Some([-1200, 150])
        );
        assert_eq!(
            on_desktop(pos2(40.0, 20.0), &ViewportInfo::default(), 1.0),
            None
        );
        assert_eq!(unscaled(vec2(400.0, 300.4), 1.5), [600, 451]);
    }
}

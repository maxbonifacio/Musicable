//! Views built from parts: the transport bar, the track headers and the
//! mixer strips. Each kind of part registers its id, its title, its size,
//! the views it fits in and what draws it, so a feature adds a part in one
//! place. A view shows the parts that fit it in the user's order, at the
//! sizes they gave them, without those they hid: right-clicking a part
//! offers to hide it, and the Customise window lists them all, to show,
//! move or resize. A workspace keeps what the user changed ([`Views`]).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use egui::{
    Align2, DragValue, Id, Key, Popup, Response, RichText, Role, Sense, SetOpenCommand, Stroke, Ui,
    WidgetInfo, vec2,
};
use serde::{Deserialize, Serialize};

use crate::arrangement::HeaderPart;
use crate::mixer::{Channel, Mixer};
use crate::shell::Action;
use crate::shortcuts::first_press;
use crate::theme::Theme;
use crate::transport_bar::Bar;

/// The transport bar, along the top of the window.
pub const TRANSPORT_BAR: &str = "transport_bar";
/// The track headers, beside the arrangement's lanes.
pub const TRACK_HEADER: &str = "track_header";
/// The mixer's strips.
pub const MIXER_STRIP: &str = "mixer_strip";

/// The views built from parts, with what menus and windows call them.
const VIEWS: [(&str, &str); 3] = [
    (TRANSPORT_BAR, "Transport Bar"),
    (TRACK_HEADER, "Track Headers"),
    (MIXER_STRIP, "Mixer Strips"),
];

/// The sizes the user can give a part, in points.
const SIZES: RangeInclusive<f32> = 1.0..=400.0;

/// A kind of part.
#[derive(Debug)]
pub(crate) struct Part {
    pub id: &'static str,
    /// What menus and the Customise window call it.
    pub title: &'static str,
    /// Whether views show it until the user hides it.
    pub shown: bool,
    pub size: Size,
    /// The views it can go in, by id.
    pub fits: &'static [&'static str],
    pub draw: Draw,
}

/// How much room a part takes: across a bar or a track header, or down a
/// mixer strip.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Size {
    /// What's in it needs.
    Natural,
    /// What the other parts leave.
    Rest,
    /// This many points, from the theme.
    Fixed(fn(&Theme) -> f32),
    /// This many points, from the theme until the user gives it a size.
    Resizable(fn(&Theme) -> f32),
}

/// What draws a part, by the kind of view it goes in.
#[derive(Debug)]
pub(crate) enum Draw {
    /// A bar's part, where the bar's layout puts it, left to right, given
    /// its size if it has one. Parts of the same `section` go together, and
    /// a line separates the sections.
    Bar {
        section: &'static str,
        draw: fn(&mut Ui, &Bar<'_, '_>, Option<f32>, &mut Vec<Action>),
    },
    /// A track header's part, painted in the place it's given.
    Header(fn(&mut Ui, &HeaderPart<'_>, &mut Vec<Action>)),
    /// A mixer strip's part: how many of the strip's rows it takes, which
    /// is the same in every strip so their faders line up, and what draws
    /// them.
    Strip {
        rows: fn(&Mixer<'_>) -> usize,
        draw: fn(&mut Ui, &Mixer<'_>, &Channel<'_>, &mut Vec<Action>),
    },
}

/// Every kind of part, in the order views show them at first.
#[derive(Debug)]
pub(crate) struct Registry {
    parts: Vec<Part>,
}

impl Registry {
    /// Musicable's own parts.
    pub fn new() -> Self {
        let mut registry = Self { parts: Vec::new() };
        crate::transport_bar::register(&mut registry);
        crate::arrangement::register(&mut registry);
        crate::mixer::register(&mut registry);
        registry
    }

    /// Adds a kind of part, after those that fit the same views.
    pub fn register(&mut self, part: Part) {
        debug_assert!(
            part.fits
                .iter()
                .all(|view| self.get(view, part.id).is_none()),
            "two parts are called {} in one view",
            part.id
        );
        self.parts.push(part);
    }

    /// The part with id `id` in `view`.
    pub fn get(&self, view: &str, id: &str) -> Option<&Part> {
        (self.parts.iter()).find(|part| part.id == id && part.fits.contains(&view))
    }

    /// The parts that fit `view`, in the order it shows them at first.
    pub fn fitting<'a>(&'a self, view: &'a str) -> impl Iterator<Item = &'a Part> + 'a {
        self.parts
            .iter()
            .filter(move |part| part.fits.contains(&view))
    }
}

/// A part a view shows, with its size in points if it has one: the size
/// the user gave it, or else the theme's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shown<'a> {
    pub part: &'a Part,
    pub size: Option<f32>,
}

/// One of a view's parts, as the user has it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    id: &'static str,
    shown: bool,
    /// The size the user gave it, in points.
    size: Option<f32>,
}

/// What the user picked from a part's menu.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Asked {
    /// Hide the part with this id in the view.
    Hide(&'static str, &'static str),
    /// Show the Customise window for the view.
    Customise(&'static str),
}

/// The views built from parts as the user has them, and the window that
/// customises them.
#[derive(Debug)]
pub(crate) struct Parts {
    registry: Registry,
    /// Each view's parts, shown and hidden, in order, by the view's id.
    lineups: BTreeMap<&'static str, Vec<Entry>>,
    /// The view the Customise window lists, while it's open.
    customising: Option<&'static str>,
    /// The view the open part menu was opened on, and the part, if it was
    /// opened on one.
    menu: Cell<Option<(&'static str, Option<&'static str>)>>,
    /// What the user picked from a part menu, to be done once the views are
    /// drawn.
    asked: Cell<Option<Asked>>,
    /// Whether the user changed a view since the app last asked.
    changed: bool,
}

impl Default for Parts {
    fn default() -> Self {
        let registry = Registry::new();
        let lineups = VIEWS
            .iter()
            .map(|&(view, _)| (view, first_lineup(&registry, view)))
            .collect();
        Self {
            registry,
            lineups,
            customising: None,
            menu: Cell::new(None),
            asked: Cell::new(None),
            changed: false,
        }
    }
}

impl Parts {
    /// The parts `view` shows, in order, with their sizes.
    pub fn shown(&self, view: &str, theme: &Theme) -> Vec<Shown<'_>> {
        let entries = self.lineups.get(view).into_iter().flatten();
        entries
            .filter(|entry| entry.shown)
            .filter_map(|entry| {
                let part = self.registry.get(view, entry.id)?;
                let size = match part.size {
                    Size::Natural | Size::Rest => None,
                    Size::Fixed(size) => Some(size(theme)),
                    Size::Resizable(size) => Some(entry.size.unwrap_or_else(|| size(theme))),
                };
                Some(Shown { part, size })
            })
            .collect()
    }

    /// Adds to a context menu opened on the part `part` of `view`, or on
    /// the view between its parts, an entry that hides the part and one
    /// that shows the Customise window.
    pub fn menu_items(&self, ui: &mut Ui, view: &'static str, part: Option<&'static str>) {
        if let Some(part) = part.and_then(|id| self.registry.get(view, id))
            && ui.button(format!("Hide {}", part.title)).clicked()
        {
            self.asked.set(Some(Asked::Hide(view, part.id)));
            ui.close();
        }
        if ui.button(format!("Customise {}...", title(view))).clicked() {
            self.asked.set(Some(Asked::Customise(view)));
            ui.close();
        }
    }

    /// The menu of [`menu_items`](Self::menu_items) for a right-click on
    /// `view` that opened no other menu, at the pointer. `anchor` is the
    /// view's response, and `clicked` says whether it was right-clicked
    /// this frame: on the part with that id, or between its parts.
    pub fn context_menu(
        &self,
        anchor: &Response,
        view: &'static str,
        clicked: Option<Option<&'static str>>,
    ) {
        let id = Popup::default_response_id(anchor);
        let ctx = &anchor.ctx;
        // A menu of the control's own goes first.
        let open = clicked.filter(|_| !Popup::is_any_open(ctx) || Popup::is_id_open(ctx, id));
        if let Some(part) = open {
            self.menu.set(Some((view, part)));
        }
        Popup::context_menu(anchor)
            .open_memory(open.map(|_| SetOpenCommand::Bool(true)))
            .show(|ui| {
                if let Some((on, part)) = self.menu.get()
                    && on == view
                {
                    self.menu_items(ui, view, part);
                }
            });
    }

    /// Does what the user picked from a part menu this frame, now that the
    /// views are drawn.
    pub fn carry_out(&mut self) {
        match self.asked.take() {
            Some(Asked::Hide(view, part)) => self.set_shown(view, part, false),
            Some(Asked::Customise(view)) => self.customising = Some(view),
            None => {}
        }
    }

    /// Shows the part with id `part` in `view`, or hides it.
    pub fn set_shown(&mut self, view: &str, part: &str, shown: bool) {
        let entry =
            (self.lineups.get_mut(view).into_iter().flatten()).find(|entry| entry.id == part);
        if let Some(entry) = entry
            && entry.shown != shown
        {
            entry.shown = shown;
            self.changed = true;
        }
    }

    /// Shows the Customise window, if it's open. Escape closes it.
    pub fn show_customise(&mut self, ctx: &egui::Context, theme: &Theme) {
        let Some(view) = self.customising else {
            return;
        };
        let mut open = !ctx.input_mut(|input| first_press(input, Key::Escape));
        // It opens below the transport bar, where the keyboard shortcuts
        // window does, out of the way of the views it customises.
        let top = ctx.content_rect().center_top() + vec2(0.0, 72.0);
        egui::Window::new(format!("Customise {}", title(view)))
            .id(Id::unique("customise parts"))
            .open(&mut open)
            .pivot(Align2::CENTER_TOP)
            .default_pos(top)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| self.list(ui, view, theme));
        if !open {
            self.customising = None;
        }
    }

    /// Every part of `view`, each with a handle to drag it by, whether it's
    /// shown, and its size, if that can change.
    fn list(&mut self, ui: &mut Ui, view: &'static str, theme: &Theme) {
        let registry = &self.registry;
        let Some(entries) = self.lineups.get_mut(view) else {
            return;
        };
        ui.label(RichText::new("Drag a part by its handle to move it.").weak());
        let across = view != MIXER_STRIP;
        let mut changed = false;
        // Where a part was dropped: its place in the list, and the place it
        // goes before.
        let mut moved = None;
        for (i, entry) in entries.iter_mut().enumerate() {
            let Some(part) = registry.get(view, entry.id) else {
                continue;
            };
            let row = ui.horizontal(|ui| {
                let id = Id::unique(("move part", view, part.id));
                ui.dnd_drag_source(id, i, |ui| handle(ui, part.title));
                changed |= ui.checkbox(&mut entry.shown, part.title).changed();
                if let Size::Resizable(size) = part.size {
                    let mut points = entry.size.unwrap_or_else(|| size(theme));
                    let field = ui.add(
                        DragValue::new(&mut points)
                            .range(SIZES)
                            .speed(0.5)
                            .max_decimals(0)
                            .suffix(" pt"),
                    );
                    let what = if across { "Width" } else { "Height" };
                    field.widget_info(|| WidgetInfo {
                        value: Some(f64::from(points)),
                        ..WidgetInfo::labeled(
                            Role::SpinButton,
                            true,
                            format!("{} {what}", part.title),
                        )
                    });
                    if field.changed() {
                        entry.size = Some(points.round());
                        changed = true;
                    }
                    field.on_hover_text(format!("Its {} in points", what.to_lowercase()));
                }
            });
            let row = row.response;
            if row.dnd_hover_payload::<usize>().is_some()
                && let Some(pointer) = ui.ctx().pointer_interact_pos()
            {
                // A line where it would go: over this part, or under it.
                let over = pointer.y < row.rect.center().y;
                let y = if over {
                    row.rect.top()
                } else {
                    row.rect.bottom()
                };
                let stroke = ui.visuals().selection.stroke;
                ui.painter().hline(row.rect.x_range(), y, stroke);
                if let Some(from) = row.dnd_release_payload::<usize>() {
                    moved = Some((*from, if over { i } else { i + 1 }));
                }
            }
        }
        if let Some((from, before)) = moved
            && from < entries.len()
        {
            let to = if before > from { before - 1 } else { before };
            if to != from {
                let entry = entries.remove(from);
                entries.insert(to.min(entries.len()), entry);
                changed = true;
            }
        }
        ui.separator();
        let reset = ui.button("Reset").on_hover_text(
            "Shows every part Musicable shows at first, in its order and at its size",
        );
        if reset.clicked() {
            let first = first_lineup(registry, view);
            changed |= *entries != first;
            *entries = first;
        }
        self.changed |= changed;
    }

    /// What the user changed about the views, for a workspace to keep.
    pub fn views(&self) -> Views {
        let changed = self
            .lineups
            .iter()
            .filter(|&(view, entries)| *entries != first_lineup(&self.registry, view));
        let views = changed.map(|(&view, entries)| {
            let ids = |entries: &mut dyn Iterator<Item = &Entry>| {
                entries.map(|entry| entry.id.to_owned()).collect()
            };
            let lineup = Lineup {
                parts: ids(&mut entries.iter()),
                hidden: ids(&mut entries.iter().filter(|entry| !entry.shown)),
                sizes: (entries.iter())
                    .filter_map(|entry| Some((entry.id.to_owned(), entry.size?.round() as u32)))
                    .collect(),
            };
            (view.to_owned(), lineup)
        });
        Views(views.collect())
    }

    /// Shows the views as `views` has them, and the others as at first.
    pub fn set_views(&mut self, views: &Views) {
        for (&view, entries) in &mut self.lineups {
            *entries = match views.0.get(view) {
                Some(lineup) => read_lineup(&self.registry, view, lineup),
                None => first_lineup(&self.registry, view),
            };
        }
    }

    /// Whether the user changed a view since this was last asked.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// What the views built from parts, as the user has them, are called.
fn title(view: &str) -> &'static str {
    VIEWS
        .iter()
        .find(|&&(id, _)| id == view)
        .map_or("View", |&(_, title)| title)
}

/// The handle the Customise window drags a part's row by: three lines.
fn handle(ui: &mut Ui, title: &str) -> Response {
    let spacing = ui.spacing();
    let size = vec2(spacing.icon_width, spacing.interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("Move {title}")));
    if ui.is_rect_visible(rect) {
        let stroke = Stroke::new(1.5, ui.visuals().weak_text_color());
        let step = size.x / 4.0;
        let across = rect.shrink2(vec2(size.x / 6.0, 0.0)).x_range();
        for line in [-1.0, 0.0, 1.0] {
            let y = ui
                .painter()
                .round_to_pixel_center(rect.center().y + line * step);
            ui.painter().hline(across, y, stroke);
        }
    }
    response.on_hover_text("Drag to move it")
}

/// The parts of `view` as Musicable shows them at first.
fn first_lineup(registry: &Registry, view: &str) -> Vec<Entry> {
    let parts = registry.fitting(view);
    parts
        .map(|part| Entry {
            id: part.id,
            shown: part.shown,
            size: None,
        })
        .collect()
}

/// `lineup`'s parts of `view`, without those Musicable doesn't have, and
/// with those it doesn't name where they go at first: after the part before
/// them.
fn read_lineup(registry: &Registry, view: &str, lineup: &Lineup) -> Vec<Entry> {
    let entry = |part: &Part| Entry {
        id: part.id,
        shown: !lineup.hidden.iter().any(|id| id == part.id),
        size: (lineup.sizes.get(part.id))
            .filter(|_| matches!(part.size, Size::Resizable(_)))
            .map(|&size| (size as f32).clamp(*SIZES.start(), *SIZES.end())),
    };
    let mut entries: Vec<Entry> = Vec::new();
    for id in &lineup.parts {
        if let Some(part) = registry.get(view, id)
            && !entries.iter().any(|entry| entry.id == part.id)
        {
            entries.push(entry(part));
        }
    }
    let mut after = None;
    for part in registry.fitting(view) {
        if !entries.iter().any(|entry| entry.id == part.id) {
            let at = after
                .and_then(|id| entries.iter().position(|entry| entry.id == id))
                .map_or(0, |i| i + 1);
            let shown = part.shown && entry(part).shown;
            entries.insert(
                at,
                Entry {
                    shown,
                    ..entry(part)
                },
            );
        }
        after = Some(part.id);
    }
    entries
}

/// What the user changed about the views built from parts, by the views'
/// ids, as a workspace keeps it. A view it doesn't have shows its parts as
/// Musicable does at first.
///
/// ```toml
/// [views.mixer_strip]
/// hidden = ["monitor"]
/// parts = ["name", "output", "input", "monitor", "sends", "pan", "fader", "readouts", "buttons", "clip_type"]
///
/// [views.track_header.sizes]
/// meter = 14
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Views(pub BTreeMap<String, Lineup>);

impl Views {
    /// Whether every view shows its parts as at first.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A view's parts, as the user has them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lineup {
    /// Its parts, shown and hidden, in order, by id. Those it leaves out
    /// go where they go at first.
    pub parts: Vec<String>,
    /// The parts hidden.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// The sizes the user gave parts, in points.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sizes: BTreeMap<String, u32>,
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui::{Pos2, vec2};
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::Workspace;
    use crate::testing::{State, harness};

    type Harness = egui_kittest::Harness<'static, State>;

    /// Drags with the mouse from `from` to `to`, a few steps at a time.
    fn drag(harness: &mut Harness, from: Pos2, to: Pos2) {
        harness.hover_at(from);
        harness.run();
        harness.drag_at(from);
        harness.run();
        for step in 1..=5 {
            harness.hover_at(from + (to - from) * step as f32 / 5.0);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();
    }

    /// The lineup of `view` the shell keeps in the workspace, if the user
    /// changed it.
    fn lineup(harness: &Harness, view: &str) -> Option<Lineup> {
        harness.state().shell.workspace().views.0.remove(view)
    }

    fn strings(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|&id| id.to_owned()).collect()
    }

    fn project() -> Project {
        let mut project = Project::new();
        project.add_track("Lead", TrackKind::Audio);
        project.add_track("Reverb", TrackKind::Return);
        project
    }

    #[test]
    fn right_clicking_a_part_offers_to_hide_it() {
        let mut harness = harness(project());
        harness.get_by_label("Metronome").click_secondary();
        harness.run();
        harness.get_by_label("Customise Transport Bar...");
        harness.get_by_label("Hide Metronome").click();
        harness.run();
        assert!(harness.query_by_label("Metronome").is_none());
        harness.get_by_label("Loop");
        let hidden = lineup(&harness, TRANSPORT_BAR).unwrap().hidden;
        assert_eq!(hidden, ["metronome"]);
        let shell = &mut harness.state_mut().shell;
        assert!(shell.changed_workspace().is_some());
        assert!(shell.changed_workspace().is_none(), "it was asked already");

        // A mixer strip's controls, and the track headers' buttons, which
        // take their own clicks.
        harness.get_by_label("Lead Input").click_secondary();
        harness.run();
        harness.get_by_label("Hide Input").click();
        harness.run();
        assert!(harness.query_by_label("Lead Input").is_none());
        harness.get_by_label("Mute track Lead").click_secondary();
        harness.run();
        // The track's menu, with the part's at the end.
        harness.get_by_label("Delete Tracks");
        harness.get_by_label("Hide Mute, Solo and Arm").click();
        harness.run();
        assert!(harness.query_by_label("Mute track Lead").is_none());
        harness.get_by_label("Show automation of track Lead");
        // The mixer's own buttons stay.
        harness.get_by_label("Mute Lead");
        let views = harness.state().shell.workspace().views;
        assert_eq!(views.0[MIXER_STRIP].hidden, ["input"]);
        assert_eq!(views.0[TRACK_HEADER].hidden, ["buttons"]);
    }

    #[test]
    fn a_control_with_a_menu_adds_the_parts_to_it() {
        let mut harness = harness(project());
        // A control that takes clicks, without a menu of its own.
        harness
            .get_by_label("Master Highest Peak")
            .click_secondary();
        harness.run();
        harness.get_by_label("Hide Gain and Peak").click();
        harness.run();
        assert!(harness.query_by_label("Master Highest Peak").is_none());
        assert_eq!(lineup(&harness, MIXER_STRIP).unwrap().hidden, ["readouts"]);
        // The pan's menu is its automation's, then the part's.
        harness
            .get_by_role_and_label(Role::Slider, "Lead Pan")
            .click_secondary();
        harness.run();
        harness.get_by_label("Show Automation");
        harness.get_by_label("Hide Pan");
        harness.get_by_label("Customise Mixer Strips...").click();
        harness.run();
        harness.get_by_label("Customise Mixer Strips");
        let readouts = harness.get_by_role_and_label(Role::CheckBox, "Gain and Peak");
        assert_eq!(
            readouts.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::False)
        );
    }

    #[test]
    fn the_customise_window_shows_moves_and_resizes_parts() {
        let mut harness = harness(project());
        let mute = harness.get_by_label("Mute track Lead").rect();
        let theme = Theme::default();
        // Where the button was before headers were made of parts.
        let headers_right = harness.get_by_label("Track Lead").rect().right();
        let buttons = 3.0 * theme.button.x + 2.0 * theme.padding / 2.0;
        let left = headers_right - 2.0 * theme.padding - theme.header_meter - buttons;
        assert_eq!(mute.left(), left);

        harness.get_by_label("Track Lead").click_secondary();
        harness.run();
        harness.get_by_label("Customise Track Headers...").click();
        harness.run();
        let titles = [
            "Colour",
            "Name",
            "Automation Button",
            "Mute, Solo and Arm",
            "Meter",
        ];
        for title in titles {
            harness.get_by_role_and_label(Role::CheckBox, title);
        }
        // Hidden, then shown again: as at first.
        harness
            .get_by_role_and_label(Role::CheckBox, "Name")
            .click();
        harness.run();
        assert_eq!(lineup(&harness, TRACK_HEADER).unwrap().hidden, ["name"]);
        harness
            .get_by_role_and_label(Role::CheckBox, "Name")
            .click();
        harness.run();
        assert!(lineup(&harness, TRACK_HEADER).is_none());

        // The meter, wider, pushes the buttons left.
        let width = harness.get_by_label("Meter Width");
        assert_eq!(
            width.accesskit_node().numeric_value(),
            Some(f64::from(theme.header_meter + theme.padding / 2.0))
        );
        width.set_value_accesskit(14.0);
        harness.run();
        let moved = harness.get_by_label("Mute track Lead").rect();
        let wider = 14.0 - theme.header_meter - theme.padding / 2.0;
        assert_eq!(moved.left(), mute.left() - wider);
        let sizes = lineup(&harness, TRACK_HEADER).unwrap().sizes;
        assert_eq!(sizes, BTreeMap::from([("meter".to_owned(), 14)]));

        // Dragged by its handle over the colour, the meter goes first.
        let from = harness.get_by_label("Move Meter").rect().center();
        let colour = harness.get_by_label("Move Colour").rect();
        drag(&mut harness, from, colour.center() - vec2(0.0, 4.0));
        let parts = lineup(&harness, TRACK_HEADER).unwrap().parts;
        let order = ["meter", "colour", "name", "automation", "buttons"];
        assert_eq!(parts, strings(&order));

        // Reset puts it all back.
        harness.get_by_label("Reset").click();
        harness.run();
        assert!(lineup(&harness, TRACK_HEADER).is_none());
        assert_eq!(harness.get_by_label("Mute track Lead").rect(), mute);

        // Escape closes it.
        harness.get_by_label("Customise Track Headers");
        harness.key_press(Key::Escape);
        harness.run();
        assert!(harness.query_by_label("Customise Track Headers").is_none());
    }

    #[test]
    fn the_transport_bar_keeps_the_cpu_meter_at_its_end() {
        let mut harness = harness(project());
        let right = harness.ctx.content_rect().right();
        let dropouts = harness.get_by_label("No dropouts").rect();
        // Inside the panel's margin.
        assert!(right - dropouts.right() < 16.0, "{dropouts:?} in {right}");
        let meter = harness
            .get_by_role_and_label(Role::Meter, "CPU load")
            .rect();
        assert_eq!(meter.width(), Theme::default().cpu_meter.x);

        // A wider meter, and the position moved after it.
        let views = Views(BTreeMap::from([(
            TRANSPORT_BAR.to_owned(),
            Lineup {
                parts: strings(&["go_to_start", "play_stop", "space", "cpu", "position"]),
                hidden: strings(&["record", "capture"]),
                sizes: BTreeMap::from([("cpu".to_owned(), 100)]),
            },
        )]));
        let shell = &mut harness.state_mut().shell;
        let workspace = Workspace {
            views,
            ..shell.workspace()
        };
        shell.set_workspace(&workspace);
        harness.run();
        let meter = harness
            .get_by_role_and_label(Role::Meter, "CPU load")
            .rect();
        assert_eq!(meter.width(), 100.0);
        let position = harness.get_by_label("1. 1. 1").rect();
        assert!(
            position.left() > meter.right(),
            "{position:?} after {meter:?}"
        );
        assert!(harness.query_by_label("Capture").is_none());
        harness.get_by_label("Loop");
        // Setting them isn't a change to save.
        assert!(harness.state_mut().shell.changed_workspace().is_none());
    }

    #[test]
    fn views_come_back_as_they_were_saved() {
        let mut parts = Parts::default();
        let text = r#"
            [mixer_strip]
            parts = ["name", "output", "gone", "input", "sends", "pan", "fader", "name"]
            hidden = ["sends"]

            [track_header.sizes]
            meter = 14
            name = 300
            colour = 0
        "#;
        let views: Views = toml::from_str(text).unwrap();
        parts.set_views(&views);
        let strip: Vec<_> = parts.lineups[MIXER_STRIP]
            .iter()
            .map(|entry| (entry.id, entry.shown))
            .collect();
        // Unknown parts go, a part named twice goes where it's named first,
        // and the parts it doesn't name go after the parts before them.
        assert_eq!(
            strip,
            [
                ("name", true),
                ("output", true),
                ("input", true),
                ("monitor", true),
                ("sends", false),
                ("pan", true),
                ("fader", true),
                ("readouts", true),
                ("buttons", true),
                ("clip_type", true),
            ]
        );
        // Only parts that can be resized keep a size, within limits.
        let sizes: Vec<_> = parts.lineups[TRACK_HEADER]
            .iter()
            .map(|entry| (entry.id, entry.size))
            .collect();
        assert_eq!(
            sizes,
            [
                ("colour", Some(1.0)),
                ("name", None),
                ("automation", None),
                ("buttons", None),
                ("meter", Some(14.0)),
            ]
        );
        // And they're saved as they're shown, with the views not changed
        // left out.
        let saved = parts.views();
        assert_eq!(
            saved.0.keys().collect::<Vec<_>>(),
            [MIXER_STRIP, TRACK_HEADER]
        );
        let mut again = Parts::default();
        again.set_views(&saved);
        assert_eq!(again.lineups, parts.lineups);
        let text = toml::to_string(&saved).unwrap();
        assert_eq!(toml::from_str::<Views>(&text).unwrap(), saved);
        assert!(!parts.take_changed(), "setting views isn't a change");
    }

    #[test]
    fn every_view_has_its_parts() {
        let registry = Registry::new();
        for (view, _) in VIEWS {
            let parts: Vec<_> = registry.fitting(view).map(|part| part.id).collect();
            let expected: &[&str] = match view {
                TRANSPORT_BAR => &[
                    "go_to_start",
                    "play_stop",
                    "record",
                    "capture",
                    "loop_punch",
                    "metronome",
                    "tempo",
                    "signature",
                    "position",
                    "space",
                    "cpu",
                ],
                TRACK_HEADER => &["colour", "name", "automation", "buttons", "meter"],
                _ => &[
                    "name",
                    "input",
                    "monitor",
                    "output",
                    "sends",
                    "pan",
                    "fader",
                    "readouts",
                    "buttons",
                    "clip_type",
                ],
            };
            assert_eq!(parts, expected, "{view}");
            for part in registry.fitting(view) {
                let drawn = match part.draw {
                    Draw::Bar { .. } => TRANSPORT_BAR,
                    Draw::Header(_) => TRACK_HEADER,
                    Draw::Strip { .. } => MIXER_STRIP,
                };
                assert_eq!(drawn, view, "{}", part.id);
            }
        }
    }
}

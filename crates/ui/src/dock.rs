//! The window's panels, docked in a tree of splits and tabs
//! ([egui_tiles](https://docs.rs/egui_tiles)) that the user rearranges by
//! dragging tabs about.
//!
//! Every panel has a tab, in a group of one or more, and the groups are
//! split across and down. A hidden panel keeps its place in the tree, to
//! come back to. Splits size their parts in points, not in shares of the
//! window: the part with the middle panel, the arrangement, takes what's
//! left, and the others keep their sizes as the window grows and shrinks.
//! A panel's size is its own: it keeps it as it moves about, a group takes
//! its front tab's, and dragging a group's edge sizes all its tabs.

use std::collections::BTreeMap;

use egui::{Id, Rect, Response, Role, Ui, WidgetInfo, WidgetText, pos2, vec2};
use egui_tiles::{
    Behavior, Container, EditAction, Linear, LinearDir, SimplificationOptions, Tabs, Tile, TileId,
    Tiles, Tree, UiResponse,
};

use crate::panels::{Cx, Panel, Panels, Place, Registry};
use crate::theme::Theme;
use crate::workspace::{Layout, Split, WindowLayout};

/// A panel in the tree, by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pane(pub &'static str);

/// Panels' widths and heights in points, by id, as the user dragged them:
/// across first.
type Sizes = BTreeMap<&'static str, [Option<f32>; 2]>;

/// How the tree tidies itself after a drag: every panel keeps a tab, and
/// splits and tabs left with one part or none go.
const TIDY: SimplificationOptions = SimplificationOptions {
    prune_empty_tabs: true,
    prune_empty_containers: true,
    prune_single_child_tabs: true,
    prune_single_child_containers: true,
    all_panes_must_have_tabs: true,
    join_nested_linear_containers: true,
    // A group dropped on another's tabs joins them.
    flatten_tabs_in_tabs: true,
};

/// The window's panels, laid out.
#[derive(Debug)]
pub(crate) struct Dock {
    tree: Tree<Pane>,
    sizes: Sizes,
    /// Whether the user changed the layout since it was last saved.
    changed: bool,
}

impl Dock {
    /// The default layout: each panel in its place, those the registry
    /// doesn't show hidden.
    pub fn new(registry: &Registry) -> Self {
        let mut dock = Self::empty();
        dock.complete(&[], registry, |panel| !panel.shown);
        dock.tree.simplify(&TIDY);
        dock
    }

    /// The layout `window` describes. Panels it doesn't have go in their
    /// places, hidden, and without any it's the default layout.
    pub fn from_layout(window: &WindowLayout, registry: &Registry, theme: &Theme) -> Self {
        let mut dock = Self::empty();
        let mut placed = Vec::new();
        let Some(root) = dock.build(&window.layout, None, registry, theme, &mut placed) else {
            // None of its panels are the registry's.
            return Self::new(registry);
        };
        dock.tree.root = Some(root);
        dock.complete(&placed, registry, |_| true);
        for id in &window.hidden {
            if let Some(panel) = registry.get(id).filter(|panel| panel.hides)
                && let Some(tile) = dock.find(panel.id)
            {
                dock.tree.set_visible(tile, false);
            }
        }
        dock.tree.simplify(&TIDY);
        dock
    }

    fn empty() -> Self {
        Self {
            tree: Tree::empty(Id::unique("dock")),
            sizes: Sizes::new(),
            changed: false,
        }
    }

    /// The layout, for the workspace file.
    pub fn layout(&self, registry: &Registry, theme: &Theme) -> WindowLayout {
        let tiles = &self.tree.tiles;
        let hidden = (registry.iter())
            .filter(|panel| {
                self.find(panel.id)
                    .is_some_and(|tile| !tiles.is_visible(tile))
            })
            .map(|panel| panel.id.to_owned())
            .collect();
        let measure = Measure::new(registry, theme, &self.sizes);
        let layout = (self.tree.root)
            .and_then(|root| self.part(root, None, &measure))
            .unwrap_or_else(|| Layout::new(Split::Across(Vec::new())));
        WindowLayout { hidden, layout }
    }

    /// The part of the layout that's the tile `id`, in a split across or
    /// down, as `dir` says.
    fn part(&self, id: TileId, dir: Option<LinearDir>, measure: &Measure<'_>) -> Option<Layout> {
        let tiles = &self.tree.tiles;
        let mut layout = match tiles.get(id)? {
            Tile::Pane(pane) => Layout::panel(pane.0),
            Tile::Container(Container::Linear(linear)) => {
                let mut parts: Vec<_> = (linear.children.iter())
                    .filter_map(|&child| self.part(child, Some(linear.dir), measure))
                    .collect();
                match parts.len() {
                    0 => return None,
                    1 => Layout {
                        size: None,
                        ..parts.remove(0)
                    },
                    _ => Layout::new(match linear.dir {
                        LinearDir::Horizontal => Split::Across(parts),
                        LinearDir::Vertical => Split::Down(parts),
                    }),
                }
            }
            Tile::Container(container) => {
                let mut panes = Vec::new();
                collect_panes(tiles, id, &mut panes);
                // The tab in front, or with them all hidden, the one that was.
                let front = match container {
                    Container::Tabs(tabs) => (tabs.next_active(tiles).or(tabs.active))
                        .and_then(|tile| tiles.get_pane(&tile)),
                    _ => None,
                };
                match panes.as_slice() {
                    [] => return None,
                    [only] => Layout::panel(only.0),
                    _ => Layout {
                        front: front.map(|pane| pane.0.to_owned()),
                        ..Layout::new(Split::Tabs(panes.iter().map(|p| p.0.to_owned()).collect()))
                    },
                }
            }
        };
        // Its size, if the user gave any of its panels one.
        layout.size = dir
            .filter(|&dir| self.sized(id, dir))
            .and_then(|dir| measure.natural(tiles, id, dir))
            .map(|size| size.round().max(1.0) as u32);
        Some(layout)
    }

    /// Whether the user gave a panel in the tile `id` a size across or down.
    fn sized(&self, id: TileId, dir: LinearDir) -> bool {
        let mut panes = Vec::new();
        collect_panes(&self.tree.tiles, id, &mut panes);
        (panes.iter())
            .any(|pane| (self.sizes.get(pane.0)).is_some_and(|size| size[axis(dir)].is_some()))
    }

    /// Adds the tiles for `layout` to the tree, returning the top one's id,
    /// or `None` if it has none of the registry's panels. `placed` gathers
    /// the panels added: one of each.
    fn build(
        &mut self,
        layout: &Layout,
        dir: Option<LinearDir>,
        registry: &Registry,
        theme: &Theme,
        placed: &mut Vec<&'static str>,
    ) -> Option<TileId> {
        let tile = match &layout.split {
            Split::Panel(id) => self.add_pane(id, registry, placed)?,
            Split::Tabs(ids) => {
                let children: Vec<_> = (ids.iter())
                    .filter_map(|id| self.add_pane(id, registry, placed))
                    .collect();
                if children.is_empty() {
                    return None;
                }
                let tiles = &self.tree.tiles;
                let front = (children.iter().copied()).find(|&child| {
                    tiles.get_pane(&child).map(|pane| pane.0) == layout.front.as_deref()
                });
                let mut tabs = Tabs::new(children);
                if let Some(front) = front {
                    tabs.set_active(front);
                }
                self.tree.tiles.insert_container(tabs)
            }
            Split::Across(parts) | Split::Down(parts) => {
                let split = match layout.split {
                    Split::Across(_) => LinearDir::Horizontal,
                    _ => LinearDir::Vertical,
                };
                let mut children: Vec<_> = (parts.iter())
                    .filter_map(|part| self.build(part, Some(split), registry, theme, placed))
                    .collect();
                match children.len() {
                    0 => return None,
                    1 => children.remove(0),
                    _ => self
                        .tree
                        .tiles
                        .insert_container(Linear::new(split, children)),
                }
            }
        };
        if let (Some(dir), Some(size)) = (dir, layout.size) {
            let defaults = Sizes::new();
            let measure = Measure::new(registry, theme, &defaults);
            set_size(
                &self.tree.tiles,
                &mut self.sizes,
                &measure,
                tile,
                dir,
                size as f32,
            );
        }
        Some(tile)
    }

    /// Adds the registry's panel with id `id` as a tile of its own, unless
    /// it's in `placed` already.
    fn add_pane(
        &mut self,
        id: &str,
        registry: &Registry,
        placed: &mut Vec<&'static str>,
    ) -> Option<TileId> {
        let panel = registry
            .get(id)
            .filter(|panel| !placed.contains(&panel.id))?;
        placed.push(panel.id);
        Some(self.tree.tiles.insert_pane(Pane(panel.id)))
    }

    /// Puts the registry's panels that aren't in `placed` in their places,
    /// the middle one first, and hides those `hidden` says to.
    fn complete(
        &mut self,
        placed: &[&'static str],
        registry: &Registry,
        hidden: impl Fn(&Panel) -> bool,
    ) {
        let middle = registry.middle();
        let others = registry.iter().filter(|panel| panel.place != Place::Middle);
        let missing: Vec<_> = (middle.into_iter().chain(others))
            .filter(|panel| !placed.contains(&panel.id))
            .collect();
        for panel in missing {
            let tile = self.place(panel, registry);
            if panel.hides && hidden(panel) {
                self.tree.set_visible(tile, false);
            }
        }
    }

    /// Adds `panel` where it goes, returning its tile.
    fn place(&mut self, panel: &Panel, registry: &Registry) -> TileId {
        let pane = self.tree.tiles.insert_pane(Pane(panel.id));
        let Some(root) = self.tree.root else {
            self.tree.root = Some(pane);
            return pane;
        };
        match panel.place {
            // Over everything else, in a layout that was without it.
            Place::Middle => self.wrap(root, pane, LinearDir::Vertical, true),
            Place::Left => self.add_to_root(pane, LinearDir::Horizontal, true),
            Place::Below => self.add_below_middle(pane, registry),
            Place::With(other) => match self.find(other) {
                Some(partner) => self.add_beside(partner, pane),
                None => self.add_below_middle(pane, registry),
            },
        }
        pane
    }

    /// Adds the tile `pane` at the start or the end of the root, if it's
    /// split `dir`, or else splits the root with it.
    fn add_to_root(&mut self, pane: TileId, dir: LinearDir, first: bool) {
        let Some(root) = self.tree.root else {
            self.tree.root = Some(pane);
            return;
        };
        match self.tree.tiles.get_mut(root) {
            Some(Tile::Container(Container::Linear(linear))) if linear.dir == dir => match first {
                true => linear.children.insert(0, pane),
                false => linear.children.push(pane),
            },
            _ => self.wrap(root, pane, dir, first),
        }
    }

    /// Adds the tile `pane` under the middle panel and the panels under it.
    fn add_below_middle(&mut self, pane: TileId, registry: &Registry) {
        let tiles = &self.tree.tiles;
        let Some(middle) = registry.middle().and_then(|middle| self.find(middle.id)) else {
            self.add_to_root(pane, LinearDir::Vertical, false);
            return;
        };
        // The middle panel's tabs, if it has some yet.
        let part = (tiles.parent_of(middle))
            .filter(|&parent| matches!(tiles.get_container(parent), Some(Container::Tabs(_))))
            .unwrap_or(middle);
        let column = tiles.parent_of(part);
        match column.and_then(|column| self.tree.tiles.get_mut(column)) {
            Some(Tile::Container(Container::Linear(linear)))
                if linear.dir == LinearDir::Vertical =>
            {
                linear.children.push(pane);
            }
            _ => self.wrap(part, pane, LinearDir::Vertical, false),
        }
    }

    /// Adds the tile `pane` in a tab beside the tile `partner`, behind it.
    fn add_beside(&mut self, partner: TileId, pane: TileId) {
        let tiles = &mut self.tree.tiles;
        let parent = tiles.parent_of(partner);
        if let Some(Tile::Container(Container::Tabs(tabs))) = parent.and_then(|p| tiles.get_mut(p))
        {
            tabs.children.push(pane);
            return;
        }
        let mut tabs = Tabs::new(vec![partner, pane]);
        tabs.set_active(partner);
        let tabs = tiles.insert_container(tabs);
        self.put_in_place_of(parent, partner, tabs);
    }

    /// Splits `dir` the tile `target` and the tile `pane`, first or last.
    fn wrap(&mut self, target: TileId, pane: TileId, dir: LinearDir, first: bool) {
        let parent = self.tree.tiles.parent_of(target);
        let children = match first {
            true => vec![pane, target],
            false => vec![target, pane],
        };
        let split = (self.tree.tiles).insert_container(Linear::new(dir, children));
        self.put_in_place_of(parent, target, split);
    }

    /// Puts the tile `new` where `old` was, in `parent` or as the root.
    fn put_in_place_of(&mut self, parent: Option<TileId>, old: TileId, new: TileId) {
        let Some(parent) = parent else {
            self.tree.root = Some(new);
            return;
        };
        match self.tree.tiles.get_mut(parent) {
            Some(Tile::Container(Container::Linear(linear))) => {
                if let Some(child) = linear.children.iter_mut().find(|child| **child == old) {
                    *child = new;
                    linear.shares.replace_with(old, new);
                }
            }
            Some(Tile::Container(Container::Tabs(tabs))) => {
                if let Some(child) = tabs.children.iter_mut().find(|child| **child == old) {
                    *child = new;
                }
                if tabs.active == Some(old) {
                    tabs.active = Some(new);
                }
            }
            _ => {}
        }
    }

    /// The tile of the panel with id `id`.
    fn find(&self, id: &str) -> Option<TileId> {
        (self.tree.tiles.iter())
            .find(|(_, tile)| matches!(tile, Tile::Pane(pane) if pane.0 == id))
            .map(|(&tile, _)| tile)
    }

    /// Whether the panel with id `id` is in sight: not hidden, nor behind
    /// another tab.
    pub fn shown(&self, id: &str) -> bool {
        let tiles = &self.tree.tiles;
        let Some(mut tile) = self.find(id) else {
            return false;
        };
        while tiles.is_visible(tile) {
            let Some(parent) = tiles.parent_of(tile) else {
                return self.tree.root == Some(tile);
            };
            if let Some(Container::Tabs(tabs)) = tiles.get_container(parent)
                && tabs.next_active(tiles) != Some(tile)
            {
                return false;
            }
            tile = parent;
        }
        false
    }

    /// Shows the panel with id `id` where it was, in front of the tabs
    /// beside it.
    pub fn show_panel(&mut self, id: &str) {
        let Some(tile) = self.find(id) else {
            return;
        };
        self.tree.set_visible(tile, true);
        self.tree.make_active(|other, _| other == tile);
        self.changed = true;
    }

    /// Hides the panel with id `id`, keeping its place.
    pub fn hide_panel(&mut self, id: &str) {
        if let Some(tile) = self.find(id) {
            self.tree.set_visible(tile, false);
            self.changed = true;
        }
    }

    /// Puts the panel with id `id` in front of the tabs beside it, if it
    /// isn't hidden, as the devices and the piano roll follow what's
    /// selected.
    pub fn bring_forward(&mut self, id: &str) {
        if let Some(tile) = self.find(id).filter(|&tile| self.tree.is_visible(tile)) {
            self.tree.make_active(|other, _| other == tile);
        }
    }

    /// Whether the user changed the layout since this was last asked.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Lays the panels out in `ui`'s space and draws them with `draw`.
    pub fn show(&mut self, ui: &mut Ui, draw: &mut Draw<'_, '_>) {
        let (registry, theme) = (draw.registry, draw.cx.theme);
        // Tidied before the sizes are worked out, so the tree shows as
        // they're worked out for.
        self.tree.simplify(&TIDY);
        let rect = ui.available_rect_before_wrap();
        let mut fitted = Vec::new();
        if let Some(root) = self.tree.root {
            let measure = Measure::new(registry, theme, &self.sizes);
            let middle = registry.middle().map(|panel| Pane(panel.id));
            let tiles = &mut self.tree.tiles;
            fit(tiles, root, rect, &measure, middle, &mut fitted);
        }
        self.tree.ui(draw, ui);
        for tile in draw.closed.drain(..) {
            self.tree.set_visible(tile, false);
            self.changed = true;
        }
        // The parts whose edges the user dragged keep their new sizes.
        let defaults = Sizes::new();
        let measure = Measure::new(registry, theme, &defaults);
        for fitted in fitted.iter().filter(|fitted| !fitted.middle) {
            let tiles = &self.tree.tiles;
            let Some(Container::Linear(linear)) = tiles.get_container(fitted.split) else {
                continue;
            };
            let size = linear.shares[fitted.part];
            if linear.children.contains(&fitted.part) && size != fitted.size {
                let dir = linear.dir;
                set_size(tiles, &mut self.sizes, &measure, fitted.part, dir, size);
                self.changed = true;
            }
        }
        self.changed |= std::mem::take(&mut draw.edited);
    }
}

/// Which of a panel's [`Sizes`] goes across or down.
fn axis(dir: LinearDir) -> usize {
    match dir {
        LinearDir::Horizontal => 0,
        LinearDir::Vertical => 1,
    }
}

/// The panels in the tile `id`, left to right and top to bottom.
fn collect_panes(tiles: &Tiles<Pane>, id: TileId, panes: &mut Vec<Pane>) {
    match tiles.get(id) {
        Some(Tile::Pane(pane)) => panes.push(*pane),
        Some(Tile::Container(container)) => {
            for &child in container.children() {
                collect_panes(tiles, child, panes);
            }
        }
        None => {}
    }
}

/// Whether the tile `id` is or has the panel `pane`.
fn holds(tiles: &Tiles<Pane>, id: TileId, pane: Pane) -> bool {
    match tiles.get(id) {
        Some(Tile::Pane(other)) => *other == pane,
        Some(Tile::Container(container)) => container.children().any(|&c| holds(tiles, c, pane)),
        None => false,
    }
}

/// What the sizes of the tree's parts are worked out from.
struct Measure<'a> {
    registry: &'a Registry,
    theme: &'a Theme,
    sizes: &'a Sizes,
}

impl<'a> Measure<'a> {
    fn new(registry: &'a Registry, theme: &'a Theme, sizes: &'a Sizes) -> Self {
        Self {
            registry,
            theme,
            sizes,
        }
    }

    fn gap(&self) -> f32 {
        self.theme.dock.gap
    }

    /// The height of the tab bar over a group, going `dir`: none across.
    fn bar(&self, dir: LinearDir) -> f32 {
        match dir {
            LinearDir::Horizontal => 0.0,
            LinearDir::Vertical => self.theme.dock.tab_bar,
        }
    }

    /// The size the panel `pane` takes across or down, as the user dragged
    /// it, or else as its kind says, under its tab bar. `None` for the
    /// middle panel, and for panels without a size that way.
    fn panel(&self, pane: Pane, dir: LinearDir) -> Option<f32> {
        let dragged = self.sizes.get(pane.0).and_then(|sizes| sizes[axis(dir)]);
        dragged.or_else(|| {
            let panel = self.registry.get(pane.0)?;
            let across = panel.across(self.registry);
            let fits = panel.place != Place::Middle && across == (dir == LinearDir::Horizontal);
            fits.then(|| (panel.size)(self.theme))
        })
    }

    /// The size the tile `id` takes across or down, from its panels': a
    /// group's front tab's and its tab bar, the sum of parts in a row, the
    /// largest of parts side by side. `None` when it has none that way.
    fn natural(&self, tiles: &Tiles<Pane>, id: TileId, dir: LinearDir) -> Option<f32> {
        match tiles.get(id)? {
            // A panel always has a tab bar over it in the layout.
            Tile::Pane(pane) => self.panel(*pane, dir).map(|size| size + self.bar(dir)),
            Tile::Container(Container::Tabs(tabs)) => {
                let front = tiles.get_pane(&tabs.next_active(tiles)?)?;
                self.panel(*front, dir).map(|size| size + self.bar(dir))
            }
            Tile::Container(Container::Linear(linear)) => {
                let visible: Vec<_> = (linear.children.iter().copied())
                    .filter(|&child| tiles.is_visible_in_layout(child))
                    .collect();
                if linear.dir == dir {
                    let sizes: Option<Vec<_>> = (visible.iter())
                        .map(|&child| self.natural(tiles, child, dir))
                        .collect();
                    let gaps = self.gap() * visible.len().saturating_sub(1) as f32;
                    sizes.map(|sizes| sizes.iter().sum::<f32>() + gaps)
                } else {
                    (visible.iter())
                        .filter_map(|&child| self.natural(tiles, child, dir))
                        .reduce(f32::max)
                }
            }
            Tile::Container(Container::Grid(_)) => None,
        }
    }
}

/// Gives the panels in the tile `id` the size `size` across or down, as
/// the tile: each tab of a group, under the tab bar, each of parts side by
/// side, and parts in a row their shares of it.
fn set_size(
    tiles: &Tiles<Pane>,
    sizes: &mut Sizes,
    measure: &Measure<'_>,
    id: TileId,
    dir: LinearDir,
    size: f32,
) {
    let mut record = |pane: &Pane, size: f32| {
        sizes.entry(pane.0).or_default()[axis(dir)] = Some(size.max(0.0));
    };
    match tiles.get(id) {
        Some(Tile::Pane(pane)) => record(pane, size - measure.bar(dir)),
        Some(Tile::Container(Container::Tabs(tabs))) => {
            for child in &tabs.children {
                if let Some(pane) = tiles.get_pane(child) {
                    record(pane, size - measure.bar(dir));
                }
            }
        }
        Some(Tile::Container(Container::Linear(linear))) if linear.dir == dir => {
            let gaps = measure.gap() * linear.children.len().saturating_sub(1) as f32;
            let room = (size - gaps).max(0.0);
            let natural: Vec<_> = (linear.children.iter())
                .map(|&child| measure.natural(tiles, child, dir).unwrap_or(1.0).max(1.0))
                .collect();
            let total: f32 = natural.iter().sum();
            for (&child, natural) in linear.children.iter().zip(natural) {
                set_size(tiles, sizes, measure, child, dir, room * natural / total);
            }
        }
        Some(Tile::Container(Container::Linear(linear))) => {
            for &child in &linear.children {
                set_size(tiles, sizes, measure, child, dir, size);
            }
        }
        Some(Tile::Container(Container::Grid(_))) | None => {}
    }
}

/// A part's size in a split, as [`fit`] set it.
struct Fitted {
    split: TileId,
    part: TileId,
    size: f32,
    /// Whether it has the middle panel, which takes what's left.
    middle: bool,
}

/// Sets the shares of the parts of each split in sight under the tile
/// `id`, laid out in `rect`, to their sizes in points, as the egui_tiles
/// layout that follows divides the space by them.
fn fit(
    tiles: &mut Tiles<Pane>,
    id: TileId,
    rect: Rect,
    measure: &Measure<'_>,
    middle: Option<Pane>,
    fitted: &mut Vec<Fitted>,
) {
    if !tiles.is_visible_in_layout(id) {
        return;
    }
    let (dir, parts) = match tiles.get(id) {
        Some(Tile::Container(Container::Tabs(tabs))) => {
            if let Some(front) = tabs.next_active(tiles) {
                let mut under = rect;
                under.min.y += measure.bar(LinearDir::Vertical);
                fit(tiles, front, under, measure, middle, fitted);
            }
            return;
        }
        Some(Tile::Container(Container::Linear(linear))) => {
            let parts: Vec<_> = (linear.children.iter().copied())
                .filter(|&child| tiles.is_visible_in_layout(child))
                .collect();
            (linear.dir, parts)
        }
        _ => return,
    };
    let natural: Vec<_> = (parts.iter())
        .map(|&part| measure.natural(tiles, part, dir))
        .collect();
    let flex = middle.and_then(|pane| parts.iter().position(|&part| holds(tiles, part, pane)));
    let length = match dir {
        LinearDir::Horizontal => rect.width(),
        LinearDir::Vertical => rect.height(),
    };
    let room = (length - measure.gap() * parts.len().saturating_sub(1) as f32).max(0.0);
    let sizes = sizes(&natural, flex, room, measure.theme.dock.min_panel);
    if let Some(Tile::Container(Container::Linear(linear))) = tiles.get_mut(id) {
        for (i, (&part, &size)) in parts.iter().zip(&sizes).enumerate() {
            linear.shares.set_share(part, size);
            let middle = flex == Some(i);
            fitted.push(Fitted {
                split: id,
                part,
                size,
                middle,
            });
        }
    }
    let mut at = match dir {
        LinearDir::Horizontal => rect.min.x,
        LinearDir::Vertical => rect.min.y,
    };
    for (&part, &size) in parts.iter().zip(&sizes) {
        let part_rect = match dir {
            LinearDir::Horizontal => {
                Rect::from_min_size(pos2(at, rect.min.y), vec2(size, rect.height()))
            }
            LinearDir::Vertical => {
                Rect::from_min_size(pos2(rect.min.x, at), vec2(rect.width(), size))
            }
        };
        fit(tiles, part, part_rect, measure, middle, fitted);
        at += size + measure.gap();
    }
}

/// The sizes of a split's parts, in points, sharing `room`: each its
/// `natural` size, but for the part `flex`, which takes what's left with
/// those without one. Without such a part, they keep their proportions. If
/// the room's too small, the others shrink to leave `flex` at least `min`.
fn sizes(natural: &[Option<f32>], flex: Option<usize>, room: f32, min: f32) -> Vec<f32> {
    let Some(flex) = flex else {
        let known: Vec<f32> = natural.iter().flatten().copied().collect();
        let average = match known.len() {
            0 => 1.0,
            n => known.iter().sum::<f32>() / n as f32,
        };
        let sizes: Vec<f32> = (natural.iter())
            .map(|size| size.unwrap_or(average).max(1.0))
            .collect();
        let total: f32 = sizes.iter().sum();
        return sizes.iter().map(|size| room * size / total).collect();
    };
    let others = |sizes: &[f32]| -> f32 {
        let all: f32 = sizes.iter().sum();
        all - sizes[flex]
    };
    let unset = (natural.iter().enumerate())
        .filter(|&(i, size)| i != flex && size.is_none())
        .count();
    let fixed: f32 = (natural.iter().enumerate())
        .filter(|&(i, _)| i != flex)
        .filter_map(|(_, size)| *size)
        .sum();
    let each = ((room - fixed) / (unset + 1) as f32).max(0.0);
    let mut sizes: Vec<f32> = natural.iter().map(|size| size.unwrap_or(each)).collect();
    sizes[flex] = 0.0;
    let taken = others(&sizes);
    if room - taken < min && taken > 0.0 {
        let scale = ((room - min).max(0.0) / taken).min(1.0);
        for (i, size) in sizes.iter_mut().enumerate() {
            if i != flex {
                *size *= scale;
            }
        }
    }
    sizes[flex] = (room - others(&sizes)).max(0.0);
    sizes
}

/// Draws the panels in the tree, and hears what the user does to it.
pub(crate) struct Draw<'a, 'v> {
    pub registry: &'a Registry,
    pub panels: &'a mut Panels,
    pub cx: Cx<'a, 'v>,
    /// Whether the user moved a panel or picked a tab.
    pub edited: bool,
    /// The tabs the user closed, to hide once the tree's drawn: hiding
    /// them while it's drawn leaves the splits without their parts.
    pub closed: Vec<TileId>,
}

impl Behavior<Pane> for Draw<'_, '_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile: TileId, pane: &mut Pane) -> UiResponse {
        if let Some(panel) = self.registry.get(pane.0) {
            (panel.show)(self.panels, ui, &mut self.cx);
        }
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> WidgetText {
        let panel = self.registry.get(pane.0);
        panel.map_or(pane.0, |panel| panel.title).into()
    }

    fn is_tab_closable(&self, tiles: &Tiles<Pane>, tile: TileId) -> bool {
        let panel = tiles
            .get_pane(&tile)
            .and_then(|pane| self.registry.get(pane.0));
        panel.is_some_and(|panel| panel.hides)
    }

    /// Closing a tab hides its panel, which keeps its place.
    fn on_tab_close(&mut self, _tiles: &mut Tiles<Pane>, tile: TileId) -> bool {
        self.closed.push(tile);
        false
    }

    /// Tabs are tabs to screen readers, called "Mixer panel" and so on, so
    /// they're told from what's in the panels.
    fn on_tab_button(
        &mut self,
        tiles: &mut Tiles<Pane>,
        tile: TileId,
        response: Response,
    ) -> Response {
        if let Some(pane) = tiles.get_pane(&tile) {
            let title = self
                .registry
                .get(pane.0)
                .map_or(pane.0, |panel| panel.title);
            let front = match tiles.parent_of(tile).and_then(|p| tiles.get_container(p)) {
                Some(Container::Tabs(tabs)) => tabs.active == Some(tile),
                _ => true,
            };
            let enabled = response.enabled();
            response.widget_info(|| {
                WidgetInfo::selected(Role::Tab, enabled, front, format!("{title} panel"))
            });
        }
        response
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        self.cx.theme.dock.tab_bar
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        self.cx.theme.dock.gap
    }

    fn min_size(&self) -> f32 {
        self.cx.theme.dock.min_panel
    }

    fn simplification_options(&self) -> SimplificationOptions {
        TIDY
    }

    /// Keys are the keymap's.
    fn cycle_tabs_with_keyboard(&self) -> bool {
        false
    }

    fn on_edit(&mut self, edit: EditAction) {
        if edit != EditAction::TileDragged {
            self.edited = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::{ARRANGEMENT, BROWSER, DEVICES, MIXER, PIANO_ROLL};

    fn layout(dock: &Dock) -> WindowLayout {
        dock.layout(&Registry::new(), &Theme::default())
    }

    fn sized(size: u32, layout: Layout) -> Layout {
        Layout {
            size: Some(size),
            ..layout
        }
    }

    /// Today's layout: the browser on the left, and the arrangement over
    /// the mixer and the devices, with the piano roll in a tab beside them.
    fn default_layout() -> WindowLayout {
        let tabs = Split::Tabs(vec![DEVICES.into(), PIANO_ROLL.into()]);
        WindowLayout {
            hidden: vec![BROWSER.into(), PIANO_ROLL.into()],
            layout: Layout::new(Split::Across(vec![
                Layout::panel(BROWSER),
                Layout::new(Split::Down(vec![
                    Layout::panel(ARRANGEMENT),
                    Layout::panel(MIXER),
                    Layout {
                        front: Some(DEVICES.into()),
                        ..Layout::new(tabs)
                    },
                ])),
            ])),
        }
    }

    #[test]
    fn the_default_layout_is_the_window_as_it_was() {
        let registry = Registry::new();
        let dock = Dock::new(&registry);
        assert_eq!(layout(&dock), default_layout());
        let shown: Vec<_> = (registry.iter())
            .filter(|panel| dock.shown(panel.id))
            .map(|panel| panel.id)
            .collect();
        assert_eq!(shown, [ARRANGEMENT, MIXER, DEVICES]);
    }

    #[test]
    fn hidden_panels_come_back_where_they_were() {
        let mut dock = Dock::new(&Registry::new());
        dock.show_panel(PIANO_ROLL);
        assert!(dock.shown(PIANO_ROLL));
        // Behind the piano roll.
        assert!(!dock.shown(DEVICES));
        dock.hide_panel(PIANO_ROLL);
        assert!(dock.shown(DEVICES));
        dock.show_panel(BROWSER);
        dock.hide_panel(MIXER);
        let mut expected = default_layout();
        expected.hidden = vec![MIXER.into(), PIANO_ROLL.into()];
        assert_eq!(layout(&dock), expected);
        assert!(dock.take_changed());
        assert!(!dock.take_changed());
        // Bringing a hidden panel forward doesn't show it.
        dock.bring_forward(PIANO_ROLL);
        assert!(!dock.shown(PIANO_ROLL));
        assert!(!dock.take_changed());
    }

    #[test]
    fn layouts_are_read_as_they_were_written() {
        let registry = Registry::new();
        let theme = Theme::default();
        let window = WindowLayout {
            hidden: vec![DEVICES.into()],
            layout: Layout::new(Split::Down(vec![
                Layout::new(Split::Across(vec![
                    Layout::panel(ARRANGEMENT),
                    sized(340, Layout::panel(BROWSER)),
                ])),
                sized(
                    250,
                    Layout {
                        front: Some(MIXER.into()),
                        ..Layout::new(Split::Tabs(vec![
                            DEVICES.into(),
                            MIXER.into(),
                            PIANO_ROLL.into(),
                        ]))
                    },
                ),
            ])),
        };
        let dock = Dock::from_layout(&window, &registry, &theme);
        assert_eq!(dock.layout(&registry, &theme), window);
        assert!(dock.shown(MIXER) && dock.shown(BROWSER));
        assert!(!dock.shown(DEVICES) && !dock.shown(PIANO_ROLL));
    }

    #[test]
    fn layouts_lose_what_they_cant_have_and_gain_what_they_lack() {
        let registry = Registry::new();
        let theme = Theme::default();
        // A panel this version doesn't know, one twice, and no devices
        // or piano roll.
        let window = WindowLayout {
            hidden: vec![ARRANGEMENT.into(), "later".into()],
            layout: Layout::new(Split::Across(vec![
                Layout::panel("later"),
                Layout::panel(ARRANGEMENT),
                Layout::new(Split::Tabs(vec![MIXER.into(), ARRANGEMENT.into()])),
            ])),
        };
        let dock = Dock::from_layout(&window, &registry, &theme);
        // The browser goes on the left and the devices under the
        // arrangement, hidden, and the piano roll beside them. The
        // arrangement can't be hidden.
        let expected = WindowLayout {
            hidden: vec![BROWSER.into(), DEVICES.into(), PIANO_ROLL.into()],
            layout: Layout::new(Split::Across(vec![
                Layout::panel(BROWSER),
                Layout::new(Split::Down(vec![
                    Layout::panel(ARRANGEMENT),
                    Layout {
                        front: Some(DEVICES.into()),
                        ..Layout::new(Split::Tabs(vec![DEVICES.into(), PIANO_ROLL.into()]))
                    },
                ])),
                Layout::panel(MIXER),
            ])),
        };
        assert_eq!(dock.layout(&registry, &theme), expected);
        assert!(dock.shown(ARRANGEMENT));
        // With none of its panels, a layout is the default one.
        let nothing = WindowLayout {
            hidden: Vec::new(),
            layout: Layout::panel("later"),
        };
        let dock = Dock::from_layout(&nothing, &registry, &theme);
        assert_eq!(dock.layout(&registry, &theme), default_layout());
    }

    #[test]
    fn the_middle_takes_what_the_other_parts_leave() {
        let natural = [Some(260.0), None, Some(100.0)];
        assert_eq!(
            sizes(&natural, Some(1), 1000.0, 50.0),
            [260.0, 640.0, 100.0]
        );
        // Parts without a size share it with the middle.
        let natural = [Some(300.0), None, None];
        assert_eq!(sizes(&natural, Some(1), 900.0, 50.0), [300.0, 300.0, 300.0]);
        // Too little room: the others shrink, the middle keeps its least.
        let natural = [None, Some(300.0), Some(200.0)];
        assert_eq!(sizes(&natural, Some(0), 300.0, 50.0), [50.0, 150.0, 100.0]);
        // Without a middle, parts keep their proportions.
        let natural = [Some(100.0), Some(300.0), None];
        assert_eq!(sizes(&natural, None, 600.0, 50.0), [100.0, 300.0, 200.0]);
    }
}

//! What's in a window, as a screen reader reads it, for agents: egui's
//! AccessKit tree, which it builds as it draws a window while AccessKit is
//! on. Asking turns it on until the window has been drawn, unless a screen
//! reader has it on already.

use std::time::{Duration, Instant};

use eframe::egui::accesskit::{self, Action, Node, NodeId, Toggled, TreeUpdate};
use eframe::egui::{self, FullOutput, Rect, ViewportId};
use musicable_mcp::Reply;
use serde_json::{Map, Value as Json, json};

/// What to read of a window, and where to answer.
pub(crate) struct Ask {
    /// The window's name for the answer: main, or its number from 1.
    pub window: Json,
    /// The panel to read only, by its id, and where it is in the window,
    /// in points.
    pub panel: Option<(String, Rect)>,
    /// Words to look for.
    pub filter: Option<String>,
    /// The most to read.
    pub limit: usize,
}

/// How long to wait for a window to be drawn.
const LONGEST_WAIT: Duration = Duration::from_secs(5);

/// The egui plugin that hands agents the windows' AccessKit trees.
#[derive(Default)]
pub(crate) struct UiTrees {
    waiting: Vec<Waiting>,
    /// The window whose pass is ending, whose output comes next.
    ending: Option<ViewportId>,
    /// Whether AccessKit is on because an agent asked.
    ours: bool,
}

/// An agent's ask, waiting for its window to be drawn.
struct Waiting {
    viewport: ViewportId,
    /// Whether the window's pass began after AccessKit was on, so egui
    /// builds the whole tree in it.
    began: bool,
    /// When the agent asked, to give up on a window that isn't drawn.
    asked: Instant,
    ask: Ask,
    reply: Reply,
}

/// Reads what `ask` says of the window `viewport` as it's next drawn, and
/// answers `reply` with it.
pub(crate) fn read(ctx: &egui::Context, viewport: ViewportId, ask: Ask, reply: Reply) {
    // A screen reader may have AccessKit on already, which stays on.
    let on = ctx.has_accesskit_node(egui::accesskit_root_id());
    // Where egui draws the windows of their own inside the main window,
    // as in tests, it reads them in the main window's tree.
    let viewport = if ctx.embed_viewports() {
        ViewportId::ROOT
    } else {
        viewport
    };
    let trees = ctx.plugin_or_default::<UiTrees>();
    let mut trees = trees.lock();
    if !on {
        ctx.enable_accesskit();
        trees.ours = true;
    }
    trees.waiting.push(Waiting {
        viewport,
        began: false,
        asked: Instant::now(),
        ask,
        reply,
    });
    ctx.request_repaint_of(viewport);
}

impl egui::Plugin for UiTrees {
    fn debug_name(&self) -> &'static str {
        "Musicable's UI trees for agents"
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        let viewport = ui.ctx().viewport_id();
        for waiting in &mut self.waiting {
            waiting.began |= waiting.viewport == viewport;
        }
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        self.ending = Some(ui.ctx().viewport_id());
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut FullOutput) {
        let Some(viewport) = self.ending.take() else {
            return;
        };
        self.waiting.retain(|waiting| !waiting.reply.is_closed());
        let (late, waiting) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|waiting| waiting.asked.elapsed() > LONGEST_WAIT);
        self.waiting = waiting;
        for Waiting { reply, .. } in late {
            reply.failed("The window wasn't drawn: is it shown?");
        }
        if let Some(update) = &output.platform_output.accesskit_update {
            let (ready, rest) = std::mem::take(&mut self.waiting)
                .into_iter()
                .partition(|waiting| waiting.viewport == viewport && waiting.began);
            self.waiting = rest;
            for Waiting { ask, reply, .. } in ready {
                reply.done(answer(update, &ask));
            }
        }
        if self.waiting.is_empty() && self.ours {
            ctx.disable_accesskit();
            self.ours = false;
        }
    }
}

/// What `ask` reads of the tree `update`: the window, the panel, the nodes
/// it keeps, nested, and how many more it left out.
fn answer(update: &TreeUpdate, ask: &Ask) -> Json {
    let nodes: std::collections::HashMap<NodeId, &Node> =
        update.nodes.iter().map(|(id, node)| (*id, node)).collect();
    let root = update.tree.as_ref().map(|tree| tree.root);
    // egui's bounds are in points, and its root scales them to pixels.
    let scale = (root.and_then(|root| nodes.get(&root)))
        .and_then(|root| root.transform())
        .map_or(1.0, |transform| transform.as_coeffs()[0]);
    let mut reading = Reading {
        nodes: &nodes,
        ask,
        focus: update.focus,
        scale,
        kept: 0,
        more: 0,
    };
    let children = match root.and_then(|root| nodes.get(&root)) {
        Some(root) => reading.children(root, false),
        None => Vec::new(),
    };
    json!({
        "window": ask.window,
        "panel": ask.panel.as_ref().map(|(id, _)| id),
        "nodes": children,
        "more": reading.more,
    })
}

/// A walk down the tree, keeping what the ask wants.
struct Reading<'a> {
    nodes: &'a std::collections::HashMap<NodeId, &'a Node>,
    ask: &'a Ask,
    focus: NodeId,
    scale: f64,
    /// How many nodes it's kept, and left out past the limit.
    kept: usize,
    more: usize,
}

/// What can be done to a node, by the names agents read.
const ACTIONS: [(Action, &str); 7] = [
    (Action::Click, "click"),
    (Action::Increment, "increment"),
    (Action::Decrement, "decrement"),
    (Action::SetValue, "set_value"),
    (Action::Expand, "expand"),
    (Action::Collapse, "collapse"),
    (Action::ShowContextMenu, "context_menu"),
];

impl Reading<'_> {
    /// What it keeps of `node`'s children, and theirs where it leaves one
    /// out. `found` says the filter found `node` or what it's in.
    fn children(&mut self, node: &Node, found: bool) -> Vec<Json> {
        let mut kept = Vec::new();
        for &id in node.children() {
            let Some(child) = self.nodes.get(&id).copied() else {
                continue;
            };
            // A text run is its label's value again.
            if !child.is_hidden() && child.role() != accesskit::Role::TextRun {
                kept.extend(self.node(id, child, found));
            }
        }
        kept
    }

    /// `node` as JSON, if it's kept, or else the nodes inside it that are.
    /// It's kept if the ask wants it and there's room, which the outer
    /// ones get first, or to hold the ones inside it that are kept, unless
    /// it's a plain container.
    fn node(&mut self, id: NodeId, node: &Node, found: bool) -> Vec<Json> {
        let found = found || self.found(node);
        let shown = shows_something(node);
        let wanted = shown && found && self.in_panel(node) != Some(false);
        let room = self.kept < self.ask.limit;
        if wanted {
            if room {
                self.kept += 1;
            } else {
                self.more += 1;
            }
        }
        let children = self.children(node, found);
        let holds = shown && !children.is_empty();
        if !(wanted && room || holds) {
            return children;
        }
        if !(wanted && room) {
            self.kept += 1;
        }
        let mut entry = self.describe(id, node);
        if !children.is_empty() {
            entry.insert("children".into(), children.into());
        }
        vec![Json::Object(entry)]
    }

    /// Whether the filter finds `node`: there's no filter, or its label,
    /// value or description has the words.
    fn found(&self, node: &Node) -> bool {
        let Some(filter) = &self.ask.filter else {
            return true;
        };
        let filter = filter.to_lowercase();
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|text| text.to_lowercase().contains(&filter))
    }

    /// Whether `node` is in the panel the ask reads, if it reads one and
    /// `node` says where it is.
    fn in_panel(&self, node: &Node) -> Option<bool> {
        let (_, panel) = self.ask.panel.as_ref()?;
        let bounds = node.bounds()?;
        let rect = Rect::from_min_max(
            egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
            egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
        );
        Some(panel.intersects(rect))
    }

    /// What agents read of `node`, without its children.
    fn describe(&self, id: NodeId, node: &Node) -> Map<String, Json> {
        let mut entry = Map::new();
        entry.insert("id".into(), format!("{:x}", id.0).into());
        entry.insert("role".into(), role(node.role()).into());
        for (name, text) in [
            ("label", node.label()),
            ("value", node.value()),
            ("description", node.description()),
            ("placeholder", node.placeholder()),
        ] {
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                entry.insert(name.into(), text.into());
            }
        }
        if let Some(number) = node.numeric_value() {
            entry.insert("number".into(), number.into());
            if let (Some(min), Some(max)) = (node.min_numeric_value(), node.max_numeric_value()) {
                entry.insert("range".into(), json!([min, max]));
            }
        }
        if let Some(toggled) = node.toggled() {
            let toggled = match toggled {
                Toggled::True => json!(true),
                Toggled::False => json!(false),
                Toggled::Mixed => json!("mixed"),
            };
            entry.insert("toggled".into(), toggled);
        }
        if let Some(selected) = node.is_selected() {
            entry.insert("selected".into(), selected.into());
        }
        if let Some(expanded) = node.is_expanded() {
            entry.insert("expanded".into(), expanded.into());
        }
        if node.is_disabled() {
            entry.insert("disabled".into(), true.into());
        }
        if id == self.focus {
            entry.insert("focused".into(), true.into());
        }
        let actions: Vec<&str> = (ACTIONS.iter())
            .filter(|(action, _)| node.supports_action(*action))
            .map(|(_, name)| *name)
            .collect();
        if !actions.is_empty() {
            entry.insert("actions".into(), actions.into());
        }
        if let Some(bounds) = node.bounds() {
            let pixel = |value: f64| (value * self.scale).round() as i64;
            entry.insert("at".into(), json!([pixel(bounds.x0), pixel(bounds.y0)]));
            let size = [bounds.x1 - bounds.x0, bounds.y1 - bounds.y0];
            entry.insert("size".into(), json!(size.map(pixel)));
        }
        entry
    }
}

/// Whether `node` shows the user something of its own, rather than only
/// holding other nodes, or marking where the mouse does something without
/// saying what.
fn shows_something(node: &Node) -> bool {
    use accesskit::Role;
    let plain = matches!(
        node.role(),
        Role::GenericContainer | Role::Unknown | Role::Window
    );
    !plain || node.label().is_some_and(|label| !label.is_empty()) || node.value().is_some()
}

/// `role`'s name for agents, such as `check_box` for `CheckBox`.
fn role(role: accesskit::Role) -> String {
    let mut name = String::new();
    for (index, letter) in format!("{role:?}").chars().enumerate() {
        if letter.is_uppercase() {
            if index > 0 {
                name.push('_');
            }
            name.extend(letter.to_lowercase());
        } else {
            name.push(letter);
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use accesskit::{Affine, Role, TreeId, TreeInfo};

    use super::*;

    fn tree() -> TreeUpdate {
        let mut root = Node::new(Role::Window);
        root.set_transform(Affine::scale(2.0));
        root.set_children(vec![NodeId(2), NodeId(5)]);
        let mut panel = Node::new(Role::GenericContainer);
        panel.set_children(vec![NodeId(3), NodeId(4)]);
        let mut play = Node::new(Role::Button);
        play.set_label("Play");
        play.add_action(Action::Click);
        play.set_bounds(accesskit::Rect::new(10.0, 10.0, 30.0, 20.0));
        let mut volume = Node::new(Role::Slider);
        volume.set_label("Volume");
        volume.set_numeric_value(-6.0);
        volume.set_min_numeric_value(-70.0);
        volume.set_max_numeric_value(6.0);
        volume.add_action(Action::Increment);
        volume.set_bounds(accesskit::Rect::new(10.0, 200.0, 30.0, 220.0));
        let mut group = Node::new(Role::Group);
        group.set_label("Mixer");
        group.set_children(vec![NodeId(6)]);
        let mut solo = Node::new(Role::CheckBox);
        solo.set_label("Solo");
        solo.set_toggled(Toggled::True);
        solo.set_disabled();
        TreeUpdate {
            nodes: vec![
                (NodeId(1), root),
                (NodeId(2), panel),
                (NodeId(3), play),
                (NodeId(4), volume),
                (NodeId(5), group),
                (NodeId(6), solo),
            ],
            tree: Some(TreeInfo::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(3),
        }
    }

    fn ask() -> Ask {
        Ask {
            window: json!("main"),
            panel: None,
            filter: None,
            limit: 300,
        }
    }

    #[test]
    fn the_tree_leaves_out_plain_containers() {
        let answer = answer(&tree(), &ask());
        assert_eq!(
            answer,
            json!({
                "window": "main",
                "panel": null,
                "more": 0,
                "nodes": [
                    {
                        "id": "3", "role": "button", "label": "Play", "focused": true,
                        "actions": ["click"], "at": [20, 20], "size": [40, 20],
                    },
                    {
                        "id": "4", "role": "slider", "label": "Volume", "number": -6.0,
                        "range": [-70.0, 6.0], "actions": ["increment"], "at": [20, 400],
                        "size": [40, 40],
                    },
                    {
                        "id": "5", "role": "group", "label": "Mixer",
                        "children": [
                            {
                                "id": "6", "role": "check_box", "label": "Solo",
                                "toggled": true, "disabled": true,
                            },
                        ],
                    },
                ],
            })
        );
    }

    #[test]
    fn filters_keep_what_they_find_and_what_it_is_in() {
        let ask = Ask {
            filter: Some("solo".into()),
            ..ask()
        };
        let answer = answer(&tree(), &ask);
        let nodes = &answer["nodes"];
        assert_eq!(nodes.as_array().unwrap().len(), 1, "{answer}");
        assert_eq!(nodes[0]["label"], "Mixer");
        assert_eq!(nodes[0]["children"][0]["label"], "Solo");
        // What it finds comes with all that's inside it.
        let ask = Ask {
            filter: Some("MIXER".into()),
            ..ask
        };
        let answer = super::answer(&tree(), &ask);
        assert_eq!(answer["nodes"][0]["children"][0]["label"], "Solo");
    }

    #[test]
    fn a_panel_keeps_what_is_in_it() {
        let ask = Ask {
            panel: Some((
                "transport".into(),
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0)),
            )),
            ..ask()
        };
        let answer = answer(&tree(), &ask);
        assert_eq!(answer["panel"], "transport");
        let labels: Vec<&str> = (answer["nodes"].as_array().unwrap().iter())
            .map(|node| node["label"].as_str().unwrap())
            .collect();
        // The mixer group says nothing of where it is, so it stays.
        assert_eq!(labels, ["Play", "Mixer"], "{answer}");
    }

    #[test]
    fn the_limit_keeps_the_first_and_counts_the_rest() {
        let ask = Ask { limit: 2, ..ask() };
        let answer = answer(&tree(), &ask);
        let labels: Vec<&str> = (answer["nodes"].as_array().unwrap().iter())
            .map(|node| node["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Play", "Volume"], "{answer}");
        assert_eq!(answer["more"], 2, "{answer}");
    }

    #[test]
    fn roles_are_snake_case() {
        assert_eq!(role(Role::CheckBox), "check_box");
        assert_eq!(role(Role::Button), "button");
        assert_eq!(role(Role::SpinButton), "spin_button");
    }
}

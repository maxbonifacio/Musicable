//! The modulator strip, at the left of the devices panel's devices, and
//! what the controls that modulators move show of them.
//!
//! The strip has a card for each of the channel's modulators, in its
//! order: an LFO's or an Envelope's controls, as a built-in device's are,
//! or a Macro's knobs and buttons. Each output has a handle, filled as far
//! as the output is. Dragging a handle onto a control routes the output to
//! it, and clicking it lists what the output moves, each with its
//! routing's settings, and every parameter it can move besides.
//!
//! A control that modulators move shows it ([`Shown`]): a line along it,
//! in the modulators' colour, from as low to as high as its routings can
//! take it, and a dot where they have it. Its menu lists its routings,
//! each with its settings, and the outputs that can move it besides
//! ([`routing_items`]).

use egui::{
    Align, Button, CursorIcon, DragValue, Frame, Id, Label, LayerId, Layout, Order, Popup, Pos2,
    Rangef, Rect, Response, RichText, Role, ScrollArea, Sense, Stroke, StrokeKind, TextStyle,
    TextWrapMode, Ui, Vec2, WidgetInfo, WidgetText, pos2, vec2,
};
use musicable_commands::{Args, CommandId, edit, modulation};
use musicable_model::modulation::{ControlKind, Curve, Modulator, ModulatorKind, Output, Routing};
use musicable_model::param::ParamAddress;
use musicable_model::{Chain, DeviceKind, PluginFormat, Project};

use crate::arrangement::{chain_params, param_name, param_spec};
use crate::browser::Dragged;
use crate::devices;
use crate::mixer::Levels;
use crate::mouse::MouseMap;
use crate::native;
use crate::shell::{Action, View};
use crate::theme::Theme;

/// How far a new routing moves its parameter while its output is at 1: a
/// quarter of the way along its range.
pub(crate) const NEW_AMOUNT: f64 = 0.25;

/// How thick the line along a modulated control is.
const BAND: f32 = 3.0;
/// The radius of the dot where modulators have a control.
const DOT: f32 = 3.0;
/// The space between a Macro's knob or button and its handle.
const HANDLE_GAP: f32 = 3.0;

/// What the menus call each curve, and what they say of it, in the order
/// of [`Curve::KEYS`].
const CURVES: [(&str, &str); 8] = [
    ("Linear", "Straight."),
    ("Exponential", "Slowly at first, then fast."),
    ("Logarithmic", "Fast at first, then slowly."),
    ("S-Curve", "Slowly at both ends, and fast in the middle."),
    ("Stepped", "In even steps."),
    (
        "Absolute",
        "The output's size without its sign, so a bipolar output only moves it one way.",
    ),
    ("Positive", "Only where the output is above 0."),
    ("Negative", "Only where the output is below 0."),
];

/// Where a control's value runs, from its lowest to its highest: a line
/// along a bar's foot, or up beside a fader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Travel {
    pub low: Pos2,
    pub high: Pos2,
}

impl Travel {
    /// The point `unit` of the way from its lowest to its highest, kept on
    /// it.
    fn at(self, unit: f32) -> Pos2 {
        self.low + (self.high - self.low) * unit.clamp(0.0, 1.0)
    }
}

/// What a control shows of the modulators that move its parameter, and
/// how it takes a modulator's output dragged onto it.
pub(crate) struct Shown<'a> {
    pub project: &'a Project,
    pub levels: &'a Levels,
    pub param: ParamAddress,
}

impl Shown<'_> {
    /// Paints how far the routings that play can move the control
    /// `response` from `at`, where its value is along `travel`, from 0 to
    /// 1, and a dot where they have it. While a modulator's output that
    /// can move it is dragged, outlines it, and returns the routing that
    /// letting the output go on it adds.
    pub(crate) fn on(
        &self,
        ui: &Ui,
        response: &Response,
        travel: Travel,
        at: f32,
        theme: &Theme,
    ) -> Option<Action> {
        if !ui.is_rect_visible(response.rect) {
            return None;
        }
        self.paint(ui, travel, at, theme);
        self.drop(ui, response, theme)
    }

    fn paint(&self, ui: &Ui, travel: Travel, at: f32, theme: &Theme) {
        let Some((low, high)) = reach(self.project, self.param) else {
            return;
        };
        let painter = ui.painter();
        let colour = theme.modulation;
        let (from, to) = (travel.at(at + low), travel.at(at + high));
        if from != to {
            let band = Stroke::new(BAND, colour.gamma_multiply(0.6));
            painter.line_segment([from, to], band);
        }
        if let Some(&moved) = self.levels.moved.get(&self.param) {
            let rim = Stroke::new(1.0, ui.visuals().extreme_bg_color);
            painter.circle(travel.at(at + moved), DOT, colour, rim);
        }
    }

    /// While an output that can move the parameter is dragged, outlines
    /// the control `response`, faintly until the pointer is over it, and
    /// returns the routing that letting the output go there adds.
    fn drop(&self, ui: &Ui, response: &Response, theme: &Theme) -> Option<Action> {
        let ctx = ui.ctx();
        let dragged = egui::DragAndDrop::payload::<Dragged>(ctx)?;
        let Dragged::Output { output, owner, .. } = *dragged else {
            return None;
        };
        if !takes(self.project, owner, output, self.param) {
            return None;
        }
        let over = response.contains_pointer();
        let outline = match over {
            true => Stroke::new(2.0, theme.modulation),
            false => Stroke::new(1.0, theme.modulation.gamma_multiply(0.5)),
        };
        let rect = response.rect.intersect(ui.clip_rect());
        let layer = LayerId::new(Order::Foreground, Id::unique("drop onto a control"));
        let painter = ctx.layer_painter(layer);
        painter.rect_stroke(rect, 2.0, outline, StrokeKind::Outside);
        if !over || !ctx.input(|input| input.pointer.any_released()) {
            return None;
        }
        egui::DragAndDrop::clear_payload(ctx);
        Some(add_routing(output, self.param, NEW_AMOUNT))
    }
}

/// Whether `output`, from `owner`'s strip, can go onto the control of the
/// parameter at `param`: modulators can move it, `owner`'s reach it, and
/// the output doesn't move it yet.
fn takes(project: &Project, owner: Chain, output: Output, param: ParamAddress) -> bool {
    let Some(modulation) = project.modulation(owner) else {
        return false;
    };
    modulation.has_output(output)
        && project.can_modulate(param)
        && project.in_reach(owner, param)
        && modulation.routing_between(output, param).is_none()
}

/// How far below and above its value the routings that play can move the
/// parameter at `param`, all together, as shares of its range: `None`
/// while none plays.
pub(crate) fn reach(project: &Project, param: ParamAddress) -> Option<(f32, f32)> {
    let bipolar = |output: Output| {
        (project.find_modulator(output.modulator)).is_some_and(|(_, _, m)| m.bipolar(output.n))
    };
    let mut reach = None;
    for (owner, routing) in project.routings_to(param) {
        if routing.bypassed || !project.routing_plays(owner, routing) {
            continue;
        }
        let scaled = routing.scale.is_some_and(bipolar);
        let (low, high) = span(routing.curve, bipolar(routing.from), scaled, routing.amount);
        let (below, above) = reach.unwrap_or((0.0, 0.0));
        reach = Some((below + low, above + high));
    }
    reach.map(|(below, above)| (below as f32, above as f32))
}

/// The lowest and the highest that a routing with `curve` and `amount`
/// moves its parameter, from an output that's `bipolar` or not, `scaled` by
/// a bipolar output or not.
fn span(curve: Curve, bipolar: bool, scaled: bool, amount: f64) -> (f64, f64) {
    let (low, high) = match (curve, bipolar) {
        (Curve::Negative, false) => (0.0, 0.0),
        (_, false) => (0.0, 1.0),
        (Curve::Absolute | Curve::Positive, true) => (0.0, 1.0),
        (Curve::Negative, true) => (-1.0, 0.0),
        (_, true) => (-1.0, 1.0),
    };
    // A bipolar scale turns it either way.
    let (low, high) = match scaled {
        true => {
            let most = f64::max(-low, high);
            (-most, most)
        }
        false => (low, high),
    };
    let (low, high) = (low * amount, high * amount);
    (low.min(high), low.max(high))
}

/// Shows the cards of `chain`'s modulators, left to right, and asks for the
/// changes the user makes to them.
pub(crate) fn strip(
    ui: &mut Ui,
    chain: Chain,
    view: &View<'_>,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    let Some(modulation) = view.project.modulation(chain) else {
        return;
    };
    let count = modulation.modulators.len();
    for (index, modulator) in modulation.modulators.iter().enumerate() {
        let card = Card {
            modulator,
            chain,
            index,
            count,
            view,
            theme,
            mouse,
        };
        ui.push_id(modulator.id, |ui| card.show(ui, actions));
    }
}

/// The Add Modulator menu, which adds a Macro, an LFO or an Envelope to
/// the end of `chain`'s strip.
pub(crate) fn add_menu(ui: &mut Ui, chain: Chain, actions: &mut Vec<Action>) {
    let menu = ui.menu_button("Add Modulator", |ui| {
        for kind in ModulatorKind::ALL {
            if ui.button(kind.name()).on_hover_text(about(kind)).clicked() {
                let args = Args::new().with("kind", kind.key());
                let args = devices::onto(args, Some(chain));
                actions.push(Action::RunWith(modulation::ADD, args));
                ui.close();
            }
        }
    });
    menu.response
        .on_hover_text("Adds a modulator, to the left of the devices, to move their controls");
}

/// What the Add Modulator menu says of a kind of modulator.
fn about(kind: ModulatorKind) -> &'static str {
    match kind {
        ModulatorKind::Macro => "Knobs and buttons played by hand.",
        ModulatorKind::Lfo => "A shape that repeats: a sine, a triangle, a saw or a square.",
        ModulatorKind::Envelope => {
            "A shape that runs once each time it's started, by a note, its gate or the \
             transport."
        }
    }
}

/// A modulator's card in the strip.
struct Card<'a> {
    modulator: &'a Modulator,
    /// The channel whose strip it's in.
    chain: Chain,
    /// Its place in the strip, and how many cards the strip has.
    index: usize,
    count: usize,
    view: &'a View<'a>,
    theme: &'a Theme,
    mouse: &'a MouseMap,
}

impl Card<'_> {
    /// Shows it: its title row, and its controls, with a line along its
    /// top in the modulators' colour that tells it from a device.
    fn show(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let theme = self.theme;
        let sizes = &theme.devices;
        let width = self.width(ui);
        let shown = Frame::new()
            .fill(theme.device)
            .stroke(Stroke::new(1.0, theme.divider))
            .corner_radius(theme.corner)
            .inner_margin(sizes.margin)
            .show(ui, |ui| {
                ui.set_width(width);
                // Top to bottom, whatever the row of cards around it does.
                ui.vertical(|ui| {
                    self.title_row(ui, actions);
                    match self.modulator.kind {
                        ModulatorKind::Macro => self.macro_controls(ui, actions),
                        _ => native::modulator_controls(
                            ui,
                            self.modulator,
                            self.view,
                            theme,
                            self.mouse,
                            actions,
                        ),
                    }
                });
            });
        let rect = shown.response.rect;
        let top = Stroke::new(2.0, theme.modulation);
        let along = rect.x_range().shrink(theme.corner);
        ui.painter().hline(along, rect.top() + 1.0, top);
    }

    /// How wide its controls are, side by side.
    fn width(&self, ui: &Ui) -> f32 {
        let sizes = &self.theme.devices;
        match self.modulator.kind {
            ModulatorKind::Macro => {
                let controls = self.modulator.controls.len();
                let columns = controls.div_ceil(native::ROWS).max(1) as f32;
                let column = sizes.control + HANDLE_GAP + handle_side(ui);
                columns * column + (columns - 1.0) * sizes.gap
            }
            kind => native::table_width(kind.params(), self.theme),
        }
    }

    /// Its title row: an LFO's or an Envelope's handle, its name, which has
    /// its menu, a Macro's + for another knob or button, and the button
    /// that deletes it.
    fn title_row(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let modulator = self.modulator;
        let name = modulator.name.as_str();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let delete = ui.small_button("×").on_hover_text("Delete");
                let label = format!("Delete {name}");
                delete.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                if delete.clicked() {
                    actions.push(self.run(modulation::DELETE, Args::new()));
                }
                if modulator.kind == ModulatorKind::Macro {
                    let add = ui
                        .small_button("+")
                        .on_hover_text("Adds a knob or a button");
                    let label = format!("Add a Control to {name}");
                    add.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                    Popup::menu(&add).show(|ui| self.control_items(ui, actions));
                }
                // On the left, after an LFO's or an Envelope's handle, in
                // the room the buttons leave.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if modulator.kind != ModulatorKind::Macro {
                        self.handle(ui, 0, actions);
                    }
                    let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
                    let (rect, title) = ui.allocate_exact_size(size, Sense::click());
                    if ui.is_rect_visible(rect) {
                        let theme = self.theme;
                        let text = RichText::new(name).strong().color(theme.header_text);
                        let wrap = Some(TextWrapMode::Truncate);
                        let text = WidgetText::from(text).into_galley(
                            ui,
                            wrap,
                            rect.width(),
                            TextStyle::Body,
                        );
                        let at = pos2(rect.left(), rect.center().y - text.size().y / 2.0);
                        ui.painter().galley(at, text, theme.header_text);
                    }
                    let label = format!("Modulator {name}");
                    title.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
                    let about = format!(
                        "{}: right-click to rename it, move it along the strip or delete it.",
                        modulator.kind.name()
                    );
                    let title = title.on_hover_text(about);
                    title.context_menu(|ui| self.menu(ui, actions));
                });
            });
        });
    }

    /// Its menu: renaming it, moving it along the strip, a Macro's new
    /// knobs and buttons, and deleting it.
    fn menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if ui.button("Rename...").clicked() {
            actions.push(Action::RunWith(edit::RENAME, self.on(Args::new())));
            ui.close();
        }
        let right = (self.index + 1 < self.count).then_some(self.index + 1);
        for (title, to) in [
            ("Move Left", self.index.checked_sub(1)),
            ("Move Right", right),
        ] {
            if ui.add_enabled(to.is_some(), Button::new(title)).clicked()
                && let Some(to) = to
            {
                let args = Args::new().with("index", to as i64);
                actions.push(self.run(modulation::MOVE, args));
                ui.close();
            }
        }
        if self.modulator.kind == ModulatorKind::Macro {
            ui.separator();
            self.control_items(ui, actions);
        }
        ui.separator();
        if ui.button("Delete").clicked() {
            actions.push(self.run(modulation::DELETE, Args::new()));
            ui.close();
        }
    }

    /// The items that add a knob or a button to a Macro.
    fn control_items(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let kinds = [
            (ControlKind::Knob, "Add Knob", "It turns from 0 to 1."),
            (
                ControlKind::Toggle,
                "Add Toggle Button",
                "Each press turns it on or off.",
            ),
            (
                ControlKind::Momentary,
                "Add Momentary Button",
                "It's on while it's held.",
            ),
        ];
        for (kind, title, about) in kinds {
            if ui.button(title).on_hover_text(about).clicked() {
                let args = Args::new().with("kind", kind.key());
                actions.push(self.run(modulation::ADD_CONTROL, args));
                ui.close();
            }
        }
    }

    /// A Macro's knobs and buttons, each with its handle beside it, in
    /// columns of up to [`native::ROWS`].
    fn macro_controls(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let modulator = self.modulator;
        let theme = self.theme;
        if modulator.controls.is_empty() {
            let text = RichText::new("No knobs or buttons: + adds one.")
                .size(theme.small_text)
                .weak();
            ui.add(Label::new(text).wrap().selectable(false));
            return;
        }
        let width = theme.devices.control;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = theme.devices.gap;
            for column in modulator.controls.chunks(native::ROWS) {
                ui.vertical(|ui| {
                    for control in column {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = HANDLE_GAP;
                            let (view, mouse) = (self.view, self.mouse);
                            native::macro_control(
                                ui, modulator, control, width, view, theme, mouse, actions,
                            );
                            self.handle(ui, control.id, actions);
                        });
                    }
                });
            }
        });
    }

    /// The handle of output `n`, filled as far as the output is: from the
    /// bottom, or from the middle for a bipolar output. Dragging it onto a
    /// control routes the output there, and clicking it lists what the
    /// output moves.
    fn handle(&self, ui: &mut Ui, n: u32, actions: &mut Vec<Action>) {
        let modulator = self.modulator;
        let output = Output::new(modulator.id, n);
        let name = output_name(self.view.project, output).unwrap_or_default();
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(handle_side(ui)), Sense::hover());
        let id = ui.make_persistent_id(("output", n));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        // Until the engine has said, a Macro's output is its control's.
        let value = (self.view.levels.outputs.get(&output).copied())
            .or_else(|| {
                modulator
                    .value(n)
                    .filter(|_| modulator.kind == ModulatorKind::Macro)
                    .map(|v| v as f32)
            })
            .unwrap_or(0.0);
        let label = format!("{name} Output");
        response.widget_info(|| WidgetInfo {
            value: Some(f64::from(value)),
            ..WidgetInfo::labeled(Role::Button, true, &label)
        });
        if ui.is_rect_visible(rect) {
            let bipolar = modulator.bipolar(n);
            paint_handle(ui, &response, rect, value, bipolar, self.theme);
        }
        let ctx = ui.ctx();
        if response.drag_started() {
            let dragged = Dragged::Output {
                output,
                owner: self.chain,
                name: name.clone(),
            };
            egui::DragAndDrop::set_payload(ctx, dragged);
        }
        if response.dragged() {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        }
        let response = response.on_hover_text(format!(
            "{name}: drag it onto a control to modulate the control, or click to see what it \
             moves."
        ));
        Popup::menu(&response).show(|ui| self.output_menu(ui, output, actions));
    }

    /// The menu of `output`: what it moves, each opening to its routing's
    /// settings, then Route To, which lists every parameter it can move.
    fn output_menu(&self, ui: &mut Ui, output: Output, actions: &mut Vec<Action>) {
        let project = self.view.project;
        let routings = project
            .modulation(self.chain)
            .map_or(&[][..], |m| &m.routings[..]);
        let routings: Vec<&Routing> = routings.iter().filter(|r| r.from == output).collect();
        if routings.is_empty() {
            ui.label(RichText::new("It moves nothing yet.").weak());
        }
        for routing in routings {
            let name = self.target_name(routing.to);
            let title = routing_title(project, self.chain, routing, name);
            ui.menu_button(title, |ui| {
                routing_settings(ui, project, self.chain, routing, actions);
            });
        }
        ui.separator();
        ui.menu_button("Route To", |ui| self.route_to(ui, output, actions));
    }

    /// What the menu of an output on the strip calls the parameter at
    /// `param`: after its channel's name if it's on another channel's.
    fn target_name(&self, param: ParamAddress) -> String {
        let project = self.view.project;
        let name = param_name(self.view, param);
        match project.param_chain(param) {
            Some(chain) if chain != self.chain => {
                format!("{name} of {}", channel_name(project, chain))
            }
            _ => name,
        }
    }

    /// The Route To menu of `output`: the parameters it can move, by
    /// channel if it reaches more than its own, which comes first. Those
    /// it moves are ticked.
    fn route_to(&self, ui: &mut Ui, output: Output, actions: &mut Vec<Action>) {
        let project = self.view.project;
        let others = (project.tracks.iter())
            .map(|track| Chain::Track(track.id))
            .filter(|&chain| chain != self.chain)
            .filter(|&chain| project.in_reach(self.chain, ParamAddress::Volume(chain)));
        let chains: Vec<Chain> = [self.chain].into_iter().chain(others).collect();
        if let [chain] = chains[..] {
            self.chain_targets(ui, chain, output, actions);
            return;
        }
        for chain in chains {
            ui.menu_button(channel_name(project, chain), |ui| {
                self.chain_targets(ui, chain, output, actions);
            });
        }
    }

    /// The parameters of `chain` that `output` can move: its own, then
    /// each device's and each modulator's, each opening to theirs.
    fn chain_targets(&self, ui: &mut Ui, chain: Chain, output: Output, actions: &mut Vec<Action>) {
        let project = self.view.project;
        let modulation = project.modulation(self.chain);
        let mut item = |ui: &mut Ui, param: ParamAddress, name: &str| {
            let on = modulation.is_some_and(|m| m.routing_between(output, param).is_some());
            if ui.add(Button::selectable(on, name)).clicked() {
                if !on {
                    let amount = new_amount(self.view, param);
                    actions.push(add_routing(output, param, amount));
                }
                ui.close();
            }
        };
        let (own, groups) = chain_params(self.view, chain);
        let moves = |(param, _): &&(ParamAddress, String)| {
            project.can_modulate(*param) && !vst3(project, *param)
        };
        for (param, name) in own.iter().filter(moves) {
            item(ui, *param, name);
        }
        for (group, params) in &groups {
            let params: Vec<_> = params.iter().filter(moves).collect();
            if params.is_empty() {
                continue;
            }
            ui.menu_button(group.as_str(), |ui| {
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (param, name) in params {
                        item(ui, *param, name);
                    }
                });
            });
        }
    }

    /// `args`, for the modulator.
    fn on(&self, args: Args) -> Args {
        args.with("modulator", self.modulator.id)
    }

    /// The command `command`, for the modulator, with `args`.
    fn run(&self, command: CommandId, args: Args) -> Action {
        Action::RunWith(command, self.on(args))
    }
}

/// Whether the parameter at `param` is a VST3 plugin's, which modulators
/// don't move yet, though a routing can go to it, so Route To leaves it out.
fn vst3(project: &Project, param: ParamAddress) -> bool {
    let device = param.device().and_then(|id| project.find_device(id));
    device.is_some_and(|(_, _, device)| {
        matches!(&device.kind, DeviceKind::Plugin(plugin) if plugin.format == PluginFormat::Vst3)
    })
}

/// How big an output's handle is: a square as tall as a control.
fn handle_side(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// Paints a handle at `rect`, which `response` is for, filled as far as
/// `value` is: from the bottom, or from the middle while it's `bipolar`.
fn paint_handle(
    ui: &Ui,
    response: &Response,
    rect: Rect,
    value: f32,
    bipolar: bool,
    theme: &Theme,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let y = |unit: f32| rect.bottom() - rect.height() * unit.clamp(0.0, 1.0);
    let (from, to) = match bipolar {
        true => (y(0.5), y((value + 1.0) / 2.0)),
        false => (y(0.0), y(value)),
    };
    let filled = Rect::from_x_y_ranges(rect.x_range(), Rangef::new(from.min(to), from.max(to)));
    painter.rect_filled(filled, 0.0, theme.modulation);
    let outline = match response.hovered() || response.dragged() {
        true => Stroke::new(2.0, theme.modulation),
        false => Stroke::new(1.0, theme.modulation),
    };
    painter.rect_stroke(rect, 2.0, outline, StrokeKind::Inside);
}

/// The items a control's menu has for the modulators of the parameter at
/// `param`, after a separator: each routing that moves it, opening to its
/// settings, then Add Modulation, which lists the outputs that can move it
/// besides. None for a parameter modulators can't move.
pub(crate) fn routing_items(
    ui: &mut Ui,
    project: &Project,
    param: ParamAddress,
    actions: &mut Vec<Action>,
) {
    if !project.can_modulate(param) {
        return;
    }
    ui.separator();
    let chain = project.param_chain(param);
    // After its channel's name, a modulator on another channel's strip.
    let name = |owner: Chain, output: Output| {
        let name = output_name(project, output).unwrap_or_default();
        match Some(owner) == chain {
            true => name,
            false => format!("{name} on {}", channel_name(project, owner)),
        }
    };
    for (owner, routing) in project.routings_to(param) {
        let title = routing_title(project, owner, routing, name(owner, routing.from));
        ui.push_id(routing.id.0, |ui| {
            ui.menu_button(title, |ui| {
                routing_settings(ui, project, owner, routing, actions);
            });
        });
    }
    let free: Vec<(Output, String)> = (project.modulations())
        .filter(|&(owner, _)| project.in_reach(owner, param))
        .flat_map(|(owner, modulation)| {
            (modulation.modulators.iter()).flat_map(move |modulator| {
                let outputs = modulator.outputs().into_iter();
                outputs.map(move |n| (owner, Output::new(modulator.id, n)))
            })
        })
        .filter(|&(owner, output)| takes(project, owner, output, param))
        .map(|(owner, output)| (output, name(owner, output)))
        .collect();
    if free.is_empty() {
        let add = ui.add_enabled(false, Button::new("Add Modulation"));
        add.on_disabled_hover_text(
            "No modulator can move it besides. Add Modulator, over the devices, adds one to \
             its track, or to a group it's in, or the master.",
        );
        return;
    }
    ui.menu_button("Add Modulation", |ui| {
        for (output, name) in free {
            if ui.button(name).clicked() {
                actions.push(add_routing(output, param, NEW_AMOUNT));
                ui.close();
            }
        }
    });
}

/// What a menu calls `routing`, on `owner`'s strip, which it lists as
/// `name`: with its amount, or why it moves nothing.
fn routing_title(project: &Project, owner: Chain, routing: &Routing, name: String) -> String {
    if routing.bypassed {
        return format!("{name} (bypassed)");
    }
    if !project.routing_plays(owner, routing) {
        return format!("{name} (out of reach)");
    }
    format!("{name} {}", amount_text(project, routing))
}

/// `routing`'s amount, as its menu shows it: "+25%", or in a plugin's own
/// units, "+0.25".
fn amount_text(project: &Project, routing: &Routing) -> String {
    match project.param_spec(routing.to) {
        Some(_) => format!("{}%", signed(routing.amount * 100.0, 1)),
        None => signed(routing.amount, 3),
    }
}

/// `value` with its sign, to as many as `decimals` places, without the
/// zeros at the end: "+25", "-12.5".
fn signed(value: f64, decimals: usize) -> String {
    let text = format!("{value:+.decimals$}");
    let text = match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.'),
        false => &text,
    };
    match text {
        "+0" | "-0" => "0".to_owned(),
        text => text.to_owned(),
    }
}

/// The settings of `routing`, which is on `owner`'s strip, for its menu:
/// its amount and its lag, which a drag changes as one step to undo, its
/// curve, its scale and its bypass, then Delete Routing.
fn routing_settings(
    ui: &mut Ui,
    project: &Project,
    owner: Chain,
    routing: &Routing,
    actions: &mut Vec<Action>,
) {
    let id = routing.id;
    let run = |command: CommandId, args: Args| Action::RunWith(command, args.with("routing", id));
    if !project.routing_plays(owner, routing) {
        let text = "Out of reach: it waits until its modulator reaches the parameter again.";
        ui.label(RichText::new(text).weak());
    }
    // A share of the range, in percent, or a plugin's own units.
    let known = project.param_spec(routing.to).is_some();
    ui.horizontal(|ui| {
        let label = ui.label("Amount");
        framed(ui);
        let (response, amount) = match known {
            true => {
                let mut percent = routing.amount * 100.0;
                let field = DragValue::new(&mut percent)
                    .speed(0.5)
                    .range(-100.0..=100.0)
                    .custom_formatter(|percent, _| signed(percent, 1))
                    .suffix("%")
                    .update_while_editing(false);
                (ui.add(field), percent / 100.0)
            }
            false => {
                let mut amount = routing.amount;
                let field = DragValue::new(&mut amount)
                    .speed(0.01)
                    .custom_formatter(|amount, _| signed(amount, 3))
                    .update_while_editing(false);
                (ui.add(field), amount)
            }
        };
        let response = response.labelled_by(label.id);
        let change = (amount != routing.amount)
            .then(|| run(modulation::SET_AMOUNT, Args::new().with("amount", amount)));
        dragged(&response, change, actions);
        response.on_hover_text(
            "How far it moves the parameter while its output is at 1: a share of the \
             parameter's range, or a plugin's own units. Below 0, it moves it the other way.",
        );
    });
    let key = routing.curve.key();
    let index = (Curve::KEYS.iter()).position(|&k| k == key).unwrap_or(0);
    let curve = ui.menu_button(format!("Curve: {}", CURVES[index].0), |ui| {
        for (&key, (name, about)) in Curve::KEYS.iter().zip(CURVES) {
            let on = key == routing.curve.key();
            if ui
                .add(Button::selectable(on, name))
                .on_hover_text(about)
                .clicked()
            {
                if !on {
                    actions.push(run(modulation::SET_CURVE, Args::new().with("curve", key)));
                }
                ui.close();
            }
        }
    });
    curve
        .response
        .on_hover_text("How it bends its output before it moves the parameter.");
    if let Curve::Stepped(steps) = routing.curve {
        ui.horizontal(|ui| {
            let label = ui.label("Steps");
            framed(ui);
            let mut n = steps;
            let field = DragValue::new(&mut n)
                .speed(0.1)
                .range(2..=Curve::STEPS_MAX)
                .update_while_editing(false);
            let response = ui.add(field).labelled_by(label.id);
            let change = (n != steps).then(|| {
                let args = Args::new().with("curve", "stepped");
                run(modulation::SET_CURVE, args.with("steps", i64::from(n)))
            });
            dragged(&response, change, actions);
        });
    }
    let modulation = project.modulation(owner);
    let scale = routing.scale.and_then(|scale| output_name(project, scale));
    let scale_name = scale.as_deref().unwrap_or("None");
    let scale = ui.menu_button(format!("Scale: {scale_name}"), |ui| {
        if ui
            .add(Button::selectable(routing.scale.is_none(), "None"))
            .clicked()
        {
            if routing.scale.is_some() {
                actions.push(run(modulation::SET_SCALE, Args::new()));
            }
            ui.close();
        }
        let modulators = modulation.map_or(&[][..], |m| &m.modulators[..]);
        for modulator in modulators {
            for n in modulator.outputs() {
                let output = Output::new(modulator.id, n);
                let on = routing.scale == Some(output);
                let name = output_name(project, output).unwrap_or_default();
                if ui.add(Button::selectable(on, name)).clicked() {
                    if !on {
                        let args = Args::new()
                            .with("modulator", modulator.id)
                            .with("output", i64::from(n));
                        actions.push(run(modulation::SET_SCALE, args));
                    }
                    ui.close();
                }
            }
        }
    });
    scale
        .response
        .on_hover_text("Another output on its strip that scales its amount, as velocity would.");
    ui.horizontal(|ui| {
        let label = ui.label("Lag");
        framed(ui);
        let mut lag = routing.lag;
        let field = DragValue::new(&mut lag)
            .speed(2.0)
            .range(0.0..=Routing::LAG_MAX)
            .suffix(" ms")
            .max_decimals(0)
            .update_while_editing(false);
        let response = ui.add(field).labelled_by(label.id);
        let change =
            (lag != routing.lag).then(|| run(modulation::SET_LAG, Args::new().with("lag", lag)));
        dragged(&response, change, actions);
        response.on_hover_text(
            "How long it takes to follow its output, which smooths it: about two thirds of the \
             way in that time.",
        );
    });
    let mut bypassed = routing.bypassed;
    let bypass = ui
        .checkbox(&mut bypassed, "Bypass")
        .on_hover_text("It moves nothing while it's bypassed, and keeps its settings.");
    if bypass.changed() {
        let args = Args::new().with("bypassed", bypassed);
        actions.push(run(modulation::SET_BYPASS, args));
    }
    ui.separator();
    if ui.button("Delete Routing").clicked() {
        actions.push(run(modulation::DELETE_ROUTING, Args::new()));
        ui.close();
    }
}

/// Gives the fields `ui` shows next the frames a menu takes off its
/// buttons, as the window's own fields have, to show they drag.
fn framed(ui: &mut Ui) {
    let widgets = ui.ctx().global_style().visuals.widgets.clone();
    let visuals = ui.visuals_mut();
    visuals.button_frame = true;
    visuals.widgets = widgets;
}

/// Asks for `change` from the field `response`, and while a drag changes
/// it, has the changes one step to undo, from when the drag starts to when
/// it stops. A value typed into it is a step of its own.
fn dragged(response: &Response, change: Option<Action>, actions: &mut Vec<Action>) {
    if response.drag_started() {
        actions.push(Action::BeginGesture);
    }
    actions.extend(change);
    if response.drag_stopped() {
        actions.push(Action::EndGesture);
    }
}

/// The command that has `output` move the parameter at `param` by
/// `amount`.
fn add_routing(output: Output, param: ParamAddress, amount: f64) -> Action {
    let args = Args::new()
        .with("modulator", output.modulator)
        .with("output", i64::from(output.n))
        .with("param", param)
        .with("amount", amount);
    Action::RunWith(modulation::ADD_ROUTING, args)
}

/// How far a new routing moves the parameter at `param`: [`NEW_AMOUNT`]
/// of its range, which for a plugin's parameter is in the plugin's units.
fn new_amount(view: &View<'_>, param: ParamAddress) -> f64 {
    if view.project.param_spec(param).is_some() {
        return NEW_AMOUNT;
    }
    let spec = param_spec(view, param);
    NEW_AMOUNT * (spec.max - spec.min)
}

/// What the menus call `output`: a Macro's knob's or button's name after
/// the Macro's, "Drums: Knob 1", or an LFO's or an Envelope's own.
fn output_name(project: &Project, output: Output) -> Option<String> {
    let (_, _, modulator) = project.find_modulator(output.modulator)?;
    let name = modulator.output_name(output.n)?;
    Some(match modulator.kind {
        ModulatorKind::Macro => format!("{}: {name}", modulator.name),
        ModulatorKind::Lfo | ModulatorKind::Envelope => name,
    })
}

/// What the modulators' menus call `chain`: its track's name, or Master.
fn channel_name(project: &Project, chain: Chain) -> &str {
    match chain {
        Chain::Track(id) => project.track(id).map_or("", |track| track.name.as_str()),
        Chain::Master => "Master",
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Toggled;
    use egui::{Event, Modifiers, PointerButton};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::sync::Arc;

    use musicable_model::modulation::{ModulatorId, RoutingId};
    use musicable_model::native::{NativeDevice, NativeKind};
    use musicable_model::param::{ParamSpec, Scale};
    use musicable_model::plugin::{PluginDevice, PluginSettings};
    use musicable_model::{DeviceId, TrackId, TrackKind};

    use super::*;
    use crate::devices::{PluginParam, PluginStatus};
    use crate::testing::{State, quick_harness};

    /// What a test's project has on Keys, the track the devices panel
    /// shows.
    struct Keys {
        track: TrackId,
        /// A Utility.
        utility: DeviceId,
        /// An LFO, then a Macro with a knob, on the strip.
        lfo: ModulatorId,
        knobs: ModulatorId,
    }

    /// Keys, a MIDI track with a Utility on it and an LFO and a Macro on its
    /// strip, as `change` leaves it, in the devices panel.
    fn keys(change: impl FnOnce(&mut Project, &Keys)) -> (Harness<'static, State>, Keys) {
        let mut project = Project::new();
        let track = project.add_track("Keys", TrackKind::Midi);
        let utility =
            project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Utility)));
        let lfo = project.new_modulator(ModulatorKind::Lfo, "LFO 1");
        let knobs = project.new_modulator(ModulatorKind::Macro, "Macro 1");
        let keys = Keys {
            track,
            utility: utility.id,
            lfo: lfo.id,
            knobs: knobs.id,
        };
        let on = project.track_mut(track).unwrap();
        on.devices.push(utility);
        on.modulation.modulators = vec![lfo, knobs];
        change(&mut project, &keys);
        let mut harness = quick_harness(project);
        harness.state_mut().editor.selected_track = Some(Chain::Track(track));
        harness.run();
        (harness, keys)
    }

    fn actions(harness: &mut Harness<'static, State>) -> Vec<Action> {
        std::mem::take(&mut harness.state_mut().actions)
    }

    fn click(harness: &mut Harness<'static, State>, label: &str) {
        harness.get_by_label(label).click();
        harness.run();
    }

    /// Opens the menu of what's called `name`, and picks `item`.
    fn menu(harness: &mut Harness<'static, State>, name: &str, item: &str) {
        harness.get_by_label(name).click_secondary();
        harness.run();
        harness.get_by_role_and_label(Role::Button, item).click();
        harness.run();
    }

    /// Presses the mouse's button at `at`, or lets go.
    fn press(harness: &mut Harness<'static, State>, at: Pos2, pressed: bool) {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.run();
    }

    /// Drags from `from` to `to`, in a few moves, without letting go.
    fn drag_to(harness: &mut Harness<'static, State>, from: Pos2, to: Pos2) {
        harness.hover_at(from);
        harness.run();
        press(harness, from, true);
        for step in 1..=4 {
            harness.hover_at(from + (to - from) * step as f32 / 4.0);
            harness.run();
        }
    }

    fn centre(harness: &Harness<'static, State>, label: &str) -> Pos2 {
        harness.get_by_label(label).rect().center()
    }

    fn command(command: CommandId, modulator: ModulatorId, args: Args) -> Action {
        Action::RunWith(command, args.with("modulator", modulator))
    }

    #[test]
    fn the_strip_has_a_card_for_each_modulator_and_adds_more() {
        let (mut harness, keys) = keys(|_, _| {});
        // Left of the devices, in the strip's order, each output with a
        // handle: an LFO's by its name, a Macro's by each knob.
        let left = |harness: &Harness<'static, State>, label: &str| {
            harness.get_by_label(label).rect().left()
        };
        let (lfo, knobs) = (
            left(&harness, "Modulator LFO 1"),
            left(&harness, "Modulator Macro 1"),
        );
        assert!(lfo < knobs);
        assert!(knobs < left(&harness, "Device Utility"));
        harness.get_by_label("LFO 1 Output");
        harness.get_by_label("Macro 1: Knob 1 Output");
        harness.get_by_role_and_label(Role::Slider, "Macro 1 Knob 1");
        harness.get_by_role_and_label(Role::Slider, "LFO 1 Rate");
        harness.get_by_role_and_label(Role::ComboBox, "LFO 1 Shape");

        // Add Modulator adds one at the end of the strip.
        click(&mut harness, "Add Modulator");
        harness
            .get_by_role_and_label(Role::Button, "Envelope")
            .click();
        harness.run();
        let add = Args::new()
            .with("kind", "envelope")
            .with("track", keys.track);
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(modulation::ADD, add)]
        );

        // A card's menu renames it, moves it along the strip and deletes it.
        menu(&mut harness, "Modulator LFO 1", "Move Right");
        menu(&mut harness, "Modulator Macro 1", "Rename...");
        menu(&mut harness, "Modulator Macro 1", "Delete");
        let index = Args::new().with("index", 1_i64);
        assert_eq!(
            actions(&mut harness),
            [
                command(modulation::MOVE, keys.lfo, index),
                command(edit::RENAME, keys.knobs, Args::new()),
                command(modulation::DELETE, keys.knobs, Args::new()),
            ]
        );
        harness.get_by_label("Modulator Macro 1").click_secondary();
        harness.run();
        let right = harness.get_by_role_and_label(Role::Button, "Move Right");
        assert!(right.accesskit_node().is_disabled());
        harness.key_press(egui::Key::Escape);
        harness.run();

        // A Macro's + adds a knob or a button, and × deletes a card.
        click(&mut harness, "Add a Control to Macro 1");
        harness
            .get_by_role_and_label(Role::Button, "Add Momentary Button")
            .click();
        harness.run();
        click(&mut harness, "Delete LFO 1");
        let momentary = Args::new().with("kind", "momentary");
        assert_eq!(
            actions(&mut harness),
            [
                command(modulation::ADD_CONTROL, keys.knobs, momentary),
                command(modulation::DELETE, keys.lfo, Args::new()),
            ]
        );
    }

    #[test]
    fn a_chain_with_modulators_and_no_devices_shows_its_strip() {
        let (harness, _) = keys(|project, keys| {
            project.track_mut(keys.track).unwrap().devices.clear();
        });
        harness.get_by_label("Modulator LFO 1");
        assert!(harness.query_by_label_contains("No devices.").is_some());
    }

    #[test]
    fn dragging_an_output_onto_a_control_routes_it_there() {
        let (mut harness, keys) = keys(|_, _| {});
        let handle = centre(&harness, "LFO 1 Output");
        // Not onto a switch, which modulators can't move.
        let mute = centre(&harness, "Utility Mute");
        drag_to(&mut harness, handle, mute);
        let dragged = egui::DragAndDrop::payload::<Dragged>(&harness.ctx);
        let output = Output::new(keys.lfo, 0);
        assert_eq!(
            dragged.as_deref(),
            Some(&Dragged::Output {
                output,
                owner: Chain::Track(keys.track),
                name: "LFO 1".to_owned(),
            })
        );
        press(&mut harness, mute, false);
        assert_eq!(actions(&mut harness), []);
        assert!(egui::DragAndDrop::payload::<Dragged>(&harness.ctx).is_none());

        // Onto a bar, it moves it a quarter of its range.
        let gain = centre(&harness, "Utility Gain");
        drag_to(&mut harness, handle, gain);
        press(&mut harness, gain, false);
        let param = ParamAddress::Device(keys.utility, 0);
        assert_eq!(
            actions(&mut harness),
            [add_routing(output, param, NEW_AMOUNT)]
        );
        // A Macro's knob goes onto the LFO's own controls.
        let knob = centre(&harness, "Macro 1: Knob 1 Output");
        let rate = centre(&harness, "LFO 1 Rate");
        drag_to(&mut harness, knob, rate);
        press(&mut harness, rate, false);
        let rate = ParamAddress::Modulator(keys.lfo, 3);
        let knob = Output::new(keys.knobs, 0);
        assert_eq!(actions(&mut harness), [add_routing(knob, rate, NEW_AMOUNT)]);
    }

    #[test]
    fn an_output_that_moves_a_control_already_doesnt_go_on_it_again() {
        let gain = |keys: &Keys| ParamAddress::Device(keys.utility, 0);
        let (mut harness, _) = keys(|project, keys| {
            let routing = project.new_routing(Output::new(keys.lfo, 0), gain(keys), 0.5);
            let on = project.track_mut(keys.track).unwrap();
            on.modulation.routings.push(routing);
        });
        let handle = centre(&harness, "LFO 1 Output");
        let to = centre(&harness, "Utility Gain");
        drag_to(&mut harness, handle, to);
        press(&mut harness, to, false);
        assert_eq!(actions(&mut harness), []);
    }

    #[test]
    fn a_momentary_button_is_on_while_its_held_and_leaves_no_step() {
        let (mut harness, keys) = keys(|project, keys| {
            let knobs = project.modulator_mut(keys.knobs).unwrap();
            for kind in [ControlKind::Momentary, ControlKind::Toggle] {
                let button = knobs.new_control(kind);
                knobs.controls.push(button);
            }
        });
        let set = |n: i64, value: f64| {
            let args = Args::new().with("param", n).with("value", value);
            command(modulation::SET_PARAM, keys.knobs, args)
        };
        // Held down, it's on, and let go, off, in one gesture.
        let button = centre(&harness, "Macro 1 Button 1");
        harness.hover_at(button);
        harness.run();
        press(&mut harness, button, true);
        assert_eq!(actions(&mut harness), [Action::BeginGesture, set(1, 1.0)]);
        press(&mut harness, button, false);
        assert_eq!(actions(&mut harness), [set(1, 0.0), Action::EndGesture]);
        // A toggle button turns on with a click, and stays on.
        click(&mut harness, "Macro 1 Button 2");
        assert_eq!(actions(&mut harness), [set(2, 1.0)]);
    }

    #[test]
    fn a_controls_menu_lists_its_routings_with_their_settings() {
        let gain = |keys: &Keys| ParamAddress::Device(keys.utility, 0);
        let mut routings = Vec::new();
        let (mut harness, _) = keys(|project, keys| {
            let lfo = project.new_routing(Output::new(keys.lfo, 0), gain(keys), 0.25);
            let mut knob = project.new_routing(Output::new(keys.knobs, 0), gain(keys), -0.5);
            knob.bypassed = true;
            routings = vec![lfo.id, knob.id];
            let on = project.track_mut(keys.track).unwrap();
            on.modulation.routings = vec![lfo, knob];
        });
        let routing = |command: CommandId, id: RoutingId, args: Args| {
            Action::RunWith(command, args.with("routing", id))
        };
        let open = |harness: &mut Harness<'static, State>, title: &str| {
            harness
                .get_by_role_and_label(Role::Slider, "Utility Gain")
                .click_secondary();
            harness.run();
            harness.get_by_label(title).hover();
            harness.run();
        };
        // Each routing, with its amount or why it moves nothing.
        open(&mut harness, "LFO 1 +25%");
        harness.get_by_label("Macro 1: Knob 1 (bypassed)");
        harness.get_by_label("Curve: Linear").hover();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "S-Curve")
            .click();
        harness.run();
        assert_eq!(
            actions(&mut harness),
            [routing(
                modulation::SET_CURVE,
                routings[0],
                Args::new().with("curve", "s_curve")
            )]
        );
        open(&mut harness, "Macro 1: Knob 1 (bypassed)");
        harness.get_by_label("Bypass").click();
        harness.run();
        open(&mut harness, "LFO 1 +25%");
        harness.get_by_label("Delete Routing").click();
        harness.run();
        assert_eq!(
            actions(&mut harness),
            [
                routing(
                    modulation::SET_BYPASS,
                    routings[1],
                    Args::new().with("bypassed", false)
                ),
                routing(modulation::DELETE_ROUTING, routings[0], Args::new()),
            ]
        );

        // Dragging the amount is one gesture, one step to undo.
        open(&mut harness, "LFO 1 +25%");
        let amount = harness
            .get_by_role_and_label(Role::SpinButton, "Amount")
            .rect()
            .center();
        drag_to(&mut harness, amount, amount + vec2(40.0, 0.0));
        press(&mut harness, amount + vec2(40.0, 0.0), false);
        let ran = actions(&mut harness);
        assert_eq!(ran.first(), Some(&Action::BeginGesture));
        assert_eq!(ran.last(), Some(&Action::EndGesture));
        let amounts: Vec<f64> = (ran[1..ran.len() - 1].iter())
            .map(|action| match action {
                Action::RunWith(id, args) if *id == modulation::SET_AMOUNT => {
                    args.get("amount").unwrap()
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(!amounts.is_empty());
        assert!(amounts.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(amounts[0] > 0.25);

        // Both outputs move it already, so there's none to add.
        harness
            .get_by_role_and_label(Role::Slider, "Utility Gain")
            .click_secondary();
        harness.run();
        let add = harness.get_by_role_and_label(Role::Button, "Add Modulation");
        assert!(add.accesskit_node().is_disabled());
    }

    #[test]
    fn a_handles_menu_lists_what_its_output_moves_and_routes_it_to_more() {
        let gain = |keys: &Keys| ParamAddress::Device(keys.utility, 0);
        let (mut harness, keys) = keys(|project, keys| {
            let lfo = project.new_routing(Output::new(keys.lfo, 0), gain(keys), 0.25);
            project.track_mut(keys.track).unwrap().modulation.routings = vec![lfo];
        });
        click(&mut harness, "LFO 1 Output");
        harness.get_by_label("Utility: Gain +25%");
        harness.get_by_label("Route To").hover();
        harness.run();
        // The track's own parameters, then each device's and modulator's.
        harness.get_by_role_and_label(Role::Button, "Volume");
        harness.get_by_label("Utility").hover();
        harness.run();
        // It moves the gain already, which is ticked, and not the width.
        let ticked = harness.get_by_role_and_label(Role::Button, "Gain");
        assert_eq!(ticked.accesskit_node().toggled(), Some(Toggled::True));
        let width = harness.get_by_role_and_label(Role::Button, "Width");
        assert_eq!(width.accesskit_node().toggled(), Some(Toggled::False));
        // The switches aren't there: modulators can't move them.
        assert!(harness.query_by_label("Mute").is_none());
        harness.get_by_role_and_label(Role::Button, "Width").click();
        harness.run();
        let width = ParamAddress::Device(keys.utility, 2);
        let output = Output::new(keys.lfo, 0);
        assert_eq!(
            actions(&mut harness),
            [add_routing(output, width, NEW_AMOUNT)]
        );
    }

    #[test]
    fn route_to_lists_a_clap_plugins_parameters_and_not_a_vst3_plugins() {
        let mut plugins = Vec::new();
        let (mut harness, keys) = keys(|project, keys| {
            for (format, name) in [
                (PluginFormat::Clap, "Surge XT"),
                (PluginFormat::Vst3, "Diva"),
            ] {
                let device = project.new_device(DeviceKind::Plugin(PluginDevice {
                    format,
                    id: name.to_lowercase(),
                    name: name.to_owned(),
                    instrument: false,
                    settings: PluginSettings::default(),
                }));
                plugins.push(device.id);
                project.track_mut(keys.track).unwrap().devices.push(device);
            }
        });
        // Both plugins have loaded, each with a parameter from 0 to 10.
        let cutoff = PluginParam {
            id: 7,
            spec: ParamSpec {
                name: "Cutoff".into(),
                min: 0.0,
                max: 10.0,
                default: 5.0,
                scale: Scale::Linear,
            },
        };
        let loaded = PluginStatus {
            loaded: true,
            params: Arc::from([cutoff]),
            ..PluginStatus::default()
        };
        for &plugin in &plugins {
            (harness.state_mut().plugin_status).insert(plugin, loaded.clone());
        }
        harness.run();
        click(&mut harness, "LFO 1 Output");
        harness.get_by_label("Route To").hover();
        harness.run();
        // The VST3 plugin's parameters aren't there: modulators don't move
        // them yet.
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Diva")
                .is_none()
        );
        harness
            .get_by_role_and_label(Role::Button, "Surge XT")
            .hover();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Cutoff")
            .click();
        harness.run();
        // A quarter of its range, in the plugin's own units.
        let cutoff = ParamAddress::Device(plugins[0], 7);
        let output = Output::new(keys.lfo, 0);
        assert_eq!(actions(&mut harness), [add_routing(output, cutoff, 2.5)]);
    }

    #[test]
    fn routings_reach_as_far_as_their_curves_and_amounts_go() {
        assert_eq!(span(Curve::Linear, false, false, 0.5), (0.0, 0.5));
        assert_eq!(span(Curve::Linear, false, false, -0.5), (-0.5, 0.0));
        assert_eq!(span(Curve::SCurve, true, false, 0.5), (-0.5, 0.5));
        assert_eq!(span(Curve::Negative, false, false, 1.0), (0.0, 0.0));
        assert_eq!(span(Curve::Absolute, true, false, 1.0), (0.0, 1.0));
        assert_eq!(span(Curve::Positive, true, false, -1.0), (-1.0, 0.0));
        assert_eq!(span(Curve::Negative, true, false, 1.0), (-1.0, 0.0));
        // A bipolar scale turns it either way.
        assert_eq!(span(Curve::Linear, false, true, 0.5), (-0.5, 0.5));

        let mut project = Project::new();
        let track = project.add_track("Keys", TrackKind::Midi);
        let volume = ParamAddress::Volume(Chain::Track(track));
        let lfo = project.new_modulator(ModulatorKind::Lfo, "LFO 1");
        let knobs = project.new_modulator(ModulatorKind::Macro, "Macro 1");
        let routings = vec![
            project.new_routing(Output::new(lfo.id, 0), volume, 0.25),
            project.new_routing(Output::new(knobs.id, 0), volume, 0.5),
        ];
        let mut bypassed = project.new_routing(Output::new(knobs.id, 0), volume, 1.0);
        bypassed.bypassed = true;
        let modulation = &mut project.track_mut(track).unwrap().modulation;
        modulation.modulators = vec![lfo, knobs];
        assert_eq!(reach(&project, volume), None);
        let modulation = &mut project.track_mut(track).unwrap().modulation;
        modulation.routings = routings;
        modulation.routings.push(bypassed);
        assert_eq!(reach(&project, volume), Some((-0.25, 0.75)));
    }
}

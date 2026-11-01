//! The devices panel, under the arrangement: the devices of the selected
//! track, which can be the master track, in the order they play, with the
//! controls of Musicable's own and the side inputs of plugins, and the
//! track's modulators at their left ([`modulators`]). Clicks on the
//! devices' title bars select them, as the mouse map says, and drags move
//! them along the chain, or onto a track or a mixer strip, or copy them
//! there with Ctrl. And the browser's list of Musicable's own devices and
//! the plugins installed, which adds them to the devices the panel shows.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use egui::{
    Align, Button, CursorIcon, FontId, Frame, Id, Label, LayerId, Layout, Order, Rect, Response,
    RichText, Role, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TextStyle, TextWrapMode, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetText, pos2, vec2,
};
use musicable_commands::{Args, CommandId, device, edit};
use musicable_model::native::NativeKind;
use musicable_model::param::ParamSpec;
use musicable_model::plugin::{NoteTiming, PluginDevice, PluginKind};
use musicable_model::routing::UnheardNotes;
use musicable_model::{Chain, Device, DeviceId, DeviceKind, PluginFormat, Project};

use crate::browser::{self, Destination, Dragged};
use crate::modulators;
use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::native::{self, sidechain};
use crate::performance::{self, Cpu};
use crate::search::score;
use crate::shell::{Action, View, with_keys};
use crate::theme::Theme;
use crate::widgets::{Face, name_box, paint_toggle, placeholder};

/// The plugin search field's id, so a key can focus it.
pub(crate) const PLUGIN_SEARCH: &str = "plugin search";

/// A plugin the browser lists: one a scan found installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    pub format: PluginFormat,
    /// Its id in its format, which projects name it by.
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// What it does with notes: what the scan found, unless the user
    /// marked it otherwise ([`browser::SET_PLUGIN_KIND`]).
    ///
    /// [`browser::SET_PLUGIN_KIND`]: musicable_commands::browser::SET_PLUGIN_KIND
    pub kind: PluginKind,
    /// Whether the user set it to Render Ahead Never, so it renders live
    /// wherever it is ([`browser::SET_PLUGIN_RENDER_AHEAD`]).
    ///
    /// [`browser::SET_PLUGIN_RENDER_AHEAD`]: musicable_commands::browser::SET_PLUGIN_RENDER_AHEAD
    pub never_ahead: bool,
    /// When the user marked its notes as coming, against its latency, if
    /// they did ([`browser::SET_PLUGIN_NOTE_TIMING`]).
    ///
    /// [`browser::SET_PLUGIN_NOTE_TIMING`]: musicable_commands::browser::SET_PLUGIN_NOTE_TIMING
    pub note_timing: Option<NoteTiming>,
}

/// How a plugin device's plugin is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginStatus {
    /// Its plugin loaded, and plays.
    pub loaded: bool,
    /// It has a window of its own.
    pub has_window: bool,
    /// Its window is open.
    pub window_open: bool,
    /// Why it plays nothing, if it doesn't.
    pub problem: Option<String>,
    /// The parameters automation can move, in the plugin's order, once it's
    /// loaded: those it neither hides nor keeps to itself.
    pub params: Arc<[PluginParam]>,
    /// Its plugin's latency, in milliseconds, once it's loaded.
    pub latency_ms: f64,
    /// When the notes its plugin plays come, against its latency, without
    /// anyone choosing for the device, and why, where it matters: for a MIDI
    /// effect, or a plugin that plays notes out, with a latency.
    pub note_timing: Option<(NoteTiming, TimingFrom)>,
}

/// Why a plugin's note timing is what it is without anyone choosing for
/// its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimingFrom {
    /// The user marked the plugin, for every project.
    Marked,
    /// Musicable played it, and heard.
    Measured,
    /// It's what's assumed for its kind, as nothing said otherwise.
    #[default]
    Assumed,
}

impl TimingFrom {
    /// What menus say of it: "marked", "measured" or "assumed".
    pub fn name(self) -> &'static str {
        match self {
            Self::Marked => "marked",
            Self::Measured => "measured",
            Self::Assumed => "assumed",
        }
    }
}

/// A plugin's parameter, for automation lanes to show.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginParam {
    /// The plugin's id for it.
    pub id: u32,
    /// Its name, its range and its default. Its values spread evenly.
    pub spec: ParamSpec,
}

/// The plugins, as the browser and the devices panel show them.
#[derive(Debug, Clone, Copy)]
pub struct Plugins<'a> {
    /// The plugins installed, in the order the browser lists them.
    pub installed: &'a [PluginEntry],
    /// Whether a scan for plugins is running.
    pub scanning: bool,
    /// How each plugin device's plugin is doing.
    pub status: &'a BTreeMap<DeviceId, PluginStatus>,
}

impl Default for Plugins<'_> {
    /// No plugins installed, and none scanning.
    fn default() -> Self {
        static NONE: BTreeMap<DeviceId, PluginStatus> = BTreeMap::new();
        Self {
            installed: &[],
            scanning: false,
            status: &NONE,
        }
    }
}

/// What the devices panel shows without a chain to show.
const NO_TRACK: &str = "Select a track, or the master track, to see its devices.";
/// What it shows for a chain without devices.
const NO_DEVICES: &str =
    "No devices. Double-click a device or a preset in the browser, or drag one here, to add it.";

/// The devices panel's own state.
#[derive(Debug, Default)]
pub(crate) struct DevicesPanel {
    /// The device being saved as a preset, while the box asks for the
    /// preset's name.
    saving: Option<Saving>,
    /// The device to scroll to, the next time the panel shows.
    reveal: Option<DeviceId>,
    /// The boxes of the devices the panel showed last, left to right, and
    /// the part of the panel they're in sight in: what's dragged into the
    /// panel goes between them.
    boxes: Vec<(DeviceId, Rect)>,
    sight: Option<Rect>,
}

/// A device being saved as a preset.
#[derive(Debug)]
struct Saving {
    device: DeviceId,
    /// The name typed so far: `None` until the box has shown the device's.
    name: Option<String>,
    device_name: String,
}

impl DevicesPanel {
    /// Shows the devices of `chain`, the selected track's, and adds what
    /// the user asked for to `actions`. Returns whether the user asked for
    /// the plugin browser.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        chain: Option<Chain>,
        theme: &Theme,
        mouse: &MouseMap,
        actions: &mut Vec<Action>,
    ) -> bool {
        let project = view.project;
        let mut browse = false;
        self.ask_preset_name(ui.ctx(), actions);
        ui.add_space(theme.devices.margin);
        ui.horizontal(|ui| {
            let name = match chain {
                Some(Chain::Master) => Some("Master"),
                Some(Chain::Track(id)) => project.track(id).map(|track| track.name.as_str()),
                None => None,
            };
            if let Some(name) = name {
                let label = ui.add(Label::new(RichText::new(name).strong()).truncate());
                let label_text = format!("Devices of {name}");
                label.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label_text));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                browse = ui
                    .button("Add Device...")
                    .on_hover_text("Shows the browser")
                    .clicked();
                if let Some(chain) = chain.filter(|_| name.is_some()) {
                    modulators::add_menu(ui, chain, actions);
                }
            });
        });
        ui.add_space(theme.devices.margin);
        if let Some(chain) = chain {
            for unheard in project.unheard_notes_of(chain) {
                let text = RichText::new(unheard_text(project, unheard))
                    .size(theme.small_text)
                    .color(theme.warning);
                ui.add(Label::new(text).wrap());
            }
        }
        self.boxes.clear();
        self.sight = None;
        let shown = chain.and_then(|chain| Some((chain, project.devices(chain)?)));
        let Some((chain, devices)) = shown else {
            placeholder(ui, NO_TRACK);
            return browse;
        };
        let modulators = project
            .modulation(chain)
            .is_some_and(|m| !m.modulators.is_empty());
        if devices.is_empty() && !modulators {
            placeholder(ui, NO_DEVICES);
            return browse;
        }
        let reveal = self.reveal.take();
        let selection = view.editor.selected_devices(project);
        let shown = ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            // Under the boxes, so a click beside them, and not in one,
            // selects none of them.
            let beside = ui.interact(ui.clip_rect(), ui.scope_id().with("beside"), Sense::click());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = theme.devices.gap;
                // The modulators first, apart from the devices.
                if modulators {
                    modulators::strip(ui, chain, view, theme, mouse, actions);
                    ui.separator();
                }
                if devices.is_empty() {
                    ui.label(RichText::new(NO_DEVICES).weak());
                }
                for device in devices {
                    ui.push_id(device.id, |ui| {
                        let saving = &mut self.saving;
                        let shown =
                            device_box(ui, device, view, &selection, theme, mouse, saving, actions);
                        if reveal == Some(device.id) {
                            shown.scroll_to_me(None);
                        }
                        self.boxes.push((device.id, shown.rect));
                    });
                }
            });
            beside
        });
        self.sight = Some(shown.inner_rect);
        if shown.inner.clicked() && !selection.is_empty() {
            actions.push(select(Vec::new(), false));
        }
        browse
    }

    /// Lets what's dragged from the browser, or from the panel's title bars,
    /// go into the panel, which is `area`, as the chain it shows, `chain`,
    /// and returns what letting it go asks for. Between the devices, a line
    /// shows where it would go. An empty chain is outlined while it's over
    /// it.
    pub fn drop(
        &self,
        ctx: &egui::Context,
        area: Rect,
        chain: Option<Chain>,
        theme: &Theme,
    ) -> Option<Action> {
        let chain = chain?;
        // Round every box.
        let around = (self.boxes.iter())
            .map(|&(_, rect)| rect)
            .reduce(Rect::union);
        let (Some(sight), Some(around)) = (self.sight, around) else {
            return browser::drop_in(ctx, area, &Destination::Chain(Some(chain)), theme);
        };
        let pos = ctx.pointer_latest_pos().filter(|&pos| area.contains(pos))?;
        let dragged = egui::DragAndDrop::payload::<Dragged>(ctx)?;
        // Before the first device whose middle is right of the pointer.
        let gap = (self.boxes.iter())
            .take_while(|(_, rect)| rect.center().x < pos.x)
            .count();
        let index = match &*dragged {
            // Counting the chain without the devices moved along it.
            Dragged::Devices {
                devices,
                copy: false,
                ..
            } => {
                let before = self.boxes[..gap].iter();
                gap - before.filter(|(id, _)| devices.contains(id)).count()
            }
            _ => gap,
        };
        let action = dragged.dropped(&Destination::Devices { chain, index })?;
        let half = theme.devices.gap / 2.0;
        let x = match (gap.checked_sub(1), self.boxes.get(gap)) {
            (Some(before), Some((_, after))) => (self.boxes[before].1.right() + after.left()) / 2.0,
            (Some(_), None) => around.right() + half,
            (None, _) => around.left() - half,
        };
        let x = x.clamp(sight.left() + 1.0, sight.right() - 1.0);
        let layer = LayerId::new(Order::Foreground, Id::unique("drop between devices"));
        let painter = ctx.layer_painter(layer).with_clip_rect(sight);
        painter.vline(x, around.y_range(), Stroke::new(2.0, theme.start_marker));
        if !ctx.input(|i| i.pointer.any_released()) {
            return None;
        }
        egui::DragAndDrop::clear_payload(ctx);
        Some(action)
    }

    /// Scrolls to `device` the next time the panel shows, if it shows it.
    pub fn reveal(&mut self, device: DeviceId) {
        self.reveal = Some(device);
    }

    /// Whether the panel asks the name of a preset, as keys answer.
    pub fn asking(&self) -> bool {
        self.saving.is_some()
    }

    /// The box that asks for the name of the preset a device is saved as,
    /// while one is.
    fn ask_preset_name(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(saving) = &mut self.saving else {
            return;
        };
        let first = saving.name.is_none();
        let name = saving
            .name
            .get_or_insert_with(|| saving.device_name.clone());
        let title = format!("Save {} as a Preset", saving.device_name);
        let Some(confirmed) = name_box(ctx, "save preset", &title, "Save", name, first) else {
            return;
        };
        let name = name.trim();
        if confirmed && !name.is_empty() {
            let args = Args::new().with("device", saving.device).with("name", name);
            actions.push(Action::RunWith(device::SAVE_PRESET, args));
        }
        self.saving = None;
    }
}

/// A device: its title bar, and a built-in device's controls, or a
/// plugin's kind, how its plugin is doing and its side inputs' sources,
/// dimmed while it's off, and outlined while it's one of the devices
/// selected, `selection`. A click on it that no control takes selects it,
/// as one on its title bar does, and the title bar drags it. Its menu works
/// on the selection if it's in it, and saves it as a preset, which `saving`
/// asks the name of. A sampler takes a sample dragged onto it from the
/// browser. Returns its box.
#[allow(clippy::too_many_arguments)]
fn device_box(
    ui: &mut Ui,
    device: &Device,
    view: &View<'_>,
    selection: &[DeviceId],
    theme: &Theme,
    mouse: &MouseMap,
    saving: &mut Option<Saving>,
    actions: &mut Vec<Action>,
) -> Response {
    let sizes = &theme.devices;
    let (name, kind) = describe(device);
    let width = match device.kind.native() {
        Some(native) => native::width(native, theme),
        None => sizes.device - 2.0 * sizes.margin,
    };
    // The box senses clicks under its controls, which take their own.
    let shown = ui.scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
        Frame::new()
            .fill(theme.device)
            .stroke(Stroke::new(1.0, theme.divider))
            .corner_radius(theme.corner)
            .inner_margin(sizes.margin)
            .show(ui, |ui| {
                ui.set_width(width);
                // Top to bottom, whatever the row of boxes around it does.
                ui.vertical(|ui| {
                    let cpu = view.performance.device(device.id);
                    let title = title_row(ui, device, name, cpu, theme, mouse, actions);
                    // A device that's off looks it, though its controls
                    // still work, so it can be set up before it comes back.
                    if device.bypassed || !device.active {
                        ui.multiply_opacity(sizes.dimmed.clamp(0.0, 1.0));
                    }
                    // What kind it is, which a click selects the device by,
                    // as it does beside the controls.
                    let kind = Label::new(RichText::new(kind).size(theme.small_text).weak());
                    let kind = kind.selectable(false);
                    match device.kind {
                        DeviceKind::Native(ref native) => {
                            native::controls(
                                ui, device.id, name, native, view, theme, mouse, actions,
                            );
                        }
                        DeviceKind::Plugin(_) => {
                            ui.add(kind);
                            plugin_status(ui, device.id, name, view, theme, actions);
                            side_inputs(ui, device.id, name, view, theme, mouse, actions);
                        }
                        _ => {
                            ui.add(kind);
                        }
                    }
                    title
                })
                .inner
            })
            .inner
    });
    let (title, body) = (shown.inner, shown.response);
    let selected = selection.contains(&device.id);
    if selected {
        let outline = Stroke::new(1.5, theme.clip_selected);
        (ui.painter()).rect_stroke(body.rect, theme.corner, outline, StrokeKind::Inside);
    }
    // Screen readers hear whether it's off, and selected, with its name.
    let mut label = format!("Device {name}");
    if device.bypassed {
        label += ", bypassed";
    }
    if !device.active {
        label += ", deactivated";
    }
    if selected {
        label += ", selected";
    }
    title.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
    if title.clicked() || body.clicked() {
        let mods = modifiers(ui.input(|input| input.modifiers));
        match mouse.action(Target::DeviceTitle, Gesture::Click, mods) {
            Some(MouseAction::Select) => actions.push(select(vec![device.id], false)),
            Some(MouseAction::ToggleSelect) => actions.push(select(vec![device.id], true)),
            _ => {}
        }
    }
    // Its menu, and a drag, take it alone if it's not selected, and select
    // it, as in Live; else the selection, all of it.
    let opened = title.secondary_clicked() || body.secondary_clicked();
    if (opened || title.drag_started()) && !selected {
        actions.push(select(vec![device.id], false));
    }
    let targets = match selected {
        true => selection.to_vec(),
        false => vec![device.id],
    };
    drag(ui, &title, &targets, name, mouse);
    if title.double_clicked() && device.kind.plugin().is_some() {
        let args = Args::new().with("device", device.id);
        actions.push(Action::RunWith(device::SHOW_WINDOW, args));
    }
    for response in [&title, &body] {
        response.context_menu(|ui| menu(ui, device, name, view, &targets, saving, actions));
    }
    // A sample dragged from the browser onto a sampler loads into it, where
    // the box is in sight.
    if (device.kind.native()).is_some_and(|native| native.kind == NativeKind::Sampler) {
        let rect = body.rect.intersect(ui.clip_rect());
        let target = Destination::Sampler(device.id);
        actions.extend(browser::drop_in(ui.ctx(), rect, &target, theme));
    }
    body
}

/// A device's title bar: its Bypass button, its name and the button that
/// deletes it, and a sampler's sample. Returns its name's part, which the
/// Bypass button and the others leave: what clicks select the device by,
/// drags take it by, and a double-click shows its plugin's window by, as
/// in Live.
fn title_row(
    ui: &mut Ui,
    device: &Device,
    name: &str,
    cpu: Option<Cpu>,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) -> Response {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let delete = ui.small_button("×").on_hover_text("Delete");
            let label = format!("Delete {name}");
            delete.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
            if delete.clicked() {
                let args = Args::new().with("device", device.id);
                actions.push(Action::RunWith(device::DELETE, args));
            }
            // A deactivated device takes no time.
            if let Some(cpu) = cpu.filter(|_| device.active) {
                cpu_figure(ui, name, cpu, theme);
            }
            let native = device.kind.native();
            if let Some(sampler) = native.filter(|native| native.kind == NativeKind::Sampler) {
                let file = sampler.sample.as_deref();
                sample(ui, device.id, name, file, theme, actions);
            }
            if !device.active {
                let text = RichText::new("Deactivated").size(theme.small_text).weak();
                ui.add(Label::new(text).selectable(false));
            }
            // On the left, after the Bypass button, in the room the buttons
            // leave.
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                bypass_button(ui, device, name, theme, mouse, actions);
                let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
                let (rect, title) = ui.allocate_exact_size(size, Sense::click_and_drag());
                let text = RichText::new(name).strong().color(theme.header_text);
                let wrap = Some(TextWrapMode::Truncate);
                let text =
                    WidgetText::from(text).into_galley(ui, wrap, rect.width(), TextStyle::Body);
                let at = pos2(rect.left(), rect.center().y - text.size().y / 2.0);
                ui.painter().galley(at, text, theme.header_text);
                title
            })
            .inner
        })
        .inner
    })
    .inner
}

/// The share of a core the device called `name` takes, small, amber or
/// red when it's busy, with how it renders and, live, its longest block in
/// the tooltip.
fn cpu_figure(ui: &mut Ui, name: &str, cpu: Cpu, theme: &Theme) {
    let figure = performance::percent(cpu.average);
    let colour = performance::colour(cpu.average, theme);
    let colour = colour.unwrap_or_else(|| ui.visuals().weak_text_color());
    let font = FontId::proportional(theme.small_text);
    let galley = ui.painter().layout_no_wrap(figure.clone(), font, colour);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo {
        value: Some(f64::from(cpu.average)),
        current_text_value: Some(figure.clone()),
        ..WidgetInfo::labeled(Role::Meter, true, format!("{name} CPU"))
    });
    if ui.is_rect_visible(rect) {
        ui.painter().galley(rect.min, galley, colour);
    }
    response.on_hover_ui(|ui| {
        let average = performance::percent(cpu.average);
        ui.label(match cpu.live_longest() {
            Some(longest) => format!(
                "CPU: {average} of a core over the last second. Live: its longest block took \
                 {} of a buffer.",
                performance::percent(longest),
            ),
            None => format!(
                "CPU: {average} of a core over the last second. Ahead: it renders ahead of the \
                 audio device, so a long block doesn't drop out."
            ),
        });
    });
}

/// Drags `devices` by the title bar `title`, of the device called `name`,
/// while the mouse map's drag does: moving them, or copying them, as Ctrl
/// does in Live's, by the modifiers held as they're let go.
fn drag(ui: &Ui, title: &Response, devices: &[DeviceId], name: &str, mouse: &MouseMap) {
    let ctx = ui.ctx();
    let started = title.drag_started();
    // Escape takes away what's dragged, and the drag with it.
    if !started && !(title.dragged() && egui::DragAndDrop::has_payload_of_type::<Dragged>(ctx)) {
        return;
    }
    let mods = modifiers(ui.input(|input| input.modifiers));
    let copy = match mouse.action(Target::DeviceTitle, Gesture::Drag, mods) {
        Some(MouseAction::Move) => false,
        Some(MouseAction::Copy) => true,
        _ => return,
    };
    let name = match devices {
        [_] => name.to_owned(),
        _ => format!("{} Devices", devices.len()),
    };
    let dragged = Dragged::Devices {
        devices: devices.to_vec(),
        copy,
        name,
    };
    egui::DragAndDrop::set_payload(ctx, dragged);
    if copy {
        ctx.set_cursor_icon(CursorIcon::Copy);
    }
}

/// A device's menu, which works on `targets`: the selected devices if it's
/// one of them, or else the device alone. Bypass is ticked while they're
/// all bypassed, and Activate shows while any is deactivated. The edit
/// commands select them first, so they take them, Save as Preset saves the
/// device itself, `name`, which `saving` asks the name of, and Render Ahead
/// sets the device's plugin's, if it's an installed plugin's.
fn menu(
    ui: &mut Ui,
    device: &Device,
    name: &str,
    view: &View<'_>,
    targets: &[DeviceId],
    saving: &mut Option<Saving>,
    actions: &mut Vec<Action>,
) {
    let all = |test: fn(&Device) -> bool| {
        (targets.iter()).all(|&id| (view.project.find_device(id)).is_some_and(|(_, _, d)| test(d)))
    };
    let (bypassed, active) = (all(|d| d.bypassed), all(|d| d.active));
    let bypass = Button::new("Bypass").selected(bypassed);
    let bypass = with_keys(bypass, view, &device::TOGGLE_BYPASS);
    if ui.add(bypass).clicked() {
        actions.push(set_bypass(targets, !bypassed));
    }
    let switch = if active { "Deactivate" } else { "Activate" };
    if ui.button(switch).clicked() {
        actions.push(set_active(targets, !active));
    }
    ui.separator();
    let pasting = view.editor.devices.clipboard.is_some();
    let edits: [(&str, CommandId); 4] = [
        ("Cut", edit::CUT),
        ("Copy", edit::COPY),
        ("Paste", edit::PASTE),
        ("Duplicate", edit::DUPLICATE),
    ];
    for (title, command) in edits {
        let enabled = pasting || command != edit::PASTE;
        let button = with_keys(Button::new(title), view, &command);
        if ui.add_enabled(enabled, button).clicked() {
            actions.extend([select(targets.to_vec(), false), Action::Run(command)]);
        }
    }
    ui.separator();
    if ui.button("Save as Preset...").clicked() {
        *saving = Some(Saving {
            device: device.id,
            name: None,
            device_name: name.to_owned(),
        });
    }
    let delete = with_keys(Button::new("Delete"), view, &edit::DELETE);
    if ui.add(delete).clicked() {
        actions.extend([select(targets.to_vec(), false), Action::Run(edit::DELETE)]);
    }
    if let (Some(plugin), Some(status)) =
        (device.kind.plugin(), view.plugins.status.get(&device.id))
        && let Some(default) = status.note_timing
    {
        ui.separator();
        note_timing_items(ui, device.id, plugin, status.latency_ms, default, actions);
    }
    // The device's own plugin's, which holds for all of its devices.
    let installed = device.kind.plugin().and_then(|plugin| {
        (view.plugins.installed.iter())
            .find(|entry| entry.format == plugin.format && entry.id == plugin.id)
    });
    if let Some(plugin) = installed {
        ui.separator();
        render_ahead_items(ui, plugin, actions);
    }
}

/// The Bypass button of `device`, which the panel calls `name`, drawn as
/// Live's power button: lit while the device plays, and dark while it's
/// bypassed or deactivated. What a click does is the mouse map's: in
/// Live's, a click bypasses the device or brings it back, and Alt-click
/// deactivates it or activates it.
fn bypass_button(
    ui: &mut Ui,
    device: &Device,
    name: &str,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    let side = ui.spacing().interact_size.y;
    let (rect, button) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    // Screen readers hear it as the switch it's named for, which is on
    // while the device is bypassed.
    let label = format!("{name} Bypass");
    button.widget_info(|| WidgetInfo::selected(Role::Button, true, device.bypassed, &label));
    let plays = device.active && !device.bypassed;
    paint_toggle(ui, rect, &button, Face::Power, plays, theme.switch, theme);
    let about = match (device.active, device.bypassed) {
        (false, _) => "Deactivated: out of the signal, latency and all. Click to activate it.",
        (true, true) => "Bypassed. Click to bring it back, or Alt-click to deactivate it.",
        (true, false) => {
            "Bypass: click to bypass it, or Alt-click to deactivate it, which takes its \
             latency out too."
        }
    };
    let button = button.on_hover_text(about);
    if !button.clicked() {
        return;
    }
    let mods = modifiers(ui.input(|input| input.modifiers));
    let id = [device.id];
    match mouse.action(Target::BypassButton, Gesture::Click, mods) {
        Some(MouseAction::ToggleBypass) if !device.active => actions.push(set_active(&id, true)),
        Some(MouseAction::ToggleBypass) => actions.push(set_bypass(&id, !device.bypassed)),
        Some(MouseAction::ToggleActive) => actions.push(set_active(&id, !device.active)),
        _ => {}
    }
}

/// The command that selects `devices`, or adds them to the selection or
/// takes them out of it with `toggle`.
fn select(devices: Vec<DeviceId>, toggle: bool) -> Action {
    let mut args = Args::new().with("devices", devices);
    if toggle {
        args = args.with("toggle", true);
    }
    Action::RunWith(device::SELECT, args)
}

/// The command that bypasses `devices`, or brings them back.
fn set_bypass(devices: &[DeviceId], bypassed: bool) -> Action {
    let args = Args::new()
        .with("devices", devices.to_vec())
        .with("bypassed", bypassed);
    Action::RunWith(device::SET_BYPASS, args)
}

/// The command that deactivates `devices`, or activates them.
fn set_active(devices: &[DeviceId], active: bool) -> Action {
    let args = Args::new()
        .with("devices", devices.to_vec())
        .with("active", active);
    Action::RunWith(device::SET_ACTIVE, args)
}

/// The sample `file` of the sampler `device`, which the panel calls `name`:
/// the button that loads another, and the file's name, whose menu takes it
/// away. Right to left, beside the device's name.
fn sample(
    ui: &mut Ui,
    device: DeviceId,
    name: &str,
    file: Option<&str>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let args = Args::new().with("device", device);
    let load = ui.small_button("Load Sample...").on_hover_text(
        "Asks for an audio file for it to play. Or drag a sample onto it from the browser.",
    );
    let label = format!("{name} Load Sample");
    load.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    if load.clicked() {
        actions.push(Action::RunWith(device::LOAD_SAMPLE, args.clone()));
    }
    // The file's own name, as far as there's room for three controls.
    let shown = match file {
        Some(file) => file.rsplit(['/', '\\']).next().unwrap_or(file),
        None => "No sample",
    };
    let text = RichText::new(shown).size(theme.small_text);
    let text = if file.is_some() { text } else { text.weak() };
    let room = vec2(3.0 * theme.devices.control, ui.spacing().interact_size.y);
    let layout = Layout::right_to_left(Align::Center);
    let shown_name = ui.allocate_ui_with_layout(room, layout, |ui| {
        let label = Label::new(text).truncate().selectable(false);
        ui.add(label.sense(Sense::click()))
    });
    let label = format!("{name} Sample: {shown}");
    let shown_name = shown_name.inner;
    shown_name.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
    if let Some(file) = file {
        let shown_name = shown_name.on_hover_text(format!("{file}\nRight-click to remove it."));
        shown_name.context_menu(|ui| {
            if ui.button("Remove Sample").clicked() {
                actions.push(Action::RunWith(device::SET_SAMPLE, args));
                ui.close();
            }
        });
    }
}

/// How the plugin of the plugin device `device` is doing: the button for
/// its window once it's loaded, or why it isn't.
fn plugin_status(
    ui: &mut Ui,
    device: DeviceId,
    name: &str,
    view: &View<'_>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let status = view.plugins.status.get(&device);
    match status {
        Some(status) if status.loaded && status.has_window => {
            let open = status.window_open;
            let button = ui.add(Button::new("Plugin Window").selected(open));
            let label = format!("{name} Plugin Window");
            button.widget_info(|| WidgetInfo::selected(Role::Button, true, open, &label));
            if button.clicked() {
                let command = if open {
                    device::HIDE_WINDOW
                } else {
                    device::SHOW_WINDOW
                };
                actions.push(Action::RunWith(command, Args::new().with("device", device)));
            }
        }
        Some(status) if status.loaded => {
            ui.label(RichText::new("No window of its own").weak());
        }
        _ => {
            let problem = match status.and_then(|s| s.problem.as_deref()) {
                Some(problem) => problem,
                None if view.plugins.scanning => "Waiting for the plugin scan",
                None => "Not loaded",
            };
            let text = RichText::new(problem)
                .size(theme.small_text)
                .color(theme.warning);
            ui.add(Label::new(text).wrap());
        }
    }
}

/// A row for each side input of the plugin device `device`, which the
/// panel calls `name`: the plugin's name for it, over its sources, as the
/// Compressor's Sidechain column has them.
fn side_inputs(
    ui: &mut Ui,
    device: DeviceId,
    name: &str,
    view: &View<'_>,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    for port in view.project.ports(device) {
        let Some(input) = view.project.input_name(port) else {
            continue;
        };
        ui.push_id(port, |ui| {
            let heading = ui.label(RichText::new(input).size(theme.small_text).weak());
            sidechain::add(ui, heading.rect, port, name, view, theme, actions);
            sidechain::sources(ui, port, name, view, theme, mouse, actions);
        });
    }
}

/// A device's name, and what kind of device it is.
pub(crate) fn describe(device: &Device) -> (&str, String) {
    match &device.kind {
        DeviceKind::Plugin(plugin) => {
            let name = match plugin.name.trim() {
                "" => plugin.id.as_str(),
                name => name,
            };
            let role = plugin.kind.name();
            (name, format!("{role} · {}", format_name(plugin.format)))
        }
        DeviceKind::Native(native) => (
            native.kind.name(),
            format!("{} · Built in", role(native.kind)),
        ),
        DeviceKind::TestGain { .. } => ("Test Gain", "Audio Effect · Built in".to_owned()),
        DeviceKind::TestInstrument { .. } => {
            ("Test Instrument", "Instrument · Built in".to_owned())
        }
    }
}

/// What a built-in device of kind `kind` is for.
fn role(kind: NativeKind) -> &'static str {
    native_kind(kind).name()
}

/// What a built-in device of kind `kind` does with notes: none is a MIDI
/// effect yet.
fn native_kind(kind: NativeKind) -> PluginKind {
    if kind.is_instrument() {
        PluginKind::Instrument
    } else {
        PluginKind::AudioEffect
    }
}

fn format_name(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
    }
}

/// Which devices the browser lists: all of them, or those of one kind,
/// the folder of the kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Kind(PluginKind),
}

impl Filter {
    fn wants(self, kind: PluginKind) -> bool {
        match self {
            Self::All => true,
            Self::Kind(wanted) => kind == wanted,
        }
    }
}

/// The browser's list of plugins.
#[derive(Debug, Default)]
pub(crate) struct Browser {
    /// What's typed in the search field.
    search: String,
    filter: Filter,
}

impl Browser {
    /// Lists Musicable's own devices that play and the plugins installed,
    /// and adds the one the user picks to `chain`'s devices: the selected
    /// track's without a chain.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        chain: Option<Chain>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        let plugins = view.plugins;
        ui.add_space(theme.devices.margin);
        ui.add(
            TextEdit::singleline(&mut self.search)
                .id(Id::unique(PLUGIN_SEARCH))
                .hint_text("Search")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal_wrapped(|ui| {
            for (filter, title) in [
                (Filter::All, "All"),
                (Filter::Kind(PluginKind::Instrument), "Instruments"),
                (Filter::Kind(PluginKind::MidiEffect), "MIDI Effects"),
                (Filter::Kind(PluginKind::AudioEffect), "Audio Effects"),
            ] {
                if ui.selectable_label(self.filter == filter, title).clicked() {
                    self.filter = filter;
                }
            }
        });
        ui.separator();
        let filter = self.filter;
        let mut built_in: Vec<(u32, NativeKind)> = (view.native.iter())
            .filter(|&&kind| filter.wants(native_kind(kind)))
            .filter_map(|&kind| Some((score(&self.search, kind.name(), kind.key(), &[])?, kind)))
            .collect();
        let mut found: Vec<(u32, &PluginEntry)> = (plugins.installed.iter())
            .filter(|p| filter.wants(p.kind))
            .filter_map(|p| {
                let label = format!("{} {}", p.name, p.vendor);
                Some((score(&self.search, &label, &p.id, &[])?, p))
            })
            .collect();
        // The best matches first, and otherwise in the order given.
        built_in.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        found.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        // A plugin installed in more than one format is listed in each, with
        // the format after its name.
        let mut formats = BTreeMap::new();
        let mut twins = BTreeSet::new();
        for p in plugins.installed {
            let key = (p.name.as_str(), p.vendor.as_str());
            if formats.insert(key, p.format).is_some_and(|f| f != p.format) {
                twins.insert(key);
            }
        }
        let to = chain_name(view.project, chain);
        let footer = ui.spacing().interact_size.y + 2.0 * theme.devices.margin;
        ScrollArea::vertical()
            .auto_shrink(false)
            .max_height((ui.available_height() - footer).max(0.0))
            .show(ui, |ui| {
                if !built_in.is_empty() {
                    ui.label(RichText::new("Built-in").strong());
                    for &(_, kind) in &built_in {
                        let row = ui.add(
                            Button::selectable(false, kind.name())
                                .min_size(vec2(ui.available_width(), 0.0))
                                .sense(Sense::click_and_drag()),
                        );
                        row.dnd_set_drag_payload(Dragged::Native(kind));
                        let row = row.on_hover_text(format!(
                            "{}\n{} · Built in\nDouble-click to add it to {to}, or drag it onto a \
                             track.",
                            kind.name(),
                            role(kind),
                        ));
                        if row.double_clicked() {
                            let args = Args::new().with("kind", kind.key());
                            actions.push(Action::RunWith(device::ADD, onto(args, chain)));
                        }
                    }
                    ui.add_space(theme.devices.margin);
                }
                ui.label(RichText::new("Plugins").strong());
                if found.is_empty() {
                    let empty = match (plugins.installed.is_empty(), plugins.scanning) {
                        (true, true) => "Looking for plugins...",
                        (true, false) => {
                            "No plugins found. Musicable looks for CLAP and VST3 \
                                          plugins where each format says they're installed."
                        }
                        (false, _) => "No plugins match.",
                    };
                    ui.add(Label::new(RichText::new(empty).weak()).wrap());
                }
                for &(_, plugin) in &found {
                    let name = if twins.contains(&(plugin.name.as_str(), plugin.vendor.as_str())) {
                        format!("{} ({})", plugin.name, format_name(plugin.format))
                    } else {
                        plugin.name.clone()
                    };
                    let row = ui.add(
                        Button::selectable(false, name)
                            .min_size(vec2(ui.available_width(), 0.0))
                            .sense(Sense::click_and_drag()),
                    );
                    row.dnd_set_drag_payload(Dragged::Plugin(plugin.clone()));
                    let row = row.on_hover_text(format!(
                        "{}{}\n{} · {}\nDouble-click to add it to {to}, or drag it onto a \
                             track.",
                        plugin.name,
                        match plugin.vendor.as_str() {
                            "" => String::new(),
                            vendor => format!(" by {vendor}"),
                        },
                        plugin.kind.name(),
                        format_name(plugin.format),
                    ));
                    if row.double_clicked() {
                        actions.push(Action::RunWith(device::ADD_PLUGIN, add(plugin, chain)));
                    }
                    row.context_menu(|ui| plugin_menu(ui, plugin, actions));
                }
            });
        ui.separator();
        ui.horizontal(|ui| {
            if plugins.scanning {
                ui.spinner();
                ui.label("Scanning...");
            } else {
                let count = match plugins.installed.len() {
                    1 => "1 plugin".to_owned(),
                    n => format!("{n} plugins"),
                };
                ui.label(RichText::new(count).weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let rescan = ui
                        .button("Rescan")
                        .on_hover_text("Looks for plugins again, and tries the ones left out");
                    if rescan.clicked() {
                        actions.push(Action::Run(musicable_commands::options::RESCAN_PLUGINS));
                    }
                });
            }
        });
    }
}

/// A plugin's menu in the browser, which marks it as an instrument, a MIDI
/// effect or an audio effect, for when the scan's rule gets it wrong, and
/// sets its Render Ahead.
fn plugin_menu(ui: &mut Ui, plugin: &PluginEntry, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Kind").weak());
    for kind in PluginKind::ALL {
        if ui.radio(plugin.kind == kind, kind.name()).clicked() {
            let args = Args::new()
                .with("format", format_key(plugin.format))
                .with("plugin", plugin.id.as_str())
                .with("kind", kind.key());
            actions.push(Action::RunWith(
                musicable_commands::browser::SET_PLUGIN_KIND,
                args,
            ));
            ui.close();
        }
    }
    ui.separator();
    render_ahead_items(ui, plugin, actions);
    ui.separator();
    ui.menu_button("Notes' Timing", |ui| {
        let items = [
            (
                "Auto",
                None,
                "As Musicable measured it, or assumes for its kind",
            ),
            (
                NoteTiming::WithSound.name(),
                Some(NoteTiming::WithSound),
                "As late as its sound, as a MIDI effect that looks ahead, or a note detector, \
                 has them",
            ),
            (
                NoteTiming::AsPlayed.name(),
                Some(NoteTiming::AsPlayed),
                "As early as what comes into it, as a synth's arpeggiator plays its steps",
            ),
        ];
        for (name, timing, tip) in items {
            if ui
                .radio(plugin.note_timing == timing, name)
                .on_hover_text(tip)
                .clicked()
            {
                let args = Args::new()
                    .with("format", format_key(plugin.format))
                    .with("plugin", plugin.id.as_str())
                    .with("timing", timing.map_or("auto", NoteTiming::key));
                actions.push(Action::RunWith(
                    musicable_commands::browser::SET_PLUGIN_NOTE_TIMING,
                    args,
                ));
                ui.close();
            }
        }
    })
    .response
    .on_hover_text(
        "When the notes the plugin plays come, against its latency, for its devices whose \
         timing nobody chose",
    );
}

/// The items of a plugin device's menu that set when the notes it plays
/// come, against its plugin's latency, `latency_ms`: Auto, which is
/// `default` and says why, With Its Sound, or As It Plays Them.
fn note_timing_items(
    ui: &mut Ui,
    device: DeviceId,
    plugin: &PluginDevice,
    latency_ms: f64,
    default: (NoteTiming, TimingFrom),
    actions: &mut Vec<Action>,
) {
    let heading = format!("Notes' Timing (latency {latency_ms:.1} ms)");
    ui.label(RichText::new(heading).weak()).on_hover_text(
        "When the notes the plugin plays come, which neither CLAP nor VST3 says. If what plays \
         them is about the plugin's latency late or early, try the other.",
    );
    let (timing, from) = default;
    let auto = format!("Auto: {} ({})", timing.name(), from.name());
    let items = [
        (auto, "auto", !plugin.note_timing_chosen),
        (
            NoteTiming::WithSound.name().to_owned(),
            NoteTiming::WithSound.key(),
            plugin.note_timing_chosen && plugin.note_timing == NoteTiming::WithSound,
        ),
        (
            NoteTiming::AsPlayed.name().to_owned(),
            NoteTiming::AsPlayed.key(),
            plugin.note_timing_chosen && plugin.note_timing == NoteTiming::AsPlayed,
        ),
    ];
    let tips = [
        "As the plugin was marked, Musicable measured it, or assumes for its kind",
        "As late as its sound, as a MIDI effect that looks ahead, or a note detector, has them: \
         the rest of the mix waits for them",
        "As early as what comes into it, as a synth's arpeggiator plays its steps as they fire",
    ];
    for ((name, key, chosen), tip) in items.into_iter().zip(tips) {
        if ui.radio(chosen, name).on_hover_text(tip).clicked() {
            let args = Args::new().with("device", device).with("timing", key);
            actions.push(Action::RunWith(device::SET_NOTE_TIMING, args));
            ui.close();
        }
    }
}

/// The items of a plugin's menus that set its Render Ahead, for every
/// project: Auto or Never.
fn render_ahead_items(ui: &mut Ui, plugin: &PluginEntry, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Render Ahead").weak());
    let items = [
        (
            "Auto",
            false,
            "Renders ahead of the audio device with the rest of the project, unless the \
             plugin says it must render live",
        ),
        (
            "Never",
            true,
            "Renders live wherever it is, as the audio device asks for each buffer, with what \
             it plays into: for a plugin that misbehaves rendered ahead, such as one that \
             talks to its other copies",
        ),
    ];
    for (name, never, tip) in items {
        if (ui.radio(plugin.never_ahead == never, name))
            .on_hover_text(tip)
            .clicked()
        {
            let args = Args::new()
                .with("format", format_key(plugin.format))
                .with("plugin", plugin.id.as_str())
                .with("render_ahead", if never { "never" } else { "auto" });
            actions.push(Action::RunWith(
                musicable_commands::browser::SET_PLUGIN_RENDER_AHEAD,
                args,
            ));
            ui.close();
        }
    }
}

/// What a command calls `format`.
fn format_key(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    }
}

/// What the browser calls `chain`, where it adds devices: the selected
/// track without a chain.
pub(crate) fn chain_name(project: &Project, chain: Option<Chain>) -> String {
    match chain {
        Some(Chain::Master) => "the master".to_owned(),
        Some(Chain::Track(id)) => match project.track(id) {
            Some(track) => track.name.clone(),
            None => "the selected track".to_owned(),
        },
        None => "the selected track".to_owned(),
    }
}

/// The arguments that add `plugin` to `chain`, or to the selected track.
pub(crate) fn add(plugin: &PluginEntry, chain: Option<Chain>) -> Args {
    let args = Args::new()
        .with("format", format_key(plugin.format))
        .with("plugin", plugin.id.as_str())
        .with("name", plugin.name.as_str())
        .with("kind", plugin.kind.key());
    onto(args, chain)
}

/// `args`, adding a device to `chain`, or to the selected track.
pub(crate) fn onto(args: Args, chain: Option<Chain>) -> Args {
    match chain {
        Some(Chain::Master) => args.with("master", true),
        Some(Chain::Track(track)) => args.with("track", track),
        None => args,
    }
}

/// What the devices panel says of notes a channel's modulators don't hear
/// ([`Project::unheard_notes`]).
fn unheard_text(project: &Project, unheard: UnheardNotes) -> String {
    let maker = project
        .find_device(unheard.maker)
        .map_or("A plugin", |(_, _, device)| device.kind.name());
    let from = unheard
        .wire
        .and_then(|id| project.wires.iter().find(|w| w.id == id))
        .and_then(|wire| project.tap_name(wire.from));
    let notes = match from {
        Some(tap) => format!("the notes from {tap}"),
        None => "the notes from this track's input".to_owned(),
    };
    format!(
        "The modulators don't hear {notes}: {maker} plays them out, and they move it, or \
         something before it."
    )
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Toggled;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{options, view, wire};
    use musicable_model::TrackKind;
    use musicable_model::mixer::Bus;
    use musicable_model::native::{NativeDevice, eq, synth, utility};
    use musicable_model::plugin::{PluginDevice, PluginInput, PluginSettings};
    use musicable_model::routing::{Port, Tap, Wire};

    use super::*;
    use crate::panels::{BROWSER, DEVICES};
    use crate::performance::Row;
    use crate::testing::{State, cursor_at, quick_harness as harness};

    fn id(name: &str) -> String {
        format!("org.example.{}", name.to_lowercase().replace(' ', "-"))
    }

    fn plugin(project: &mut Project, name: &str, kind: PluginKind) -> Device {
        project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: id(name),
            name: name.to_owned(),
            kind,
            inputs: Vec::new(),
            note_output: false,
            note_timing: musicable_model::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: PluginSettings::default(),
        }))
    }

    fn entry(name: &str, vendor: &str, kind: PluginKind) -> PluginEntry {
        PluginEntry {
            format: PluginFormat::Clap,
            id: id(name),
            name: name.to_owned(),
            vendor: vendor.to_owned(),
            kind,
            never_ahead: false,
            note_timing: None,
        }
    }

    fn on(device: &Device) -> Args {
        Args::new().with("device", device.id)
    }

    fn click(harness: &mut Harness<'static, State>, label: &str) {
        harness.get_by_label(label).click();
        harness.run();
    }

    /// Makes `chain`'s track the selected track, as clicking its header
    /// does.
    fn select(harness: &mut Harness<'static, State>, chain: Chain) {
        harness.state_mut().editor.selected_track = Some(chain);
        harness.run();
    }

    /// Double-clicks at `at`, in a window from [`quick_harness`].
    fn double_click(harness: &mut Harness<'static, State>, at: Pos2) {
        harness.hover_at(at);
        // A third of a second, so the clicks before don't make this a triple
        // click.
        for _ in 0..20 {
            harness.step();
        }
        for pressed in [true, false, true, false] {
            harness.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        harness.run();
    }

    fn actions(harness: &mut Harness<'static, State>) -> Vec<Action> {
        std::mem::take(&mut harness.state_mut().actions)
    }

    /// Opens the menu of what's called `name` and picks `item`: whether
    /// the item was ticked.
    fn menu(harness: &mut Harness<'static, State>, name: &str, item: &str) -> Option<Toggled> {
        harness.get_by_label(name).click_secondary();
        harness.run();
        let item = harness.get_by_role_and_label(Role::Button, item);
        let ticked = item.accesskit_node().toggled();
        item.click();
        harness.run();
        ticked
    }

    fn native(project: &mut Project, kind: NativeKind) -> Device {
        project.new_device(DeviceKind::Native(NativeDevice::new(kind)))
    }

    /// Sets parameter `param` of `device` to `value`.
    fn set(device: &Device, param: usize, value: f64) -> Action {
        let args = on(device).with("param", param as i64).with("value", value);
        Action::RunWith(device::SET_PARAM, args)
    }

    /// The harness, showing `devices` on a track of their own.
    fn showing(devices: Vec<Device>, project: Project) -> Harness<'static, State> {
        let mut project = project;
        let track = project.add_track("Keys", TrackKind::Midi);
        project.track_mut(track).unwrap().devices = devices;
        let mut harness = harness(project);
        harness.state_mut().editor.selected_track = Some(Chain::Track(track));
        harness.run();
        harness
    }

    #[test]
    fn the_panel_shows_the_selected_tracks_devices() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let synth = plugin(&mut project, "Surge XT", PluginKind::Instrument);
        let reverb = plugin(&mut project, "Old Reverb", PluginKind::AudioEffect);
        let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let limiter = plugin(&mut project, "Limiter", PluginKind::AudioEffect);
        (project.track_mut(keys).unwrap().devices).extend([synth.clone(), reverb.clone()]);
        project.master.devices.extend([gain, limiter]);
        let mut harness = harness(project);
        harness.get_by_label(NO_TRACK);

        // Selecting a track shows its devices, in the order they play, with
        // how their plugins are doing.
        let missing = "Old Reverb isn't installed, or Musicable hasn't found it: it plays nothing";
        let state = harness.state_mut();
        state.editor.selected_track = Some(Chain::Track(keys));
        let loaded = PluginStatus {
            loaded: true,
            has_window: true,
            ..PluginStatus::default()
        };
        state.plugin_status.insert(synth.id, loaded.clone());
        let failed = PluginStatus {
            problem: Some(missing.to_owned()),
            ..PluginStatus::default()
        };
        state.plugin_status.insert(reverb.id, failed);
        harness.run();
        let left =
            |harness: &Harness<'static, State>, label| harness.get_by_label(label).rect().left();
        assert!(left(&harness, "Device Surge XT") < left(&harness, "Device Old Reverb"));
        harness.get_by_label("Instrument · CLAP");
        harness.get_by_label(missing);
        assert!(harness.query_by_label("Old Reverb Plugin Window").is_none());

        // The window button shows the plugin's window, and closes it once
        // it's open. A double-click on the name shows it too, as in Live.
        click(&mut harness, "Surge XT Plugin Window");
        let state = harness.state_mut();
        state.plugin_status.get_mut(&synth.id).unwrap().window_open = true;
        harness.run();
        click(&mut harness, "Surge XT Plugin Window");
        let at = harness.get_by_label("Device Surge XT").rect().center();
        double_click(&mut harness, at);
        click(&mut harness, "Delete Old Reverb");
        // Each click of the double-click selects it, as one click does.
        let selects = Action::RunWith(device::SELECT, Args::new().with("devices", vec![synth.id]));
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::SHOW_WINDOW, on(&synth)),
                Action::RunWith(device::HIDE_WINDOW, on(&synth)),
                selects.clone(),
                selects,
                Action::RunWith(device::SHOW_WINDOW, on(&synth)),
                Action::RunWith(device::DELETE, on(&reverb)),
            ]
        );

        // The master's, while the master track is selected: the panel says
        // whose devices they are, and has no buttons to pick another's.
        harness.get_by_role_and_label(Role::Label, "Devices of Keys");
        select(&mut harness, Chain::Master);
        harness.get_by_role_and_label(Role::Label, "Devices of Master");
        assert!(harness.query_by_label("Devices of Keys").is_none());
        assert!(harness.query_by_label("Device Surge XT").is_none());
        harness.get_by_label("Device Test Gain");
        harness.get_by_label("Not loaded");
        harness.state_mut().scanning = true;
        harness.run();
        harness.get_by_label("Waiting for the plugin scan");
        select(&mut harness, Chain::Track(keys));
        harness.get_by_label("Device Surge XT");
        select(&mut harness, Chain::Track(bass));
        harness.get_by_label("Devices of Bass");
        harness.get_by_label(NO_DEVICES);

        // Add Device shows the browser.
        harness.state_mut().scanning = false;
        assert!(harness.query_by_label("Plugins").is_none());
        click(&mut harness, "Add Device...");
        harness.get_by_label("Plugins");
        assert!(actions(&mut harness).is_empty());
    }

    #[test]
    fn a_plugin_devices_menu_sets_when_its_notes_come_where_it_matters() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let arp = plugin(&mut project, "Humanize", PluginKind::MidiEffect);
        let synth = plugin(&mut project, "Surge XT", PluginKind::Instrument);
        (project.track_mut(keys).unwrap().devices).extend([arp.clone(), synth.clone()]);
        let mut harness = harness(project);
        let state = harness.state_mut();
        state.editor.selected_track = Some(Chain::Track(keys));
        let latent = PluginStatus {
            loaded: true,
            latency_ms: 20.0,
            note_timing: Some((NoteTiming::WithSound, TimingFrom::Measured)),
            ..PluginStatus::default()
        };
        state.plugin_status.insert(arp.id, latent);
        state
            .plugin_status
            .insert(synth.id, PluginStatus::default());
        harness.run();
        // Auto says what it is, and why, until someone chooses.
        let pick = |harness: &mut Harness<'static, State>, item: &str| {
            harness.get_by_label("Device Humanize").click_secondary();
            harness.run();
            harness.get_by_label(item).click();
            harness.run();
        };
        pick(&mut harness, "Auto: With Its Sound (measured)");
        pick(&mut harness, "As It Plays Them");
        let timing = |timing: &str| {
            let args = Args::new().with("device", arp.id).with("timing", timing);
            Action::RunWith(device::SET_NOTE_TIMING, args)
        };
        assert_eq!(
            actions(&mut harness),
            [
                super::select(vec![arp.id], false),
                timing("auto"),
                super::select(vec![arp.id], false),
                timing("as_played"),
            ]
        );
        // Where it doesn't matter, the menu doesn't say.
        harness.get_by_label("Device Surge XT").click_secondary();
        harness.run();
        assert!(harness.query_by_label_contains("Notes' Timing").is_none());
    }

    #[test]
    fn the_panel_says_which_notes_the_modulators_cant_hear() {
        // The Keys' synth plays out notes, which a note wire plays on the
        // master's synth, and the master's Envelope, which hears what that
        // plays, moves the Keys' synth.
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut echo = plugin(&mut project, "Echo", PluginKind::Instrument);
        if let DeviceKind::Plugin(plugin) = &mut echo.kind {
            plugin.note_output = true;
        }
        let master_synth =
            project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Synth)));
        let wire = project.new_wire(Tap::Device(echo.id), Port::Notes(master_synth.id));
        project.wires.push(wire);
        let envelope = project.new_modulator(
            musicable_model::modulation::ModulatorKind::Envelope,
            "Envelope 1",
        );
        let output = musicable_model::modulation::Output::new(envelope.id, 0);
        let moves = project.new_routing(
            output,
            musicable_model::param::ParamAddress::Device(echo.id, 3),
            0.5,
        );
        project.master.modulation.modulators.push(envelope);
        project.master.modulation.routings.push(moves);
        project.track_mut(keys).unwrap().devices.push(echo);
        project.master.devices.push(master_synth);
        let mut harness = harness(project);
        harness.state_mut().editor.selected_track = Some(Chain::Master);
        harness.run();
        harness.get_by_label(
            "The modulators don't hear the notes from Keys after Echo: Echo plays them out, and \
             they move it, or something before it.",
        );
        // The Keys' own devices say nothing of it.
        harness.state_mut().editor.selected_track = Some(Chain::Track(keys));
        harness.run();
        assert!(harness.query_by_label_contains("don't hear").is_none());
    }

    #[test]
    fn what_the_empty_panel_says_takes_the_text_cursor_only_over_its_words() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        // Under Add Device, in the room the words are in the middle of.
        let add = harness.get_by_label("Add Device...").rect();
        let beside = add.right_bottom() + vec2(-5.0, 30.0);
        for (words, chain) in [(NO_TRACK, None), (NO_DEVICES, Some(Chain::Track(keys)))] {
            harness.state_mut().editor.selected_track = chain;
            harness.run();
            let shown = harness.get_by_label(words).rect();
            assert_eq!(cursor_at(&mut harness, shown.center()), CursorIcon::Text);
            let cursor = cursor_at(&mut harness, beside);
            assert_eq!(cursor, CursorIcon::Default, "{words}");
        }
    }

    #[test]
    fn the_browser_finds_plugins_and_adds_them() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        let state = harness.state_mut();
        state.plugins = vec![
            entry(
                "Dragonfly Hall Reverb",
                "Michael Willis",
                PluginKind::AudioEffect,
            ),
            entry("Note Arp", "Example", PluginKind::MidiEffect),
            entry("Surge XT", "Surge Synth Team", PluginKind::Instrument),
            entry("Vital", "Vital Audio", PluginKind::Instrument),
        ];
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        let names = ["Dragonfly Hall Reverb", "Note Arp", "Surge XT", "Vital"];
        let tops: Vec<f32> = (names.iter())
            .map(|name| harness.get_by_label(name).rect().top())
            .collect();
        assert!(tops.is_sorted(), "in the order given: {tops:?}");
        harness.get_by_label("4 plugins");

        // Picking a kind's folder, and searching, by name or by vendor.
        let listed = |harness: &Harness<'static, State>| -> Vec<&str> {
            (names.into_iter())
                .filter(|name| harness.query_by_label(name).is_some())
                .collect()
        };
        click(&mut harness, "Audio Effects");
        assert_eq!(listed(&harness), ["Dragonfly Hall Reverb"]);
        click(&mut harness, "MIDI Effects");
        assert_eq!(listed(&harness), ["Note Arp"]);
        // No built-in device is a MIDI effect.
        assert!(harness.query_by_label("Built-in").is_none());
        click(&mut harness, "Instruments");
        assert_eq!(listed(&harness), ["Surge XT", "Vital"]);
        click(&mut harness, "All");
        harness.get_by_role(Role::TextInput).click();
        harness.run();
        harness.get_by_role(Role::TextInput).type_text("synth team");
        harness.run();
        assert_eq!(listed(&harness), ["Surge XT"]);
        harness.get_by_role(Role::TextInput).type_text(" nothing");
        harness.run();
        harness.get_by_label("No plugins match.");

        // A double-click adds the plugin to the selected track.
        let mut harness = self::harness(Project::new());
        let state = harness.state_mut();
        state.project.add_track("Keys", TrackKind::Midi);
        state.plugins = vec![entry("Vital", "Vital Audio", PluginKind::Instrument)];
        state.editor.selected_track = Some(Chain::Track(keys));
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        let at = harness.get_by_label("Vital").rect().center();
        double_click(&mut harness, at);
        // Or to the master, while the master track is selected.
        select(&mut harness, Chain::Master);
        double_click(&mut harness, at);
        click(&mut harness, "Rescan");
        let add = Args::new()
            .with("format", "clap")
            .with("plugin", "org.example.vital")
            .with("name", "Vital")
            .with("kind", "instrument");
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::ADD_PLUGIN, add.clone().with("track", keys)),
                Action::RunWith(device::ADD_PLUGIN, add.with("master", true)),
                Action::Run(options::RESCAN_PLUGINS),
            ]
        );

        // Before a scan has found anything.
        let state = harness.state_mut();
        state.plugins.clear();
        state.scanning = true;
        // The spinner keeps it drawing.
        harness.run_steps(2);
        harness.get_by_label("Looking for plugins...");
        harness.get_by_label("Scanning...");
        harness.state_mut().scanning = false;
        harness.run();
        harness.get_by_label_contains("No plugins found.");
        harness.get_by_label("0 plugins");
    }

    #[test]
    fn a_plugin_in_both_formats_is_listed_in_each_with_its_format() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        let state = harness.state_mut();
        let surge = entry("Surge XT", "Surge Synth Team", PluginKind::Instrument);
        state.plugins = vec![
            surge.clone(),
            PluginEntry {
                format: PluginFormat::Vst3,
                id: "D07A3E8BF8E64BB8A1A2B3C4D5E6F708".to_owned(),
                ..surge
            },
            entry("Vital", "Vital Audio", PluginKind::Instrument),
        ];
        state.editor.selected_track = Some(Chain::Track(keys));
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        harness.get_by_label("Surge XT (CLAP)");
        harness.get_by_label("Vital");
        harness.get_by_label("3 plugins");

        // Each adds its own, by the name the plugin has.
        let at = harness.get_by_label("Surge XT (VST3)").rect().center();
        double_click(&mut harness, at);
        let add = Args::new()
            .with("format", "vst3")
            .with("plugin", "D07A3E8BF8E64BB8A1A2B3C4D5E6F708")
            .with("name", "Surge XT")
            .with("kind", "instrument")
            .with("track", keys);
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(device::ADD_PLUGIN, add)]
        );
    }

    #[test]
    fn the_view_menu_shows_and_hides_the_panels() {
        let mut harness = harness(Project::new());
        click(&mut harness, "View");
        let devices = harness.get_by_label("Devices Ctrl+Alt+L");
        assert_eq!(devices.accesskit_node().toggled(), Some(Toggled::True));
        let browser = harness.get_by_label("Browser Ctrl+Alt+B");
        assert_eq!(browser.accesskit_node().toggled(), Some(Toggled::False));
        browser.click();
        harness.run();
        assert_eq!(actions(&mut harness), [Action::Run(view::TOGGLE_BROWSER)]);
        // The app runs the command, which asks for this.
        harness.get_by_label(NO_TRACK);
        harness.state_mut().shell.toggle_panel(DEVICES);
        harness.run();
        assert!(harness.query_by_label(NO_TRACK).is_none());
    }

    #[test]
    fn built_in_devices_have_a_control_for_each_parameter() {
        let mut project = Project::new();
        let mut utility = native(&mut project, NativeKind::Utility);
        utility.kind.native_mut().unwrap().values[utility::GAIN] = 0.5;
        let eq = native(&mut project, NativeKind::Eq);
        let mut harness = showing(vec![utility.clone(), eq.clone()], project);
        // Named for screen readers after the device and the parameter, in
        // columns under their groups' names.
        for info in NativeKind::Eq.params() {
            harness.get_by_label(&format!("EQ {}", info.full_name()));
        }
        harness.get_by_label("Band 8");
        let left =
            |harness: &Harness<'static, State>, label| harness.get_by_label(label).rect().left();
        assert!(left(&harness, "EQ Band 1 Freq") < left(&harness, "EQ Band 2 Freq"));
        let freq = harness.get_by_role_and_label(Role::Slider, "EQ Band 3 Freq");
        assert_eq!(freq.accesskit_node().numeric_value(), Some(250.0));
        let on = harness
            .get_by_label("EQ Band 1 On")
            .accesskit_node()
            .toggled();
        assert_eq!(on, Some(Toggled::False));

        // Switches turn on and off, menus pick a choice, and screen readers
        // set a number.
        click(&mut harness, "EQ Band 1 On");
        click(&mut harness, "Utility Mute");
        harness
            .get_by_role_and_label(Role::ComboBox, "EQ Band 3 Shape")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Notch").click();
        harness.run();
        let freq = harness.get_by_role_and_label(Role::Slider, "EQ Band 3 Freq");
        freq.set_value_accesskit(1000.0);
        harness.run();
        // A double-click gives the default.
        let at = harness.get_by_label("Utility Gain").rect().center();
        double_click(&mut harness, at);
        // Typing a value, in the right-click menu.
        harness
            .get_by_role_and_label(Role::Slider, "EQ Band 2 Freq")
            .click_secondary();
        harness.run();
        harness.get_by_role(Role::TextInput).type_text("1.2k");
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();
        assert!(harness.query_by_role(Role::TextInput).is_none());
        let band = |band: usize, param: usize| band * eq::PER_BAND + param;
        assert_eq!(
            actions(&mut harness),
            [
                set(&eq, eq::ON, 1.0),
                set(&utility, utility::MUTE, 1.0),
                set(&eq, band(2, eq::SHAPE), 5.0),
                set(&eq, band(2, eq::FREQ), 1000.0),
                set(&utility, utility::GAIN, 1.0),
                set(&eq, band(1, eq::FREQ), 1200.0),
            ]
        );
    }

    #[test]
    fn each_device_shows_its_share_of_a_core() {
        let mut project = Project::new();
        let reverb = plugin(&mut project, "Old Reverb", PluginKind::AudioEffect);
        let mut gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let synth = plugin(&mut project, "Surge XT", PluginKind::Instrument);
        gain.active = false;
        let devices = vec![synth.clone(), reverb.clone(), gain.clone()];
        let mut harness = showing(devices, project);
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.run();
        // Without figures, none shows.
        assert!(harness.query_by_label("Old Reverb CPU").is_none());
        let cpu = |average, longest| Cpu {
            average,
            longest,
            ahead: false,
        };
        harness.state_mut().performance.rows = BTreeMap::from([
            (Row::Device(synth.id), cpu(0.6, 0.9)),
            (Row::Device(reverb.id), cpu(0.12, 0.2)),
            (Row::Device(gain.id), cpu(0.0, 0.0)),
        ]);
        harness.run();
        let figure = |harness: &Harness<'static, State>, label| {
            harness.get_by_role_and_label(Role::Meter, label).value()
        };
        assert_eq!(figure(&harness, "Surge XT CPU").as_deref(), Some("60%"));
        assert_eq!(figure(&harness, "Old Reverb CPU").as_deref(), Some("12%"));
        // In the title bar, right of the name.
        let name = harness.get_by_label("Device Old Reverb").rect();
        let at = harness.get_by_label("Old Reverb CPU").rect();
        assert!(at.left() >= name.right() - 0.5 && (at.center().y - name.center().y).abs() < 4.0);
        // A deactivated device takes no time.
        assert!(harness.query_by_label("Test Gain CPU").is_none());
    }

    #[test]
    fn the_bypass_button_bypasses_and_alt_click_deactivates() {
        let mut project = Project::new();
        let mut eq = native(&mut project, NativeKind::Eq);
        eq.bypassed = true;
        let reverb = plugin(&mut project, "Old Reverb", PluginKind::AudioEffect);
        let mut gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        gain.active = false;
        let devices = vec![eq.clone(), reverb.clone(), gain.clone()];
        let mut harness = showing(devices, project);
        // Wide enough for all three.
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.run();
        let bypass = |device: &Device, bypassed: bool| {
            let args = Args::new()
                .with("devices", vec![device.id])
                .with("bypassed", bypassed);
            Action::RunWith(device::SET_BYPASS, args)
        };
        let active = |device: &Device, active: bool| {
            let args = Args::new()
                .with("devices", vec![device.id])
                .with("active", active);
            Action::RunWith(device::SET_ACTIVE, args)
        };
        // Left of the name, which says whether the device is off. Screen
        // readers hear the button on while the device is bypassed.
        let button = harness.get_by_label("Old Reverb Bypass").rect();
        let name = harness.get_by_label("Device Old Reverb").rect();
        assert!(button.right() <= name.left() && (button.center().y - name.center().y).abs() < 2.0);
        let toggled = |harness: &Harness<'static, State>, label: &str| {
            harness.get_by_label(label).accesskit_node().toggled()
        };
        assert_eq!(toggled(&harness, "EQ Bypass"), Some(Toggled::True));
        assert_eq!(toggled(&harness, "Old Reverb Bypass"), Some(Toggled::False));
        assert_eq!(toggled(&harness, "Test Gain Bypass"), Some(Toggled::False));
        harness.get_by_label("Device EQ, bypassed");
        harness.get_by_label("Device Test Gain, deactivated");
        harness.get_by_label("Deactivated");

        // A click bypasses a device or brings it back, and Alt-click
        // deactivates it. A deactivated device's button activates it.
        click(&mut harness, "EQ Bypass");
        click(&mut harness, "Old Reverb Bypass");
        harness
            .get_by_label("Old Reverb Bypass")
            .click_modifiers(Modifiers::ALT);
        harness.run();
        click(&mut harness, "Test Gain Bypass");
        harness
            .get_by_label("Test Gain Bypass")
            .click_modifiers(Modifiers::ALT);
        harness.run();
        // A bypassed device's controls still work.
        click(&mut harness, "EQ Band 1 On");
        assert_eq!(
            actions(&mut harness),
            [
                bypass(&eq, false),
                bypass(&reverb, true),
                active(&reverb, false),
                active(&gain, true),
                active(&gain, true),
                set(&eq, eq::ON, 1.0),
            ]
        );

        // The name's menu has them too, with Bypass ticked while the device
        // is bypassed, and its key. Opening it selects the device.
        let ticked = menu(&mut harness, "Device EQ, bypassed", "Bypass 0");
        assert_eq!(ticked, Some(Toggled::True));
        let ticked = menu(&mut harness, "Device Old Reverb", "Bypass 0");
        assert_eq!(ticked, Some(Toggled::False));
        menu(&mut harness, "Device Old Reverb", "Deactivate");
        menu(&mut harness, "Device Test Gain, deactivated", "Activate");
        assert_eq!(
            actions(&mut harness),
            [
                super::select(vec![eq.id], false),
                bypass(&eq, false),
                super::select(vec![reverb.id], false),
                bypass(&reverb, true),
                super::select(vec![reverb.id], false),
                active(&reverb, false),
                super::select(vec![gain.id], false),
                active(&gain, true),
            ]
        );
    }

    #[test]
    fn the_mouse_map_says_what_the_bypass_button_does() {
        let mut project = Project::new();
        let utility = native(&mut project, NativeKind::Utility);
        let mut harness = showing(vec![utility.clone()], project);
        let mut mouse = MouseMap::live();
        let (button, click) = (Target::BypassButton, Gesture::Click);
        let none = musicable_commands::Modifiers::NONE;
        mouse.bind(button, click, none, MouseAction::ToggleActive);
        mouse.unbind(button, click, musicable_commands::Modifiers::ALT);
        harness.state_mut().shell.set_mouse_map(mouse);
        harness.run();
        harness.get_by_label("Utility Bypass").click();
        harness.run();
        // Alt does nothing of its own now, so it clicks as nothing held does.
        harness
            .get_by_label("Utility Bypass")
            .click_modifiers(Modifiers::ALT);
        harness.run();
        let args = Args::new()
            .with("devices", vec![utility.id])
            .with("active", false);
        let deactivate = Action::RunWith(device::SET_ACTIVE, args);
        assert_eq!(actions(&mut harness), [deactivate.clone(), deactivate]);
    }

    #[test]
    fn a_sampler_shows_its_sample_and_loads_another() {
        let mut project = Project::new();
        let mut kick = native(&mut project, NativeKind::Sampler);
        kick.kind.native_mut().unwrap().sample = Some("Samples/kick.wav".to_owned());
        let empty = native(&mut project, NativeKind::Sampler);
        let synth = native(&mut project, NativeKind::Synth);
        let devices = vec![kick.clone(), empty.clone(), synth];
        let mut harness = showing(devices, project);
        // Wide enough for both samplers.
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.run();
        // By the file's own name, beside the device's.
        let shown = harness.get_by_label("Sampler Sample: kick.wav").rect();
        let title = harness
            .get_all_by_label("Device Sampler")
            .next()
            .unwrap()
            .rect();
        assert!(title.right() < shown.left() && (shown.center().y - title.center().y).abs() < 2.0);
        harness.get_by_label("Sampler Sample: No sample");
        assert!(harness.query_by_label("Synth Load Sample").is_none());

        // The second sampler's.
        let load = |harness: &Harness<'static, State>| {
            let mut buttons = harness.get_all_by_label("Sampler Load Sample");
            buttons.nth(1).unwrap().click();
        };
        load(&harness);
        harness.run();
        harness
            .get_by_label("Sampler Sample: kick.wav")
            .click_secondary();
        harness.run();
        click(&mut harness, "Remove Sample");
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::LOAD_SAMPLE, on(&empty)),
                Action::RunWith(device::SET_SAMPLE, on(&kick)),
            ]
        );
    }

    #[test]
    fn a_sample_dragged_onto_a_sampler_loads_into_it() {
        let mut project = Project::new();
        let sampler = native(&mut project, NativeKind::Sampler);
        let synth = native(&mut project, NativeKind::Synth);
        let mut harness = showing(vec![sampler.clone(), synth], project);
        let keys = harness.state().project.tracks[0].id;
        // Wide enough for both.
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.run();
        let drop_on = |harness: &mut Harness<'static, State>, label: &str, dragged| {
            let at = harness.get_by_label(label).rect().center();
            egui::DragAndDrop::set_payload(&harness.ctx, dragged);
            harness.hover_at(at);
            harness.run();
            harness.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            harness.run();
        };

        // Let go over the sampler, it's the sampler's.
        let kick = std::path::PathBuf::from("/samples/kick.wav");
        drop_on(
            &mut harness,
            "Sampler Sample: No sample",
            Dragged::Sample(kick.clone()),
        );
        let load = Action::LoadSample {
            device: sampler.id,
            file: kick.clone(),
        };
        assert_eq!(actions(&mut harness), [load]);
        // Over the synth, it goes nowhere.
        drop_on(&mut harness, "Device Synth", Dragged::Sample(kick));
        assert_eq!(actions(&mut harness), []);
        // A device let go over the sampler goes in the chain, as anywhere in
        // the panel: in front of the sampler, left of its middle.
        drop_on(
            &mut harness,
            "Device Sampler",
            Dragged::Native(NativeKind::Eq),
        );
        let args = Args::new().with("kind", "eq").with("track", keys);
        let args = args.with("index", 0_i64);
        assert_eq!(actions(&mut harness), [Action::RunWith(device::ADD, args)]);
    }

    /// The command that selects `devices`, or toggles them.
    fn selects(devices: &[&Device], toggle: bool) -> Action {
        super::select(devices.iter().map(|device| device.id).collect(), toggle)
    }

    /// Presses the mouse's button at `at` with `mods` held, or lets go.
    fn press(harness: &mut Harness<'static, State>, at: Pos2, pressed: bool, mods: Modifiers) {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: mods,
        });
        harness.run();
    }

    /// Drags from `from` towards `to` with `mods` held, in a few moves,
    /// without letting go.
    fn drag_to(harness: &mut Harness<'static, State>, from: Pos2, to: Pos2, mods: Modifiers) {
        harness.event(Event::ModifiersChanged(mods));
        harness.hover_at(from);
        harness.run();
        press(harness, from, true, mods);
        for step in 1..=4 {
            harness.hover_at(from + (to - from) * step as f32 / 4.0);
            harness.run();
        }
    }

    /// Drags from `from` to `to` with `mods` held, and lets go there.
    fn drag(harness: &mut Harness<'static, State>, from: Pos2, to: Pos2, mods: Modifiers) {
        drag_to(harness, from, to, mods);
        press(harness, to, false, mods);
        harness.event(Event::ModifiersChanged(Modifiers::NONE));
        harness.run();
    }

    #[test]
    fn clicks_select_devices_and_a_click_beside_them_selects_none() {
        let mut project = Project::new();
        let first = plugin(&mut project, "First", PluginKind::AudioEffect);
        let second = plugin(&mut project, "Second", PluginKind::AudioEffect);
        let mut harness = showing(vec![first.clone(), second.clone()], project);
        // A click on a title bar selects its device alone, and Ctrl- or
        // Shift-click adds it to the selection, or takes it out. A click in
        // its box where no control is does as one on its title bar.
        click(&mut harness, "Device First");
        for mods in [Modifiers::CTRL, Modifiers::SHIFT] {
            harness.get_by_label("Device Second").click_modifiers(mods);
            harness.run();
        }
        let kind = harness
            .get_all_by_label("Audio Effect · CLAP")
            .nth(1)
            .unwrap();
        let kind = kind.rect().center();
        crate::testing::click_at(&mut harness, kind);
        assert_eq!(
            actions(&mut harness),
            [
                selects(&[&first], false),
                selects(&[&second], true),
                selects(&[&second], true),
                selects(&[&second], false),
            ]
        );

        // A selected device's box is outlined, and screen readers hear it's
        // selected. A click beside the boxes selects none of them, if any
        // are selected.
        harness.state_mut().editor.devices.selected = [second.id].into();
        harness.run();
        harness.get_by_label("Device First");
        let title = harness.get_by_label("Device Second, selected").rect();
        let beside = pos2(title.right() + 200.0, title.center().y);
        crate::testing::click_at(&mut harness, beside);
        assert_eq!(actions(&mut harness), [selects(&[], false)]);
        harness.state_mut().editor.devices.selected.clear();
        harness.run();
        crate::testing::click_at(&mut harness, beside);
        assert_eq!(actions(&mut harness), []);
    }

    #[test]
    fn title_bars_drag_devices_along_the_chain_and_ctrl_copies_them() {
        let mut project = Project::new();
        let [a, b, c] =
            ["A", "B", "C"].map(|name| plugin(&mut project, name, PluginKind::AudioEffect));
        let mut harness = showing(vec![a.clone(), b.clone(), c.clone()], project);
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.run();
        let keys = harness.state().project.tracks[0].id;
        let title = |harness: &Harness<'static, State>, name: &str| {
            let label = match harness.state().editor.devices.selected.is_empty() {
                true => format!("Device {name}"),
                false => format!("Device {name}, selected"),
            };
            harness.get_by_label(&label).rect()
        };
        let to = |command, devices: &[&Device], index: i64| {
            let devices: Vec<DeviceId> = devices.iter().map(|device| device.id).collect();
            let args = Args::new().with("devices", devices).with("track", keys);
            Action::RunWith(command, args.with("index", index))
        };

        // Let go right of the middle of the last, a device goes after it,
        // so it's third in the chain without it. Dragging it selects it.
        let from = title(&harness, "A").center();
        let after_c = title(&harness, "C").right_center() + vec2(4.0, 0.0);
        drag(&mut harness, from, after_c, Modifiers::NONE);
        assert_eq!(
            actions(&mut harness),
            [selects(&[&a], false), to(device::MOVE, &[&a], 2)]
        );

        // A selected device drags the selection, all of it, in the order
        // they play: in front of the first.
        harness.state_mut().editor.devices.selected = [a.id, b.id].into();
        harness.run();
        let from = title(&harness, "B").center();
        let before_a = title(&harness, "A").left_center() - vec2(4.0, 0.0);
        drag(&mut harness, from, before_a, Modifiers::NONE);
        assert_eq!(actions(&mut harness), [to(device::MOVE, &[&a, &b], 0)]);
        harness.state_mut().editor.devices.selected.clear();
        harness.run();

        // Ctrl copies, with a line where the copy would go, and the pointer
        // says so.
        let from = title(&harness, "C").center();
        let after_a = title(&harness, "A").right_center() + vec2(4.0, 0.0);
        drag_to(&mut harness, from, after_a, Modifiers::CTRL);
        let dragged = egui::DragAndDrop::payload::<Dragged>(&harness.ctx).unwrap();
        let copy = Dragged::Devices {
            devices: vec![c.id],
            copy: true,
            name: "C".to_owned(),
        };
        assert_eq!((*dragged).clone(), copy);
        assert_eq!(copy.name(), "Copy C");
        let start_marker = harness.state().shell.theme().start_marker;
        let (left, right) = (title(&harness, "A").right(), title(&harness, "B").left());
        let lines = (harness.output().shapes.iter()).filter(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke } => {
                stroke.color == start_marker && (left..right).contains(&points[0].x)
            }
            _ => false,
        });
        assert_eq!(lines.count(), 1);
        press(&mut harness, after_a, false, Modifiers::CTRL);
        assert_eq!(
            actions(&mut harness),
            [selects(&[&c], false), to(device::COPY, &[&c], 1)]
        );

        // Escape stops a drag: nothing comes of letting go.
        drag_to(&mut harness, from, after_a, Modifiers::NONE);
        harness.key_press(Key::Escape);
        harness.run();
        press(&mut harness, after_a, false, Modifiers::NONE);
        assert_eq!(actions(&mut harness), [selects(&[&c], false)]);
        assert!(egui::DragAndDrop::payload::<Dragged>(&harness.ctx).is_none());
    }

    #[test]
    fn devices_dragged_onto_tracks_and_strips_go_on_them() {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Audio);
        let a = plugin(&mut project, "A", PluginKind::AudioEffect);
        let mut harness = showing(vec![a.clone()], project);
        harness.set_size(egui::vec2(1800.0, 900.0));
        harness.run();
        let moved = |args: Args| {
            let args = args.with("devices", vec![a.id]);
            Action::RunWith(device::MOVE, args)
        };
        let from = harness.get_by_label("Device A").rect().center();

        // Onto a track's row in the arrangement, at the end of its chain.
        let row = harness.get_by_label("Track Bass").rect();
        drag(&mut harness, from, row.center(), Modifiers::NONE);
        let selected = selects(&[&a], false);
        let onto_bass = moved(Args::new().with("track", bass));
        assert_eq!(actions(&mut harness), [selected.clone(), onto_bass.clone()]);
        // Under the tracks, onto a track of its own.
        let master = harness.get_by_label("Master track").rect();
        let keys = harness.get_by_label("Track Keys").rect();
        let below = pos2(row.center().x, (keys.bottom() + master.top()) / 2.0);
        drag(&mut harness, from, below, Modifiers::NONE);
        let new_track = moved(Args::new().with("new_track", true));
        assert_eq!(actions(&mut harness), [selected.clone(), new_track]);
        // Onto the master track, or a mixer strip.
        drag(&mut harness, from, master.center(), Modifiers::NONE);
        let strip = harness.get_by_label("Bass Strip").rect();
        drag(&mut harness, from, strip.center(), Modifiers::NONE);
        assert_eq!(
            actions(&mut harness),
            [
                selected.clone(),
                moved(Args::new().with("master", true)),
                selected,
                onto_bass,
            ]
        );
    }

    #[test]
    fn a_devices_menu_works_on_the_selection_if_its_in_it() {
        let mut project = Project::new();
        let [mut a, b, c] =
            ["A", "B", "C"].map(|name| plugin(&mut project, name, PluginKind::AudioEffect));
        a.bypassed = true;
        let mut harness = showing(vec![a.clone(), b.clone(), c.clone()], project);
        harness.set_size(egui::vec2(1800.0, 600.0));
        harness.state_mut().editor.devices.selected = [a.id, c.id].into();
        harness.run();
        let run = |command| Action::Run(command);

        // On the selection, of which one is bypassed: Bypass bypasses both.
        let ticked = menu(&mut harness, "Device A, bypassed, selected", "Bypass 0");
        assert_eq!(ticked, Some(Toggled::False));
        menu(&mut harness, "Device C, selected", "Cut Ctrl+X");
        menu(
            &mut harness,
            "Device A, bypassed, selected",
            "Delete Backspace, Delete",
        );
        let both = Args::new().with("devices", vec![a.id, c.id]);
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::SET_BYPASS, both.with("bypassed", true)),
                selects(&[&a, &c], false),
                run(edit::CUT),
                selects(&[&a, &c], false),
                run(edit::DELETE),
            ]
        );

        // On a device that isn't selected, it alone, which it selects.
        // Paste waits for something cut or copied.
        harness.get_by_label("Device B").click_secondary();
        harness.run();
        let paste = harness.get_by_role_and_label(Role::Button, "Paste Ctrl+V");
        assert!(paste.accesskit_node().is_disabled());
        harness.key_press(Key::Escape);
        harness.run();
        let clipboard = musicable_commands::editor::DeviceClipboard {
            devices: Vec::new(),
        };
        harness.state_mut().editor.devices.clipboard = Some(clipboard);
        harness.run();
        menu(&mut harness, "Device B", "Paste Ctrl+V");
        menu(&mut harness, "Device B", "Duplicate Ctrl+D");
        assert_eq!(
            actions(&mut harness),
            [
                selects(&[&b], false),
                selects(&[&b], false),
                selects(&[&b], false),
                run(edit::PASTE),
                selects(&[&b], false),
                selects(&[&b], false),
                run(edit::DUPLICATE),
            ]
        );
    }

    #[test]
    fn dragging_a_control_is_one_gesture_in_whole_steps_where_it_has_them() {
        let mut project = Project::new();
        let synth = native(&mut project, NativeKind::Synth);
        let mut harness = showing(vec![synth.clone()], project);
        let voices = harness.get_by_role_and_label(Role::Slider, "Synth Voices");
        let at = voices.rect().left_center() + egui::vec2(4.0, 0.0);
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        // Each move is less than a step from the one before: they add up.
        for x in 1..=10 {
            harness.hover_at(at + egui::vec2(3.0 * x as f32, 0.0));
            harness.run();
        }
        harness.drop_at(at + egui::vec2(30.0, 0.0));
        harness.run();
        let actions = actions(&mut harness);
        assert_eq!(actions.first(), Some(&Action::BeginGesture));
        assert_eq!(actions.last(), Some(&Action::EndGesture));
        let values: Vec<f64> = actions[1..actions.len() - 1]
            .iter()
            .map(|action| match action {
                Action::RunWith(id, args) if *id == device::SET_PARAM => {
                    assert_eq!(args.get::<i64>("param"), Ok(synth::VOICES as i64));
                    args.get("value").unwrap()
                }
                other => panic!("{other:?} in the middle of a drag"),
            })
            .collect();
        assert!(
            values.iter().all(|value| value.fract() == 0.0),
            "{values:?}"
        );
        let last = *values.last().unwrap();
        // The synth starts at eight voices, and the drag moved it a third
        // of the way along.
        assert!(
            values.len() > 3 && (16.0..=20.0).contains(&last),
            "{values:?}"
        );
    }

    #[test]
    fn automated_controls_follow_their_lanes_and_offer_their_commands() {
        use musicable_commands::automation::{CLEAR, RE_ENABLE, SHOW_LANE};
        use musicable_model::automation::{Lane, Point};
        use musicable_model::param::ParamAddress;
        use musicable_model::time::Beats;

        let mut project = Project::new();
        let eq = native(&mut project, NativeKind::Eq);
        let freq = 2 * eq::PER_BAND + eq::FREQ;
        let param = |n: usize| ParamAddress::Device(eq.id, n as u32);
        let at = |beat, value| Point::new(Beats::new(beat), value);
        let mut switch = Lane::new(param(eq::ON), vec![at(0, 1.0)]);
        switch.overridden = true;
        project.automation = vec![
            Lane::new(param(freq), vec![at(0, 1000.0), at(8, 4000.0)]),
            switch,
        ];
        let mut harness = showing(vec![eq.clone()], project);
        // The bar shows what the lane gives it at the playhead, not its own
        // 250 Hz, and the overridden switch its own value, off.
        let bar = harness.get_by_role_and_label(Role::Slider, "EQ Band 3 Freq");
        assert_eq!(bar.accesskit_node().numeric_value(), Some(1000.0));
        let on = harness.get_by_label("EQ Band 1 On");
        assert_eq!(on.accesskit_node().toggled(), Some(Toggled::False));

        // The bar's menu has the automation's commands under the typed
        // value; the switch's has them alone.
        let menu = |harness: &mut Harness<'static, State>, label: &str, item: &str| {
            harness.get_by_label(label).click_secondary();
            harness.run();
            harness.get_by_role_and_label(Role::Button, item).click();
            harness.run();
        };
        menu(&mut harness, "EQ Band 3 Freq", "Clear Automation");
        menu(&mut harness, "EQ Band 1 On", "Re-Enable Automation");
        menu(&mut harness, "EQ Band 2 Freq", "Show Automation");
        let run = |command, n: usize| Action::RunWith(command, Args::new().with("param", param(n)));
        let band2 = eq::PER_BAND + eq::FREQ;
        assert_eq!(
            actions(&mut harness),
            [
                run(CLEAR, freq),
                run(RE_ENABLE, eq::ON),
                run(SHOW_LANE, band2)
            ]
        );
    }

    #[test]
    fn a_plugins_menus_set_its_render_ahead() {
        let mut project = Project::new();
        let outboard = plugin(&mut project, "Outboard", PluginKind::AudioEffect);
        let missing = plugin(&mut project, "Missing", PluginKind::AudioEffect);
        let both = [outboard.id, missing.id].into();
        let mut harness = showing(vec![outboard, missing], project);
        let state = harness.state_mut();
        state.plugins = vec![PluginEntry {
            never_ahead: true,
            note_timing: None,
            ..entry("Outboard", "Example", PluginKind::AudioEffect)
        }];
        // Selected, so their menus don't select them.
        state.editor.devices.selected = both;
        state.shell.toggle_panel(BROWSER);
        harness.run();
        let set = |render_ahead| {
            let args = Args::new()
                .with("format", "clap")
                .with("plugin", "org.example.outboard")
                .with("render_ahead", render_ahead);
            Action::RunWith(musicable_commands::browser::SET_PLUGIN_RENDER_AHEAD, args)
        };
        // The device's menu says the plugin's, which is Never, and sets it.
        harness
            .get_by_label("Device Outboard, selected")
            .click_secondary();
        harness.run();
        harness.get_by_label("Render Ahead");
        let never = harness.get_by_label("Never");
        assert_eq!(never.accesskit_node().toggled(), Some(Toggled::True));
        harness.get_by_label("Auto").click();
        harness.run();
        assert_eq!(actions(&mut harness), [set("auto")]);
        // Not for a plugin that isn't installed.
        harness
            .get_by_label("Device Missing, selected")
            .click_secondary();
        harness.run();
        assert!(harness.query_by_label("Render Ahead").is_none());
        harness.key_press(Key::Escape);
        harness.run();
        // The browser's.
        click(&mut harness, "Devices");
        harness.get_by_label("Outboard").click_secondary();
        harness.run();
        harness.get_by_label("Never").click();
        harness.run();
        assert_eq!(actions(&mut harness), [set("never")]);
        assert!(
            harness.query_by_label("Render Ahead").is_none(),
            "the menu closes"
        );
    }

    #[test]
    fn a_plugins_menu_in_the_browser_marks_its_kind() {
        let mut harness = harness(Project::new());
        let state = harness.state_mut();
        state.plugins = vec![entry("Note Arp", "Example", PluginKind::AudioEffect)];
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        harness.get_by_label("Note Arp").click_secondary();
        harness.run();
        harness.get_by_label("Kind");
        click(&mut harness, "MIDI Effect");
        let args = Args::new()
            .with("format", "clap")
            .with("plugin", "org.example.note-arp")
            .with("kind", "midi_effect");
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(
                musicable_commands::browser::SET_PLUGIN_KIND,
                args
            )]
        );
        assert!(harness.query_by_label("Kind").is_none(), "the menu closes");
    }

    #[test]
    fn the_browser_lists_the_built_in_devices_that_play_first() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        let state = harness.state_mut();
        state.native = vec![NativeKind::Eq, NativeKind::Utility, NativeKind::Synth];
        state.plugins = vec![entry("Vital", "Vital Audio", PluginKind::Instrument)];
        state.editor.selected_track = Some(Chain::Track(keys));
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        let top =
            |harness: &Harness<'static, State>, label| harness.get_by_label(label).rect().top();
        assert!(top(&harness, "Built-in") < top(&harness, "EQ"));
        assert!(top(&harness, "EQ") < top(&harness, "Synth"));
        assert!(top(&harness, "Synth") < top(&harness, "Plugins"));
        assert!(top(&harness, "Plugins") < top(&harness, "Vital"));
        assert!(harness.query_by_label("Compressor").is_none());

        let listed = |harness: &Harness<'static, State>| -> Vec<&str> {
            (["Built-in", "EQ", "Utility", "Synth", "Vital"].into_iter())
                .filter(|name| harness.query_by_label(name).is_some())
                .collect()
        };
        click(&mut harness, "Instruments");
        assert_eq!(listed(&harness), ["Built-in", "Synth", "Vital"]);
        click(&mut harness, "Audio Effects");
        assert_eq!(listed(&harness), ["Built-in", "EQ", "Utility"]);
        click(&mut harness, "All");
        harness.get_by_role(Role::TextInput).click();
        harness.run();
        harness.get_by_role(Role::TextInput).type_text("vit");
        harness.run();
        assert_eq!(listed(&harness), ["Vital"]);
        for _ in 0.."vit".len() {
            harness.key_press(Key::Backspace);
            harness.run();
        }
        harness.get_by_role(Role::TextInput).type_text("til");
        harness.run();
        assert_eq!(listed(&harness), ["Built-in", "Utility"]);

        // A double-click adds it to the selected track, or to the master.
        let at = harness.get_by_label("Utility").rect().center();
        double_click(&mut harness, at);
        select(&mut harness, Chain::Master);
        double_click(&mut harness, at);
        let add = Args::new().with("kind", "utility");
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::ADD, add.clone().with("track", keys)),
                Action::RunWith(device::ADD, add.with("master", true)),
            ]
        );
    }

    #[test]
    fn the_sidechain_column_picks_a_compressors_sources_from_any_track() {
        let mut project = Project::new();
        let kick = project.add_track("Kick", TrackKind::Audio);
        let gate = native(&mut project, NativeKind::Gate);
        project.track_mut(kick).unwrap().devices = vec![gate];
        let pad = project.add_track("Pad", TrackKind::Audio);
        let group = project.add_track("Bus", TrackKind::Group);
        // A narrow device before it, so its box is in sight.
        let utility = native(&mut project, NativeKind::Utility);
        let compressor = native(&mut project, NativeKind::Compressor);
        let mut harness = showing(vec![utility.clone(), compressor.clone()], project);
        let keys = harness.state().project.tracks.last().unwrap().id;
        harness.state_mut().project.track_mut(keys).unwrap().output = Bus::Group(group);
        harness.run();
        let source = |harness: &Harness<'static, State>, label: &str| {
            let picker = harness.get_by_role_and_label(Role::ComboBox, label);
            picker.value()
        };
        let first = "Compressor Sidechain Source";
        assert_eq!(source(&harness, first).as_deref(), Some("No Input"));
        // No gain without a wire, and nothing to add to.
        assert!(
            harness
                .query_by_label("Compressor Sidechain Gain")
                .is_none()
        );
        assert!(
            harness
                .query_by_label("Add a Source to Compressor Sidechain")
                .is_none()
        );
        // Its switches share a row, above its filters.
        let rect =
            |harness: &Harness<'static, State>, label: &str| harness.get_by_label(label).rect();
        let (on, listen) = (
            rect(&harness, "Compressor Sidechain On"),
            rect(&harness, "Compressor Sidechain Listen"),
        );
        assert_eq!(on.top(), listen.top());
        assert!(on.right() < listen.left());
        assert!(on.bottom() < rect(&harness, "Compressor Sidechain High-Pass").top());

        // The other tracks, each opening to its taps, then the points before
        // the compressor on its own track. The group the track feeds would
        // close a loop.
        harness.get_by_role_and_label(Role::ComboBox, first).click();
        harness.run();
        assert!(
            harness
                .get_by_role_and_label(Role::Button, "Bus")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_role_and_label(Role::Button, "Kick").hover();
        harness.run();
        for tap in ["Pre FX", "after Gate", "Post FX", "Post Fader"] {
            harness.get_by_role_and_label(Role::Button, tap);
        }
        harness
            .get_by_role_and_label(Role::Button, "Post FX")
            .click();
        harness.run();
        let connect = Args::new().with("device", compressor.id);
        let kick_post_fx = connect.clone().with("track", kick).with("tap", "post_fx");
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(wire::CONNECT, kick_post_fx)]
        );

        // With a wire from the kick, and one from after the utility.
        let port = Port::Sidechain(compressor.id);
        let project = &mut harness.state_mut().project;
        let from_kick = project.new_wire(Tap::PostFx(Chain::Track(kick)), port);
        let after_utility = Wire {
            gain: 0.5,
            ..project.new_wire(Tap::Device(utility.id), port)
        };
        project.wires = vec![from_kick, after_utility];
        harness.run();
        assert_eq!(source(&harness, first).as_deref(), Some("Kick, Post FX"));
        let second = "Compressor Sidechain Source 2";
        assert_eq!(source(&harness, second).as_deref(), Some("after Utility"));
        let gain = harness.get_by_role_and_label(Role::Slider, "Compressor Sidechain Gain 2");
        assert_eq!(gain.accesskit_node().numeric_value(), Some(0.5));

        // Another tap moves the wire, and No Input takes it away.
        harness.get_by_role_and_label(Role::ComboBox, first).click();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "This track")
            .hover();
        harness.run();
        // The second wire takes it already.
        let after_utility = harness.get_by_role_and_label(Role::Button, "after Utility");
        assert!(after_utility.accesskit_node().is_disabled());
        harness.get_by_role_and_label(Role::Button, "Kick").hover();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Pre FX")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::ComboBox, first).click();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "No Input")
            .click();
        harness.run();
        // The + adds a source, and the bar sets a wire's gain.
        harness
            .get_by_label("Add a Source to Compressor Sidechain")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Pad").hover();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Post Fader")
            .click();
        harness.run();
        harness
            .get_by_role_and_label(Role::Slider, "Compressor Sidechain Gain")
            .set_value_accesskit(0.25);
        harness.run();
        let on = |args: Args| args.with("wire", from_kick.id);
        let kick_pre_fx = on(Args::new()).with("track", kick).with("tap", "pre_fx");
        let pad_post_fader = connect.with("track", pad).with("tap", "post_fader");
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(wire::SET_SOURCE, kick_pre_fx),
                Action::RunWith(wire::DISCONNECT, on(Args::new())),
                Action::RunWith(wire::CONNECT, pad_post_fader),
                Action::RunWith(wire::SET_GAIN, on(Args::new()).with("gain", 0.25)),
            ]
        );
    }
    #[test]
    fn a_plugins_box_has_a_row_for_each_of_its_side_inputs() {
        let mut project = Project::new();
        let kick = project.add_track("Kick", TrackKind::Audio);
        let mut vocoder = plugin(&mut project, "Vocoder", PluginKind::AudioEffect);
        let input = |id, name: &str| PluginInput {
            id,
            name: name.to_owned(),
        };
        let inputs = vec![input(7, "Sidechain"), input(9, "Carrier")];
        vocoder.kind.plugin_mut().unwrap().inputs = inputs;
        let reverb = plugin(&mut project, "Reverb", PluginKind::AudioEffect);
        let mut harness = showing(vec![vocoder.clone(), reverb], project);
        let source = |harness: &Harness<'static, State>, label: &str| {
            let picker = harness.get_by_role_and_label(Role::ComboBox, label);
            picker.value()
        };
        let rect =
            |harness: &Harness<'static, State>, label: &str| harness.get_by_label(label).rect();
        // Each under the plugin's name for it, in the plugin's order.
        let (sidechain, carrier) = ("Vocoder Sidechain Source", "Vocoder Carrier Source");
        assert_eq!(source(&harness, sidechain).as_deref(), Some("No Input"));
        assert_eq!(source(&harness, carrier).as_deref(), Some("No Input"));
        assert!(rect(&harness, sidechain).bottom() < rect(&harness, carrier).top());
        let heading = harness.get_by_label("Carrier").rect();
        assert!(rect(&harness, sidechain).bottom() <= heading.top());
        assert!(heading.bottom() <= rect(&harness, carrier).top());
        // No gain without a wire, and nothing to add to. A plugin without
        // side inputs has no rows.
        assert!(harness.query_by_label("Vocoder Sidechain Gain").is_none());
        assert!(
            harness
                .query_by_label("Add a Source to Vocoder Sidechain")
                .is_none()
        );
        assert!(
            harness
                .query_by_label_contains("Reverb Sidechain")
                .is_none()
        );

        // A tap from the menu wires it into that input.
        harness
            .get_by_role_and_label(Role::ComboBox, sidechain)
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Kick").hover();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "Post Fader")
            .click();
        harness.run();
        let into = Args::new().with("device", vocoder.id).with("input", 7_i64);
        let kick_post_fader = into.clone().with("track", kick).with("tap", "post_fader");
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(wire::CONNECT, kick_post_fader)]
        );

        // With a wire, the picker shows its source, with its gain under it,
        // and the + adds another source, but not the same one again.
        let port = Port::PluginInput(vocoder.id, 7);
        let project = &mut harness.state_mut().project;
        let from_kick = Wire {
            gain: 0.5,
            ..project.new_wire(Tap::PostFader(Chain::Track(kick)), port)
        };
        project.wires = vec![from_kick];
        harness.run();
        assert_eq!(
            source(&harness, sidechain).as_deref(),
            Some("Kick, Post Fader")
        );
        assert_eq!(source(&harness, carrier).as_deref(), Some("No Input"));
        let gain = harness.get_by_role_and_label(Role::Slider, "Vocoder Sidechain Gain");
        assert_eq!(gain.accesskit_node().numeric_value(), Some(0.5));
        harness
            .get_by_label("Add a Source to Vocoder Sidechain")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Kick").hover();
        harness.run();
        let taken = harness.get_by_role_and_label(Role::Button, "Post Fader");
        assert!(taken.accesskit_node().is_disabled());
        harness
            .get_by_role_and_label(Role::Button, "Pre FX")
            .click();
        harness.run();
        let kick_pre_fx = into.with("track", kick).with("tap", "pre_fx");
        assert_eq!(
            actions(&mut harness),
            [Action::RunWith(wire::CONNECT, kick_pre_fx)]
        );
    }
}

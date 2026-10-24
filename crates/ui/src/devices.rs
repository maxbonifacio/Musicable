//! The devices panel, under the arrangement: the devices of the selected
//! track, which can be the master track, in the order they play, with the
//! controls of Musicable's own. And the browser's list of Musicable's own
//! devices and the plugins installed, which adds them to the devices the
//! panel shows.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use egui::{
    Align, Button, Frame, Id, Label, Layout, RichText, Role, ScrollArea, Sense, Stroke, TextEdit,
    Ui, WidgetInfo, vec2,
};
use musicable_commands::{Args, device};
use musicable_model::native::NativeKind;
use musicable_model::param::ParamSpec;
use musicable_model::{Chain, Device, DeviceId, DeviceKind, PluginFormat, Project};

use crate::browser::Dragged;
use crate::mouse::MouseMap;
use crate::native;
use crate::search::score;
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::widgets::name_box;

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
    /// It plays notes.
    pub instrument: bool,
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
            });
        });
        ui.add_space(theme.devices.margin);
        let devices = chain.and_then(|chain| project.devices(chain));
        let Some(devices) = devices else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(NO_TRACK).weak());
            });
            return browse;
        };
        if devices.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(NO_DEVICES).weak());
            });
            return browse;
        }
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = theme.devices.gap;
                for device in devices {
                    ui.push_id(device.id, |ui| {
                        let saving = &mut self.saving;
                        device_box(ui, device, view, theme, mouse, saving, actions);
                    });
                }
            });
        });
        browse
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

/// A device: its name, and a built-in device's controls, or a plugin's
/// kind and how its plugin is doing. Its name's menu saves it as a preset,
/// which `saving` asks the name of.
fn device_box(
    ui: &mut Ui,
    device: &Device,
    view: &View<'_>,
    theme: &Theme,
    mouse: &MouseMap,
    saving: &mut Option<Saving>,
    actions: &mut Vec<Action>,
) {
    let sizes = &theme.devices;
    let (name, kind) = describe(device);
    let width = match device.kind.native() {
        Some(native) => native::width(native, theme),
        None => sizes.device - 2.0 * sizes.margin,
    };
    Frame::new()
        .fill(theme.device)
        .stroke(Stroke::new(1.0, theme.divider))
        .corner_radius(theme.corner)
        .inner_margin(sizes.margin)
        .show(ui, |ui| {
            ui.set_width(width);
            // Top to bottom, whatever the row of boxes around it does.
            ui.vertical(|ui| {
                title_row(ui, device, name, theme, saving, actions);
                match device.kind {
                    DeviceKind::Native(ref native) => {
                        native::controls(ui, device.id, name, native, view, theme, mouse, actions);
                    }
                    DeviceKind::Plugin(_) => {
                        ui.label(RichText::new(kind).size(theme.small_text).weak());
                        plugin_status(ui, device.id, name, view, theme, actions);
                    }
                    _ => {
                        ui.label(RichText::new(kind).size(theme.small_text).weak());
                    }
                }
            });
        });
}

/// A device's name, which shows its plugin's window when double-clicked, as
/// in Live, and whose menu saves it as a preset, and the button that
/// deletes it. And a sampler's sample.
fn title_row(
    ui: &mut Ui,
    device: &Device,
    name: &str,
    theme: &Theme,
    saving: &mut Option<Saving>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let delete = ui.small_button("×").on_hover_text("Delete");
            let label = format!("Delete {name}");
            delete.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
            let native = device.kind.native();
            if let Some(sampler) = native.filter(|native| native.kind == NativeKind::Sampler) {
                let file = sampler.sample.as_deref();
                sample(ui, device.id, name, file, theme, actions);
            }
            let title = RichText::new(name).strong().color(theme.header_text);
            let title = Label::new(title)
                .truncate()
                .selectable(false)
                .sense(Sense::click());
            // On the left, in the room the button leaves.
            let title = ui
                .with_layout(Layout::left_to_right(Align::Center), |ui| ui.add(title))
                .inner;
            let label = format!("Device {name}");
            title.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
            let args = Args::new().with("device", device.id);
            if title.double_clicked() && device.kind.plugin().is_some() {
                actions.push(Action::RunWith(device::SHOW_WINDOW, args.clone()));
            }
            title.context_menu(|ui| {
                if ui.button("Save as Preset...").clicked() {
                    *saving = Some(Saving {
                        device: device.id,
                        name: None,
                        device_name: name.to_owned(),
                    });
                }
                if ui.button("Delete").clicked() {
                    actions.push(Action::RunWith(device::DELETE, args.clone()));
                }
            });
            if delete.clicked() {
                actions.push(Action::RunWith(device::DELETE, args));
            }
        });
    });
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
    let load = ui
        .small_button("Load Sample...")
        .on_hover_text("Asks for an audio file for it to play");
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

/// A device's name, and what kind of device it is.
pub(crate) fn describe(device: &Device) -> (&str, String) {
    match &device.kind {
        DeviceKind::Plugin(plugin) => {
            let name = match plugin.name.trim() {
                "" => plugin.id.as_str(),
                name => name,
            };
            let role = if plugin.instrument {
                "Instrument"
            } else {
                "Effect"
            };
            (name, format!("{role} · {}", format_name(plugin.format)))
        }
        DeviceKind::Native(native) => (
            native.kind.name(),
            format!("{} · Built in", role(native.kind)),
        ),
        DeviceKind::TestGain { .. } => ("Test Gain", "Effect · Built in".to_owned()),
        DeviceKind::TestInstrument { .. } => {
            ("Test Instrument", "Instrument · Built in".to_owned())
        }
    }
}

/// What a built-in device of kind `kind` is for.
fn role(kind: NativeKind) -> &'static str {
    if kind.is_instrument() {
        "Instrument"
    } else {
        "Effect"
    }
}

fn format_name(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
    }
}

/// Which plugins the browser lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Instruments,
    Effects,
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
        ui.horizontal(|ui| {
            for (filter, title) in [
                (Filter::All, "All"),
                (Filter::Instruments, "Instruments"),
                (Filter::Effects, "Effects"),
            ] {
                if ui.selectable_label(self.filter == filter, title).clicked() {
                    self.filter = filter;
                }
            }
        });
        ui.separator();
        let filter = self.filter;
        let wanted = |instrument: bool| match filter {
            Filter::All => true,
            Filter::Instruments => instrument,
            Filter::Effects => !instrument,
        };
        let mut built_in: Vec<(u32, NativeKind)> = (view.native.iter())
            .filter(|kind| wanted(kind.is_instrument()))
            .filter_map(|&kind| Some((score(&self.search, kind.name(), kind.key(), &[])?, kind)))
            .collect();
        let mut found: Vec<(u32, &PluginEntry)> = (plugins.installed.iter())
            .filter(|p| wanted(p.instrument))
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
                        if plugin.instrument {
                            "Instrument"
                        } else {
                            "Effect"
                        },
                        format_name(plugin.format),
                    ));
                    if row.double_clicked() {
                        actions.push(Action::RunWith(device::ADD_PLUGIN, add(plugin, chain)));
                    }
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
    let format = match plugin.format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    };
    let args = Args::new()
        .with("format", format)
        .with("plugin", plugin.id.as_str())
        .with("name", plugin.name.as_str())
        .with("instrument", plugin.instrument);
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

#[cfg(test)]
mod tests {
    use egui::accesskit::Toggled;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{options, view};
    use musicable_model::TrackKind;
    use musicable_model::native::{NativeDevice, eq, synth, utility};
    use musicable_model::plugin::{PluginDevice, PluginSettings};

    use super::*;
    use crate::panels::{BROWSER, DEVICES};
    use crate::testing::{State, quick_harness as harness};

    fn id(name: &str) -> String {
        format!("org.example.{}", name.to_lowercase().replace(' ', "-"))
    }

    fn plugin(project: &mut Project, name: &str, instrument: bool) -> Device {
        project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: id(name),
            name: name.to_owned(),
            instrument,
            settings: PluginSettings::default(),
        }))
    }

    fn entry(name: &str, vendor: &str, instrument: bool) -> PluginEntry {
        PluginEntry {
            format: PluginFormat::Clap,
            id: id(name),
            name: name.to_owned(),
            vendor: vendor.to_owned(),
            instrument,
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
        let synth = plugin(&mut project, "Surge XT", true);
        let reverb = plugin(&mut project, "Old Reverb", false);
        let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let limiter = plugin(&mut project, "Limiter", false);
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
        assert_eq!(
            actions(&mut harness),
            [
                Action::RunWith(device::SHOW_WINDOW, on(&synth)),
                Action::RunWith(device::HIDE_WINDOW, on(&synth)),
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
    fn the_browser_finds_plugins_and_adds_them() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        let state = harness.state_mut();
        state.plugins = vec![
            entry("Dragonfly Hall Reverb", "Michael Willis", false),
            entry("Surge XT", "Surge Synth Team", true),
            entry("Vital", "Vital Audio", true),
        ];
        state.shell.toggle_panel(BROWSER);
        harness.run();
        click(&mut harness, "Devices");
        let names = ["Dragonfly Hall Reverb", "Surge XT", "Vital"];
        let tops: Vec<f32> = (names.iter())
            .map(|name| harness.get_by_label(name).rect().top())
            .collect();
        assert!(tops.is_sorted(), "in the order given: {tops:?}");
        harness.get_by_label("3 plugins");

        // Picking effects or instruments, and searching, by name or by
        // vendor.
        let listed = |harness: &Harness<'static, State>| -> Vec<&str> {
            (names.into_iter())
                .filter(|name| harness.query_by_label(name).is_some())
                .collect()
        };
        click(&mut harness, "Effects");
        assert_eq!(listed(&harness), ["Dragonfly Hall Reverb"]);
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
        state.plugins = vec![entry("Vital", "Vital Audio", true)];
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
            .with("instrument", true);
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
        let surge = entry("Surge XT", "Surge Synth Team", true);
        state.plugins = vec![
            surge.clone(),
            PluginEntry {
                format: PluginFormat::Vst3,
                id: "D07A3E8BF8E64BB8A1A2B3C4D5E6F708".to_owned(),
                ..surge
            },
            entry("Vital", "Vital Audio", true),
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
            .with("instrument", true)
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
    fn the_browser_lists_the_built_in_devices_that_play_first() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        let state = harness.state_mut();
        state.native = vec![NativeKind::Eq, NativeKind::Utility, NativeKind::Synth];
        state.plugins = vec![entry("Vital", "Vital Audio", true)];
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
        click(&mut harness, "Effects");
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
}

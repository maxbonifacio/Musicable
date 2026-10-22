//! The devices panel, under the arrangement: the devices of the selected
//! track, or the master's, in the order they play. And the plugin browser,
//! beside the arrangement, which lists the plugins installed and adds them
//! to the devices the panel shows.

use std::collections::BTreeMap;

use egui::{
    Align, Button, Frame, Label, Layout, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, vec2,
};
use musicable_commands::{Args, device};
use musicable_model::{Chain, Device, DeviceId, DeviceKind, PluginFormat, Project, TrackId};

use crate::search::score;
use crate::shell::{Action, View};
use crate::theme::Theme;

/// The devices panel's id, for [`Shell::toggle_panel`](crate::Shell::toggle_panel).
pub const DEVICES: &str = "devices";
/// The plugin browser's id.
pub const BROWSER: &str = "browser";

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginStatus {
    /// Its plugin loaded, and plays.
    pub loaded: bool,
    /// It has a window of its own.
    pub has_window: bool,
    /// Its window is open.
    pub window_open: bool,
    /// Why it plays nothing, if it doesn't.
    pub problem: Option<String>,
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
const NO_TRACK: &str = "Select a track to see its devices, or pick Master.";
/// What it shows for a chain without devices.
const NO_DEVICES: &str = "No devices. Double-click a plugin in the plugin browser to add it here.";

/// The devices panel's own state.
#[derive(Debug, Default)]
pub(crate) struct DevicesPanel {
    /// It shows the master's devices, not the selected track's.
    master: bool,
    /// The selected track as the panel last saw it, so selecting another
    /// track shows that track's devices.
    track: Option<TrackId>,
}

impl DevicesPanel {
    /// The chain the panel shows: the master's if the user picked it, else
    /// the selected track's.
    pub fn chain(&mut self, project: &Project, selected: Option<TrackId>) -> Option<Chain> {
        let selected = selected.filter(|&id| project.track(id).is_some());
        if selected != self.track {
            self.track = selected;
            self.master = false;
        }
        match self.master {
            true => Some(Chain::Master),
            false => selected.map(Chain::Track),
        }
    }

    /// Shows the devices of `chain`, and adds what the user asked for to
    /// `actions`. Returns whether the user asked for the plugin browser.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        chain: Option<Chain>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) -> bool {
        let project = view.project;
        let mut browse = false;
        ui.add_space(theme.devices.margin);
        ui.horizontal(|ui| {
            let track = self.track.and_then(|id| project.track(id));
            if let Some(track) = track
                && ui.selectable_label(!self.master, &track.name).clicked()
            {
                self.master = false;
            }
            if ui.selectable_label(self.master, "Master").clicked() {
                self.master = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                browse = ui
                    .button("Add Plugin...")
                    .on_hover_text("Shows the plugin browser")
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
        ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = theme.devices.gap;
                for device in devices {
                    ui.push_id(device.id, |ui| {
                        device_box(ui, device, view, theme, actions);
                    });
                }
            });
        });
        browse
    }
}

/// A device: its name, its kind, and how its plugin is doing.
fn device_box(
    ui: &mut Ui,
    device: &Device,
    view: &View<'_>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let sizes = &theme.devices;
    let args = || Args::new().with("device", device.id);
    let (name, kind) = describe(device);
    Frame::new()
        .fill(theme.device)
        .stroke(Stroke::new(1.0, theme.divider))
        .corner_radius(theme.corner)
        .inner_margin(sizes.margin)
        .show(ui, |ui| {
            ui.set_width(sizes.device - 2.0 * sizes.margin);
            ui.horizontal(|ui| {
                let delete = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let delete = ui.small_button("×").on_hover_text("Delete");
                        let title = RichText::new(name).strong().color(theme.header_text);
                        let title = ui.add(Label::new(title).truncate().sense(Sense::click()));
                        // As Live's devices do.
                        if title.double_clicked() && device.kind.plugin().is_some() {
                            actions.push(Action::RunWith(device::SHOW_WINDOW, args()));
                        }
                        delete.clicked()
                    })
                    .inner;
                if delete {
                    actions.push(Action::RunWith(device::DELETE, args()));
                }
            });
            ui.label(RichText::new(kind).size(theme.small_text).weak());
            if device.kind.plugin().is_none() {
                return;
            }
            let status = view.plugins.status.get(&device.id);
            match status {
                Some(status) if status.loaded && status.has_window => {
                    let button = Button::new("Plugin Window").selected(status.window_open);
                    if ui.add(button).clicked() {
                        actions.push(Action::RunWith(device::SHOW_WINDOW, args()));
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
                    let text = RichText::new(problem).color(theme.warning);
                    ui.add(Label::new(text).wrap());
                }
            }
        });
}

/// A device's name, and what kind of device it is.
fn describe(device: &Device) -> (&str, String) {
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
        DeviceKind::TestGain { .. } => ("Test Gain", "Effect · Built in".to_owned()),
        DeviceKind::TestInstrument { .. } => {
            ("Test Instrument", "Instrument · Built in".to_owned())
        }
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

/// The plugin browser's own state.
#[derive(Debug, Default)]
pub(crate) struct Browser {
    /// What's typed in the search field.
    search: String,
    filter: Filter,
}

impl Browser {
    /// Lists the plugins installed, and adds the one the user picks to
    /// `chain`'s devices: the selected track's without a chain.
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
        ui.label(RichText::new("Plugins").strong());
        ui.add(
            TextEdit::singleline(&mut self.search)
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
        let mut found: Vec<(u32, &PluginEntry)> = (plugins.installed.iter())
            .filter(|p| match self.filter {
                Filter::All => true,
                Filter::Instruments => p.instrument,
                Filter::Effects => !p.instrument,
            })
            .filter_map(|p| {
                let label = format!("{} {}", p.name, p.vendor);
                Some((score(&self.search, &label, &p.id, &[])?, p))
            })
            .collect();
        // The best matches first, and otherwise in the order given.
        found.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        let to = match chain {
            Some(Chain::Master) => "the master".to_owned(),
            Some(Chain::Track(id)) => match view.project.track(id) {
                Some(track) => track.name.clone(),
                None => "the selected track".to_owned(),
            },
            None => "the selected track".to_owned(),
        };
        let footer = ui.spacing().interact_size.y + 2.0 * theme.devices.margin;
        ScrollArea::vertical()
            .auto_shrink(false)
            .max_height((ui.available_height() - footer).max(0.0))
            .show(ui, |ui| {
                if found.is_empty() {
                    let empty = match (plugins.installed.is_empty(), plugins.scanning) {
                        (true, true) => "Looking for plugins...",
                        (true, false) => {
                            "No plugins found. Musicable looks for CLAP plugins \
                                          where CLAP says they're installed."
                        }
                        (false, _) => "No plugins match.",
                    };
                    ui.add(Label::new(RichText::new(empty).weak()).wrap());
                }
                for &(_, plugin) in &found {
                    let row = ui
                        .add(
                            Button::selectable(false, &plugin.name)
                                .min_size(vec2(ui.available_width(), 0.0)),
                        )
                        .on_hover_text(format!(
                            "{}{}\n{} · {}\nDouble-click to add it to {to}.",
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

/// The arguments that add `plugin` to `chain`, or to the selected track.
fn add(plugin: &PluginEntry, chain: Option<Chain>) -> Args {
    let format = match plugin.format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    };
    let args = Args::new()
        .with("format", format)
        .with("plugin", plugin.id.as_str())
        .with("name", plugin.name.as_str())
        .with("instrument", plugin.instrument);
    match chain {
        Some(Chain::Master) => args.with("master", true),
        Some(Chain::Track(track)) => args.with("track", track),
        None => args,
    }
}

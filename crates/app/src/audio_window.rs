//! The audio settings window: which device to play through, and how.

use eframe::egui::{self, Button, ComboBox, Grid, RadioButton, RichText, Ui};
use musicable_audio::{self as audio, Capabilities, Device, Mode, StreamRequest};

use crate::Output;
use crate::output::{kilohertz, millis};

/// The buffer sizes on offer, where the device takes them.
const BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

/// The audio settings window. The settings change when the user applies
/// them, all at once, since each change reopens the device.
#[derive(Default)]
pub struct AudioWindow {
    open: bool,
    /// The output devices, when the window opened or was refreshed.
    devices: Vec<Device>,
    /// Why the devices couldn't be listed.
    devices_error: Option<String>,
    /// The settings as edited, until they're applied.
    draft: StreamRequest,
    /// What the draft's device supports, or why that isn't known.
    capabilities: Option<Result<Capabilities, String>>,
    /// How the last Apply went.
    outcome: Option<Result<String, String>>,
}

impl AudioWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the window on the settings in use, and lists the devices.
    pub fn open(&mut self, output: &Output) {
        self.open = true;
        self.draft = output.request().clone();
        self.outcome = None;
        self.refresh();
    }

    /// Shows the window, if it's open. Returns the settings to apply, when
    /// the user applies them.
    pub fn show(&mut self, ctx: &egui::Context, output: &Output) -> Option<StreamRequest> {
        let mut open = self.open;
        let mut apply = None;
        egui::Window::new("Audio Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                Grid::new("audio_settings")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        self.device_row(ui);
                        self.mode_row(ui);
                        self.rate_row(ui);
                        self.buffer_row(ui);
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let changed = self.draft != *output.request();
                    if ui.add_enabled(changed, Button::new("Apply")).clicked() {
                        apply = Some(self.draft.clone());
                    }
                    if ui.add_enabled(changed, Button::new("Revert")).clicked() {
                        self.draft = output.request().clone();
                        self.load_capabilities();
                    }
                });
                match &self.outcome {
                    Some(Ok(text)) => {
                        ui.label(text);
                    }
                    Some(Err(text)) => {
                        ui.label(RichText::new(text).color(ui.visuals().warn_fg_color));
                    }
                    None => {}
                }
                ui.separator();
                ui.label(RichText::new("Now playing").strong());
                ui.label(output.details());
            });
        self.open = open;
        apply
    }

    /// Says how applying the settings went.
    pub fn applied(&mut self, outcome: Result<String, String>) {
        self.outcome = Some(outcome);
    }

    fn device_row(&mut self, ui: &mut Ui) {
        ui.label("Device");
        ui.horizontal(|ui| {
            let mut device = self.draft.device.clone();
            let default_name = self
                .devices
                .iter()
                .find(|d| d.is_default)
                .map_or("Default output".to_owned(), |d| {
                    format!("Default output ({})", d.name)
                });
            let selected = match &device {
                None => default_name.clone(),
                Some(id) => self
                    .devices
                    .iter()
                    .find(|d| &d.id == id)
                    .map_or("A device that isn't plugged in".to_owned(), |d| {
                        d.name.clone()
                    }),
            };
            ComboBox::from_id_salt("device")
                .selected_text(selected)
                .width(300.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut device, None, default_name);
                    for d in &self.devices {
                        ui.selectable_value(&mut device, Some(d.id.clone()), &d.name);
                    }
                });
            if device != self.draft.device {
                self.draft.device = device;
                // Each device has its own rates and sizes.
                self.draft.sample_rate = None;
                self.draft.period = None;
                self.load_capabilities();
                if let Some(Ok(caps)) = &self.capabilities
                    && caps.exclusive.is_none()
                {
                    self.draft.mode = Mode::Shared;
                }
            }
            if ui
                .button("Refresh")
                .on_hover_text("Lists the devices again, after plugging one in")
                .clicked()
            {
                self.refresh();
            }
        });
        ui.end_row();
        let caps_error = match &self.capabilities {
            Some(Err(error)) => Some(format!("The device didn't say what it supports: {error}.")),
            _ => None,
        };
        for problem in self.devices_error.iter().chain(&caps_error) {
            ui.label("");
            ui.label(RichText::new(problem).color(ui.visuals().warn_fg_color));
            ui.end_row();
        }
    }

    fn mode_row(&mut self, ui: &mut Ui) {
        // When the device doesn't say, let the user try.
        let exclusive = match &self.capabilities {
            Some(Ok(caps)) => caps.exclusive.is_some(),
            _ => true,
        };
        ui.label("Mode");
        ui.horizontal(|ui| {
            let shared = ui
                .radio(self.draft.mode == Mode::Shared, "Shared")
                .on_hover_text("Other apps can play at the same time");
            if shared.clicked() {
                self.draft.mode = Mode::Shared;
                self.draft.sample_rate = None;
                self.draft.period = None;
            }
            let radio = RadioButton::new(self.draft.mode == Mode::Exclusive, "Exclusive");
            let clicked = ui
                .add_enabled(exclusive, radio)
                .on_hover_text(
                    "The device to Musicable alone, for the lowest latency. Other apps go silent.",
                )
                .on_disabled_hover_text("This device doesn't offer exclusive mode.")
                .clicked();
            if clicked && self.draft.mode != Mode::Exclusive {
                self.draft.mode = Mode::Exclusive;
                self.draft.period = None;
            }
        });
        ui.end_row();
    }

    fn rate_row(&mut self, ui: &mut Ui) {
        ui.label("Sample rate");
        let device_rate = self.device_rate();
        let rates = match (self.draft.mode, self.exclusive()) {
            (Mode::Exclusive, Some(exclusive)) => exclusive.sample_rates.clone(),
            _ => Vec::new(),
        };
        if rates.is_empty() {
            let text = match device_rate {
                Some(rate) => format!("{}, the device's rate", kilohertz(rate)),
                None => "The device's rate".to_owned(),
            };
            ui.label(text).on_hover_text(
                "In shared mode the device runs at the rate set in the system's sound settings",
            );
        } else {
            let label = |rate: Option<u32>| match (rate, device_rate) {
                (Some(rate), _) => kilohertz(rate),
                (None, Some(rate)) => format!("{}, the device's rate", kilohertz(rate)),
                (None, None) => "The device's rate".to_owned(),
            };
            let mut rate = self.draft.sample_rate;
            ComboBox::from_id_salt("sample_rate")
                .selected_text(label(rate))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut rate, None, label(None));
                    for r in rates {
                        ui.selectable_value(&mut rate, Some(r), kilohertz(r));
                    }
                });
            self.draft.sample_rate = rate;
        }
        ui.end_row();
    }

    fn buffer_row(&mut self, ui: &mut Ui) {
        ui.label("Buffer size");
        let rate = self.draft.sample_rate.or(self.device_rate());
        let (smallest, default) = match (self.draft.mode, self.capabilities()) {
            (Mode::Exclusive, Some(caps)) => (caps.exclusive.as_ref().map(|e| e.min_period), None),
            (_, Some(caps)) => (caps.min_period, caps.default_period),
            (_, None) => (None, None),
        };
        let size = |frames: u32| match rate {
            Some(rate) => format!(
                "{frames} samples ({})",
                millis(std::time::Duration::from_secs_f64(
                    f64::from(frames) / f64::from(rate)
                ))
            ),
            None => format!("{frames} samples"),
        };
        let entry = |frames: u32| {
            if Some(frames) == default {
                format!("{}, the system's default", size(frames))
            } else {
                size(frames)
            }
        };
        let label = |period: Option<u32>| match (period, smallest) {
            (Some(frames), _) => size(frames),
            (None, Some(frames)) => format!("Smallest: {}", entry(frames)),
            (None, None) => "Smallest".to_owned(),
        };
        // Sizes the device takes, past the smallest, which has its own entry.
        let mut sizes: Vec<u32> = BUFFER_SIZES
            .into_iter()
            .chain(default)
            .filter(|&frames| smallest.is_none_or(|min| frames > min))
            .collect();
        sizes.sort_unstable();
        sizes.dedup();
        let mut period = self.draft.period;
        ComboBox::from_id_salt("buffer_size")
            .selected_text(label(period))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut period, None, label(None));
                for frames in sizes {
                    ui.selectable_value(&mut period, Some(frames), entry(frames));
                }
            })
            .response
            .on_hover_text(
                "Smaller buffers play sooner after a key press, and take more CPU. Drivers \
                 round the size to one they take: \"Now playing\" says what you got.",
            );
        self.draft.period = period;
        ui.end_row();
    }

    /// Lists the devices again, and what the draft's device supports.
    fn refresh(&mut self) {
        match audio::output_devices() {
            Ok(devices) => {
                self.devices = devices;
                self.devices_error = None;
            }
            Err(error) => {
                self.devices.clear();
                self.devices_error = Some(format!("The devices couldn't be listed: {error}."));
            }
        }
        self.load_capabilities();
    }

    fn load_capabilities(&mut self) {
        let capabilities = audio::capabilities(self.draft.device.as_deref());
        self.capabilities = Some(capabilities.map_err(|error| error.to_string()));
    }

    fn capabilities(&self) -> Option<&Capabilities> {
        self.capabilities.as_ref()?.as_ref().ok()
    }

    fn exclusive(&self) -> Option<&audio::ExclusiveCapabilities> {
        self.capabilities()?.exclusive.as_ref()
    }

    /// The rate the draft's device runs at now.
    fn device_rate(&self) -> Option<u32> {
        self.capabilities().map(|caps| caps.sample_rate)
    }
}

//! The Settings window's Audio page: which device to play through, and
//! how, and how far ahead of it the engine renders.

use eframe::egui::{Button, ComboBox, Grid, Id, RadioButton, RichText, Ui};
use musicable_audio::{
    self as audio, AsioCapabilities, Capabilities, Device, DriverType, Mode, StreamRequest,
};

use crate::Output;
use crate::output::{kilohertz, millis};
use crate::settings::RenderAhead;
use crate::settings_window::Asked;

/// The buffer sizes on offer, where the device takes them.
const BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

/// The Audio page. The device's settings change when the user applies them,
/// all at once, since each change reopens the device. Render Ahead changes
/// at once, since it doesn't.
#[derive(Default)]
pub struct AudioPage {
    /// The output devices, when the page came into sight or was refreshed.
    devices: Vec<Device>,
    /// Why the devices couldn't be listed.
    devices_error: Option<String>,
    /// What the draft's device plays through: the devices on offer.
    driver_type: DriverType,
    /// The settings as edited, until they're applied.
    draft: StreamRequest,
    /// What the draft's device supports, or why that isn't known.
    capabilities: Option<Result<Capabilities, String>>,
    /// The device playing, at its rate and buffer size, when the
    /// capabilities were loaded. An ASIO driver's change with its own
    /// settings shows there.
    playing: Option<(String, u32, u32)>,
    /// How the last Apply went.
    outcome: Option<Result<String, String>>,
}

impl AudioPage {
    /// Starts from the settings in use, and lists the devices: the page
    /// came into sight.
    pub fn reset(&mut self, output: &Output) {
        self.draft = output.request().clone();
        self.driver_type = driver_type(&self.draft);
        self.outcome = None;
        self.playing = playing(output);
        self.refresh();
    }

    /// Draws the page, with Render Ahead at `render_ahead`. Returns what
    /// the user changed: the device's settings to apply, when the user
    /// applies them, or Render Ahead.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        output: &Output,
        render_ahead: RenderAhead,
    ) -> Option<Asked> {
        let mut asked = None;
        if playing(output) != self.playing {
            self.playing = playing(output);
            self.load_capabilities();
        }
        Grid::new("audio_settings")
            .num_columns(2)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                if audio::driver_types().len() > 1 {
                    self.driver_type_row(ui);
                }
                self.device_row(ui);
                if self.driver_type == DriverType::System {
                    self.mode_row(ui);
                }
                self.rate_row(ui);
                self.buffer_row(ui);
                if self.driver_type == DriverType::Asio {
                    self.driver_settings_row(ui, output);
                }
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let changed = self.draft != *output.request();
            // With no ASIO driver picked, there's nothing to apply.
            let picked = self.driver_type == DriverType::System || self.draft.device.is_some();
            if ui
                .add_enabled(changed && picked, Button::new("Apply"))
                .clicked()
            {
                asked = Some(Asked::Stream(self.draft.clone()));
            }
            if ui.add_enabled(changed, Button::new("Revert")).clicked() {
                self.draft = output.request().clone();
                self.driver_type = driver_type(&self.draft);
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
        if let Some(chosen) = render_ahead_choice(ui, render_ahead) {
            asked = Some(Asked::RenderAhead(chosen));
        }
        ui.separator();
        ui.label(RichText::new("Now playing").strong());
        ui.label(output.details());
        ui.separator();
        gpl_notice(ui);
        asked
    }

    /// Says how applying the settings went.
    pub fn applied(&mut self, outcome: Result<String, String>) {
        self.outcome = Some(outcome);
    }

    fn driver_type_row(&mut self, ui: &mut Ui) {
        let label = ui.label("Driver type");
        let mut chosen = self.driver_type;
        ComboBox::from_id_salt("driver_type")
            .selected_text(driver_type_name(chosen))
            .show_ui(ui, |ui| {
                for &driver_type in audio::driver_types() {
                    ui.selectable_value(&mut chosen, driver_type, driver_type_name(driver_type));
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(
                "ASIO drivers talk to their hardware themselves, for the lowest latency. Other \
                 apps can't play through the device while Musicable has it.",
            );
        if chosen != self.driver_type {
            self.driver_type = chosen;
            // The default output, or the first driver of the type.
            self.draft.device = match chosen {
                DriverType::System => None,
                _ => self
                    .devices
                    .iter()
                    .find(|d| d.driver_type == chosen)
                    .map(|d| d.id.clone()),
            };
            self.draft.mode = Mode::Shared;
            self.draft.sample_rate = None;
            self.draft.period = None;
            self.load_capabilities();
        }
        ui.end_row();
    }

    fn device_row(&mut self, ui: &mut Ui) {
        let system = self.driver_type == DriverType::System;
        let label = ui.label(if system { "Device" } else { "Driver" });
        ui.horizontal(|ui| {
            let mut device = self.draft.device.clone();
            let offered: Vec<&Device> = self
                .devices
                .iter()
                .filter(|d| d.driver_type == self.driver_type)
                .collect();
            let default_name = offered
                .iter()
                .find(|d| d.is_default)
                .map_or("Default output".to_owned(), |d| {
                    format!("Default output ({})", d.name)
                });
            let missing = if system {
                "A device that isn't plugged in"
            } else {
                "A driver that isn't installed"
            };
            let selected = match &device {
                None if system => default_name.clone(),
                None => "No ASIO drivers are installed".to_owned(),
                Some(id) => offered
                    .iter()
                    .find(|d| &d.id == id)
                    .map_or(missing.to_owned(), |d| d.name.clone()),
            };
            ComboBox::from_id_salt("device")
                .selected_text(selected)
                .width(300.0)
                .show_ui(ui, |ui| {
                    if system {
                        ui.selectable_value(&mut device, None, default_name);
                    }
                    for d in offered {
                        ui.selectable_value(&mut device, Some(d.id.clone()), &d.name);
                    }
                })
                .response
                .labelled_by(label.id);
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
        let row = ui.label("Sample rate");
        let device_rate = self.device_rate();
        let rates = match (self.asio(), self.draft.mode, self.exclusive()) {
            // Switching an ASIO driver's rate switches it for its hardware.
            (Some(asio), _, _) => asio.sample_rates.clone(),
            (None, Mode::Exclusive, Some(exclusive)) => exclusive.sample_rates.clone(),
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
                })
                .response
                .labelled_by(row.id);
            self.draft.sample_rate = rate;
        }
        ui.end_row();
    }

    fn buffer_row(&mut self, ui: &mut Ui) {
        let row = ui.label("Buffer size");
        let rate = self.draft.sample_rate.or(self.device_rate());
        if let Some(asio) = self.asio() {
            let sizes = asio.buffer_sizes.clone();
            let preferred = self.capabilities().and_then(|caps| caps.default_period);
            self.asio_buffer_row(ui, row.id, rate, &sizes, preferred);
            return;
        }
        let (smallest, default) = match (self.draft.mode, self.capabilities()) {
            (Mode::Exclusive, Some(caps)) => (caps.exclusive.as_ref().map(|e| e.min_period), None),
            (_, Some(caps)) => (caps.min_period, caps.default_period),
            (_, None) => (None, None),
        };
        let size = |frames: u32| samples(frames, rate);
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
            .labelled_by(row.id)
            .on_hover_text(
                "Smaller buffers play sooner after a key press, and take more CPU. Drivers \
                 round the size to one they take: \"Now playing\" says what you got.",
            );
        self.draft.period = period;
        ui.end_row();
    }

    /// An ASIO driver's buffer sizes: those it lists, and by default the
    /// one it prefers, which is the one set in its own settings if it has
    /// any. The label with id `row` names them.
    fn asio_buffer_row(
        &mut self,
        ui: &mut Ui,
        row: Id,
        rate: Option<u32>,
        sizes: &[u32],
        preferred: Option<u32>,
    ) {
        let label = |period: Option<u32>| match (period, preferred) {
            (Some(frames), _) => samples(frames, rate),
            (None, Some(frames)) => format!("The driver's: {}", samples(frames, rate)),
            (None, None) => "The driver's".to_owned(),
        };
        let mut period = self.draft.period;
        ComboBox::from_id_salt("buffer_size")
            .selected_text(label(period))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut period, None, label(None));
                for &frames in sizes {
                    ui.selectable_value(&mut period, Some(frames), samples(frames, rate));
                }
            })
            .response
            .labelled_by(row)
            .on_hover_text(
                "Smaller buffers play sooner after a key press, and take more CPU. Many ASIO \
                 drivers only take the size set in their own settings: Driver Settings opens \
                 them.",
            );
        self.draft.period = period;
        ui.end_row();
    }

    /// Opens the playing ASIO driver's own settings.
    fn driver_settings_row(&mut self, ui: &mut Ui, output: &Output) {
        ui.label("");
        let playing = output
            .info()
            .is_some_and(|info| self.draft.device.as_ref() == Some(&info.device.id));
        let clicked = ui
            .add_enabled(playing, Button::new("Driver Settings…"))
            .on_hover_text(
                "The driver's own settings, where many ASIO drivers set their buffer size. When \
                 they change, Musicable opens the driver again with them.",
            )
            .on_disabled_hover_text("Apply first: the settings open for the driver that plays.")
            .clicked();
        if clicked {
            output.show_driver_settings();
        }
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
        if self.driver_type == DriverType::Asio && self.draft.device.is_none() {
            // No driver to ask.
            self.capabilities = None;
            return;
        }
        let capabilities = audio::capabilities(self.draft.device.as_deref());
        self.capabilities = Some(capabilities.map_err(|error| error.to_string()));
    }

    fn capabilities(&self) -> Option<&Capabilities> {
        self.capabilities.as_ref()?.as_ref().ok()
    }

    fn exclusive(&self) -> Option<&audio::ExclusiveCapabilities> {
        self.capabilities()?.exclusive.as_ref()
    }

    fn asio(&self) -> Option<&AsioCapabilities> {
        self.capabilities()?.asio.as_ref()
    }

    /// The rate the draft's device runs at now.
    fn device_rate(&self) -> Option<u32> {
        self.capabilities().map(|caps| caps.sample_rate)
    }
}

/// Render Ahead's choices, with `current` chosen. Returns the one the user
/// picks.
fn render_ahead_choice(ui: &mut Ui, current: RenderAhead) -> Option<RenderAhead> {
    ui.label(RichText::new("Render Ahead").strong());
    let mut chosen = current;
    for choice in RenderAhead::ALL {
        let (name, hover) = match choice {
            RenderAhead::Off => (
                "Off",
                "Every block renders as the device asks for it, within its buffer",
            ),
            RenderAhead::Short => (
                "Short, about 20 ms",
                "Knobs turned by hand are heard soonest",
            ),
            RenderAhead::Normal => (
                "Normal, about 50 ms",
                "Room for a heavy mix at a small buffer",
            ),
            RenderAhead::Long => (
                "Long, about 200 ms",
                "The most room, for the heaviest mixes",
            ),
        };
        ui.radio_value(&mut chosen, choice, name)
            .on_hover_text(hover);
    }
    ui.label(
        RichText::new(
            "While no track monitors its input, Musicable renders the whole project this far \
             ahead of the device, on all its cores, so a small buffer plays a heavy mix without \
             dropouts. Knobs turned by hand, Stop and jumps are heard up to this much later.",
        )
        .weak(),
    );
    (chosen != current).then_some(chosen)
}

/// What the device `request` names plays through.
fn driver_type(request: &StreamRequest) -> DriverType {
    request
        .device
        .as_deref()
        .map_or(DriverType::System, DriverType::of)
}

fn driver_type_name(driver_type: DriverType) -> &'static str {
    match driver_type {
        DriverType::Asio => "ASIO",
        _ if cfg!(windows) => "WASAPI",
        _ => "System",
    }
}

/// The device playing, its rate and its buffer size.
fn playing(output: &Output) -> Option<(String, u32, u32)> {
    output
        .info()
        .map(|info| (info.device.id.clone(), info.sample_rate, info.period))
}

/// A buffer size, and how long it lasts at `rate` if that's known.
fn samples(frames: u32, rate: Option<u32>) -> String {
    match rate {
        Some(rate) => format!(
            "{frames} samples ({})",
            millis(std::time::Duration::from_secs_f64(
                f64::from(frames) / f64::from(rate)
            ))
        ),
        None => format!("{frames} samples"),
    }
}

/// The notice the GPL asks for in a program's windows, since Musicable is
/// distributed under it (README.md).
fn gpl_notice(ui: &mut Ui) {
    let small = |text: &str| RichText::new(text).small().weak();
    ui.label(small(
        "Copyright © the Musicable contributors. Musicable is free software, distributed under \
         the GNU General Public License, version 3 or any later version: you may share and \
         change it under that license. It comes with no warranty.",
    ));
    ui.hyperlink_to(
        small("The GNU GPL, version 3"),
        "https://www.gnu.org/licenses/gpl-3.0.html",
    );
    if audio::driver_types().contains(&DriverType::Asio) {
        ui.label(small(
            "ASIO is a trademark of Steinberg Media Technologies GmbH.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_type_comes_from_the_device_id() {
        let request = |device: Option<&str>| StreamRequest {
            device: device.map(str::to_owned),
            ..StreamRequest::default()
        };
        assert_eq!(driver_type(&request(None)), DriverType::System);
        assert_eq!(
            driver_type(&request(Some("{0.0.0.00000000}.{a1b2c3d4}"))),
            DriverType::System
        );
        assert_eq!(
            driver_type(&request(Some(
                "asio:{A1B2C3D4-0000-0000-0000-000000000000}"
            ))),
            DriverType::Asio
        );
    }

    #[test]
    fn buffer_sizes_say_how_long_they_last() {
        assert_eq!(samples(480, Some(48_000)), "480 samples (10.0 ms)");
        assert_eq!(samples(256, None), "256 samples");
    }
}

//! Settings as commands: [`Request::SetSetting`] and
//! [`Request::SetAudioDevice`] change them as the Settings window does,
//! and agents read them, and the audio devices there are. Settings aren't
//! steps in the undo history, so each change says what the setting was, for
//! an agent to put it back.

use eframe::egui;
use musicable_audio::{self as audio, DriverType, Mode, StreamInfo, StreamRequest};
use musicable_commands::AudioDevice;
use serde_json::{Value as Json, json};

use super::App;
use musicable_ui::{MAX_PREVIEW_VOLUME, MIN_PREVIEW_VOLUME};

use crate::settings::{SETTINGS_FILE, Settings};
use crate::settings_window::Asked;

/// The settings commands can change, by section, with `*` for all of a
/// section's. The rest are the user's: the theme, the workspaces, the
/// profiles, whether agents are let in, and how plugins are marked.
const CHANGEABLE: &[(&str, &[&str])] = &[
    (
        "audio",
        &[
            "device",
            "exclusive",
            "sample_rate",
            "buffer_size",
            "render_ahead",
        ],
    ),
    ("behaviour", &["*"]),
    ("export", &["*"]),
    (
        "browser",
        &["auto_preview", "preview_volume", "presets_folder"],
    ),
    ("recording", &["capture_minutes"]),
    ("analysis", &["fast"]),
    ("render_cache", &["*"]),
];

impl App {
    /// The settings as the settings file has them, with the profile in
    /// use's behaviour, appearance and workspace, as JSON.
    fn settings_json(&self) -> Json {
        let mut settings = self.settings.clone();
        settings.behaviour = self.profile.behaviour;
        settings.appearance = self.profile.appearance.clone();
        settings.workspaces = self.profile.workspaces.clone();
        serde_json::to_value(&settings).unwrap_or(Json::Null)
    }

    /// The settings, the profile in use, and what plays, for an agent.
    pub(super) fn settings_for_agent(&self) -> Json {
        let file = self.config_dir.as_ref().map(|dir| dir.join(SETTINGS_FILE));
        json!({
            "file": file,
            "profile": self.profiles.current_name(),
            "settings": self.settings_json(),
            "changeable": changeable(),
            "playing": self.output.info().map(playing),
            "recording_offset_frames": self.session.recording_offset(),
        })
    }

    /// The audio devices there are to play through and record from, listed
    /// again, for an agent.
    pub(super) fn audio_devices_for_agent(&mut self) -> Json {
        self.list_inputs();
        let playing = self.output.info().map(|info| info.device.id.clone());
        let outputs: Vec<Json> = match audio::output_devices() {
            Ok(devices) => (devices.into_iter())
                .map(|device| {
                    json!({
                        "id": device.id,
                        "name": device.name,
                        "driver_type": driver_type_name(device.driver_type),
                        "default": device.is_default,
                        "playing": playing.as_ref() == Some(&device.id),
                    })
                })
                .collect(),
            Err(error) => {
                (self.messages).push(format!("The audio devices couldn't be listed: {error}."));
                Vec::new()
            }
        };
        let inputs: Vec<Json> = (self.input_choices.iter())
            .map(|input| json!({ "id": input.id, "name": input.name, "channels": input.channels }))
            .collect();
        let midi_inputs: Vec<Json> = (self.midi_choices.iter())
            .map(|input| json!({ "id": input.id, "name": input.name }))
            .collect();
        let driver_types: Vec<&str> = (audio::driver_types().iter())
            .map(|driver_type| driver_type_name(*driver_type))
            .collect();
        json!({
            "driver_types": driver_types,
            "outputs": outputs,
            "inputs": inputs,
            "midi_inputs": midi_inputs,
        })
    }

    /// Changes the setting `key`, such as `audio.render_ahead`, to `value`,
    /// as the Settings window would, and says what it was and is now.
    pub(super) fn set_setting(
        &mut self,
        key: &str,
        value: &str,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) -> Result<Json, String> {
        let unknown = || {
            format!(
                "There's no setting called {key} that commands can change. They can change \
                 {}.",
                changeable().join(", ")
            )
        };
        let (section, name) = key.split_once('.').ok_or_else(unknown)?;
        let allowed = (CHANGEABLE.iter())
            .find(|(changeable, _)| *changeable == section)
            .is_some_and(|(_, names)| names.contains(&"*") || names.contains(&name));
        let mut settings = self.settings_json();
        let Some(Json::Object(fields)) = settings.get_mut(section) else {
            return Err(unknown());
        };
        // A setting that can be empty is left out while it is.
        let was = fields.get(name).cloned().unwrap_or(Json::Null);
        if !allowed || (was.is_null() && !optional(section, name)) {
            return Err(unknown());
        }
        let wanted = parse(value, &was).ok_or_else(|| cant_be(key, value, &was))?;
        fields.insert(name.to_owned(), wanted.clone());
        let changed: Settings =
            serde_json::from_value(settings).map_err(|_| cant_be(key, value, &was))?;
        // What serde made of it, which leaves out what it doesn't know.
        let made = setting(
            &serde_json::to_value(&changed).unwrap_or_default(),
            section,
            name,
        );
        if made != wanted {
            return Err(cant_be(key, value, &was));
        }
        match (section, name) {
            ("audio", "render_ahead") => {
                let ahead = changed.audio.render_ahead;
                self.settings_asked(Asked::RenderAhead(ahead), ctx, frame);
            }
            ("audio", _) => {
                let mut request = changed.audio.request();
                request.inputs = self.output.request().inputs.clone();
                request.midi_inputs = self.output.request().midi_inputs.clone();
                self.apply_audio(request)?;
            }
            ("behaviour", _) => {
                self.settings_asked(Asked::Behaviour(changed.behaviour), ctx, frame);
            }
            ("export", _) => {
                self.settings.export = changed.export;
                self.save_settings();
            }
            ("browser", "presets_folder") => {
                self.set_presets_folder(changed.browser.presets_folder);
            }
            ("browser", "preview_volume") => {
                let volume = changed.browser.preview_volume;
                if !(MIN_PREVIEW_VOLUME..=MAX_PREVIEW_VOLUME).contains(&volume) {
                    return Err(format!(
                        "browser.preview_volume must be from {MIN_PREVIEW_VOLUME} to \
                         {MAX_PREVIEW_VOLUME} dB."
                    ));
                }
                self.settings.browser.preview_volume = volume;
                (self.session).set_preview_gain(self.settings.browser.preview_gain());
                self.save_settings();
            }
            ("browser", _) => {
                self.settings.browser.auto_preview = changed.browser.auto_preview;
                self.save_settings();
            }
            ("recording", _) => {
                let minutes = changed.recording.capture_minutes;
                self.settings_asked(Asked::CaptureMinutes(minutes), ctx, frame);
            }
            ("analysis", _) => {
                let fast = changed.analysis.fast;
                self.settings_asked(Asked::FastAnalysis(fast), ctx, frame);
            }
            ("render_cache", _) => {
                let limits = changed.render_cache;
                self.settings_asked(Asked::RenderCache(limits), ctx, frame);
            }
            _ => return Err(unknown()),
        }
        let now = setting(&self.settings_json(), section, name);
        Ok(json!({ "key": key, "was": was, "now": now }))
    }

    /// Plays through the device `device` asks for, as the Audio page's
    /// Apply does, and says what played and what plays now. If it won't
    /// open, the device before plays on.
    pub(super) fn set_audio_device(&mut self, device: AudioDevice) -> Result<Json, String> {
        let was = self.output.info().map(playing);
        let mut request = self.output.request().clone();
        let current = request
            .device
            .as_deref()
            .map_or(DriverType::System, DriverType::of);
        let driver_type = match device.driver_type.as_deref() {
            None => None,
            Some("system") => Some(DriverType::System),
            Some("asio") => Some(DriverType::Asio),
            Some(other) => return Err(format!("There's no driver type called {other}.")),
        };
        if let Some(wanted) = driver_type
            && !audio::driver_types().contains(&wanted)
        {
            return Err("This build of Musicable plays through no ASIO drivers.".into());
        }
        match (device.device.as_deref(), driver_type) {
            (Some(named), wanted) => {
                let devices = audio::output_devices()
                    .map_err(|error| format!("The audio devices couldn't be listed: {error}."))?;
                let found = (devices.iter())
                    .filter(|d| wanted.is_none_or(|wanted| d.driver_type == wanted))
                    .find(|d| d.id == named || d.name.eq_ignore_ascii_case(named))
                    .ok_or_else(|| {
                        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
                        format!(
                            "There's no audio device called {named}{}. There are: {}.",
                            match wanted {
                                Some(DriverType::Asio) => " among the ASIO drivers",
                                _ => "",
                            },
                            names.join(", ")
                        )
                    })?;
                request.device = Some(found.id.clone());
            }
            (None, Some(wanted)) if wanted != current => {
                request.device = match wanted {
                    DriverType::Asio => {
                        let devices = audio::output_devices().map_err(|error| {
                            format!("The audio devices couldn't be listed: {error}.")
                        })?;
                        let first = (devices.into_iter())
                            .find(|d| d.driver_type == DriverType::Asio)
                            .ok_or("There's no ASIO driver installed.")?;
                        Some(first.id)
                    }
                    _ => None,
                };
                // Another driver's buffer and rate needn't fit.
                request.period = None;
                request.sample_rate = None;
            }
            (None, _) => {}
        }
        if let Some(buffer) = device.buffer {
            request.period = Some(buffer);
        }
        if let Some(rate) = device.sample_rate {
            request.sample_rate = Some(rate);
        }
        if let Some(exclusive) = device.exclusive {
            request.mode = if exclusive {
                Mode::Exclusive
            } else {
                Mode::Shared
            };
        }
        self.apply_audio(request)?;
        Ok(json!({ "was": was, "now": self.output.info().map(playing) }))
    }

    /// Plays through the device `request` asks for, as the Audio page's
    /// Apply does, keeping it in the settings. If it won't open, the device
    /// before plays on, and the error says so.
    pub(super) fn apply_audio(&mut self, request: StreamRequest) -> Result<(), String> {
        let wanted = self.session.wanted_inputs();
        let outcome = match self.output.apply(request, &wanted, &mut self.session) {
            Ok(()) => {
                self.settings.audio.set_request(self.output.request());
                self.save_settings();
                Ok(format!("Playing through {}.", self.output.device_name()))
            }
            Err(error) => Err(format!(
                "The device didn't open with these settings: {error}. The ones before are back."
            )),
        };
        self.settings_window.applied(outcome.clone());
        outcome.map(|_| ())
    }
}

/// The settings commands can change, as `section.name`, with `section.*`
/// for all of a section's.
fn changeable() -> Vec<String> {
    (CHANGEABLE.iter())
        .flat_map(|(section, names)| names.iter().map(move |name| format!("{section}.{name}")))
        .collect()
}

/// Whether the setting can be empty, and is left out of the file while it
/// is.
fn optional(section: &str, name: &str) -> bool {
    matches!(
        (section, name),
        ("audio", "device" | "sample_rate" | "buffer_size") | ("browser", "presets_folder")
    ) || section == "export"
}

/// The setting `section.name` in `settings`, or null while it's empty.
fn setting(settings: &Json, section: &str, name: &str) -> Json {
    settings
        .get(section)
        .and_then(|fields| fields.get(name))
        .cloned()
        .unwrap_or(Json::Null)
}

/// `text` as a value for a setting that's `was` now: true or false for a
/// switch, a number for a number, `none` or nothing to empty one that can
/// be, and else the text.
fn parse(text: &str, was: &Json) -> Option<Json> {
    let text = text.trim();
    let number = || -> Option<Json> {
        match text.parse::<i64>() {
            Ok(n) => Some(n.into()),
            Err(_) => text
                .parse::<f64>()
                .ok()
                .and_then(|n| serde_json::Number::from_f64(n).map(Json::Number)),
        }
    };
    match was {
        _ if text.is_empty() || text.eq_ignore_ascii_case("none") => match was {
            Json::String(_) => Some(Json::String(String::new())),
            _ => Some(Json::Null),
        },
        Json::Bool(_) => match text {
            "true" | "on" => Some(Json::Bool(true)),
            "false" | "off" => Some(Json::Bool(false)),
            _ => None,
        },
        Json::Number(_) => number(),
        Json::Null => Some(number().unwrap_or_else(|| Json::String(text.to_owned()))),
        _ => Some(Json::String(text.to_owned())),
    }
}

/// Why `key` can't be `value`.
fn cant_be(key: &str, value: &str, was: &Json) -> String {
    let kind = match was {
        Json::Bool(_) => " It's true or false.",
        Json::Number(_) => " It's a number.",
        _ => "",
    };
    format!("{key} can't be {value}.{kind} It's {was} now.")
}

/// What plays, for an agent.
fn playing(info: &StreamInfo) -> Json {
    let ms =
        |frames: u32| (f64::from(frames) * 10_000.0 / f64::from(info.sample_rate)).round() / 10.0;
    json!({
        "device": info.device.id,
        "name": info.device.name,
        "driver_type": driver_type_name(info.device.driver_type),
        "api": info.api.to_string(),
        "sample_rate": info.sample_rate,
        "buffer": info.period,
        "latency_ms": ms(info.latency),
        "inputs": (info.inputs.iter())
            .map(|input| json!({
                "id": input.id,
                "name": input.name,
                "channels": input.channels,
                "error": input.error,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A driver type as the settings commands name it.
fn driver_type_name(driver_type: DriverType) -> &'static str {
    match driver_type {
        DriverType::Asio => "asio",
        _ => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_read_as_the_setting_is() {
        assert_eq!(parse("true", &json!(false)), Some(json!(true)));
        assert_eq!(parse("on", &json!(false)), Some(json!(true)));
        assert_eq!(parse("yes", &json!(false)), None);
        assert_eq!(parse("10", &json!(5)), Some(json!(10)));
        assert_eq!(parse("0.5", &json!(1)), Some(json!(0.5)));
        assert_eq!(parse("ten", &json!(5)), None);
        assert_eq!(parse("long", &json!("normal")), Some(json!("long")));
        // An empty setting takes a number, or text.
        assert_eq!(parse("64", &Json::Null), Some(json!(64)));
        assert_eq!(parse("asio:{1}", &Json::Null), Some(json!("asio:{1}")));
        assert_eq!(parse("none", &json!(64)), Some(Json::Null));
        assert_eq!(parse("", &Json::Null), Some(Json::Null));
    }
}

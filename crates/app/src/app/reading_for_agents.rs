//! What agents read that only the window knows: the plugins installed, how
//! the panels are laid out, and the meters.

use musicable_commands::param_text;
use musicable_model::PluginFormat;
use musicable_ui::{Level, PanelLayout};
use serde_json::{Value as Json, json};

use super::App;

impl App {
    /// The plugins installed, as the browser lists them, or those whose
    /// names, vendors or ids hold `filter`, in any case.
    pub(super) fn plugins_for_agent(&self, filter: Option<&str>) -> Json {
        let filter = filter.map(str::to_lowercase);
        let holds = |text: &str| {
            (filter.as_deref()).is_none_or(|filter| text.to_lowercase().contains(filter))
        };
        let plugins: Vec<Json> = (self.plugin_entries.iter())
            .filter(|plugin| holds(&plugin.name) || holds(&plugin.vendor) || holds(&plugin.id))
            .map(|plugin| {
                json!({
                    "format": format_key(plugin.format),
                    "id": plugin.id,
                    "name": plugin.name,
                    "vendor": plugin.vendor,
                    "kind": plugin.kind.key(),
                    "render_ahead": if plugin.never_ahead { "never" } else { "auto" },
                })
            })
            .collect();
        json!({
            "plugins": plugins,
            "installed": self.plugin_entries.len(),
            "scanning": self.plugin_scan.is_running(),
        })
    }

    /// How the panels and the windows of their own are laid out, the
    /// monitors, and the workspace and the profile in use.
    pub(super) fn layout_for_agent(&self) -> Json {
        let PanelLayout { panels, windows } = self.shell.layout();
        let panels: Vec<Json> = (panels.into_iter())
            .map(|panel| {
                json!({
                    "id": panel.id,
                    "title": panel.title,
                    "window": panel.window.map_or(json!("main"), |window| json!(window)),
                    "hidden": panel.hidden,
                    "in_sight": panel.in_sight,
                })
            })
            .collect();
        let windows: Vec<Json> = (windows.into_iter().enumerate())
            .map(|(index, window)| {
                json!({
                    "window": index + 1,
                    "at": window.at,
                    "size": window.size,
                    "maximized": window.maximized,
                    "panels": window.panels,
                })
            })
            .collect();
        let monitors: Vec<Json> = (self.monitors.iter())
            .map(|monitor| {
                json!({
                    "at": monitor.at,
                    "size": monitor.size,
                    "primary": monitor.primary,
                })
            })
            .collect();
        json!({
            "panels": panels,
            "windows": windows,
            "monitors": monitors,
            "workspace": self.workspaces.current_name(),
            "profile": self.profiles.current_name(),
        })
    }

    /// What the meters read: each track's and the master's, in dBFS, where
    /// each modulator's outputs are, and how far routings move parameters.
    pub(super) fn levels_for_agent(&self) -> Json {
        let levels = &self.levels;
        let project = self.session.project();
        let tracks: Vec<Json> = (project.tracks.iter())
            .map(|track| {
                let mut entry = level(levels.track(track.id));
                entry["track"] = track.id.0.into();
                entry["name"] = track.name.as_str().into();
                entry
            })
            .collect();
        let modulators: Vec<Json> = (levels.outputs.iter())
            .map(|(output, value)| {
                json!({
                    "modulator": output.modulator.0,
                    "output": output.n,
                    "value": rounded(f64::from(*value), 1000.0),
                })
            })
            .collect();
        let moved: Vec<Json> = (levels.moved.iter())
            .map(|(param, by)| json!({ "param": param_text(*param), "by": rounded(f64::from(*by), 1000.0) }))
            .collect();
        json!({
            "playing": self.output.info().is_some(),
            "moving": levels.moving(),
            "master": level(levels.master),
            "tracks": tracks,
            "modulators": modulators,
            "moved": moved,
        })
    }
}

/// A meter's readings in dBFS, to a hundredth of a dB, null for silence.
fn level(level: Level) -> Json {
    let pair = |[left, right]: [f32; 2]| json!([dbfs(left), dbfs(right)]);
    json!({
        "peak": pair(level.peak),
        "rms": pair(level.rms),
        "highest": dbfs(level.highest),
    })
}

/// A linear reading in dBFS, to a hundredth of a dB: `None` for silence.
fn dbfs(reading: f32) -> Option<f64> {
    let reading = f64::from(reading);
    (reading > 0.0 && reading.is_finite()).then(|| rounded(20.0 * reading.log10(), 100.0))
}

/// `value` to the nearest 1 / `steps`.
fn rounded(value: f64, steps: f64) -> f64 {
    (value * steps).round() / steps
}

/// What commands call `format`.
fn format_key(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_in_dbfs_and_silence_is_null() {
        assert_eq!(dbfs(1.0), Some(0.0));
        assert_eq!(dbfs(0.5), Some(-6.02));
        assert_eq!(dbfs(2.0), Some(6.02));
        assert_eq!(dbfs(0.0), None);
        assert_eq!(dbfs(f32::NAN), None);
        let read = level(Level {
            peak: [0.5, 0.0],
            rms: [0.25, 0.0],
            highest: 1.0,
        });
        assert_eq!(
            read,
            json!({ "peak": [-6.02, null], "rms": [-12.04, null], "highest": 0.0 })
        );
    }
}

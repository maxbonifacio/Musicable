//! Plugins in device chains: which plugin a device is, and how it's set.
//!
//! A plugin keeps its settings in its own state, bytes only it understands.
//! The project holds the state the plugin last gave, and the parameter
//! values set since, so it can always bring the plugin back as it was: when
//! the project opens, and when undo brings back a device that was deleted.

use std::fmt;
use std::sync::Arc;

use crate::PluginFormat;

/// A plugin in a device chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginDevice {
    pub format: PluginFormat,
    /// The plugin's id in its format, which finds the plugin when the
    /// project opens: CLAP's reverse-domain id, such as
    /// `org.surge-synth-team.surge-xt`.
    pub id: String,
    /// The plugin's name when it was added, to show while it's missing.
    pub name: String,
    /// Whether it plays notes. A MIDI track's notes go to its first
    /// instrument.
    pub instrument: bool,
    /// Its side inputs, in its order, as the plugin said when it last
    /// loaded, so wires into them keep their ports while it's missing. An
    /// input it no longer had then stays, after the others, if a wire went
    /// into it. Empty until it first loads.
    pub inputs: Vec<PluginInput>,
    pub settings: PluginSettings,
}

impl PluginDevice {
    /// The side input with the plugin's id `id`, if it has it.
    pub fn input(&self, id: u32) -> Option<&PluginInput> {
        self.inputs.iter().find(|input| input.id == id)
    }
}

/// One of a plugin's side inputs: an audio input beyond its main one, such
/// as a compressor's sidechain or a vocoder's carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInput {
    /// The plugin's id for it: a CLAP port's id, or a VST3 bus's index.
    pub id: u32,
    /// What the plugin calls it, such as "Sidechain".
    pub name: String,
}

/// How a plugin is set: the state it last saved, and the values of the
/// parameters set after it. The plugin gets the state first, then the
/// values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PluginSettings {
    /// `None` for a plugin as it starts, new.
    pub state: Option<PluginState>,
    /// In order of parameter id, at most one value for each.
    pub params: Vec<PluginParam>,
}

impl PluginSettings {
    /// The value set for the parameter `id`, or `None` if the state has it.
    pub fn param(&self, id: u32) -> Option<f64> {
        self.index(id).ok().map(|i| self.params[i].value)
    }

    /// Sets the parameter `id` to `value`, or leaves it to the state with
    /// `None`, and returns what it was.
    pub fn set_param(&mut self, id: u32, value: Option<f64>) -> Option<f64> {
        match (self.index(id), value) {
            (Ok(i), Some(value)) => Some(std::mem::replace(&mut self.params[i].value, value)),
            (Ok(i), None) => Some(self.params.remove(i).value),
            (Err(i), Some(value)) => {
                self.params.insert(i, PluginParam { id, value });
                None
            }
            (Err(_), None) => None,
        }
    }

    /// Whether a project file can hold the settings: the parameters in
    /// order of id, at most one value for each, and every value a finite
    /// number.
    pub fn is_valid(&self) -> bool {
        self.params.is_sorted_by(|a, b| a.id < b.id)
            && self.params.iter().all(|p| p.value.is_finite())
    }

    fn index(&self, id: u32) -> Result<usize, usize> {
        self.params.binary_search_by_key(&id, |p| p.id)
    }
}

/// A plugin parameter's value, in the plugin's own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluginParam {
    /// The plugin's id for the parameter.
    pub id: u32,
    pub value: f64,
}

/// A plugin's saved state, in the plugin's own format. Clones share the
/// bytes, so the undo history can hold many without copying them.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PluginState(Arc<[u8]>);

impl PluginState {
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PluginState {
    /// The size only: states can be megabytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PluginState({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_stay_in_order_of_id() {
        let mut settings = PluginSettings::default();
        assert_eq!(settings.set_param(7, Some(0.5)), None);
        assert_eq!(settings.set_param(3, Some(1.0)), None);
        assert_eq!(settings.set_param(7, Some(0.25)), Some(0.5));
        assert_eq!(settings.param(7), Some(0.25));
        assert_eq!(settings.param(4), None);
        let ids: Vec<u32> = settings.params.iter().map(|p| p.id).collect();
        assert_eq!(ids, [3, 7]);
        assert_eq!(settings.set_param(3, None), Some(1.0));
        assert_eq!(settings.set_param(3, None), None);
        assert_eq!(settings.params, [PluginParam { id: 7, value: 0.25 }]);
    }

    #[test]
    fn valid_settings_have_parameters_in_order_once_each() {
        let param = |id, value| PluginParam { id, value };
        let settings = |params| PluginSettings {
            state: None,
            params,
        };
        assert!(settings(vec![]).is_valid());
        assert!(settings(vec![param(1, 0.5), param(4, -2.0)]).is_valid());
        assert!(!settings(vec![param(4, 0.5), param(1, 0.5)]).is_valid());
        assert!(!settings(vec![param(1, 0.5), param(1, 0.25)]).is_valid());
        assert!(!settings(vec![param(1, f64::NAN)]).is_valid());
        assert!(!settings(vec![param(1, f64::INFINITY)]).is_valid());
    }

    #[test]
    fn states_show_their_size() {
        let state = PluginState::new(vec![1, 2, 3]);
        assert_eq!(format!("{state:?}"), "PluginState(3 bytes)");
        assert_eq!(state.clone().bytes(), [1, 2, 3]);
    }
}

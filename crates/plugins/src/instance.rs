//! A plugin of either format, as the app hosts it.

use std::path::Path;
use std::time::Instant;

use musicable_engine::{EngineConfig, Node, NodeMaker};
use musicable_model::PluginFormat;
use musicable_model::plugin::PluginInput;

use crate::{Error, ParamInfo, ParentWindow, PluginEvent, PluginInfo, Waker, clap, vst3};

/// A plugin, CLAP or VST3, loaded and initialized. The two work alike, and
/// each method here is the one of [`clap::Instance`] or [`vst3::Instance`],
/// whichever the plugin is: their docs say how each format goes about it.
pub enum Instance {
    Clap(clap::Instance),
    Vst3(vst3::Instance),
}

/// Calls the same method of whichever instance it is.
macro_rules! either {
    ($instance:expr, $plugin:ident => $call:expr) => {
        match $instance {
            Instance::Clap($plugin) => $call,
            Instance::Vst3($plugin) => $call,
        }
    };
}

impl Instance {
    /// Loads plugin `id` from the library at `path`, which is in `format`,
    /// and initializes it. `waker` is called, from any thread but an audio
    /// thread, when the plugin needs [`poll`](Self::poll) called.
    pub fn new(
        format: PluginFormat,
        path: &Path,
        id: &str,
        waker: Option<Waker>,
    ) -> Result<Self, Error> {
        Ok(match format {
            PluginFormat::Clap => Self::Clap(clap::Instance::new(path, id, waker)?),
            PluginFormat::Vst3 => Self::Vst3(vst3::Instance::new(path, id, waker)?),
        })
    }

    /// The CLAP instance, if it's a CLAP plugin.
    pub fn as_clap(&self) -> Option<&clap::Instance> {
        match self {
            Self::Clap(plugin) => Some(plugin),
            Self::Vst3(_) => None,
        }
    }

    /// The VST3 instance, if it's a VST3 plugin.
    pub fn as_vst3(&self) -> Option<&vst3::Instance> {
        match self {
            Self::Vst3(plugin) => Some(plugin),
            Self::Clap(_) => None,
        }
    }

    pub fn info(&self) -> &PluginInfo {
        either!(self, plugin => plugin.info())
    }

    /// Makes the nodes that process the plugin in the engine's graph. Every
    /// node it makes processes this instance.
    pub fn node_source(&self) -> NodeSource {
        match self {
            Self::Clap(plugin) => NodeSource::Clap(plugin.node_source()),
            Self::Vst3(plugin) => NodeSource::Vst3(plugin.node_source()),
        }
    }

    /// The plugin's parameters, as it last described them.
    pub fn params(&self) -> &[ParamInfo] {
        either!(self, plugin => plugin.params())
    }

    /// A parameter's value, as the plugin has it.
    pub fn param_value(&self, id: u32) -> Option<f64> {
        either!(self, plugin => plugin.param_value(id))
    }

    /// A value of a parameter, as the plugin would show it: `440 Hz`.
    pub fn value_to_text(&self, id: u32, value: f64) -> Option<String> {
        either!(self, plugin => plugin.value_to_text(id, value))
    }

    /// The value of a parameter someone typed, as the plugin reads it.
    pub fn text_to_value(&self, id: u32, text: &str) -> Option<f64> {
        either!(self, plugin => plugin.text_to_value(id, text))
    }

    /// Sets a parameter, which the plugin gets as it next processes, or at
    /// once if nothing processes it.
    pub fn set_param(&mut self, id: u32, value: f64) {
        either!(self, plugin => plugin.set_param(id, value))
    }

    /// Whether the plugin is active, so its nodes can process it.
    pub fn is_active(&self) -> bool {
        either!(self, plugin => plugin.is_active())
    }

    /// Whether a node renders the plugin: from the first block it plays
    /// until it stops, as its device is bypassed ([`Node::stop`]).
    pub fn is_running(&self) -> bool {
        either!(self, plugin => plugin.is_running())
    }

    /// Activates the plugin for the engine's sample rate and block size, so
    /// its nodes can process it. An active plugin is deactivated first.
    pub fn activate(&mut self, config: EngineConfig) -> Result<(), Error> {
        either!(self, plugin => plugin.activate(config))
    }

    /// Deactivates the plugin, waiting for a node that's processing it to
    /// finish its block. It fails only if a node is stuck in the plugin.
    pub fn deactivate(&mut self) -> Result<(), Error> {
        either!(self, plugin => plugin.deactivate())
    }

    /// The plugin's latency, in frames, as it last said when it activated.
    pub fn latency(&self) -> usize {
        either!(self, plugin => plugin.latency())
    }

    /// The plugin's side inputs, as it last described them: its audio
    /// inputs besides the main one.
    pub fn side_inputs(&self) -> &[PluginInput] {
        either!(self, plugin => plugin.side_inputs())
    }

    /// Switches on the side inputs with ids in `wired`, the ones wires go
    /// into, and off the others, restarting an active plugin if that changes
    /// which are on. Says whether it restarted.
    pub fn set_side_inputs(&mut self, wired: &[u32]) -> Result<bool, Error> {
        either!(self, plugin => plugin.set_side_inputs(wired))
    }

    /// Whether the plugin has a state to save, besides its parameters.
    pub fn has_state(&self) -> bool {
        either!(self, plugin => plugin.has_state())
    }

    /// The plugin's state, as it saves it: everything it needs to come back
    /// as it is.
    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        either!(self, plugin => plugin.save_state())
    }

    /// Restores state the plugin saved.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), Error> {
        either!(self, plugin => plugin.load_state(state))
    }

    /// Carries out what the plugin asked for since the last poll, and says
    /// what happened to it.
    pub fn poll(&mut self) -> Vec<PluginEvent> {
        either!(self, plugin => plugin.poll())
    }

    /// When the plugin next needs [`poll`](Self::poll), for a timer it set,
    /// if it set one.
    pub fn next_timer(&self) -> Option<Instant> {
        either!(self, plugin => plugin.next_timer())
    }

    /// Whether the plugin has a window to show.
    pub fn has_window(&self) -> bool {
        either!(self, plugin => plugin.has_window())
    }

    /// Whether the plugin's window is open.
    pub fn is_window_open(&self) -> bool {
        either!(self, plugin => plugin.is_window_open())
    }

    /// Opens the plugin's window inside `parent`, at `scale` times its
    /// natural size, and says how big it is. An open window closes first.
    pub fn open_window(&mut self, parent: ParentWindow, scale: f64) -> Result<(u32, u32), Error> {
        either!(self, plugin => plugin.open_window(parent, scale))
    }

    /// Opens the plugin's window as a window of its own, titled `title`.
    /// Only CLAP plugins have windows of their own.
    pub fn open_floating_window(&mut self, title: &str) -> Result<(), Error> {
        either!(self, plugin => plugin.open_floating_window(title))
    }

    /// Closes the plugin's window, if it's open.
    pub fn close_window(&mut self) {
        either!(self, plugin => plugin.close_window())
    }

    /// The size of the plugin's window, while it's open.
    pub fn window_size(&self) -> Option<(u32, u32)> {
        either!(self, plugin => plugin.window_size())
    }

    /// Asks the plugin's window, inside Musicable's, to take a new size, and
    /// says the size it took: the nearest it can, or the one it had if it
    /// can't resize.
    pub fn resize_window(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        either!(self, plugin => plugin.resize_window(width, height))
    }
}

/// Makes the engine's nodes for an [`Instance`]: see
/// [`Instance::node_source`].
#[derive(Debug, Clone)]
pub enum NodeSource {
    Clap(clap::NodeSource),
    Vst3(vst3::NodeSource),
}

/// The compiler makes a plugin device's node with it: see
/// [`Devices`](musicable_engine::Devices).
impl NodeMaker for NodeSource {
    fn make(&self) -> Box<dyn Node> {
        match self {
            Self::Clap(source) => source.make(),
            Self::Vst3(source) => source.make(),
        }
    }
}

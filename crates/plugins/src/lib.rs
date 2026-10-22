//! Plugin hosting: CLAP ([`clap`]) and VST3 ([`vst3`]), then ARA 2 on top
//! of VST3. The two work alike, and an [`Instance`] is a plugin of either.
//!
//! [`Instance::new`] loads a plugin's library and makes an instance of the
//! plugin. The instance lives on the thread that made it, the app's main
//! thread: CLAP and VST3 call for the main thread for everything but
//! processing, from creating the plugin and activating it for the engine's
//! sample rate and block size to its parameters, its state and its window.
//! [`Instance::node_source`] gives the engine a [`Node`] that processes the
//! plugin. Every node made from an instance processes the same plugin, so a
//! graph swap carries on where the last block left off, with nothing to
//! hand over in [`Node::adopt`]. Only one node processes at a time, and
//! while the instance reconfigures the plugin, its nodes play silence.
//!
//! What happens to a plugin away from the main thread comes back through
//! [`Instance::poll`]: a parameter someone turned in the plugin's window,
//! the plugin asking to restart with a new latency, its state changing.
//!
//! Scanning ([`scan`]) runs in a process of its own, so a plugin that
//! crashes or hangs as it loads takes only the scan down, and the scanner
//! remembers it as bad.
//!
//! `docs/plugins.md` has the threads, the rules and how plugins fit the
//! project and its undo history.
//!
//! [`Node`]: musicable_engine::Node
//! [`Node::adopt`]: musicable_engine::Node::adopt

use std::ffi::c_void;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use musicable_model::PluginFormat;
use serde::{Deserialize, Serialize};

mod bundle;
pub mod clap;
mod instance;
mod process;
pub mod scan;
#[cfg(test)]
mod tests;
mod threads;
pub mod vst3;

#[cfg(test)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

pub use instance::{Instance, NodeSource};

/// Tells the app that a plugin needs [`Instance::poll`] called, from
/// whatever thread the plugin asks on, though never an audio thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A plugin, as its library describes it: what a scan finds, and what the
/// browser lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    #[serde(with = "format_name")]
    pub format: PluginFormat,
    /// The plugin's id in its format, which projects name it by: CLAP's
    /// reverse-domain id, such as `org.surge-synth-team.surge-xt`, or
    /// VST3's class id, as 32 hex digits.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub description: String,
    /// What the plugin says it is, in its format's words: CLAP's features,
    /// such as `instrument`, `audio-effect` or `reverb`, or VST3's
    /// subcategories, such as `Instrument`, `Fx` or `Reverb`.
    pub features: Vec<String>,
    /// The library it's in.
    pub path: PathBuf,
}

impl PluginInfo {
    /// Whether the plugin plays notes.
    pub fn is_instrument(&self) -> bool {
        self.features
            .iter()
            .any(|f| f.eq_ignore_ascii_case("instrument"))
    }
}

/// A plugin parameter, as the plugin describes it.
///
/// Its values are the ones the plugin takes: CLAP's, from `min` to `max`,
/// and VST3's normalized values, from 0 to 1, whatever they stand for.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    /// The plugin's id for it, which stays the same from one version of the
    /// plugin to the next.
    pub id: u32,
    pub name: String,
    /// Where the plugin groups it, such as `Oscillators/Osc 1`, or empty.
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// How many steps its values take from `min` to `max`, evenly spaced:
    /// whole numbers for CLAP's stepped parameters, and VST3's step count.
    /// 0 for any value in between.
    pub steps: u32,
    /// The plugin doesn't want it shown.
    pub hidden: bool,
    /// The host can read it but not change it.
    pub read_only: bool,
    /// It bypasses the plugin.
    pub bypass: bool,
    pub automatable: bool,
    pub modulatable: bool,
    /// Each note's voice can be modulated on its own: CLAP's modulatable
    /// per note id.
    pub modulatable_per_note: bool,
}

/// A native window to put a plugin's window in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentWindow {
    /// A Win32 `HWND`.
    Win32(*mut c_void),
    /// A Cocoa `NSView`.
    Cocoa(*mut c_void),
    /// An X11 window.
    X11(u64),
}

/// Something that happened to a plugin, for the main thread to act on:
/// see [`Instance::poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum PluginEvent {
    /// Someone started changing a parameter in the plugin's window, such as
    /// dragging a knob.
    GestureBegin { param: u32 },
    /// A parameter changed in the plugin's window, or the plugin changed it
    /// itself. The plugin has the value already.
    ParamChanged { param: u32, value: f64 },
    /// The change [`GestureBegin`](Self::GestureBegin) started is done.
    GestureEnd { param: u32 },
    /// The plugin's state changed in a way its parameters don't say, as when
    /// someone picks a preset or loads a sample in its window.
    StateChanged,
    /// The plugin's parameters changed: which there are, what they're
    /// called or their values. [`Instance::params`] has the new list.
    ParamsChanged,
    /// The plugin restarted, as it asked to. Its latency may differ, in
    /// which case its node needs a new graph.
    Restarted { latency_changed: bool },
    /// The plugin's window asks to be this size.
    WindowResize { width: u32, height: u32 },
    /// The plugin's own window closed, or the plugin asks the host to close
    /// it.
    WindowClosed,
    /// Something went wrong with the plugin, such as it failing to restart.
    Failed(String),
}

/// What went wrong with a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// [`PluginFormat`] in files: `"clap"` or `"vst3"`.
pub(crate) mod format_name {
    use musicable_model::PluginFormat;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(format: &PluginFormat, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(name(*format))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PluginFormat, D::Error> {
        let name = String::deserialize(d)?;
        parse(&name).ok_or_else(|| serde::de::Error::custom(format!("no plugin format {name:?}")))
    }

    pub fn name(format: PluginFormat) -> &'static str {
        match format {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
        }
    }

    pub fn parse(name: &str) -> Option<PluginFormat> {
        match name {
            "clap" => Some(PluginFormat::Clap),
            "vst3" => Some(PluginFormat::Vst3),
            _ => None,
        }
    }
}

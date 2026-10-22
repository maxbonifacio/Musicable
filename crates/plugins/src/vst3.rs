//! VST3 hosting.
//!
//! A VST3 module is a `.vst3` bundle, a folder with a binary for each
//! platform inside, or on Windows also a single `.vst3` file. Its factory
//! makes the classes in it. [`Instance`] makes a plugin of one of its audio
//! module classes, with the plugin's edit controller, initialized and ready
//! to activate, and [`Instance::node_source`] makes the [`Vst3Node`]s that
//! process it in the engine's graph.
//!
//! A VST3 plugin is two parts: the component, whose audio processor
//! processes, and the edit controller, which has the parameters' names and
//! text, and the plugin's window. Some plugins are both in one object.
//! Parameter values are normalized, from 0 to 1, at both.

mod changes;
mod com;
mod host;
mod instance;
mod module;
mod node;
mod preset;
#[cfg(test)]
mod tests;

pub use instance::Instance;
pub use node::{NodeSource, Vst3Node};

pub(crate) use module::Module;

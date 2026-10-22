//! CLAP hosting.
//!
//! A CLAP library holds one or more plugins. [`Instance`] makes one,
//! initialized and ready to activate, and [`Instance::node_source`] makes
//! the [`ClapNode`]s that process it in the engine's graph.

mod events;
mod host;
mod instance;
mod library;
mod node;

pub use instance::{Instance, ParentWindow};
pub use node::{ClapNode, NodeSource};

pub(crate) use library::Library;

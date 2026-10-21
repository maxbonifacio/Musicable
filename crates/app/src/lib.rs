//! The app: the open project wired to the engine, and the commands that
//! drive it. The `musicable` binary is built on it, and the app shell (M7)
//! will be.

mod session;

pub use session::Session;

use musicable_commands::{Registry, transport};

/// Every command the app has.
pub fn commands() -> Registry {
    let mut registry = Registry::new();
    transport::register(&mut registry);
    registry
}

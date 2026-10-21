//! The view commands: what the window shows.
//!
//! Commands can't show a window, so these ask the app to, with a [`Request`].

use crate::{CommandId, Registry, Request};

/// Shows the command palette, which finds any command by name and runs it.
pub const COMMAND_PALETTE: CommandId = CommandId::new("view.command_palette");

/// Every view command, with its title and what it asks for.
const COMMANDS: [(CommandId, &str, Request); 1] = [(
    COMMAND_PALETTE,
    "Command Palette...",
    Request::CommandPalette,
)];

/// Registers the view commands.
pub fn register(registry: &mut Registry) {
    crate::register_requests(registry, &COMMANDS);
}

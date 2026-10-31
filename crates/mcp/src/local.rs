//! Tools the bridge runs itself, in its own process, rather than passing
//! them on to Musicable: the app's audio commands, and reading its log,
//! which work on files and need no window, so they work while Musicable is
//! closed or doesn't let agents in. The app implements [`LocalTools`], so this crate needn't know
//! them.

use serde_json::{Map, Value as Json};

/// A tool the bridge runs itself, as `tools/list` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalTool {
    pub name: String,
    pub title: String,
    pub description: String,
    /// The JSON Schema of its arguments, an object.
    pub input_schema: Map<String, Json>,
    /// The JSON Schema of what it gives back, an object too.
    pub output_schema: Map<String, Json>,
    /// Whether it only reads files, writing none.
    pub read_only: bool,
}

/// The tools the bridge runs itself.
pub trait LocalTools: Send + Sync + 'static {
    fn tools(&self) -> Vec<LocalTool>;

    /// What agents are told about the tools, after what they're told about
    /// the app's.
    fn instructions(&self) -> String;

    /// Runs the tool `name` with `arguments`, on a thread of its own, and
    /// gives back what it gives back, or why it failed, in a sentence.
    /// `cancelled` says whether the agent has given up on the call, which
    /// should end it soon. `progress` says how much of it is done, from 0
    /// to 1, for the agent to see.
    fn call(
        &self,
        name: &str,
        arguments: &Map<String, Json>,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(f64),
    ) -> Result<Json, String>;
}

/// No tools of the bridge's own.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLocalTools;

impl LocalTools for NoLocalTools {
    fn tools(&self) -> Vec<LocalTool> {
        Vec::new()
    }

    fn instructions(&self) -> String {
        String::new()
    }

    fn call(
        &self,
        name: &str,
        _arguments: &Map<String, Json>,
        _cancelled: &dyn Fn() -> bool,
        _progress: &dyn Fn(f64),
    ) -> Result<Json, String> {
        Err(format!("There's no tool called {name}."))
    }
}

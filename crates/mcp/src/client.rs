//! An agent's side of the pipe, for Musicable's own programs that drive a
//! Musicable as agents do, such as `musicable check`, which runs scenarios,
//! and `musicable soak`, which watches a long run, and for the bridge's
//! lifecycle tools. It calls the same tools, through the same JSON path, as
//! an agent's calls take.

use std::io;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value as Json;
use tokio::runtime::Runtime;

use crate::pipe::{self, Address};

/// A connection to the Musicable listening at an address, as an agent's.
pub struct Client {
    runtime: Runtime,
    running: RunningService<RoleClient, ClientConfig>,
}

impl Client {
    /// Connects to the Musicable listening at `address`, as the agent
    /// called `name`. Fails if none does.
    pub fn connect(address: &Address, name: &str) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("musicable-client")
            .enable_all()
            .build()?;
        let running = runtime.block_on(async {
            let stream = pipe::connect(address).await?;
            let info = Implementation::new(name, env!("CARGO_PKG_VERSION"));
            let client = ClientConfig::new(ClientCapabilities::default(), info);
            client.serve(stream).await.map_err(io::Error::other)
        })?;
        Ok(Self { runtime, running })
    }

    /// Connects as [`connect`](Self::connect) does, trying again until a
    /// Musicable listens or `wait` has passed, or `gone` says the one
    /// starting has stopped.
    pub fn connect_within(
        address: &Address,
        name: &str,
        wait: Duration,
        mut gone: impl FnMut() -> Option<String>,
    ) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match Self::connect(address, name) {
                Ok(client) => return Ok(client),
                Err(error) => {
                    if let Some(why) = gone() {
                        return Err(why);
                    }
                    if start.elapsed() >= wait {
                        return Err(format!(
                            "No Musicable answered at {address} within {} seconds: {error}",
                            wait.as_secs()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Calls the tool `name` with `arguments`, an object, and gives back
    /// what it answered, as JSON, or why it failed, as the agent reads it.
    pub fn call(&self, name: &str, arguments: Json) -> Result<Json, String> {
        let Json::Object(arguments) = arguments else {
            return Err(format!("{name}'s arguments must be an object."));
        };
        let request = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        let peer = self.running.peer().clone();
        let result = self.runtime.block_on(peer.call_tool(request));
        match result {
            Ok(result) => read(&result),
            Err(error) => Err(format!("Musicable didn't answer {name}: {error}.")),
        }
    }

    /// Whether the connection has closed, as when Musicable quits.
    pub fn is_closed(&self) -> bool {
        self.running.is_closed()
    }
}

/// Whether a Musicable listens at `address` now.
pub fn answers(address: &Address) -> bool {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime.block_on(listening(address))
}

/// Whether a Musicable listens at `address` now, in a runtime already.
pub(crate) async fn listening(address: &Address) -> bool {
    tokio::time::timeout(Duration::from_secs(2), pipe::connect(address))
        .await
        .is_ok_and(|connected| connected.is_ok())
}

/// Waits until no Musicable listens at `address`, at most `wait`. Returns
/// whether none does.
pub fn wait_until_gone(address: &Address, wait: Duration) -> bool {
    let start = Instant::now();
    while answers(address) {
        if start.elapsed() >= wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// What a tool answered, as JSON, or why it failed: its text, which is
/// JSON when it succeeds.
pub(crate) fn read(result: &CallToolResult) -> Result<Json, String> {
    let text = (result.content.first())
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default();
    match result.is_error {
        Some(true) => Err(text),
        _ => serde_json::from_str(&text)
            .map_err(|error| format!("Musicable's answer isn't JSON ({error}): {text}")),
    }
}

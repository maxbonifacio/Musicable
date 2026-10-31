//! `musicable mcp`: the bridge an agent's client starts. It speaks MCP over
//! its standard input and output, lists the tools itself, and passes each
//! tool call on to the Musicable that's running, through the pipe, as the
//! agent's client named itself. While no Musicable is running, or it
//! doesn't let agents in, the tools say so, and the bridge tries again at
//! the next call, so the agent needn't start over once the user lets it in.
//!
//! Its [local tools](LocalTools), such as the audio commands, it runs
//! itself, on threads of its own, whether Musicable is running or not. They tell the
//! agent how far they've got, if it asks, and stop when it cancels them.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
    Implementation, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RunningService, ServiceError};
use rmcp::{ErrorData, Peer, RoleClient, RoleServer, ServerHandler, ServiceExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::local::{LocalTools, NoLocalTools};
use crate::pipe::{self, Address};
use crate::tools::{self, failed};

/// What the tools say while they can't reach Musicable.
const NOT_RUNNING: &str = "Musicable isn't running, or it doesn't let agents control it. Ask \
                           the user to open Musicable and switch on \"Let agents control \
                           Musicable\" on the Agents page of its Settings window, then try \
                           again.";

/// Local tools tell the agent how far they've got at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Bridges an agent's client, on the standard input and output, to the
/// Musicable whose settings are in `config_dir`, until the client goes,
/// running the `local` tools itself.
pub fn run(config_dir: Option<&Path>, local: Arc<dyn LocalTools>) -> io::Result<()> {
    let address = match config_dir {
        Some(dir) => Address::new(dir).map_err(|error| error.to_string()),
        None => Err(
            "The system doesn't say where Musicable's settings go, so agents can't \
                     reach it."
                .to_owned(),
        ),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(
        Bridge::with_local(address, local),
        tokio::io::stdin(),
        tokio::io::stdout(),
    ))
}

/// Serves the bridge to the agent's client at the other ends of `input`
/// and `output`, until it goes.
pub async fn serve<R, W>(bridge: Bridge, input: R, output: W) -> io::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let running = (bridge.clone())
        .serve((input, output))
        .await
        .map_err(io::Error::other)?;
    // Musicable shows the agent as connected from the start.
    let agent = running
        .peer()
        .peer_info()
        .map(|info| info.client_info.clone());
    let _ = bridge.connect(agent).await;
    let _ = running.waiting().await;
    bridge.disconnect().await;
    Ok(())
}

/// The tools, passed on to Musicable, and the local tools.
#[derive(Clone)]
pub struct Bridge {
    /// Where Musicable listens, or why there's nowhere.
    address: Result<Address, String>,
    /// The connection to Musicable, once there's one.
    app: Arc<Mutex<Option<RunningService<RoleClient, ClientConfig>>>>,
    local: Arc<dyn LocalTools>,
    /// The local tools, as `tools/list` gives them, and their names.
    local_tools: Arc<Vec<Tool>>,
    local_names: Arc<BTreeSet<String>>,
}

impl Bridge {
    /// A bridge to the Musicable listening at `address`, or one whose tools
    /// say why there's nowhere to listen.
    pub fn new(address: Result<Address, String>) -> Self {
        Self::with_local(address, Arc::new(NoLocalTools))
    }

    /// A bridge as [`new`](Self::new) makes it, that runs the `local` tools
    /// itself too.
    pub fn with_local(address: Result<Address, String>, local: Arc<dyn LocalTools>) -> Self {
        let local_tools: Vec<Tool> = (local.tools().into_iter())
            .map(|tool| {
                let annotations = ToolAnnotations::new()
                    .read_only(tool.read_only)
                    .open_world(false);
                Tool::new(tool.name, tool.description, Arc::new(tool.input_schema))
                    .with_title(tool.title)
                    .with_raw_output_schema(Arc::new(tool.output_schema))
                    .annotate(annotations)
            })
            .collect();
        let local_names = local_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        Self {
            address,
            app: Arc::default(),
            local,
            local_tools: Arc::new(local_tools),
            local_names: Arc::new(local_names),
        }
    }

    /// The connection to Musicable, made now as the agent `agent` unless
    /// there's one already.
    async fn connect(&self, agent: Option<Implementation>) -> Result<Peer<RoleClient>, String> {
        let mut app = self.app.lock().await;
        if let Some(running) = app.as_ref().filter(|running| !running.is_closed()) {
            return Ok(running.peer().clone());
        }
        let address = self.address.as_ref().map_err(Clone::clone)?;
        let stream = (pipe::connect(address).await).map_err(|_| NOT_RUNNING.to_owned())?;
        let agent = agent.unwrap_or_else(|| Implementation::new("agent", ""));
        let client = ClientConfig::new(ClientCapabilities::default(), agent);
        let running = (client.serve(stream).await).map_err(|_| NOT_RUNNING.to_owned())?;
        let peer = running.peer().clone();
        *app = Some(running);
        Ok(peer)
    }

    /// Closes the connection to Musicable, if there's one.
    async fn disconnect(&self) {
        if let Some(running) = self.app.lock().await.take() {
            let _ = running.cancel().await;
        }
    }

    /// Runs the local tool the call names, on a thread of its own, sending
    /// its progress to the agent as it goes if the agent asked for it.
    async fn call_local(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let local = Arc::clone(&self.local);
        let name = request.name.to_string();
        let arguments = request.arguments.unwrap_or_default();
        let cancelled = context.ct.clone();
        let (sender, mut progress) = tokio::sync::mpsc::unbounded_channel();
        let notes = context.meta.get_progress_token().map(|token| {
            let peer = context.peer.clone();
            tokio::spawn(async move {
                while let Some(done) = progress.recv().await {
                    let note = ProgressNotificationParam::new(token.clone(), done).with_total(1.0);
                    if peer.notify_progress(note).await.is_err() {
                        break;
                    }
                }
            })
        });
        let ran = tokio::task::spawn_blocking(move || {
            let last = Cell::new(None::<Instant>);
            let progress = |done: f64| {
                let now = Instant::now();
                if last.get().is_none_or(|at| now - at >= PROGRESS_EVERY) {
                    last.set(Some(now));
                    let _ = sender.send(done.clamp(0.0, 1.0));
                }
            };
            local.call(&name, &arguments, &|| cancelled.is_cancelled(), &progress)
        })
        .await;
        // The notes go before the answer.
        if let Some(notes) = notes {
            let _ = notes.await;
        }
        match ran {
            Ok(Ok(json)) => CallToolResult::structured(json),
            Ok(Err(why)) => failed(why),
            Err(_) => failed(format!(
                "{} stopped with an error of its own: please tell Musicable's developers.",
                request.name
            )),
        }
    }
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        tools::server_info_with(&self.local.instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut list = tools::list();
        list.extend(self.local_tools.iter().cloned());
        Ok(ListToolsResult::with_all_items(list))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.local_names.contains(request.name.as_ref()) {
            return Ok(self.call_local(request, context).await.into());
        }
        let agent = context.client_info();
        let app = match self.connect(agent.clone()).await {
            Ok(app) => app,
            Err(why) => return Ok(failed(why).into()),
        };
        match app.call_tool_once(request).await {
            Ok(response) => Ok(response),
            Err(ServiceError::McpError(error)) => Err(error),
            // The connection closed: Musicable closed, or stopped letting
            // agents in, before the call or during it. If it lets them in
            // again already, it may have made the change.
            Err(_) => {
                self.disconnect().await;
                let why = match self.connect(agent).await {
                    Ok(_) => "Musicable stopped letting agents control it before it answered: \
                              get_history says whether the change was made."
                        .to_owned(),
                    Err(why) => why,
                };
                Ok(failed(why).into())
            }
        }
    }
}

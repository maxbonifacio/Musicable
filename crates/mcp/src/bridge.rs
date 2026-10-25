//! `musicable mcp`: the bridge an agent's client starts. It speaks MCP over
//! its standard input and output, lists the tools itself, and passes each
//! tool call on to the Musicable that's running, through the pipe, as the
//! agent's client named itself. While no Musicable is running, or it
//! doesn't let agents in, the tools say so, and the bridge tries again at
//! the next call, so the agent needn't start over once the user lets it in.

use std::io;
use std::path::Path;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerConfig,
};
use rmcp::service::{RequestContext, RunningService, ServiceError};
use rmcp::{ErrorData, Peer, RoleClient, RoleServer, ServerHandler, ServiceExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::pipe::{self, Address};
use crate::tools::{self, failed};

/// What the tools say while they can't reach Musicable.
const NOT_RUNNING: &str = "Musicable isn't running, or it doesn't let agents control it. Ask \
                           the user to open Musicable and switch on \"Let agents control \
                           Musicable\" on the Agents page of its Settings window, then try \
                           again.";

/// Bridges an agent's client, on the standard input and output, to the
/// Musicable whose settings are in `config_dir`, until the client goes.
pub fn run(config_dir: Option<&Path>) -> io::Result<()> {
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
        Bridge::new(address),
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

/// The tools, passed on to Musicable.
#[derive(Clone)]
pub struct Bridge {
    /// Where Musicable listens, or why there's nowhere.
    address: Result<Address, String>,
    /// The connection to Musicable, once there's one.
    app: Arc<Mutex<Option<RunningService<RoleClient, ClientConfig>>>>,
}

impl Bridge {
    /// A bridge to the Musicable listening at `address`, or one whose tools
    /// say why there's nowhere to listen.
    pub fn new(address: Result<Address, String>) -> Self {
        Self {
            address,
            app: Arc::default(),
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
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        tools::server_info()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
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

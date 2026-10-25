//! The server's side of the pipe: while the user lets agents in, the app
//! runs a host, which listens for agents and serves each one the tools, on
//! a thread of its own with Tokio.

use std::io;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use musicable_commands::Registry;
use rmcp::ServiceExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::runtime::Runtime;

use crate::Call;
use crate::pipe::{self, Address, Listener};
use crate::tools::{self, Agents, Link, Tools};

/// How long stopping waits for the connections to close, so that the pipe
/// is free for the next host at once.
const STOPPING: Duration = Duration::from_millis(500);

/// Listens for agents on the pipe and serves them the tools, until it's
/// dropped, which closes their connections. The calls they make wait for
/// the window to answer them: see [`calls`](Self::calls).
pub struct Host {
    runtime: Option<Runtime>,
    calls: Receiver<Call>,
    agents: Agents,
    address: Address,
}

impl Host {
    /// Listens for agents at `address`, and serves them the tools, which
    /// run the commands in `commands`. `wake` is called from the server's
    /// thread when a call waits for the window, and when an agent connects
    /// or goes. Fails if another Musicable listens there already.
    pub fn start(
        address: Address,
        commands: &Registry,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("musicable-agents")
            .enable_all()
            .build()?;
        let listener = runtime.block_on(pipe::listen(&address))?;
        let (sender, calls) = mpsc::channel();
        let link = Arc::new(Link {
            calls: sender,
            wake: Arc::new(wake),
            commands: tools::commands(commands),
        });
        let agents = Agents::default();
        runtime.spawn(accept(listener, link, agents.clone()));
        Ok(Self {
            runtime: Some(runtime),
            calls,
            agents,
            address,
        })
    }

    /// The calls waiting for the window to answer them, oldest first: see
    /// [`answer`](crate::answer).
    pub fn calls(&self) -> Vec<Call> {
        self.calls.try_iter().collect()
    }

    /// The agents connected, as their clients name themselves, such as
    /// `claude-code`, in the order they connected.
    pub fn agents(&self) -> Vec<String> {
        let agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        agents.values().cloned().collect()
    }

    /// Where it listens.
    pub fn address(&self) -> &Address {
        &self.address
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(STOPPING);
        }
    }
}

/// Serves each agent that connects through `listener`.
async fn accept(mut listener: Listener, link: Arc<Link>, agents: Agents) {
    let mut connections = 0;
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            // Such as when the process has too many files open: it can
            // pass.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        connections += 1;
        let tools = Tools {
            link: link.clone(),
            connection: connections,
            agents: agents.clone(),
        };
        tokio::spawn(serve(stream, tools));
    }
}

/// Serves `tools` to the agent at the other end of `stream`, until it goes.
pub(crate) async fn serve<S>(stream: S, tools: Tools)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let Tools {
        link,
        connection,
        agents,
    } = tools.clone();
    let lock = || agents.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(running) = tools.serve(stream).await else {
        return;
    };
    // The agent as its client named itself as it connected. Clients that
    // don't say so name themselves with each call.
    let name = running
        .peer()
        .peer_info()
        .map(|info| info.client_info.name.clone());
    lock().insert(connection, name.unwrap_or_else(|| "an agent".to_owned()));
    (link.wake)();
    let _ = running.waiting().await;
    lock().remove(&connection);
    (link.wake)();
}

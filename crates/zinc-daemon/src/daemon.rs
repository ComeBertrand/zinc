use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info};
use zinc_proto::{Request, Response};

use crate::agent::Agent;

pub struct Daemon {
    state: Arc<Mutex<DaemonState>>,
    socket_path: PathBuf,
}

struct DaemonState {
    agents: HashMap<String, Agent>,
    next_id: u64,
    shutdown: bool,
}

impl Daemon {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(DaemonState {
                agents: HashMap::new(),
                next_id: 1,
                shutdown: false,
            })),
            socket_path,
        }
    }

    pub async fn run(&self) -> Result<()> {
        // Create socket directory
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Remove stale socket file
        let _ = tokio::fs::remove_file(&self.socket_path).await;

        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("failed to bind socket at {:?}", self.socket_path))?;
        info!("zincd listening on {:?}", self.socket_path);

        // Write PID file next to socket
        let pid_path = self.socket_path.with_extension("pid");
        tokio::fs::write(&pid_path, std::process::id().to_string()).await?;

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let state = self.state.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, state).await {
                                    error!("client error: {}", e);
                                }
                            });
                        }
                        Err(e) => error!("accept error: {}", e),
                    }
                }
                _ = shutdown_signal(self.state.clone()) => {
                    info!("shutting down");
                    break;
                }
            }
        }

        // Cleanup
        let _ = tokio::fs::remove_file(&self.socket_path).await;
        let _ = tokio::fs::remove_file(&pid_path).await;

        Ok(())
    }
}

/// Poll until the shutdown flag is set.
async fn shutdown_signal(state: Arc<Mutex<DaemonState>>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if state.lock().await.shutdown {
            return;
        }
    }
}

/// Handle a single client connection.
/// Starts as newline-delimited JSON request/response.
/// If the client sends an Attach request, switches to raw byte streaming.
async fn handle_connection(stream: UnixStream, state: Arc<Mutex<DaemonState>>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    while buf_reader.read_line(&mut line).await? > 0 {
        let request: Request = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("invalid request: {}", e),
                };
                write_response(&mut writer, &resp).await?;
                line.clear();
                continue;
            }
        };

        // Attach takes over the connection — handle it specially
        if let Request::Attach { id, cols, rows } = request {
            // Validate and grab agent resources under the lock
            let attach_resources = {
                let state_guard = state.lock().await;
                match state_guard.agents.get(&id) {
                    Some(agent) => {
                        agent.resize(cols, rows);
                        Ok((
                            agent.subscribe(),
                            agent.scrollback_contents(),
                            agent.pty_master(),
                            agent.viewers(),
                        ))
                    }
                    None => Err(format!("agent '{}' not found", id)),
                }
            };

            match attach_resources {
                Ok((output_rx, scrollback, master, viewers)) => {
                    write_response(&mut writer, &Response::Attached).await?;
                    let buffered = buf_reader.buffer().to_vec();
                    let reader = buf_reader.into_inner();
                    return handle_attach_session(
                        reader, writer, buffered, output_rx, scrollback, master, viewers,
                    )
                    .await;
                }
                Err(msg) => {
                    write_response(&mut writer, &Response::Error { message: msg }).await?;
                }
            }
        } else {
            let response = dispatch(request, &state).await;
            write_response(&mut writer, &response).await?;
        }

        line.clear();

        // Break out after shutdown to let the daemon exit
        if state.lock().await.shutdown {
            break;
        }
    }

    Ok(())
}

/// Bidirectional raw byte relay between a client and an agent's PTY.
async fn handle_attach_session(
    mut reader: tokio::net::unix::OwnedReadHalf,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    buffered: Vec<u8>,
    mut output_rx: broadcast::Receiver<Vec<u8>>,
    scrollback: Vec<u8>,
    master: Arc<OwnedFd>,
    viewers: Arc<AtomicUsize>,
) -> Result<()> {
    viewers.fetch_add(1, Ordering::Relaxed);
    // Send scrollback so the client sees recent context
    if !scrollback.is_empty() {
        writer.write_all(&scrollback).await?;
    }

    // Forward any data buffered during the JSON handshake
    if !buffered.is_empty() {
        nix::unistd::write(&*master, &buffered)
            .map_err(|e| anyhow::anyhow!("PTY write error: {}", e))?;
    }

    let write_master = master.clone();

    // PTY output → client
    let output_task = async move {
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    if writer.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Client input → PTY
    let input_task = async move {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if nix::unistd::write(&*write_master, &buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::select! {
        _ = output_task => {}
        _ = input_task => {}
    }

    viewers.fetch_sub(1, Ordering::Relaxed);
    Ok(())
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
) -> Result<()> {
    let mut json = serde_json::to_string(response)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    Ok(())
}

async fn dispatch(request: Request, state: &Arc<Mutex<DaemonState>>) -> Response {
    match request {
        Request::Spawn {
            provider,
            dir,
            id,
            args,
        } => handle_spawn(state, provider, dir, id, args).await,
        Request::List => handle_list(state).await,
        Request::Kill { id } => handle_kill(state, &id).await,
        Request::Attach { id, .. } => Response::Error {
            message: format!(
                "attach for '{}' should be handled by connection handler",
                id
            ),
        },
        Request::Shutdown => handle_shutdown(state).await,
    }
}

async fn handle_spawn(
    state: &Arc<Mutex<DaemonState>>,
    provider: String,
    dir: PathBuf,
    id: Option<String>,
    args: Vec<String>,
) -> Response {
    let mut state = state.lock().await;

    let id = id.unwrap_or_else(|| {
        let id = format!("agent-{}", state.next_id);
        state.next_id += 1;
        id
    });

    if state.agents.contains_key(&id) {
        return Response::Error {
            message: format!("agent '{}' already exists", id),
        };
    }

    match Agent::spawn(&provider, &dir, &args) {
        Ok(agent) => {
            info!(id = %id, provider = %provider, dir = %dir.display(), "spawned agent");
            state.agents.insert(id.clone(), agent);
            Response::Spawned { id }
        }
        Err(e) => Response::Error {
            message: format!("failed to spawn agent: {}", e),
        },
    }
}

async fn handle_list(state: &Arc<Mutex<DaemonState>>) -> Response {
    let mut state = state.lock().await;
    let mut agents = Vec::new();

    for (id, agent) in state.agents.iter_mut() {
        agent.refresh_state();
        agents.push(agent.info(id));
    }

    Response::Agents { agents }
}

async fn handle_kill(state: &Arc<Mutex<DaemonState>>, id: &str) -> Response {
    let mut state = state.lock().await;

    match state.agents.get_mut(id) {
        Some(agent) => {
            if let Err(e) = agent.kill() {
                return Response::Error {
                    message: format!("failed to kill agent '{}': {}", id, e),
                };
            }
            info!(id = %id, "killed agent");
            state.agents.remove(id);
            Response::Ok
        }
        None => Response::Error {
            message: format!("agent '{}' not found", id),
        },
    }
}

async fn handle_shutdown(state: &Arc<Mutex<DaemonState>>) -> Response {
    let mut state = state.lock().await;

    // Kill all agents
    let ids: Vec<String> = state.agents.keys().cloned().collect();
    for id in &ids {
        if let Some(agent) = state.agents.get_mut(id) {
            let _ = agent.kill();
        }
    }
    state.agents.clear();
    state.shutdown = true;

    info!("shutdown requested");
    Response::Ok
}

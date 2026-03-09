use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What an agent is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Actively producing output
    Working,
    /// Waiting for user input
    Input,
    /// Running but inactive
    Idle,
    /// Exited successfully
    Done,
    /// Exited with error
    Error,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Working => write!(f, "working"),
            Self::Input => write!(f, "input"),
            Self::Idle => write!(f, "idle"),
            Self::Done => write!(f, "done"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Summary info about a running agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub provider: String,
    pub dir: PathBuf,
    pub state: AgentState,
    pub pid: Option<u32>,
    pub uptime_secs: u64,
}

/// Client -> Daemon request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Spawn {
        provider: String,
        dir: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    List,
    Kill {
        id: String,
    },
    Shutdown,
}

/// Daemon -> Client response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Spawned { id: String },
    Agents { agents: Vec<AgentInfo> },
    Ok,
    Error { message: String },
}

/// Default socket path for daemon communication.
pub fn default_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("zinc").join("sock")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".zinc").join("sock")
    }
}

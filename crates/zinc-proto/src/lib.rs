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
    #[serde(default)]
    pub viewers: usize,
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
    Attach {
        id: String,
        cols: u16,
        rows: u16,
    },
    Shutdown,
}

/// Daemon -> Client response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Spawned { id: String },
    Agents { agents: Vec<AgentInfo> },
    Attached,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_display() {
        assert_eq!(AgentState::Working.to_string(), "working");
        assert_eq!(AgentState::Input.to_string(), "input");
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Done.to_string(), "done");
        assert_eq!(AgentState::Error.to_string(), "error");
    }

    #[test]
    fn agent_state_serde_roundtrip() {
        for state in [
            AgentState::Working,
            AgentState::Input,
            AgentState::Idle,
            AgentState::Done,
            AgentState::Error,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: AgentState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn agent_state_serde_values() {
        assert_eq!(
            serde_json::to_string(&AgentState::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&AgentState::Input).unwrap(),
            "\"input\""
        );
    }

    #[test]
    fn request_spawn_roundtrip() {
        let req = Request::Spawn {
            provider: "claude".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("fix-auth".into()),
            args: vec!["--verbose".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::Spawn {
                provider,
                dir,
                id,
                args,
            } => {
                assert_eq!(provider, "claude");
                assert_eq!(dir, PathBuf::from("/tmp"));
                assert_eq!(id, Some("fix-auth".into()));
                assert_eq!(args, vec!["--verbose"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_spawn_omits_none_id() {
        let req = Request::Spawn {
            provider: "claude".into(),
            dir: PathBuf::from("/tmp"),
            id: None,
            args: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("\"id\""),
            "id:None should be omitted: {}",
            json
        );
    }

    #[test]
    fn request_list_serde() {
        let json = serde_json::to_string(&Request::List).unwrap();
        assert_eq!(json, r#"{"type":"list"}"#);
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request::List));
    }

    #[test]
    fn request_kill_serde() {
        let req = Request::Kill { id: "test".into() };
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::Kill { id } => assert_eq!(id, "test"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_shutdown_serde() {
        let json = serde_json::to_string(&Request::Shutdown).unwrap();
        assert_eq!(json, r#"{"type":"shutdown"}"#);
    }

    #[test]
    fn response_spawned_serde() {
        let resp = Response::Spawned {
            id: "fix-auth".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Spawned { id } => assert_eq!(id, "fix-auth"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_agents_serde() {
        let resp = Response::Agents {
            agents: vec![AgentInfo {
                id: "test".into(),
                provider: "claude".into(),
                dir: PathBuf::from("/tmp"),
                state: AgentState::Working,
                pid: Some(1234),
                uptime_secs: 60,
                viewers: 0,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Agents { agents } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].id, "test");
                assert_eq!(agents[0].state, AgentState::Working);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_ok_serde() {
        let json = serde_json::to_string(&Response::Ok).unwrap();
        assert_eq!(json, r#"{"type":"ok"}"#);
    }

    #[test]
    fn response_error_serde() {
        let resp = Response::Error {
            message: "boom".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("boom"));
    }

    #[test]
    fn deserialize_from_wire_format() {
        // Pin the exact wire format expected by clients
        let json = r#"{"type":"spawn","provider":"claude","dir":"/tmp","args":[]}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Spawn {
                provider,
                dir,
                id,
                args,
            } => {
                assert_eq!(provider, "claude");
                assert_eq!(dir, PathBuf::from("/tmp"));
                assert_eq!(id, None);
                assert!(args.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_attach_serde() {
        let req = Request::Attach {
            id: "test".into(),
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::Attach { id, cols, rows } => {
                assert_eq!(id, "test");
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_attached_serde() {
        let json = serde_json::to_string(&Response::Attached).unwrap();
        assert_eq!(json, r#"{"type":"attached"}"#);
    }

    #[test]
    fn deserialize_unknown_type_fails() {
        let json = r#"{"type":"explode"}"#;
        assert!(serde_json::from_str::<Request>(json).is_err());
    }

    #[test]
    fn deserialize_garbage_fails() {
        assert!(serde_json::from_str::<Request>("not json").is_err());
    }
}

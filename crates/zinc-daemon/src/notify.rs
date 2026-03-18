use std::process::Command;

use serde::Deserialize;
use tracing::{error, info};
use zinc_proto::AgentState;

/// Notification configuration from config.toml.
#[derive(Debug, Clone)]
pub struct NotifyConfig {
    /// Command template to run on state transitions.
    /// Placeholders: `{id}`, `{state}`, `{old_state}`.
    pub command: String,
    /// Which states trigger notifications.
    pub on_states: Vec<AgentState>,
}

/// Raw TOML shape for the [notify] section.
#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    notify: Option<NotifySection>,
}

#[derive(Debug, Deserialize)]
struct NotifySection {
    command: Option<String>,
    on_states: Option<Vec<String>>,
}

/// Load notify config from the standard config path.
/// Returns None if no [notify] section or no command configured.
pub fn load_notify_config() -> Option<NotifyConfig> {
    let config_path = dirs::config_dir()?.join("zinc").join("config.toml");

    let content = std::fs::read_to_string(&config_path).ok()?;
    let file: ConfigFile = toml::from_str(&content).ok()?;
    let section = file.notify?;
    let command = section.command?;

    let on_states = section
        .on_states
        .unwrap_or_else(|| vec!["input".into(), "blocked".into()])
        .into_iter()
        .filter_map(|s| parse_state(&s))
        .collect();

    Some(NotifyConfig { command, on_states })
}

fn parse_state(s: &str) -> Option<AgentState> {
    match s {
        "working" => Some(AgentState::Working),
        "blocked" => Some(AgentState::Blocked),
        "input" => Some(AgentState::Input),
        "idle" => Some(AgentState::Idle),
        _ => None,
    }
}

/// Shell-quote a string for safe interpolation into `sh -c` commands.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'/'
                | b'.'
                | b':'
                | b'@'
                | b'='
                | b'+'
                | b','
        )
    }) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Fire the notification command for a state change, if it matches.
pub fn fire_if_matching(config: &NotifyConfig, id: &str, old: AgentState, new: AgentState) {
    if !config.on_states.contains(&new) {
        return;
    }

    let cmd = config
        .command
        .replace("{id}", &shell_quote(id))
        .replace("{state}", &shell_quote(&new.to_string()))
        .replace("{old_state}", &shell_quote(&old.to_string()));

    info!(id = %id, state = %new, "firing notification");

    // Fire and forget — don't block the state monitor
    std::thread::spawn(move || {
        if let Err(e) = Command::new("sh").arg("-c").arg(&cmd).status() {
            error!(cmd = %cmd, error = %e, "notification command failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_states() {
        assert_eq!(parse_state("working"), Some(AgentState::Working));
        assert_eq!(parse_state("blocked"), Some(AgentState::Blocked));
        assert_eq!(parse_state("input"), Some(AgentState::Input));
        assert_eq!(parse_state("idle"), Some(AgentState::Idle));
        assert_eq!(parse_state("unknown"), None);
    }

    #[test]
    fn fire_matching_state() {
        let config = NotifyConfig {
            command: "echo {id} {state} {old_state}".into(),
            on_states: vec![AgentState::Input],
        };

        // Should fire — new state is Input
        fire_if_matching(&config, "test", AgentState::Working, AgentState::Input);

        // Should not fire — new state is Working, not in on_states
        fire_if_matching(&config, "test", AgentState::Input, AgentState::Working);
    }

    #[test]
    fn shell_quote_in_notification() {
        let config = NotifyConfig {
            command: "echo {id}".into(),
            on_states: vec![AgentState::Input],
        };
        // Agent with spaces in name shouldn't break the command
        fire_if_matching(&config, "my agent", AgentState::Working, AgentState::Input);
    }
}

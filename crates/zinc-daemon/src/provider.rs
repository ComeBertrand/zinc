use std::path::Path;
use std::process::Command;
use std::time::Duration;

use zinc_proto::AgentState;

/// Adapter for a specific agent tool (claude, codex, etc.).
///
/// Providers know how to launch the agent and how to detect its state.
/// Hook-based providers (e.g. Claude) return `None` from `detect_state_from_output`
/// and push state via hooks instead. PTY-heuristic providers analyze output directly.
pub trait Provider: Send + Sync {
    /// Unique name for this provider (e.g. "claude", "codex").
    fn name(&self) -> &str;

    /// Build the command to launch the agent in a directory.
    fn build_command(&self, dir: &Path, args: &[String]) -> Command;

    /// Analyze agent state from recent PTY output and time since last output.
    /// Returns `None` if this provider doesn't do output-based detection (e.g. uses hooks).
    fn detect_state_from_output(
        &self,
        recent_output: &[u8],
        idle_duration: Duration,
    ) -> Option<AgentState>;
}

/// Claude Code provider.
///
/// State detection will use hooks (configured at spawn time). Output-based
/// detection returns None — state is pushed via hook callbacks.
pub struct ClaudeProvider;

impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    fn build_command(&self, dir: &Path, args: &[String]) -> Command {
        let mut cmd = Command::new("claude");
        cmd.current_dir(dir);
        cmd.args(args);
        cmd
    }

    fn detect_state_from_output(
        &self,
        _recent_output: &[u8],
        _idle_duration: Duration,
    ) -> Option<AgentState> {
        // Claude uses hooks for state detection, not output parsing
        None
    }
}

/// Generic provider for any CLI agent.
///
/// Uses the provider name as the command and PTY activity heuristic for state detection.
pub struct GenericProvider {
    command: String,
    idle_timeout: Duration,
}

impl GenericProvider {
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            idle_timeout: Duration::from_secs(5),
        }
    }
}

impl Provider for GenericProvider {
    fn name(&self) -> &str {
        &self.command
    }

    fn build_command(&self, dir: &Path, args: &[String]) -> Command {
        let mut cmd = Command::new(&self.command);
        cmd.current_dir(dir);
        cmd.args(args);
        cmd
    }

    fn detect_state_from_output(
        &self,
        _recent_output: &[u8],
        idle_duration: Duration,
    ) -> Option<AgentState> {
        if idle_duration >= self.idle_timeout {
            Some(AgentState::Idle)
        } else {
            Some(AgentState::Working)
        }
    }
}

/// Resolve a provider name to a concrete provider.
pub fn resolve(name: &str) -> Box<dyn Provider> {
    match name {
        "claude" => Box::new(ClaudeProvider),
        other => Box::new(GenericProvider::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn claude_provider_basics() {
        let p = ClaudeProvider;
        assert_eq!(p.name(), "claude");

        let cmd = p.build_command(&PathBuf::from("/tmp"), &["--verbose".into()]);
        assert_eq!(cmd.get_program(), "claude");
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp")));
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, &["--verbose"]);
    }

    #[test]
    fn claude_returns_none_for_output_detection() {
        let p = ClaudeProvider;
        assert_eq!(
            p.detect_state_from_output(b"anything", Duration::from_secs(0)),
            None
        );
    }

    #[test]
    fn generic_provider_basics() {
        let p = GenericProvider::new("codex");
        assert_eq!(p.name(), "codex");

        let cmd = p.build_command(&PathBuf::from("/home"), &[]);
        assert_eq!(cmd.get_program(), "codex");
    }

    #[test]
    fn generic_working_when_active() {
        let p = GenericProvider::new("test");
        let state = p.detect_state_from_output(b"output", Duration::from_secs(1));
        assert_eq!(state, Some(AgentState::Working));
    }

    #[test]
    fn generic_idle_after_timeout() {
        let p = GenericProvider::new("test");
        let state = p.detect_state_from_output(b"", Duration::from_secs(6));
        assert_eq!(state, Some(AgentState::Idle));
    }

    #[test]
    fn resolve_claude() {
        let p = resolve("claude");
        assert_eq!(p.name(), "claude");
    }

    #[test]
    fn resolve_unknown_gives_generic() {
        let p = resolve("my-agent");
        assert_eq!(p.name(), "my-agent");
    }
}

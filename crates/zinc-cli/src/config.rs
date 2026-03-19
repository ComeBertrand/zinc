use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Raw TOML shape — all fields optional.
#[derive(Debug, Deserialize, Default)]
pub struct ConfigFile {
    pub spawn: Option<SpawnConfig>,
    pub daemon: Option<DaemonConfig>,
    pub notify: Option<NotifyConfig>,
}

#[derive(Debug, Deserialize)]
pub struct NotifyConfig {
    pub command: Option<String>,
    pub on_states: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SpawnConfig {
    pub agent: Option<String>,
    pub namer: Option<String>,
    pub interactive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    pub scrollback: Option<usize>,
}

/// Resolved config with defaults applied.
#[derive(Debug)]
pub struct Config {
    /// Default provider for `zinc spawn` (default: "claude").
    pub agent: String,
    /// Command template to derive agent ID from directory.
    /// `{dir}` is replaced with the shell-quoted directory path.
    pub namer: Option<String>,
    /// Whether `zinc spawn` prompts interactively for missing values (default: true).
    pub interactive: bool,
    /// Scrollback buffer size in bytes (default: 1MB).
    pub scrollback: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            namer: None,
            interactive: true,
            scrollback: 1_048_576,
        }
    }
}

/// Parse a TOML string into a resolved Config.
pub fn parse_config(toml_str: &str) -> Result<Config> {
    let file: ConfigFile = toml::from_str(toml_str)?;
    let defaults = Config::default();

    let agent = file
        .spawn
        .as_ref()
        .and_then(|s| s.agent.clone())
        .unwrap_or(defaults.agent);

    let namer = file.spawn.as_ref().and_then(|s| s.namer.clone());

    let interactive = file
        .spawn
        .as_ref()
        .and_then(|s| s.interactive)
        .unwrap_or(defaults.interactive);

    let scrollback = file
        .daemon
        .as_ref()
        .and_then(|d| d.scrollback)
        .unwrap_or(defaults.scrollback);

    Ok(Config {
        agent,
        namer,
        interactive,
        scrollback,
    })
}

/// Load config from the standard path, or return defaults if the file doesn't exist.
pub fn load_config() -> Result<Config> {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("zinc")
        .join("config.toml");

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        parse_config(&content)
    } else {
        Ok(Config::default())
    }
}

/// Shell-quote a string for safe interpolation into `sh -c` commands.
///
/// Strings containing only safe characters are returned as-is.
/// Everything else is wrapped in single quotes with internal `'` escaped.
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

/// Run the namer command, substituting `{dir}` with the shell-quoted directory.
/// Returns the first line of stdout, trimmed.
pub fn run_namer(template: &str, dir: &std::path::Path) -> Result<String> {
    let dir_str = dir.to_string_lossy();
    let cmd = template.replace("{dir}", &shell_quote(&dir_str));
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .with_context(|| format!("failed to run namer: {cmd}"))?;
    if !output.status.success() {
        anyhow::bail!("namer command failed: {cmd}");
    }
    let name = String::from_utf8(output.stdout)
        .context("namer produced non-UTF8 output")?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        anyhow::bail!("namer command produced empty output: {cmd}");
    }
    Ok(name)
}

/// Resolve the agent ID: explicit flag → namer → directory basename.
pub fn resolve_id(
    explicit: Option<String>,
    namer: Option<&str>,
    dir: &std::path::Path,
) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    if let Some(template) = namer {
        return run_namer(template, dir);
    }
    Ok(default_id_from_dir(dir))
}

/// Derive the default agent ID from a directory path.
/// Uses the directory basename (last component).
pub fn default_id_from_dir(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent".into())
}

/// Find agents running in a specific directory.
/// Returns matching agent IDs.
pub fn find_agents_in_dir(agents: &[zinc_proto::AgentInfo], dir: &std::path::Path) -> Vec<String> {
    agents
        .iter()
        .filter(|a| a.dir == dir)
        .map(|a| a.id.clone())
        .collect()
}

/// Resolved parameters for spawning an agent.
pub struct SpawnParams {
    pub agent: String,
    pub resume: bool,
    pub prompt: Option<String>,
}

/// Interactively prompt for spawn parameters.
/// Each field that's already set (via CLI flags) skips its question.
/// `reader` is injectable for testing.
pub fn interactive_spawn_params(
    reader: &mut dyn std::io::BufRead,
    writer: &mut dyn std::io::Write,
    default_agent: &str,
    cli_agent: Option<&str>,
    cli_resume: bool,
    cli_prompt: Option<&str>,
) -> Result<SpawnParams> {
    // Agent
    let agent = if let Some(a) = cli_agent {
        a.to_string()
    } else {
        write!(writer, "Agent [{}]: ", default_agent)?;
        writer.flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            default_agent.to_string()
        } else {
            trimmed.to_string()
        }
    };

    // Resume
    let resume = if cli_resume {
        true
    } else {
        write!(writer, "Resume previous session? [y/N]: ")?;
        writer.flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
    };

    // Prompt
    let prompt = if let Some(p) = cli_prompt {
        Some(p.to_string())
    } else {
        write!(writer, "Starting prompt (optional, enter to skip): ")?;
        writer.flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    Ok(SpawnParams {
        agent,
        resume,
        prompt,
    })
}

/// Known agent providers. The CLI rejects unknown providers.
pub const KNOWN_PROVIDERS: &[&str] = &["claude", "codex"];

/// Validate that a provider name is in the known list.
pub fn validate_provider(name: &str) -> anyhow::Result<()> {
    if KNOWN_PROVIDERS.contains(&name) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown agent '{}'. Known agents: {}",
            name,
            KNOWN_PROVIDERS.join(", ")
        );
    }
}

/// Initialize agent hooks in the agent's settings file.
/// Currently only supports Claude Code (~/.claude/settings.json).
pub fn init_agent_hooks(agent: &str) -> Result<()> {
    match agent {
        "claude" => init_claude_hooks(),
        _ => anyhow::bail!("init not supported for agent '{agent}'"),
    }
}

fn init_claude_hooks() -> Result<()> {
    let settings_path = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".claude")
        .join("settings.json");

    // Read existing settings or start fresh
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", settings_path.display()))?
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .context("settings.json is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks = hooks.as_object_mut().context("hooks is not an object")?;

    // Define zinc hooks to add
    let zinc_hooks: &[(&str, Option<&str>, &str)] = &[
        ("UserPromptSubmit", None, "user_prompt_submit"),
        ("Stop", None, "stop"),
        (
            "Notification",
            Some("idle_prompt"),
            "notification:idle_prompt",
        ),
        (
            "Notification",
            Some("permission_prompt"),
            "notification:permission_prompt",
        ),
    ];

    let mut added = Vec::new();
    let mut skipped = Vec::new();

    for &(event, matcher, zinc_event) in zinc_hooks {
        let hook_entry = make_hook_entry(matcher, zinc_event);

        let event_hooks = hooks.entry(event).or_insert_with(|| serde_json::json!([]));

        let arr = event_hooks
            .as_array_mut()
            .with_context(|| format!("hooks.{event} is not an array"))?;

        // Check if zinc hook already exists for this event+matcher
        if arr
            .iter()
            .any(|entry| entry_matches_zinc(entry, zinc_event))
        {
            skipped.push(format!(
                "{event}{}",
                matcher.map(|m| format!("({m})")).unwrap_or_default()
            ));
        } else {
            arr.push(hook_entry);
            added.push(format!(
                "{event}{}",
                matcher.map(|m| format!("({m})")).unwrap_or_default()
            ));
        }
    }

    // Write back
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let formatted = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, formatted.as_bytes())
        .with_context(|| format!("failed to write {}", settings_path.display()))?;

    if !added.is_empty() {
        println!("Added hooks: {}", added.join(", "));
    }
    if !skipped.is_empty() {
        println!("Already configured: {}", skipped.join(", "));
    }
    println!("Wrote {}", settings_path.display());

    Ok(())
}

fn make_hook_entry(matcher: Option<&str>, zinc_event: &str) -> serde_json::Value {
    let hook = serde_json::json!({
        "type": "command",
        "command": format!("zinc hook-notify --event {zinc_event}"),
        "timeout": 5
    });

    let mut entry = serde_json::Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".into(), serde_json::Value::String(m.into()));
    }
    entry.insert("hooks".into(), serde_json::json!([hook]));
    serde_json::Value::Object(entry)
}

/// Check if a hook entry already contains a zinc hook-notify command for this event.
fn entry_matches_zinc(entry: &serde_json::Value, zinc_event: &str) -> bool {
    let expected_cmd = format!("zinc hook-notify --event {zinc_event}");
    entry["hooks"]
        .as_array()
        .map(|hooks| {
            hooks
                .iter()
                .any(|h| h["command"].as_str() == Some(&expected_cmd))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn defaults() {
        let config = Config::default();
        assert_eq!(config.agent, "claude");
        assert!(config.namer.is_none());
        assert!(config.interactive);
        assert_eq!(config.scrollback, 1_048_576);
    }

    #[test]
    fn parse_empty_toml() {
        let config = parse_config("").unwrap();
        assert_eq!(config.agent, "claude");
        assert!(config.namer.is_none());
        assert!(config.interactive);
        assert_eq!(config.scrollback, 1_048_576);
    }

    #[test]
    fn parse_spawn_section() {
        let toml = r#"
[spawn]
agent = "codex"
namer = "basename {dir}"
interactive = false
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.agent, "codex");
        assert_eq!(config.namer.unwrap(), "basename {dir}");
        assert!(!config.interactive);
    }

    #[test]
    fn parse_partial_spawn() {
        let toml = r#"
[spawn]
agent = "codex"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.agent, "codex");
        assert!(config.namer.is_none());
        assert!(config.interactive); // default preserved
    }

    #[test]
    fn parse_daemon_section() {
        let toml = r#"
[daemon]
scrollback = 2097152
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.scrollback, 2_097_152);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[spawn]
agent = "claude"
namer = "yawn prettify {dir}"
interactive = true

[daemon]
scrollback = 524288
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.agent, "claude");
        assert_eq!(config.namer.unwrap(), "yawn prettify {dir}");
        assert!(config.interactive);
        assert_eq!(config.scrollback, 524_288);
    }

    #[test]
    fn parse_invalid_toml() {
        let result = parse_config("this is not valid toml {{{");
        assert!(result.is_err());
    }

    #[test]
    fn parse_unknown_fields_ignored() {
        let toml = r#"
[spawn]
agent = "claude"
unknown_field = "ignored"
"#;
        // serde ignores unknown fields by default
        let config = parse_config(toml).unwrap();
        assert_eq!(config.agent, "claude");
    }

    #[test]
    fn validate_known_provider() {
        assert!(validate_provider("claude").is_ok());
    }

    #[test]
    fn validate_unknown_provider() {
        let err = validate_provider("bash").unwrap_err();
        assert!(err.to_string().contains("unknown agent 'bash'"));
        assert!(err.to_string().contains("claude"));
    }

    #[test]
    fn default_id_from_regular_dir() {
        assert_eq!(
            default_id_from_dir(Path::new("/home/user/worktrees/myapp--fix-auth")),
            "myapp--fix-auth"
        );
    }

    #[test]
    fn default_id_from_root() {
        // Root has no file_name, fallback to "agent"
        assert_eq!(default_id_from_dir(Path::new("/")), "agent");
    }

    #[test]
    fn find_agents_matching_dir() {
        use zinc_proto::{AgentInfo, AgentState};

        let agents = vec![
            AgentInfo {
                id: "a".into(),
                provider: "claude".into(),
                dir: PathBuf::from("/tmp/project"),
                state: AgentState::Working,
                pid: Some(1),
                uptime_secs: 0,
                viewers: 0,
                context_percent: None,
            },
            AgentInfo {
                id: "b".into(),
                provider: "claude".into(),
                dir: PathBuf::from("/tmp/other"),
                state: AgentState::Working,
                pid: Some(2),
                uptime_secs: 0,
                viewers: 0,
                context_percent: None,
            },
            AgentInfo {
                id: "c".into(),
                provider: "claude".into(),
                dir: PathBuf::from("/tmp/project"),
                state: AgentState::Idle,
                pid: Some(3),
                uptime_secs: 0,
                viewers: 0,
                context_percent: None,
            },
        ];

        let matches = find_agents_in_dir(&agents, Path::new("/tmp/project"));
        assert_eq!(matches, vec!["a", "c"]);

        let matches = find_agents_in_dir(&agents, Path::new("/tmp/other"));
        assert_eq!(matches, vec!["b"]);

        let matches = find_agents_in_dir(&agents, Path::new("/tmp/nowhere"));
        assert!(matches.is_empty());
    }

    #[test]
    fn shell_quote_safe_string() {
        assert_eq!(shell_quote("/tmp/foo-bar"), "/tmp/foo-bar");
        assert_eq!(shell_quote("hello"), "hello");
    }

    #[test]
    fn shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_quote_spaces() {
        assert_eq!(shell_quote("/tmp/my project"), "'/tmp/my project'");
    }

    #[test]
    fn shell_quote_injection() {
        assert_eq!(shell_quote("/tmp/foo; rm -rf /"), "'/tmp/foo; rm -rf /'");
    }

    #[test]
    fn shell_quote_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn namer_with_echo() {
        let name = run_namer("echo test-name", Path::new("/tmp")).unwrap();
        assert_eq!(name, "test-name");
    }

    #[test]
    fn namer_with_basename() {
        let name = run_namer("basename {dir}", Path::new("/tmp/my-project")).unwrap();
        assert_eq!(name, "my-project");
    }

    #[test]
    fn namer_empty_output_fails() {
        let result = run_namer("echo", Path::new("/tmp"));
        assert!(result.is_err());
    }

    #[test]
    fn namer_failing_command() {
        let result = run_namer("false", Path::new("/tmp"));
        assert!(result.is_err());
    }

    #[test]
    fn resolve_id_explicit_wins() {
        let id = resolve_id(
            Some("my-id".into()),
            Some("basename {dir}"),
            Path::new("/tmp/project"),
        )
        .unwrap();
        assert_eq!(id, "my-id");
    }

    #[test]
    fn resolve_id_namer_second() {
        let id = resolve_id(None, Some("basename {dir}"), Path::new("/tmp/project")).unwrap();
        assert_eq!(id, "project");
    }

    #[test]
    fn resolve_id_basename_fallback() {
        let id = resolve_id(None, None, Path::new("/tmp/project")).unwrap();
        assert_eq!(id, "project");
    }

    /// Helper to run interactive_spawn_params with simulated stdin.
    fn interactive(
        input: &str,
        agent: Option<&str>,
        resume: bool,
        prompt: Option<&str>,
    ) -> SpawnParams {
        let mut reader = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut writer = Vec::new();
        interactive_spawn_params(&mut reader, &mut writer, "claude", agent, resume, prompt).unwrap()
    }

    #[test]
    fn interactive_all_defaults() {
        // User presses enter for everything
        let params = interactive("\n\n\n", None, false, None);
        assert_eq!(params.agent, "claude");
        assert!(!params.resume);
        assert!(params.prompt.is_none());
    }

    #[test]
    fn interactive_custom_agent() {
        let params = interactive("codex\n\n\n", None, false, None);
        assert_eq!(params.agent, "codex");
    }

    #[test]
    fn interactive_resume_yes() {
        let params = interactive("\ny\n\n", None, false, None);
        assert_eq!(params.agent, "claude");
        assert!(params.resume);
    }

    #[test]
    fn interactive_with_prompt() {
        let params = interactive("\n\nfix the bug\n", None, false, None);
        assert_eq!(params.prompt.unwrap(), "fix the bug");
    }

    #[test]
    fn interactive_flags_skip_questions() {
        // All flags provided — no stdin needed
        let params = interactive("", Some("claude"), true, Some("do stuff"));
        assert_eq!(params.agent, "claude");
        assert!(params.resume);
        assert_eq!(params.prompt.unwrap(), "do stuff");
    }

    #[test]
    fn interactive_partial_flags() {
        // Agent provided via flag, resume and prompt interactive
        let params = interactive("y\nhello world\n", Some("claude"), false, None);
        assert_eq!(params.agent, "claude");
        assert!(params.resume);
        assert_eq!(params.prompt.unwrap(), "hello world");
    }
}

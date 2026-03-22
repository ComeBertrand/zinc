use std::os::unix::process::CommandExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Raw TOML shape — all fields optional.
#[derive(Debug, Deserialize, Default)]
pub struct ConfigFile {
    pub spawn: Option<SpawnConfig>,
    pub daemon: Option<DaemonConfig>,
    pub tui: Option<TuiConfig>,
}

#[derive(Debug, Deserialize)]
pub struct SpawnConfig {
    #[serde(alias = "agent")]
    pub default_agent: Option<String>,
    pub namer: Option<String>,
    pub project_picker: Option<String>,
    pub project_resolver: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    pub scrollback: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct TuiConfig {
    pub open: Option<String>,
}

/// Resolved config with defaults applied.
#[derive(Debug)]
pub struct Config {
    /// Default provider for `zinc spawn` (default: "claude").
    pub default_agent: String,
    /// Command template to derive agent ID from directory.
    /// `{dir}` is replaced with the shell-quoted directory path.
    pub namer: Option<String>,
    /// Shell command that outputs project names/paths, one per line (e.g. "yawn list").
    pub project_picker: Option<String>,
    /// Shell command to resolve a project name to a path. `{name}` is replaced
    /// with the selected item. If absent, picker items are used as paths directly.
    pub project_resolver: Option<String>,
    /// Scrollback buffer size in bytes (default: 1MB).
    pub scrollback: usize,
    /// Command template for the TUI "open" action.
    /// Placeholders: {id}, {dir}, {provider}.
    pub open: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_agent: "claude".into(),
            namer: None,
            project_picker: None,
            project_resolver: None,
            scrollback: 1_048_576,
            open: None,
        }
    }
}

/// Parse a TOML string into a resolved Config.
pub fn parse_config(toml_str: &str) -> Result<Config> {
    let file: ConfigFile = toml::from_str(toml_str)?;
    let defaults = Config::default();

    let default_agent = file
        .spawn
        .as_ref()
        .and_then(|s| s.default_agent.clone())
        .unwrap_or(defaults.default_agent);

    let namer = file.spawn.as_ref().and_then(|s| s.namer.clone());
    let project_picker = file.spawn.as_ref().and_then(|s| s.project_picker.clone());
    let project_resolver = file.spawn.as_ref().and_then(|s| s.project_resolver.clone());

    let scrollback = file
        .daemon
        .as_ref()
        .and_then(|d| d.scrollback)
        .unwrap_or(defaults.scrollback);

    let open = file.tui.as_ref().and_then(|t| t.open.clone());

    Ok(Config {
        default_agent,
        namer,
        project_picker,
        project_resolver,
        scrollback,
        open,
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
pub(crate) fn shell_quote(s: &str) -> String {
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

/// Run the project resolver command, substituting `{name}` with the shell-quoted name.
/// Returns the resolved path as a PathBuf.
pub fn run_project_resolver(template: &str, name: &str) -> Result<std::path::PathBuf> {
    let cmd = template.replace("{name}", &shell_quote(name));
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .with_context(|| format!("failed to run project resolver: {cmd}"))?;
    if !output.status.success() {
        anyhow::bail!("project resolver command failed: {cmd}");
    }
    let path = String::from_utf8(output.stdout)
        .context("project resolver produced non-UTF8 output")?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if path.is_empty() {
        anyhow::bail!("project resolver produced empty output: {cmd}");
    }
    Ok(std::path::PathBuf::from(path))
}

/// Resolve the agent ID: explicit flag → namer → directory basename.
/// Spawn the open command for the TUI, fully detached from the current process.
/// Substitutes `{id}`, `{dir}`, `{provider}` placeholders (shell-quoted).
pub fn run_open_command(
    template: &str,
    id: &str,
    dir: &std::path::Path,
    provider: &str,
) -> Result<()> {
    let dir_str = dir.to_string_lossy();
    let cmd = template
        .replace("{id}", &shell_quote(id))
        .replace("{dir}", &shell_quote(&dir_str))
        .replace("{provider}", &shell_quote(provider));

    unsafe {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .pre_exec(|| {
                nix::unistd::setsid().map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                Ok(())
            })
            .spawn()
            .with_context(|| format!("failed to run open command: {cmd}"))?;
    }
    Ok(())
}

/// Detect the current terminal emulator and return an appropriate open command template.
/// Returns None if the terminal is not recognized.
pub fn detect_open_command() -> Option<String> {
    let term = std::env::var("TERM_PROGRAM").ok()?;
    let tmpl = match term.as_str() {
        "kitty" => "kitty --directory {dir} -e zinc attach {id}",
        "WezTerm" => "wezterm cli spawn --cwd {dir} -- zinc attach {id}",
        "Alacritty" | "alacritty" => "alacritty --working-directory {dir} -e zinc attach {id}",
        "ghostty" => "ghostty --working-directory={dir} -e zinc attach {id}",
        "Apple_Terminal" => "open -a Terminal {dir}",
        _ => return None,
    };
    Some(tmpl.into())
}

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

/// Display info for a session in the picker.
pub struct SessionDisplay {
    pub id: String,
    pub summary: String,
    pub turns: usize,
    pub age: String,
}

fn format_session_line(s: &SessionDisplay) -> String {
    format!("[{}] {} ({} turns)", s.age, s.summary, s.turns)
}

/// Show a session picker and return the selected session ID, or None for "new".
/// Uses fzf if available, otherwise falls back to a numbered list.
pub fn pick_session(sessions: &[SessionDisplay]) -> Result<Option<String>> {
    if let Ok(result) = pick_session_fzf(sessions) {
        return Ok(result);
    }
    let mut stdin = std::io::stdin().lock();
    let mut stderr = std::io::stderr();
    pick_session_fallback(&mut stdin, &mut stderr, sessions)
}

/// Try to pick a session using fzf. Returns Err if fzf is not available.
fn pick_session_fzf(sessions: &[SessionDisplay]) -> Result<Option<String>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("fzf")
        .args(["--header", "Pick session", "--height", "~50%"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut fzf_stdin = child.stdin.take().context("failed to open fzf stdin")?;
    writeln!(fzf_stdin, "new session")?;
    for s in sessions {
        writeln!(fzf_stdin, "{}", format_session_line(s))?;
    }
    drop(fzf_stdin);

    let output = child.wait_with_output()?;
    if !output.status.success() {
        // User pressed Esc/ctrl-c in fzf — treat as "new session"
        return Ok(None);
    }

    let choice = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if choice == "new session" || choice.is_empty() {
        return Ok(None);
    }

    // Match the selected line back to a session
    for s in sessions {
        if format_session_line(s) == choice {
            return Ok(Some(s.id.clone()));
        }
    }

    Ok(None)
}

/// Fallback numbered list picker when fzf is not available.
/// `reader` and `writer` are injectable for testing.
pub fn pick_session_fallback(
    reader: &mut dyn std::io::BufRead,
    writer: &mut dyn std::io::Write,
    sessions: &[SessionDisplay],
) -> Result<Option<String>> {
    writeln!(writer, "  1) new session (default)")?;
    for (i, s) in sessions.iter().enumerate() {
        writeln!(writer, "  {}) {}", i + 2, format_session_line(s))?;
    }
    write!(writer, "Pick session [1]: ")?;
    writer.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed == "1" {
        return Ok(None);
    }

    match trimmed.parse::<usize>() {
        Ok(n) if n >= 2 && n <= sessions.len() + 1 => Ok(Some(sessions[n - 2].id.clone())),
        _ => {
            writeln!(writer, "Invalid choice, starting new session.")?;
            Ok(None)
        }
    }
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
        assert_eq!(config.default_agent, "claude");
        assert!(config.namer.is_none());
        assert_eq!(config.scrollback, 1_048_576);
    }

    #[test]
    fn parse_empty_toml() {
        let config = parse_config("").unwrap();
        assert_eq!(config.default_agent, "claude");
        assert!(config.namer.is_none());
        assert_eq!(config.scrollback, 1_048_576);
    }

    #[test]
    fn parse_spawn_section() {
        let toml = r#"
[spawn]
default_agent = "codex"
namer = "basename {dir}"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.default_agent, "codex");
        assert_eq!(config.namer.unwrap(), "basename {dir}");
    }

    #[test]
    fn parse_spawn_agent_alias() {
        let toml = r#"
[spawn]
agent = "codex"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.default_agent, "codex");
        assert!(config.namer.is_none());
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
default_agent = "claude"
namer = "yawn prettify {dir}"

[daemon]
scrollback = 524288
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.default_agent, "claude");
        assert_eq!(config.namer.unwrap(), "yawn prettify {dir}");
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
default_agent = "claude"
unknown_field = "ignored"
"#;
        // serde ignores unknown fields by default
        let config = parse_config(toml).unwrap();
        assert_eq!(config.default_agent, "claude");
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

    fn make_sessions() -> Vec<SessionDisplay> {
        vec![
            SessionDisplay {
                id: "sess-1".into(),
                summary: "fix-auth-bug".into(),
                turns: 42,
                age: "2h ago".into(),
            },
            SessionDisplay {
                id: "sess-2".into(),
                summary: "add-tests".into(),
                turns: 15,
                age: "1d ago".into(),
            },
        ]
    }

    #[test]
    fn pick_session_default_is_new() {
        let sessions = make_sessions();
        let mut reader = std::io::Cursor::new(b"\n".to_vec());
        let mut writer = Vec::new();
        let result = pick_session_fallback(&mut reader, &mut writer, &sessions).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pick_session_explicit_new() {
        let sessions = make_sessions();
        let mut reader = std::io::Cursor::new(b"1\n".to_vec());
        let mut writer = Vec::new();
        let result = pick_session_fallback(&mut reader, &mut writer, &sessions).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pick_session_select_first_session() {
        let sessions = make_sessions();
        let mut reader = std::io::Cursor::new(b"2\n".to_vec());
        let mut writer = Vec::new();
        let result = pick_session_fallback(&mut reader, &mut writer, &sessions).unwrap();
        assert_eq!(result.as_deref(), Some("sess-1"));
    }

    #[test]
    fn pick_session_select_second_session() {
        let sessions = make_sessions();
        let mut reader = std::io::Cursor::new(b"3\n".to_vec());
        let mut writer = Vec::new();
        let result = pick_session_fallback(&mut reader, &mut writer, &sessions).unwrap();
        assert_eq!(result.as_deref(), Some("sess-2"));
    }

    #[test]
    fn pick_session_invalid_choice_gives_new() {
        let sessions = make_sessions();
        let mut reader = std::io::Cursor::new(b"99\n".to_vec());
        let mut writer = Vec::new();
        let result = pick_session_fallback(&mut reader, &mut writer, &sessions).unwrap();
        assert!(result.is_none());
    }
}

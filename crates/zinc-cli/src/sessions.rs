use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::SessionDisplay;

/// Metadata about a discovered session file.
struct SessionInfo {
    id: String,
    summary: String,
    turns: usize,
    modified: SystemTime,
}

/// List sessions for a given provider and working directory.
pub fn list_sessions(provider: &str, dir: &Path) -> Vec<SessionDisplay> {
    let sessions = match provider {
        "claude" => list_claude_sessions(dir),
        "codex" => list_codex_sessions(dir),
        _ => vec![],
    };
    sessions
        .into_iter()
        .map(|s| SessionDisplay {
            id: s.id,
            summary: s.summary,
            turns: s.turns,
            age: format_date(s.modified),
        })
        .collect()
}

/// Scan ~/.claude/projects/<encoded-dir>/ for .jsonl session files.
/// Extracts the first user message as a summary. Sorted by mtime descending.
fn list_claude_sessions(dir: &Path) -> Vec<SessionInfo> {
    let Some(home) = std::env::var("HOME").ok() else {
        return vec![];
    };
    let encoded = encode_claude_path(dir);
    let project_dir = PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join(&encoded);

    if !project_dir.is_dir() {
        return vec![];
    }

    let mut sessions = Vec::new();
    let entries = match std::fs::read_dir(&project_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let id = stem.to_string();
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let (summary, turns) = extract_claude_metadata(&path);
        sessions.push(SessionInfo {
            id,
            summary,
            turns,
            modified,
        });
    }

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

/// Extract custom-title and user turn count from a Claude JSONL session file.
fn extract_claude_metadata(path: &Path) -> (String, usize) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return ("unknown".into(), 0),
    };

    let mut title = None;
    let mut turns = 0;

    for line in content.lines() {
        // Count user turns
        if line.contains("\"user\"") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                if value.get("type").and_then(|t| t.as_str()) == Some("user") {
                    turns += 1;
                }
            }
        }
        // Extract custom-title (last one wins, they can be updated)
        if line.contains("\"custom-title\"") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                if value.get("type").and_then(|t| t.as_str()) == Some("custom-title") {
                    if let Some(t) = value.get("customTitle").and_then(|t| t.as_str()) {
                        title = Some(t.to_string());
                    }
                }
            }
        }
    }

    (title.unwrap_or_else(|| "untitled".into()), turns)
}

/// Scan ~/.codex/sessions/ for session files matching the given working directory.
fn list_codex_sessions(dir: &Path) -> Vec<SessionInfo> {
    let Some(home) = std::env::var("HOME").ok() else {
        return vec![];
    };
    let sessions_dir = PathBuf::from(&home).join(".codex").join("sessions");
    if !sessions_dir.is_dir() {
        return vec![];
    }

    let dir_str = dir.to_string_lossy().to_string();
    let mut sessions = Vec::new();

    // Walk YYYY/MM/DD directories
    let Ok(years) = std::fs::read_dir(&sessions_dir) else {
        return vec![];
    };
    for year in years.filter_map(|e| e.ok()) {
        if !year.path().is_dir() {
            continue;
        }
        let Ok(months) = std::fs::read_dir(year.path()) else {
            continue;
        };
        for month in months.filter_map(|e| e.ok()) {
            if !month.path().is_dir() {
                continue;
            }
            let Ok(days) = std::fs::read_dir(month.path()) else {
                continue;
            };
            for day in days.filter_map(|e| e.ok()) {
                if !day.path().is_dir() {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(day.path()) else {
                    continue;
                };
                for file in files.filter_map(|e| e.ok()) {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    // Check CWD from first line
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Some(first_line) = content.lines().next() {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(first_line)
                            else {
                                continue;
                            };
                            let cwd = value
                                .get("payload")
                                .and_then(|p| p.get("cwd"))
                                .and_then(|c| c.as_str());
                            if cwd != Some(&dir_str) {
                                continue;
                            }
                            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                                continue;
                            };
                            let modified = file
                                .metadata()
                                .ok()
                                .and_then(|m| m.modified().ok())
                                .unwrap_or(SystemTime::UNIX_EPOCH);
                            let (summary, turns) = extract_codex_metadata(&content);
                            sessions.push(SessionInfo {
                                id: stem.to_string(),
                                summary,
                                turns,
                                modified,
                            });
                        }
                    }
                }
            }
        }
    }

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

/// Extract last user message and user turn count from Codex JSONL content.
fn extract_codex_metadata(content: &str) -> (String, usize) {
    let mut last_user_msg = None;
    let mut turns = 0;

    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        if let Some(payload) = value.get("payload") {
            if payload.get("role").and_then(|r| r.as_str()) == Some("user") {
                turns += 1;
                if let Some(text) = payload
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|b| b.get("text"))
                    .and_then(|t| t.as_str())
                {
                    last_user_msg = Some(truncate(text, 60));
                }
            }
        }
    }

    (last_user_msg.unwrap_or_else(|| "unknown".into()), turns)
}

/// Encode a directory path the way Claude does: /home/user/foo → home-user-foo
fn encode_claude_path(dir: &Path) -> String {
    let s = dir.to_string_lossy();
    s.replace('/', "-")
}

/// Format a SystemTime as a relative age string.
fn format_date(time: SystemTime) -> String {
    let elapsed = time.elapsed().unwrap_or_default();
    let secs = elapsed.as_secs();

    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.lines().next().unwrap_or(s);
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_path() {
        assert_eq!(
            encode_claude_path(Path::new("/home/user/Workspace/zinc")),
            "-home-user-Workspace-zinc"
        );
    }

    #[test]
    fn format_date_recent() {
        let time = SystemTime::now();
        assert_eq!(format_date(time), "just now");
    }

    #[test]
    fn format_date_hours() {
        let time = SystemTime::now() - std::time::Duration::from_secs(7200);
        assert_eq!(format_date(time), "2h ago");
    }

    #[test]
    fn format_date_days() {
        let time = SystemTime::now() - std::time::Duration::from_secs(259200);
        assert_eq!(format_date(time), "3d ago");
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 60), "hello");
    }

    #[test]
    fn truncate_long() {
        let long = "a".repeat(100);
        let result = truncate(&long, 60);
        assert!(result.len() <= 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_claude_metadata_with_title() {
        let dir = std::env::temp_dir().join("zinc-test-meta-title");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"user","message":{"content":"fix the auth bug"}}
{"type":"custom-title","customTitle":"fix-auth-bug","sessionId":"abc"}
{"type":"user","message":{"content":"now add tests"}}"#,
        )
        .unwrap();
        let (title, turns) = extract_claude_metadata(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(title, "fix-auth-bug");
        assert_eq!(turns, 2);
    }

    #[test]
    fn extract_claude_metadata_no_title() {
        let dir = std::env::temp_dir().join("zinc-test-meta-notitle");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.jsonl");
        std::fs::write(&path, r#"{"type":"user","message":{"content":"hello"}}"#).unwrap();
        let (title, turns) = extract_claude_metadata(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(title, "untitled");
        assert_eq!(turns, 1);
    }

    #[test]
    fn extract_claude_metadata_empty() {
        let dir = std::env::temp_dir().join("zinc-test-meta-empty");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.jsonl");
        std::fs::write(&path, "").unwrap();
        let (title, turns) = extract_claude_metadata(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(title, "untitled");
        assert_eq!(turns, 0);
    }
}

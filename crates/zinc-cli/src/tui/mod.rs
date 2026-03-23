mod app;
mod ui;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use zinc_proto::{AgentInfo, ServerMessage};

use crate::client::{self, Client};
use crate::config::{self, Config};
use crate::sessions;

use self::app::{App, Mode, PickerItem, PickerState};

/// Actions returned from the event loop select.
/// Client-borrowing work (send requests) happens after the select,
/// so the borrow from `client.read_message()` is released.
enum Action {
    None,
    Quit,
    SelectNext,
    SelectPrev,
    Attach {
        id: String,
        provider: String,
    },
    Kill {
        id: String,
    },
    DoSpawn {
        dir: PathBuf,
        resume_session: Option<String>,
    },
    CustomCommand {
        name: String,
        command: String,
        id: String,
        dir: PathBuf,
        provider: String,
    },
    TogglePeek,
    RefreshPeek,
}

pub async fn run() -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "TUI requires an interactive terminal"
    );

    let config = config::load_config()?;
    let mut client = Client::connect().await?;

    // Fetch initial agent list
    let agents = fetch_agents(&mut client).await?;

    // Set up terminal
    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode().context("failed to enable raw mode")?;
    stdout
        .execute(EnterAlternateScreen)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.commands = config.commands.clone();
    app.set_agents(agents);

    let result = run_loop(&mut terminal, &mut app, &mut client, &config).await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

    result
}

/// Spawn a thread that reads crossterm events and sends them on a channel.
/// The `active` flag pauses reading during attach (so the raw relay owns stdin).
fn spawn_crossterm_reader() -> (tokio::sync::mpsc::UnboundedReceiver<Event>, Arc<AtomicBool>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let active = Arc::new(AtomicBool::new(true));
    let active_clone = active.clone();

    std::thread::spawn(move || loop {
        if !active_clone.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        // Use poll+read so we can check the active flag periodically
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => match event::read() {
                Ok(ev) => {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    });

    (rx, active)
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    client: &mut Client,
    config: &Config,
) -> Result<()> {
    let (mut ct_rx, ct_active) = spawn_crossterm_reader();
    let mut peek_timer = tokio::time::interval(Duration::from_secs(2));
    peek_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        // Wait for a crossterm event, daemon message, or peek refresh
        let action = tokio::select! {
            Some(ev) = ct_rx.recv() => {
                match ev {
                    Event::Key(key) => handle_key_event(key, app, config),
                    _ => Action::None, // Resize etc. → just redraw
                }
            }
            msg = client.read_message() => {
                match msg? {
                    ServerMessage::Event(event) => apply_event(app, event),
                    ServerMessage::Response(_) => {} // unexpected, ignore
                }
                Action::None
            }
            _ = peek_timer.tick(), if app.peek.is_some() => Action::RefreshPeek
        };

        // Execute actions that need &mut client (borrow is free after select)
        match action {
            Action::Quit => return Ok(()),
            Action::SelectNext => {
                app.select_next();
                refresh_peek(client, app).await;
            }
            Action::SelectPrev => {
                app.select_prev();
                refresh_peek(client, app).await;
            }
            Action::Attach { id, provider } => {
                ct_active.store(false, Ordering::Relaxed);
                attach_agent(terminal, &id, &provider).await?;
                ct_active.store(true, Ordering::Relaxed);
                app.set_agents(fetch_agents(client).await?);
                refresh_peek(client, app).await;
            }
            Action::DoSpawn {
                dir,
                resume_session,
            } => {
                do_spawn(client, app, config, dir, resume_session).await?;
                app.set_agents(fetch_agents(client).await?);
            }
            Action::Kill { id } => {
                kill_agent(client, app, &id).await?;
                app.set_agents(fetch_agents(client).await?);
                refresh_peek(client, app).await;
            }
            Action::CustomCommand {
                name,
                command,
                id,
                dir,
                provider,
            } => match config::run_custom_command(&command, &id, &dir, &provider) {
                Ok(()) => {
                    app.set_status(format!("{name}: {id}"), Duration::from_secs(3));
                }
                Err(e) => {
                    app.set_status(format!("{name} failed: {e}"), Duration::from_secs(5));
                }
            },
            Action::TogglePeek => {
                if app.peek.is_some() {
                    app.peek = None;
                } else {
                    app.peek = Some(String::new());
                    refresh_peek(client, app).await;
                }
            }
            Action::RefreshPeek => {
                refresh_peek(client, app).await;
            }
            Action::None => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Key event dispatch
// ---------------------------------------------------------------------------

fn handle_key_event(key: KeyEvent, app: &mut App, config: &Config) -> Action {
    if app.filter_active {
        return handle_filter_key(key, app);
    }
    if matches!(app.mode, Mode::Normal) {
        handle_normal_key(key, app, config)
    } else if matches!(app.mode, Mode::SpawnEnterPath(_)) {
        handle_enter_path_key(key, app, config)
    } else {
        handle_picker_key(key, app, config)
    }
}

fn handle_normal_key(key: KeyEvent, app: &mut App, config: &Config) -> Action {
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
        (KeyCode::Char('j') | KeyCode::Down, _) => Action::SelectNext,
        (KeyCode::Char('k') | KeyCode::Up, _) => Action::SelectPrev,
        (KeyCode::Enter, _) => {
            if let Some(agent) = app.selected_agent() {
                Action::Attach {
                    id: agent.id.clone(),
                    provider: agent.provider.clone(),
                }
            } else {
                Action::None
            }
        }
        (KeyCode::Char('/'), _) => {
            app.filter_active = true;
            Action::None
        }
        (KeyCode::Char('n'), _) => start_spawn_picker(app, config),
        (KeyCode::Char('p'), _) => Action::TogglePeek,
        (KeyCode::Char('d'), _) => {
            if let Some(agent) = app.selected_agent() {
                Action::Kill {
                    id: agent.id.clone(),
                }
            } else {
                Action::None
            }
        }
        (KeyCode::Char(c), _) => {
            if let Some(cmd) = config.commands.iter().find(|cmd| cmd.key_char() == c) {
                if let Some(agent) = app.selected_agent() {
                    Action::CustomCommand {
                        name: cmd.name.clone(),
                        command: cmd.command.clone(),
                        id: agent.id.clone(),
                        dir: agent.dir.clone(),
                        provider: agent.provider.clone(),
                    }
                } else {
                    Action::None
                }
            } else {
                Action::None
            }
        }
        _ => Action::None,
    }
}

fn handle_filter_key(key: KeyEvent, app: &mut App) -> Action {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            if app.filter.is_empty() {
                app.filter_active = false;
            } else {
                app.filter.clear();
                app.selected = 0;
            }
            Action::None
        }
        (KeyCode::Enter, _) => {
            app.filter_active = false;
            Action::None
        }
        (KeyCode::Backspace, _) => {
            app.filter.pop();
            app.selected = 0;
            Action::None
        }
        (KeyCode::Down, _) => Action::SelectNext,
        (KeyCode::Up, _) => Action::SelectPrev,
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.filter.push(c);
            app.selected = 0;
            Action::None
        }
        _ => Action::None,
    }
}

fn handle_picker_key(key: KeyEvent, app: &mut App, config: &Config) -> Action {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.mode = Mode::Normal;
            Action::None
        }
        (KeyCode::Down, _) => {
            picker_mut(app, PickerState::select_next);
            Action::None
        }
        (KeyCode::Up, _) => {
            picker_mut(app, PickerState::select_prev);
            Action::None
        }
        (KeyCode::Enter, _) => handle_picker_enter(app, config),
        (KeyCode::Backspace, _) => {
            picker_mut(app, PickerState::backspace);
            Action::None
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            picker_mut(app, |p| p.type_char(c));
            Action::None
        }
        _ => Action::None,
    }
}

fn handle_enter_path_key(key: KeyEvent, app: &mut App, config: &Config) -> Action {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.mode = Mode::Normal;
            Action::None
        }
        (KeyCode::Enter, _) => {
            let path = match &app.mode {
                Mode::SpawnEnterPath(p) => p.clone(),
                _ => return Action::None,
            };
            let dir = PathBuf::from(&path);
            if !dir.is_dir() {
                app.set_status(format!("Not a directory: {path}"), Duration::from_secs(3));
                app.mode = Mode::Normal;
                return Action::None;
            }
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            transition_to_session_picker(app, config, dir)
        }
        (KeyCode::Backspace, _) => {
            if let Mode::SpawnEnterPath(ref mut path) = app.mode {
                path.pop();
            }
            Action::None
        }
        (KeyCode::Char(c), _) => {
            if let Mode::SpawnEnterPath(ref mut path) = app.mode {
                path.push(c);
            }
            Action::None
        }
        _ => Action::None,
    }
}

/// Apply a function to the current picker state, if any.
fn picker_mut(app: &mut App, f: impl FnOnce(&mut PickerState)) {
    match &mut app.mode {
        Mode::SpawnPickProject(p) | Mode::SpawnPickSession { picker: p, .. } => f(p),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Spawn picker flow
// ---------------------------------------------------------------------------

fn start_spawn_picker(app: &mut App, config: &Config) -> Action {
    if let Some(ref cmd) = config.project_picker {
        let projects = run_project_picker(cmd);
        let cwd_display = std::env::current_dir()
            .ok()
            .map(|p| ui::shorten_home(&p.display().to_string()))
            .unwrap_or_else(|| ".".into());

        let mut items = vec![
            PickerItem {
                display: format!(". ({cwd_display})"),
                id: "__cwd__".into(),
            },
            PickerItem {
                display: "enter path...".into(),
                id: "__enter_path__".into(),
            },
        ];

        for name in projects {
            items.push(PickerItem {
                display: name.clone(),
                id: name,
            });
        }

        app.mode = Mode::SpawnPickProject(PickerState::new("Pick project", items));
        Action::None
    } else {
        // No project picker configured, use CWD
        let dir = std::env::current_dir().unwrap_or_default();
        transition_to_session_picker(app, config, dir)
    }
}

fn handle_picker_enter(app: &mut App, config: &Config) -> Action {
    // Get the selected item's id before transitioning
    let selected_id = match &app.mode {
        Mode::SpawnPickProject(p) | Mode::SpawnPickSession { picker: p, .. } => {
            p.selected_item().map(|item| item.id.clone())
        }
        _ => return Action::None,
    };

    let Some(selected_id) = selected_id else {
        return Action::None;
    };

    // Take ownership of mode to transition
    let mode = std::mem::replace(&mut app.mode, Mode::Normal);

    match mode {
        Mode::SpawnPickProject(_) => {
            if selected_id == "__enter_path__" {
                app.mode = Mode::SpawnEnterPath(String::new());
                return Action::None;
            }

            let dir = if selected_id == "__cwd__" {
                std::env::current_dir().unwrap_or_default()
            } else if let Some(ref resolver) = config.project_resolver {
                match config::run_project_resolver(resolver, &selected_id) {
                    Ok(path) => path,
                    Err(e) => {
                        app.set_status(format!("Resolver failed: {e}"), Duration::from_secs(5));
                        app.mode = Mode::Normal;
                        return Action::None;
                    }
                }
            } else {
                PathBuf::from(&selected_id)
            };
            match std::fs::canonicalize(&dir) {
                Ok(dir) => transition_to_session_picker(app, config, dir),
                Err(_) => {
                    app.set_status(
                        format!("Directory not found: {}", dir.display()),
                        Duration::from_secs(5),
                    );
                    Action::None
                }
            }
        }
        Mode::SpawnPickSession { dir, .. } => {
            let resume_session = if selected_id == "__new__" {
                None
            } else {
                Some(selected_id)
            };
            Action::DoSpawn {
                dir,
                resume_session,
            }
        }
        _ => Action::None,
    }
}

fn transition_to_session_picker(app: &mut App, config: &Config, dir: PathBuf) -> Action {
    let found = sessions::list_sessions(&config.default_agent, &dir);

    if found.is_empty() {
        // No sessions, spawn directly
        return Action::DoSpawn {
            dir,
            resume_session: None,
        };
    }

    let mut items = vec![PickerItem {
        display: "new session".into(),
        id: "__new__".into(),
    }];

    for s in &found {
        items.push(PickerItem {
            display: format!("[{}] {} ({} turns)", s.age, s.summary, s.turns),
            id: s.id.clone(),
        });
    }

    app.mode = Mode::SpawnPickSession {
        dir,
        picker: PickerState::new("Pick session", items),
    };

    Action::None
}

fn run_project_picker(command: &str) -> Vec<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect(),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Peek
// ---------------------------------------------------------------------------

/// Refresh the peek preview content if peek mode is active.
async fn refresh_peek(client: &mut Client, app: &mut App) {
    if app.peek.is_none() {
        return;
    }
    if let Some(agent) = app.selected_agent() {
        let id = agent.id.clone();
        match fetch_scrollback(client, &id).await {
            Ok(data) => app.peek = Some(data),
            Err(e) => app.peek = Some(format!("(error: {e})")),
        }
    }
}

async fn fetch_scrollback(client: &mut Client, id: &str) -> Result<String> {
    let resp = client
        .send(zinc_proto::Request::Scrollback { id: id.into() })
        .await?;
    match resp {
        zinc_proto::Response::Scrollback { data } => Ok(data),
        zinc_proto::Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("unexpected response to Scrollback"),
    }
}

// ---------------------------------------------------------------------------
// Agent actions
// ---------------------------------------------------------------------------

async fn do_spawn(
    client: &mut Client,
    app: &mut App,
    config: &Config,
    dir: PathBuf,
    resume_session: Option<String>,
) -> Result<()> {
    let id = config::resolve_id(None, config.namer.as_deref(), &dir).ok();
    let resp = client
        .send(zinc_proto::Request::Spawn {
            provider: config.default_agent.clone(),
            dir: dir.clone(),
            id,
            args: vec![],
            resume_session,
            prompt: None,
        })
        .await?;
    match resp {
        zinc_proto::Response::Spawned { id } => {
            app.set_status(format!("Spawned {id}"), Duration::from_secs(3));
        }
        zinc_proto::Response::Error { message } => {
            app.set_status(format!("Error: {message}"), Duration::from_secs(5));
        }
        _ => {}
    }
    Ok(())
}

/// Leave the TUI, attach to an agent's PTY with a status bar, then restore the TUI on detach.
async fn attach_agent(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    id: &str,
    provider: &str,
) -> Result<()> {
    // Leave alternate screen — agent output goes to the main screen
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    let (cols, rows) = client::terminal_size();

    // Set up status bar: reserve the last row via scroll region
    if rows > 2 {
        draw_status_bar(id, provider, cols, rows);
    }

    // Agent gets one fewer row (status bar takes the last one)
    let agent_rows = if rows > 2 { rows - 1 } else { rows };

    // Open a separate connection for the attach session
    let attach_client = Client::connect().await?;
    let _result = attach_client.attach_relay(id, cols, agent_rows).await;

    // Clean up whatever the agent did to terminal state
    client::reset_terminal_state();

    // Return to the TUI
    terminal.backend_mut().execute(EnterAlternateScreen)?;
    terminal.clear()?;

    Ok(())
}

/// Draw a status bar on the last terminal row and set scroll region above it.
fn draw_status_bar(id: &str, provider: &str, cols: u16, rows: u16) {
    use std::io::Write;
    let mut out = std::io::stdout();

    let bar = format!(" zinc: {id} | {provider}");
    let hint = "ctrl-]: detach ";
    let padding = (cols as usize).saturating_sub(bar.len() + hint.len());

    // Set scroll region to all rows except the last (1-indexed)
    let _ = write!(out, "\x1b[1;{}r", rows - 1);
    // Move to last row, draw status bar in reverse video
    let _ = write!(out, "\x1b[{rows};1H\x1b[7m{bar}{:padding$}{hint}\x1b[m", "");
    // Move cursor back into scroll region
    let _ = write!(out, "\x1b[1;1H");
    let _ = out.flush();
}

async fn kill_agent(client: &mut Client, app: &mut App, id: &str) -> Result<()> {
    let resp = client
        .send(zinc_proto::Request::Kill { id: id.into() })
        .await?;
    match resp {
        zinc_proto::Response::Ok => {
            app.set_status(format!("Killed {id}"), Duration::from_secs(3));
        }
        zinc_proto::Response::Error { message } => {
            app.set_status(format!("Error: {message}"), Duration::from_secs(5));
        }
        _ => {}
    }
    Ok(())
}

fn apply_event(app: &mut App, event: zinc_proto::Event) {
    match event {
        zinc_proto::Event::AgentSpawned { info, .. } => {
            app.add_agent(info);
        }
        zinc_proto::Event::StateChange { id, new, .. } => {
            app.update_state(&id, new);
        }
        zinc_proto::Event::AgentExited { id, .. } => {
            app.remove_agent(&id);
        }
        zinc_proto::Event::ContextUpdate {
            id,
            context_percent,
        } => {
            app.update_context(&id, context_percent);
        }
    }
}

async fn fetch_agents(client: &mut Client) -> Result<Vec<AgentInfo>> {
    let resp = client.send(zinc_proto::Request::List).await?;
    match resp {
        zinc_proto::Response::Agents { agents } => Ok(agents),
        zinc_proto::Response::Error { message } => anyhow::bail!("daemon error: {message}"),
        _ => anyhow::bail!("unexpected response to List"),
    }
}

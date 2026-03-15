mod app;
mod ui;

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use zinc_proto::{AgentInfo, ServerMessage};

use crate::client::{self, Client};

use self::app::App;

/// Actions returned from the event loop select.
/// Client-borrowing work (send requests) happens after the select,
/// so the borrow from `client.read_message()` is released.
enum Action {
    None,
    Quit,
    SelectNext,
    SelectPrev,
    Attach { id: String, provider: String },
    Spawn,
    Kill { id: String },
}

pub async fn run() -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "TUI requires an interactive terminal"
    );

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
    app.set_agents(agents);

    let result = run_loop(&mut terminal, &mut app, &mut client).await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

    result
}

/// Spawn a thread that reads crossterm events and sends them on a channel.
/// The `active` flag pauses reading during attach (so the raw relay owns stdin).
fn spawn_crossterm_reader() -> (
    tokio::sync::mpsc::UnboundedReceiver<Event>,
    Arc<AtomicBool>,
) {
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
) -> Result<()> {
    let (mut ct_rx, ct_active) = spawn_crossterm_reader();

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        // Wait for either a crossterm event or a daemon message
        let action = tokio::select! {
            Some(ev) = ct_rx.recv() => {
                match ev {
                    Event::Key(key) => match (key.code, key.modifiers) {
                        (KeyCode::Char('q'), _)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
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
                        (KeyCode::Char('n'), _) => Action::Spawn,
                        (KeyCode::Char('d'), _) => {
                            if let Some(agent) = app.selected_agent() {
                                Action::Kill { id: agent.id.clone() }
                            } else {
                                Action::None
                            }
                        }
                        _ => Action::None,
                    },
                    Event::Resize(_, _) => Action::None, // redraw at top of loop
                    _ => Action::None,
                }
            }
            msg = client.read_message() => {
                match msg? {
                    ServerMessage::Event(event) => apply_event(app, event),
                    ServerMessage::Response(_) => {} // unexpected, ignore
                }
                Action::None
            }
        };

        // Execute actions that need &mut client (borrow is free after select)
        match action {
            Action::Quit => return Ok(()),
            Action::SelectNext => app.select_next(),
            Action::SelectPrev => app.select_prev(),
            Action::Attach { id, provider } => {
                ct_active.store(false, Ordering::Relaxed);
                attach_agent(terminal, &id, &provider).await?;
                ct_active.store(true, Ordering::Relaxed);
                app.set_agents(fetch_agents(client).await?);
            }
            Action::Spawn => {
                spawn_agent(client, app).await?;
                app.set_agents(fetch_agents(client).await?);
            }
            Action::Kill { id } => {
                kill_agent(client, app, &id).await?;
                app.set_agents(fetch_agents(client).await?);
            }
            Action::None => {}
        }
    }
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

async fn spawn_agent(client: &mut Client, app: &mut App) -> Result<()> {
    let dir = std::env::current_dir()?;
    let resp = client
        .send(zinc_proto::Request::Spawn {
            provider: "claude".into(),
            dir,
            id: None,
            args: vec![],
            resume: false,
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

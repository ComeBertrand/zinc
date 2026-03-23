use std::io::IsTerminal;

use anyhow::Result;
use clap::Parser;

mod cli;
mod client;
mod config;
mod sessions;
mod tui;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::load_config()?;

    let command = match cli.command {
        Some(cmd) => cmd,
        None => return tui::run().await,
    };

    match command {
        Commands::Spawn {
            agent,
            dir,
            id,
            prompt,
            new,
            attach,
            args,
        } => {
            let dir = std::fs::canonicalize(&dir)
                .map_err(|e| anyhow::anyhow!("invalid directory '{}': {}", dir.display(), e))?;

            // Resolve agent: --agent flag → config.default_agent
            let agent = agent.unwrap_or_else(|| config.default_agent.clone());

            config::validate_provider(&agent)?;
            let id = Some(config::resolve_id(id, config.namer.as_deref(), &dir)?);

            // Resolve session: --new or non-terminal → None, else show picker
            let resume_session = if new || !std::io::stdin().is_terminal() {
                None
            } else {
                let found = sessions::list_sessions(&agent, &dir);
                if found.is_empty() {
                    None
                } else {
                    config::pick_session(&found)?
                }
            };

            let mut client = client::Client::connect().await?;
            let resp = client
                .send(zinc_proto::Request::Spawn {
                    provider: agent,
                    dir,
                    id,
                    args,
                    resume_session,
                    prompt,
                })
                .await?;
            match resp {
                zinc_proto::Response::Spawned { id } => {
                    if attach {
                        let client = client::Client::connect().await?;
                        client.attach(&id).await?;
                    } else {
                        println!("Spawned agent: {}", id);
                    }
                }
                zinc_proto::Response::Error { message } => {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        Commands::List { json } => {
            let mut client = client::Client::connect().await?;
            let resp = client.send(zinc_proto::Request::List).await?;
            match resp {
                zinc_proto::Response::Agents { agents } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&agents)?);
                    } else if agents.is_empty() {
                        println!("No agents running.");
                    } else {
                        println!(
                            "{:<10} {:<10} {:<15} {:<40} {:>8}  {:>7}",
                            "STATE", "AGENT", "ID", "DIRECTORY", "UPTIME", "VIEWERS"
                        );
                        for agent in agents {
                            let dir = shorten_home(&agent.dir.display().to_string());
                            println!(
                                "{:<10} {:<10} {:<15} {:<40} {:>8}  {:>7}",
                                agent.state,
                                agent.provider,
                                agent.id,
                                dir,
                                format_uptime(agent.uptime_secs),
                                agent.viewers,
                            );
                        }
                    }
                }
                zinc_proto::Response::Error { message } => {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        Commands::Attach { id } => {
            let id = match id {
                Some(id) => id,
                None => {
                    // Resolve from CWD: find agents running in current directory
                    let cwd = std::fs::canonicalize(".")
                        .map_err(|e| anyhow::anyhow!("cannot resolve current directory: {}", e))?;
                    let mut client = client::Client::connect().await?;
                    let resp = client.send(zinc_proto::Request::List).await?;
                    let agents = match resp {
                        zinc_proto::Response::Agents { agents } => agents,
                        _ => anyhow::bail!("unexpected response from daemon"),
                    };
                    let matches = config::find_agents_in_dir(&agents, &cwd);
                    match matches.len() {
                        0 => anyhow::bail!("no agent running in {}", cwd.display()),
                        1 => matches.into_iter().next().unwrap(),
                        _ => anyhow::bail!(
                            "multiple agents in {}: {}",
                            cwd.display(),
                            matches.join(", ")
                        ),
                    }
                }
            };
            let client = client::Client::connect().await?;
            client.attach(&id).await?;
        }

        Commands::Kill { id } => {
            let mut client = client::Client::connect().await?;
            let id = match id {
                Some(id) => id,
                None => {
                    let cwd = std::fs::canonicalize(".")
                        .map_err(|e| anyhow::anyhow!("cannot resolve current directory: {}", e))?;
                    let resp = client.send(zinc_proto::Request::List).await?;
                    let agents = match resp {
                        zinc_proto::Response::Agents { agents } => agents,
                        _ => anyhow::bail!("unexpected response from daemon"),
                    };
                    let matches = config::find_agents_in_dir(&agents, &cwd);
                    match matches.len() {
                        0 => anyhow::bail!("no agent running in {}", cwd.display()),
                        1 => matches.into_iter().next().unwrap(),
                        _ => anyhow::bail!(
                            "multiple agents in {}: {}",
                            cwd.display(),
                            matches.join(", ")
                        ),
                    }
                }
            };
            let resp = client
                .send(zinc_proto::Request::Kill { id: id.clone() })
                .await?;
            match resp {
                zinc_proto::Response::Ok => println!("Killed agent: {}", id),
                zinc_proto::Response::Error { message } => {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        Commands::Init { agent } => {
            config::validate_provider(&agent)?;
            config::init_agent_hooks(&agent)?;
        }

        Commands::Shutdown => {
            let mut client = client::Client::connect().await?;
            let resp = client.send(zinc_proto::Request::Shutdown).await?;
            match resp {
                zinc_proto::Response::Ok => println!("Daemon shutting down."),
                zinc_proto::Response::Error { message } => {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        Commands::Status => match client::Client::try_connect().await? {
            Some(_) => println!("Daemon is running."),
            None => {
                println!("Daemon is not running.");
                std::process::exit(1);
            }
        },

        Commands::Daemon => {
            use tracing_subscriber::EnvFilter;
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .init();
            #[cfg(unix)]
            {
                let _ = nix::unistd::setsid();
            }
            let socket_path = zinc_proto::default_socket_path();
            let d = zinc_daemon::daemon::Daemon::new(socket_path);
            return d.run().await;
        }

        Commands::HookNotify { agent, event } => {
            // Not running under zinc — silently succeed so hooks don't block the agent
            let Some(agent) = agent else {
                return Ok(());
            };
            // Best-effort: if daemon isn't running or rejects the event, don't block the agent
            if let Ok(mut client) = client::Client::connect().await {
                let _ = client
                    .send(zinc_proto::Request::HookEvent {
                        agent_id: agent,
                        event,
                    })
                    .await;
            }
        }
    }

    Ok(())
}

fn shorten_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{}", rest);
        }
    }
    path.to_string()
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(59), "59s");
        assert_eq!(format_uptime(60), "1m");
        assert_eq!(format_uptime(3599), "59m");
        assert_eq!(format_uptime(3600), "1h0m");
        assert_eq!(format_uptime(3661), "1h1m");
    }

    #[test]
    fn test_shorten_home() {
        // Can't control $HOME in parallel tests, so test the non-matching case
        assert_eq!(shorten_home("/other/path"), "/other/path");

        // Test with a known prefix
        if let Ok(home) = std::env::var("HOME") {
            let input = format!("{}/projects/foo", home);
            assert_eq!(shorten_home(&input), "~/projects/foo");

            // Exact match of HOME
            assert_eq!(shorten_home(&home), "~");
        }
    }
}

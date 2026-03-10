use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;

#[derive(Parser)]
#[command(name = "zinc", about = "Agent multiplexer for the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch a new agent
    Spawn {
        /// Agent tool to use (e.g. claude, codex)
        #[arg(long, default_value = "claude")]
        agent: String,

        /// Working directory for the agent
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// Agent ID (auto-generated if omitted)
        #[arg(long)]
        id: Option<String>,

        /// Extra arguments passed to the agent command
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// List all agents and their states
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Attach to an agent's terminal
    Attach {
        /// Agent ID
        id: String,
    },

    /// Kill an agent
    Kill {
        /// Agent ID
        id: String,
    },

    /// Stop all agents and shut down the daemon
    Shutdown,

    /// Check if the daemon is running
    Status,

    /// Notify the daemon of a hook event (called by agent hooks)
    HookNotify {
        /// Agent ID (defaults to $ZINC_AGENT_ID)
        #[arg(long, env = "ZINC_AGENT_ID")]
        agent: String,

        /// Hook event name (e.g. stop, notification:permission_prompt)
        #[arg(long)]
        event: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Spawn {
            agent,
            dir,
            id,
            args,
        } => {
            let dir = std::fs::canonicalize(&dir)
                .map_err(|e| anyhow::anyhow!("invalid directory '{}': {}", dir.display(), e))?;
            let mut client = client::Client::connect().await?;
            let resp = client
                .send(zinc_proto::Request::Spawn {
                    provider: agent,
                    dir,
                    id,
                    args,
                })
                .await?;
            match resp {
                zinc_proto::Response::Spawned { id } => {
                    println!("Spawned agent: {}", id);
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
            let client = client::Client::connect().await?;
            client.attach(&id).await?;
        }

        Commands::Kill { id } => {
            let mut client = client::Client::connect().await?;
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

        Commands::HookNotify { agent, event } => {
            let mut client = client::Client::connect().await?;
            let resp = client
                .send(zinc_proto::Request::HookEvent {
                    agent_id: agent,
                    event,
                })
                .await?;
            if let zinc_proto::Response::Error { message } = resp {
                eprintln!("Error: {}", message);
                std::process::exit(1);
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

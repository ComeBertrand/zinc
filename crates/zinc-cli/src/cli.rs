use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zinc", about = "Agent multiplexer for the terminal")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Launch a new agent
    Spawn {
        /// Agent tool to use (e.g. claude, codex)
        #[arg(long)]
        agent: Option<String>,

        /// Working directory for the agent
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// Agent ID (auto-generated if omitted)
        #[arg(long)]
        id: Option<String>,

        /// Initial prompt text (e.g. `zinc spawn "Fix this issue ..."`)
        prompt: Option<String>,

        /// Skip session picker, always start a new session
        #[arg(long)]
        new: bool,

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
        /// Agent ID (resolved from current directory if omitted)
        id: Option<String>,
    },

    /// Kill an agent
    Kill {
        /// Agent ID
        id: String,
    },

    /// Configure agent hooks for state detection
    Init {
        /// Agent to configure (e.g. claude)
        #[arg(long)]
        agent: String,
    },

    /// Stop all agents and shut down the daemon
    Shutdown,

    /// Check if the daemon is running
    Status,

    /// Notify the daemon of a hook event (called by agent hooks).
    /// Silently exits if not running under zinc (no ZINC_AGENT_ID).
    HookNotify {
        /// Agent ID (defaults to $ZINC_AGENT_ID; exits quietly if absent)
        #[arg(long, env = "ZINC_AGENT_ID")]
        agent: Option<String>,

        /// Hook event name (e.g. stop, notification:permission_prompt)
        #[arg(long)]
        event: String,
    },
}

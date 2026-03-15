# zinc - Agent multiplexer for the terminal

[![CI](https://github.com/ComeBertrand/zinc/actions/workflows/ci.yml/badge.svg)](https://github.com/ComeBertrand/zinc/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/zinc-cli.svg)](https://crates.io/crates/zinc-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Manage AI coding agents as persistent background daemons. Attach, detach, and switch between agent sessions instantly. Think tmux, but purpose-built for AI coding agents.

## Features

- **Never lose a session** — agents run as background daemons, survive terminal close
- **TUI supervisor** — full-screen view of all agents, keyboard-driven
- **Instant switching** — one keystroke to attach/detach from any agent
- **State tracking** — see at a glance which agents are working, waiting for input, or blocked
- **Interactive spawn** — guided agent creation with resume and prompt support
- **Composable** — works with any worktree/project workflow (designed to pair with [yawn](https://github.com/ComeBertrand/yawn))

## Quick start

```bash
cargo install zinc-cli zinc-daemon
```

```bash
# Spawn a Claude agent in your project
cd ~/projects/myapp
zinc spawn

# Or with a prompt
zinc spawn --prompt "fix the failing auth tests"

# Attach (resolves from current directory)
zinc attach

# Open the TUI to see all agents
zinc
```

## TUI

Running `zinc` with no arguments opens the interactive supervisor:

```
 zinc — 3 agents (1 needs input)
  STATE     AGENT      ID              DIRECTORY                          UPTIME
  ● work    claude     fix-auth        ~/worktrees/myapp--fix-auth          12m
▸ ▲ input   claude     fix-nav         ~/worktrees/myapp--fix-nav            8m
  ● work    claude     fix-api         ~/worktrees/myapp--fix-api            3m
 enter:attach  n:new  d:kill  q:quit
```

- `j`/`k` or arrows to navigate
- `enter` to attach to the selected agent
- `n` to spawn a new agent
- `d` to kill the selected agent
- `q` to quit (agents keep running)

When attached, a status bar shows the agent info. `ctrl-]` detaches back to the list.

## CLI commands

```bash
zinc                              # open TUI
zinc spawn [options]              # launch a new agent
zinc attach [id]                  # attach to an agent (resolves from CWD if omitted)
zinc list [--json]                # list all agents and their states
zinc kill <id>                    # stop an agent
zinc shutdown                     # stop all agents and the daemon
zinc status                       # check if the daemon is running
```

### `zinc spawn`

```bash
zinc spawn                                    # interactive: prompts for options
zinc spawn --prompt "fix the bug"             # start with a prompt
zinc spawn --resume                           # resume previous conversation
zinc spawn --resume --prompt "now the tests"  # resume + new prompt
zinc spawn -y                                 # skip prompts, use defaults
zinc spawn --agent claude --dir ~/project     # explicit agent and directory
```

**Flags:**
- `--agent <name>` — provider to use (default: from config, or `claude`)
- `--dir <path>` — working directory (default: current directory)
- `--id <name>` — agent ID (default: derived from directory name)
- `--resume` — resume previous conversation
- `--prompt <text>` — initial prompt
- `--yes` / `-y` — skip interactive prompts

### `zinc attach`

```bash
zinc attach fix-auth              # attach by ID
zinc attach                       # attach to the agent in current directory
```

When no ID is given, zinc finds the agent running in your current directory. Errors clearly if there are zero or multiple agents.

## State detection

zinc uses layered detection to track what each agent is doing:

| State | Meaning |
|---|---|
| `working` | Actively producing output |
| `blocked` | Needs user action (e.g. permission prompt) |
| `input` | Waiting for new user prompt |
| `idle` | Running but inactive |

For Claude Code, state is detected via hooks (immediate, distinguishes `input` from `blocked`). For other agents, PTY activity heuristics are used as a fallback.

## Configuration

`~/.config/zinc/config.toml` — all fields optional:

```toml
[spawn]
agent = "claude"                       # default provider
namer = "yawn prettify {dir}"          # derive agent ID from directory
interactive = true                     # prompt for missing values on spawn

[daemon]
scrollback = 1048576                   # scrollback buffer size in bytes (1MB)

[keys]
detach = "ctrl-]"                      # detach keybinding
```

### Namer

The `spawn.namer` field runs a command to derive the agent ID from the directory. `{dir}` is replaced with the shell-quoted path:

```toml
namer = "basename {dir}"               # use directory name as-is
namer = "yawn prettify {dir}"          # use yawn for clean names
```

Fallback chain: `--id` flag > namer > directory basename.

## Composing with yawn

zinc and [yawn](https://github.com/ComeBertrand/yawn) are independent tools that compose through directories:

```bash
# Create worktree + spawn agent
yawn create fix-auth --init
cd "$(yawn resolve fix-auth)"
zinc spawn --prompt "fix the auth bug"

# Later: attach from the worktree
cd "$(yawn resolve fix-auth)"
zinc attach
```

Shell function for the common case:

```bash
yagent() {
  yawn create "$1" --init
  zinc spawn --dir "$(yawn resolve "$1")" -y "${@:2}"
}

yagent fix-auth --prompt "fix the auth bug"
```

## Architecture

zinc uses a daemon-client architecture (like tmux):

- **`zincd`** — long-running daemon that owns agent PTYs, tracks state, broadcasts events
- **`zinc`** — short-lived client that connects to the daemon via Unix socket

The daemon starts automatically on first `zinc` command and keeps agents alive independently of any terminal.

## Install

### From source

```bash
cargo install zinc-cli zinc-daemon
```

### GitHub Releases

Download binaries from the [releases page](https://github.com/ComeBertrand/zinc/releases).

### Nix flake

```nix
inputs.zinc.url = "github:ComeBertrand/zinc";

# then in your packages:
inputs.zinc.packages.${system}.default
```

## License

MIT

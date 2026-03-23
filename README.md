# Zinc Is Not Cowork

> Agent multiplexer for the terminal

[![CI](https://github.com/ComeBertrand/zinc/actions/workflows/ci.yml/badge.svg)](https://github.com/ComeBertrand/zinc/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/zinc-cli.svg)](https://crates.io/crates/zinc-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Manage AI coding agents as persistent background daemons. Attach, detach, and switch between agent sessions instantly.

**Requires Linux or macOS.** zinc uses Unix PTYs and is not available on Windows.

## How it works

zinc runs agents as background daemons — independent of any terminal. The TUI shows all agents and their state. Attach to one for full-screen interaction. Detach to return to the overview. Agents keep running either way.

zinc is not a terminal multiplexer. It works inside tmux, zellij, or a bare terminal. It manages agents, not panes.

## Features

- **Never lose a session** - agents run as background daemons, survive terminal close
- **Supervisor dashboard** - see all agents at a glance, with live state updates
- **Pop-in/pop-out** - full-screen attach to any agent, one keystroke to return to the overview
- **State tracking** - know which agents are working, waiting for input, or blocked on a permission prompt
- **Session picker** - resume previous sessions or start fresh, with automatic session discovery
- **Composable** - works in any terminal, alongside any workflow

## Quick start

```bash
cargo install zinc-cli
```

```bash
# Set up Claude Code hooks for accurate state detection
zinc init --agent claude

# Spawn a Claude agent in your project
cd ~/projects/myapp
zinc spawn

# Or spawn with a prompt and attach immediately
zinc spawn -A "fix the failing auth tests"

# Attach (resolves from current directory)
zinc attach

# Open the TUI to see all agents
zinc
```

## TUI

Running `zinc` with no arguments opens the interactive supervisor:

```
 zinc -3 agents (1 needs input)
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
- `p` to toggle scrollback preview
- `/` to filter agents by ID, provider, or directory
- `q` to quit (agents keep running)

When attached, a status bar shows the agent info. `ctrl-]` detaches back to the list.

## CLI commands

```bash
zinc                              # open TUI
zinc spawn [prompt] [options]     # launch a new agent
zinc attach [id]                  # attach to an agent (resolves from CWD if omitted)
zinc list [--json]                # list all agents and their states
zinc kill [id]                    # stop an agent (resolves from CWD if omitted)
zinc init --agent <name>          # configure agent hooks for state detection
zinc shutdown                     # stop all agents and the daemon
zinc status                       # check if the daemon is running
```

### `zinc spawn`

```bash
zinc spawn                                    # shows session picker if sessions exist
zinc spawn "fix the bug"                      # start with a prompt
zinc spawn -A "fix the bug"                   # spawn and attach immediately
zinc spawn --new                              # skip session picker, always start fresh
zinc spawn --new "fix the bug"                # new session with a prompt
zinc spawn --agent codex --dir ~/project      # explicit agent and directory
```

**Arguments:**
- `<prompt>` - initial prompt text (positional, optional)

**Flags:**
- `-a`/`--agent <name>` - provider to use (default: from config, or `claude`)
- `-d`/`--dir <path>` - working directory (default: current directory)
- `-i`/`--id <name>` - agent ID (default: derived from directory name)
- `-n`/`--new` - skip session picker, always start a new session
- `-A`/`--attach` - attach to the agent immediately after spawning

### `zinc attach`

```bash
zinc attach fix-auth              # attach by ID
zinc attach                       # attach to the agent in current directory
```

When no ID is given, zinc finds the agent running in your current directory. Errors clearly if there are zero or multiple agents.

### `zinc kill`

```bash
zinc kill fix-auth                # kill by ID
zinc kill                         # kill the agent in current directory
```

Same CWD resolution as `zinc attach`.

### `zinc init`

```bash
zinc init --agent claude          # configure Claude Code hooks
```

Sets up agent hooks for accurate state detection. For Claude Code, this writes hook entries to `~/.claude/settings.json` so zinc can distinguish between `working`, `input`, and `blocked` states. Without this, zinc falls back to PTY activity heuristics.

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

`~/.config/zinc/config.toml` - all fields optional:

```toml
[spawn]
default_agent = "claude"               # default provider (alias: agent)
namer = "yawn prettify {dir}"          # derive agent ID from directory

[daemon]
scrollback = 1048576                   # scrollback buffer size in bytes (1MB)

[notify]
command = "notify-send 'zinc: {id}' '{state}'"   # command to run on state change
on_states = ["input", "blocked"]                  # which states trigger (default)
```

### Custom TUI commands

```toml
[[tui.commands]]
name = "open in editor"
key = "o"
command = "code {dir}"
```

Adds a keybinding to the TUI that runs a shell command for the selected agent. Placeholders: `{id}`, `{dir}`, `{provider}`. Reserved keys (`q`, `j`, `k`, `n`, `p`, `d`, `/`) cannot be used.

### Project picker

```toml
[spawn]
project_picker = "yawn list"                  # command that outputs project names, one per line
project_resolver = "yawn resolve {name}"      # resolves a name to a directory path
```

When configured, pressing `n` in the TUI shows a project picker before spawning. The picker lists items from `project_picker` output. If `project_resolver` is set, the selected item is resolved to a directory path; otherwise, items are treated as paths directly.

### Notifications

The `notify.command` field runs a command when an agent transitions to a matching state. Placeholders `{id}`, `{state}`, `{old_state}` are shell-quoted and substituted.

```toml
# Linux (libnotify)
command = "notify-send 'zinc: {id}' '{state}'"

# macOS
command = "osascript -e 'display notification \"{state}\" with title \"zinc: {id}\"'"
```

### Namer

The `spawn.namer` field runs a command to derive the agent ID from the directory. `{dir}` is replaced with the shell-quoted path:

```toml
namer = "basename {dir}"               # use directory name as-is
namer = "yawn prettify {dir}"          # use yawn for clean names
```

Fallback chain: `--id` flag > namer > directory basename.

## Composing with other tools

zinc operates on directories, so it composes naturally with any worktree or project manager. See [yawn](https://github.com/ComeBertrand/yawn) for an example of worktree-based workflows with zinc.

## Architecture

zinc uses a daemon-client architecture:

- **`zinc daemon`** - long-running daemon that owns agent PTYs, tracks state, broadcasts events
- **`zinc`** - short-lived client that connects to the daemon via Unix socket

The daemon starts automatically on first `zinc` command and shuts down automatically after 30 seconds of inactivity (no agents, no connected clients). It keeps agents alive independently of any terminal.

## Install

### From source

```bash
cargo install zinc-cli
```

### GitHub Releases

Download binaries from the [releases page](https://github.com/ComeBertrand/zinc/releases).

### Nix flake

```nix
inputs.zinc.url = "github:ComeBertrand/zinc";

# then in your packages:
inputs.zinc.packages.${system}.default
```

## Shell completion & man page

Shell completions are generated at build time:

```bash
# Bash
cp completions/zinc.bash ~/.local/share/bash-completion/completions/zinc

# Zsh (or place it anywhere in your $fpath)
cp completions/_zinc ~/.local/share/zsh/site-functions/_zinc

# Fish
cp completions/zinc.fish ~/.config/fish/completions/zinc.fish
```

A man page is generated at build time:

```bash
man target/*/build/zinc-cli-*/out/man/zinc.1
```

## FAQ

**Can I use this with tmux/zellij?**

Yes. zinc is a daemon-client tool, not a terminal multiplexer. It works in any terminal, including inside tmux or zellij panes.

**Why not just run agents in tmux panes?**

You can. zinc adds state tracking (which agent needs you?), session resume, and a structured lifecycle (spawn/kill/list by ID). If you run 3+ agents, the supervisor view saves a lot of context-switching.

## License

MIT

# zinc — an agent multiplexer for the terminal

> zinc is not claude

## Vision

AI coding agents (Claude Code, Codex, OpenCode, Gemini CLI) are becoming a core part of the development workflow. Developers increasingly run multiple agents in parallel — each working on a separate task in its own worktree. But managing these agents is manual and fragile: agents die when terminals close, there's no unified view of what's running, and switching between agent sessions requires hunting through tabs or tmux panes.

zinc is a terminal tool that solves this. It manages agent processes as persistent background daemons, provides a TUI to monitor their status, and lets you attach/detach from any agent session instantly. Think of it as tmux, but purpose-built for AI coding agents.

## Goals

1. **Never lose an agent session.** Agents run as background daemons. Closing your terminal, quitting zinc, or logging out does not kill them. You can reattach at any time.

2. **Know when you're needed.** The TUI shows the state of every agent at a glance: working, waiting for input, idle, done, errored. Notifications alert you when an agent needs attention.

3. **Switch instantly.** One keystroke to attach to any agent. One keystroke to detach back to the overview. No hunting through tabs.

4. **Agent-agnostic.** Claude Code first, but the architecture supports any CLI-based agent through a provider system. Adding a new agent should require minimal code.

5. **Composable, not coupled.** zinc manages agent processes. It does not manage workspaces, git branches, or project discovery. It composes with tools like yawn (worktree manager), but does not depend on them.

## Non-goals

- **Orchestration.** zinc does not assign work to agents, generate prompts, or manage GitHub issues. It supervises, it does not direct.
- **IDE integration.** zinc is a terminal tool. It does not embed into VSCode, Cursor, or other editors.
- **Remote agents.** v1 targets local agents only. Distributed/cloud agent management is out of scope.

## Core concepts

### Agent

A running instance of an AI coding tool (Claude Code, Codex, etc.) operating in a specific directory. An agent is a process attached to a PTY that zinc owns. Agents run independently of any terminal — they are background daemons.

An agent has:
- **ID** — a short, unique identifier (e.g. `fix-auth`, auto-generated or user-provided)
- **Provider** — which tool is running (claude, codex, etc.)
- **Directory** — the working directory (typically a repo or worktree)
- **State** — what the agent is doing right now
- **PTY** — the pseudo-terminal zinc allocated for it
- **Scrollback** — a buffer of recent output

### State

What an agent is doing at any given moment:

| State | Meaning |
|---|---|
| `working` | Agent is actively producing output (generating code, running tools) |
| `input` | Agent is idle, waiting for user input |
| `idle` | Agent process is running but inactive (no prompt visible, no output) |
| `done` | Agent process exited successfully (exit code 0) |
| `error` | Agent process exited with a non-zero exit code |

State detection is the core technical challenge. See the [State detection](#state-detection) section for details.

### Provider

An adapter that knows how to work with a specific agent tool. A provider defines:
- How to launch the agent (command, arguments, environment)
- How to detect the agent's state (parsing output patterns, recognizing prompts)
- How to identify the agent if discovered externally (process name, signatures)

The provider system allows zinc to support new agents without changing core logic. Each provider implements a trait/interface, and zinc dispatches to the appropriate one based on configuration.

### Viewport

A terminal connected to an agent's PTY. When you "attach" to an agent, zinc creates a viewport: your terminal's input goes to the agent's PTY, the agent's output streams to your terminal. When you "detach", the viewport closes but the agent continues running.

Properties:
- **Multiple viewports per agent.** Two people (or terminals) can attach to the same agent simultaneously.
- **Zero viewports.** An agent with no one watching still runs. zinc tracks its state in the background.
- **Ephemeral.** Viewports are created and destroyed freely. The agent's lifecycle is independent.

## Architecture

zinc uses a daemon-client architecture, similar to tmux:

```
                    ┌──────────────────────────────┐
                    │          zincd               │
                    │       (daemon)               │
                    │                              │
                    │  ┌───────┐ ┌───────┐         │
                    │  │agent 1│ │agent 2│  ...    │
                    │  │  PTY  │ │  PTY  │         │
                    │  │scroll │ │scroll │         │
                    │  └───┬───┘ └───┬───┘         │
                    │      │state    │state        │
                    │      ▼         ▼             │
                    │  ┌──────────────────┐        │
                    │  │  state tracker   │        │
                    │  └──────────────────┘        │
                    │           │                  │
                    │    Unix socket               │
                    └───────────┼──────────────────┘
                                │
              ┌─────────────────┼─────────────────┐
              │                 │                 │
        ┌─────┴──────┐   ┌──────┴──────┐   ┌──────┴──────┐
        │  zinc TUI  │   │  zinc CLI   │   │  zinc CLI   │
        │ (client 1) │   │ (client 2)  │   │ (client 3)  │
        └────────────┘   └─────────────┘   └─────────────┘
```

### Daemon (`zincd`)

A long-running background process. Started automatically on first `zinc` invocation (like tmux server). Responsibilities:

- **PTY management.** Allocates a pseudo-terminal for each agent. The agent process is forked with its stdin/stdout/stderr connected to this PTY.
- **Scrollback buffer.** Maintains a ring buffer of recent output per agent (configurable size, default ~10,000 lines). When a client attaches, it receives the scrollback so the user sees context.
- **State tracking.** Monitors each agent's PTY output and process status. Updates state using the agent's provider logic.
- **Client communication.** Listens on a Unix domain socket (`$XDG_RUNTIME_DIR/zinc/sock` or `~/.zinc/sock`). Accepts commands from clients and pushes state updates.
- **Lifecycle management.** Starts/stops/restarts agent processes on command. Detects agent crashes and updates state accordingly.

The daemon keeps running as long as at least one agent is alive. It can also be explicitly stopped with `zinc shutdown`.

### Client (`zinc`)

A short-lived process that connects to the daemon. Two modes:

**TUI mode** (`zinc` with no arguments) — interactive full-screen interface showing all agents and their states. Keyboard-driven: navigate, attach, spawn, kill.

**CLI mode** (`zinc <command>`) — one-shot commands for scripting and automation:

```
zinc spawn [options]          Launch a new agent
zinc list [--json]            List all agents and their states
zinc attach <id>              Attach to an agent's PTY
zinc kill <id>                Stop an agent
zinc restart <id>             Restart an agent
zinc shutdown                 Stop all agents and the daemon
zinc status                   Show daemon status
```

### Socket protocol

JSON messages over a Unix domain socket. The protocol is request-response for commands, with server-pushed events for state changes.

**Client → Daemon (commands):**
```json
{"type": "spawn", "agent": "claude", "dir": "/home/user/worktrees/myapp--fix-auth", "id": "fix-auth"}
{"type": "list"}
{"type": "attach", "id": "fix-auth"}
{"type": "kill", "id": "fix-auth"}
{"type": "input", "id": "fix-auth", "data": "base64-encoded-bytes"}
```

**Daemon → Client (responses/events):**
```json
{"type": "agents", "agents": [{"id": "fix-auth", "state": "working", "agent": "claude", "dir": "...", "uptime": 720}]}
{"type": "state_change", "id": "fix-auth", "old": "working", "new": "input"}
{"type": "output", "id": "fix-auth", "data": "base64-encoded-bytes"}
{"type": "error", "message": "agent not found: fix-auth"}
```

For `attach`, the protocol switches from JSON to raw byte streaming (PTY relay), similar to how `docker exec -it` works. The client's terminal is put into raw mode and bytes flow bidirectionally between the client's terminal and the agent's PTY.

## TUI design

### Main view — agent list

```
zinc ─ 3 agents (1 needs input)              esc:detach  n:new  k:kill  q:quit
─────────────────────────────────────────────────────────────────────────────────
  STATE     AGENT    ID            DIRECTORY                          UPTIME
  ● work    claude   fix-auth      ~/worktrees/myapp--fix-auth        12m
▸ ▲ input   claude   fix-nav       ~/worktrees/myapp--fix-nav          8m
  ● work    codex    fix-api       ~/worktrees/myapp--fix-api          3m
  ✓ done    claude   refactor      ~/worktrees/myapp--refactor        42m
```

- Arrow keys / j/k to navigate
- `enter` to attach to the selected agent
- `n` to spawn a new agent
- `k` to kill the selected agent
- `q` to quit zinc (agents keep running)
- Agents needing input sort to the top (or are highlighted)

### Attached view

When attached, zinc's TUI disappears and your terminal is directly connected to the agent's PTY. You interact with the agent as if it were running in your terminal. A small status bar (optional, toggleable) shows:

```
[zinc: fix-auth | claude | working]                          ctrl-z: detach
```

`ctrl-z` (or a configurable key) detaches and returns to the agent list.

### Notifications

When an agent transitions to `input` state (needs user attention), zinc can:
- Highlight the agent in the TUI (always)
- Send a desktop notification (configurable)
- Ring the terminal bell (configurable)

## State detection

### The challenge

Determining what an agent is doing from the outside is inherently imprecise. Agents don't expose a structured status API — they're interactive CLI programs that produce terminal output.

### Approach: layered detection

**Layer 1 — Process status (universal, no false positives)**

| Signal | State |
|---|---|
| Process exited, exit code 0 | `done` |
| Process exited, exit code != 0 | `error` |
| Process running | → check layer 2 |

**Layer 2 — PTY activity (agent-agnostic, heuristic)**

Since zinc owns the PTY, it sees all output in real-time.

| Signal | State |
|---|---|
| Output received in last N seconds | `working` |
| No output for N seconds | → check layer 3 |

**Layer 3 — Provider-specific patterns (per-agent, most accurate)**

Each provider can define patterns that identify states:

```rust
trait Provider {
    /// Command to launch the agent
    fn command(&self, dir: &Path) -> Command;

    /// Analyze recent output to determine state
    fn detect_state(&self, output: &[u8], idle_seconds: u64) -> AgentState;
}
```

For Claude Code, the provider might look for:
- The input prompt character/pattern → `input`
- "Thinking...", tool call output → `working`
- Specific exit messages → `done`

For an unknown/generic agent, fall back to layer 2 heuristics.

### Tuning

The idle timeout (how long before "no output" means "waiting for input" vs "still thinking") is configurable per provider. Claude Code thinks in bursts with pauses; codex might stream continuously. Different agents need different thresholds.

## Provider system

### Provider trait

```rust
pub trait Provider: Send + Sync {
    /// Unique name for this provider (e.g. "claude", "codex")
    fn name(&self) -> &str;

    /// Build the command to launch the agent in a directory
    fn build_command(&self, dir: &Path, args: &[String]) -> Command;

    /// Analyze agent state from recent PTY output and idle time
    fn detect_state(&self, recent_output: &[u8], idle_duration: Duration) -> AgentState;
}
```

### Built-in providers

**v0: Claude Code**
- Command: `claude`
- State detection: prompt pattern matching + output activity

**Later:**
- Codex (`codex`)
- OpenCode (`opencode`)
- Gemini CLI (`gemini`)
- Generic (PTY activity heuristics only — works with any interactive CLI)

### Custom providers via config

Users can define simple providers in config without writing Rust:

```toml
[agents.my-agent]
command = "my-agent-cli"
prompt_pattern = "^> $"           # regex matching the input prompt
working_patterns = ["thinking", "running"]
idle_timeout = 5                  # seconds before "no output" = "input"
```

This covers most agents without needing a compiled provider.

## Configuration

`~/.config/zinc/config.toml`

```toml
# Daemon settings
[daemon]
socket = "~/.zinc/sock"            # Unix socket path
scrollback = 10000                 # Lines of scrollback per agent

# Notification settings
[notify]
desktop = true                     # Desktop notifications on state change
bell = false                       # Terminal bell
on_states = ["input"]              # Which state transitions trigger notifications

# Detach keybinding
[keys]
detach = "ctrl-z"

# Built-in provider overrides
[agents.claude]
command = "claude"
idle_timeout = 5

[agents.codex]
command = "codex"
idle_timeout = 3

# Custom provider
[agents.aider]
command = "aider"
prompt_pattern = "^aider> "
idle_timeout = 5
```

## Composing with yawn

zinc and yawn are independent tools. Neither depends on the other. They compose through directories and standard shell mechanisms.

### yawn's job
Answer: **"where does the work happen?"**
- Create and manage worktrees
- Discover and navigate projects
- Initialize workspaces (copy config files, install dependencies)

### zinc's job
Answer: **"who's doing the work, and do they need me?"**
- Launch and supervise agent processes
- Provide a unified view of all running agents
- Let you attach/detach from agent sessions instantly

### Combined workflows

**Spin up a workspace and an agent:**
```bash
yawn create fix-auth --init
zinc spawn --agent claude --dir "$(yawn resolve fix-auth)"
```

**Batch create:**
```bash
for branch in fix-auth fix-nav fix-api; do
  yawn create "$branch" --init
  zinc spawn --agent claude --dir "$(yawn resolve "$branch")"
done
```

**Tear down:**
```bash
zinc kill fix-auth
yawn delete fix-auth
```

**Shell function for the common case:**
```bash
# Add to ~/.bashrc or ~/.zshrc
yagent() {
  local branch="$1"
  shift
  yawn create "$branch" --init
  zinc spawn --agent "${1:-claude}" --dir "$(yawn resolve "$branch")" --id "$branch"
}

yagent fix-auth            # create worktree + launch claude
yagent fix-nav codex       # create worktree + launch codex
```

### Why not couple them?

It would be tempting to add `zinc spawn --yawn-create fix-auth` or have zinc call yawn internally. This should be resisted because:

1. **Independent testing.** Each tool can be developed, tested, and released on its own schedule.
2. **Independent use.** zinc is useful without worktrees (run agents in any directory). yawn is useful without agents (project switching).
3. **User control.** A shell function is transparent and customizable. Built-in coupling hides behavior and adds configuration surface.
4. **Simplicity.** Each tool does one thing well.

## Implementation plan

### Phase 0 — Foundation

Scaffolding and core infrastructure. No TUI yet.

**Deliverables:**
- Project setup (Cargo workspace, CI, linting)
- Daemon process with Unix socket listener
- Client library for daemon communication
- PTY allocation and management (fork agent process with PTY)
- Scrollback ring buffer
- Basic agent lifecycle: spawn, kill, list
- CLI commands: `zinc spawn`, `zinc list`, `zinc kill`, `zinc shutdown`

**Key crates:**
- `portable-pty` or `nix` — PTY management
- `tokio` — async runtime for daemon
- `serde` / `serde_json` — socket protocol
- `clap` — CLI parsing

**Exit criteria:** Can spawn a Claude Code process, list it with `zinc list`, and kill it with `zinc kill`. Agent survives terminal close.

### Phase 1 — Attach/detach

The core multiplexer capability.

**Deliverables:**
- `zinc attach <id>` — connect terminal to agent PTY
- Raw mode terminal handling (pass through all bytes)
- Detach keybinding (ctrl-z)
- Scrollback replay on attach (see recent context)
- Multiple simultaneous viewers

**Exit criteria:** Can attach to a running Claude Code session, interact with it, detach, and reattach. Output produced while detached is visible on reattach.

### Phase 2 — State detection

Make zinc aware of what agents are doing.

**Deliverables:**
- Provider trait definition
- Claude Code provider (prompt detection, working patterns)
- Generic fallback provider (PTY activity heuristic)
- State tracking in daemon, exposed via `zinc list`
- `zinc list --json` for scripting

**Exit criteria:** `zinc list` shows accurate state for Claude Code agents. Transitions between `working` and `input` are detected within a few seconds.

### Phase 3 — TUI

The interactive supervisor interface.

**Deliverables:**
- Full-screen TUI with agent list (using `ratatui`)
- Keyboard navigation (j/k, enter to attach, n to spawn, k to kill)
- State-based highlighting and sorting (needs-input agents surface to top)
- Status bar in attached mode
- Live updates via daemon push events

**Exit criteria:** Running `zinc` opens a TUI showing all agents. Can navigate, attach, detach, spawn, and kill entirely from the TUI.

### Phase 4 — Notifications and polish

**Deliverables:**
- Desktop notifications on state transitions (via `notify-rust` or similar)
- Terminal bell support
- Configurable notification rules (which states, which agents)
- Agent restart (`zinc restart`)
- Daemon auto-start on first `zinc` command
- Daemon auto-shutdown when last agent exits (configurable)
- Man page, shell completions

### Phase 5 — Multi-agent support

**Deliverables:**
- Codex provider
- OpenCode provider
- Gemini CLI provider
- Custom provider via config (prompt pattern, idle timeout)
- Provider auto-detection (detect which agent is configured for a directory)

### Future considerations (not planned)

- **Agent output logging** — write agent output to log files for post-hoc review
- **Agent metrics** — track tokens used, time spent, tools called (if parseable from output)
- **Remote agents** — manage agents on remote machines via SSH
- **Web UI** — browser-based supervisor (alternative to TUI)
- **Orchestration layer** — assign tasks to agents from GitHub issues, project boards, etc. (likely a separate tool that uses both zinc and yawn)

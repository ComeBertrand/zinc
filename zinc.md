# zinc — an agent multiplexer for the terminal

> zinc is not claude

## Vision

AI coding agents (Claude Code, Codex, OpenCode, Gemini CLI) are becoming a core part of the development workflow. Developers increasingly run multiple agents in parallel — each working on a separate task in its own worktree. But managing these agents is manual and fragile: agents die when terminals close, there's no unified view of what's running, and switching between agent sessions requires hunting through tabs or tmux panes.

zinc is a terminal tool that solves this. It manages agent processes as persistent background daemons, provides a TUI to monitor their status, and lets you attach/detach from any agent session instantly. Think of it as tmux, but purpose-built for AI coding agents.

## Goals

1. **Never lose an agent session.** Agents run as background daemons. Closing your terminal, quitting zinc, or logging out does not kill them. You can reattach at any time.

2. **Know when you're needed.** The TUI shows the state of every agent at a glance: working, waiting for input, blocked, idle. Notifications alert you when an agent needs attention.

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
- **ID** — a short, unique identifier derived from the directory name, a configurable namer script, or explicitly provided
- **Provider** — which tool is running (must be a known provider, e.g. `claude`)
- **Directory** — the working directory (typically a repo or worktree)
- **State** — what the agent is doing right now
- **PTY** — the pseudo-terminal zinc allocated for it
- **Scrollback** — a ring buffer of recent output (default 1MB)

### State

What an agent is doing at any given moment. Agent exit is an event, not a state — exited agents are cleaned up immediately and removed from the list.

| State | Meaning |
|---|---|
| `working` | Agent is actively producing output (generating code, running tools) |
| `blocked` | Agent needs user action to continue (e.g. permission prompt) |
| `input` | Agent finished its current task, waiting for new user prompt |
| `idle` | Agent process is running but inactive (heuristic fallback for generic agents) |

State detection uses a layered approach. See the [State detection](#state-detection) section.

### Provider

An adapter that knows how to work with a specific agent tool. A provider defines:
- How to launch the agent (command, arguments, environment)
- How to map `--resume` and `--prompt` flags to provider-specific CLI arguments
- How to detect the agent's state (hooks for Claude, PTY heuristics for generic agents)

The CLI validates that the provider is in the known list before spawning. Only known providers are accepted — zinc is purpose-built for AI coding agents, not a general process manager. Providers live in `zinc-daemon` only — the CLI/TUI doesn't need them.

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

- **PTY management.** Allocates a pseudo-terminal for each agent via `nix::pty::openpty()`. The agent process is forked with its stdin/stdout/stderr connected to this PTY. A dedicated reader thread per agent reads output in a blocking loop.
- **Scrollback buffer.** Maintains a byte-based ring buffer of recent output per agent (default 1MB). When a client attaches, it receives the scrollback so the user sees context.
- **State tracking.** A background monitor task runs every 1 second, checking each agent for process exit and state changes. State is determined by the provider (hooks for Claude, PTY idle heuristic for generic agents).
- **Event broadcasting.** A `tokio::broadcast` channel pushes events (`AgentSpawned`, `StateChange`, `AgentExited`) to all connected clients in real-time.
- **Client communication.** Listens on a Unix domain socket (`$XDG_RUNTIME_DIR/zinc/sock` or `~/.zinc/sock`). Accepts newline-delimited JSON commands from clients.
- **Lifecycle management.** Starts/stops agent processes on command. Kill uses SIGTERM then SIGKILL after 200ms.

The daemon keeps running as long as at least one agent is alive. It can also be explicitly stopped with `zinc shutdown`.

### Client (`zinc`)

A single binary with two modes:

**TUI mode** (`zinc` with no arguments) — interactive full-screen interface (ratatui + crossterm) showing all agents and their states. Keyboard-driven: navigate, attach, spawn, kill.

**CLI mode** (`zinc <command>`) — one-shot commands for scripting and automation:

```
zinc spawn [options]          Launch a new agent
zinc list [--json]            List all agents and their states
zinc attach [id]              Attach to an agent's PTY (resolves from CWD if omitted)
zinc kill <id>                Stop an agent
zinc shutdown                 Stop all agents and the daemon
zinc status                   Check if the daemon is running
zinc hook-notify              Notify daemon of a hook event (called by agent hooks)
```

#### `zinc spawn`

Launches a new agent. Supports both interactive and non-interactive modes.

**Flags:**
- `--agent <name>` — provider to use (default: from config, or `claude`)
- `--dir <path>` — working directory (default: CWD)
- `--id <name>` — agent ID (default: namer script → directory basename)
- `--resume` — resume previous conversation (maps to `--resume` for Claude)
- `--prompt <text>` — initial prompt text (maps to positional arg for Claude)
- `--yes` / `-y` — skip interactive prompts, use defaults

**Interactive mode** (when `spawn.interactive = true`, stdin is a TTY, and `--yes` is not passed):

```
$ zinc spawn
Agent [claude]:
Resume previous session? [y/N]: y
Starting prompt (optional, enter to skip): continue with the tests now
Spawned agent: fix-auth (resumed)
```

Each flag provided skips its corresponding question. `--yes` skips all prompts.

**ID resolution chain:** `--id` flag → `spawn.namer` config script → directory basename.

#### `zinc attach`

Attaches to a running agent. If no ID is given, resolves from the current directory:
- If exactly one agent is running in CWD → attach to it
- If none → error: "no agent running in /path/to/dir"
- If multiple → error: "multiple agents in /path/to/dir: agent-a, agent-b"

### Socket protocol

Newline-delimited JSON messages over a Unix domain socket. The protocol is request-response for commands, with server-pushed events for state changes. Messages are deserialized via an untagged `ServerMessage` enum that tries `Response` first, then `Event` — the `type` field values don't overlap.

**Client → Daemon (requests):**
```json
{"type": "spawn", "provider": "claude", "dir": "/home/user/project", "id": "fix-auth", "args": [], "resume": true, "prompt": "fix the auth bug"}
{"type": "list"}
{"type": "attach", "id": "fix-auth", "cols": 120, "rows": 40}
{"type": "kill", "id": "fix-auth"}
{"type": "hook_event", "agent_id": "fix-auth", "event": "stop"}
{"type": "shutdown"}
```

**Daemon → Client (responses):**
```json
{"type": "spawned", "id": "fix-auth"}
{"type": "agents", "agents": [{"id": "fix-auth", "state": "working", "provider": "claude", "dir": "...", "uptime_secs": 720, "viewers": 1}]}
{"type": "attached"}
{"type": "ok"}
{"type": "error", "message": "agent not found: fix-auth"}
```

**Daemon → Client (pushed events):**
```json
{"type": "agent_spawned", "id": "fix-auth", "info": {"id": "fix-auth", "provider": "claude", ...}}
{"type": "state_change", "id": "fix-auth", "old": "working", "new": "input"}
{"type": "agent_exited", "id": "fix-auth", "exit_code": 0}
```

For `attach`, the protocol switches from JSON to raw byte streaming (PTY relay), similar to how `docker exec -it` works. The client's terminal is put into raw mode and bytes flow bidirectionally between the client's terminal and the agent's PTY.

The `resume` and `prompt` fields in the Spawn request are backward-compatible: `resume` defaults to `false` via `#[serde(default)]`, and `prompt` is omitted when `None`. Old clients that don't send these fields work unchanged.

## TUI design

### Main view — agent list

```
 zinc — 3 agents (1 needs input)
  STATE     AGENT      ID              DIRECTORY                          UPTIME   VIEWERS
  ● work    claude     fix-auth        ~/worktrees/myapp--fix-auth          12m         0
▸ ▲ input   claude     fix-nav         ~/worktrees/myapp--fix-nav            8m         0
  ● work    codex      fix-api         ~/worktrees/myapp--fix-api            3m         1
 enter:attach  n:new  d:kill  q:quit
```

- `j`/`k` or arrow keys to navigate
- `enter` to attach to the selected agent
- `n` to spawn a new agent (Claude in CWD by default)
- `d` to kill the selected agent (`k` is reserved for vim-up navigation)
- `q` or `ctrl-c` to quit zinc (agents keep running)
- Agents needing input/blocked sort to the top
- State indicators are color-coded: working=blue, input=yellow, blocked=red, idle=gray
- Transient status messages (spawn/kill confirmation, errors) display in the footer with auto-expiry
- Empty state shows "No agents running. Press n to spawn one."

### Attached view

When attached, the TUI's alternate screen is replaced by the agent's terminal output. A status bar is drawn on the last row via scroll region, so agent output doesn't overwrite it:

```
[agent output fills the screen above]
 zinc: fix-auth | claude                                     ctrl-]: detach
```

The agent's PTY is resized to `height - 1` rows so its output fits within the scroll region.

`ctrl-]` detaches and returns to the agent list. On detach, terminal state is comprehensively reset (keyboard protocol, mouse tracking, alternate screen, scroll region, colors) to handle whatever escape sequences the agent may have sent.

### Implementation details

The TUI uses a **two-connection model**: one persistent connection for commands and event streaming, and a second connection opened for each attach session. This means the TUI never loses events while attached.

The event loop uses a **dedicated crossterm reader thread** that sends terminal events over a `tokio::sync::mpsc` channel. The main loop uses `tokio::select!` on both the crossterm channel and `client.read_message()` from the daemon, so keyboard input and daemon events are handled with equal responsiveness. The crossterm reader is paused (via `AtomicBool` flag) during attach to avoid conflicting with the raw byte relay's stdin reader.

Agent output during attach is filtered by `KbdProtoFilter`, a state machine that strips Kitty keyboard protocol and xterm modifyOtherKeys sequences. This prevents the agent's TUI from changing the outer terminal's key encoding, ensuring `ctrl-]` always arrives as raw byte `0x1d`.

## State detection

### The challenge

Determining what an agent is doing from the outside is inherently imprecise. Agents don't expose a structured status API — they're interactive CLI programs that produce terminal output.

### Approach: layered detection

**Layer 1 — Process status (universal, no false positives)**

| Signal | State |
|---|---|
| Process exited | → `AgentExited` event emitted, agent removed from list |
| Process running | → check layer 2 |

Agent exit is an event, not a state. The daemon's state monitor checks `waitpid` every second and broadcasts `AgentExited` with the exit code. The agent is immediately removed from the agent map.

**Layer 2 — PTY activity (agent-agnostic, heuristic)**

Since zinc owns the PTY, it sees all output in real-time. A dedicated reader thread per agent updates a `last_output_at` timestamp on every read.

| Signal | State |
|---|---|
| Output received in last N seconds | `working` |
| No output for N seconds | `idle` (generic) or → check layer 3 |

This is the primary detection method for `GenericProvider` (any agent without specific integration). Default idle timeout is 5 seconds.

**Layer 3 — Hook-based detection (per-agent, most accurate)**

For agents that support hooks (currently Claude Code), state is pushed directly rather than inferred from output. The daemon injects `ZINC_AGENT_ID` and `ZINC_SOCKET` environment variables when spawning the agent. The agent's hooks call `zinc hook-notify --event <event_name>`, which sends a `HookEvent` request to the daemon. The provider's `map_hook_event` method translates event names to states.

Claude Code hook events:
| Hook event | State |
|---|---|
| `stop`, `notification:idle_prompt` | `input` |
| `notification:permission_prompt` | `blocked` |
| `pre_tool_use`, `subagent_start` | `working` |

Hook-based detection is immediate (no polling delay) and distinguishes `input` from `blocked` — something PTY heuristics cannot do.

## Provider system

### Constrained providers

zinc only accepts known providers. The CLI validates the provider name before sending a Spawn request. Unknown agents are rejected with a clear error:

```
$ zinc spawn --agent bash
Error: unknown agent 'bash'. Known agents: claude
```

The daemon still uses `GenericProvider` as an internal fallback (for integration tests that use `sleep`/`true`/`false`), but the CLI gates user-facing input to the known list.

### Provider trait

Lives in `zinc-daemon` only. The CLI/TUI doesn't need it.

```rust
pub trait Provider: Send + Sync {
    /// Unique name for this provider (e.g. "claude", "codex")
    fn name(&self) -> &str;

    /// Build the command to launch the agent in a directory.
    /// `resume` and `prompt` are mapped to provider-specific CLI arguments.
    fn build_command(
        &self,
        dir: &Path,
        args: &[String],
        resume: bool,
        prompt: Option<&str>,
    ) -> Command;

    /// Analyze agent state from PTY output and idle time.
    /// Returns None if this provider uses hooks instead.
    fn detect_state_from_output(
        &self,
        recent_output: &[u8],
        idle_duration: Duration,
    ) -> Option<AgentState>;

    /// Map a hook event name to an agent state.
    /// Returns None if this provider doesn't handle hooks.
    fn map_hook_event(&self, event: &str) -> Option<AgentState>;
}
```

### Built-in providers

**ClaudeProvider** (`claude`)
- Command: `claude`
- `resume: true` → adds `--resume` flag
- `prompt: Some(text)` → adds text as positional argument (after any extra args)
- State detection: hooks only (`detect_state_from_output` returns `None`)
- Hook mapping: `stop`→Input, `notification:permission_prompt`→Blocked, `pre_tool_use`→Working, etc.

**GenericProvider** (internal only, not user-facing)
- Command: uses the provider name as the binary (e.g. `sleep`, `true`)
- Ignores `resume` and `prompt` (not meaningful for generic commands)
- State detection: PTY idle heuristic (idle after 5s of no output)
- No hook support
- Used by integration tests only

### Custom providers via config

*Not yet implemented.* Users will be able to define simple providers in config without writing Rust:

```toml
[agents.my-agent]
command = "my-agent-cli"
prompt_pattern = "^> $"           # regex matching the input prompt
working_patterns = ["thinking", "running"]
idle_timeout = 5                  # seconds before "no output" = "input"
```

## Configuration

`~/.config/zinc/config.toml` — loaded by the client on startup. The daemon does not read config; all spawn options are passed through the protocol.

```toml
[spawn]
agent = "claude"                       # default provider (used when --agent is omitted)
namer = "yawn prettify {dir}"          # command to derive agent ID from directory
                                       # {dir} is replaced with the shell-quoted path
                                       # fallback: directory basename
interactive = true                     # prompt for missing values on spawn (default: true)
                                       # set to false for non-interactive usage (same as --yes)

[daemon]
scrollback = 1048576                   # scrollback buffer size in bytes (default: 1MB)
```

All fields are optional. Missing fields use defaults. Missing file uses all defaults.

### Namer

The `spawn.namer` config field defines a command template to derive the agent ID from the working directory. The `{dir}` placeholder is replaced with the shell-quoted directory path, and the command is run via `sh -c`. The first line of stdout is used as the agent ID.

**ID resolution chain:** `--id` flag → namer command → directory basename.

Examples:
```toml
namer = "basename {dir}"               # just use the directory name
namer = "yawn prettify {dir}"          # use yawn to derive a clean name
```

## Composing with yawn

zinc and yawn are independent tools. Neither depends on the other. They compose through directories, config templates, and standard shell mechanisms.

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

### CWD-based workflow

zinc operations default to the current directory. In a worktree-based workflow, this means you can `cd` into a worktree and interact with its agent without specifying IDs:

```bash
cd ~/worktrees/myapp--fix-auth
zinc spawn                             # spawns claude in this dir, ID = "myapp--fix-auth"
zinc attach                            # attaches to the agent in this dir
zinc spawn --resume --prompt "now fix the tests"  # resume with a new prompt
```

With a namer configured (`namer = "yawn prettify {dir}"`), the ID becomes cleaner (e.g. `fix-auth` instead of `myapp--fix-auth`).

### Shell functions

```bash
# Spawn agent in a worktree
zagent() {
  local dir="${1:-.}"
  dir="$(cd "$dir" && pwd)"
  zinc spawn --dir "$dir" -y "${@:2}"
}

# Create worktree + spawn agent
yagent() {
  yawn create "$1" --init
  zagent "$(yawn resolve "$1")" "${@:2}"
}

yagent fix-auth                        # create worktree + launch claude
yagent fix-auth --resume               # create worktree + resume session
yagent fix-auth --prompt "fix auth"    # create worktree + start with prompt
```

### Why not couple them?

It would be tempting to add `zinc spawn --yawn-create fix-auth` or have zinc call yawn internally. This should be resisted because:

1. **Independent testing.** Each tool can be developed, tested, and released on its own schedule.
2. **Independent use.** zinc is useful without worktrees (run agents in any directory). yawn is useful without agents (project switching).
3. **User control.** A shell function is transparent and customizable. Built-in coupling hides behavior and adds configuration surface.
4. **Simplicity.** Each tool does one thing well.

## Implementation plan

### Phase 0 — Foundation ✅

Scaffolding and core infrastructure. No TUI yet.

**Delivered:**
- Cargo workspace with three crates: `zinc-proto`, `zinc-daemon`, `zinc-cli`
- Daemon process with Unix socket listener, PID file, auto-cleanup
- Client library with auto-start (spawns `zincd` if not running)
- PTY allocation via `nix::pty::openpty()`, fork with `setsid`/`TIOCSCTTY`
- Byte-based scrollback ring buffer (default 1MB)
- Agent lifecycle: spawn, kill, list, shutdown
- CLI commands: `zinc spawn`, `zinc list`, `zinc kill`, `zinc shutdown`, `zinc status`
- Newline-delimited JSON protocol with `serde` tagged enums

**Key crates:** `nix` (PTY, signals), `tokio` (async runtime), `serde`/`serde_json` (protocol), `clap` (CLI)

### Phase 1 — Attach/detach ✅

The core multiplexer capability.

**Delivered:**
- `zinc attach <id>` — raw mode terminal, bidirectional byte relay
- Detach via `ctrl-]` (intercepted on client side, not sent to agent)
- Scrollback replay on attach
- Multiple simultaneous viewers (viewer count tracked per agent)
- `KbdProtoFilter` — strips Kitty keyboard protocol and xterm modifyOtherKeys sequences from agent output to protect the outer terminal
- Comprehensive terminal reset on detach (keyboard modes, mouse tracking, alternate screen, scroll region, colors, cursor)
- Dedicated stdin reader task to prevent keystroke loss during heavy agent output

### Phase 2 — State detection ✅

Make zinc aware of what agents are doing.

**Delivered:**
- `Provider` trait in `zinc-daemon` with four methods: `name`, `build_command`, `detect_state_from_output`, `map_hook_event`
- `ClaudeProvider` — hook-based state detection (output detection returns None)
- `GenericProvider` — PTY idle heuristic (working if output in last 5s, otherwise idle)
- `AgentState` enum: `Working`, `Blocked`, `Input`, `Idle` (no Done/Error — exit is an event)
- Background state monitor in daemon (1s interval), broadcasts `StateChange` events
- `AgentExited` event on process exit, agent removed from map
- `zinc hook-notify` CLI command for agent hooks to push state
- `ZINC_AGENT_ID` and `ZINC_SOCKET` env vars injected at spawn time
- `zinc list --json` for scripting

**Design decisions:**
- `Done`/`Error` states removed — agent exit is a transient event, not a persistent state
- `Blocked` state added — distinguishes permission prompts from idle input prompts
- Hooks preferred over output parsing for Claude (immediate, more accurate)

### Phase 3 — TUI ✅

The interactive supervisor interface.

**Delivered:**
- Full-screen TUI with ratatui + crossterm
- Agent table with state icons/colors, sorted by priority (blocked/input first)
- Keyboard navigation: `j`/`k`/arrows, `enter` attach, `n` spawn, `d` kill, `q` quit
- Live updates via daemon push events (`AgentSpawned`, `StateChange`, `AgentExited`)
- Attach/detach from TUI: leaves alternate screen, opens second connection for relay, restores on detach
- Status bar in attached mode via scroll region (agent gets rows-1)
- Transient status messages for spawn/kill feedback
- Empty state message when no agents running
- Terminal resize handling

**Design decisions:**
- Two-connection model: persistent connection for events/commands, second for attach relay
- `d` for kill (not `k`, since `k` is vim-up navigation)
- Dedicated crossterm reader thread with `AtomicBool` pause flag (paused during attach)
- `tokio::select!` on crossterm channel + daemon socket for responsive event handling
- `AgentSpawned` event added to protocol so TUI updates live when agents are spawned externally

### Phase 3.5 — Workflow and configuration ✅

Opinionated workflow support while staying composable.

**Delivered:**
- Configuration file: `~/.config/zinc/config.toml` with `[spawn]` and `[daemon]` sections
- Constrained providers: only known agents accepted (`claude`), CLI validates before spawn
- CWD-based operations: `zinc attach` resolves from CWD, `zinc spawn` defaults ID from directory
- Namer: configurable `{dir}` template command to derive agent ID (shell-quoted, `sh -c`)
- First-class `--resume` and `--prompt` flags on `zinc spawn`, mapped to provider-specific args
- Interactive spawn: prompts for missing values when TTY is available, `--yes`/`-y` to skip
- Config-driven defaults: `spawn.agent`, `spawn.namer`, `spawn.interactive`

**Design decisions:**
- Config is client-side only — daemon receives everything through the protocol
- Namer uses yawn's `{dir}` template + `shell_quote` pattern for safe command interpolation
- `GenericProvider` kept for integration tests (bypasses CLI validation via raw protocol)
- Interactive mode respects both config (`interactive = false`) and CLI flag (`--yes`)
- `resume` and `prompt` are protocol fields, not provider-specific — provider maps them to CLI args

### Phase 4 — Notifications and polish

**Planned:**
- Desktop notifications on state transitions (via `notify-rust` or similar)
- Terminal bell support
- Configurable notification rules (which states, which agents)
- Agent restart (`zinc restart`)
- Daemon auto-shutdown when last agent exits (configurable)
- Man page, shell completions

**Already done:** Daemon auto-start on first `zinc` command.

### Phase 5 — Multi-agent support

**Planned:**
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

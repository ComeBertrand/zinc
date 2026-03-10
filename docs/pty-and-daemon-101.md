# PTY and Daemon Architecture 101

How zinc manages agent processes, what a PTY is, and how bytes flow
between the user's terminal and the agent.

## What is a PTY?

A PTY (pseudo-terminal) is a pair of virtual devices provided by the OS
kernel. It looks like a hardware terminal to the program running inside
it, but is actually controlled by software.

```
┌─────────────────────────┐
│   PTY pair              │
│                         │
│  Master ◄────► Slave    │
│  (daemon)     (agent)   │
└─────────────────────────┘
```

- **Slave side:** The agent (nvim, Claude Code, bash) is connected here.
  Its stdin, stdout, and stderr all point to the slave. From the agent's
  perspective, it's talking to a real terminal — it can query dimensions,
  receive SIGWINCH on resize, and send escape sequences.

- **Master side:** The daemon reads from and writes to this end. Anything
  the agent writes to stdout goes through the slave and comes out the
  master. Anything the daemon writes to the master goes in through the
  agent's stdin.

This is the same mechanism tmux, screen, and SSH use. The PTY provides
full terminal emulation — line discipline, echo, signal delivery — without
needing a real terminal.

## Why not just pipes?

You might wonder: why not connect the agent's stdin/stdout to regular
pipes? Because many programs behave differently when they detect they're
not on a terminal:

- They disable color output
- They buffer stdout in large chunks instead of line-by-line
- They refuse to run at all ("not a terminal")
- They can't handle resize signals (SIGWINCH)
- Escape sequences for cursor movement, alternate screen, etc. don't work

A PTY solves all of this. The agent sees `isatty() == true` and behaves
normally.

## The daemon

The daemon (`zincd`) is a long-running background process that owns all
the agents. It survives after the CLI client exits, which is the whole
point — agents keep running when you're not looking.

### Lifecycle

```
User runs `zinc spawn` → zinc CLI starts zincd (if not running) → zincd
listens on a Unix socket → accepts client connections → manages agents
```

**Startup:**

1. The CLI finds `zincd` next to its own binary
2. Spawns it with `setsid()` (creates a new session, detaches from the
   user's terminal) and null stdio
3. The daemon creates its socket at `$XDG_RUNTIME_DIR/zinc/sock`
4. Writes a PID file for identification
5. Enters the main accept loop

**Shutdown:**

Triggered by `zinc shutdown` or when all agents are done. The daemon
kills remaining agents, removes the socket and PID file, and exits.

### Connection handling

Each client connection starts with a JSON request/response protocol over
the Unix socket:

```
Client                         Daemon
  │                              │
  │──── {"type":"spawn",...} ───►│
  │◄─── {"type":"spawned",...} ──│
  │                              │
  │──── {"type":"list"} ────────►│
  │◄─── {"type":"agents",...} ───│
```

For `attach`, the protocol switches from JSON to raw bytes after the
handshake (more on this below).

## Spawning an agent

When the daemon receives a `Spawn` request:

```
1. openpty()
   ├─ Returns: master fd + slave fd
   └─ Kernel creates the PTY pair

2. Command::new("claude")
   ├─ .stdin(slave)     ← agent reads from slave
   ├─ .stdout(slave)    ← agent writes to slave
   ├─ .stderr(slave)    ← errors also go to slave
   ├─ pre_exec:
   │   ├─ setsid()      ← new session (detach from daemon)
   │   └─ TIOCSCTTY     ← make slave the controlling terminal
   └─ .spawn()          ← fork + exec

3. Start PTY reader thread (see below)

4. Store Agent in HashMap
```

The `setsid()` + `TIOCSCTTY` in the child process is important: it makes
the PTY slave the agent's *controlling terminal*. This means:
- `ctrl+c` from the PTY generates SIGINT to the agent
- SIGWINCH (resize) is delivered to the agent
- The agent can query terminal size via `ioctl(TIOCGWINSZ)`

## The PTY reader thread

The daemon spawns a dedicated OS thread (not a tokio task) for each
agent. This thread does blocking reads on the PTY master:

```
PTY reader thread (one per agent):
  loop {
      bytes = read(master_fd)    ← blocks until agent produces output
      scrollback.write(bytes)    ← append to ring buffer (1MB default)
      broadcast.send(bytes)      ← notify all attached clients
  }
```

**Why a real thread and not async?** PTY file descriptors are not regular
files or sockets — they don't always work well with async I/O runtimes
like tokio. A blocking thread is the reliable approach.

**Two consumers of the output:**

1. **Scrollback buffer** — a ring buffer (1MB) that stores recent output.
   When a client attaches, it receives the scrollback first, so it sees
   context from before it connected.

2. **Broadcast channel** — a tokio broadcast channel that fans out live
   output to all currently attached clients. Multiple clients can watch
   the same agent simultaneously.

## Attaching to an agent

This is the most interesting part. The attach flow crosses between JSON
and raw byte protocols, and involves both the daemon and client doing
terminal setup.

### Step by step

```
Client                          Daemon
  │                               │
  │── {"type":"attach",           │
  │    "id":"abc",                │
  │    "cols":120,"rows":40} ────►│
  │                               ├─ Resize agent's PTY
  │                               ├─ Subscribe to broadcast
  │                               ├─ Snapshot scrollback
  │◄── {"type":"attached"} ───────│
  │                               │
  │   ═══ Protocol switches to raw bytes ═══
  │                               │
  │◄──── scrollback bytes ────────│  (catch-up)
  │                               │
  │◄──── live output bytes ───────│  ← broadcast
  │───── keyboard bytes ─────────►│  → PTY master
  │◄──── response bytes ──────────│  ← PTY master
  │      ...                      │
  │   (ctrl+] detected)           │
  │                               │
  │   ═══ Connection closes ═══   │
```

### What the client does

1. **Enter raw mode** — disables the terminal's line editing, echo, and
   signal handling. Keystrokes arrive as raw bytes immediately instead
   of waiting for Enter.

2. **Start a dedicated stdin reader task** — reads keyboard input on a
   separate tokio task so it's never cancelled by the select loop. This
   prevents losing keystrokes when the agent is producing heavy output.

3. **Select loop:**
   ```
   loop {
       select! {
           stdin data → check for ctrl+] → forward to daemon
           socket data → filter keyboard protocols → write to stdout
       }
   }
   ```

4. **On detach** — restore original terminal settings and send reset
   sequences (leave alternate screen, reset colors, etc.).

### What the daemon does

1. **Send scrollback** — the full ring buffer contents, so the client
   sees recent context.

2. **Two parallel tasks:**
   - Broadcast receiver → socket writer (agent output to client)
   - Socket reader → PTY master writer (client input to agent)

3. **Exit when either task ends** — client disconnect or agent exit.

4. **Viewer counting** — an atomic counter tracks how many clients are
   attached. Incremented on attach, decremented on detach.

## Data flow diagram

Putting it all together, here is the complete path a keystroke takes
from the user to the agent and back:

```
User presses a key
       │
       ▼
User's terminal encodes keystroke as bytes
       │
       ▼
Client reads from stdin (raw mode)
       │
       ├─ Is it ctrl+] (0x1d)?  → detach
       │
       ▼
Client writes to Unix socket
       │
       ▼
Daemon reads from socket
       │
       ▼
Daemon writes to PTY master
       │
       ▼
Kernel delivers bytes to PTY slave (agent's stdin)
       │
       ▼
Agent processes input, writes response to stdout (PTY slave)
       │
       ▼
Kernel delivers bytes to PTY master
       │
       ▼
PTY reader thread reads master, broadcasts
       │
       ▼
Daemon attach task receives broadcast, writes to socket
       │
       ▼
Client reads from socket
       │
       ▼
Client filters keyboard protocol sequences
       │
       ▼
Client writes to stdout
       │
       ▼
User's terminal renders output
```

## Multiple viewers

Because the PTY reader broadcasts output to a channel, multiple clients
can attach to the same agent simultaneously:

```
                    ┌─── Client A (Alice's terminal)
                    │
Agent ── PTY ── Reader ─── Broadcast ─┤
                    │
                    └─── Client B (Bob's terminal)
```

All viewers see the same output. Any viewer can type — their input all
goes to the same PTY master. This is like `tmux attach` from multiple
terminals.

## Scrollback

The scrollback buffer is a ring buffer backed by a `VecDeque<u8>` with a
default capacity of 1MB. When it fills up, old bytes are discarded from
the front:

```
Capacity: 1MB
                  ┌─────────────────────────┐
Oldest bytes ──►  │ ... output data ...     │  ◄── Newest bytes
                  └─────────────────────────┘
                  ▲                           ▲
               drain here              append here
               when full
```

When a client attaches, it receives a snapshot of the current scrollback
before subscribing to live output. This means the client sees context
even if the agent produced output hours ago with nobody watching.

## Process hierarchy

```
User's shell
  └─ zinc (CLI client)                    ← exits after command
       └─ zincd (daemon)                  ← persists (setsid)
            ├─ PTY reader thread (agent1) ← OS thread
            ├─ PTY reader thread (agent2)
            │
            ├─ agent1 (nvim)              ← own session (setsid)
            │   └─ child processes...
            └─ agent2 (claude)
                └─ child processes...
```

Each agent has its own session (`setsid`), so killing the daemon doesn't
leave orphan processes — the daemon explicitly sends SIGTERM (then
SIGKILL if needed) to each agent on shutdown.

## Comparison to tmux

zinc's architecture is deliberately similar to tmux:

| Concept | tmux | zinc |
|---------|------|------|
| Server process | `tmux server` | `zincd` |
| Communication | Unix socket | Unix socket |
| Terminal virtualization | Built-in terminal emulator | Raw PTY relay |
| Session management | Sessions, windows, panes | Agents (flat list) |
| Attach protocol | Custom binary protocol | JSON handshake → raw bytes |
| Scrollback | Per-pane buffer | Per-agent ring buffer |

The key difference is that tmux includes a full VT100 terminal emulator
that interprets all escape sequences and re-renders. zinc passes bytes
through directly (with minimal filtering), which is simpler but means
the agent's escape sequences reach the outer terminal — hence the need
for keyboard protocol filtering.

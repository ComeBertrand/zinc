# Keyboard Encoding Protocols 101

How terminals turn keystrokes into bytes, why modern TUI apps break that,
and what zinc does about it.

## The basics: how a keystroke reaches your program

When you press a key in a terminal, three things happen in sequence:

```
Physical key → Terminal emulator → Byte sequence → Program reads stdin
```

The terminal emulator (Kitty, WezTerm, Ghostty, iTerm2, Zellij, etc.)
decides *what bytes* to send. For simple keys this is obvious: pressing
`a` sends the byte `0x61`. But for modifier combinations and special keys,
things get complicated.

## Legacy encoding (the default)

The original terminal encoding dates back to the 1970s. For ctrl+key
combinations, the terminal computes a "control code" by masking off bits
from the ASCII value:

```
ctrl+] → ']' is 0x5D → 0x5D & 0x1F = 0x1D
ctrl+c → 'c' is 0x63 → 0x63 & 0x1F = 0x03
ctrl+a → 'a' is 0x61 → 0x61 & 0x1F = 0x01
```

This is how `ctrl+c` sends SIGINT (byte `0x03`) and how zinc's `ctrl+]`
detach works (byte `0x1D`). The program just reads a single byte from
stdin.

**The problem:** this encoding is lossy and ambiguous. `ctrl+i` and `Tab`
both produce `0x09`. `ctrl+[` and `Escape` both produce `0x1B`. There's
no way to distinguish `ctrl+shift+a` from `ctrl+a`. For basic CLI tools
this is fine, but for editors and TUIs that want to bind every possible
key combination, it's a dead end.

## Modern keyboard protocols

Two protocols exist to fix this. Both work the same way: the application
sends an escape sequence to the terminal asking it to switch to a richer
encoding. After that, keystrokes arrive as structured escape sequences
instead of raw bytes.

### Protocol 1: Kitty keyboard protocol

Designed by the author of the Kitty terminal. Despite the name, it's
supported by most modern terminals (WezTerm, foot, Ghostty, Rio, iTerm2)
and used by most modern TUI apps (neovim, helix, fish, Claude Code).

**How the app enables it:**

The app writes an escape sequence to stdout:

```
ESC [ > 1 u       → push keyboard mode (flags=1: disambiguate)
ESC [ > 3 u       → push keyboard mode (flags=3: disambiguate + report events)
```

The terminal reads this and changes how it encodes keystrokes.

**How keystrokes change:**

```
Before (legacy):   ctrl+]  →  0x1D  (single byte)
After (Kitty):     ctrl+]  →  ESC [ 93 ; 5 u
                                  │    │   │
                                  │    │   └─ 'u' = CSI u format marker
                                  │    └───── 5 = modifier (1 + ctrl bitmask 4)
                                  └────────── 93 = Unicode codepoint of ']'
```

**How the app disables it:**

```
ESC [ < u          → pop keyboard mode (restore previous)
```

The protocol uses a push/pop stack, so nested TUI apps can each set
their own mode and restore cleanly on exit.

**Full set of control sequences:**

| Sequence         | Meaning                        |
|------------------|--------------------------------|
| `CSI > Ps u`     | Push keyboard mode (enable)    |
| `CSI < u`        | Pop keyboard mode (restore)    |
| `CSI = Ps u`     | Set keyboard flags directly    |
| `CSI ? u`        | Query current mode             |

### Protocol 2: xterm modifyOtherKeys

Older protocol, originating in xterm. Used by vim, emacs, and tmux.

**How the app enables it:**

```
ESC [ > 4 ; 2 m   → enable modifyOtherKeys mode 2
```

**How keystrokes change:**

The encoding format depends on an additional setting (formatOtherKeys):

```
Default format:    ctrl+]  →  ESC [ 27 ; 5 ; 93 ~
                                    │    │    │
                                    │    │    └── 93 = codepoint of ']'
                                    │    └─────── 5 = modifier (ctrl)
                                    └──────────── 27 = fixed magic number

CSI u format:      ctrl+]  →  ESC [ 93 ; 5 u     (same as Kitty)
```

**How the app disables it:**

```
ESC [ > 4 m        → reset modifyOtherKeys to default (off)
```

**Modes:**

| Mode | Behavior |
|------|----------|
| 0    | Disabled. ctrl+] sends `0x1D` as usual |
| 1    | Conservative. Only re-encodes ambiguous keys. ctrl+] likely unchanged |
| 2    | Full. Re-encodes ALL modified keys. ctrl+] becomes an escape sequence |

## Why this matters for zinc

Zinc relays bytes between the agent's PTY and the user's terminal:

```
Agent (nvim) ──PTY──→ Daemon ──socket──→ Client ──stdout──→ User's terminal
```

When nvim starts, it writes `ESC [ > 1 u` to enable the Kitty keyboard
protocol. This sequence flows through the entire chain and reaches the
user's real terminal. Now the user's terminal is in Kitty keyboard mode.

When the user presses `ctrl+]` to detach from zinc, the terminal no
longer sends `0x1D`. It sends `ESC [ 93 ; 5 u`. Zinc's client is
looking for `0x1D`, so it never sees the detach key. The user is stuck.

## What zinc does about it

### Output filtering

The client filters keyboard protocol sequences out of the agent's output
before writing them to the user's terminal:

```
Agent output:  ...text... ESC[>1u ...more text...
                          ^^^^^^^^
                          stripped by filter

User sees:     ...text... ...more text...
```

The filter is a state machine that recognizes:

- `CSI [><=] ... u` — Kitty keyboard protocol (push/pop/flags)
- `CSI > ... m` — xterm modifyOtherKeys (set modifier resources)

It passes through everything else: colors, cursor movement, alternate
screen, device attribute queries, etc.

**Why this is safe:** The agent's own PTY is unaffected. The PTY has its
own terminal emulator that processes these sequences. The agent thinks
keyboard mode is enabled (and it is, on its PTY). We're only preventing
the mode from leaking to the outer terminal.

### Terminal reset on detach

When the user detaches, the client sends reset sequences to clean up
any state the agent changed on the outer terminal:

```
ESC [ < u          → pop keyboard mode (Kitty)
ESC [ > 4 m        → reset modifyOtherKeys (xterm)
ESC [ ? 1049 l     → leave alternate screen
ESC [ m            → reset text attributes (colors, bold, etc.)
ESC [ ? 25 h       → show cursor
```

This is defense-in-depth — if the filter misses something (e.g., the
terminal was already in keyboard mode before zinc started), the reset
ensures the user gets a clean terminal back.

## Reference

- [Kitty keyboard protocol spec](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
- [xterm modifyOtherKeys documentation](https://invisible-island.net/xterm/modified-keys.html)
- [fixterms proposal](http://www.leonerd.org.uk/hacks/fixterms/) — the original CSI u proposal that inspired both protocols

# zinc

Terminal multiplexer for AI coding agents. Daemon-client architecture (like tmux).

## Build & test

```bash
cargo build          # build all crates
cargo test           # run all tests
cargo check          # type-check without building
cargo fmt            # format code
cargo clippy         # lint
```

Binaries: `zinc` (CLI client) and `zincd` (daemon).

## Crate structure

- `crates/zinc-proto` — shared types & wire protocol (Request, Response, AgentState)
- `crates/zinc-daemon` — daemon binary + lib (agent lifecycle, PTY management, socket listener)
- `crates/zinc-cli` — CLI client (connects to daemon, auto-starts it if needed)

## Testing

- Unit tests inline in `#[cfg(test)]` modules
- Integration tests in `crates/zinc-daemon/tests/` — spin up a real daemon on a temp socket
- Use `sleep` as a long-lived test agent, `true`/`false` for exit code testing

## Commits

Prefer fewer, feature-consistent commits over many small ones. Batch related changes into a single commit that represents a coherent unit of work.

## Reference

`zinc.md` contains the full design spec: phases, protocol format, TUI design, provider system.

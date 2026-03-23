use std::io::IsTerminal;
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg, Termios};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use zinc_proto::{Request, Response, ServerMessage};

const DETACH_KEY: u8 = 0x1d; // ctrl-]

pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    pending_events: Vec<zinc_proto::Event>,
}

impl Client {
    /// Try to connect without starting the daemon. Returns None if not running.
    pub async fn try_connect() -> Result<Option<Self>> {
        let socket_path = zinc_proto::default_socket_path();
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                let mut client = Self {
                    reader: BufReader::new(reader),
                    writer,
                    pending_events: Vec::new(),
                };
                client.handshake().await?;
                Ok(Some(client))
            }
            Err(_) => Ok(None),
        }
    }

    /// Connect to the daemon, starting it if necessary.
    pub async fn connect() -> Result<Self> {
        let socket_path = zinc_proto::default_socket_path();

        let stream = match UnixStream::connect(&socket_path).await {
            Ok(s) => s,
            Err(_) => {
                Self::start_daemon()?;
                // Poll until daemon is ready (up to 2s)
                let mut attempts = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    match UnixStream::connect(&socket_path).await {
                        Ok(s) => break s,
                        Err(_) if attempts < 40 => {
                            attempts += 1;
                            continue;
                        }
                        Err(e) => {
                            return Err(e).context("failed to connect to daemon after starting it");
                        }
                    }
                }
            }
        };

        let (reader, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            pending_events: Vec::new(),
        };
        client.handshake().await?;
        Ok(client)
    }

    /// Perform protocol version handshake with the daemon.
    async fn handshake(&mut self) -> Result<()> {
        let resp = self
            .send(Request::Hello {
                protocol_version: zinc_proto::PROTOCOL_VERSION,
            })
            .await?;
        match resp {
            Response::Hello { .. } => Ok(()),
            Response::Error { message } if message.contains("protocol version mismatch") => {
                anyhow::bail!("{}", message);
            }
            Response::Error { .. } => {
                // Old daemon that doesn't understand Hello — suggest restart
                anyhow::bail!(
                    "daemon is running an older version. Run 'zinc shutdown' then retry."
                );
            }
            _ => Ok(()),
        }
    }

    /// Launch the daemon as a background process (`zinc daemon`).
    fn start_daemon() -> Result<()> {
        use std::process::{Command, Stdio};

        let zinc = std::env::current_exe().context("failed to determine current executable")?;

        eprintln!("Starting daemon...");

        unsafe {
            Command::new(&zinc)
                .arg("daemon")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .pre_exec(|| {
                    // Detach from parent session
                    nix::unistd::setsid()
                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                    Ok(())
                })
                .spawn()
                .with_context(|| format!("failed to start daemon via {:?}", zinc))?;
        }

        Ok(())
    }

    /// Send a request and wait for the response.
    /// Any events that arrive before the response are buffered for later retrieval.
    pub async fn send(&mut self, request: Request) -> Result<Response> {
        let mut json = serde_json::to_string(&request)?;
        json.push('\n');
        self.writer.write_all(json.as_bytes()).await?;

        loop {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .await
                .context("lost connection to daemon")?;

            let msg: ServerMessage =
                serde_json::from_str(line.trim()).context("failed to parse daemon message")?;
            match msg {
                ServerMessage::Response(resp) => return Ok(resp),
                ServerMessage::Event(event) => self.pending_events.push(event),
            }
        }
    }

    /// Read the next message from the daemon (blocking).
    /// Returns buffered events first, then waits on the socket.
    pub async fn read_message(&mut self) -> Result<ServerMessage> {
        if let Some(event) = self.pending_events.pop() {
            return Ok(ServerMessage::Event(event));
        }

        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .await
            .context("lost connection to daemon")?;
        if n == 0 {
            anyhow::bail!("lost connection to daemon");
        }
        serde_json::from_str(line.trim()).context("failed to parse daemon message")
    }

    /// Attach to an agent from the CLI: enter raw mode, relay, restore on exit.
    pub async fn attach(self, id: &str) -> Result<()> {
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "cannot attach: stdin is not a terminal"
        );

        let original = enter_raw_mode()?;
        let (cols, rows) = terminal_size();
        let result = self.attach_relay(id, cols, rows).await;
        restore_terminal(&original);
        reset_terminal_state();
        eprintln!("[detached from {}]", id);
        result
    }

    /// Attach handshake + raw byte relay. Does NOT change terminal mode —
    /// caller is responsible for raw mode and cleanup.
    /// Returns when the user detaches (ctrl-]) or the connection closes.
    pub async fn attach_relay(mut self, id: &str, cols: u16, rows: u16) -> Result<()> {
        let resp = self
            .send(Request::Attach {
                id: id.into(),
                cols,
                rows,
            })
            .await?;

        match resp {
            Response::Attached => {}
            Response::Error { message } => {
                anyhow::bail!("{}", message);
            }
            other => {
                anyhow::bail!("unexpected response: {:?}", other);
            }
        }

        self.raw_relay().await
    }

    /// Bidirectional relay: stdin→socket, socket→stdout.
    /// Intercepts the detach key (ctrl-]) to break out.
    ///
    /// Agent output is filtered to strip keyboard protocol sequences
    /// (Kitty keyboard protocol, xterm modifyOtherKeys) so the agent
    /// cannot change the outer terminal's key encoding mode. This ensures
    /// ctrl+] always arrives as raw byte 0x1d regardless of what TUI
    /// the agent runs.
    ///
    /// Stdin is read on a dedicated task to avoid losing keystrokes when
    /// the agent is producing heavy output (tokio::select! drops the
    /// non-selected future each iteration, which can cancel pending
    /// blocking stdin reads).
    async fn raw_relay(&mut self) -> Result<()> {
        let mut stdout = tokio::io::stdout();
        let mut socket_buf = [0u8; 4096];
        let mut filter = KbdProtoFilter::new();
        let mut filtered = Vec::with_capacity(4096);

        // Dedicated stdin reader — never cancelled by select!
        let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if stdin_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        loop {
            tokio::select! {
                biased;
                // Keyboard input → agent (priority: detect detach key promptly)
                Some(data) = stdin_rx.recv() => {
                    if let Some(pos) = data.iter().position(|&b| b == DETACH_KEY) {
                        if pos > 0 {
                            self.writer.write_all(&data[..pos]).await?;
                        }
                        break;
                    }
                    self.writer.write_all(&data).await?;
                }
                // Agent output → terminal (filtered)
                result = self.reader.read(&mut socket_buf) => {
                    match result {
                        Ok(0) => break,
                        Ok(n) => {
                            filtered.clear();
                            filter.filter(&socket_buf[..n], &mut filtered);
                            stdout.write_all(&filtered).await?;
                            stdout.flush().await?;
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        Ok(())
    }
}

/// Strips keyboard protocol escape sequences from a byte stream.
///
/// TUI apps change how the terminal encodes keystrokes by sending protocol
/// sequences. When relayed through zinc, these reach the *outer* terminal
/// and break detach key detection. This filter removes them.
///
/// Filtered sequences:
/// - Kitty keyboard protocol: `CSI > Ps u` (push), `CSI < u` (pop),
///   `CSI = Ps u` (set flags). Used by nvim, Claude Code, helix, and others.
/// - xterm modifyOtherKeys: `CSI > Ps m` (set modifier key resources).
///   Used by vim, emacs, and other xterm-aware apps.
///
/// The agent's own PTY is unaffected — these sequences are consumed by
/// the PTY's terminal emulator on the agent side.
///
/// Handles sequences split across buffer boundaries via a small state machine.
struct KbdProtoFilter {
    pending: Vec<u8>,
    state: FilterState,
}

#[derive(Clone, Copy)]
enum FilterState {
    Normal,
    Esc,                     // saw ESC
    Csi,                     // saw ESC [
    KbdProto { marker: u8 }, // saw ESC [ [><=], accumulating params
}

impl KbdProtoFilter {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            state: FilterState::Normal,
        }
    }

    /// Process `input` bytes, appending filtered output to `out`.
    fn filter(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            match self.state {
                FilterState::Normal => {
                    if b == 0x1b {
                        self.state = FilterState::Esc;
                        self.pending.push(b);
                    } else {
                        out.push(b);
                    }
                }
                FilterState::Esc => {
                    if b == b'[' {
                        self.pending.push(b);
                        self.state = FilterState::Csi;
                    } else {
                        self.emit_pending_and_reprocess(b, out);
                    }
                }
                FilterState::Csi => {
                    if b == b'>' || b == b'<' || b == b'=' {
                        self.pending.push(b);
                        self.state = FilterState::KbdProto { marker: b };
                    } else {
                        self.emit_pending_and_reprocess(b, out);
                    }
                }
                FilterState::KbdProto { marker } => {
                    if self.is_kbd_final_byte(b, marker) {
                        // Complete keyboard protocol sequence — drop it
                        self.pending.clear();
                        self.state = FilterState::Normal;
                    } else if b.is_ascii_digit() || b == b';' {
                        self.pending.push(b);
                    } else {
                        self.emit_pending_and_reprocess(b, out);
                    }
                }
            }
        }
    }

    /// Check if `b` is a final byte that marks a keyboard protocol sequence.
    fn is_kbd_final_byte(&self, b: u8, marker: u8) -> bool {
        match b {
            // Kitty keyboard protocol (all markers)
            b'u' => true,
            // xterm modifyOtherKeys (only CSI > ... m)
            b'm' if marker == b'>' => true,
            _ => false,
        }
    }

    /// Emit buffered pending bytes, clear state, and reprocess the current byte
    /// (which may be the start of a new escape sequence).
    fn emit_pending_and_reprocess(&mut self, b: u8, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.pending);
        self.pending.clear();
        if b == 0x1b {
            self.pending.push(b);
            self.state = FilterState::Esc;
        } else {
            out.push(b);
            self.state = FilterState::Normal;
        }
    }
}

/// Get the current terminal dimensions.
pub(crate) fn terminal_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        libc::ioctl(
            libc::STDOUT_FILENO,
            libc::TIOCGWINSZ as libc::c_ulong,
            &mut ws,
        )
    };
    if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24) // sensible fallback
    }
}

/// Put the terminal into raw mode. Returns the original settings for restoration.
fn enter_raw_mode() -> Result<Termios> {
    let stdin_fd = std::io::stdin();
    let original = tcgetattr(&stdin_fd).context("failed to get terminal attributes")?;
    let mut raw = original.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&stdin_fd, SetArg::TCSAFLUSH, &raw).context("failed to set raw mode")?;
    Ok(original)
}

/// Restore the terminal to the given settings.
fn restore_terminal(original: &Termios) {
    let stdin_fd = std::io::stdin();
    let _ = tcsetattr(&stdin_fd, SetArg::TCSAFLUSH, original);
}

/// Reset terminal state that the agent may have changed.
///
/// TUI agents can change many terminal modes that persist after we restore
/// termios. We send a comprehensive set of reset sequences to ensure the
/// user gets a clean terminal regardless of what the agent did.
pub(crate) fn reset_terminal_state() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(
        concat!(
            // Keyboard protocol resets
            "\x1b[<u",  // pop keyboard mode (Kitty keyboard protocol)
            "\x1b[>4m", // reset modifyOtherKeys to default (xterm)
            // Leave alternate screen (restores main buffer if agent used it)
            "\x1b[?1049l",
            // Disable mouse tracking modes
            "\x1b[?1000l", // normal mouse tracking
            "\x1b[?1002l", // button-event tracking
            "\x1b[?1003l", // any-event tracking
            "\x1b[?1006l", // SGR mouse format
            // Disable other common modes
            "\x1b[?2004l", // bracketed paste
            "\x1b[?1l",    // normal cursor keys (reset DECCKM)
            "\x1b[?7h",    // re-enable line wrapping (DECAWM)
            // Reset visual state
            "\x1b[r",    // reset scrolling region to full screen
            "\x1b[m",    // reset colors/attributes
            "\x1b[?25h", // show cursor
            // Clear screen so agent's TUI content doesn't linger
            "\x1b[H",  // cursor home
            "\x1b[2J", // clear entire screen
        )
        .as_bytes(),
    );
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(input: &[u8]) -> Vec<u8> {
        let mut f = KbdProtoFilter::new();
        let mut out = Vec::new();
        f.filter(input, &mut out);
        out
    }

    #[test]
    fn filter_passes_plain_text() {
        assert_eq!(filtered(b"hello world"), b"hello world");
    }

    #[test]
    fn filter_passes_normal_csi() {
        // Cursor position, SGR color — should pass through
        assert_eq!(filtered(b"\x1b[1;1H"), b"\x1b[1;1H");
        assert_eq!(filtered(b"\x1b[31m"), b"\x1b[31m");
    }

    #[test]
    fn filter_passes_da2_query() {
        // CSI > 0 c — device attributes, final byte 'c' not 'u'
        assert_eq!(filtered(b"\x1b[>0c"), b"\x1b[>0c");
    }

    #[test]
    fn filter_strips_kbd_push() {
        // CSI > 1 u — push keyboard mode
        assert_eq!(filtered(b"\x1b[>1u"), b"");
        // With multiple params
        assert_eq!(filtered(b"\x1b[>1;1u"), b"");
    }

    #[test]
    fn filter_strips_kbd_pop() {
        // CSI < u — pop keyboard mode
        assert_eq!(filtered(b"\x1b[<u"), b"");
        // With params
        assert_eq!(filtered(b"\x1b[<1u"), b"");
    }

    #[test]
    fn filter_strips_kbd_flags() {
        // CSI = 1 u — set keyboard flags
        assert_eq!(filtered(b"\x1b[=1u"), b"");
    }

    #[test]
    fn filter_strips_modify_other_keys() {
        // CSI > 4 ; 2 m — xterm modifyOtherKeys mode 2
        assert_eq!(filtered(b"\x1b[>4;2m"), b"");
        // CSI > 4 m — reset modifyOtherKeys
        assert_eq!(filtered(b"\x1b[>4m"), b"");
    }

    #[test]
    fn filter_passes_csi_less_than_m() {
        // CSI < ... m should NOT be stripped (could be SGR mouse release)
        assert_eq!(filtered(b"\x1b[<0;10;20m"), b"\x1b[<0;10;20m");
    }

    #[test]
    fn filter_preserves_surrounding_data() {
        assert_eq!(filtered(b"before\x1b[>1uafter"), b"beforeafter");
    }

    #[test]
    fn filter_strips_multiple_sequences() {
        assert_eq!(filtered(b"\x1b[>1utext\x1b[<u"), b"text");
    }

    #[test]
    fn filter_handles_split_across_calls() {
        let mut f = KbdProtoFilter::new();
        let mut out = Vec::new();
        // Split ESC[>1u across two calls
        f.filter(b"\x1b[>", &mut out);
        f.filter(b"1u", &mut out);
        assert_eq!(out, b"");
    }

    #[test]
    fn filter_handles_split_at_esc() {
        let mut f = KbdProtoFilter::new();
        let mut out = Vec::new();
        f.filter(b"hello\x1b", &mut out);
        f.filter(b"[>1u", &mut out);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn filter_emits_incomplete_on_non_match() {
        // ESC [ > 1 c — starts like kbd proto but final byte is 'c'
        assert_eq!(filtered(b"\x1b[>1c"), b"\x1b[>1c");
    }

    #[test]
    fn filter_handles_consecutive_esc() {
        // ESC ESC [ > 1 u — first ESC is standalone, second starts the sequence
        let result = filtered(b"\x1b\x1b[>1u");
        assert_eq!(result, b"\x1b");
    }

    #[test]
    fn filter_alt_screen_passes_through() {
        // CSI ? 1049 h — alternate screen, should not be filtered
        assert_eq!(filtered(b"\x1b[?1049h"), b"\x1b[?1049h");
    }
}

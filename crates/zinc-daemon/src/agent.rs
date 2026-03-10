use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::sync::broadcast;
use tracing::error;
use zinc_proto::{AgentInfo, AgentState};

use crate::provider::Provider;
use crate::scrollback::ScrollbackBuffer;

pub struct Agent {
    provider: Arc<dyn Provider>,
    dir: PathBuf,
    state: AgentState,
    child: Child,
    pty_master: Arc<OwnedFd>,
    scrollback: Arc<Mutex<ScrollbackBuffer>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    viewers: Arc<AtomicUsize>,
    started_at: Instant,
    _reader_handle: JoinHandle<()>,
}

impl Agent {
    /// Spawn a new agent process attached to a PTY.
    pub fn spawn(provider: Arc<dyn Provider>, dir: &Path, args: &[String]) -> Result<Self> {
        // Verify directory exists
        anyhow::ensure!(dir.is_dir(), "directory does not exist: {}", dir.display());

        let mut cmd = provider.build_command(dir, args);

        // Create PTY pair
        let pty = openpty(None, None).context("failed to create PTY")?;
        let master = pty.master;
        let slave = pty.slave;

        // Grab raw fd before slave is consumed (valid in child after fork)
        let slave_raw_fd = slave.as_raw_fd();

        // Create stdio from slave PTY
        let stdin_fd = slave.try_clone().context("failed to clone slave fd")?;
        let stdout_fd = slave.try_clone().context("failed to clone slave fd")?;
        let stderr_fd = slave; // consumes original

        let child = unsafe {
            cmd.stdin(Stdio::from(stdin_fd))
                .stdout(Stdio::from(stdout_fd))
                .stderr(Stdio::from(stderr_fd))
                .pre_exec(move || {
                    // Create new session so the agent is detached from our terminal
                    nix::unistd::setsid()
                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                    // Set the slave PTY as the controlling terminal
                    if libc::ioctl(slave_raw_fd, libc::TIOCSCTTY, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .with_context(|| {
                    format!(
                        "failed to spawn '{}' in {}",
                        provider.name(),
                        dir.display()
                    )
                })?
        };

        let master = Arc::new(master);
        let scrollback = Arc::new(Mutex::new(ScrollbackBuffer::default()));
        let (output_tx, _) = broadcast::channel(64);

        // Spawn a reader thread that drains PTY output into scrollback + broadcast
        let reader_handle = {
            let master = master.clone();
            let scrollback = scrollback.clone();
            let output_tx = output_tx.clone();
            std::thread::Builder::new()
                .name("pty-reader".to_string())
                .spawn(move || {
                    pty_reader_loop(master.as_raw_fd(), scrollback, output_tx);
                })
                .context("failed to spawn PTY reader thread")?
        };

        Ok(Self {
            provider,
            dir: dir.to_path_buf(),
            state: AgentState::Working,
            child,
            pty_master: master,
            scrollback,
            output_tx,
            viewers: Arc::new(AtomicUsize::new(0)),
            started_at: Instant::now(),
            _reader_handle: reader_handle,
        })
    }

    /// Check if the child process has exited. Returns Some(exit_code) if so.
    /// Agents that exit are cleaned up by the daemon — exit is an event, not a state.
    pub fn check_exited(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }

    /// Send SIGTERM, wait briefly, then SIGKILL if needed.
    pub fn kill(&mut self) -> Result<()> {
        let pid = Pid::from_raw(self.child.id() as i32);

        // Try graceful shutdown first
        let _ = kill(pid, Signal::SIGTERM);
        std::thread::sleep(std::time::Duration::from_millis(200));

        match self.child.try_wait() {
            Ok(Some(_)) => Ok(()),
            _ => {
                let _ = kill(pid, Signal::SIGKILL);
                let _ = self.child.wait();
                Ok(())
            }
        }
    }

    /// Build an AgentInfo snapshot for reporting to clients.
    pub fn info(&self, id: &str) -> AgentInfo {
        AgentInfo {
            id: id.to_string(),
            provider: self.provider.name().to_string(),
            dir: self.dir.clone(),
            state: self.state,
            pid: Some(self.child.id()),
            uptime_secs: self.started_at.elapsed().as_secs(),
            viewers: self.viewers.load(Ordering::Relaxed),
        }
    }

    /// Get the viewer count handle for increment/decrement by attach sessions.
    pub fn viewers(&self) -> Arc<AtomicUsize> {
        self.viewers.clone()
    }

    /// Subscribe to live PTY output. Returns a broadcast receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// Get a copy of the current scrollback buffer contents.
    pub fn scrollback_contents(&self) -> Vec<u8> {
        self.scrollback.lock().unwrap().to_vec()
    }

    /// Get a clone of the PTY master fd (kept alive by Arc).
    pub fn pty_master(&self) -> Arc<OwnedFd> {
        self.pty_master.clone()
    }

    /// Resize the agent's PTY and notify the agent process.
    pub fn resize(&self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.pty_master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
        // Notify the agent process of the resize
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGWINCH);
    }
}

/// Blocking loop that reads PTY master output, stores it in the scrollback buffer,
/// and broadcasts it to any attached clients.
/// Exits when the PTY slave side is closed (agent exits).
fn pty_reader_loop(
    master_fd: i32,
    scrollback: Arc<Mutex<ScrollbackBuffer>>,
    output_tx: broadcast::Sender<Vec<u8>>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match nix::unistd::read(master_fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let data = buf[..n].to_vec();
                if let Ok(mut sb) = scrollback.lock() {
                    sb.write(&data);
                }
                // Ignore send errors (no receivers is fine)
                let _ = output_tx.send(data);
            }
            Err(nix::errno::Errno::EIO) => break, // PTY closed
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                error!("PTY read error: {}", e);
                break;
            }
        }
    }
}

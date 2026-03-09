use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tracing::error;
use zinc_proto::{AgentInfo, AgentState};

use crate::scrollback::ScrollbackBuffer;

pub struct Agent {
    provider: String,
    dir: PathBuf,
    state: AgentState,
    child: Child,
    _pty_master: Arc<OwnedFd>,
    _scrollback: Arc<Mutex<ScrollbackBuffer>>,
    started_at: Instant,
    _reader_handle: JoinHandle<()>,
}

impl Agent {
    /// Spawn a new agent process attached to a PTY.
    pub fn spawn(provider: &str, dir: &Path, args: &[String]) -> Result<Self> {
        let command = provider_command(provider);

        // Verify directory exists
        anyhow::ensure!(dir.is_dir(), "directory does not exist: {}", dir.display());

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
            Command::new(&command)
                .args(args)
                .current_dir(dir)
                .stdin(Stdio::from(stdin_fd))
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
                .with_context(|| format!("failed to spawn '{}' in {}", command, dir.display()))?
        };

        let master = Arc::new(master);
        let scrollback = Arc::new(Mutex::new(ScrollbackBuffer::default()));

        // Spawn a reader thread that drains PTY output into the scrollback buffer
        let reader_handle = {
            let master = master.clone();
            let scrollback = scrollback.clone();
            std::thread::Builder::new()
                .name(format!("pty-reader"))
                .spawn(move || {
                    pty_reader_loop(master.as_raw_fd(), scrollback);
                })
                .context("failed to spawn PTY reader thread")?
        };

        Ok(Self {
            provider: provider.to_string(),
            dir: dir.to_path_buf(),
            state: AgentState::Working,
            child,
            _pty_master: master,
            _scrollback: scrollback,
            started_at: Instant::now(),
            _reader_handle: reader_handle,
        })
    }

    /// Update state by checking if the child process is still alive.
    pub fn refresh_state(&mut self) {
        // Don't overwrite terminal states
        if matches!(self.state, AgentState::Done | AgentState::Error) {
            return;
        }

        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.state = if status.success() {
                    AgentState::Done
                } else {
                    AgentState::Error
                };
            }
            Ok(None) => {
                // Still running — Phase 0 just reports "working"
                self.state = AgentState::Working;
            }
            Err(_) => {
                self.state = AgentState::Error;
            }
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
            provider: self.provider.clone(),
            dir: self.dir.clone(),
            state: self.state,
            pid: Some(self.child.id()),
            uptime_secs: self.started_at.elapsed().as_secs(),
        }
    }
}

/// Resolve provider name to the CLI command.
fn provider_command(provider: &str) -> String {
    // For now, the provider name IS the command.
    // Custom mappings can be added later via config.
    provider.to_string()
}

/// Blocking loop that reads PTY master output and stores it in the scrollback buffer.
/// Exits when the PTY slave side is closed (agent exits).
fn pty_reader_loop(master_fd: i32, scrollback: Arc<Mutex<ScrollbackBuffer>>) {
    let mut buf = [0u8; 4096];
    loop {
        match nix::unistd::read(master_fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut sb) = scrollback.lock() {
                    sb.write(&buf[..n]);
                }
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

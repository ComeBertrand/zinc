use std::os::unix::process::CommandExt;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use zinc_proto::{Request, Response};

pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    /// Try to connect without starting the daemon. Returns None if not running.
    pub async fn try_connect() -> Result<Option<Self>> {
        let socket_path = zinc_proto::default_socket_path();
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                Ok(Some(Self {
                    reader: BufReader::new(reader),
                    writer,
                }))
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
                            return Err(e)
                                .context("failed to connect to daemon after starting it");
                        }
                    }
                }
            }
        };

        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
        })
    }

    /// Launch zincd as a background process.
    fn start_daemon() -> Result<()> {
        use std::process::{Command, Stdio};

        // Look for zincd next to the zinc binary
        let zincd = std::env::current_exe()?
            .parent()
            .map(|p| p.join("zincd"))
            .unwrap_or_else(|| "zincd".into());

        eprintln!("Starting daemon...");

        unsafe {
            Command::new(&zincd)
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
                .with_context(|| format!("failed to start zincd at {:?}", zincd))?;
        }

        Ok(())
    }

    /// Send a request and wait for the response.
    pub async fn send(&mut self, request: Request) -> Result<Response> {
        let mut json = serde_json::to_string(&request)?;
        json.push('\n');
        self.writer.write_all(json.as_bytes()).await?;

        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .await
            .context("lost connection to daemon")?;

        serde_json::from_str(line.trim()).context("failed to parse daemon response")
    }
}

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod agent;
mod daemon;
mod scrollback;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Detach from controlling terminal so we survive terminal close
    #[cfg(unix)]
    {
        let _ = nix::unistd::setsid();
    }

    let socket_path = zinc_proto::default_socket_path();
    let d = daemon::Daemon::new(socket_path);
    d.run().await
}

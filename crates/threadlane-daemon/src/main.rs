use std::path::PathBuf;
use std::sync::Arc;
use threadlane_daemon::{DaemonServer, RpcDispatcher};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting Threadlane Daemon v{}", env!("CARGO_PKG_VERSION"));

    let dispatcher = Arc::new(RpcDispatcher::new());
    let server = DaemonServer::new(dispatcher);

    // 1. Start Unix Domain Socket listener
    let socket_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".threadlane")
        .join("daemon.sock");
    server.serve_uds(socket_path).await?;

    // 2. Start WebSocket TCP listener for web/mobile clients
    let port = std::env::var("THREADLANE_DAEMON_PORT").unwrap_or_else(|_| "9234".to_string());
    let bind_addr = format!("127.0.0.1:{port}");
    server.serve_ws(&bind_addr).await?;

    info!("Threadlane Daemon is ready. Press Ctrl+C to stop.");

    // Wait for termination signal
    tokio::signal::ctrl_c().await?;
    info!("Shutting down Threadlane Daemon...");

    Ok(())
}

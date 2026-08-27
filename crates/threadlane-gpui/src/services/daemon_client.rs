use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use threadlane_protocol::client::DaemonClient;
use tokio::sync::OnceCell;
use tracing::{info, warn};

static DAEMON_CLIENT: OnceCell<Arc<DaemonClient>> = OnceCell::const_new();

pub fn default_daemon_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".threadlane")
        .join("daemon.sock")
}

/// Spawns the local daemon binary if not already running.
pub fn ensure_local_daemon_running() {
    let socket_path = default_daemon_socket_path();
    if socket_path.exists() {
        return;
    }

    info!("Spawning background threadlane-daemon...");
    if let Ok(child) = Command::new("threadlane-daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        // Detach process
        let _ = child.id();
        std::thread::sleep(Duration::from_millis(200));
    } else {
        warn!("Failed to auto-spawn threadlane-daemon executable. Daemon may need manual launch.");
    }
}

/// Returns the shared daemon client instance for GPUI.
pub async fn get_daemon_client() -> Result<Arc<DaemonClient>, String> {
    DAEMON_CLIENT
        .get_or_try_init(|| async {
            ensure_local_daemon_running();
            let socket_path = default_daemon_socket_path();
            let client = DaemonClient::connect_uds(&socket_path).await?;
            Ok(Arc::new(client))
        })
        .await
        .cloned()
}

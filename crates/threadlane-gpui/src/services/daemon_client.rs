use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use threadlane_protocol::client::DaemonClient;
use tokio::runtime::Runtime;
use tokio::sync::OnceCell;
use tracing::{info, warn};

static DAEMON_CLIENT: OnceCell<Arc<DaemonClient>> = OnceCell::const_new();
static DAEMON_RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn daemon_runtime() -> &'static Runtime {
    DAEMON_RUNTIME.get_or_init(|| Runtime::new().expect("create daemon client Tokio runtime"))
}

pub fn default_daemon_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".threadlane")
        .join("daemon.sock")
}

fn daemon_executable() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("THREADLANE_DAEMON_PATH") {
        candidates.push(PathBuf::from(path));
    }

    // In development the daemon is built beside the GPUI binary. In an app
    // bundle both executables are placed in Contents/MacOS.
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("threadlane-daemon"));
        }
    }

    candidates.into_iter().find(|path| path.is_file())
}

/// Spawns the local daemon binary if not already running.
pub fn ensure_local_daemon_running() {
    let socket_path = default_daemon_socket_path();
    if socket_path.exists() {
        return;
    }

    let executable = daemon_executable().unwrap_or_else(|| PathBuf::from("threadlane-daemon"));
    info!(
        "Spawning background threadlane-daemon from {}...",
        executable.display()
    );
    match Command::new(&executable)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let _ = child.id();
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(error) => {
            warn!(
                "Failed to auto-spawn threadlane-daemon from {}: {error}. Daemon may need manual launch.",
                executable.display()
            );
        }
    }
}

/// Returns the shared daemon client instance for GPUI.
pub async fn get_daemon_client() -> Result<Arc<DaemonClient>, String> {
    DAEMON_CLIENT
        .get_or_try_init(|| async {
            ensure_local_daemon_running();
            let socket_path = default_daemon_socket_path();
            let client = daemon_runtime()
                .spawn(async move { DaemonClient::connect_uds(&socket_path).await })
                .await
                .map_err(|error| format!("Daemon client task failed: {error}"))??;
            Ok(Arc::new(client))
        })
        .await
        .cloned()
}

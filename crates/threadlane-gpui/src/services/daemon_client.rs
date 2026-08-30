use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use threadlane_daemon::{DaemonServer, RpcDispatcher};
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

    let found = candidates.into_iter().find(|path| path.is_file());
    if let (Some(daemon), Ok(app)) = (&found, std::env::current_exe()) {
        // A stale dev daemon produces confusing misbehavior (missing RPC
        // methods, silent event loss); warn instead of failing silently.
        let daemon_newer = |a: &PathBuf, b: &PathBuf| {
            let a_time = std::fs::metadata(a).and_then(|m| m.modified()).ok();
            let b_time = std::fs::metadata(b).and_then(|m| m.modified()).ok();
            matches!((a_time, b_time), (Some(a), Some(b)) if a < b)
        };
        if daemon_newer(daemon, &app) {
            warn!(
                "threadlane-daemon at {} is older than the app binary; rebuild it (cargo build -p threadlane-daemon) or set THREADLANE_DAEMON_PATH",
                daemon.display()
            );
        }
    }
    found
}

/// Spawns the local daemon binary if not already running.
pub fn ensure_local_daemon_running() {
    let socket_path = default_daemon_socket_path();
    if socket_path.exists() {
        info!(
            "Reusing existing threadlane-daemon at {}; use --restart-daemon for fresh debug logs",
            socket_path.display()
        );
        return;
    }

    let executable = daemon_executable().unwrap_or_else(|| PathBuf::from("threadlane-daemon"));
    info!(
        "Spawning background threadlane-daemon from {}...",
        executable.display()
    );
    let inherit_stdio = std::env::var("THREADLANE_DAEMON_STDIO")
        .map(|value| value == "inherit")
        .unwrap_or(false);
    let mut command = Command::new(&executable);
    if inherit_stdio {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }

    match command.spawn() {
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

/// Starts the daemon server in-process on the daemon runtime. Used when no
/// external daemon binary could be launched or reached (e.g. a bare
/// `cargo run -p threadlane-gpui` dev run), so the app is self-contained.
fn start_embedded_daemon(socket_path: PathBuf) {
    static EMBEDDED_DAEMON: OnceLock<()> = OnceLock::new();
    if EMBEDDED_DAEMON.get().is_some() {
        return;
    }
    if EMBEDDED_DAEMON.set(()).is_err() {
        return;
    }
    info!(
        "Starting embedded in-process daemon at {}",
        socket_path.display()
    );
    daemon_runtime().spawn(async move {
        let server = DaemonServer::new(Arc::new(RpcDispatcher::new()));
        if let Err(error) = server.serve_uds(socket_path).await {
            warn!("Embedded daemon failed to serve: {error}");
        }
    });
}

async fn connect_with_retries(
    socket_path: &PathBuf,
    attempts: usize,
    delay: Duration,
) -> Result<DaemonClient, String> {
    let mut last_error = String::from("daemon not attempted");
    for _ in 0..attempts {
        match DaemonClient::connect_uds(socket_path).await {
            Ok(client) => return Ok(client),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(delay).await;
    }
    Err(last_error)
}

/// Returns the shared daemon client instance for GPUI.
pub async fn get_daemon_client() -> Result<Arc<DaemonClient>, String> {
    DAEMON_CLIENT
        .get_or_try_init(|| async {
            ensure_local_daemon_running();
            let socket_path = default_daemon_socket_path();

            // Give an externally spawned daemon time to bind before falling
            // back to the embedded in-process server. Connection attempts run
            // on the daemon runtime, which owns the Tokio reactor.
            let attempt_socket = socket_path.clone();
            let external = daemon_runtime()
                .spawn(async move {
                    connect_with_retries(&attempt_socket, 10, Duration::from_millis(100)).await
                })
                .await
                .map_err(|error| format!("Daemon client task failed: {error}"))?;
            let client = match external {
                Ok(client) => client,
                Err(external_error) => {
                    info!("External daemon unavailable ({external_error}); using embedded daemon");
                    start_embedded_daemon(socket_path.clone());
                    daemon_runtime()
                        .spawn(async move {
                            connect_with_retries(&socket_path, 10, Duration::from_millis(100)).await
                        })
                        .await
                        .map_err(|error| format!("Daemon client task failed: {error}"))??
                }
            };
            Ok(Arc::new(client))
        })
        .await
        .cloned()
}

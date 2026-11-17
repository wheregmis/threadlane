//! Daemon-hosted pseudo-terminals.
//!
//! The daemon owns real PTYs on its own machine so clients — local or
//! remote — get the host's shell. Each terminal is addressed by a
//! client-chosen `terminal_id` (unique per spawn); output and exit
//! lifecycle stream out as `SessionEvent::TerminalEvent` frames through the
//! ingest channel, and `TerminalInput`/`TerminalResize`/`TerminalClose`
//! commands drive the other direction.
//!
//! Output flows reader thread → batcher thread → ingest. The batcher
//! coalesces bursts into frames so a flood produces bounded events per
//! second instead of one event per read, and so interactive echo arrives
//! with one flush window of latency at worst.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;

use threadlane_protocol::daemon::{SessionEvent, TerminalEvent};

/// Largest payload a single `TerminalEvent::Output` frame carries.
const OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
/// How long the batcher waits to coalesce a burst into one frame.
const OUTPUT_FLUSH_WINDOW: Duration = Duration::from_millis(12);
/// Reader drain size; matches the client's terminal read chunk.
const READ_CHUNK_BYTES: usize = 8192;

/// `std::fs::canonicalize` returns verbatim `\\?\` paths on Windows, which
/// children (cmd.exe most visibly) cannot use as a working directory, so
/// downgrade the common drive-letter form back to a plain path.
/// Intentionally duplicated in `threadlane-ui-terminal`: that leaf UI crate
/// takes no dependency on server-side crates — keep the copies in sync.
#[cfg(windows)]
fn simplified_cwd(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    match text.strip_prefix("\\\\?\\") {
        Some(rest) if rest.len() >= 2 && rest.as_bytes()[1] == b':' => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Shell programs to try, in order. On Windows, POSIX-style `SHELL` values
/// (e.g. `/bin/sh` inherited from Git Bash or MSYS) are not spawnable via
/// CreateProcess, so `SHELL`/`COMSPEC` only qualify when they point at a real
/// file — and even a real file can fail to spawn (a batch script or data
/// file), so callers must fall through the list on spawn errors.
/// Intentionally duplicated in `threadlane-ui-terminal` — keep in sync.
fn shell_candidates() -> Vec<String> {
    if cfg!(windows) {
        let mut candidates = Vec::new();
        if let Some(shell) = std::env::var("SHELL")
            .ok()
            .filter(|shell| Path::new(shell).is_file())
        {
            candidates.push(shell);
        }
        if let Some(comspec) = std::env::var("COMSPEC")
            .ok()
            .filter(|comspec| !comspec.is_empty() && Path::new(comspec).is_file())
        {
            candidates.push(comspec);
        }
        candidates.push("cmd.exe".into());
        candidates
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    }
}

struct PtyHandle {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// Hosts pseudo-terminals owned by the daemon process.
#[derive(Default)]
pub struct TerminalManager {
    sessions: Arc<Mutex<HashMap<String, Arc<PtyHandle>>>>,
}

impl TerminalManager {
    /// Spawn an interactive shell in `cwd` and start streaming its output.
    /// `ingest` is the daemon's event channel — every lifecycle event goes
    /// to every attached client.
    pub fn open(
        &self,
        terminal_id: &str,
        cwd: &Path,
        cols: u16,
        rows: u16,
        ingest: mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<(), String> {
        {
            let sessions = self.sessions.lock().expect("terminals poisoned");
            if sessions.contains_key(terminal_id) {
                return Err(format!("terminal {terminal_id} already exists"));
            }
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("could not open pty: {error}"))?;
        // Selection logic mirrors `threadlane-ui-terminal` — keep in sync.
        let mut last_spawn_error = None;
        let mut child = None;
        for shell in shell_candidates() {
            let mut command = CommandBuilder::new(shell);
            #[cfg(windows)]
            command.cwd(simplified_cwd(cwd));
            #[cfg(not(windows))]
            command.cwd(cwd);
            command.env("TERM", "xterm-256color");
            if !cfg!(windows) {
                command.arg("-i");
            }
            match pair.slave.spawn_command(command) {
                Ok(spawned) => {
                    child = Some(spawned);
                    break;
                }
                Err(error) => last_spawn_error = Some(error),
            }
        }
        let child = child.ok_or_else(|| {
            format!(
                "could not spawn shell in {}: {}",
                cwd.display(),
                last_spawn_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "no shell candidates".into())
            )
        })?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("could not clone pty reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("could not take pty writer: {error}"))?;
        drop(pair.slave);

        let handle = Arc::new(PtyHandle {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
        });
        // Register before the pump threads so a shell that exits instantly
        // still has an entry for the batcher to reap (and Exited to send);
        // a spawn failure below removes the entry and drops the handle,
        // killing the child.
        self.sessions
            .lock()
            .expect("terminals poisoned")
            .insert(terminal_id.to_string(), handle);

        // Reader: raw PTY bytes into a channel the batcher drains.
        let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let sessions = self.sessions.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("threadlane-daemon-pty-reader-{terminal_id}"))
            .spawn(move || {
                let mut buffer = [0_u8; READ_CHUNK_BYTES];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if chunk_tx.send(buffer[..read].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
        {
            sessions
                .lock()
                .expect("terminals poisoned")
                .remove(terminal_id);
            return Err(format!("could not spawn pty reader: {error}"));
        }

        // Batcher: coalesce bursts into Output frames; on channel close the
        // shell ended — report Exited only when the entry is still ours
        // (a TerminalClose that already removed it skips the duplicate).
        let sessions = self.sessions.clone();
        let batch_id = terminal_id.to_string();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("threadlane-daemon-pty-batch-{batch_id}"))
            .spawn(move || {
                let terminal_id = batch_id;
                // Holds an incomplete trailing UTF-8 sequence split by a
                // frame boundary; a multibyte char must survive the split
                // instead of becoming replacement chars in both frames.
                let mut tail = Vec::new();
                while let Ok(first) = chunk_rx.recv() {
                    let mut data = std::mem::take(&mut tail);
                    data.extend_from_slice(&first);
                    let deadline = Instant::now() + OUTPUT_FLUSH_WINDOW;
                    while data.len() < OUTPUT_CHUNK_BYTES {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match chunk_rx.recv_timeout(remaining) {
                            Ok(chunk) => data.extend_from_slice(&chunk),
                            Err(_) => break,
                        }
                    }
                    while data.len() < OUTPUT_CHUNK_BYTES {
                        match chunk_rx.try_recv() {
                            Ok(chunk) => data.extend_from_slice(&chunk),
                            Err(_) => break,
                        }
                    }
                    if let Err(error) = std::str::from_utf8(&data) {
                        if error.error_len().is_none() {
                            tail = data.split_off(error.valid_up_to());
                        }
                    }
                    if data.is_empty() {
                        continue;
                    }
                    if ingest
                        .send(SessionEvent::TerminalEvent {
                            event: TerminalEvent::Output {
                                terminal_id: terminal_id.clone(),
                                data: String::from_utf8_lossy(&data).into_owned(),
                            },
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                if !tail.is_empty() {
                    let _ = ingest.send(SessionEvent::TerminalEvent {
                        event: TerminalEvent::Output {
                            terminal_id: terminal_id.clone(),
                            data: String::from_utf8_lossy(&tail).into_owned(),
                        },
                    });
                }
                let removed = {
                    let mut sessions = sessions.lock().expect("terminals poisoned");
                    sessions.remove(&terminal_id)
                };
                if let Some(handle) = removed {
                    // EOF means the slave side closed — wait() returns once
                    // the shell is reaped instead of racing try_wait.
                    let exit_code = handle
                        .child
                        .lock()
                        .ok()
                        .and_then(|mut child| child.wait().ok())
                        .and_then(|status| {
                            status.signal().is_none().then(|| status.exit_code() as i32)
                        });
                    let _ = ingest.send(SessionEvent::TerminalEvent {
                        event: TerminalEvent::Exited {
                            terminal_id: terminal_id.clone(),
                            exit_code,
                        },
                    });
                }
            })
        {
            self.sessions
                .lock()
                .expect("terminals poisoned")
                .remove(terminal_id);
            return Err(format!("could not spawn pty output pump: {error}"));
        }
        Ok(())
    }

    /// Write keyboard input into the terminal.
    pub fn input(&self, terminal_id: &str, data: &str) -> Result<(), String> {
        let handle = self
            .sessions
            .lock()
            .expect("terminals poisoned")
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| format!("unknown terminal {terminal_id}"))?;
        let mut writer = handle.writer.lock().expect("pty writer poisoned");
        writer
            .write_all(data.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| format!("pty write failed: {error}"))
    }

    /// Resize the terminal; other attached clients hear about it through a
    /// `TerminalEvent::Resized` frame.
    pub fn resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
        ingest: &mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<(), String> {
        let handle = self
            .sessions
            .lock()
            .expect("terminals poisoned")
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| format!("unknown terminal {terminal_id}"))?;
        handle
            .master
            .lock()
            .expect("pty master poisoned")
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("pty resize failed: {error}"))?;
        let _ = ingest.send(SessionEvent::TerminalEvent {
            event: TerminalEvent::Resized {
                terminal_id: terminal_id.to_string(),
                cols,
                rows,
            },
        });
        Ok(())
    }

    /// Kill the shell. The entry stays until the reader hits EOF and the
    /// batcher reaps it, which keeps `Exited` ordered after the final
    /// output chunk.
    pub fn close(&self, terminal_id: &str) {
        let handle = self
            .sessions
            .lock()
            .expect("terminals poisoned")
            .get(terminal_id)
            .cloned();
        if let Some(handle) = handle {
            if let Ok(mut child) = handle.child.lock() {
                let _ = child.kill();
            }
        }
    }
}

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use threadlane_protocol::terminal::*;
use tokio::sync::broadcast;

struct ActiveTerminal {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[derive(Clone)]
pub struct TerminalService {
    terminals: Arc<Mutex<HashMap<String, ActiveTerminal>>>,
    output_broadcaster: broadcast::Sender<TerminalOutputEvent>,
}

impl Default for TerminalService {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalService {
    pub fn new() -> Self {
        let (output_broadcaster, _) = broadcast::channel(1024);
        Self {
            terminals: Arc::new(Mutex::new(HashMap::new())),
            output_broadcaster,
        }
    }

    pub fn subscribe_output(&self) -> broadcast::Receiver<TerminalOutputEvent> {
        self.output_broadcaster.subscribe()
    }

    pub fn spawn_terminal(&self, req: SpawnTerminalRequest) -> Result<TerminalSpawnedResponse, String> {
        let terminal_id = req.terminal_id.unwrap_or_else(|| format!("term_{}", uuid_v4_like()));
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: req.rows,
                cols: req.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {e}"))?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(Path::new(&req.project_path));

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {e}"))?;

        let pid = child.process_id();
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to get PTY writer: {e}"))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {e}"))?;

        let output_tx = self.output_broadcaster.clone();
        let term_id_clone = terminal_id.clone();

        // Spawn background reader thread for PTY output
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = output_tx.send(TerminalOutputEvent {
                            terminal_id: term_id_clone.clone(),
                            data: String::new(),
                            exit_code: Some(0),
                        });
                        break;
                    }
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = output_tx.send(TerminalOutputEvent {
                            terminal_id: term_id_clone.clone(),
                            data,
                            exit_code: None,
                        });
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        let mut lock = self.terminals.lock().map_err(|e| e.to_string())?;
        lock.insert(
            terminal_id.clone(),
            ActiveTerminal {
                writer,
                master: pair.master,
                _child: child,
            },
        );

        Ok(TerminalSpawnedResponse {
            terminal_id,
            pid,
        })
    }

    pub fn write_input(&self, req: TerminalInputRequest) -> Result<(), String> {
        let mut lock = self.terminals.lock().map_err(|e| e.to_string())?;
        if let Some(term) = lock.get_mut(&req.terminal_id) {
            term.writer
                .write_all(req.data.as_bytes())
                .map_err(|e| format!("Failed to write to terminal: {e}"))?;
            term.writer.flush().map_err(|e| format!("Failed to flush terminal: {e}"))?;
            Ok(())
        } else {
            Err(format!("Terminal '{}' not found", req.terminal_id))
        }
    }

    pub fn resize_terminal(&self, req: ResizeTerminalRequest) -> Result<(), String> {
        let lock = self.terminals.lock().map_err(|e| e.to_string())?;
        if let Some(term) = lock.get(&req.terminal_id) {
            term.master
                .resize(PtySize {
                    rows: req.rows,
                    cols: req.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("Failed to resize terminal: {e}"))?;
            Ok(())
        } else {
            Err(format!("Terminal '{}' not found", req.terminal_id))
        }
    }

    pub fn close_terminal(&self, req: CloseTerminalRequest) -> Result<TerminalClosedResponse, String> {
        let mut lock = self.terminals.lock().map_err(|e| e.to_string())?;
        if lock.remove(&req.terminal_id).is_some() {
            Ok(TerminalClosedResponse {
                terminal_id: req.terminal_id,
                exit_code: Some(0),
            })
        } else {
            Err(format!("Terminal '{}' not found", req.terminal_id))
        }
    }
}

fn uuid_v4_like() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

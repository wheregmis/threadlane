use crate::connection::ConnectionHandler;
use crate::dispatcher::RpcDispatcher;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tracing::{error, info, warn};

pub struct DaemonServer {
    dispatcher: Arc<RpcDispatcher>,
}

impl DaemonServer {
    pub fn new(dispatcher: Arc<RpcDispatcher>) -> Self {
        Self { dispatcher }
    }

    /// Serves incoming client connections over a Unix Domain Socket (macOS / Linux).
    pub async fn serve_uds(&self, socket_path: PathBuf) -> Result<(), String> {
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("Failed to bind UDS at {}: {e}", socket_path.display()))?;

        info!(
            "Daemon listening on Unix Domain Socket: {}",
            socket_path.display()
        );

        let dispatcher = self.dispatcher.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let handler = ConnectionHandler::new(dispatcher.clone());
                        let (reader, writer) = stream.into_split();
                        tokio::spawn(async move {
                            handler.handle_stream(reader, writer).await;
                        });
                    }
                    Err(err) => {
                        error!("UDS accept error: {err}");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Serves incoming WebSocket clients over TCP (Web / Mobile / Remote Dev).
    pub async fn serve_ws(&self, bind_addr: &str) -> Result<(), String> {
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("Failed to bind TCP at {bind_addr}: {e}"))?;

        info!("Daemon listening on WebSocket endpoint: ws://{bind_addr}");

        let dispatcher = self.dispatcher.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let dispatcher_clone = dispatcher.clone();
                        tokio::spawn(async move {
                            match tokio_tungstenite::accept_async(stream).await {
                                Ok(ws_stream) => {
                                    let handler = ConnectionHandler::new(dispatcher_clone);
                                    handler.handle_websocket(ws_stream).await;
                                }
                                Err(err) => {
                                    warn!("WebSocket handshake failed: {err}");
                                }
                            }
                        });
                    }
                    Err(err) => {
                        error!("TCP accept error: {err}");
                        break;
                    }
                }
            }
        });

        Ok(())
    }
}

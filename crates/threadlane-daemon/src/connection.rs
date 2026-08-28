use crate::dispatcher::RpcDispatcher;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use threadlane_protocol::rpc::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

pub struct ConnectionHandler {
    dispatcher: Arc<RpcDispatcher>,
}

impl ConnectionHandler {
    pub fn new(dispatcher: Arc<RpcDispatcher>) -> Self {
        Self { dispatcher }
    }

    /// Handles a line-delimited stream (e.g. Unix Domain Socket or standard TCP).
    pub async fn handle_stream<R, W>(&self, reader: R, mut writer: W)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let mut lines = BufReader::new(reader).lines();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(128);

        // Writer pump
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Reader pump
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<RpcRequest>(line) {
                Ok(req) => {
                    let method = req.method.clone();
                    let req_id = req.id.clone();
                    let params = req.params.clone();

                    if method == "session/subscribe" {
                        let session_id = params
                            .and_then(|p| {
                                p.get("session_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .unwrap_or_default();

                        match self
                            .dispatcher
                            .session_service
                            .subscribe_session(&session_id)
                            .await
                        {
                            Ok(mut event_rx) => {
                                let out_tx_clone = out_tx.clone();
                                tokio::spawn(async move {
                                    while let Ok(event) = event_rx.recv().await {
                                        let notif = RpcNotification::new(
                                            "session/event",
                                            Some(serde_json::to_value(&event).unwrap()),
                                        );
                                        let json = serde_json::to_string(&notif).unwrap();
                                        if out_tx_clone.send(json).await.is_err() {
                                            break;
                                        }
                                    }
                                });
                                let res = RpcResponse::success(req_id, Value::Bool(true));
                                let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                            }
                            Err(err) => {
                                let res = RpcResponse::error(
                                    req_id,
                                    RpcError::new(ERROR_SESSION_NOT_FOUND, err),
                                );
                                let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                            }
                        }
                    } else if method == "terminal/subscribe" {
                        let mut term_rx = self.dispatcher.terminal_service.subscribe_output();
                        let out_tx_clone = out_tx.clone();
                        tokio::spawn(async move {
                            while let Ok(event) = term_rx.recv().await {
                                let notif = RpcNotification::new(
                                    "terminal/event",
                                    Some(serde_json::to_value(&event).unwrap()),
                                );
                                let json = serde_json::to_string(&notif).unwrap();
                                if out_tx_clone.send(json).await.is_err() {
                                    break;
                                }
                            }
                        });
                        let res = RpcResponse::success(req_id, Value::Bool(true));
                        let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                    } else {
                        let response = self.dispatcher.dispatch(req).await;
                        let json = serde_json::to_string(&response).unwrap();
                        if out_tx.send(json).await.is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let res = RpcResponse::error(
                        0u64,
                        RpcError::new(
                            ERROR_PARSE_ERROR,
                            format!("Failed to parse JSON-RPC: {err}"),
                        ),
                    );
                    let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                }
            }
        }

        let _ = writer_task.await;
    }

    /// Handles a WebSocket stream (e.g. for web and mobile frontends).
    pub async fn handle_websocket<S>(&self, ws_stream: tokio_tungstenite::WebSocketStream<S>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(128);

        // Forward outgoing messages to websocket
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if ws_sender.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
        });

        while let Some(msg_result) = ws_receiver.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    if let Ok(req) = serde_json::from_str::<RpcRequest>(&text) {
                        let method = req.method.clone();
                        let req_id = req.id.clone();
                        let params = req.params.clone();

                        if method == "session/subscribe" {
                            let session_id = params
                                .and_then(|p| {
                                    p.get("session_id")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                })
                                .unwrap_or_default();

                            match self
                                .dispatcher
                                .session_service
                                .subscribe_session(&session_id)
                                .await
                            {
                                Ok(mut event_rx) => {
                                    let out_tx_clone = out_tx.clone();
                                    tokio::spawn(async move {
                                        while let Ok(event) = event_rx.recv().await {
                                            let notif = RpcNotification::new(
                                                "session/event",
                                                Some(serde_json::to_value(&event).unwrap()),
                                            );
                                            let json = serde_json::to_string(&notif).unwrap();
                                            if out_tx_clone.send(json).await.is_err() {
                                                break;
                                            }
                                        }
                                    });
                                    let res = RpcResponse::success(req_id, Value::Bool(true));
                                    let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                                }
                                Err(err) => {
                                    let res = RpcResponse::error(
                                        req_id,
                                        RpcError::new(ERROR_SESSION_NOT_FOUND, err),
                                    );
                                    let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                                }
                            }
                        } else if method == "terminal/subscribe" {
                            let mut term_rx = self.dispatcher.terminal_service.subscribe_output();
                            let out_tx_clone = out_tx.clone();
                            tokio::spawn(async move {
                                while let Ok(event) = term_rx.recv().await {
                                    let notif = RpcNotification::new(
                                        "terminal/event",
                                        Some(serde_json::to_value(&event).unwrap()),
                                    );
                                    let json = serde_json::to_string(&notif).unwrap();
                                    if out_tx_clone.send(json).await.is_err() {
                                        break;
                                    }
                                }
                            });
                            let res = RpcResponse::success(req_id, Value::Bool(true));
                            let _ = out_tx.send(serde_json::to_string(&res).unwrap()).await;
                        } else {
                            let response = self.dispatcher.dispatch(req).await;
                            let json = serde_json::to_string(&response).unwrap();
                            let _ = out_tx.send(json).await;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    }
}

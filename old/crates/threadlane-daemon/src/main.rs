//! `threadlane-daemon`: the standalone session-owning process.
//!
//! Wraps [`DaemonCore`] behind a WebSocket transport speaking
//! `threadlane-protocol::daemon`: clients send one `SessionCommand` JSON per
//! text frame and receive `SessionEvent` JSON frames — the journal tail on
//! attach, then live events.
//!
//! Configuration (environment):
//! - `THREADLANE_DAEMON_ADDR` — listen address, default `127.0.0.1:4747`.
//!   The daemon is localhost-only by design; put a real transport in front
//!   rather than binding a public interface.
//! - `THREADLANE_DAEMON_TOKEN` — optional bearer token; when set, every
//!   client must present `Authorization: Bearer <token>` at handshake.

use std::sync::Arc;

use tokio::net::TcpListener;

use threadlane_daemon::core::DaemonCore;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let addr = std::env::var("THREADLANE_DAEMON_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:4747".to_string());
    let token = std::env::var("THREADLANE_DAEMON_TOKEN").ok();
    if !addr.starts_with("127.0.0.1") && !addr.starts_with("localhost") && token.is_none() {
        // The command surface includes prompt submission and session
        // deletion; serving it unauthenticated on a routable interface is
        // remote code execution for anyone who can reach the port.
        eprintln!(
            "refusing to bind {addr}: non-loopback addresses require \
             THREADLANE_DAEMON_TOKEN"
        );
        std::process::exit(2);
    }

    let core = DaemonCore::new().expect("could not start daemon core");
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|error| panic!("could not bind {addr}: {error}"));
    tracing::info!(%addr, "threadlane-daemon listening");
    threadlane_daemon::server::serve(listener, core, token).await;
}

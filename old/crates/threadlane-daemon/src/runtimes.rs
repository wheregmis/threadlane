//! Daemon-side ownership of live session runtimes.
//!
//! `WorktreePrepared` travels the event stream as serializable
//! [`crate::types::SessionInfo`] data — a runtime handle cannot cross the
//! wire — so the freshly constructed runtime waits here, keyed by
//! `session_id`, until the consumer that processes the prepared event claims
//! it into its own `session_runtimes` map. Same in-process hand-off as
//! before; the event itself now stays wire-clean.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use threadlane_coding_agent::controller::SessionRuntime;

fn prepared() -> &'static Mutex<HashMap<String, Arc<SessionRuntime>>> {
    static PREPARED: OnceLock<Mutex<HashMap<String, Arc<SessionRuntime>>>> = OnceLock::new();
    PREPARED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parks a runtime only while setup remains active. Cancellation uses the same
/// registry lock, so a cancelled runtime can never be published afterward.
pub fn park_prepared_runtime_if_active(
    session_id: String,
    runtime: Arc<SessionRuntime>,
    cancelled: &std::sync::atomic::AtomicBool,
) -> bool {
    let Ok(mut map) = prepared().lock() else {
        return false;
    };
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    map.insert(session_id, runtime);
    true
}

/// Mark setup cancelled and remove any runtime it already published.
pub fn cancel_prepared_runtime(
    session_id: &str,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Option<Arc<SessionRuntime>> {
    let mut map = prepared().lock().ok()?;
    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    map.remove(session_id)
}

/// Claims the parked runtime for `session_id`, if one is still waiting.
pub fn take_prepared_runtime(session_id: &str) -> Option<Arc<SessionRuntime>> {
    prepared()
        .lock()
        .ok()
        .and_then(|mut map| map.remove(session_id))
}

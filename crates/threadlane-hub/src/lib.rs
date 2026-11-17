//! The hub process of one space (Plan.md §3.1, §3.3, docs/milestone-1.md §9).
//!
//! One process per hub, and it is the only process that opens the hub's
//! database. [`HubCore`] holds its roles: the handler and feed ([`HubCore::api`]),
//! placement, begin-run, the tool gateway, and run endings (the
//! [`HubLink`](threadlane_core::link::HubLink) from [`HubCore::link`]), the
//! dispatcher, and the lease sweep. [`Hub`] is the process around it: data
//! directory, lock, database, socket.

mod accounts;
mod begin_run;
pub mod config;
mod context;
mod core;
mod defaults;
mod dispatcher;
mod error;
mod finish;
mod gateway;
mod handler;
mod link;
mod memory;
mod node;
mod pairing;
mod placement;
mod publish;
mod recurrence;
mod settings;
mod timers;
mod tools;
mod wake;

use std::fs::{File, OpenOptions};

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::config::{DataDir, LogConfig, LogFormat};

pub use crate::begin_run::{ENDED_REASON, OFF_REASON, SKIP_REASON};
pub use crate::context::{LEAD_RULES, TASK_RULES, format_time};
pub use crate::core::HubCore;
pub use crate::defaults::{
    HUB_ENDPOINT, HUB_MACHINE_NAME, LEAD_INSTRUCTIONS, LEAD_NAME, LEAD_ROLE,
};
pub use crate::dispatcher::ClientChannel;
pub use crate::error::HubError;
pub use crate::handler::CLIENT_SOURCE;
pub use crate::memory::{MEMORY_CREATE, MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME, MEMORY_VIEW};
pub use crate::node::Hub;
pub use crate::pairing::JOIN_CODE_TTL;
pub use crate::settings::HubSettings;
pub use crate::timers::{CANCEL_TIMER, CREATE_SCHEDULE, LIST_TIMERS, SET_FOLLOW_UP};
pub use crate::tools::{
    CANCEL_TASK, CLIENT_CHANNEL, CREATE_TASK, CombinedTools, FINISH_TASK, HubTools, LIST_TASKS,
    SEND_MESSAGE, TierPolicy, UPDATE_TASK, derived_call_key, offered_in,
};
pub use threadlane_core::forge::{OPEN_PULL_REQUEST, PUBLISH_BRANCH};

/// Installs the global `tracing` subscriber. `RUST_LOG` overrides the
/// configured filter.
///
/// # Errors
///
/// Fails if the filter is invalid or a subscriber is already installed.
pub fn init_logging(config: &LogConfig) -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.filter))
        .context("invalid log filter")?;
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let result = match config.format {
        LogFormat::Pretty => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
    result.map_err(|error| anyhow::anyhow!("installing the log subscriber: {error}"))
}

/// Holds the data directory's lock file for the life of the hub, so a second
/// process cannot open the same database (Plan.md §18.1).
#[derive(Debug)]
pub struct DirLock {
    _file: File,
}

impl DirLock {
    /// Takes the lock without waiting.
    ///
    /// # Errors
    ///
    /// Fails if another process holds it or the file cannot be created.
    pub fn acquire(data_dir: &DataDir) -> anyhow::Result<Self> {
        let path = data_dir.lock();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.try_lock().map_err(|error| {
            anyhow::anyhow!(
                "another hub process is using {} ({error})",
                data_dir.root().display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

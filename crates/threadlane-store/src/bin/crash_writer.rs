//! Child process for the database crash gate (Plan.md §18.1).
//!
//! Usage: `threadlane-crash-writer <db-path> <first-sequence>`
//!
//! Opens the hub database and creates tasks forever, each in one transaction
//! that writes a lane, a task, and a start event. After each commit returns it
//! prints `committed <n>` and flushes, so the parent knows exactly which
//! transactions were reported committed when it kills this process.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use threadlane_core::model::{NewAgent, NewTask, TaskOrigin, TaskStatus};
use threadlane_core::store::{OrgStore, TaskStore};
use threadlane_core::time::{Clock, SystemClock};
use threadlane_store::{Bootstrap, HubDb};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("missing database path")?);
    let first: i64 = args.next().ok_or("missing first sequence")?.parse()?;

    let clock = Arc::new(SystemClock);
    let hub = HubDb::open(&path, &Bootstrap::default(), clock.clone()).await?;
    let general = hub.general_project().await?;
    let agent = match hub.agents().await?.into_iter().next() {
        Some(agent) => agent,
        None => {
            hub.create_agent(&NewAgent::named("gate", "Engineer"))
                .await?
        }
    };

    let mut out = std::io::stdout().lock();
    writeln!(out, "ready")?;
    out.flush()?;

    for n in first.. {
        hub.create_task(
            None,
            &NewTask {
                project_id: general.id,
                agent_id: agent.id,
                title: format!("gate {n}"),
                goal: "x".repeat(512),
                repositories: Vec::new(),
                parent_task_id: None,
                depends_on: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: Some(format!("gate-{n}")),
                created_at: clock.now(),
                playbooks: Vec::new(),
            },
        )
        .await?;
        writeln!(out, "committed {n}")?;
        out.flush()?;
    }
    Ok(())
}

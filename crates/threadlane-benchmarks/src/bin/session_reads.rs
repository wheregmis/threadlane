//! Measures reading an ordered run journal from the hub's in-memory store.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::ids::RunId;
use threadlane_core::model::{
    EventKind, JournalEntry, MachineTrust, NewEvent, NewMachine, SessionChoice, StartRun, Trust,
};
use threadlane_core::store::{InboxStore, MachineStore, OrgStore, SessionStore};
use threadlane_core::time::{SystemClock, Timestamp};
use threadlane_store::{Bootstrap, HubDb};

const SAMPLES: usize = 10;
const READS: usize = 10;
const ENTRIES: i64 = 1_000;
const NOW: Timestamp = Timestamp::from_millis(1_767_225_600_000);

#[hotpath::measure]
async fn read_journal(db: &HubDb, run_id: RunId) {
    for _ in 0..READS {
        black_box(db.journal(run_id).await.expect("read run journal"));
    }
}

#[hotpath::main(percentiles = [50, 95])]
fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("start benchmark runtime")
        .block_on(benchmark());
}

async fn benchmark() {
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .expect("open in-memory database");
    let project = db.general_project().await.expect("load General project");
    let machine = db
        .register_machine(&NewMachine {
            name: "benchmark".into(),
            endpoint_id: "benchmark-key".into(),
            is_hub: false,
            trust: MachineTrust::Trusted,
            max_parallel: 1,
        })
        .await
        .expect("register fixture machine");
    let lease = db
        .grant_lease(
            project.lead_lane_id,
            machine.id,
            NOW,
            Duration::from_secs(60),
        )
        .await
        .expect("grant fixture lease")
        .expect("lead lane is free");
    let inserted = db
        .insert_event(&NewEvent {
            project_id: project.id,
            lane_id: project.lead_lane_id,
            source: "benchmark".into(),
            source_event_id: "fixture".into(),
            kind: EventKind::UserMessage,
            payload: json!({"text": "Review project updates."}),
            trust: Trust::User,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: NOW,
        })
        .await
        .expect("insert fixture event");
    let run = db
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::New,
                agent_id: None,
                backend: None,
                model: None,
                native_session_id: None,
                events: vec![inserted.id()],
                started_at: NOW,
            },
        )
        .await
        .expect("start fixture run");
    let entries: Vec<_> = (1..=ENTRIES)
        .map(|seq| JournalEntry {
            run_id: run.id,
            seq,
            at: NOW,
            entry: json!({"type": "assistant", "text": "x".repeat(256)}),
        })
        .collect();
    db.append_journal(&lease, &entries)
        .await
        .expect("seed fixture journal");
    assert_eq!(
        db.journal(run.id).await.expect("verify fixture journal"),
        entries
    );

    for _ in 0..SAMPLES {
        read_journal(&db, run.id).await;
    }
}

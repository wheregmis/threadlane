use super::*;
use crate::Bootstrap;
use crate::db::exec;
use std::sync::Arc;
use threadlane_core::ids::LaneId;
use threadlane_core::model::{DecisionAnswer, TaskStatus};
use threadlane_core::store::{DecisionStore, TaskStore};
use threadlane_core::time::SystemClock;

async fn hub() -> HubDb {
    HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .unwrap()
}

#[tokio::test]
async fn searches_scoped_outputs_and_reads_only_the_selected_attachment() {
    let db = hub().await;
    let tx = db.db.begin().await.unwrap();
    tx.conn().execute_batch(r#"
       INSERT INTO projects (id,name,max_parallel_tasks,created_at) VALUES (2,'Other',1,0);
       INSERT INTO memory_files(path,version,content,written_at) VALUES
         ('projects/1/notes/reports/weekly.md',1,'Test results',20),
         ('projects/2/notes/reports/private.md',1,'Other report',20);
       INSERT INTO events(project_id,lane_id,source,source_event_id,kind,payload,trust,requested,received_at)
         VALUES(1,1,'client','capture','user_message','{"images":[{"name":"capture.png","data":"AQID"}]}','user',1,10);
       INSERT INTO decisions(project_id,lane_id,tool,arguments,arguments_hash,kind,question,options,status,created_at,outcome)
         VALUES(1,1,'open_pull_request','{}','','input','?','[]','answered',30,'{"performed":true,"pull_request":"https://github.com/example/app/pull/9"}');
    "#).await.unwrap();
    tx.commit().await.unwrap();
    let rows = db.project_outputs(ProjectId(1), "").await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[0].source,OutputSource::PullRequest(url) if url.ends_with("/9")));
    assert!(
        matches!(&rows[1].source,OutputSource::Report(path) if path=="project/notes/reports/weekly.md")
    );
    assert_eq!(
        db.project_outputs(ProjectId(1), "WEEKLY").await.unwrap(),
        rows[1..2]
    );
    let OutputSource::Attachment { event, index } = rows[2].source else {
        panic!("image")
    };
    assert_eq!(
        db.output_image(ProjectId(1), event, index)
            .await
            .unwrap()
            .unwrap()
            .bytes(),
        [1, 2, 3]
    );
    assert!(
        db.output_image(ProjectId(2), event, index)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_later_success_preserves_the_failed_occurrences_result() {
    let db = hub().await;
    let tx = db.db.begin().await.unwrap();
    tx.conn().execute_batch(r#"
        INSERT INTO sessions(id,lane_id,status,created_at,last_active_at) VALUES(1,1,'open',0,0);
        INSERT INTO runs(id,lane_id,session_id,machine_id,lease_epoch,status,started_at,ended_at,error,rationale)
          VALUES(1,1,1,1,1,'failed',11,12,'Runner unavailable',NULL),(2,1,1,1,2,'committed',21,22,NULL,'Checked');
        INSERT INTO events(id,project_id,lane_id,source,source_event_id,kind,payload,trust,requested,received_at,status)
          VALUES(1,1,1,'timer','7:10','timer_fired','{"timer_id":7,"instruction":"Check","occurrence":10}','system',1,10,'dead'),
                (2,1,1,'timer','7:20','timer_fired','{"timer_id":7,"instruction":"Check","occurrence":20}','system',1,20,'done');
        INSERT INTO run_events(run_id,event_id) VALUES(1,1),(2,2);
    "#).await.unwrap();
    tx.commit().await.unwrap();
    let all = db.routine_history(Some(TimerId(7)), None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].result.as_deref(), Some("Checked"));
    let older = db
        .routine_history(Some(TimerId(7)), Some(all[0].event))
        .await
        .unwrap();
    assert_eq!(older, all[1..]);
    assert_eq!(older[0].result.as_deref(), Some("Runner unavailable"));
    assert_eq!(older[0].started, Some(Timestamp::from_millis(11)));
}

#[tokio::test]
async fn usage_includes_failed_runs_and_counts_only_the_latest_report_once() {
    let db = hub().await;
    let tx = db.db.begin().await.unwrap();
    tx.conn().execute_batch(r#"
       INSERT INTO sessions (id,lane_id,status,created_at,last_active_at) VALUES(1,1,'open',0,0);
       INSERT INTO runs(id,lane_id,session_id,machine_id,lease_epoch,backend,status,started_at,ended_at)
         VALUES(1,1,1,1,1,'claude','committed',10,20),(2,1,1,1,1,'claude','failed',20,30);
       INSERT INTO journal(run_id,seq,at,entry) VALUES
         (1,1,15,'{"type":"finished","usage":{"usage":{"input_tokens":100}}}'),
         (1,2,20,'{"type":"finished","usage":{"usage":{"input_tokens":12,"output_tokens":3},"total_cost_usd":0.25}}');
    "#).await.unwrap();
    tx.commit().await.unwrap();
    let g = db
        .space_usage(Timestamp::EPOCH, Timestamp::from_millis(30))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        (
            g.runs,
            g.duration_ms,
            g.tokens,
            g.token_reports,
            g.cost_reports
        ),
        (2, 20, 15, 1, 1)
    );
    assert_eq!(g.cost, 0.25);
    assert!(g.tasks.is_empty());
}

/// Runs `(id, session, backend, started_at, usage)` on lane 1, one session per backend, where
/// `usage` is the finished entry's `usage` JSON or `None` for a run that reported nothing.
async fn usage_db(runs: &[(i64, i64, &str, i64, Option<&str>)]) -> HubDb {
    let db = hub().await;
    let tx = db.db.begin().await.unwrap();
    for (id, session, backend, started, usage) in runs {
        exec(
            tx.conn(),
            "INSERT OR IGNORE INTO sessions(id,lane_id,status,created_at,last_active_at)
             VALUES(?1,1,'closed',0,0)",
            params![*session],
        )
        .await
        .unwrap();
        exec(
            tx.conn(),
            "INSERT INTO runs(id,lane_id,session_id,machine_id,lease_epoch,backend,status,started_at,ended_at)
             VALUES(?1,1,?2,1,1,?3,'committed',?4,?4+1)",
            params![*id, *session, *backend, *started],
        )
        .await
        .unwrap();
        if let Some(usage) = usage {
            exec(
                tx.conn(),
                "INSERT INTO journal(run_id,seq,at,entry)
                 VALUES(?1,1,?2,json_object('type','finished','usage',json(?3)))",
                params![*id, *started, *usage],
            )
            .await
            .unwrap();
        }
    }
    tx.commit().await.unwrap();
    db
}

async fn usage_since(db: &HubDb, since: i64) -> UsageGroup {
    let mut groups = db
        .space_usage(Timestamp::from_millis(since), Timestamp::from_millis(100))
        .await
        .unwrap();
    assert_eq!(groups.len(), 1);
    groups.pop().unwrap()
}

fn claude(cost: f64, input: u64) -> String {
    json!({"total_cost_usd": cost, "usage": {"input_tokens": input, "output_tokens": 1,
        "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10}})
    .to_string()
}

fn codex(input: u64, output: u64) -> String {
    json!({"usage": {"input_tokens": input, "cached_input_tokens": input / 2, "output_tokens": output}})
        .to_string()
}

#[tokio::test]
async fn usage_counts_a_resumed_claude_sessions_cost_once() {
    let (a, b, c) = (claude(0.5, 1), claude(0.75, 2), claude(1.0, 3));
    let db = usage_db(&[
        (1, 1, "claude", 10, Some(&a)),
        (2, 1, "claude", 20, Some(&b)),
        (3, 1, "claude", 30, Some(&c)),
    ])
    .await;
    let g = usage_since(&db, 0).await;
    assert_eq!(g.cost, 1.0);
    // Claude's tokens are per run, with cache reads and writes: 3 * 111 + (1 + 2 + 3).
    assert_eq!((g.tokens, g.token_reports, g.cost_reports), (339, 3, 3));
    let per_run = db.lane_run_usage(LaneId(1)).await.unwrap();
    assert_eq!(per_run[&RunId(2)].input_tokens, Some(112));
    assert_eq!(per_run[&RunId(2)].output_tokens, Some(1));
    assert_eq!(per_run[&RunId(2)].cost_usd, Some(0.25));
}

#[tokio::test]
async fn usage_counts_a_resumed_codex_sessions_tokens_once() {
    let (a, b, c) = (codex(100, 10), codex(250, 25), codex(400, 40));
    let db = usage_db(&[
        (1, 1, "codex", 10, Some(&a)),
        (2, 1, "codex", 20, Some(&b)),
        (3, 1, "codex", 30, Some(&c)),
    ])
    .await;
    let g = usage_since(&db, 0).await;
    assert_eq!((g.tokens, g.token_reports, g.cost_reports), (440, 3, 0));
    let per_run = db.lane_run_usage(LaneId(1)).await.unwrap();
    assert_eq!(
        (
            per_run[&RunId(3)].input_tokens,
            per_run[&RunId(3)].output_tokens
        ),
        (Some(150), Some(15))
    );
}

#[tokio::test]
async fn usage_takes_a_total_that_restarted_lower_as_is() {
    let (a, b, c, d) = (codex(100, 0), codex(300, 0), codex(50, 0), codex(120, 0));
    let db = usage_db(&[
        (1, 1, "codex", 10, Some(&a)),
        (2, 1, "codex", 20, Some(&b)),
        (3, 1, "codex", 30, Some(&c)),
        (4, 1, "codex", 40, Some(&d)),
    ])
    .await;
    assert_eq!(usage_since(&db, 0).await.tokens, 100 + 200 + 50 + 70);
}

#[tokio::test]
async fn usage_differences_across_a_run_that_reported_nothing() {
    let (a, c) = (claude(0.5, 0), claude(0.8, 0));
    let db = usage_db(&[
        (1, 1, "claude", 10, Some(&a)),
        (2, 1, "claude", 20, None),
        (3, 1, "claude", 30, Some("{}")),
        (4, 1, "claude", 40, Some(&c)),
        // Another session's total is its own.
        (5, 2, "claude", 50, Some(&a)),
    ])
    .await;
    let g = usage_since(&db, 0).await;
    assert_eq!((g.runs, g.cost_reports), (5, 3));
    assert!((g.cost - 1.3).abs() < 1e-9, "{}", g.cost);
    let per_run = db.lane_run_usage(LaneId(1)).await.unwrap();
    assert!(!per_run.contains_key(&RunId(2)) && !per_run.contains_key(&RunId(3)));
    assert!((per_run[&RunId(4)].cost_usd.unwrap() - 0.3).abs() < 1e-9);
}

#[tokio::test]
async fn usage_window_starting_mid_session_subtracts_the_earlier_total() {
    let (a, b) = (claude(0.5, 0), claude(0.8, 0));
    let (c, d) = (codex(100, 10), codex(250, 25));
    let db = usage_db(&[
        (1, 1, "claude", 10, Some(&a)),
        (2, 1, "claude", 30, Some(&b)),
        (3, 2, "codex", 10, Some(&c)),
        (4, 2, "codex", 30, Some(&d)),
    ])
    .await;
    let groups = db
        .space_usage(Timestamp::from_millis(20), Timestamp::from_millis(100))
        .await
        .unwrap();
    let [claude, codex] = groups.as_slice() else {
        panic!("{groups:?}")
    };
    assert_eq!((claude.runs, claude.cost_reports), (1, 1));
    assert!((claude.cost - 0.3).abs() < 1e-9, "{}", claude.cost);
    assert_eq!((codex.runs, codex.tokens), (1, 165));
}

#[tokio::test]
async fn owner_can_request_changes_then_accept_the_revised_result() {
    let db = hub().await;
    let tx = db.db.begin().await.unwrap();
    tx.conn().execute_batch(r#"
       INSERT INTO agents(id,name,role) VALUES (99,'Reviewer','reviewer');
       INSERT INTO lanes(id,project_id,kind,task_id) VALUES(2,1,'task',1);
       INSERT INTO tasks(id,project_id,lane_id,agent_id,title,goal,origin,status,created_by_call,result,created_at)
        VALUES(1,1,2,99,'Review loading','Check the changes','user','running','workflow-review:1:test','{"summary":"First"}',0);
       INSERT INTO sessions(id,lane_id,status,created_at,last_active_at) VALUES(1,2,'open',0,0);
       INSERT INTO runs(id,lane_id,session_id,machine_id,lease_epoch,status,started_at,ended_at)
        VALUES(1,2,1,1,1,'committed',0,5),(2,2,1,1,2,'committed',6,10),(3,2,1,1,3,'committed',11,15);
    "#).await.unwrap();
    tx.commit().await.unwrap();
    for (run, answer, summary) in [(1, 1, "First"), (2, 0, "Revised")] {
        let tx = db.db.begin().await.unwrap();
        exec(
            tx.conn(),
            "UPDATE tasks SET status='running',result=?1 WHERE id=1",
            params![json!({"summary":summary}).to_string()],
        )
        .await
        .unwrap();
        crate::task::on_run_committed(tx.conn(), LaneId(2), RunId(run), db.now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            db.task(TaskId(1)).await.unwrap().unwrap().status,
            TaskStatus::WaitingOnYou
        );
        let decisions = db.pending_decisions(LaneId(2)).await.unwrap();
        assert_eq!(decisions.len(), 1);
        db.answer(
            decisions[0].id,
            &DecisionAnswer {
                option: answer,
                note: None,
                standing_rule: false,
                answered_at: db.now(),
            },
        )
        .await
        .unwrap();
        if answer == 1 {
            assert!(db.task(TaskId(1)).await.unwrap().unwrap().result.is_none());
        }
    }
    let tx = db.db.begin().await.unwrap();
    exec(
        tx.conn(),
        "UPDATE tasks SET status='running' WHERE id=1",
        (),
    )
    .await
    .unwrap();
    crate::task::on_run_committed(tx.conn(), LaneId(2), RunId(3), db.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        db.task(TaskId(1)).await.unwrap().unwrap().status,
        TaskStatus::Done
    );
}

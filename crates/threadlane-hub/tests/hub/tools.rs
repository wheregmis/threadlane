//! The tool gateway (Plan.md §4.6, §9.1, invariants 5 and 6).

use std::sync::Arc;

use serde_json::json;
use threadlane_core::backend::NativeSession;
use threadlane_core::link::Begin;
use threadlane_core::model::RunOutcome;
use threadlane_core::store::OutboxStore;
use threadlane_core::time::Clock;
use threadlane_core::tools::{Tier, ToolDefinition, ToolError, ToolRegistry};
use threadlane_hub::{
    CANCEL_TASK, CREATE_TASK, CombinedTools, LIST_TASKS, SEND_MESSAGE, UPDATE_TASK,
};
use threadlane_testkit::fakes::FakeTools;

use crate::common::{PAST_LEASE, TestHub, begin};

async fn outbox_rows(hub: &TestHub) -> usize {
    hub.db
        .deliverable(hub.clock.now(), 100)
        .await
        .unwrap()
        .len()
        + hub.feed().await.len()
}

#[tokio::test]
async fn the_same_call_id_twice_performs_once() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let (lease, run) = begin(&*hub.link).await;
    let arguments = json!({ "text": "Hello!" });
    let first = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &arguments,
        )
        .await
        .unwrap();
    let second = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &arguments,
        )
        .await
        .unwrap();
    assert_eq!(first, second);
    assert!(first.get("outbox_id").is_some());
    assert_eq!(outbox_rows(&hub).await, 1);
    assert_eq!(
        hub.link.complete(&lease, run.run_id, None).await.unwrap(),
        RunOutcome::Sent
    );
}

#[tokio::test]
async fn a_resumed_session_repeating_a_call_after_a_crash_sends_nothing_new() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let arguments = json!({ "text": "Hello!" });
    let (first_lease, first) = begin(&*hub.link).await;
    let sent = hub
        .link
        .call_tool(
            &first_lease,
            first.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &arguments,
        )
        .await
        .unwrap();

    // The runner dies; its lease runs out; the lane is claimed again.
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    let (lease, second) = begin(&*hub.link).await;
    assert_ne!(second.run_id, first.run_id);
    assert_eq!(second.session_id, first.session_id);
    assert!(matches!(
        second.native_session,
        NativeSession::Resume { .. }
    ));

    let again = hub
        .link
        .call_tool(
            &lease,
            second.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &arguments,
        )
        .await
        .unwrap();
    assert_eq!(again, sent, "the original row is returned");
    hub.link
        .complete(&lease, second.run_id, None)
        .await
        .unwrap();
    hub.link.release(&lease).await.unwrap();
    assert_eq!(outbox_rows(&hub).await, 1);
    hub.core.dispatch_once().await.unwrap();
    assert_eq!(hub.feed().await.len(), 1);
}

#[tokio::test]
async fn an_unknown_tool_is_refused() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let (lease, run) = begin(&*hub.link).await;
    let result = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_1"),
            "launch_rockets",
            &json!({}),
        )
        .await;
    assert_eq!(result, Err(ToolError::UnknownTool("launch_rockets".into())));
}

#[tokio::test]
async fn bad_arguments_are_refused_and_the_refusal_is_recorded() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let (lease, run) = begin(&*hub.link).await;
    for _ in 0..2 {
        let result = hub
            .link
            .call_tool(
                &lease,
                run.run_id,
                Some("toolu_1"),
                SEND_MESSAGE,
                &json!({ "text": "" }),
            )
            .await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
    }
    assert_eq!(outbox_rows(&hub).await, 0);
}

fn tool(name: &str, tier: Tier) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("test tool {name}"),
        input_schema: json!({ "type": "object" }),
        tier,
        safe_to_repeat: true,
    }
}

#[tokio::test]
async fn tools_above_tier_one_are_blocked() {
    let base = TestHub::start().await;
    let fake = FakeTools::new();
    fake.register(tool("read_notes", Tier::Read), Ok(json!({ "notes": [] })));
    fake.register(
        tool("merge_pull_request", Tier::SharedReversible),
        Ok(json!({ "number": 7 })),
    );
    let tools = CombinedTools::new(vec![base.core.tools(), Arc::new(fake.clone())]);
    let core = base.core.clone().with_tools(Arc::new(tools));
    let hub = TestHub::around(core, base.clock.clone()).await;
    hub.hello().await;
    hub.send("m-1", "merge the PR").await;
    let (lease, run) = begin(&*hub.link).await;
    let names: Vec<&str> = run.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            SEND_MESSAGE,
            CREATE_TASK,
            UPDATE_TASK,
            CANCEL_TASK,
            LIST_TASKS,
            "read_notes",
            "merge_pull_request"
        ]
    );

    let blocked = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_1"),
            "merge_pull_request",
            &json!({}),
        )
        .await;
    assert!(matches!(blocked, Err(ToolError::Blocked(_))), "{blocked:?}");
    assert!(fake.calls().is_empty(), "a blocked call is never executed");

    let allowed = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_2"),
            "read_notes",
            &json!({}),
        )
        .await;
    assert_eq!(allowed, Ok(json!({ "notes": [] })));
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(fake.calls()[0].call_id.as_deref(), Some("toolu_2"));
    assert_eq!(
        ToolRegistry::definition(&*hub.core.tools(), "merge_pull_request").map(|d| d.tier),
        Some(Tier::SharedReversible)
    );
}

#[tokio::test]
async fn a_derived_key_dedupes_within_a_run_but_a_later_run_sends_again() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let arguments = json!({ "text": "Same words." });

    hub.send("m-1", "first").await;
    let (lease, run) = begin(&*hub.link).await;
    let first = hub
        .link
        .call_tool(&lease, run.run_id, None, SEND_MESSAGE, &arguments)
        .await
        .unwrap();
    let repeat = hub
        .link
        .call_tool(&lease, run.run_id, None, SEND_MESSAGE, &arguments)
        .await
        .unwrap();
    assert_eq!(first, repeat);
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    assert_eq!(outbox_rows(&hub).await, 1);

    hub.send("m-2", "second").await;
    let (lease, later) = begin(&*hub.link).await;
    assert_eq!(later.session_id, run.session_id, "the same session");
    let second = hub
        .link
        .call_tool(&lease, later.run_id, None, SEND_MESSAGE, &arguments)
        .await
        .unwrap();
    assert_ne!(second, first, "a new run sends again");
    hub.link.complete(&lease, later.run_id, None).await.unwrap();
    hub.link.release(&lease).await.unwrap();
    assert_eq!(outbox_rows(&hub).await, 2);
}

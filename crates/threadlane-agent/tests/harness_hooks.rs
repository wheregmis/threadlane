use std::sync::Arc;
use threadlane_agent::harness::{AgentHarness, HookContext, HookKind, HookRegistry, MemoryStore};

#[test]
fn hooks_run_in_registration_order_and_before_tool_fails_closed() {
    let mut hooks = HookRegistry::default();
    hooks
        .register(
            HookKind::BeforeTool,
            "first",
            Arc::new(|_| Err("blocked".into())),
        )
        .unwrap();
    hooks
        .register(
            HookKind::BeforeTool,
            "second",
            Arc::new(|_| Err("also blocked".into())),
        )
        .unwrap();
    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("r".into()),
        resume_data: None,
    };
    let failures = hooks.run_before_tool(&context).unwrap_err();
    assert_eq!(
        failures
            .iter()
            .map(|failure| failure.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn resume_data_is_scoped_to_the_matching_stable_hook_id() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::default();
    let first_seen = seen.clone();
    hooks
        .register(
            HookKind::BeforeResume,
            "first",
            Arc::new(move |context| {
                first_seen.lock().unwrap().push(context.resume_data.clone());
                Ok(())
            }),
        )
        .unwrap();
    let second_seen = seen.clone();
    hooks
        .register(
            HookKind::BeforeResume,
            "second",
            Arc::new(move |context| {
                second_seen
                    .lock()
                    .unwrap()
                    .push(context.resume_data.clone());
                Ok(())
            }),
        )
        .unwrap();
    hooks.set_resume_data("second", "checkpoint-2").unwrap();

    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("r".into()),
        resume_data: None,
    };
    assert!(hooks.run_before_resume(&context).is_empty());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some("checkpoint-2".into())]
    );
}

#[test]
fn resume_data_round_trips_through_the_durable_harness() {
    let mut harness = AgentHarness::new(MemoryStore::new("s"));
    harness
        .set_hook_resume_data("main", "checkpoint", "saved", Some("run-1".into()))
        .unwrap();
    harness.drive_to_completion().unwrap();

    let seen = Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    harness
        .hooks_mut()
        .register(
            HookKind::BeforeResume,
            "checkpoint",
            Arc::new(move |context| {
                *captured.lock().unwrap() = context.resume_data.clone();
                Ok(())
            }),
        )
        .unwrap();
    harness.restore_hooks_for_lane("main").unwrap();
    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("run-1".into()),
        resume_data: None,
    };
    assert!(harness.hooks().run_before_resume(&context).is_empty());
    assert_eq!(*seen.lock().unwrap(), Some("saved".into()));
}

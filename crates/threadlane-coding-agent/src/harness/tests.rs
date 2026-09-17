    use super::*;
    fn temp_session() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        (dir, path)
    }

    fn open_long_run(path: &Path) -> CodingSessionHarness {
        let mut harness = CodingSessionHarness::open(path).unwrap();
        harness
            .begin_run("run-compact", AgentMessage::user("start", vec![]))
            .unwrap();
        for index in 0..28 {
            harness
                .append_message(AgentMessage::user(
                    format!("history-{index}-{}", "x".repeat(16_000)),
                    vec![],
                ))
                .unwrap();
        }
        harness
    }

    #[test]
    fn recover_abort_terminates_suspended_foreground_run_without_prior_cancel() {
        let (_dir, path) = temp_session();
        {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            harness
                .begin_run("interrupted-run", AgentMessage::user("first", vec![]))
                .unwrap();
        }

        let mut reopened = CodingSessionHarness::open(&path).unwrap();
        assert!(reopened.recover_abort().unwrap());

        let state = Reducer::reduce(&reopened.store).unwrap();
        let main = state.lane("main").unwrap();
        assert!(main.open_operation.is_none());
        assert!(reopened.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::OperationFinished {
                    run_id,
                    outcome: OperationOutcome::Aborted,
                    ..
                } if run_id == "interrupted-run"
            )
        }));

        reopened
            .begin_run("next-run", AgentMessage::user("continue", vec![]))
            .unwrap();
    }

    fn boundary_request(overflow_recovery: bool) -> ProviderBoundaryRequest {
        ProviderBoundaryRequest {
            attempt: 1,
            model: "unknown/test-model".into(),
            messages: Vec::new(),
            tool_schema_json: None,
            overflow_recovery,
        }
    }

    #[test]
    fn provider_boundary_retains_and_budgets_current_system_after_reload() {
        let config = AgentConfig::default();
        for overflow_recovery in [false, true] {
            let (_dir, path) = temp_session();
            let mut harness = open_long_run(&path);
            let system = AgentMessage::System {
                content: format!(
                    "current instructions {overflow_recovery} {}",
                    "rule ".repeat(1000)
                ),
            };
            let mut request = boundary_request(overflow_recovery);
            request.messages = vec![
                system.clone(),
                AgentMessage::user("stale runtime history", vec![]),
            ];
            let prepared = harness
                .prepare_provider_boundary("run-compact", request, &config)
                .unwrap();
            assert_eq!(prepared.messages.first(), Some(&system));
            assert!(!prepared.messages.iter().any(|message| {
                matches!(message, AgentMessage::User { content } if content == "stale runtime history")
            }));
            let actual = estimate_request_tokens(
                &prepared.messages,
                None,
                &CompactionParams::from(&config),
            );
            assert!(actual < prepared.context_limit);
            assert_eq!(prepared.provisional_estimated_tokens, Some(actual));
            drop(harness);
            harness = CodingSessionHarness::open(&path).unwrap();
            assert!(!harness
                .model_context("main")
                .unwrap()
                .messages()
                .iter()
                .any(|message| { matches!(message, AgentMessage::System { .. }) }));
            let updated_system = AgentMessage::System {
                content: "updated instructions after restart".into(),
            };
            let mut request = boundary_request(false);
            request.messages = vec![updated_system.clone()];
            let resumed = harness
                .prepare_provider_boundary("run-compact", request, &config)
                .unwrap();
            assert_eq!(resumed.messages.first(), Some(&updated_system));
            assert!(!resumed.messages.contains(&system));
        }
    }

    #[test]
    fn provider_identity_survives_reopen_of_same_open_run() {
        let (_dir, path) = temp_session();
        let accepted = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let accepted = harness
                .begin_run("restart-run", AgentMessage::user("hello", vec![]))
                .unwrap();
            let first = harness
                .prepare_provider_boundary(
                    "restart-run",
                    boundary_request(false),
                    &AgentConfig::default(),
                )
                .unwrap();
            let attempt = first.provider_attempt.unwrap();
            let request_id = first.provider_request_id.unwrap();
            harness
                .record_provider_trace(
                    "restart-run",
                    ProviderTraceEvent::Started {
                        attempt,
                        request_id,
                        model: "unknown/test-model".into(),
                        provider: "fake".into(),
                    },
                )
                .unwrap();
            accepted
        };

        let mut resumed = CodingSessionHarness::open(&path).unwrap();
        resumed.validate_accepted_run(&accepted).unwrap();
        let second = resumed
            .prepare_provider_boundary(
                "restart-run",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        assert_eq!(second.provider_attempt, Some(2));
        let second_request_id = second.provider_request_id.clone().unwrap();
        resumed
            .record_provider_trace(
                "restart-run",
                ProviderTraceEvent::Started {
                    attempt: second.provider_attempt.unwrap(),
                    request_id: second_request_id,
                    model: "unknown/test-model".into(),
                    provider: "fake".into(),
                },
            )
            .unwrap();

        let starts: Vec<_> = resumed
            .store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    id,
                    attempt,
                    request_id: Some(request_id),
                    ..
                } => Some((id, *attempt, request_id.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!((starts[0].1, starts[1].1), (1, 2));
        assert_ne!(starts[0].0, starts[1].0);
        assert_ne!(starts[0].2, starts[1].2);
        assert!(Reducer::reduce(resumed.store.store()).is_ok());
    }
    #[test]
    fn adaptive_compaction_commits_before_next_provider_attempt() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        harness
            .record_provider_trace(
                "run-compact",
                ProviderTraceEvent::Started {
                    attempt: 1,
                    request_id: "request-after-compaction".into(),
                    model: "unknown/test-model".into(),
                    provider: "fake".into(),
                },
            )
            .unwrap();
        let compacted_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    seq,
                    reason: CompactionReason::AdaptiveBudget,
                    ..
                } => Some(*seq),
                _ => None,
            })
            .expect("adaptive compaction");
        let start_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ProviderRequestStarted { seq, .. } if *seq > compacted_seq => {
                    Some(*seq)
                }
                _ => None,
            })
            .expect("provider start after compaction");
        assert!(compacted_seq < start_seq);
        assert_eq!(
            Reducer::reduce(&harness.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact"),
            "provider-boundary compaction must preserve the foreground run"
        );
    }

    #[test]
    fn reload_uses_checkpoint_tail_but_transcript_keeps_original_entries() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        drop(harness);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let context = reloaded.model_context("main").unwrap();
        assert!(context.checkpoint.is_some());
        assert!(reloaded.transcript("main").entries.len() > context.entries.len());
    }

    #[cfg(unix)]
    #[test]
    fn compaction_persistence_failure_appends_no_checkpoint_prefix() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        let original = fs::metadata(&path).unwrap().permissions();
        let canonical = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let result = harness.prepare_provider_boundary(
            "run-compact",
            boundary_request(false),
            &AgentConfig::default(),
        );
        fs::set_permissions(&path, original).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), canonical);
        assert!(!harness
            .store
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::ProviderRequestStarted { .. })));
    }

    #[test]
    fn ineffective_compaction_retries_once() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run(
                "run-compact",
                AgentMessage::user("x".repeat(50_000), vec![]),
            )
            .unwrap();
        harness
            .append_message(AgentMessage::user("y".repeat(50_000), vec![]))
            .unwrap();
        // The oversized accepted tail survives the normal checkpoint, so the
        // strict pass is attempted and deterministically cannot drop further.
        harness
            .append_message(AgentMessage::user("z".repeat(500_000), vec![]))
            .unwrap();
        let result = harness.prepare_provider_boundary(
            "run-compact",
            boundary_request(false),
            &AgentConfig::default(),
        );
        let error = result.expect_err("strict compaction must remain over budget");
        assert_eq!(
            error,
            "context preparation could not drop historical messages"
        );
        let attempts = harness
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    generation,
                    reason: CompactionReason::AdaptiveBudget,
                    run_id,
                    ..
                } => Some((*generation, run_id.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        // The normal attempt commits generation 1. The exact terminal error
        // proves the second (strict) attempt ran and found no further droppable
        // history; there is no recursive third checkpoint.
        assert_eq!(attempts, vec![(1, "run-compact")]);
    }

    #[test]
    fn provider_overflow_retries_once() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(true),
                &AgentConfig::default(),
            )
            .unwrap();
        let recoveries = harness
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    generation,
                    reason: CompactionReason::OverflowRecovery,
                    run_id,
                    pre_tokens,
                    post_tokens,
                    ..
                } => Some((*generation, run_id.as_str(), *pre_tokens, *post_tokens)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].0, 1);
        assert_eq!(recoveries[0].1, "run-compact");
        assert!(recoveries[0].2 > recoveries[0].3);
        assert_eq!(
            Reducer::reduce(&harness.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact")
        );
    }

    #[test]
    fn retained_tail_appends_equal_occurrences_without_value_deduplication() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-compact", AgentMessage::user("old", vec![]))
            .unwrap();
        let repeated = AgentMessage::user("repeat", vec![]);
        let summary_text = concat!(
            "before\n",
            "<!-- threadlane:context-snapshots:index:begin -->\n",
            "## Available context snapshots\n",
            "- user-authored marker-looking content\n",
            "<!-- threadlane:context-snapshots:index:end -->\n",
            "after",
        );
        let summary = AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({ "summary": summary_text }),
        };
        harness
            .commit_prepared_compaction(
                "run-compact",
                "unknown/test-model",
                None,
                &AgentConfig::default(),
                context_budget("unknown/test-model", &BudgetConfig::from(&AgentConfig::default())),
                CompactionReason::AdaptiveBudget,
                PreparedCompaction {
                    messages: vec![summary, repeated.clone(), repeated.clone()],
                    pre_tokens: 100,
                    post_tokens: 20,
                    compacted_messages: 1,
                    retained_tail_target: 12,
                    retained_tail_tokens: 10,
                },
            )
            .unwrap();

        let context = harness.model_context("main").unwrap().messages();
        assert_eq!(context.len(), 3);
        assert_eq!(
            threadlane_compaction::compaction_summary_text(&context[0]),
            Some(summary_text)
        );
        assert_eq!(context[1], repeated);
        assert_eq!(context[2], repeated);
        let compacted = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    retained_tail_target,
                    retained_tail_tokens,
                    ..
                } => Some((*retained_tail_target, *retained_tail_tokens)),
                _ => None,
            })
            .expect("compaction telemetry");
        assert_eq!(compacted, (12, 10));
        let tail_entries = harness
            .store
            .entries()
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>();
        assert_ne!(tail_entries[0].id, tail_entries[1].id);
        assert_eq!(tail_entries[0].message, tail_entries[1].message);
    }

    #[test]
    fn manual_compaction_telemetry_reports_true_pre_post_and_removed_count() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let config = AgentConfig::default();
        harness
            .begin_run(
                "run",
                AgentMessage::user(format!("old-{}", "x".repeat(4_000)), vec![]),
            )
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("discarded".repeat(500)),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        let before = harness.model_context("main").unwrap().messages();
        let pre_tokens =
            estimate_request_tokens(&before, None, &CompactionParams::from(&config));
        let compacted_messages = 2;
        harness
            .checkpoint_open_run_compaction("run", "durable summary", CompactionReason::Manual)
            .unwrap();
        let tail = AgentMessage::user("tail", vec![]);
        let retained_tail_tokens =
            estimate_request_tokens(
                std::slice::from_ref(&tail),
                None,
                &CompactionParams::from(&config),
            );
        harness.append_message_occurrence(tail).unwrap();
        let expected_post = estimate_request_tokens(
            &harness.model_context("main").unwrap().messages(),
            None,
            &CompactionParams::from(&config),
        );
        harness
            .record_manual_compaction(
                "run",
                "unknown/test-model",
                &config,
                pre_tokens,
                retained_tail_tokens,
                compacted_messages,
            )
            .unwrap();

        let telemetry = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    reason: CompactionReason::Manual,
                    pre_tokens,
                    post_tokens,
                    retained_tail_tokens,
                    compacted_messages,
                    ..
                } => Some((
                    *pre_tokens,
                    *post_tokens,
                    *retained_tail_tokens,
                    *compacted_messages,
                )),
                _ => None,
            })
            .expect("manual compaction telemetry");
        assert_eq!(
            telemetry,
            (
                pre_tokens,
                expected_post,
                retained_tail_tokens,
                compacted_messages,
            )
        );
        assert!(telemetry.1 < telemetry.0);
    }

    #[test]
    fn cancellation_before_compaction_has_no_partial_operation_or_provider_start() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness.request_abort().unwrap();
        let abort_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::AbortRequested { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("abort record");
        assert!(harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .is_err());
        assert!(!harness.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ContextCompacted { seq, .. } if *seq > abort_seq)
                || matches!(record, HarnessRecord::ProviderRequestStarted { seq, .. } if *seq > abort_seq)
        }));
    }

    #[test]
    fn cancellation_after_accepted_checkpoint_keeps_complete_canonical_state() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        let compacted = harness.model_context("main").unwrap();
        let checkpoint = compacted.checkpoint.clone().expect("accepted checkpoint");
        assert!(
            compacted.messages().len() > 1,
            "retained tail was committed"
        );
        let compacted_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("completed compaction telemetry");

        harness.request_abort().unwrap();
        let error = harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .expect_err("accepted cancellation blocks subsequent provider preparation");
        assert_eq!(error, "context preparation cancelled");
        assert!(!harness.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ContextCompacted { seq, .. } if *seq > compacted_seq)
                || matches!(record, HarnessRecord::ProviderRequestStarted { .. })
        }));

        drop(harness);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let reloaded_context = reloaded.model_context("main").unwrap();
        assert_eq!(reloaded_context.checkpoint, Some(checkpoint));
        let expected_json = serde_json::to_vec(&reloaded_context.messages()).unwrap();
        drop(reloaded);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let second_json =
            serde_json::to_vec(&reloaded.model_context("main").unwrap().messages()).unwrap();
        assert_eq!(
            (second_json.len(), Sha256::digest(&second_json)),
            (expected_json.len(), Sha256::digest(&expected_json))
        );
        assert_eq!(
            Reducer::reduce(&reloaded.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact")
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn path_scoped_async_helper_uses_blocking_worker() {
        let (_dir, path) = temp_session();
        let caller = std::thread::current().id();
        let result = AgentToolResult::external("missing", "read_file", "result", false);

        let _ = CodingSessionHarness::record_tool_result_to_path(&path, "run", &result).await;

        assert_ne!(last_path_operation_thread().unwrap(), caller);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn context_snapshot_load_path_helper_uses_blocking_worker() {
        let (_dir, path) = temp_session();
        let caller = std::thread::current().id();

        CodingSessionHarness::record_context_snapshot_load_to_path(
            &path,
            "ctx-1",
            "main",
            None,
            ContextSnapshotLoadOutcome::Missing,
        )
        .await
        .unwrap();

        assert_ne!(last_path_operation_thread().unwrap(), caller);
    }

    #[tokio::test]
    async fn tool_intent_precedes_physical_execution_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt("run-1").unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "request-1".into(),
                reasoning: None,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{"path":"README.md"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        CodingSessionHarness::record_tool_execution_to_path(
            &path,
            "run-1",
            ToolExecutionTraceEvent::Started {
                tool_call_id: "call-1".into(),
                tool_name: "read_file".into(),
                executor_kind: "builtin".into(),
                effective_arguments: r#"{"path":"README.md"}"#.into(),
                started_at_ms: 10,
            },
        )
        .await
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let intent_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolStarted { seq, .. } => Some(*seq),
            _ => None,
        });
        let observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolExecutionObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (intent_seq, observed_seq),
            (Some(intent), Some(observed)) if intent < observed
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_tool_observations_receive_distinct_sequences() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt("run-1").unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "request-1".into(),
                reasoning: None,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![
                        threadlane_provider::openai::ToolCall {
                            id: "call-1".into(),
                            r#type: "function".into(),
                            function: threadlane_provider::openai::ToolCallFunction {
                                name: "grep_search".into(),
                                arguments: "{}".into(),
                            },
                            thought_signature: None,
                        },
                        threadlane_provider::openai::ToolCall {
                            id: "call-2".into(),
                            r#type: "function".into(),
                            function: threadlane_provider::openai::ToolCallFunction {
                                name: "grep_search".into(),
                                arguments: "{}".into(),
                            },
                            thought_signature: None,
                        },
                    ]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        let started = |call_id: &str| ToolExecutionTraceEvent::Started {
            tool_call_id: call_id.into(),
            tool_name: "grep_search".into(),
            executor_kind: "builtin".into(),
            started_at_ms: 10,
            effective_arguments: "{}".into(),
        };
        let left_path = path.clone();
        let right_path = path.clone();
        let left = tokio::spawn(async move {
            CodingSessionHarness::record_tool_execution_to_path(
                &left_path,
                "run-1",
                started("call-1"),
            )
            .await
        });
        let right = tokio::spawn(async move {
            CodingSessionHarness::record_tool_execution_to_path(
                &right_path,
                "run-1",
                started("call-2"),
            )
            .await
        });
        left.await.unwrap().unwrap();
        right.await.unwrap().unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let sequences = store
            .records()
            .iter()
            .map(HarnessRecord::seq)
            .collect::<Vec<_>>();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn provider_attempt_trace_has_one_ordered_terminal_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Finished {
                attempt: 1,
                request_id: "request-1".into(),
                outcome: threadlane_runtime::harness::ProviderOutcome::Completed,
                error: None,
                duration_ms: 12,
                usage: Some(TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    total_tokens: 5,
                    ..Default::default()
                }),
            },
        )
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let start_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestStarted {
                seq, request_id, ..
            } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => Some(*seq),
            _ => None,
        });
        let finishes = store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestFinished {
                    seq,
                    request_id,
                    usage,
                    ..
                } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => {
                    assert_eq!(usage.as_ref().map(|usage| usage.total_tokens), Some(5));
                    Some(*seq)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finishes.len(), 1);
        assert!(start_seq.is_some_and(|seq| seq < finishes[0]));
    }

    #[test]
    fn cancellation_closes_an_unfinished_provider_attempt_before_abort_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        let run_id = harness.request_abort().unwrap().unwrap();
        harness.observe_abort_signal(&run_id, true).unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let provider_finish_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestFinished {
                seq,
                outcome: ProviderOutcome::Aborted,
                ..
            } => Some(*seq),
            _ => None,
        });
        let abort_observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::AbortObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (provider_finish_seq, abort_observed_seq),
            (Some(provider), Some(abort)) if provider < abort
        ));
    }

    #[test]
    fn subagent_start_returns_accepted_child_run_while_main_is_busy() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let parent = harness
            .begin_run("parent-run", AgentMessage::user("parent prompt", vec![]))
            .unwrap();

        let started = harness
            .start_subagent_lane("worker", "inspect", Some(&parent.prompt_entry_id))
            .unwrap();

        assert_eq!(started.accepted.run_id, started.identity.run_id);
        assert_eq!(started.accepted.lane, started.identity.lane_name);
        assert_eq!(
            started.accepted.prompt_entry_id,
            format!("entry-{}-user", started.identity.run_id)
        );
        assert!(started.accepted.accepted_through_seq >= started.identity.started_seq);
        harness.validate_accepted_run(&started.accepted).unwrap();

        let state = Reducer::reduce(harness.store.store()).unwrap();
        assert_eq!(
            state
                .lane("main")
                .and_then(|lane| lane.open_operation.as_deref()),
            Some("parent-run")
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(
                    record,
                    HarnessRecord::OperationStarted { lane, .. } if lane == "main"
                ))
                .count(),
            1
        );
        assert_eq!(
            harness
                .store
                .entries()
                .iter()
                .filter(|entry| entry.lane == "main"
                    && matches!(entry.message, AgentMessage::User { .. }))
                .count(),
            1
        );
    }
    #[test]
    fn subagent_checkpoint_persists_tool_calls_and_results_on_child_lane() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let parent = harness
            .begin_run("parent-run", AgentMessage::user("parent prompt", vec![]))
            .unwrap();
        let started = harness
            .start_subagent_lane("worker", "inspect", Some(&parent.prompt_entry_id))
            .unwrap();
        let call = threadlane_provider::openai::ToolCall {
            id: "shared-call".into(),
            r#type: "function".into(),
            function: threadlane_provider::openai::ToolCallFunction {
                name: "read_file".into(),
                arguments: r#"{"path":"src/lib.rs"}"#.into(),
            },
            thought_signature: None,
        };
        harness
            .checkpoint(
                &started.identity.lane_name,
                &started.identity.run_id,
                &[
                    AgentMessage::Assistant {
                        content: None,
                        tool_calls: Some(vec![call]),
                        stop_reason: None,
                        deferred_handle: None,
                    },
                    AgentMessage::Tool {
                        tool_call_id: "shared-call".into(),
                        name: "read_file".into(),
                        content: "contents".into(),
                        is_error: false,
                        terminate: false,
                        images: Vec::new(),
                    },
                ],
            )
            .unwrap();

        let transcript = harness.store.transcript(&started.identity.lane_name);
        assert!(transcript.entries.iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Assistant { tool_calls: Some(calls), .. }
                if calls.iter().any(|call| call.id == "shared-call")
        )));
        assert!(transcript.entries.iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Tool { tool_call_id, content, .. }
                if tool_call_id == "shared-call" && content == "contents"
        )));
    }

    #[test]
    fn invalid_subagent_source_is_not_retained_for_passive_commit() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let identity = harness
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();

        assert!(identity.identity.source_leaf_id.is_none());
        assert!(harness.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted {
                lane,
                source_leaf_id: None,
                ..
            } if lane == &identity.identity.lane_name
        )));
    }

    // ── No-tool prompt: one OperationStarted + one StepAttempt + one
    //    OperationFinished ──────────────────────────────────────────────
    #[test]
    fn no_tool_prompt_produces_one_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();

        // Prepare an assistant attempt
        let _result_entry_id = harness.prepare_assistant_attempt(&run_id).unwrap();

        // Append the assistant message
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("Hello!".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        // Record the attempt
        harness
            .record_assistant_attempt(
                &run_id,
                TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 0,
                },
            )
            .unwrap();

        // Finish
        harness
            .finish_run(&run_id, OperationOutcome::Completed, None)
            .unwrap();

        // Verify records
        let records = harness.store.records();
        let started = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationStarted { .. }))
            .count();
        let attempts = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::StepAttempt { .. }))
            .count();
        let finished = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationFinished { .. }))
            .count();

        assert_eq!(started, 1, "expected exactly one OperationStarted");
        assert_eq!(attempts, 1, "expected exactly one StepAttempt");
        assert_eq!(finished, 1, "expected exactly one OperationFinished");

        // Verify sequences are monotonically increasing
        let seqs: Vec<u64> = records.iter().map(|r| r.seq()).collect();
        for window in seqs.windows(2) {
            assert!(window[0] < window[1], "sequences must increase");
        }
    }

    // ── Reopening produces same reduced main-lane state ──────────────
    #[test]
    fn reopening_produces_same_main_lane_state() {
        let (_dir, path) = temp_session();

        let _run_id = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let id = harness.unique_run_id("test").unwrap();
            harness
                .begin_run(&id, AgentMessage::user("hello", vec![]))
                .unwrap();
            harness.prepare_assistant_attempt(&id).unwrap();
            harness
                .append_message(AgentMessage::Assistant {
                    content: Some("Hi there".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                })
                .unwrap();
            harness
                .record_assistant_attempt(
                    &id,
                    TokenUsage {
                        input_tokens: 10,
                        output_tokens: 3,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        total_tokens: 0,
                    },
                )
                .unwrap();
            harness
                .finish_run(&id, OperationOutcome::Completed, None)
                .unwrap();
            id
        };

        // Reopen and verify
        let mut reopened = CodingSessionHarness::open(&path).unwrap();
        let state = Reducer::reduce(&reopened.store).unwrap();
        let main_lane = state.lane("main").expect("main lane should exist");

        // The operation should be completed (not open)
        assert!(
            main_lane.open_operation.is_none(),
            "main lane should not have an open operation after finish"
        );

        // Verify the snapshot is consistent
        let snapshot = reopened.snapshot().unwrap();
        let main_snapshot = snapshot
            .state
            .lanes
            .iter()
            .find(|l| l.name == "main")
            .expect("main lane in snapshot");
        assert_eq!(main_snapshot.attempts, 1);
        assert_eq!(main_snapshot.open_operation, None);

        // Verify entries exist
        let entries = reopened.store.entries();
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::User { .. })),
            "user prompt entry should be present"
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::Assistant { .. })),
            "assistant entry should be present"
        );
    }

    // ── Error during finish_run propagates correctly ──────────────────
    #[test]
    fn error_during_run_terminates_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("error occurred".into()),
                tool_calls: None,
                stop_reason: Some("error".into()),
                deferred_handle: None,
            })
            .unwrap();

        let result = harness.finish_run(
            &run_id,
            OperationOutcome::Failed,
            Some("provider error".into()),
        );
        assert!(result.is_ok(), "finish with error should succeed");

        // Verify the operation is marked as failed
        let state = Reducer::reduce(&harness.store).unwrap();
        let main_lane = state.lane("main").unwrap();
        assert!(main_lane.open_operation.is_none());

        // Verify records show the failure
        let records = harness.store.records();
        let finished_record = records
            .iter()
            .find(|r| matches!(r, HarnessRecord::OperationFinished { .. }));
        assert!(finished_record.is_some(), "should have OperationFinished");
    }

    // Legacy-only reconciliation tests. Production provider execution uses
    // typed append/finish operations and never calls sync_messages.
    // ── Sync messages deduplicates correctly ──────────────────────────
    #[test]
    fn sync_messages_deduplicates_existing_entries() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        let entry_count_before = harness.store.entries().len();

        // Syncing the same messages again should not create duplicates
        harness
            .sync_messages(&[AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            }])
            .unwrap();

        assert_eq!(
            harness.store.entries().len(),
            entry_count_before,
            "sync_messages should not create duplicate entries"
        );
    }

    #[tokio::test]
    async fn sync_messages_repairs_a_tool_intent_without_its_result_entry() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();

        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{\"path\":\"README.md\"}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();

        let result = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "contents".into(),
            is_error: false,
            terminate: false,
            images: Vec::new(),
        };
        harness.sync_messages(&[result.clone()]).unwrap();

        let state = Reducer::reduce(&harness.store).unwrap();
        assert!(
            state
                .lane("main")
                .unwrap()
                .tools
                .iter()
                .find(|tool| tool.tool_call_id == "call-1")
                .unwrap()
                .completed
        );
    }

    #[test]
    fn sync_messages_persists_provider_visible_queued_user_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let initial = AgentMessage::user("initial", vec![]);
        let queued = AgentMessage::user("queued follow-up", vec![]);

        harness.begin_run(&run_id, initial.clone()).unwrap();
        harness
            .sync_messages(&[initial.clone(), queued.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[initial, queued.clone()])
            .unwrap();

        assert!(harness
            .store
            .model_context("main")
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.message == queued));
    }

    #[test]
    fn model_visibility_rejects_extra_durable_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let extra = AgentMessage::Assistant {
            content: Some("stale response".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.sync_messages(&[prompt.clone(), extra]).unwrap();

        let error = harness.assert_model_visible(&[prompt]).unwrap_err();
        assert!(error.contains("durable_count=2, provider_count=1"));
    }

    #[test]
    fn sync_messages_persists_reasoning_before_model_visibility_check() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let thinking = AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "reasoning"}),
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness
            .sync_messages(&[prompt.clone(), thinking.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[prompt, thinking.clone()])
            .unwrap();

        assert!(harness
            .store
            .entries()
            .iter()
            .any(|entry| entry.message == thinking));
    }

    #[tokio::test]
    async fn context_snapshot_capture_indexes_only_successful_local_read_results_once() {
        let (dir, path) = temp_session();
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let read_output = threadlane_tools::try_execute_tool_in_workspace(
            "read_file",
            r#"{"path":"README.md","start_line":1,"end_line":1}"#,
            dir.path(),
        )
        .unwrap();
        let output_chars = read_output.chars().count();
        let entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: read_output.clone(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        std::fs::write(dir.path().join("README.md"), "changed after read").unwrap();

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, output_chars)
                .unwrap(),
            Some("ctx-v2-tool-result-read-1".into())
        );
        assert_eq!(harness.context_snapshots("main").len(), 1);
        assert_eq!(
            harness.context_snapshots("main")[0].file_sha256.as_str(),
            threadlane_tools::read_file_snapshot_digest(&read_output).unwrap()
        );
        assert!(super::super::context_snapshots::resolve_context_snapshot(
            &path,
            dir.path(),
            "ctx-v2-tool-result-read-1",
        )
        .err()
        .unwrap()
        .starts_with("Context snapshot stale:"));
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        assert_eq!(
            harness
                .store
                .entries()
                .iter()
                .filter(|entry| matches!(entry.message, AgentMessage::Tool { .. }))
                .count(),
            1
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, output_chars)
                .unwrap(),
            Some("ctx-v2-tool-result-read-1".into())
        );
        let resolved = super::super::context_snapshots::resolve_context_snapshot(
            &path,
            dir.path(),
            "ctx-v2-tool-result-read-1",
        )
        .unwrap();
        assert_eq!(resolved.content, read_output);
        assert_eq!(resolved.snapshot.source_entry_id, entry_id);

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "missing", &entry_id, 13)
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn context_snapshot_capture_persists_fuzzy_read_actual_path() {
        let (dir, path) = temp_session();
        let actual_path = dir.path().join("crates/app/src/view.rs");
        std::fs::create_dir_all(actual_path.parent().unwrap()).unwrap();
        std::fs::write(&actual_path, "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot-fuzzy").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-fuzzy".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"src/view.rs\"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-fuzzy",
                "read_file",
                serde_json::json!({"path": "src/view.rs"}),
            )
            .await
            .unwrap();
        let read_output = threadlane_tools::try_execute_tool_in_workspace(
            "read_file",
            r#"{"path":"src/view.rs"}"#,
            dir.path(),
        )
        .unwrap();
        let entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-fuzzy".into(),
                name: "read_file".into(),
                content: read_output.clone(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();

        let context_id = harness
            .index_read_snapshot(
                &run_id,
                dir.path(),
                "read-fuzzy",
                &entry_id,
                read_output.chars().count(),
            )
            .unwrap()
            .unwrap();
        let snapshot = &harness.context_snapshots("main")[0];
        assert_eq!(snapshot.path, "crates/app/src/view.rs");
        assert_eq!(
            super::super::context_snapshots::resolve_context_snapshot(
                &path,
                dir.path(),
                &context_id,
            )
            .unwrap()
            .content,
            read_output
        );
    }

    #[tokio::test]
    async fn compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint() {
        let (dir, path) = temp_session();
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot-compact").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let read_output = threadlane_tools::try_execute_tool_in_workspace(
            "read_file",
            r#"{"path":"README.md","start_line":1,"end_line":1}"#,
            dir.path(),
        )
        .unwrap();
        let output_chars = read_output.chars().count();
        let source_entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: read_output.clone(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        let context_id = harness
            .index_read_snapshot(
                &run_id,
                dir.path(),
                "read-1",
                &source_entry_id,
                output_chars,
            )
            .unwrap()
            .unwrap();
        harness
            .store
            .append_entry_gated(HarnessEntry {
                id: "later-non-indexed-duplicate-tool-result".into(),
                parent_id: Some(source_entry_id.clone()),
                lane: "main".into(),
                seq: harness.next_seq(),
                timestamp: timestamp(),
                message: AgentMessage::Tool {
                    tool_call_id: "read-1".into(),
                    name: "read_file".into(),
                    content: "later non-indexed duplicate tool body".into(),
                    is_error: false,
                    terminate: false,
                    images: Vec::new(),
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        harness.store.drive_to_completion().unwrap();
        harness
            .append_message(AgentMessage::user("non-indexed evidence", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::user("continue", vec![]))
            .unwrap();
        let config = AgentConfig::default();
        let before = harness.model_context("main").unwrap().messages();
        let prepared =
            compact_for_budget(&before, None, 1, &CompactionParams::from(&config)).unwrap();
        harness
            .commit_prepared_compaction(
                &run_id,
                "unknown/test-model",
                None,
                &config,
                context_budget("unknown/test-model", &BudgetConfig::from(&config)),
                CompactionReason::AdaptiveBudget,
                prepared,
            )
            .unwrap();

        let compacted_messages = harness.model_context("main").unwrap().messages();
        let checkpoint_message = compacted_messages
            .iter()
            .find(|message| threadlane_compaction::compaction_summary_text(message).is_some())
            .unwrap();
        let checkpoint = threadlane_compaction::compaction_summary_text(checkpoint_message).unwrap();
        assert!(!checkpoint.contains(&context_id));
        assert!(!checkpoint.contains("README.md:1-1 sha256="));
        assert!(!checkpoint.contains("snapshot body"));
        assert!(checkpoint.contains("later non-indexed duplicate tool body"));
        let AgentMessage::Custom { payload, .. } = checkpoint_message else {
            unreachable!();
        };
        let index = payload["context_snapshot_index"]
            .as_array()
            .expect("structured snapshot index");
        assert_eq!(index.len(), 1);
        assert_eq!(index[0]["context_id"], context_id);
        let provider_checkpoint =
            threadlane_provider::convert_to_llm(std::slice::from_ref(checkpoint_message))[0]
                ["content"]
                .as_str()
                .unwrap()
                .to_owned();
        assert!(provider_checkpoint.contains(&context_id));
        assert!(provider_checkpoint.contains("README.md:1-1 sha256="));
        assert!(!provider_checkpoint.contains("snapshot body"));
        assert!(provider_checkpoint.contains("non-indexed evidence"));
        assert!(!compacted_messages
            .iter()
            .any(|message| matches!(message, AgentMessage::Tool { content, .. } if content == "snapshot body")));
        assert!(JsonlStore::open(&path)
            .unwrap()
            .entries()
            .iter()
            .any(|entry| entry.id == source_entry_id && matches!(&entry.message, AgentMessage::Tool { content, .. } if content == &read_output)));
        assert_eq!(
            super::super::context_snapshots::resolve_context_snapshot(
                &path,
                dir.path(),
                &context_id
            )
            .unwrap()
            .content,
            read_output
        );
        let post_tokens = estimate_request_tokens(
            &compacted_messages,
            None,
            &CompactionParams::from(&config),
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::ContextCompacted { post_tokens, .. } => Some(*post_tokens),
                    _ => None,
                }),
            Some(post_tokens)
        );

        harness
            .append_message(AgentMessage::user("continue again", vec![]))
            .unwrap();
        let second_config = AgentConfig::builder().max_checkpoint_chars(100_000).build();
        let before = harness.model_context("main").unwrap().messages();
        let prepared =
            compact_for_budget(&before, None, 1, &CompactionParams::from(&second_config))
                .unwrap();
        harness
            .commit_prepared_compaction(
                &run_id,
                "unknown/test-model",
                None,
                &second_config,
                context_budget("unknown/test-model", &BudgetConfig::from(&second_config)),
                CompactionReason::AdaptiveBudget,
                prepared,
            )
            .unwrap();
        let checkpoint_message = harness
            .model_context("main")
            .unwrap()
            .messages()
            .into_iter()
            .find(|message| threadlane_compaction::compaction_summary_text(message).is_some())
            .unwrap();
        let checkpoint = threadlane_compaction::compaction_summary_text(&checkpoint_message).unwrap();
        assert!(!checkpoint.contains(&context_id), "{checkpoint}");
        let AgentMessage::Custom { payload, .. } = &checkpoint_message else {
            unreachable!();
        };
        assert_eq!(
            payload["context_snapshot_index"]
                .as_array()
                .expect("structured snapshot index")
                .len(),
            1
        );
        let provider_checkpoint = threadlane_provider::convert_to_llm(&[checkpoint_message])[0]
            ["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(provider_checkpoint.matches(&context_id).count(), 1);
        assert_eq!(
            provider_checkpoint
                .matches("## Available context snapshots")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn context_snapshot_capture_skips_failed_virtual_and_unrecorded_reads() {
        let (dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot-skip").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    threadlane_provider::openai::ToolCall {
                        id: "failed".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                    threadlane_provider::openai::ToolCall {
                        id: "virtual".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"virtual://README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                    threadlane_provider::openai::ToolCall {
                        id: "remote".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"https:README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                    threadlane_provider::openai::ToolCall {
                        id: "unbound".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        for (call_id, arguments) in [
            ("failed", serde_json::json!({"path": "README.md"})),
            (
                "virtual",
                serde_json::json!({"path": "virtual://README.md"}),
            ),
            ("remote", serde_json::json!({"path": "https:README.md"})),
            ("unbound", serde_json::json!({"path": "README.md"})),
        ] {
            harness
                .append_tool_intent(&run_id, call_id, "read_file", arguments)
                .await
                .unwrap();
        }
        let failed_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "failed".into(),
                name: "read_file".into(),
                content: "not found".into(),
                is_error: true,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        let virtual_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "virtual".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        let remote_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "remote".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        let unbound_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "unbound".into(),
                name: "read_file".into(),
                content: "body without an execution-bound digest".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();
        let unrecorded_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "unrecorded".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "failed", &failed_entry, 9)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "virtual", &virtual_entry, 4)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "remote", &remote_entry, 4)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "unrecorded", &unrecorded_entry, 4)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "unbound", &unbound_entry, 38)
                .unwrap(),
            None
        );
        assert!(harness.context_snapshots("main").is_empty());
    }

    #[test]
    fn stale_metadata_and_chained_tool_results_remain_model_visible_and_get_lifecycle_records() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();

        let mut stale_store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
        stale_store.set_name("stale metadata").unwrap();

        let assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![
                threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
                threadlane_provider::openai::ToolCall {
                    id: "call-2".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "grep".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
            ]),
            stop_reason: None,
            deferred_handle: None,
        };
        let first_tool = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "first".into(),
            is_error: false,
            terminate: false,
            images: Vec::new(),
        };
        let second_tool = AgentMessage::Tool {
            tool_call_id: "call-2".into(),
            name: "grep".into(),
            content: "second".into(),
            is_error: false,
            terminate: false,
            images: Vec::new(),
        };
        let final_assistant = AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };
        let messages = vec![prompt, assistant, first_tool, second_tool, final_assistant];

        harness.sync_messages(&messages).unwrap();
        harness.assert_model_visible(&messages).unwrap();
        harness
            .record_completed_tools_with_termination(&run_id, &HashMap::new())
            .unwrap();

        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolStarted { .. }))
                .count(),
            2
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn sync_messages_persists_identical_empty_assistant_results_for_each_run() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let empty_assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        };
        let mut provider_messages = Vec::new();

        for prompt_text in ["first prompt", "second prompt"] {
            let run_id = harness.unique_run_id("test").unwrap();
            let prompt = AgentMessage::user(prompt_text, vec![]);
            harness.begin_run(&run_id, prompt.clone()).unwrap();
            provider_messages.push(prompt);
            provider_messages.push(empty_assistant.clone());

            // This mirrors CodingAgent's full provider-state synchronization
            // after each prompt. The second empty assistant must be a new
            // durable entry even though its content matches the first one.
            harness.sync_messages(&provider_messages).unwrap();
            harness
                .record_assistant_attempt(&run_id, TokenUsage::default())
                .unwrap();
            harness
                .finish_run(&run_id, OperationOutcome::Completed, None)
                .unwrap();
        }

        let assistant_entries: Vec<_> = harness
            .store
            .entries()
            .iter()
            .filter(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
            .collect();
        assert_eq!(assistant_entries.len(), 2);
        assert_ne!(assistant_entries[0].id, assistant_entries[1].id);
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn assistant_ready_persists_provider_response_attached_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("test prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            &run_id,
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "req-123".into(),
                reasoning: Some("deep thinking".into()),
                message: AgentMessage::Assistant {
                    content: Some("final answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            &run_id,
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "req-123-retry".into(),
                reasoning: Some("deep thinking".into()),
                message: AgentMessage::Assistant {
                    content: Some("final answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        let updated_harness = CodingSessionHarness::open(&path).unwrap();
        assert_eq!(
            updated_harness
                .store
                .entries()
                .iter()
                .filter(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
                .count(),
            1
        );
        let response_record = updated_harness
            .store
            .records()
            .iter()
            .find(|record| matches!(record, HarnessRecord::ProviderResponseAttached { .. }))
            .expect("must record ProviderResponseAttached");

        if let HarnessRecord::ProviderResponseAttached {
            run_id: rec_run_id,
            attempt,
            request_id,
            entry_id,
            reasoning_entry_id,
            ..
        } = response_record
        {
            assert_eq!(rec_run_id, &run_id);
            assert_eq!(*attempt, 1);
            assert_eq!(request_id.as_ref().map(|r| r.as_str()), Some("req-123"));
            assert!(!entry_id.is_empty());
            assert!(reasoning_entry_id.is_some());
        } else {
            panic!("expected ProviderResponseAttached");
        }
    }

    #[test]
    fn sequential_single_call_turns_start_tools_on_lane() {
        // Regression for session_1788900913874865000: the second ACP tool
        // faulted the gate with "tool intent does not match assistant
        // declaration" because the lane path derived the tool index from the
        // count of prior ToolStarted records instead of the declaring entry.
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        for call_id in ["call-a", "call-b"] {
            // ACP flow: one assistant entry declaring exactly one call.
            harness
                .append_message(AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: call_id.into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                })
                .unwrap();
            harness
                .tool_started_on_lane("main", "run-1", call_id, "read", serde_json::json!({}))
                .unwrap();
        }
        let state = Reducer::reduce(&harness.store).unwrap();
        let tools = &state.lane("main").unwrap().tools;
        assert_eq!(tools.len(), 2);
        assert_ne!(tools[0].assistant_entry_id, tools[1].assistant_entry_id);
        assert_eq!((tools[0].tool_index, tools[1].tool_index), (0, 0));
    }

//! Automation mutations through the owning daemon, in-process or remote.
//!
//! Run these requests on the shared Tokio executor, as with `project_io`.
//! UI projections still come exclusively from the local service watch or
//! the ordered remote `AutomationChanged` stream.

use std::sync::Arc;

use threadlane_client::DaemonClient;
use threadlane_protocol::automation::{AutomationCommand, AutomationResponse};
use threadlane_protocol::daemon::{CommandResponse, SessionCommand};

/// Capability error returned before sending a mutation to a pre-v5 daemon.
pub const UNSUPPORTED_AUTOMATIONS: &str =
    "the attached daemon does not support automation requests (protocol v5)";

/// Execute a mutation without using its unsequenced reply as a UI projection.
/// Snapshot reads need `DaemonClient::request` so their payload is available.
pub async fn mutate(
    client: &Arc<dyn DaemonClient>,
    command: AutomationCommand,
) -> Result<(), String> {
    if matches!(command, AutomationCommand::GetSnapshot) {
        return Err("automation snapshot reads must use DaemonClient::request".into());
    }
    if !client.is_connected() {
        return Err("Daemon disconnected. Reconnect and retry.".into());
    }
    if !client.supports_github_automation() {
        return Err(UNSUPPORTED_AUTOMATIONS.into());
    }
    match client
        .request(SessionCommand::AutomationRequest { command })
        .await?
    {
        CommandResponse::Automation {
            response: AutomationResponse::Projection { .. },
        } => {
            // The core journals every changed service projection. Applying
            // this unsequenced reply could regress a newer streamed update
            // (including same-revision question changes), or strip the local
            // watch's runtime handle. Let those existing feeds notify views.
            Ok(())
        }
        _ => Err("daemon answered AutomationRequest with a mismatched response".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{mutate, UNSUPPORTED_AUTOMATIONS};
    use crate::AppState;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use threadlane_client::DaemonClient;
    use threadlane_protocol::automation::{
        AutomationCommand, AutomationProjection, AutomationResponse, Definition, Schedule,
    };
    use threadlane_protocol::daemon::{
        CommandRequest, CommandResponse, SessionCommand, SessionEvent,
    };
    use threadlane_protocol::QuestionRequest;
    use tokio::sync::{mpsc, oneshot, Notify};

    /// Records transport requests and can hold a reply behind a newer event.
    struct FakeDaemon {
        requests: Mutex<Vec<CommandRequest>>,
        response: Result<CommandResponse, String>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
        started: Notify,
        supported: bool,
        connected: bool,
    }

    impl FakeDaemon {
        /// Start connected and capable, returning the supplied reply for each request.
        fn new(response: Result<CommandResponse, String>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response,
                release: Mutex::new(None),
                started: Notify::new(),
                supported: true,
                connected: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl DaemonClient for FakeDaemon {
        /// Reject fire-and-forget dispatch because mutation failures need a reply.
        async fn command(&self, _: SessionCommand) -> Result<(), String> {
            panic!("automation mutations must use the reply-carrying request path");
        }

        /// Record dispatch before optionally waiting, so tests control reply ordering.
        async fn command_request(
            &self,
            request: CommandRequest,
        ) -> Result<CommandResponse, String> {
            self.requests.lock().unwrap().push(request);
            self.started.notify_one();
            let release = self.release.lock().unwrap().take();
            if let Some(release) = release {
                release.await.unwrap();
            }
            self.response.clone()
        }

        /// Keep envelope support consistent with the selected protocol capability.
        fn supports_command_requests(&self) -> bool {
            self.supported
        }

        /// Advertise project I/O independently to expose incorrect capability gating.
        fn supports_project_io(&self) -> bool {
            true // Project I/O alone must not authorize automation requests.
        }

        /// Expose the capability under test without falling back to a local service.
        fn supports_github_automation(&self) -> bool {
            self.supported
        }

        /// Simulate an unavailable transport separately from an older protocol.
        fn is_connected(&self) -> bool {
            self.connected
        }

        /// Leave event delivery to each test's explicit ordered-stream injection.
        fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionEvent> {
            let (_, receiver) = mpsc::unbounded_channel();
            receiver
        }
    }

    /// Wrap a projection in the only successful automation mutation reply shape.
    fn response(projection: AutomationProjection) -> CommandResponse {
        CommandResponse::Automation {
            response: AutomationResponse::Projection { projection },
        }
    }

    /// Provide a daemon-host definition whose full payload must survive routing.
    fn definition() -> Definition {
        Definition {
            id: "automation".into(),
            revision: 1,
            name: "Remote review".into(),
            prompt: "Review changes".into(),
            project: "/daemon/project".into(),
            model: "model".into(),
            effort: "medium".into(),
            worktree: false,
            schedule: Schedule::Manual,
            enabled: false,
            notify_all: false,
            anchor: 0,
            next_at: None,
            failures: 0,
            paused_reason: None,
        }
    }

    /// Every UI action uses one correlated daemon request in either connection mode.
    #[tokio::test]
    async fn every_mutation_routes_through_the_daemon_without_a_local_service() {
        let commands = [
            AutomationCommand::Save {
                definition: definition(),
            },
            AutomationCommand::SetEnabled {
                id: "automation".into(),
                enabled: true,
            },
            AutomationCommand::SetEnabled {
                id: "automation".into(),
                enabled: false,
            },
            AutomationCommand::RunNow {
                id: "automation".into(),
            },
            AutomationCommand::Cancel { id: "run".into() },
            AutomationCommand::Delete {
                id: "automation".into(),
            },
            AutomationCommand::DeleteRun { id: "run".into() },
            AutomationCommand::Review { id: "run".into() },
        ];
        // LocalDaemon implements the same request path: neither mode needs
        // a view-owned service handle or a client-host filesystem fallback.
        for remote in [false, true] {
            let fake = Arc::new(FakeDaemon::new(Ok(response(Default::default()))));
            let mut state = AppState::load_from_registry(Vec::new());
            state.daemon_remote = remote;
            state.daemon_client = fake.clone();
            assert!(state.automation_service.is_none());
            for command in &commands {
                mutate(&state.daemon_client, command.clone()).await.unwrap();
            }
            let requests = fake.requests.lock().unwrap();
            assert_eq!(requests.len(), commands.len());
            for (request, expected) in requests.iter().zip(&commands) {
                let SessionCommand::AutomationRequest { command } = &request.command else {
                    panic!("unexpected request: {:?}", request.command);
                };
                assert_eq!(command, expected);
            }
            let ids: std::collections::HashSet<_> = requests.iter().map(|r| r.request_id).collect();
            assert_eq!(ids.len(), commands.len());
        }
    }

    /// Preserve dispatch errors and reject malformed success replies without retrying.
    #[tokio::test]
    async fn request_failures_and_mismatched_replies_are_visible() {
        for (reply, expected) in [
            (
                Err("remote storage is read-only".into()),
                "remote storage is read-only",
            ),
            (
                Ok(CommandResponse::Ack),
                "daemon answered AutomationRequest with a mismatched response",
            ),
        ] {
            let fake = Arc::new(FakeDaemon::new(reply));
            let client: Arc<dyn DaemonClient> = fake.clone();
            assert_eq!(
                mutate(
                    &client,
                    AutomationCommand::Delete {
                        id: "automation".into()
                    }
                )
                .await,
                Err(expected.into())
            );
            assert_eq!(
                fake.requests.lock().unwrap().len(),
                1,
                "never retry a mutation"
            );
        }
    }

    /// Unsupported or disconnected hosts fail before any mutation is transmitted.
    #[tokio::test]
    async fn unsupported_and_disconnected_daemons_fail_without_dispatch() {
        for (supported, connected, expected) in [
            (false, true, UNSUPPORTED_AUTOMATIONS),
            (true, false, "Daemon disconnected. Reconnect and retry."),
        ] {
            let mut fake = FakeDaemon::new(Ok(response(Default::default())));
            fake.supported = supported;
            fake.connected = connected;
            let fake = Arc::new(fake);
            let client: Arc<dyn DaemonClient> = fake.clone();
            assert_eq!(
                mutate(
                    &client,
                    AutomationCommand::RunNow {
                        id: "automation".into()
                    }
                )
                .await,
                Err(expected.into())
            );
            assert!(fake.requests.lock().unwrap().is_empty());
        }
    }

    /// Keep snapshot reads out of the mutation helper, which intentionally drops payloads.
    #[tokio::test]
    async fn snapshot_reads_cannot_silently_discard_their_result() {
        let fake = Arc::new(FakeDaemon::new(Ok(response(Default::default()))));
        let client: Arc<dyn DaemonClient> = fake.clone();
        assert!(mutate(&client, AutomationCommand::GetSnapshot)
            .await
            .is_err());
        assert!(fake.requests.lock().unwrap().is_empty());
    }

    /// A held reply cannot replace newer streamed state, even at the same store revision.
    #[tokio::test]
    async fn late_reply_cannot_regress_remote_state_or_same_revision_questions() {
        for reply_revision in [6, 7] {
            let mut reply = AutomationProjection::default();
            reply.snapshot.revision = reply_revision;
            let (release, receiver) = oneshot::channel();
            let fake = Arc::new(FakeDaemon::new(Ok(response(reply))));
            *fake.release.lock().unwrap() = Some(receiver);
            let mut state = AppState::load_from_registry(Vec::new());
            state.daemon_remote = true;
            state.daemon_client = fake.clone();
            let client = state.daemon_client.clone();
            let task = tokio::spawn(async move {
                mutate(&client, AutomationCommand::Review { id: "run".into() }).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(1), fake.started.notified())
                .await
                .unwrap();
            let mut streamed = AutomationProjection {
                question_queues: Some(HashMap::from([(
                    "session".into(),
                    vec![QuestionRequest {
                        id: "new-question".into(),
                        questions: Vec::new(),
                    }],
                )])),
                ..Default::default()
            };
            streamed.snapshot.revision = 7;
            assert!(
                state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
                    projection: streamed.clone(),
                }])
            );
            release.send(()).unwrap();
            task.await.unwrap().unwrap();
            assert_eq!(state.client.automation, streamed);
            assert_eq!(state.automations.snapshot, streamed.snapshot);
            assert_eq!(state.pending_questions["session"].id, "new-question");

            // A later stream update still applies and requests a redraw.
            streamed.snapshot.revision = 8;
            streamed.question_queues = Some(HashMap::new());
            assert!(
                state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
                    projection: streamed.clone(),
                }])
            );
            assert_eq!(state.client.automation, streamed);
            assert!(!state.pending_questions.contains_key("session"));
        }
    }

    /// Local watch-only state survives both command replies and duplicate wire events.
    #[tokio::test]
    async fn local_watch_remains_authoritative_after_a_successful_request() {
        let mut state = AppState::load_from_registry(Vec::new());
        let mut local = crate::automation::Projection {
            notification: Some(("run".into(), "Finished".into())),
            ..Default::default()
        };
        local.snapshot.revision = 5;
        state.apply_automation_projection(local.clone());
        let client_projection = state.client.automation.clone();
        state.daemon_client = Arc::new(FakeDaemon::new(Ok(response(Default::default()))));
        mutate(
            &state.daemon_client,
            AutomationCommand::Review { id: "run".into() },
        )
        .await
        .unwrap();
        assert_eq!(state.client.automation, client_projection);
        assert_eq!(state.automations.snapshot, local.snapshot);
        assert_eq!(state.automations.notification, local.notification);

        // Local mode also ignores wire events, preserving its richer watch.
        assert!(
            !state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
                projection: Default::default(),
            }])
        );
        local.snapshot.revision = 6;
        state.apply_automation_projection(local.clone());
        assert_eq!(state.automations.snapshot, local.snapshot);
        assert_eq!(state.automations.notification, local.notification);
    }
}

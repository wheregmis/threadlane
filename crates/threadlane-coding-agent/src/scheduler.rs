//! Session work scheduler: durable intent plus an in-memory wake.
//!
//! Every queued input is persisted as a durable queue entry first
//! (`QueueEnqueued`); the in-memory [`AgentWork`] wake only tells the turn
//! loop something is pending. Consumption is always by exact
//! `(queue, entry_id)` identity — never by message value, since consecutive
//! identical messages are legitimate — and a wake is finished only after its
//! own entry is consumed, so a steer or follow-up arriving mid-turn keeps its
//! own scheduler wake. This is the only in-memory staging in the session;
//! there is no second queue implementation.

use super::harness::CodingSessionHarness;
#[cfg(test)]
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use threadlane_protocol::{AgentMessage, ImageAttachment};
#[cfg(test)]
use threadlane_protocol::{AgentToolDefinition, ToolExecutor};
use threadlane_runtime::AgentRuntime;
use threadlane_runtime::harness::QueueKind;
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWork {
    DurableQueueWake {
        queue: QueueKind,
        entry_id: String,
    },
    SteerMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
    QueueMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentWorkExecution {
    Idle,
    Completed { work_items: usize },
}

impl AgentWorkExecution {
    pub(crate) fn completed(self) -> bool {
        matches!(self, Self::Completed { work_items } if work_items > 0)
    }
}

#[cfg(test)]
pub type AgentWorkObserver = Arc<std::sync::Mutex<Vec<AgentWork>>>;
#[cfg(test)]
pub type SubagentObserverState = Arc<std::sync::Mutex<Option<AgentWorkObserver>>>;
#[cfg(test)]
pub type SubagentBoundaryObserver = Arc<dyn Fn() + Send + Sync>;

fn enqueue_harness_queue(
    session_file: &Path,
    queue: QueueKind,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    harness.enqueue_unbound_with_images(queue, content, images)
}

pub(crate) fn enqueue_harness_follow_up(
    session_file: &Path,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    enqueue_harness_queue(session_file, QueueKind::FollowUp, content, images)
}

#[derive(Clone, Default)]
pub struct AgentWorkScheduler {
    pending: Arc<std::sync::Mutex<VecDeque<AgentWork>>>,
    acp_model: Arc<AtomicBool>,
    wake: Arc<Notify>,
    /// Exactly one executor may drive a session scheduler at a time. Queue
    /// wakes are shared across surface adapters, but the CodingAgent runtime
    /// and its durable reconciliation must have one execution owner.
    execution_owner: Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)]
    test_observer: SubagentObserverState,
}

impl AgentWorkScheduler {
    pub(crate) async fn acquire_execution_owner(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.execution_owner.clone().lock_owned().await
    }
    pub(crate) fn schedule(&self, work: AgentWork) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push_back(work);
            self.wake.notify_one();
        }
    }

    /// Wait until this scheduler has queued work. The notification is shared
    /// by all scheduler clones and is only a wake; the queue remains the
    /// durable source of truth for execution.
    #[allow(dead_code)]
    pub(crate) async fn wait_for_work(&self) {
        loop {
            if self.next().is_some() {
                return;
            }
            self.wake.notified().await;
        }
    }

    fn drain(&self) -> Vec<AgentWork> {
        self.pending
            .lock()
            .map(|mut pending| pending.drain(..).collect())
            .unwrap_or_default()
    }

    pub(crate) fn set_acp_model(&self, is_acp: bool) {
        self.acp_model.store(is_acp, Ordering::SeqCst);
    }

    pub(crate) fn next(&self) -> Option<AgentWork> {
        self.pending.lock().ok()?.front().cloned()
    }

    pub(crate) fn finish_next(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.pop_front();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_test_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.test_observer.lock() {
            *current = Some(observer);
        }
    }

    /// Run one scheduler batch while the caller retains the session lease.
    /// The main runtime uses this across its whole drain loop, so another
    /// surface cannot take ownership between adjacent batches.
    pub(crate) async fn run_executor_with_owner(
        &self,
        agent: &mut AgentRuntime,
        session_file: Option<&Path>,
        _execution_owner: &tokio::sync::OwnedMutexGuard<()>,
    ) -> AgentWorkExecution {
        let pending = self.drain();
        if pending.is_empty() {
            return AgentWorkExecution::Idle;
        }
        let work_items = pending.len();
        #[cfg(test)]
        if let Ok(Some(observer)) = self.test_observer.lock().map(|observer| observer.clone()) {
            if let Ok(mut observed) = observer.lock() {
                observed.extend(pending);
            }
            return AgentWorkExecution::Completed { work_items };
        }
        for work in pending {
            match work {
                AgentWork::DurableQueueWake { queue, entry_id } => {
                    let Some(path) = session_file else { continue };
                    if let Ok(mut harness) = CodingSessionHarness::open(path) {
                        if let Ok(Some(message)) =
                            harness.consume_unbound_queue_entry(queue.clone(), &entry_id)
                        {
                            agent.run_consumed_queue_message(queue, message).await;
                        }
                    }
                }
                AgentWork::SteerMessage { content, images } => {
                    agent.steer(AgentMessage::user(content, images));
                    agent.run_steer().await;
                }
                AgentWork::QueueMessage { content, images } => {
                    agent.follow_up(AgentMessage::user(content, images));
                    agent.run_follow_up().await;
                }
            }
        }
        AgentWorkExecution::Completed { work_items }
    }
}

#[cfg(test)]
pub struct DeterministicSubagentToolExecutor {
    pub(crate) observed: Arc<AtomicBool>,
}

#[cfg(test)]
#[async_trait]
impl ToolExecutor for DeterministicSubagentToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.test.subagent_tool"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition {
            name: "test_child_tool".into(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
            strict: None,
        }]
        .into()
    }

    async fn execute_tool(&self, name: &str, _args: &str) -> Option<Result<String, String>> {
        (name == "test_child_tool").then(|| {
            self.observed.store(true, Ordering::SeqCst);
            Ok("test child tool result".into())
        })
    }
}

#[derive(Clone)]
pub struct CodingAgentWorkHandle {
    scheduler: AgentWorkScheduler,
    session_file: Option<PathBuf>,
}

impl CodingAgentWorkHandle {
    pub(crate) async fn wait_for_work(&self) {
        self.scheduler.wait_for_work().await;
    }
    pub(crate) fn new(scheduler: AgentWorkScheduler, session_file: Option<PathBuf>) -> Self {
        Self {
            scheduler,
            session_file,
        }
    }

    pub fn queue_steer_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        if self.scheduler.acp_model.load(Ordering::SeqCst) {
            return Err("This agent does not support live steering; use Queue.".into());
        }
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            let entry_id = enqueue_harness_queue(path, QueueKind::Steer, content, images)?;
            self.scheduler.schedule(AgentWork::DurableQueueWake {
                queue: QueueKind::Steer,
                entry_id,
            });
        } else {
            self.scheduler
                .schedule(AgentWork::SteerMessage { content, images });
        }
        Ok(())
    }

    pub fn try_queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            // ACP accepts the next prompt only after its current turn ends.
            // NextRun also retains the unsent input when that turn is stopped.
            let queue = if self.scheduler.acp_model.load(Ordering::SeqCst) {
                QueueKind::NextRun
            } else {
                QueueKind::FollowUp
            };
            let entry_id = enqueue_harness_queue(path, queue.clone(), content, images)?;
            self.scheduler
                .schedule(AgentWork::DurableQueueWake { queue, entry_id });
        } else {
            self.scheduler
                .schedule(AgentWork::QueueMessage { content, images });
        }
        Ok(())
    }
}

#[cfg(test)]
mod execution_owner_tests {
    use super::{AgentWork, AgentWorkExecution, AgentWorkScheduler};
    use std::time::Duration;

    #[tokio::test]
    async fn execution_owner_serializes_session_drivers() {
        let scheduler = AgentWorkScheduler::default();
        let first = scheduler.acquire_execution_owner().await;
        let scheduler_clone = scheduler.clone();
        let mut waiting =
            tokio::spawn(async move { scheduler_clone.acquire_execution_owner().await });

        let result = tokio::time::timeout(Duration::from_millis(20), &mut waiting).await;
        assert!(result.is_err());
        waiting.abort();
        drop(first);

        let second = tokio::time::timeout(Duration::from_secs(1), async {
            scheduler.acquire_execution_owner().await
        })
        .await
        .expect("second executor should acquire the released session lease");
        drop(second);
    }

    #[test]
    fn execution_result_distinguishes_idle_from_completed_work() {
        assert!(!AgentWorkExecution::Idle.completed());
        assert!(AgentWorkExecution::Completed { work_items: 1 }.completed());
    }

    #[tokio::test]
    async fn scheduling_wakes_cloned_scheduler_waiters() {
        let scheduler = AgentWorkScheduler::default();
        let waiter = scheduler.clone();
        let waiting = tokio::spawn(async move { waiter.wait_for_work().await });

        scheduler.schedule(AgentWork::QueueMessage {
            content: "wake".into(),
            images: Vec::new(),
        });

        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("scheduled work should wake the supervisor")
            .expect("waiter task should finish");
    }
}

use super::harness::CodingSessionHarness;
#[cfg(test)]
use async_trait::async_trait;
use log::warn;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use threadlane_runtime::harness::QueueKind;
use threadlane_runtime::{AgentMessage, AgentRuntime, ImageAttachment};
#[cfg(test)]
use threadlane_runtime::{AgentToolDefinition, ToolExecutor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentWork {
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

#[cfg(test)]
pub(crate) type AgentWorkObserver = Arc<std::sync::Mutex<Vec<AgentWork>>>;
#[cfg(test)]
pub(crate) type SubagentObserverState = Arc<std::sync::Mutex<Option<AgentWorkObserver>>>;
#[cfg(test)]
pub(crate) type SubagentBoundaryObserver = Arc<dyn Fn() + Send + Sync>;

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
pub(crate) struct AgentWorkScheduler {
    pending: Arc<std::sync::Mutex<VecDeque<AgentWork>>>,
    acp_model: Arc<AtomicBool>,
    #[cfg(test)]
    test_observer: SubagentObserverState,
}

impl AgentWorkScheduler {
    pub(crate) fn schedule(&self, work: AgentWork) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push_back(work);
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

    pub(crate) async fn run_executor(
        &self,
        agent: &mut AgentRuntime,
        session_file: Option<&Path>,
    ) -> bool {
        let pending = self.drain();
        if pending.is_empty() {
            return false;
        }
        #[cfg(test)]
        if let Ok(Some(observer)) = self.test_observer.lock().map(|observer| observer.clone()) {
            if let Ok(mut observed) = observer.lock() {
                observed.extend(pending);
            }
            return true;
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
        true
    }
}

#[cfg(test)]
pub(crate) struct DeterministicSubagentToolExecutor {
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
    pub(crate) fn new(scheduler: AgentWorkScheduler, session_file: Option<PathBuf>) -> Self {
        Self {
            scheduler,
            session_file,
        }
    }

    pub fn queue_follow_up(&self, content: impl Into<String>) {
        self.queue_follow_up_with_images(content, Vec::new());
    }

    fn queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) {
        if let Err(error) = self.try_queue_follow_up_with_images(content, images) {
            warn!("Failed to persist queued follow-up: {error}");
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

    pub fn cancel_queued_follow_up(&self, entry_id: &str) -> Result<(), String> {
        let Some(path) = self.session_file.as_deref() else {
            return Err("session persistence is unavailable".into());
        };
        let mut harness = CodingSessionHarness::open(path)?;
        harness.cancel_queued_unbound(entry_id)
    }
}

//! Live agent-to-agent messaging (oh-my-pi hub/IRC parity).
//!
//! One-shot `subagent` batches previously ran isolated children with no
//! channel between them. This module provides the shared mailbox backing two
//! tools:
//! - child `message_peer` (sibling IRC during parallel batches; doubles as
//!   inbox drain so one call both sends and receives);
//! - parent `hub` (`list` roster, `send` steer-live-lane, `read` lane
//!   transcript, `revive` parked lane, `kill` live lane, `wait` for lanes)
//!   for persistent background workers spawned with `subagent(wait=false)`.
//!
//! Mailboxes are in-memory (live coordination); every send/receive also
//! passes through tool calls/results, so the durable harness transcript
//! retains the exchange. Inbox follow-ups are bounded to avoid loops.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One queued peer message.
#[derive(Debug, Clone)]
pub(crate) struct QueuedMessage {
    pub(crate) from: String,
    pub(crate) body: String,
    pub(crate) seq: u64,
}

/// Lane roster entry for `hub list`.
#[derive(Debug, Clone)]
pub(crate) struct HubLaneInfo {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) model: String,
    pub(crate) live: bool,
    pub(crate) unread: usize,
    /// Terminal outcome once settled: `completed`, `failed`, or `killed`.
    pub(crate) outcome: Option<String>,
}

#[derive(Debug, Default)]
struct HubInner {
    inbox: HashMap<String, VecDeque<QueuedMessage>>,
    lanes: HashMap<String, HubLaneInfo>,
    /// Lane/agent keys flagged for shutdown via `hub kill`. Children observe
    /// this at the next turn boundary (see `wait_killed`).
    killed: HashSet<String>,
    /// Terminal outcomes by lane name, set on settle/kill.
    outcomes: HashMap<String, String>,
    next_seq: u64,
}

/// Shared mailbox + live-lane roster.
///
/// Keys are lane names; agent-name aliases resolve via the roster (first live
/// lane with that agent name). `send` to `"all"` broadcasts to every other
/// lane with an inbox.
///
/// Liveness notifications fan out through `wake`: `register`, `mark_settled`,
/// and `flag_killed` all notify, so `hub wait` and the child kill race never
/// poll the mutex in a hot loop.
#[derive(Debug, Clone, Default)]
pub(crate) struct SubagentHub {
    inner: Arc<Mutex<HubInner>>,
    wake: Arc<tokio::sync::Notify>,
}

impl SubagentHub {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a live lane (idempotent). Re-registering (e.g. revive)
    /// clears any stale kill flag and outcome for the lane and agent alias.
    pub(crate) fn register(
        &self,
        lane_name: String,
        run_id: String,
        agent: String,
        task: String,
        model: String,
    ) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.killed.remove(&lane_name);
            inner.killed.remove(&agent);
            inner.outcomes.remove(&lane_name);
            inner.lanes.insert(
                lane_name.clone(),
                HubLaneInfo {
                    lane_name: lane_name.clone(),
                    run_id,
                    agent,
                    task,
                    model,
                    live: true,
                    unread: 0,
                    outcome: None,
                },
            );
            inner.inbox.entry(lane_name).or_default();
        }
        self.wake.notify_waiters();
    }

    pub(crate) fn mark_settled(&self, lane_name: &str, live: bool) {
        {
            if let Ok(mut inner) = self.inner.lock() {
                if let Some(lane) = inner.lanes.get_mut(lane_name) {
                    lane.live = live;
                }
            }
        }
        self.wake.notify_waiters();
    }

    /// Record a terminal outcome (`completed`, `failed`, `killed`).
    pub(crate) fn set_outcome(&self, lane_name: &str, outcome: impl Into<String>) {
        {
            if let Ok(mut inner) = self.inner.lock() {
                inner
                    .outcomes
                    .insert(lane_name.to_string(), outcome.into());
            }
        }
        self.wake.notify_waiters();
    }

    /// Flag a lane for shutdown. Returns false when the lane is unknown.
    /// The child observes the flag at its next turn boundary and exits its
    /// turn loop with a killed result; the caller records the harness abort.
    pub(crate) fn flag_killed(&self, lane_name: &str, agent: &str) -> bool {
        let known = {
            match self.inner.lock() {
                Ok(mut inner) => {
                    if !inner.lanes.contains_key(lane_name) {
                        return false;
                    }
                    inner.killed.insert(lane_name.to_string());
                    if !agent.is_empty() {
                        inner.killed.insert(agent.to_string());
                    }
                    true
                }
                Err(_) => return false,
            }
        };
        if known {
            // Wake both the child's kill race and any parent `hub wait`.
            self.wake.notify_waiters();
        }
        known
    }

    pub(crate) fn is_killed(&self, lane_name: &str, agent: &str) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            inner.killed.contains(lane_name)
                || (!agent.is_empty() && inner.killed.contains(agent))
        })
    }

    /// Resolve when the lane (or its agent alias) is kill-flagged. Never
    /// holds the mutex across an await point.
    pub(crate) async fn wait_killed(&self, lane_name: &str, agent: &str) {
        loop {
            if self.is_killed(lane_name, agent) {
                return;
            }
            self.wake.notified().await;
        }
    }

    /// Resolve `target` (lane name or agent alias) to a roster entry.
    pub(crate) fn resolve_lane(&self, target: &str) -> Option<HubLaneInfo> {
        self.roster()
            .into_iter()
            .find(|lane| lane.lane_name == target || lane.agent == target)
    }

    /// Wait until every listed lane settles or `timeout` elapses. An empty
    /// target list waits for all currently live lanes. Returns the per-lane
    /// status lines plus whether the wait timed out.
    pub(crate) async fn wait_settled(
        &self,
        lane_names: &[String],
        timeout: Duration,
    ) -> (Vec<String>, bool) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let roster = self.roster();
            let watch: Vec<&HubLaneInfo> = if lane_names.is_empty() {
                roster.iter().filter(|lane| lane.live).collect()
            } else {
                roster
                    .iter()
                    .filter(|lane| lane_names.iter().any(|name| name == &lane.lane_name))
                    .collect()
            };
            if watch.iter().all(|lane| !lane.live) {
                let lines = watch
                    .iter()
                    .map(|lane| {
                        format!(
                            "- {} [settled{}] agent={}",
                            lane.lane_name,
                            lane.outcome
                                .as_deref()
                                .map(|outcome| format!(":{outcome}"))
                                .unwrap_or_default(),
                            lane.agent
                        )
                    })
                    .collect();
                return (lines, false);
            }
            if tokio::time::Instant::now() >= deadline {
                let lines = watch
                    .iter()
                    .map(|lane| {
                        format!(
                            "- {} [{}] agent={}",
                            lane.lane_name,
                            if lane.live { "live" } else { "settled" },
                            lane.agent
                        )
                    })
                    .collect();
                return (lines, true);
            }
            tokio::select! {
                _ = self.wake.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(200)) => {},
            }
        }
    }

    fn resolve_key_locked(inner: &HubInner, target: &str) -> Option<String> {
        if inner.inbox.contains_key(target) {
            return Some(target.to_string());
        }
        // Agent-name alias: first lane (prefer live) with that agent name.
        let mut fallback: Option<String> = None;
        for (lane_name, info) in &inner.lanes {
            if info.agent == target {
                if info.live {
                    return Some(lane_name.clone());
                }
                fallback.get_or_insert_with(|| lane_name.clone());
            }
        }
        fallback
    }

    /// Send a message. `from` is the sender lane (or `"parent"`).
    /// Unknown targets create a pending inbox so early sends to not-yet-
    /// started siblings are queued, not dropped. Callers validate names
    /// (sibling list / roster) before sending so typos still error there.
    /// Returns the resolved recipient keys.
    pub(crate) fn send(&self, from: &str, to: &str, body: String) -> Result<Vec<String>, String> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Err("message must be non-empty".into());
        }
        if body.chars().count() > 8_000 {
            return Err("message exceeds 8,000 characters".into());
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Subagent hub is unavailable".to_string())?;
        let targets: Vec<String> = if to == "all" {
            let mut keys: Vec<String> = inner
                .inbox
                .keys()
                .filter(|key| key.as_str() != from)
                .cloned()
                .collect();
            // Include registered lanes without an inbox yet so a broadcast
            // at batch start reaches late starters via their agent key.
            for info in inner.lanes.values() {
                for key in [&info.lane_name, &info.agent] {
                    if key != from && !keys.iter().any(|existing| existing == key) {
                        keys.push(key.clone());
                    }
                }
            }
            keys
        } else if let Some(resolved) = Self::resolve_key_locked(&inner, to) {
            vec![resolved]
        } else {
            // Pending inbox for a not-yet-started sibling; the recipient
            // drains it under this key once it starts.
            vec![to.to_string()]
        };
        if targets.is_empty() {
            return Err("no message recipients".into());
        }
        for target in &targets {
            inner.next_seq += 1;
            let seq = inner.next_seq;
            inner
                .inbox
                .entry(target.clone())
                .or_default()
                .push_back(QueuedMessage {
                    from: from.to_string(),
                    body: body.clone(),
                    seq,
                });
            if let Some(lane) = inner.lanes.get_mut(target) {
                lane.unread += 1;
            }
        }
        Ok(targets)
    }

    /// Drain pending inbox messages for a lane (marks roster read).
    pub(crate) fn drain(&self, lane_name: &str) -> Vec<QueuedMessage> {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => return Vec::new(),
        };
        let messages: Vec<QueuedMessage> = inner
            .inbox
            .get_mut(lane_name)
            .map(|queue| queue.drain(..).collect())
            .unwrap_or_default();
        if let Some(lane) = inner.lanes.get_mut(lane_name) {
            lane.unread = 0;
        }
        messages
    }

    pub(crate) fn roster(&self) -> Vec<HubLaneInfo> {
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => return Vec::new(),
        };
        // Snapshot queue lengths first to avoid borrowing `inner` both ways.
        let unread: HashMap<String, usize> = inner
            .inbox
            .iter()
            .map(|(lane_name, queue)| (lane_name.clone(), queue.len()))
            .collect();
        let mut lanes: Vec<HubLaneInfo> = inner
            .lanes
            .values()
            .map(|lane| HubLaneInfo {
                unread: unread.get(&lane.lane_name).copied().unwrap_or(lane.unread),
                outcome: inner.outcomes.get(&lane.lane_name).cloned(),
                ..lane.clone()
            })
            .collect();
        lanes.sort_by(|a, b| a.lane_name.cmp(&b.lane_name));
        lanes
    }
}

/// Request to revive a settled lane with a follow-up prompt.
/// Fulfilled by the session runtime, which owns the provider context
/// needed to spawn the follow-up run on the same lane.
#[derive(Debug, Clone)]
pub(crate) struct ReviveRequest {
    pub(crate) lane_name: String,
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) model: String,
    pub(crate) message: String,
}

/// Spawns a revived follow-up run; returns a short ack for the tool result.
pub(crate) type ReviveHook = Arc<
    dyn Fn(ReviveRequest) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_send_drain_and_alias_resolution() {
        let hub = SubagentHub::new();
        hub.register("lane-a".into(), "run-a".into(), "scout".into(), "t".into(), "m".into());
        hub.register("lane-b".into(), "run-b".into(), "worker".into(), "t".into(), "m".into());
        let targets = hub.send("lane-a", "worker", "hello".into()).unwrap();
        assert_eq!(targets, vec!["lane-b".to_string()]);
        let inbox = hub.drain("lane-b");
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "lane-a");
        assert_eq!(inbox[0].body, "hello");
        assert!(hub.drain("lane-b").is_empty());
    }

    #[test]
    fn broadcast_skips_sender_and_queues_unknown() {
        let hub = SubagentHub::new();
        hub.register("lane-a".into(), "run-a".into(), "a".into(), "t".into(), "m".into());
        hub.register("lane-b".into(), "run-b".into(), "b".into(), "t".into(), "m".into());
        hub.send("lane-a", "all", "hi all".into()).unwrap();
        assert!(hub.drain("lane-a").is_empty());
        assert_eq!(hub.drain("lane-b").len(), 1);
        // Unknown targets queue as pending inboxes for late starters.
        let targets = hub.send("lane-a", "ghost", "x".into()).unwrap();
        assert_eq!(targets, vec!["ghost".to_string()]);
        assert_eq!(hub.drain("ghost").len(), 1);
        assert!(hub.send("lane-a", "lane-b", "   ".into()).is_err());
    }

    #[tokio::test]
    async fn message_peer_sends_and_drains_inbox() {
        use threadlane_runtime::ToolExecutor;
        let hub = SubagentHub::new();
        let siblings = vec!["scout".to_string(), "worker".to_string()];
        let scout = MessagePeerToolExecutor::new(
            hub.clone(),
            "lane-scout".into(),
            "scout".into(),
            siblings.clone(),
        );
        let worker = MessagePeerToolExecutor::new(
            hub.clone(),
            "lane-worker".into(),
            "worker".into(),
            siblings,
        );
        // Unknown targets error against the sibling list before queueing.
        let error = worker
            .execute_tool("message_peer", r#"{"to":"ghost","message":"hi"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("unknown message target"));
        let sent = scout
            .execute_tool("message_peer", r#"{"to":"worker","message":"schema moved"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(sent.contains("lane-worker") || sent.contains("worker"));
        let received = worker
            .execute_tool("message_peer", r#"{"to":"scout","message":"ack"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(received.contains("schema moved"));
    }

    #[tokio::test]
    async fn hub_list_send_read_validate_targets() {
        use threadlane_runtime::ToolExecutor;
        let hub = SubagentHub::new();
        let executor = HubToolExecutor::new(hub.clone(), None);
        let empty = executor
            .execute_tool("hub", r#"{"action":"list"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(empty.contains("No subagent lanes"));
        hub.register(
            "lane-a".into(),
            "run-a".into(),
            "scout".into(),
            "explore".into(),
            "m".into(),
        );
        let list = executor
            .execute_tool("hub", r#"{"action":"list"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(list.contains("lane-a"));
        let unknown = executor
            .execute_tool("hub", r#"{"action":"send","target":"ghost","message":"hi"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(unknown.contains("unknown hub target"));
        let sent = executor
            .execute_tool("hub", r#"{"action":"send","target":"scout","message":"steer"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(sent.contains("lane-a"));
        assert_eq!(hub.drain("lane-a").len(), 1);
    }

    #[tokio::test]
    async fn hub_kill_flags_live_lane_and_rejects_settled() {
        use threadlane_runtime::ToolExecutor;
        let hub = SubagentHub::new();
        let executor = HubToolExecutor::new(hub.clone(), None);
        hub.register(
            "lane-k".into(),
            "run-k".into(),
            "worker".into(),
            "build".into(),
            "m".into(),
        );
        // Unknown targets error before any flag is set.
        let unknown = executor
            .execute_tool("hub", r#"{"action":"kill","target":"ghost"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(unknown.contains("unknown hub target"));
        assert!(!hub.is_killed("lane-k", "worker"));
        // Live lane: flag set, roster settles as killed. No session file, so
        // the harness abort is skipped but the flag still lands.
        let killed = executor
            .execute_tool("hub", r#"{"action":"kill","target":"worker"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(killed.contains("lane-k"));
        assert!(hub.is_killed("lane-k", "worker"));
        let lane = hub.resolve_lane("lane-k").unwrap();
        assert!(!lane.live);
        assert_eq!(lane.outcome.as_deref(), Some("killed"));
        // Second kill reports the settled state instead of re-flagging.
        let again = executor
            .execute_tool("hub", r#"{"action":"kill","target":"lane-k"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(again.contains("already settled"));
        // Re-registering (revive path) clears the flag and outcome.
        hub.register(
            "lane-k".into(),
            "run-k2".into(),
            "worker".into(),
            "build".into(),
            "m".into(),
        );
        assert!(!hub.is_killed("lane-k", "worker"));
        assert!(hub.resolve_lane("lane-k").unwrap().live);
        assert!(hub.resolve_lane("lane-k").unwrap().outcome.is_none());
    }

    #[tokio::test]
    async fn hub_wait_returns_settled_or_times_out() {
        use threadlane_runtime::ToolExecutor;
        let hub = SubagentHub::new();
        let executor = HubToolExecutor::new(hub.clone(), None);
        // Nothing live: immediate no-op.
        let idle = executor
            .execute_tool("hub", r#"{"action":"wait"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(idle.contains("nothing to wait"));
        hub.register(
            "lane-w".into(),
            "run-w".into(),
            "scout".into(),
            "explore".into(),
            "m".into(),
        );
        // Live lane with a short timeout: reports the waiter, not success.
        let timed_out = executor
            .execute_tool("hub", r#"{"action":"wait","timeout":1}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(timed_out.contains("Timed out"));
        assert!(timed_out.contains("lane-w"));
        // After settling, the same wait resolves with the outcome.
        hub.mark_settled("lane-w", false);
        hub.set_outcome("lane-w", "completed");
        let settled = executor
            .execute_tool(
                "hub",
                r#"{"action":"wait","sessions":["scout"],"timeout":5}"#,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(settled.contains("lane-w"));
        assert!(settled.contains("completed"));
        assert!(!settled.contains("Timed out"));
        // Unknown wait targets error with the known lanes.
        let unknown = executor
            .execute_tool("hub", r#"{"action":"wait","sessions":["ghost"]}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(unknown.contains("unknown hub target"));
    }

    #[tokio::test]
    async fn hub_revive_validates_liveness_and_delegates_to_hook() {
        use threadlane_runtime::ToolExecutor;
        use std::sync::{Arc, Mutex as StdMutex};
        let hub = SubagentHub::new();
        let without_hook = HubToolExecutor::new(hub.clone(), None);
        hub.register(
            "lane-r".into(),
            "run-r".into(),
            "worker".into(),
            "build".into(),
            "m".into(),
        );
        // Live lanes reject revive (steer with `hub send` instead).
        let live = without_hook
            .execute_tool("hub", r#"{"action":"revive","target":"lane-r"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(live.contains("still live"));
        hub.mark_settled("lane-r", false);
        hub.set_outcome("lane-r", "completed");
        // Settled without a spawner: explicit unavailable error.
        let no_hook = without_hook
            .execute_tool("hub", r#"{"action":"revive","target":"lane-r"}"#)
            .await
            .unwrap()
            .unwrap_err();
        assert!(no_hook.contains("unavailable"));
        // Settled with a hook: request fields forwarded, ack returned.
        let seen: Arc<StdMutex<Vec<ReviveRequest>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_hook = seen.clone();
        let hook: ReviveHook = Arc::new(move |req: ReviveRequest| {
            let seen_hook = seen_hook.clone();
            Box::pin(async move {
                seen_hook.lock().unwrap().push(req);
                Ok("revived".into())
            })
        });
        let with_hook = HubToolExecutor::new(hub.clone(), None).with_revive_hook(hook);
        let ack = with_hook
            .execute_tool(
                "hub",
                r#"{"action":"revive","target":"worker","message":"try again"}"#,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ack, "revived");
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].lane_name, "lane-r");
        assert_eq!(requests[0].agent, "worker");
        assert_eq!(requests[0].message, "try again");
    }
}

// ── Tool executors ─────────────────────────────────────────────────────
// `message_peer` runs inside child lanes; `hub` runs on the parent. Both
// share `SubagentHub` so sibling IRC and parent steering use one channel.

pub(crate) const HUB_TOOL_NAME: &str = "hub";
pub(crate) const MESSAGE_PEER_TOOL_NAME: &str = "message_peer";

/// Drain both the lane-name inbox and the agent-role inbox. Lanes address
/// each other by role (stable before journal identities resolve); the parent
/// addresses by lane name from `hub list`.
pub(crate) fn drain_lane_inbox(hub: &SubagentHub, lane_name: &str, agent: &str) -> Vec<QueuedMessage> {
    let mut messages = hub.drain(lane_name);
    if agent != lane_name {
        messages.extend(hub.drain(agent));
    }
    messages.sort_by_key(|message| message.seq);
    messages
}

pub(crate) fn format_inbox(messages: &[QueuedMessage]) -> serde_json::Value {
    serde_json::Value::Array(
        messages
            .iter()
            .map(|message| {
                serde_json::json!({"from": message.from, "message": message.body})
            })
            .collect(),
    )
}

#[derive(Clone)]
pub(crate) struct MessagePeerToolExecutor {
    hub: SubagentHub,
    lane_name: String,
    agent: String,
    /// Sibling agent roles (+ own lane) valid as `to` targets.
    siblings: Vec<String>,
}

impl MessagePeerToolExecutor {
    pub(crate) fn new(
        hub: SubagentHub,
        lane_name: String,
        agent: String,
        siblings: Vec<String>,
    ) -> Self {
        Self { hub, lane_name, agent, siblings }
    }
}

#[async_trait::async_trait]
impl threadlane_runtime::ToolExecutor for MessagePeerToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.message_peer"
    }

    fn tool_definitions(&self) -> Arc<[threadlane_runtime::AgentToolDefinition]> {
        vec![threadlane_runtime::AgentToolDefinition {
            name: MESSAGE_PEER_TOOL_NAME.into(),
            description: Some(
                "Send a live message to a sibling subagent (lane name, agent role, or `all`) and receive pending inbox messages. One call both sends and drains your inbox.".into(),
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "Recipient lane name, agent role, or `all`."},
                    "message": {"type": "string", "description": "Message body (non-empty, max 8,000 chars)."}
                },
                "required": ["to", "message"],
                "additionalProperties": false
            }),
            strict: None,
        }]
        .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != MESSAGE_PEER_TOOL_NAME {
            return None;
        }
        let parsed: serde_json::Value = match serde_json::from_str(args) {
            Ok(value) => value,
            Err(error) => return Some(Err(format!("Invalid message_peer arguments: {error}"))),
        };
        let to = parsed
            .get("to")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let message = parsed
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if to.is_empty() || message.trim().is_empty() {
            return Some(Err("`message_peer` requires non-empty `to` and `message`".into()));
        }
        let known = to == "all"
            || self.siblings.iter().any(|sibling| sibling == to)
            || to == self.lane_name
            || to == self.agent;
        if !known {
            return Some(Err(format!(
                "unknown message target: {to}. Known siblings: {}",
                self.siblings.join(", ")
            )));
        }
        let from = if self.agent.is_empty() {
            self.lane_name.clone()
        } else {
            self.agent.clone()
        };
        let delivered = match self.hub.send(&from, to, message.to_string()) {
            Ok(targets) => targets,
            Err(error) => return Some(Err(error)),
        };
        let inbox = drain_lane_inbox(&self.hub, &self.lane_name, &self.agent);
        Some(Ok(serde_json::json!({
            "delivered_to": delivered,
            "inbox": format_inbox(&inbox),
        })
        .to_string()))
    }
}

#[derive(Clone)]
pub(crate) struct HubToolExecutor {
    hub: SubagentHub,
    session_file: Option<PathBuf>,
    revive_hook: Option<ReviveHook>,
}

impl HubToolExecutor {
    pub(crate) fn new(hub: SubagentHub, session_file: Option<PathBuf>) -> Self {
        Self { hub, session_file, revive_hook: None }
    }

    pub(crate) fn with_revive_hook(mut self, hook: ReviveHook) -> Self {
        self.revive_hook = Some(hook);
        self
    }

    fn read_lane(&self, target: &str) -> Result<String, String> {
        let roster = self.hub.roster();
        let lane = roster
            .iter()
            .find(|lane| lane.lane_name == target || lane.agent == target)
            .ok_or_else(|| {
                let known: Vec<_> = roster
                    .iter()
                    .map(|lane| format!("{} ({})", lane.lane_name, lane.agent))
                    .collect();
                format!("unknown hub target: {target}. Live lanes: {}", known.join(", "))
            })?;
        let Some(path) = self.session_file.as_deref() else {
            return Ok(serde_json::json!({
                "lane": lane.lane_name,
                "agent": lane.agent,
                "live": lane.live,
                "note": "session persistence is unavailable; roster only",
            })
            .to_string());
        };
        let store = threadlane_runtime::harness::JsonlStore::open_read_only(path)
            .map_err(|error| error.to_string())?;
        let mut entries: Vec<_> = store
            .entries()
            .iter()
            .filter(|entry| entry.lane == lane.lane_name)
            .collect();
        entries.sort_by_key(|entry| entry.seq);
        // Latest assistant output (agent:// parity) + recent activity tail.
        let output = entries
            .iter()
            .rev()
            .filter_map(|entry| match &entry.message {
                threadlane_runtime::AgentMessage::Assistant { content: Some(content), .. }
                    if !content.trim().is_empty() =>
                {
                    Some(content.clone())
                }
                _ => None,
            })
            .next()
            .unwrap_or_default();
        let tail: Vec<String> = entries
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|entry| {
                let summary = match &entry.message {
                    threadlane_runtime::AgentMessage::Assistant { content, tool_calls, .. } => {
                        let text = content.as_deref().unwrap_or_default();
                        let text: String = text.chars().take(240).collect();
                        let tools = tool_calls
                            .as_ref()
                            .map(|calls| {
                                calls
                                    .iter()
                                    .map(|call| call.function.name.clone())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            })
                            .filter(|tools| !tools.is_empty())
                            .map(|tools| format!(" [tools: {tools}]"))
                            .unwrap_or_default();
                        format!("assistant: {text}{tools}")
                    }
                    threadlane_runtime::AgentMessage::Tool { name, content, is_error, .. } => {
                        let text: String = content.chars().take(240).collect();
                        format!("tool {name} (error={is_error}): {text}")
                    }
                    threadlane_runtime::AgentMessage::User { content } => {
                        let text: String = content.chars().take(240).collect();
                        format!("user: {text}")
                    }
                    _ => "event".to_string(),
                };
                format!("#{} {summary}", entry.seq)
            })
            .collect();
        Ok(serde_json::json!({
            "lane": lane.lane_name,
            "agent": lane.agent,
            "task": lane.task,
            "model": lane.model,
            "live": lane.live,
            "unread": lane.unread,
            "output": output,
            "recent": tail,
        })
        .to_string())
    }

    /// Revive a settled lane with a follow-up prompt, reusing the same lane
    /// so history stays continuous. Live lanes reject (use `hub send`).
    async fn revive_lane(&self, target: &str, message: &str) -> Result<String, String> {
        let lane = self.hub.resolve_lane(target).ok_or_else(|| {
            let known: Vec<_> = self
                .hub
                .roster()
                .iter()
                .map(|lane| format!("{} ({})", lane.lane_name, lane.agent))
                .collect();
            format!("unknown hub target: {target}. Lanes: {}", known.join(", "))
        })?;
        if lane.live {
            return Err(format!(
                "Lane {} is still live; use `hub send` to steer it instead of reviving.",
                lane.lane_name
            ));
        }
        let Some(hook) = self.revive_hook.clone() else {
            return Err("`hub revive` is unavailable in this session (no spawner).".into());
        };
        let message = message.trim();
        let message = if message.is_empty() {
            "Continue your assigned task; address any queued inbox notes, then report."
        } else {
            message
        };
        hook(ReviveRequest {
            lane_name: lane.lane_name,
            agent: lane.agent,
            task: lane.task,
            model: lane.model,
            message: message.to_string(),
        })
        .await
    }

    /// Flag a live lane for shutdown and record the harness abort. The child
    /// observes the flag at its next turn boundary and exits with a killed
    /// result; follow-up turns are skipped.
    fn kill_lane(&self, target: &str) -> Result<String, String> {
        let lane = self.hub.resolve_lane(target).ok_or_else(|| {
            let known: Vec<_> = self
                .hub
                .roster()
                .iter()
                .map(|lane| format!("{} ({})", lane.lane_name, lane.agent))
                .collect();
            format!("unknown hub target: {target}. Lanes: {}", known.join(", "))
        })?;
        if !lane.live {
            return Err(format!(
                "Lane {} already settled ({}); nothing to kill. Use `hub revive` to restart it.",
                lane.lane_name,
                lane.outcome.as_deref().unwrap_or("settled")
            ));
        }
        if !self.hub.flag_killed(&lane.lane_name, &lane.agent) {
            return Err(format!("Lane {} is no longer tracked.", lane.lane_name));
        }
        let _ = self.hub.send(
            "parent",
            &lane.lane_name,
            "You are being shut down via `hub kill`. Stop after your current step; do not start new work.".to_string(),
        );
        let mut abort_note = "kill flag set; the worker exits at its next turn boundary.";
        if let Some(path) = self.session_file.as_deref() {
            match super::harness::CodingSessionHarness::open(path) {
                Ok(mut journal) => {
                    if let Err(error) = journal.finish_subagent_lane(
                        &lane.lane_name,
                        &lane.run_id,
                        threadlane_runtime::harness::OperationOutcome::Aborted,
                        Some("killed via hub".into()),
                    ) {
                        abort_note = "kill flag set, but the harness abort record failed; the worker still exits at its next turn boundary.";
                        log::warn!("hub kill harness abort failed: {error}");
                    }
                }
                Err(error) => {
                    abort_note = "kill flag set, but the session journal is unavailable; the worker still exits at its next turn boundary.";
                    log::warn!("hub kill journal open failed: {error}");
                }
            }
        }
        self.hub.mark_settled(&lane.lane_name, false);
        self.hub.set_outcome(&lane.lane_name, "killed");
        Ok(format!("Killed lane {}. {abort_note}", lane.lane_name))
    }

    /// Wait for lanes to settle. Empty `sessions` waits for all live lanes.
    async fn wait_lanes(&self, sessions: &[String], timeout: Duration) -> Result<String, String> {
        let mut lane_names = Vec::new();
        if !sessions.is_empty() {
            let roster = self.hub.roster();
            for target in sessions {
                let lane = roster
                    .iter()
                    .find(|lane| lane.lane_name == *target || lane.agent == *target)
                    .ok_or_else(|| {
                        let known: Vec<_> = roster
                            .iter()
                            .map(|lane| format!("{} ({})", lane.lane_name, lane.agent))
                            .collect();
                        format!("unknown hub target: {target}. Lanes: {}", known.join(", "))
                    })?;
                if !lane_names.iter().any(|name| name == &lane.lane_name) {
                    lane_names.push(lane.lane_name.clone());
                }
            }
        }
        if lane_names.is_empty() && self.hub.roster().iter().all(|lane| !lane.live) {
            return Ok("No live lanes; nothing to wait for.".into());
        }
        let (lines, timed_out) = self.hub.wait_settled(&lane_names, timeout).await;
        let mut report = if timed_out {
            format!(
                "Timed out after {}s; still waiting on:\n{}\nCall `hub wait` again or `hub read <lane>` for partial output.",
                timeout.as_secs(),
                lines.join("\n")
            )
        } else if lines.is_empty() {
            "All watched lanes settled.".into()
        } else {
            format!(
                "Settled:\n{}\nUse `hub read <lane>` for full output.",
                lines.join("\n")
            )
        };
        if !timed_out {
            // Surface completed outputs inline would risk unbounded tool
            // output; the roster read path stays authoritative.
            let _ = &mut report;
        }
        Ok(report)
    }
}

#[async_trait::async_trait]
impl threadlane_runtime::ToolExecutor for HubToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.hub"
    }

    fn tool_definitions(&self) -> Arc<[threadlane_runtime::AgentToolDefinition]> {
        vec![threadlane_runtime::AgentToolDefinition {
            name: HUB_TOOL_NAME.into(),
            description: Some(
                "Supervise subagent lanes: `list` the roster, `send` a steering message to a live lane, `read` a lane's latest output and recent activity, `revive` a settled lane with a follow-up prompt, `kill` a live lane, or `wait` for lanes to settle.".into(),
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "send", "read", "revive", "kill", "wait"]},
                    "target": {"type": "string", "description": "Lane name or agent role for `send`/`read`/`revive`/`kill`."},
                    "message": {"type": "string", "description": "Steering message for `send`, follow-up prompt for `revive`."},
                    "sessions": {"type": "array", "items": {"type": "string"}, "description": "Lane names or agent roles for `wait`. Omit to wait for all live lanes."},
                    "timeout": {"type": "number", "description": "Max seconds for `wait` (default 30, max 300)."}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            strict: None,
        }]
        .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != HUB_TOOL_NAME {
            return None;
        }
        let parsed: serde_json::Value = match serde_json::from_str(args) {
            Ok(value) => value,
            Err(error) => return Some(Err(format!("Invalid hub arguments: {error}"))),
        };
        let action = parsed
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match action {
            "list" => {
                let roster = self.hub.roster();
                if roster.is_empty() {
                    return Some(Ok("No subagent lanes registered.".into()));
                }
                let lines: Vec<String> = roster
                    .into_iter()
                    .map(|lane| {
                        let status = if lane.live {
                            "live".to_string()
                        } else {
                            format!(
                                "settled{}",
                                lane.outcome
                                    .as_deref()
                                    .map(|outcome| format!(":{outcome}"))
                                    .unwrap_or_default()
                            )
                        };
                        let task: String = lane.task.chars().take(120).collect();
                        format!(
                            "- {} [{}] agent={} model={} unread={} run={} task={}",
                            lane.lane_name, status, lane.agent, lane.model, lane.unread, lane.run_id, task
                        )
                    })
                    .collect();
                Some(Ok(lines.join("\n")))
            }
            "send" => {
                let target = parsed
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                let message = parsed
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if target.is_empty() || message.trim().is_empty() {
                    return Some(Err("`hub send` requires non-empty `target` and `message`".into()));
                }
                // Validate against the roster so parent typos error instead of
                // queueing to a phantom inbox.
                let roster = self.hub.roster();
                let resolved = roster.iter().find(|lane| {
                    lane.lane_name == target || lane.agent == target
                });
                let Some(lane) = resolved else {
                    let known: Vec<_> = roster
                        .iter()
                        .map(|lane| format!("{} ({})", lane.lane_name, lane.agent))
                        .collect();
                    return Some(Err(format!(
                        "unknown hub target: {target}. Lanes: {}",
                        known.join(", ")
                    )));
                };
                let lane_name = lane.lane_name.clone();
                let live = lane.live;
                match self.hub.send("parent", &lane_name, message.to_string()) {
                    Ok(_) if live => Some(Ok(format!("Message queued to live lane {lane_name}."))),
                    Ok(_) => Some(Ok(format!(
                        "Lane {lane_name} already settled; message queued and will be picked up if you `hub revive` it. Use `hub read` for its output."
                    ))),
                    Err(error) => Some(Err(error)),
                }
            }
            "read" => {
                let target = parsed
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                if target.is_empty() {
                    return Some(Err("`hub read` requires `target`".into()));
                }
                Some(self.read_lane(target))
            }
            "revive" => {
                let target = parsed
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                if target.is_empty() {
                    return Some(Err("`hub revive` requires `target`".into()));
                }
                let message = parsed
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                Some(self.revive_lane(target, message).await)
            }
            "kill" => {
                let target = parsed
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                if target.is_empty() {
                    return Some(Err("`hub kill` requires `target`".into()));
                }
                Some(self.kill_lane(target))
            }
            "wait" => {
                let sessions: Vec<String> = parsed
                    .get("sessions")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let timeout_secs = parsed
                    .get("timeout")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(30.0)
                    .clamp(1.0, 300.0);
                Some(
                    self.wait_lanes(&sessions, Duration::from_secs_f64(timeout_secs))
                        .await,
                )
            }
            _ => Some(Err(
                "`hub` action must be `list`, `send`, `read`, `revive`, `kill`, or `wait`".into(),
            )),
        }
    }
}

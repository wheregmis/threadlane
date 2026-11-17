//! A fake [`HubLink`] that records every call in order and answers from
//! scripted queues, falling back to the answer of a quiet, healthy hub.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{LaneId, RunId, SessionId};
use threadlane_core::link::{Begin, HubLink, HubMessage, LinkError, RunFailure};
use threadlane_core::model::{Event, JournalEntry, Lease, MachineReport, RunOutcome};
use threadlane_core::tools::ToolError;
use tokio::sync::mpsc;

/// One call the runner made, in the order it was made.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Hello,
    Heartbeat {
        leases: Vec<Lease>,
        load: u32,
    },
    RequestWork,
    Pending {
        lane: LaneId,
    },
    BeginRun {
        lane: LaneId,
    },
    NativeSession {
        session: SessionId,
        native: String,
    },
    Append {
        seqs: Vec<i64>,
    },
    Tool {
        run: RunId,
        call_id: Option<String>,
        tool: String,
        arguments: Json,
    },
    Complete {
        run: RunId,
        final_text: Option<String>,
    },
    Fail {
        run: RunId,
        failure: RunFailure,
    },
    Release {
        lease: Lease,
    },
}

#[derive(Debug, Default)]
struct State {
    calls: Vec<Call>,
    reports: Vec<MachineReport>,
    /// Entries of every append that succeeded, in order.
    journal: Vec<JournalEntry>,
    request_work: VecDeque<Result<Option<Lease>, LinkError>>,
    pending: VecDeque<Result<Vec<Event>, LinkError>>,
    begin_run: VecDeque<Result<Begin, LinkError>>,
    append: VecDeque<Result<(), LinkError>>,
    call_tool: VecDeque<Result<Json, ToolError>>,
    complete: VecDeque<Result<RunOutcome, LinkError>>,
    fail: VecDeque<Result<(), LinkError>>,
    heartbeat: VecDeque<Result<Vec<Lease>, LinkError>>,
}

/// See the module docs. Unscripted answers: no work, nothing pending,
/// `Idle`, every write accepted, every lease renewed unchanged.
#[derive(Debug)]
pub struct FakeLink {
    state: Mutex<State>,
    nudges: mpsc::UnboundedSender<HubMessage>,
    messages: tokio::sync::Mutex<mpsc::UnboundedReceiver<HubMessage>>,
}

impl Default for FakeLink {
    fn default() -> Self {
        let (nudges, messages) = mpsc::unbounded_channel();
        Self {
            state: Mutex::default(),
            nudges,
            messages: tokio::sync::Mutex::new(messages),
        }
    }
}

impl FakeLink {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake link lock poisoned")
    }

    fn record(&self, call: Call) {
        self.state().calls.push(call);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    pub fn reports(&self) -> Vec<MachineReport> {
        self.state().reports.clone()
    }

    pub fn journal(&self) -> Vec<JournalEntry> {
        self.state().journal.clone()
    }

    pub fn count(&self, matches: impl Fn(&Call) -> bool) -> usize {
        self.state()
            .calls
            .iter()
            .filter(|call| matches(call))
            .count()
    }

    pub fn grant(&self, lease: Lease) {
        self.push_request_work(Ok(Some(lease)));
    }

    pub fn push_request_work(&self, answer: Result<Option<Lease>, LinkError>) {
        self.state().request_work.push_back(answer);
    }

    pub fn push_pending(&self, answer: Result<Vec<Event>, LinkError>) {
        self.state().pending.push_back(answer);
    }

    pub fn push_begin(&self, answer: Result<Begin, LinkError>) {
        self.state().begin_run.push_back(answer);
    }

    pub fn push_fail(&self, answer: Result<(), LinkError>) {
        self.state().fail.push_back(answer);
    }

    pub fn push_append(&self, answer: Result<(), LinkError>) {
        self.state().append.push_back(answer);
    }

    pub fn push_tool(&self, answer: Result<Json, ToolError>) {
        self.state().call_tool.push_back(answer);
    }

    pub fn push_complete(&self, answer: Result<RunOutcome, LinkError>) {
        self.state().complete.push_back(answer);
    }

    pub fn push_heartbeat(&self, answer: Result<Vec<Lease>, LinkError>) {
        self.state().heartbeat.push_back(answer);
    }

    pub fn nudge(&self) {
        self.nudges
            .send(HubMessage::Nudge)
            .expect("the fake link holds its own receiver");
    }
}

#[async_trait]
impl HubLink for FakeLink {
    async fn hello(&self, report: &MachineReport) -> Result<(), LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Hello);
        state.reports.push(report.clone());
        Ok(())
    }

    async fn heartbeat(
        &self,
        report: &MachineReport,
        leases: &[Lease],
    ) -> Result<Vec<Lease>, LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Heartbeat {
            leases: leases.to_vec(),
            load: report.load,
        });
        state.reports.push(report.clone());
        state
            .heartbeat
            .pop_front()
            .unwrap_or_else(|| Ok(leases.to_vec()))
    }

    async fn request_work(&self) -> Result<Option<Lease>, LinkError> {
        let mut state = self.state();
        state.calls.push(Call::RequestWork);
        state.request_work.pop_front().unwrap_or(Ok(None))
    }

    async fn pending(&self, lease: &Lease) -> Result<Vec<Event>, LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Pending {
            lane: lease.lane_id,
        });
        state.pending.pop_front().unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn begin_run(&self, lease: &Lease) -> Result<Begin, LinkError> {
        let mut state = self.state();
        state.calls.push(Call::BeginRun {
            lane: lease.lane_id,
        });
        state.begin_run.pop_front().unwrap_or(Ok(Begin::Idle))
    }

    async fn native_session(
        &self,
        _lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> Result<(), LinkError> {
        self.record(Call::NativeSession {
            session: session_id,
            native: native_session_id.to_owned(),
        });
        Ok(())
    }

    async fn append(&self, _lease: &Lease, entries: &[JournalEntry]) -> Result<(), LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Append {
            seqs: entries.iter().map(|entry| entry.seq).collect(),
        });
        let answer = state.append.pop_front().unwrap_or(Ok(()));
        if answer.is_ok() {
            state.journal.extend_from_slice(entries);
        }
        answer
    }

    async fn call_tool(
        &self,
        _lease: &Lease,
        run_id: RunId,
        call_id: Option<&str>,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let mut state = self.state();
        state.calls.push(Call::Tool {
            run: run_id,
            call_id: call_id.map(str::to_owned),
            tool: tool.to_owned(),
            arguments: arguments.clone(),
        });
        state
            .call_tool
            .pop_front()
            .unwrap_or_else(|| Ok(json!({ "ok": true })))
    }

    async fn complete(
        &self,
        _lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Complete {
            run: run_id,
            final_text: final_text.map(str::to_owned),
        });
        state.complete.pop_front().unwrap_or(Ok(RunOutcome::Silent))
    }

    async fn fail(
        &self,
        _lease: &Lease,
        run_id: RunId,
        failure: &RunFailure,
    ) -> Result<(), LinkError> {
        let mut state = self.state();
        state.calls.push(Call::Fail {
            run: run_id,
            failure: failure.clone(),
        });
        state.fail.pop_front().unwrap_or(Ok(()))
    }

    async fn release(&self, lease: &Lease) -> Result<(), LinkError> {
        self.record(Call::Release { lease: *lease });
        Ok(())
    }

    async fn next_message(&self) -> Option<HubMessage> {
        self.messages.lock().await.recv().await
    }
}

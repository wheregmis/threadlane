//! Device-local completion attempts, independent of presented-result acknowledgment.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};
use threadlane_protocol::daemon::{RunCompletionToken, SessionCompletionSummary, SessionInfo};

const MAX_TRACKED: usize = 4096;
const MAX_PENDING: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompletionAlertIdentity {
    pub project: PathBuf,
    pub session_id: String,
    pub session_file: PathBuf,
}
impl CompletionAlertIdentity {
    pub fn new(project: &Path, session: &SessionInfo) -> Self {
        Self {
            project: project.into(),
            session_id: session.id.clone(),
            session_file: session.session_file.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionAlertIntent {
    pub identity: CompletionAlertIdentity,
    pub token: RunCompletionToken,
    pub generation: u64,
}

#[derive(Default)]
struct Baseline {
    registered: bool,
    confirmed: bool,
    token: Option<RunCompletionToken>,
}

#[derive(Default)]
pub struct CompletionAlerts {
    enabled: bool,
    generation: u64,
    tracked: HashMap<CompletionAlertIdentity, Baseline>,
    pending: VecDeque<CompletionAlertIntent>,
}
impl CompletionAlerts {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.tracked.clear();
        self.pending.clear();
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.reset();
        }
        self.enabled = enabled;
    }

    /// Only newly created interactive files are registered with an empty baseline.
    pub fn register(&mut self, identity: CompletionAlertIdentity) {
        self.baseline_interactive(identity, &SessionCompletionSummary::None);
    }

    /// A fork's inherited completion is history, not a newly finished run.
    pub fn baseline_interactive(
        &mut self,
        identity: CompletionAlertIdentity,
        summary: &SessionCompletionSummary,
    ) {
        if self.enabled && self.tracked.len() < MAX_TRACKED {
            self.tracked.entry(identity).or_insert(Baseline {
                registered: true,
                confirmed: *summary != SessionCompletionSummary::Unknown,
                token: match summary {
                    SessionCompletionSummary::Latest(token) => Some(token.clone()),
                    _ => None,
                },
            });
        }
    }

    pub fn observe(
        &mut self,
        project: &Path,
        sessions: &[SessionInfo],
        automation_files: &HashSet<PathBuf>,
    ) {
        if !self.enabled {
            return;
        }
        let inventory: HashSet<_> = sessions
            .iter()
            .map(|session| CompletionAlertIdentity::new(project, session))
            .collect();
        self.tracked.retain(|key, baseline| {
            key.project != project
                || ((baseline.registered || inventory.contains(key))
                    && !automation_files.contains(&key.session_file))
        });
        self.pending.retain(|intent| {
            intent.identity.project != project
                || (inventory.contains(&intent.identity)
                    && !automation_files.contains(&intent.identity.session_file))
        });
        for session in sessions {
            if automation_files.contains(&session.session_file) {
                continue;
            }
            let identity = CompletionAlertIdentity::new(project, session);
            if !self.tracked.contains_key(&identity) && self.tracked.len() >= MAX_TRACKED {
                continue;
            }
            let baseline = self.tracked.entry(identity.clone()).or_default();
            baseline.registered = false;
            let token = match &session.completion_summary {
                SessionCompletionSummary::Unknown => continue,
                SessionCompletionSummary::None => {
                    baseline.confirmed = true;
                    continue;
                }
                SessionCompletionSummary::Latest(token) => token,
            };
            if baseline.token.as_ref().is_some_and(|previous| {
                token.seq <= previous.seq
                    || token.record_id == previous.record_id
                    || token.run_id == previous.run_id
            }) {
                continue;
            }
            let emit = baseline.confirmed;
            baseline.confirmed = true;
            baseline.token = Some(token.clone());
            if emit {
                self.pending.retain(|intent| intent.identity != identity);
                if self.pending.len() == MAX_PENDING {
                    self.pending.pop_front();
                }
                self.pending.push_back(CompletionAlertIntent {
                    identity,
                    token: token.clone(),
                    generation: self.generation,
                });
            }
        }
    }

    pub fn retain_projects(&mut self, projects: &HashSet<PathBuf>) {
        self.tracked
            .retain(|key, _| projects.contains(&key.project));
        self.pending
            .retain(|intent| projects.contains(&intent.identity.project));
    }

    pub fn drain(&mut self) -> Vec<CompletionAlertIntent> {
        self.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{CompletionAlertIdentity, CompletionAlerts, MAX_PENDING, MAX_TRACKED};
    use std::{collections::HashSet, path::Path};
    use threadlane_protocol::daemon::{
        RunCompletionToken, SessionCompletionSummary as Summary, SessionInfo,
    };

    fn session(project: &str, id: &str, summary: Summary) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            work_dir: project.into(),
            session_file: Path::new(project).join(format!("{id}.jsonl")),
            completion_summary: summary,
            ..Default::default()
        }
    }
    fn latest(seq: u64) -> Summary {
        Summary::Latest(RunCompletionToken {
            seq,
            run_id: format!("run-{seq}"),
            record_id: format!("record-{seq}"),
        })
    }
    fn observe(alerts: &mut CompletionAlerts, sessions: &[SessionInfo]) {
        alerts.observe(Path::new("/a"), sessions, &HashSet::new());
    }
    fn enabled() -> CompletionAlerts {
        let mut alerts = CompletionAlerts::default();
        alerts.set_enabled(true);
        alerts
    }
    #[test]
    fn completion_alerts_default_off_and_history_never_replays() {
        let mut alerts = CompletionAlerts::default();
        let s = session("/a", "one", latest(5));
        observe(&mut alerts, std::slice::from_ref(&s));
        assert!(alerts.tracked.is_empty());
        alerts.set_enabled(true);
        observe(&mut alerts, &[s]);
        assert!(alerts.drain().is_empty());
        observe(&mut alerts, &[session("/a", "one", latest(6))]);
        assert_eq!(alerts.drain().len(), 1);
    }
    #[test]
    fn completion_alerts_unknown_waits_for_confirmed_baseline() {
        for first in [Summary::Unknown, Summary::None, latest(8)] {
            let mut alerts = enabled();
            observe(&mut alerts, &[session("/a", "one", first.clone())]);
            assert!(alerts.drain().is_empty());
            observe(&mut alerts, &[session("/a", "one", latest(9))]);
            assert_eq!(alerts.drain().len(), usize::from(first != Summary::Unknown));
            observe(&mut alerts, &[session("/a", "one", Summary::Unknown)]);
            observe(&mut alerts, &[session("/a", "one", latest(10))]);
            assert_eq!(alerts.drain().len(), 1);
        }
    }
    #[test]
    fn completion_alerts_repeated_backward_and_malformed_tokens_stay_quiet() {
        let mut alerts = enabled();
        observe(&mut alerts, &[session("/a", "one", latest(3))]);
        for summary in [
            latest(3),
            latest(2),
            Summary::Unknown,
            Summary::None,
            Summary::Latest(RunCompletionToken {
                seq: 4,
                run_id: "run-3".into(),
                record_id: "other".into(),
            }),
            Summary::Latest(RunCompletionToken {
                seq: 4,
                run_id: "other".into(),
                record_id: "record-3".into(),
            }),
        ] {
            observe(&mut alerts, &[session("/a", "one", summary)]);
            assert!(alerts.drain().is_empty());
        }
        observe(&mut alerts, &[session("/a", "one", latest(4))]);
        assert_eq!(alerts.drain().len(), 1);
        assert!(alerts.drain().is_empty());
    }
    #[test]
    fn completion_alerts_new_interactive_session_survives_precreation_inventory() {
        let mut alerts = enabled();
        let s = session("/a", "new", latest(1));
        alerts.register(CompletionAlertIdentity::new(Path::new("/a"), &s));
        observe(&mut alerts, &[]);
        observe(&mut alerts, &[s.clone()]);
        assert_eq!(alerts.drain()[0].identity.session_file, s.session_file);
        observe(&mut alerts, &[]);
        assert!(alerts.tracked.is_empty());
        observe(&mut alerts, &[s]);
        assert!(alerts.drain().is_empty());
    }
    #[test]
    fn completion_alerts_same_id_in_two_projects_is_distinct() {
        let mut alerts = enabled();
        for project in ["/a", "/b"] {
            let s = session(project, "same", Summary::None);
            alerts.observe(Path::new(project), &[s], &HashSet::new());
        }
        for project in ["/a", "/b"] {
            alerts.observe(
                Path::new(project),
                &[session(project, "same", latest(1))],
                &HashSet::new(),
            );
        }
        let intents = alerts.drain();
        assert_eq!(intents.len(), 2);
        assert_ne!(intents[0].identity, intents[1].identity);
    }
    #[test]
    fn completion_alerts_reset_disable_and_reenable_drop_pending_and_rebaseline() {
        let mut alerts = enabled();
        let old_generation = alerts.generation();
        observe(&mut alerts, &[session("/a", "one", Summary::None)]);
        observe(&mut alerts, &[session("/a", "one", latest(1))]);
        alerts.reset();
        assert!(alerts.drain().is_empty());
        assert_ne!(alerts.generation(), old_generation);
        observe(&mut alerts, &[session("/a", "one", latest(2))]);
        assert!(alerts.drain().is_empty());
        alerts.set_enabled(false);
        observe(&mut alerts, &[session("/a", "one", latest(3))]);
        alerts.set_enabled(true);
        observe(&mut alerts, &[session("/a", "one", latest(3))]);
        assert!(alerts.drain().is_empty());
        observe(&mut alerts, &[session("/a", "one", latest(4))]);
        assert_eq!(alerts.drain().len(), 1);
    }
    #[test]
    fn completion_alerts_rapid_completions_coalesce_and_automation_is_excluded() {
        let mut alerts = enabled();
        observe(&mut alerts, &[session("/a", "one", Summary::None)]);
        for seq in 1..20 {
            observe(&mut alerts, &[session("/a", "one", latest(seq))]);
        }
        assert_eq!(alerts.drain()[0].token.seq, 19);
        observe(&mut alerts, &[session("/a", "one", latest(20))]);
        let s = session("/a", "one", latest(21));
        alerts.observe(
            Path::new("/a"),
            &[s.clone()],
            &HashSet::from([s.session_file]),
        );
        assert!(alerts.drain().is_empty());
        assert!(alerts.tracked.is_empty());
    }
    #[test]
    fn completion_alerts_removed_projects_and_bounded_inventory_cleanup() {
        let mut alerts = enabled();
        let sessions: Vec<_> = (0..MAX_TRACKED + 100)
            .map(|i| session("/a", &i.to_string(), Summary::None))
            .collect();
        observe(&mut alerts, &sessions);
        assert_eq!(alerts.tracked.len(), MAX_TRACKED);
        let completed: Vec<_> = sessions
            .into_iter()
            .map(|mut s| {
                s.completion_summary = latest(1);
                s
            })
            .collect();
        observe(&mut alerts, &completed);
        assert_eq!(alerts.pending.len(), MAX_PENDING);
        alerts.retain_projects(&HashSet::new());
        assert!(alerts.tracked.is_empty());
        assert!(alerts.drain().is_empty());
    }
}

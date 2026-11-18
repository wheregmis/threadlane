//! First-send worktree preparation, off the UI thread.
use std::time::Duration;

use threadlane_runtime::harness::{JsonlStore, SessionStore};

use crate::{SessionEvent, SessionInfo};

// `SetupStage`/`WorktreeSetup` are daemon wire types, canonical in
// `threadlane_protocol::daemon` (also the `PrepareWorktree` command payload
// and the durable `worktree_setup` fact shape); re-exported here so
// `worktree_setup::SetupStage` paths keep working.
pub use threadlane_protocol::daemon::{SetupStage, WorktreeSetup};

/// Model output is untrusted: allow only a bounded ASCII slug and add a unique suffix.
pub fn branch_name(raw: &str, session_id: &str) -> String {
    let slug = raw
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase()
        .chars()
        .take(48)
        .collect::<String>();
    let slug = slug.trim_matches('-');
    let suffix = session_id.strip_prefix("session_").unwrap_or(session_id);
    format!(
        "worktree/{}-{suffix}",
        if slug.is_empty() { "task" } else { slug }
    )
}

pub fn start(
    core: std::sync::Arc<crate::core::DaemonCore>,
    setup: WorktreeSetup,
    mut options: threadlane_coding_agent::CodingAgentOptions,
    tx: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
) -> Result<(), String> {
    crate::chat::executor()?.spawn(async move {
        let progress = |stage, branch| {
            let _ = tx.send(SessionEvent::WorktreeProgress {
                session_id: setup.session_id.clone(),
                stage,
                branch,
            });
        };
        let ensure_active = || {
            if setup.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                Err("Worktree setup cancelled".to_string())
            } else {
                Ok(())
            }
        };
        let result = async {
            ensure_active()?;
            // A retry after runtime construction failed reuses the recorded checkout.
            if !setup.worktree.exists() {
                let model = options.model.clone();
                let prompt = format!(
                    "Name this coding task in 3 to 6 words. Return only the name. Task:\n{}",
                    setup.text.chars().take(4_000).collect::<String>()
                );
                let generated = tokio::time::timeout(Duration::from_secs(20), async {
                    if let Some(agent) = threadlane_acp_engine::acp_agent_id(&model) {
                        threadlane_acp_engine::generate_title(
                            threadlane_project::default_global_threadlane_dir(),
                            setup.project.clone(),
                            agent,
                            &prompt,
                        )
                        .await
                    } else {
                        let (key, account) =
                            threadlane_coding_agent::credentials::provider_credentials(&model);
                        threadlane_coding_agent::credentials::provider_client_for(key, account)
                            .generate_title(&model, &prompt)
                            .await
                    }
                })
                .await
                .ok()
                .and_then(Result::ok);
                let branch = branch_name(
                    generated.as_deref().unwrap_or(&setup.text),
                    &setup.session_id,
                );
                ensure_active()?;
                progress(SetupStage::Creating, Some(branch.clone()));
                let request = setup.clone();
                tokio::task::spawn_blocking(move || {
                    threadlane_git::create_worktree_from(
                        &request.project,
                        &request.worktree,
                        &branch,
                        &request.base,
                    )
                    .map_err(|e| e.to_string())?;
                    let commit = threadlane_git::list_commits(&request.worktree, 1)
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .next()
                        .ok_or("Prepared worktree has no commit")?
                        .sha;
                    for (key, value) in [
                        ("git_branch", branch),
                        ("worktree_base", request.base),
                        ("worktree_setup_commit", commit),
                    ] {
                        threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
                            &request.session_file, "main", key, &value, None,
                        ).map_err(|e| e.to_string())?;
                    }
                    Ok::<_, String>(())
                })
                .await
                .map_err(|e| e.to_string())??;
            }
            let store = JsonlStore::open(&setup.session_file).map_err(|e| e.to_string())?;
            let branch =
                threadlane_git::current_branch(&setup.worktree).map_err(|e| e.to_string())?;
            if branch.as_deref() != store.facts().get("git_branch").map(String::as_str)
                || branch.is_none()
            {
                return Err("The prepared checkout changed; inspect it before continuing".into());
            }
            ensure_active()?;
            progress(SetupStage::Starting, branch);
            options.work_dir = setup.worktree.clone();
            options.session_file = Some(setup.session_file.clone());
            let runtime = core.clone().get_or_create_runtime_async(
                setup.session_id.clone(),
                setup.worktree.clone(),
                setup.session_file.clone(),
                options,
                None,
            )
            .await;
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => return Err(error.to_string()),
            };
            if let Err(error) = ensure_active() {
                core.release_cancelled_runtime(&setup.session_file, &runtime);
                return Err(error);
            }
            let project = setup.project.clone();
            let id = setup.session_id.clone();
            let session = tokio::task::spawn_blocking(move || {
                crate::discovery::discover_sessions_in_project(&project)
                    .into_iter()
                    .find(|s| s.id == id)
                    .ok_or_else(|| "Could not reload the prepared session".to_string())
            })
            .await
            .map_err(|e| e.to_string());
            if let Err(error) = ensure_active() {
                core.release_cancelled_runtime(&setup.session_file, &runtime);
                return Err(error);
            }
            let session = match session {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    core.release_cancelled_runtime(&setup.session_file, &runtime);
                    return Err(error);
                }
                Err(error) => {
                    core.release_cancelled_runtime(&setup.session_file, &runtime);
                    return Err(error);
                }
            };
            // The event stream stays wire-clean: the runtime handle waits in
            // the daemon-side mailbox until the consumer claims it.
            if !crate::runtimes::park_prepared_runtime_if_active(
                setup.session_id.clone(),
                runtime.clone(),
                &setup.cancelled,
            ) {
                core.release_cancelled_runtime(&setup.session_file, &runtime);
                return Err("Worktree setup cancelled".into());
            }
            if let Err(error) = ensure_active() {
                let _ = crate::runtimes::cancel_prepared_runtime(
                    &setup.session_id,
                    &setup.cancelled,
                );
                core.release_cancelled_runtime(&setup.session_file, &runtime);
                return Err(error);
            }
            Ok(session)
        }
        .await;
        let _ = tx.send(SessionEvent::WorktreePrepared {
            session_id: setup.session_id,
            result,
        });
    });
    Ok(())
}

/// Save the unsent request with the session so an interrupted setup retains the user's input.
pub fn persist_request(setup: &WorktreeSetup) -> Result<(), String> {
    for (key, value) in [
        ("is_worktree", "true".to_string()),
        (
            "worktree_path",
            setup.worktree.to_string_lossy().into_owned(),
        ),
        ("reasoning_effort", setup.effort.label().to_string()),
        (
            "worktree_setup",
            serde_json::to_string(setup).map_err(|e| e.to_string())?,
        ),
    ] {
        threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
            &setup.session_file,
            "main",
            key,
            &value,
            None,
        )
        .map_err(|e| e.to_string())?;
    }
    let mut store = JsonlStore::open(&setup.session_file).map_err(|e| e.to_string())?;
    store.set_model(&setup.model).map_err(|e| e.to_string())?;
    let title = threadlane_runtime::titles::normalize_session_title(&setup.text);
    store
        .set_name(if title.is_empty() {
            "New worktree task"
        } else {
            &title
        })
        .map_err(|e| e.to_string())
}

/// Recover an interrupted first-send without automatically replaying any accepted turn.
pub fn recover(session: &SessionInfo) -> Option<WorktreeSetup> {
    let store = JsonlStore::open_read_only(&session.session_file).ok()?;
    if !store.entries().is_empty() {
        return None;
    }
    let facts = store.facts();
    let mut setup: WorktreeSetup = serde_json::from_str(facts.get("worktree_setup")?).ok()?;
    if setup.session_id != session.id
        || setup.worktree
            != setup
                .project
                .join(".threadlane/worktrees")
                .join(&setup.session_id)
        || std::fs::canonicalize(&setup.project).ok().as_ref() != Some(&session.work_dir)
        || std::fs::canonicalize(&setup.session_file).ok().as_ref() != Some(&session.session_file)
    {
        return None;
    }
    setup.project = session.work_dir.clone();
    setup.session_file = session.session_file.clone();
    setup.worktree = session.runtime_work_dir.clone();
    setup.error =
        Some("Setup was interrupted. Retry to continue, or cancel to recover your message.".into());
    Some(setup)
}

/// Only discard an unstarted session and the unchanged checkout recorded by its creator.
pub fn cleanup_cancelled(setup: &WorktreeSetup) -> Result<(), String> {
    let store = JsonlStore::open_read_only(&setup.session_file).map_err(|e| e.to_string())?;
    if !store.entries().is_empty() {
        return Err("The session has recorded work; keep it for manual cleanup".into());
    }
    let facts = store.facts();
    let branch = facts.get("git_branch");
    if setup.worktree.exists() {
        let current = threadlane_git::current_branch(&setup.worktree).map_err(|e| e.to_string())?;
        if branch.is_none() || current.as_ref() != branch {
            return Err(
                "The checkout is not owned by this setup; keep it for manual cleanup".into(),
            );
        }
        let commit = threadlane_git::list_commits(&setup.worktree, 1).map_err(|e| e.to_string())?;
        if facts.get("worktree_setup_commit") != commit.first().map(|c| &c.sha) {
            return Err("The checkout has changed; keep it for manual cleanup".into());
        }
        let files = threadlane_git::inspect_files(&setup.worktree).map_err(|e| e.to_string())?;
        let dirty: Vec<_> = files
            .iter()
            .filter(|file| !(file.is_untracked() && file.path.starts_with(".threadlane/")))
            .take(3)
            .map(|file| file.path.as_str())
            .collect();
        if !dirty.is_empty() {
            return Err(format!(
                "The checkout contains changes ({}); keep it for manual cleanup",
                dirty.join(", ")
            ));
        }
        threadlane_git::remove_worktree(&setup.project, &setup.worktree, true)
            .map_err(|e| e.to_string())?;
        threadlane_tools::remove_worktree_cargo_target_dir(&setup.worktree);
        // The branch still points at the original setup commit, verified above.
        threadlane_git::delete_branch(&setup.project, branch.unwrap(), true)
            .map_err(|e| e.to_string())?;
    }
    drop(store);
    std::fs::remove_file(&setup.session_file).map_err(|e| e.to_string())
}

pub fn clear_request(setup: &WorktreeSetup) {
    if let Err(error) = threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
        &setup.session_file,
        "main",
        "worktree_setup",
        "",
        None,
    ) {
        tracing::warn!("Could not clear worktree setup request: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::branch_name;
    #[test]
    fn generated_worktree_name_is_bounded_and_cannot_inject_git_options() {
        assert_eq!(
            branch_name("Fix login expiry", "session_123"),
            "worktree/fix-login-expiry-123"
        );
        assert_eq!(
            branch_name("../ --force / 🦀", "session_123"),
            "worktree/force-123"
        );
        assert_eq!(branch_name("🦀", "session_123"), "worktree/task-123");
        assert!(branch_name(&"a".repeat(500), "session_123").len() < 70);
    }
}

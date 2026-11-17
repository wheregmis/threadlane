use super::*;

impl CodingSessionHarness {
    /// Recover conversation context into a new journal, never copying pending
    /// operations, provider continuation handles, queues, or tool-call protocol.
    /// The original remains the authoritative, unmodified diagnostic transcript.
    pub fn fork_to_path(source: &Path, destination: &Path) -> Result<(), String> {
        // Recovery bypasses reduction but retains durable entry identities.
        fs::metadata(source)
            .map_err(|error| format!("Could not read the source session: {error}"))?;
        let source_store = JsonlStore::open_read_only(source).ok();
        let source_id = source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("session");
        let title = source_store
            .as_ref()
            .map(|store| threadlane_runtime::titles::extract_session_title(store, source_id))
            .unwrap_or_else(|| source_id.to_owned());
        let facts = source_store
            .as_ref()
            .map(SessionStore::facts)
            .unwrap_or_default();
        let messages = if let Some(context) = source_store
            .as_ref()
            .and_then(|store| store.model_context("main").ok())
        {
            context.into_messages()
        } else {
            JsonlStore::recover_main_entries(source)
                .map_err(|error| format!("Could not recover the source transcript: {error}"))?
                .into_iter()
                .map(|entry| entry.message)
                .collect()
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        // Reserve exclusively: even a generated id must never overwrite history.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| format!("Could not create the fork: {error}"))?;
        let result = (|| {
            let mut fork = Self::open(destination)?;
            for key in [
                "model",
                "reasoning_effort",
                "orchestrator_mode",
                "github_issue",
                "git_branch",
            ] {
                if let Some(value) = facts.get(key) {
                    fork.set_fact("main", key, value.clone())?;
                }
            }
            fork.set_fact("main", "forked_from", source_id.to_owned())?;
            fork.set_fact("main", "name", format!("{title} (fork)"))?;
            let mut actions = Vec::with_capacity(messages.len());
            let mut parent_id = None;
            let first_seq = fork.store.store().next_sequence();
            for message in messages {
                let recovered = match message {
                    message @ (AgentMessage::User { .. } | AgentMessage::UserWithImages { .. }) => {
                        message
                    }
                    AgentMessage::Assistant {
                        content,
                        tool_calls,
                        ..
                    } => {
                        let mut text = content.unwrap_or_default();
                        for call in tool_calls.unwrap_or_default() {
                            text.push_str(&format!(
                                "\n[Previous tool call: {}({})]",
                                call.function.name, call.function.arguments
                            ));
                        }
                        if text.is_empty() {
                            continue;
                        }
                        AgentMessage::Assistant {
                            content: Some(text),
                            tool_calls: None,
                            stop_reason: None,
                            deferred_handle: None,
                        }
                    }
                    AgentMessage::Tool { name, content, .. } => AgentMessage::Assistant {
                        content: Some(format!("[Previous tool result: {name}]\n{content}")),
                        tool_calls: None,
                        stop_reason: None,
                        deferred_handle: None,
                    },
                    message @ AgentMessage::Custom { .. } => {
                        let Some(summary) =
                            threadlane_compaction::compaction_summary_text(&message)
                        else {
                            continue;
                        };
                        AgentMessage::Assistant {
                            content: Some(summary.to_owned()),
                            tool_calls: None,
                            stop_reason: None,
                            deferred_handle: None,
                        }
                    }
                    AgentMessage::System { .. } => continue,
                };
                let seq = first_seq + actions.len() as u64;
                let id = format!("fork-entry-{seq}");
                actions.push(threadlane_runtime::harness::EffectAction::AppendEntry {
                    entry: HarnessEntry {
                        id: id.clone(),
                        parent_id,
                        lane: "main".into(),
                        seq,
                        timestamp: timestamp(),
                        message: recovered,
                        surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                        terminate: false,
                    },
                });
                parent_id = Some(id);
            }
            fork.store
                .store_mut()
                .append_actions_atomically(&actions)
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            if let Err(error) = fs::remove_file(destination) {
                tracing::warn!(
                    "Failed to remove incomplete fork {}: {error}",
                    destination.display()
                );
            }
        }
        result
    }
}

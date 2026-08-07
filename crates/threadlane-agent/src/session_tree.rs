use crate::types::{AgentMessage, PlanItem, SessionPlan};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    let temp: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    // Unlike std::fs::rename, MoveFileExW can replace an existing destination
    // on Windows. The replacement remains a same-volume rename and the write
    // through flag asks Windows to flush the move before returning.
    if unsafe {
        MoveFileExW(
            temp.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, destination)
}

pub(crate) fn session_file_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub message: AgentMessage,
}

/// One line of a session file.
///
/// Loading previously tried `SessionRecord` and then `SessionNode`, which
/// parsed the JSON *text* of every node line twice — and node lines are the
/// overwhelming majority. `untagged` buffers the parse once and matches the
/// variants against that buffer instead.
///
/// Order matters: `SessionRecord` is internally tagged and cannot match a node
/// line, which carries no `type`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SessionLine {
    Record(SessionRecord),
    Node(SessionNode),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum SessionRecord {
    #[serde(rename = "session_metadata")]
    Metadata {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        name: Option<String>,
        #[serde(default)]
        title_attempted: bool,
        #[serde(default)]
        active_node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
    },
    #[serde(rename = "session_plan")]
    Plan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
        #[serde(default)]
        items: Vec<PlanItem>,
    },
    #[serde(rename = "global_fact")]
    GlobalFact { key: String, value: String },
}

#[derive(Debug, Clone, Default)]
pub struct SessionTree {
    pub session_id: String,
    pub name: Option<String>,
    title_attempted: bool,
    pub model: Option<String>,
    pub parent_session_id: Option<String>,
    plan: SessionPlan,
    pub global_facts: HashMap<String, String>,
    pub nodes: HashMap<String, SessionNode>,
    /// Node IDs in persisted/insertion order. This is intentionally separate
    /// from `nodes`: the map is only an index and does not define ordering.
    node_order: Vec<String>,
    active_node_id: Option<String>,
    pub file_path: Option<PathBuf>,
    /// Whether a session metadata record was present on disk. Legacy files
    /// have no metadata and retain their historical all-branches lookup rules.
    pub metadata_present: bool,
    /// V2 harness lines are retained verbatim so legacy metadata rewrites do
    /// not discard records that the legacy tree does not model.
    v2_lines: Vec<String>,
    v2_entry_ids: HashSet<String>,
}

impl SessionTree {
    fn next_persisted_seq(&self) -> u64 {
        let mut max_seq = self
            .node_order
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                self.nodes
                    .get(id)
                    .map(|node| node.seq.unwrap_or(index as u64 + 1))
            })
            .max()
            .unwrap_or(0);
        if let Some(path) = &self.file_path {
            for suffix in ["harness.jsonl", "oplog.jsonl"] {
                let sidecar = path.with_extension(suffix);
                if let Ok(contents) = std::fs::read_to_string(sidecar) {
                    for line in contents.lines() {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                            let seq = value
                                .get("seq")
                                .and_then(serde_json::Value::as_u64)
                                .or_else(|| {
                                    value
                                        .as_object()
                                        .and_then(|object| object.values().next())
                                        .and_then(|record| record.get("seq"))
                                        .and_then(serde_json::Value::as_u64)
                                })
                                .unwrap_or(0);
                            max_seq = max_seq.max(seq);
                        }
                    }
                }
            }
        }
        max_seq + 1
    }

    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            name: None,
            title_attempted: false,
            model: None,
            parent_session_id: None,
            plan: SessionPlan::default(),
            global_facts: HashMap::new(),
            nodes: HashMap::new(),
            node_order: Vec::new(),
            active_node_id: None,
            file_path: None,
            metadata_present: false,
            v2_lines: Vec::new(),
            v2_entry_ids: HashSet::new(),
        }
    }

    pub fn has_name(&self) -> bool {
        self.name.as_ref().is_some_and(|name| !name.is_empty())
    }

    pub fn active_node_id(&self) -> Option<&str> {
        self.active_node_id.as_deref()
    }

    pub fn get_fact(&self, key: &str) -> Option<&str> {
        self.global_facts.get(key).map(|s| s.as_str())
    }

    pub fn set_fact(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> std::io::Result<()> {
        let key = key.into();
        let value = value.into();

        if let Some(ref path) = self.file_path {
            let _guard = session_file_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            let record = SessionRecord::GlobalFact {
                key: key.clone(),
                value: value.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&record)?)?;
        }

        self.global_facts.insert(key, value);
        Ok(())
    }

    pub fn set_fact_in_memory(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.global_facts.insert(key.into(), value.into());
    }

    pub fn plan(&self) -> &SessionPlan {
        &self.plan
    }

    #[cfg(test)]
    fn replace_plan(&mut self, plan: SessionPlan) -> std::io::Result<()> {
        if let Some(path) = &self.file_path {
            Self::append_plan_to_file(path, &plan)?;
        }
        self.plan = plan;
        Ok(())
    }

    pub fn append_plan_to_file(path: &Path, plan: &SessionPlan) -> std::io::Result<()> {
        let _guard = session_file_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let record = SessionRecord::Plan {
            explanation: plan.explanation.clone(),
            items: plan.items.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }

    pub fn set_name(&mut self, name: String) -> std::io::Result<()> {
        if name.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session name cannot be empty",
            ));
        }

        let previous_name = self.name.clone();
        self.name = Some(name.clone());
        if let Some(path) = self.file_path.clone() {
            // Reload while holding the same process-wide lock used by node
            // appends. This closes the read/replace window: nodes appended by
            // the normal agent turn are included in the title rewrite.
            let _guard = session_file_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut latest = match Self::load_from_file(&path) {
                Ok(tree) => tree,
                Err(error) => {
                    self.name = previous_name;
                    return Err(error);
                }
            };
            latest.name = Some(name);
            let result = latest.save_transactionally(&path);
            if result.is_ok() {
                *self = latest;
            } else {
                self.name = previous_name;
            }
            result
        } else {
            Ok(())
        }
    }

    pub fn set_model(&mut self, model: String) -> std::io::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session model cannot be empty",
            ));
        }

        let model = model.to_string();
        let previous_model = self.model.clone();
        self.model = Some(model.clone());
        let Some(path) = self.file_path.clone() else {
            return Ok(());
        };

        let _guard = session_file_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut latest = if path.exists() {
            match Self::load_from_file(&path) {
                Ok(tree) => tree,
                Err(error) => {
                    self.model = previous_model;
                    return Err(error);
                }
            }
        } else {
            self.clone()
        };
        latest.model = Some(model);
        latest.metadata_present = true;
        let result = latest.save_transactionally(&path);
        if result.is_ok() {
            *self = latest;
        } else {
            self.model = previous_model;
        }
        result
    }

    /// Update the compatibility view when V2 persists the model as a lane
    /// fact. This deliberately does not rewrite the session file.
    pub fn set_model_in_memory(&mut self, model: String) -> std::io::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session model cannot be empty",
            ));
        }
        self.model = Some(model.to_owned());
        self.metadata_present = true;
        Ok(())
    }

    pub fn set_name_in_memory(&mut self, name: String) -> std::io::Result<()> {
        if name.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session name cannot be empty",
            ));
        }
        self.name = Some(name);
        Ok(())
    }

    /// Persist the one-shot automatic title attempt before the provider is spawned.
    pub fn mark_title_attempted(&mut self) -> std::io::Result<bool> {
        let Some(path) = self.file_path.clone() else {
            if self.title_attempted {
                return Ok(false);
            }
            self.title_attempted = true;
            return Ok(true);
        };
        let _guard = session_file_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut latest = Self::load_from_file(&path)?;
        if latest.title_attempted {
            self.title_attempted = true;
            return Ok(false);
        }
        latest.title_attempted = true;
        latest.save_transactionally(&path)?;
        *self = latest;
        Ok(true)
    }

    fn save_transactionally(&self, path: &Path) -> std::io::Result<()> {
        static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let temp_path = directory.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = self
            .save_to_file(&temp_path)
            .and_then(|_| replace_file(&temp_path, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    pub fn add_message(&mut self, message: AgentMessage) -> String {
        let leaf_id = self.active_node_id.clone();
        self.add_message_at_leaf(leaf_id.as_deref(), message)
    }

    /// Update the compatibility tree without writing a second durable copy.
    /// V2's JsonlStore owns persistence; this keeps the in-memory UI tree in
    /// sync until the next reload.
    pub fn add_message_in_memory(&mut self, message: AgentMessage) -> String {
        let path = self.file_path.take();
        let node_id = self.add_message(message);
        self.file_path = path;
        node_id
    }

    /// Appends a message to the passive tree anchored at a specific leaf ID.
    /// Updates active_node_id if the leaf matches the current active node.
    pub fn add_message_at_leaf(&mut self, leaf_id: Option<&str>, message: AgentMessage) -> String {
        let mut next_id = self.nodes.len() + 1;
        let node_id = loop {
            let candidate = format!("node_{next_id}");
            if !self.nodes.contains_key(&candidate) {
                break candidate;
            }
            next_id += 1;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let parent_id = leaf_id.map(|s| s.to_string());
        let node = SessionNode {
            id: node_id.clone(),
            parent_id,
            timestamp: now,
            seq: Some(self.next_persisted_seq()),
            message,
        };

        let old_active = self.active_node_id.clone();
        self.nodes.insert(node_id.clone(), node.clone());
        self.node_order.push(node_id.clone());

        if leaf_id == old_active.as_deref() || old_active.is_none() {
            self.active_node_id = Some(node_id.clone());
        }

        if let Some(ref path) = self.file_path {
            let _guard = session_file_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.append_node_and_metadata_to_file(path, &node).is_err() {
                self.nodes.remove(&node_id);
                self.node_order.pop();
                self.active_node_id = old_active;
                return String::new();
            }
        }

        node_id
    }

    pub fn append_passive_branch(
        &mut self,
        parent_leaf_id: Option<&str>,
        messages: Vec<AgentMessage>,
    ) -> Result<Vec<String>, String> {
        if parent_leaf_id.is_some_and(|id| !self.nodes.contains_key(id)) {
            return Err(format!(
                "Parent session node '{}' not found",
                parent_leaf_id.unwrap()
            ));
        }
        let mut next = self.clone();
        let active_node_id = next.active_node_id.clone();
        let mut parent_id = parent_leaf_id.map(str::to_owned);
        let mut created = Vec::with_capacity(messages.len());

        for message in messages {
            let mut next_id = next.nodes.len() + 1;
            let node_id = loop {
                let candidate = format!("node_{next_id}");
                if !next.nodes.contains_key(&candidate) {
                    break candidate;
                }
                next_id += 1;
            };
            let node = SessionNode {
                id: node_id.clone(),
                parent_id: parent_id.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                seq: Some(next.next_persisted_seq()),
                message,
            };
            next.nodes.insert(node_id.clone(), node);
            next.node_order.push(node_id.clone());
            parent_id = Some(node_id.clone());
            created.push(node_id);
        }
        next.active_node_id = active_node_id;

        if let Some(path) = next.file_path.clone() {
            next.save_transactionally(&path)
                .map_err(|error| format!("Failed to persist passive branch: {error}"))?;
        }
        *self = next;
        Ok(created)
    }

    pub fn append_passive_branch_in_memory(
        &mut self,
        parent_leaf_id: Option<&str>,
        messages: Vec<AgentMessage>,
    ) -> Result<Vec<String>, String> {
        let path = self.file_path.take();
        let result = self.append_passive_branch(parent_leaf_id, messages);
        self.file_path = path;
        result
    }

    /// Replaces the active context with a new root branch while retaining old
    /// nodes as navigable history. New nodes are appended to the same session file.
    pub fn replace_active_branch(&mut self, messages: Vec<AgentMessage>) {
        self.active_node_id = None;
        for message in messages {
            if !matches!(message, AgentMessage::System { .. }) {
                self.add_message(message);
            }
        }
    }

    /// Rebuild the compatibility view without rewriting a V2-owned session.
    pub fn replace_active_branch_in_memory(&mut self, messages: Vec<AgentMessage>) {
        let path = self.file_path.take();
        self.replace_active_branch(messages);
        self.file_path = path;
    }

    pub fn get_active_branch_messages(&self) -> Vec<AgentMessage> {
        self.get_branch_messages(self.active_node_id.as_deref())
    }

    /// Traverses the passive DAG from the specified leaf ID back to root.
    pub fn get_branch_messages(&self, leaf_id: Option<&str>) -> Vec<AgentMessage> {
        let mut path_nodes = Vec::new();
        let mut curr = leaf_id.map(|s| s.to_string());

        while let Some(id) = curr {
            if let Some(node) = self.nodes.get(&id) {
                path_nodes.push(node.message.clone());
                curr = node.parent_id.clone();
            } else {
                break;
            }
        }

        path_nodes.reverse();
        path_nodes
    }

    /// Messages in persisted/insertion order, including messages from all
    /// branches. This is used for legacy unnamed sessions only.
    pub fn get_persisted_messages(&self) -> Vec<AgentMessage> {
        self.node_order
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .map(|node| node.message.clone())
            .collect()
    }

    pub fn replace_tool_result(
        &mut self,
        tool_call_id: &str,
        content: String,
        is_error: bool,
    ) -> bool {
        if let Some(path) = self.file_path.clone() {
            let _guard = session_file_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Ok(mut latest) = Self::load_from_file(&path) else {
                return false;
            };
            let Some(node_id) = latest
                .node_order
                .iter()
                .find(|node_id| {
                    matches!(
                        latest.nodes.get(*node_id).map(|node| &node.message),
                        Some(AgentMessage::Tool { tool_call_id: id, .. }) if id == tool_call_id
                    )
                })
                .cloned()
            else {
                return false;
            };
            let Some(node) = latest.nodes.get_mut(&node_id) else {
                return false;
            };
            if let AgentMessage::Tool {
                content: current_content,
                is_error: current_is_error,
                ..
            } = &mut node.message
            {
                *current_content = content;
                *current_is_error = is_error;
            }
            if latest.save_transactionally(&path).is_err() {
                return false;
            }
            *self = latest;
            return true;
        }
        let Some(node_id) = self
            .node_order
            .iter()
            .find(|node_id| {
                matches!(
                    self.nodes.get(*node_id).map(|node| &node.message),
                    Some(AgentMessage::Tool { tool_call_id: id, .. }) if id == tool_call_id
                )
            })
            .cloned()
        else {
            return false;
        };
        let previous = self
            .nodes
            .get(&node_id)
            .map(|node| node.message.clone())
            .unwrap();
        {
            let node = self.nodes.get_mut(&node_id).unwrap();
            if let AgentMessage::Tool {
                content: current_content,
                is_error: current_is_error,
                ..
            } = &mut node.message
            {
                *current_content = content;
                *current_is_error = is_error;
            }
        }
        if let Some(path) = self.file_path.clone() {
            if self.save_transactionally(&path).is_err() {
                self.nodes.get_mut(&node_id).unwrap().message = previous;
                return false;
            }
        }
        true
    }

    pub fn switch_active_node(&mut self, node_id: &str) -> bool {
        if !self.nodes.contains_key(node_id) {
            return false;
        }
        if let Some(path) = self.file_path.clone() {
            let _guard = session_file_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let old_active = self.active_node_id.clone();
            self.active_node_id = Some(node_id.to_string());
            if self.append_metadata_to_file(&path).is_err() {
                self.active_node_id = old_active;
                return false;
            }
        } else {
            self.active_node_id = Some(node_id.to_string());
        }
        true
    }

    pub fn fork_branch(&mut self, node_id: &str) -> Option<SessionTree> {
        if !self.nodes.contains_key(node_id) {
            return None;
        }

        let suffix = node_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let new_id = format!("{}_fork_{}", self.session_id, suffix);
        let mut forked = SessionTree::new(new_id);
        forked.parent_session_id = Some(self.session_id.clone());
        forked.name = self.name.clone();
        forked.title_attempted = self.title_attempted;
        forked.model = self.model.clone();
        forked.plan = self.plan.clone();
        forked.global_facts = self.global_facts.clone();

        let mut curr = Some(node_id.to_string());
        let mut path_nodes = Vec::new();

        while let Some(id) = curr {
            if let Some(node) = self.nodes.get(&id) {
                path_nodes.push(node.clone());
                curr = node.parent_id.clone();
            } else {
                break;
            }
        }
        path_nodes.reverse();

        for node in path_nodes {
            forked.add_message(node.message);
        }

        Some(forked)
    }

    fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        let mut file = File::create(path)?;
        for node_id in self
            .node_order
            .iter()
            .chain(self.nodes.keys().filter(|id| !self.node_order.contains(id)))
        {
            if let Some(node) = self.nodes.get(node_id) {
                if self.v2_entry_ids.contains(node_id) {
                    continue;
                }
                writeln!(file, "{}", serde_json::to_string(node)?)?;
            }
        }
        if self.has_name()
            || self.title_attempted
            || self.active_node_id.is_some()
            || self.model.is_some()
            || self.parent_session_id.is_some()
        {
            let metadata = SessionRecord::Metadata {
                session_id: Some(self.session_id.clone()),
                name: self.name.clone(),
                title_attempted: self.title_attempted,
                active_node_id: self.active_node_id.clone(),
                model: self.model.clone(),
                parent_session_id: self.parent_session_id.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        }
        for (key, value) in &self.global_facts {
            let fact = SessionRecord::GlobalFact {
                key: key.clone(),
                value: value.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&fact)?)?;
        }
        let plan = SessionRecord::Plan {
            explanation: self.plan.explanation.clone(),
            items: self.plan.items.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&plan)?)?;
        for line in &self.v2_lines {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
    fn append_metadata_to_file(&self, path: &Path) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let metadata = SessionRecord::Metadata {
            session_id: Some(self.session_id.clone()),
            name: self.name.clone(),
            title_attempted: self.title_attempted,
            active_node_id: self.active_node_id.clone(),
            model: self.model.clone(),
            parent_session_id: self.parent_session_id.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        Ok(())
    }

    fn append_node_and_metadata_to_file(
        &self,
        path: &Path,
        node: &SessionNode,
    ) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(node)?)?;
        let metadata = SessionRecord::Metadata {
            session_id: Some(self.session_id.clone()),
            name: self.name.clone(),
            title_attempted: self.title_attempted,
            active_node_id: self.active_node_id.clone(),
            model: self.model.clone(),
            parent_session_id: self.parent_session_id.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;

        let session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());

        let mut tree = SessionTree::new(session_id);
        tree.file_path = Some(path.to_path_buf());

        let mut explicit_active = false;
        let mut v2_main_leaf = None;
        let line_count = data.split('\n').count();
        for (line_number, l) in data.split('\n').enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            let torn_tail = line_number + 1 == line_count && !data.ends_with('\n');
            let value = match serde_json::from_str::<serde_json::Value>(&l) {
                Ok(value) => value,
                Err(_error) if torn_tail => break,
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to parse session line {}: {error}", line_number + 1),
                    ));
                }
            };
            if serde_json::from_value::<crate::harness::Record>(value.clone()).is_ok() {
                tree.v2_lines.push(l.to_owned());
                continue;
            }
            if value.get("lane").is_some() {
                match serde_json::from_value::<crate::harness::Entry>(value.clone()) {
                    Ok(entry) => {
                        if entry.lane == "main" {
                            v2_main_leaf = Some(entry.id.clone());
                        }
                        tree.node_order.push(entry.id.clone());
                        tree.v2_entry_ids.insert(entry.id.clone());
                        tree.nodes.insert(
                            entry.id.clone(),
                            SessionNode {
                                id: entry.id,
                                parent_id: entry.parent_id,
                                timestamp: entry.timestamp,
                                seq: Some(entry.seq),
                                message: entry.message,
                            },
                        );
                        tree.v2_lines.push(l.to_owned());
                        continue;
                    }
                    Err(_) => {}
                }
            }
            match serde_json::from_str::<SessionLine>(&l) {
                Ok(SessionLine::Record(record)) => match record {
                    SessionRecord::Metadata {
                        session_id,
                        name,
                        title_attempted,
                        active_node_id,
                        model,
                        parent_session_id,
                    } => {
                        tree.metadata_present = true;
                        if let Some(session_id) = session_id {
                            tree.session_id = session_id;
                        }
                        tree.name = name;
                        tree.title_attempted = title_attempted;
                        tree.model = model;
                        tree.parent_session_id = parent_session_id;
                        if active_node_id.is_some() {
                            explicit_active = true;
                        }
                        tree.active_node_id = active_node_id;
                    }
                    SessionRecord::Plan { explanation, items } => {
                        tree.plan = SessionPlan { explanation, items };
                    }
                    SessionRecord::GlobalFact { key, value } => {
                        tree.global_facts.insert(key, value);
                    }
                },
                Ok(SessionLine::Node(node)) => {
                    if !explicit_active {
                        tree.active_node_id = Some(node.id.clone());
                    }
                    tree.node_order.push(node.id.clone());
                    tree.nodes.insert(node.id.clone(), node);
                }
                Err(_error) if torn_tail => break,
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to parse session line {}: {error}", line_number + 1),
                    ));
                }
            }
        }

        if let Some(leaf_id) = v2_main_leaf {
            tree.active_node_id = Some(leaf_id);
        }

        let harness_path = path.with_extension("harness.jsonl");
        if harness_path.exists() {
            let harness_data = std::fs::read_to_string(&harness_path)?;
            let line_count = harness_data.split('\n').count();
            for (line_number, line) in harness_data.split('\n').enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let torn_tail = line_number + 1 == line_count && !harness_data.ends_with('\n');
                let value = match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(value) => value,
                    Err(_error) if torn_tail => break,
                    Err(error) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("Failed to parse harness line {}: {error}", line_number + 1),
                        ));
                    }
                };
                match serde_json::from_value::<crate::harness::Record>(value) {
                    Ok(crate::harness::Record::FactSet {
                        run_id: None,
                        key,
                        value,
                        ..
                    }) => {
                        tree.global_facts.insert(key.clone(), value.clone());
                        match key.as_str() {
                            "model" => tree.model = Some(value),
                            "name" => tree.name = Some(value),
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("Failed to parse harness line {}: {error}", line_number + 1),
                        ));
                    }
                }
            }
        }

        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::SessionStore;
    use crate::types::{PlanItem, PlanItemStatus, SessionPlan};

    fn test_plan(status: PlanItemStatus) -> SessionPlan {
        SessionPlan {
            explanation: Some("Keep the session plan".into()),
            items: vec![PlanItem {
                step: "Inspect".into(),
                status,
            }],
        }
    }

    #[test]
    fn session_plan_round_trips_and_latest_record_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());

        tree.replace_plan(test_plan(PlanItemStatus::InProgress))
            .unwrap();
        tree.replace_plan(test_plan(PlanItemStatus::Completed))
            .unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.plan(), &test_plan(PlanItemStatus::Completed));
    }

    #[test]
    fn session_plan_empty_replacement_clears_persisted_plan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.replace_plan(test_plan(PlanItemStatus::Pending))
            .unwrap();
        tree.replace_plan(SessionPlan::default()).unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.plan(), &SessionPlan::default());
    }

    #[test]
    fn session_plan_survives_metadata_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.replace_plan(test_plan(PlanItemStatus::Pending))
            .unwrap();
        tree.set_name("Named session".into()).unwrap();
        tree.set_model("gpt-5".into()).unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.plan(), &test_plan(PlanItemStatus::Pending));
    }

    #[test]
    fn session_plan_defaults_for_legacy_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"session_metadata\",\"name\":\"Legacy\",\"title_attempted\":false,\"active_node_id\":null}\n",
        )
        .unwrap();

        assert_eq!(
            SessionTree::load_from_file(&path).unwrap().plan(),
            &SessionPlan::default()
        );
    }

    #[test]
    fn metadata_name_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.name = Some("Improve session titles".into());
        tree.add_message(AgentMessage::User {
            content: "Help".into(),
        });
        tree.save_to_file(&path).unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.name.as_deref(), Some("Improve session titles"));
        assert_eq!(loaded.get_active_branch_messages().len(), 1);
    }

    #[test]
    fn v2_compatibility_updates_do_not_rewrite_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::user("hello", vec![]));
        let before = std::fs::read_to_string(&path).unwrap();

        tree.set_model_in_memory("gpt-test".into()).unwrap();
        tree.add_message_in_memory(AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("stop".into()),
            deferred_handle: None,
        });

        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(tree.model.as_deref(), Some("gpt-test"));
        assert_eq!(tree.get_active_branch_messages().len(), 2);
    }

    #[test]
    fn direct_load_applies_v2_model_and_name_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::user("hello", vec![]));
        let mut store = crate::harness::JsonlStore::open(&path).unwrap();
        store
            .append_record(crate::harness::Record::FactSet {
                id: "fact-model".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: None,
                key: "model".into(),
                value: "antigravity/provider-model".into(),
            })
            .unwrap();
        store
            .append_record(crate::harness::Record::FactSet {
                id: "fact-name".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 2,
                run_id: None,
                key: "name".into(),
                value: "Durable title".into(),
            })
            .unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("antigravity/provider-model"));
        assert_eq!(loaded.name.as_deref(), Some("Durable title"));
        assert_eq!(loaded.get_fact("model"), Some("antigravity/provider-model"));
    }

    #[test]
    fn selected_model_round_trips_with_other_metadata_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::User {
            content: "Help".into(),
        });

        tree.set_model("antigravity/claude-opus-4-6".into())
            .unwrap();
        tree.set_name("Persistent model".into()).unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("antigravity/claude-opus-4-6"));
        assert_eq!(loaded.name.as_deref(), Some("Persistent model"));
        assert_eq!(loaded.nodes.len(), 1);
    }

    #[test]
    fn legacy_metadata_without_model_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"session_metadata\",\"name\":\"Legacy\",\"title_attempted\":false,\"active_node_id\":null}\n",
        )
        .unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.name.as_deref(), Some("Legacy"));
        assert!(loaded.model.is_none());
    }

    #[test]
    fn legacy_node_only_file_still_loads_without_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            "{\"id\":\"node_1\",\"parent_id\":null,\"timestamp\":1,\"message\":{\"role\":\"user\",\"content\":\"Help\"}}\n",
        )
        .unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert!(loaded.name.is_none());
        assert_eq!(loaded.nodes.len(), 1);
    }

    #[test]
    fn legacy_metadata_rewrites_preserve_v2_entries_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        let root = tree.add_message(AgentMessage::user("Help", Vec::new()));
        tree.set_name("Before V2".into()).unwrap();

        let mut store = crate::harness::JsonlStore::open(&path).unwrap();
        store
            .append_entry(crate::harness::Entry {
                id: "v2-entry".into(),
                parent_id: Some(root),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("durable".into()),
                    tool_calls: None,
                    stop_reason: Some("stop".into()),
                    deferred_handle: None,
                },
                terminate: false,
            })
            .unwrap();
        store
            .append_record(crate::harness::Record::OperationStarted {
                id: "run-v2".into(),
                seq: 3,
                lane: "main".into(),
                timestamp: 3,
                source_leaf_id: Some("v2-entry".into()),
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();
        drop(store);

        let mut loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.nodes.len(), 2);
        assert!(loaded.nodes.contains_key("v2-entry"));
        assert_eq!(loaded.active_node_id(), Some("v2-entry"));
        loaded.set_name("Preserved".into()).unwrap();

        let reopened = crate::harness::JsonlStore::open(&path).unwrap();
        assert!(reopened
            .entries()
            .iter()
            .any(|entry| entry.id == "v2-entry"));
        assert!(reopened
            .records()
            .iter()
            .any(|record| record.id() == "run-v2"));
    }

    #[test]
    fn set_name_rewrites_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::User {
            content: "Help".into(),
        });

        tree.set_name("A useful title".into()).unwrap();
        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.name.as_deref(), Some("A useful title"));
        assert_eq!(loaded.nodes.len(), 1);
    }

    #[test]
    fn set_name_retains_previous_name_when_persistence_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.name = Some("Existing title".into());
        tree.file_path = Some(path);

        let error = tree.set_name("New title".into()).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(tree.name.as_deref(), Some("Existing title"));
    }

    #[test]
    fn title_update_preserves_nodes_appended_by_normal_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut initial = SessionTree::new("session");
        initial.file_path = Some(path.clone());
        initial.add_message(AgentMessage::User {
            content: "first".into(),
        });

        // Simulate the title task having loaded before the normal turn writes.
        let mut title_task = SessionTree::load_from_file(&path).unwrap();
        let mut normal_turn = SessionTree::load_from_file(&path).unwrap();
        normal_turn.add_message(AgentMessage::Assistant {
            content: Some("concurrent".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        title_task.set_name("Generated title".into()).unwrap();
        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.name.as_deref(), Some("Generated title"));
        assert_eq!(loaded.nodes.len(), 2);
        assert!(loaded.nodes.values().any(|node| matches!(
            &node.message,
            AgentMessage::Assistant { content: Some(text), .. } if text == "concurrent"
        )));
    }

    #[test]
    fn title_attempt_marker_round_trips_without_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::User {
            content: "hello".into(),
        });
        assert!(tree.mark_title_attempted().unwrap());
        assert!(!tree.mark_title_attempted().unwrap());
        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert!(loaded.title_attempted);
        assert!(loaded.name.is_none());
    }

    #[test]
    fn reload_preserves_explicit_active_branch_for_title_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        tree.add_message(AgentMessage::User {
            content: "root".into(),
        });
        let root = tree.active_node_id.clone().unwrap();
        tree.add_message(AgentMessage::User {
            content: "branch a".into(),
        });
        assert!(tree.switch_active_node(&root));
        tree.add_message(AgentMessage::User {
            content: "branch b".into(),
        });
        let active = tree.active_node_id.clone().unwrap();
        tree.set_name("title".into()).unwrap();
        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.active_node_id.as_deref(), Some(active.as_str()));
        assert!(matches!(
            loaded.get_active_branch_messages().last(),
            Some(AgentMessage::User { content }) if content == "branch b"
        ));
    }
    #[test]
    fn set_name_rejects_empty_name() {
        let mut tree = SessionTree::new("session");
        assert!(tree.set_name(String::new()).is_err());
        assert!(tree.name.is_none());
    }

    #[test]
    fn passive_dag_branch_retrieval_by_leaf() {
        let mut tree = SessionTree::new("passive_session");
        let n1 = tree.add_message(AgentMessage::User {
            content: "root".into(),
        });
        let n2 = tree.add_message_at_leaf(
            Some(&n1),
            AgentMessage::User {
                content: "lane_a_1".into(),
            },
        );
        let n3 = tree.add_message_at_leaf(
            Some(&n1),
            AgentMessage::User {
                content: "lane_b_1".into(),
            },
        );

        let branch_a = tree.get_branch_messages(Some(&n2));
        assert_eq!(branch_a.len(), 2);
        assert!(matches!(&branch_a[1], AgentMessage::User { content } if content == "lane_a_1"));

        let branch_b = tree.get_branch_messages(Some(&n3));
        assert_eq!(branch_b.len(), 2);
        assert!(matches!(&branch_b[1], AgentMessage::User { content } if content == "lane_b_1"));
    }

    #[test]
    fn passive_branch_append_preserves_active_leaf_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(path.clone());
        let parent = tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });
        let expected = vec![
            AgentMessage::User {
                content: "child task".into(),
            },
            AgentMessage::Assistant {
                content: Some("child result".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];

        let branch = tree
            .append_passive_branch(Some(&parent), expected.clone())
            .unwrap();

        assert_eq!(tree.active_node_id(), Some(parent.as_str()));
        assert_eq!(
            serde_json::to_value(tree.get_branch_messages(branch.last().map(String::as_str)))
                .unwrap(),
            serde_json::to_value(
                [
                    vec![AgentMessage::User {
                        content: "parent".into()
                    }],
                    expected
                ]
                .concat()
            )
            .unwrap()
        );
        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.active_node_id(), Some(parent.as_str()));
        assert_eq!(
            serde_json::to_value(loaded.get_branch_messages(branch.last().map(String::as_str)))
                .unwrap(),
            serde_json::to_value(tree.get_branch_messages(branch.last().map(String::as_str)))
                .unwrap()
        );
    }

    #[test]
    fn passive_branch_append_rolls_back_when_persistence_fails() {
        let mut tree = SessionTree::new("session");
        let parent = tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });
        tree.file_path = Some(
            tempfile::tempdir()
                .unwrap()
                .path()
                .join("missing/session.jsonl"),
        );
        let node_count = tree.nodes.len();

        assert!(tree
            .append_passive_branch(
                Some(&parent),
                vec![AgentMessage::User {
                    content: "child".into(),
                }],
            )
            .is_err());
        assert_eq!(tree.nodes.len(), node_count);
        assert_eq!(tree.active_node_id(), Some(parent.as_str()));
    }

    #[test]
    fn global_facts_persistence_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_facts.jsonl");

        let mut tree = SessionTree::new("facts_session");
        tree.file_path = Some(path.clone());
        tree.set_fact("git_branch", "feat/multi-lane").unwrap();
        tree.set_fact("env", "staging").unwrap();

        assert_eq!(tree.get_fact("git_branch"), Some("feat/multi-lane"));
        assert_eq!(tree.get_fact("env"), Some("staging"));

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.get_fact("git_branch"), Some("feat/multi-lane"));
        assert_eq!(loaded.get_fact("env"), Some("staging"));
    }

    #[test]
    fn global_facts_survive_metadata_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_facts_rewrite.jsonl");
        let mut tree = SessionTree::new("facts_session");
        tree.file_path = Some(path.clone());
        tree.set_fact("git_branch", "feat/multi-lane").unwrap();

        tree.set_name("Named session".into()).unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.get_fact("git_branch"), Some("feat/multi-lane"));
    }

    #[test]
    fn replaces_recovered_tool_result_in_place() {
        let mut tree = SessionTree::new("recovered");
        tree.add_message(AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "[Recovered tool result]".into(),
            is_error: false,
            terminate: false,
        });

        assert!(tree.replace_tool_result("call-1", "actual output".into(), false));
        assert!(matches!(
            tree.get_active_branch_messages().last(),
            Some(AgentMessage::Tool { content, is_error, .. })
                if content == "actual output" && !is_error
        ));
    }
}

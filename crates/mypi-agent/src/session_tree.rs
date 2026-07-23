use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
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

fn session_file_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: u64,
    pub message: AgentMessage,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum SessionRecord {
    #[serde(rename = "session_metadata")]
    Metadata {
        name: Option<String>,
        #[serde(default)]
        title_attempted: bool,
        #[serde(default)]
        active_node_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct SessionTree {
    pub session_id: String,
    pub name: Option<String>,
    pub title_attempted: bool,
    pub nodes: HashMap<String, SessionNode>,
    /// Node IDs in persisted/insertion order. This is intentionally separate
    /// from `nodes`: the map is only an index and does not define ordering.
    pub node_order: Vec<String>,
    pub active_node_id: Option<String>,
    pub file_path: Option<PathBuf>,
    /// Whether a session metadata record was present on disk. Legacy files
    /// have no metadata and retain their historical all-branches lookup rules.
    pub metadata_present: bool,
}

impl SessionTree {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            name: None,
            title_attempted: false,
            nodes: HashMap::new(),
            node_order: Vec::new(),
            active_node_id: None,
            file_path: None,
            metadata_present: false,
        }
    }

    pub fn has_name(&self) -> bool {
        self.name.as_ref().is_some_and(|name| !name.is_empty())
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
            let _guard = session_file_lock().lock().unwrap();
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

    /// Persist the one-shot automatic title attempt before the provider is spawned.
    pub fn mark_title_attempted(&mut self) -> std::io::Result<bool> {
        let Some(path) = self.file_path.clone() else {
            if self.title_attempted {
                return Ok(false);
            }
            self.title_attempted = true;
            return Ok(true);
        };
        let _guard = session_file_lock().lock().unwrap();
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

        let node = SessionNode {
            id: node_id.clone(),
            parent_id: self.active_node_id.clone(),
            timestamp: now,
            message,
        };

        self.nodes.insert(node_id.clone(), node.clone());
        self.node_order.push(node_id.clone());
        self.active_node_id = Some(node_id.clone());

        if let Some(ref path) = self.file_path {
            let _guard = session_file_lock().lock().unwrap();
            let _ = self.append_node_to_file(path, &node);
            let _ = self.append_metadata_to_file(path);
        }

        node_id
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

    pub fn get_active_branch_messages(&self) -> Vec<AgentMessage> {
        let mut path_nodes = Vec::new();
        let mut curr = self.active_node_id.clone();

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

    pub fn switch_active_node(&mut self, node_id: &str) -> bool {
        if self.nodes.contains_key(node_id) {
            self.active_node_id = Some(node_id.to_string());
            if let Some(path) = self.file_path.clone() {
                let _guard = session_file_lock().lock().unwrap();
                let _ = self.append_metadata_to_file(&path);
            }
            true
        } else {
            false
        }
    }

    pub fn fork_branch(&mut self, node_id: &str) -> Option<SessionTree> {
        if !self.nodes.contains_key(node_id) {
            return None;
        }

        let new_id = format!("{}_fork", self.session_id);
        let mut forked = SessionTree::new(new_id);

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

    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        let mut file = File::create(path)?;
        for node_id in self
            .node_order
            .iter()
            .chain(self.nodes.keys().filter(|id| !self.node_order.contains(id)))
        {
            if let Some(node) = self.nodes.get(node_id) {
                writeln!(file, "{}", serde_json::to_string(node)?)?;
            }
        }
        if self.has_name() || self.title_attempted || self.active_node_id.is_some() {
            let metadata = SessionRecord::Metadata {
                name: self.name.clone(),
                title_attempted: self.title_attempted,
                active_node_id: self.active_node_id.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        }
        Ok(())
    }

    fn append_metadata_to_file(&self, path: &Path) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let metadata = SessionRecord::Metadata {
            name: self.name.clone(),
            title_attempted: self.title_attempted,
            active_node_id: self.active_node_id.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        Ok(())
    }

    fn append_node_to_file(&self, path: &Path, node: &SessionNode) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(node)?)?;
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());

        let mut tree = SessionTree::new(session_id);
        tree.file_path = Some(path.to_path_buf());

        let mut explicit_active = false;
        for line in reader.lines() {
            let l = line?;
            if l.trim().is_empty() {
                continue;
            }
            if let Ok(SessionRecord::Metadata {
                name,
                title_attempted,
                active_node_id,
            }) = serde_json::from_str::<SessionRecord>(&l)
            {
                tree.metadata_present = true;
                tree.name = name;
                tree.title_attempted = title_attempted;
                if active_node_id.is_some() {
                    explicit_active = true;
                }
                tree.active_node_id = active_node_id;
            } else if let Ok(node) = serde_json::from_str::<SessionNode>(&l) {
                if !explicit_active {
                    tree.active_node_id = Some(node.id.clone());
                }
                tree.node_order.push(node.id.clone());
                tree.nodes.insert(node.id.clone(), node);
            }
        }

        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

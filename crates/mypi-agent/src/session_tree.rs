use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    Metadata { name: Option<String> },
}

#[derive(Debug, Clone, Default)]
pub struct SessionTree {
    pub session_id: String,
    pub name: Option<String>,
    pub nodes: HashMap<String, SessionNode>,
    pub active_node_id: Option<String>,
    pub file_path: Option<PathBuf>,
}

impl SessionTree {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            name: None,
            nodes: HashMap::new(),
            active_node_id: None,
            file_path: None,
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

        self.name = Some(name);
        if let Some(path) = self.file_path.clone() {
            let directory = path.parent().unwrap_or_else(|| Path::new("."));
            static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
            let temp_path = directory.join(format!(
                ".{}.{}.{}.tmp",
                path.file_name().unwrap_or_default().to_string_lossy(),
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));

            let result = self
                .save_to_file(&temp_path)
                .and_then(|_| std::fs::rename(&temp_path, &path));
            if result.is_err() {
                let _ = std::fs::remove_file(&temp_path);
            }
            result
        } else {
            Ok(())
        }
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
        self.active_node_id = Some(node_id.clone());

        if let Some(ref path) = self.file_path {
            let _ = self.append_node_to_file(path, &node);
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

    pub fn switch_active_node(&mut self, node_id: &str) -> bool {
        if self.nodes.contains_key(node_id) {
            self.active_node_id = Some(node_id.to_string());
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
        if self.has_name() {
            let metadata = SessionRecord::Metadata {
                name: self.name.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        }
        for node in self.nodes.values() {
            let line = serde_json::to_string(node)?;
            writeln!(file, "{}", line)?;
        }
        Ok(())
    }

    fn append_node_to_file(&self, path: &Path, node: &SessionNode) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let line = serde_json::to_string(node)?;
        writeln!(file, "{}", line)?;
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

        for line in reader.lines() {
            let l = line?;
            if l.trim().is_empty() {
                continue;
            }
            if let Ok(SessionRecord::Metadata { name }) = serde_json::from_str::<SessionRecord>(&l)
            {
                tree.name = name;
            } else if let Ok(node) = serde_json::from_str::<SessionNode>(&l) {
                tree.active_node_id = Some(node.id.clone());
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
    fn set_name_rejects_empty_name() {
        let mut tree = SessionTree::new("session");
        assert!(tree.set_name(String::new()).is_err());
        assert!(tree.name.is_none());
    }
}

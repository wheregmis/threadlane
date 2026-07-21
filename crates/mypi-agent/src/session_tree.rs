use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: u64,
    pub message: AgentMessage,
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

    pub fn add_message(&mut self, message: AgentMessage) -> String {
        let node_id = format!("node_{}", self.nodes.len() + 1);
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
            if let Ok(node) = serde_json::from_str::<SessionNode>(&l) {
                tree.active_node_id = Some(node.id.clone());
                tree.nodes.insert(node.id.clone(), node);
            }
        }

        Ok(tree)
    }
}

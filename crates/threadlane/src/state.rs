//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use std::path::PathBuf;
use threadlane_agent::{
    harness::{HarnessEvent, Snapshot},
    AgentEvent,
};
use threadlane_coding_agent::{
    CapabilityCatalog, ExtensionRecord, ExtensionScope, SkillManager, TaskAgentEvent,
};

pub use crate::panels::chat::*;
pub use crate::panels::command_palette::*;

pub use crate::panels::sessions::*;
pub use crate::path_utils::truncate_chars;

#[derive(Clone)]
pub struct CapabilitySkillRow {
    pub id: String,
    #[allow(dead_code)]
    pub description: String,
    pub scope_label: String,
    pub file_path: PathBuf,
    pub enabled: bool,
    pub is_valid: bool,
}

impl CapabilitySkillRow {
    pub fn scope_status(&self) -> String {
        let status = if !self.is_valid {
            "Invalid"
        } else if self.enabled {
            "Enabled"
        } else {
            "Disabled"
        };
        format!("{} · {status}", self.scope_label)
    }
}

#[derive(Clone)]
pub struct CapabilityExtensionRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub module_path: PathBuf,
    pub scope: ExtensionScope,
    pub enabled: bool,
    pub effective: bool,
}

impl CapabilityExtensionRow {
    pub fn scope_status(&self) -> String {
        let scope = match self.scope {
            ExtensionScope::Global => "Global",
            ExtensionScope::Project => "Project",
        };
        let status = if !self.enabled {
            "Disabled"
        } else if self.effective {
            "Active"
        } else {
            "Overridden"
        };
        format!("{scope} · {status}")
    }

    pub fn matches_record(&self, record: &ExtensionRecord) -> bool {
        record.id() == self.id
            && record.name() == self.name
            && record.version() == self.version
            && record.scope() == self.scope
            && record.module_path() == self.module_path
    }
}

#[derive(Clone)]
pub struct CapabilityMcpRow {
    pub id: String,
    pub name: String,
    pub transport_detail: String,
    pub scope: threadlane_coding_agent::McpScope,
    pub enabled: bool,
    pub status_text: String,
}

impl CapabilityMcpRow {
    pub fn scope_status(&self) -> String {
        let scope = match self.scope {
            threadlane_coding_agent::McpScope::Global => "Global",
            threadlane_coding_agent::McpScope::Project => "Project",
        };
        format!("{scope} · {}", self.status_text)
    }
}

#[derive(Clone)]
pub struct CapabilityAcpRow {
    pub id: String,
    pub name: String,
    pub command_detail: String,
    pub scope: threadlane_coding_agent::AcpScope,
    pub enabled: bool,
    pub status_text: String,
}

impl CapabilityAcpRow {
    pub fn scope_status(&self) -> String {
        let scope = match self.scope {
            threadlane_coding_agent::AcpScope::Global => "Global",
            threadlane_coding_agent::AcpScope::Project => "Project",
        };
        format!("{scope} · {}", self.status_text)
    }
}

#[derive(Default)]
pub struct CapabilityState {
    pub extensions: Vec<CapabilityExtensionRow>,
    pub skills: Vec<CapabilitySkillRow>,
    pub mcp_servers: Vec<CapabilityMcpRow>,
    pub acp_agents: Vec<CapabilityAcpRow>,
}

impl CapabilityState {
    pub fn refresh(&mut self, catalog: &CapabilityCatalog) {
        self.refresh_records(catalog.extensions());
    }

    pub fn refresh_skills(&mut self, project_root: Option<&std::path::Path>) {
        let mut manager = SkillManager::new();
        manager.discover_skills(project_root);
        self.skills = manager
            .list_skills()
            .into_iter()
            .map(|skill| CapabilitySkillRow {
                id: skill.id.clone(),
                description: skill.description.clone(),
                scope_label: skill.scope.display_name().to_owned(),
                file_path: skill.file_path().to_path_buf(),
                enabled: skill.enabled,
                is_valid: skill.is_valid,
            })
            .collect();
    }

    pub fn refresh_mcp_records(&mut self, records: Vec<threadlane_coding_agent::McpServerRecord>) {
        self.mcp_servers = records
            .into_iter()
            .map(|rec| {
                let transport_detail = match &rec.config.transport {
                    threadlane_coding_agent::McpTransport::Stdio { command, args, .. } => {
                        format!("stdio: {} {}", command, args.join(" "))
                    }
                    threadlane_coding_agent::McpTransport::Sse { url, .. } => {
                        format!("sse: {}", url)
                    }
                };
                CapabilityMcpRow {
                    id: rec.config.id,
                    name: rec.config.name,
                    transport_detail,
                    scope: rec.config.scope,
                    enabled: rec.config.enabled,
                    status_text: rec.status.display_status(),
                }
            })
            .collect();
    }

    pub fn refresh_acp_records(&mut self, records: Vec<threadlane_coding_agent::AcpAgentRecord>) {
        self.acp_agents = records
            .into_iter()
            .map(|rec| CapabilityAcpRow {
                command_detail: rec.config.command_line(),
                id: rec.config.id,
                name: rec.config.name,
                scope: rec.config.scope,
                enabled: rec.config.enabled,
                status_text: rec.status.display_status(),
            })
            .collect();
    }

    fn refresh_records(&mut self, extensions: &[ExtensionRecord]) {
        self.extensions = extensions
            .iter()
            .map(|extension| CapabilityExtensionRow {
                id: extension.id().to_owned(),
                name: extension.name().to_owned(),
                version: extension.version().to_owned(),
                module_path: extension.module_path().to_path_buf(),
                scope: extension.scope(),
                enabled: extension.is_enabled(),
                effective: extension.is_effective(),
            })
            .collect();
    }
}

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    GenerationAgent {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        event: AgentEvent,
    },
    DeviceCodePrompt {
        user_code: String,
        url: String,
    },
    DeviceLoginSuccess,
    DeviceLoginError(String),
    SessionTitleGenerated,
    AvailableModelsLoaded(Vec<String>),
    ProjectFolderPicked(Result<Option<PathBuf>, String>),
    ExtensionFilePicked {
        path: Option<PathBuf>,
        scope: ExtensionScope,
    },
    ExtensionReloadCompleted {
        reload_id: u64,
        reloaded: usize,
        failures: Vec<String>,
    },
    BackgroundTask(TaskAgentEvent),
    CommandOutput {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        output: String,
    },
    GenerationFinished {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
    },
    HarnessEvent {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        event: HarnessEvent,
    },
    HarnessSnapshot {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        snapshot: Snapshot,
    },
    HarnessResumeFinished {
        work_dir: PathBuf,
        session_id: String,
        result: Result<bool, String>,
    },
    AntigravityLoginSuccess {
        email: Option<String>,
    },
    AntigravityLoginError(String),
    AntigravityDoctorReport(String),
    GitStatusLoaded {
        request_id: u64,
        work_dir: PathBuf,
        result: Result<crate::git::GitStatus, String>,
    },
    GitOperationFinished {
        request_id: u64,
        work_dir: PathBuf,
        operation: String,
        result: Result<(), String>,
    },

    GitDiffLoaded {
        request_id: u64,
        path: String,
        result: Result<String, String>,
    },
    GitCommitMessageGenerated {
        request_id: u64,
        work_dir: PathBuf,
        result: Result<String, String>,
    },
    McpRefreshCompleted(Vec<threadlane_coding_agent::McpServerRecord>),
    AcpRefreshCompleted(Vec<threadlane_coding_agent::AcpAgentRecord>),
    /// A chat session opened its external agent session; the UI stores it so
    /// follow-up turns and cancellation reuse the same conversation.
    AcpSessionStarted {
        work_dir: PathBuf,
        session_id: String,
        chat: crate::app::AcpChat,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use threadlane_coding_agent::ExtensionManager;

    fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            bytes.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        push_unsigned_leb(payload.len() as u32, wasm);
        wasm.extend_from_slice(payload);
    }

    fn manifest_wasm(name: &str, version: &str) -> Vec<u8> {
        let manifest = format!(
            r#"{{"api_version":1,"name":"{name}","version":"{version}","description":"test fixture"}}"#
        );
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(&mut wasm, 1, &[1, 0x60, 0, 1, 0x7e]);
        push_section(&mut wasm, 3, &[1, 0]);
        push_section(&mut wasm, 5, &[1, 0, 1]);

        let mut exports = vec![2];
        push_unsigned_leb("extension_info".len() as u32, &mut exports);
        exports.extend_from_slice(b"extension_info");
        exports.extend_from_slice(&[0, 0]);
        push_unsigned_leb("memory".len() as u32, &mut exports);
        exports.extend_from_slice(b"memory");
        exports.extend_from_slice(&[2, 0]);
        push_section(&mut wasm, 7, &exports);

        let mut body = vec![0, 0x42];
        push_signed_leb(manifest.len() as i64, &mut body);
        body.push(0x0b);
        let mut code = vec![1];
        push_unsigned_leb(body.len() as u32, &mut code);
        code.extend_from_slice(&body);
        push_section(&mut wasm, 10, &code);

        let mut data = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(manifest.len() as u32, &mut data);
        data.extend_from_slice(manifest.as_bytes());
        push_section(&mut wasm, 11, &data);
        wasm
    }

    #[test]
    fn capability_refresh_projects_both_scopes_and_runtime_state() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-capability-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let global_threadlane = root.join("global-threadlane");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let global_source = root.join("global.wasm");
        let project_source = root.join("project.wasm");
        fs::write(&global_source, manifest_wasm("shared_ext", "1.0.0")).unwrap();
        fs::write(&project_source, manifest_wasm("shared_ext", "2.0.0")).unwrap();

        let manager = ExtensionManager::new(Some(global_threadlane.clone()), Some(project.clone()));
        manager
            .install_from_wasm(&global_source, ExtensionScope::Global)
            .unwrap();
        manager
            .install_from_wasm(&project_source, ExtensionScope::Project)
            .unwrap();
        let global_threadlane = global_threadlane.canonicalize().unwrap();
        let project = project.canonicalize().unwrap();

        let mut capabilities = CapabilityState::default();
        let records = manager.discover();
        capabilities.refresh_records(&records);

        let global = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Global)
            .unwrap();
        assert_eq!(global.id, "shared_ext");
        assert_eq!(global.name, "shared_ext");
        assert_eq!(global.version, "1.0.0");
        assert_eq!(
            global.module_path,
            global_threadlane.join("extensions/shared_ext.wasm")
        );
        assert!(global.enabled);
        assert!(!global.effective);
        assert_eq!(global.scope_status(), "Global · Overridden");

        let project_row = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Project)
            .unwrap();
        assert_eq!(
            project_row.module_path,
            project.join(".threadlane/extensions/shared_ext.wasm")
        );
        assert_eq!(project_row.version, "2.0.0");
        assert!(project_row.enabled);
        assert!(project_row.effective);
        assert_eq!(project_row.scope_status(), "Project · Active");

        let project_record = records
            .iter()
            .find(|record| record.scope() == ExtensionScope::Project)
            .unwrap();
        manager.set_enabled(project_record, false).unwrap();
        capabilities.refresh_records(&manager.discover());
        let project_row = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Project)
            .unwrap();
        assert_eq!(project_row.scope_status(), "Project · Disabled");
        let global = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Global)
            .unwrap();
        assert_eq!(global.scope_status(), "Global · Active");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn visible_extension_identity_rejects_replaced_manifest() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-extension-identity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = root.join("identity.wasm");
        fs::write(&source, manifest_wasm("identity_ext", "1.0.0")).unwrap();
        let manager = ExtensionManager::new(None, Some(project));
        manager
            .install_from_wasm(&source, ExtensionScope::Project)
            .unwrap();
        let record = manager.discover().into_iter().next().unwrap();
        let visible = CapabilityExtensionRow {
            id: record.id().to_owned(),
            name: record.name().to_owned(),
            version: record.version().to_owned(),
            module_path: record.module_path().to_path_buf(),
            scope: record.scope(),
            enabled: record.is_enabled(),
            effective: record.is_effective(),
        };

        assert!(visible.matches_record(&record));

        let mut replaced_name = visible.clone();
        replaced_name.name = "replacement_ext".to_owned();
        assert!(!replaced_name.matches_record(&record));

        let mut replaced_version = visible;
        replaced_version.version = "2.0.0".to_owned();
        assert!(!replaced_version.matches_record(&record));

        fs::remove_dir_all(root).unwrap();
    }
}

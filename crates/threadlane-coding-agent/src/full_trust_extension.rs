use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustStore {
    pub approved_revisions: HashMap<String, String>,
}

impl TrustStore {
    pub fn load_from_file(file: &Path) -> Self {
        if file.exists() {
            if let Ok(contents) = fs::read_to_string(file) {
                if let Ok(store) = serde_json::from_str::<TrustStore>(&contents) {
                    return store;
                }
            }
        }
        Self {
            approved_revisions: HashMap::new(),
        }
    }

    pub fn save_to_file(&self, file: &Path) -> Result<(), String> {
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize trust store: {e}"))?;
        let tmp = file.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| format!("Failed to write trust store: {e}"))?;
        fs::rename(tmp, file).map_err(|e| format!("Failed to save trust store: {e}"))?;
        Ok(())
    }

    pub fn is_trusted(&self, package_id: &str, revision: &str) -> bool {
        self.approved_revisions
            .get(package_id)
            .map(|r| r == revision)
            .unwrap_or(false)
    }

    pub fn approve(&mut self, package_id: String, revision: String) {
        self.approved_revisions.insert(package_id, revision);
    }

    pub fn revoke(&mut self, package_id: &str) {
        self.approved_revisions.remove(package_id);
    }
}

pub fn compute_executable_revision(exe_path: &Path) -> Result<String, String> {
    let bytes = fs::read(exe_path)
        .map_err(|e| format!("Failed to read executable '{}': {e}", exe_path.display()))?;
    Ok(format!("{:x}", md5::compute(&bytes)))
}

pub struct FullTrustRunner {
    pub package_id: String,
    pub exe_path: PathBuf,
    pub revision: String,
}

impl FullTrustRunner {
    pub fn new(package_id: String, exe_path: PathBuf) -> Result<Self, String> {
        if !exe_path.exists() {
            return Err(format!(
                "Executable path does not exist: {}",
                exe_path.display()
            ));
        }
        let revision = compute_executable_revision(&exe_path)?;
        Ok(Self {
            package_id,
            exe_path,
            revision,
        })
    }

    pub fn execute_request(&self, request_json: &str, trust_file: &Path) -> Result<String, String> {
        let store = TrustStore::load_from_file(trust_file);
        if !store.is_trusted(&self.package_id, &self.revision) {
            return Err(format!(
                "Security Denial: Full-trust extension for package '{}' (revision {}) is not approved. Explicit user approval is required.",
                self.package_id, self.revision
            ));
        }

        let mut child = Command::new(&self.exe_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to launch full-trust extension process: {e}"))?;

        if let Some(ref mut stdin) = child.stdin {
            writeln!(stdin, "{request_json}")
                .map_err(|e| format!("Failed to send input to extension process: {e}"))?;
        }

        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut response_line = String::new();
        stdout
            .read_line(&mut response_line)
            .map_err(|e| format!("Failed to read line from extension process: {e}"))?;

        let status = child
            .wait()
            .map_err(|e| format!("Error waiting on extension process: {e}"))?;

        if !status.success() {
            return Err(format!(
                "Full-trust extension process exited with status: {status}"
            ));
        }

        Ok(response_line.trim().to_string())
    }
}

use crate::full_trust_extension::{compute_executable_revision, TrustStore};
use crate::packages::{PackageManager, PackageRecord};
use crate::skills::{SkillManager, SkillMetadata};
use crate::wasi_extension::WasiExtensionManager;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub is_full_trust: bool,
    pub enabled: bool,
    pub is_valid: bool,
    pub validation_error: Option<String>,
    pub revision: Option<String>,
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCatalog {
    pub skills: Vec<SkillMetadata>,
    pub extensions: Vec<ExtensionMetadata>,
    pub packages: Vec<PackageRecord>,
}

impl CapabilityCatalog {
    pub fn discover(project_root: Option<&Path>, global_dir: &Path) -> Self {
        // Discover skills
        let mut skill_mgr = SkillManager::new();
        skill_mgr.discover_skills(project_root);
        let skills = skill_mgr.list_skills();

        // Discover packages
        let pkg_mgr = PackageManager::new(global_dir.to_path_buf());
        let packages = pkg_mgr.list_packages(project_root);

        // Discover extensions (both WASM and Full-Trust)
        let mut extensions = Vec::new();

        // Sandboxed WASM extensions
        if let Some(proj) = project_root {
            let mut wasi_mgr = WasiExtensionManager::for_project(proj);
            wasi_mgr.discover_and_load(proj);
            for (id, ext) in wasi_mgr.get_extensions() {
                extensions.push(ExtensionMetadata {
                    id: id.clone(),
                    name: ext.manifest.name.clone(),
                    path: ext.file_path.clone().unwrap_or_default(),
                    is_full_trust: false,
                    enabled: true,
                    is_valid: true,
                    validation_error: None,
                    revision: None,
                    is_trusted: true, // WASM sandboxed
                });
            }
        }

        // Full-trust extensions from packages
        let trust_file = global_dir.join("state/trust.json");
        let trust_store = TrustStore::load_from_file(&trust_file);

        for pkg in &packages {
            if let Some(ref exe_rel) = pkg.manifest.full_trust_executable {
                let exe_path = pkg.root_dir.join(exe_rel);
                let (is_valid, err, rev) = match compute_executable_revision(&exe_path) {
                    Ok(r) => (true, None, Some(r)),
                    Err(e) => (false, Some(e), None),
                };

                let trusted = if let Some(ref r) = rev {
                    trust_store.is_trusted(&pkg.manifest.id, r)
                } else {
                    false
                };

                extensions.push(ExtensionMetadata {
                    id: format!("{}-exe", pkg.manifest.id),
                    name: format!("{} (Executable)", pkg.manifest.name),
                    path: exe_path,
                    is_full_trust: true,
                    enabled: trusted && is_valid,
                    is_valid,
                    validation_error: err,
                    revision: rev,
                    is_trusted: trusted,
                });
            }
        }

        Self {
            skills,
            extensions,
            packages,
        }
    }
}

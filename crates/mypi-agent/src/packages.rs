use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub skills: Option<Vec<String>>,
    pub extensions: Option<Vec<String>>,
    pub full_trust_executable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRecord {
    pub manifest: PackageManifest,
    pub scope: PackageScope,
    pub root_dir: PathBuf,
    pub enabled: bool,
    pub revision: String,
}

pub struct PackageManager {
    global_dir: PathBuf,
}

impl PackageManager {
    pub fn new(global_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(global_dir.join("packages"));
        Self { global_dir }
    }

    pub fn list_packages(&self, project_root: Option<&Path>) -> Vec<PackageRecord> {
        let mut packages = Vec::new();

        // Scan global packages: ~/.mypi/packages/
        self.scan_packages_dir(&self.global_dir.join("packages"), PackageScope::Global, &mut packages);

        // Scan project packages: <project>/.mypi/packages/
        if let Some(proj) = project_root {
            self.scan_packages_dir(&proj.join(".mypi/packages"), PackageScope::Project, &mut packages);
        }

        packages
    }

    fn scan_packages_dir(&self, dir: &Path, scope: PackageScope, out: &mut Vec<PackageRecord>) {
        if !dir.is_dir() {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let manifest_file = path.join("mypi-package.json");
                if manifest_file.exists() {
                    if let Ok(contents) = fs::read_to_string(&manifest_file) {
                        if let Ok(manifest) = serde_json::from_str::<PackageManifest>(&contents) {
                            let rev = compute_dir_revision(&path);
                            out.push(PackageRecord {
                                manifest,
                                scope,
                                root_dir: path,
                                enabled: true,
                                revision: rev,
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn install_from_local(&self, source_path: &Path, scope: PackageScope, project_root: Option<&Path>) -> Result<PackageRecord, String> {
        let manifest_file = source_path.join("mypi-package.json");
        if !manifest_file.exists() {
            return Err("Source directory does not contain 'mypi-package.json'".into());
        }

        let contents = fs::read_to_string(&manifest_file)
            .map_err(|e| format!("Failed to read package manifest: {e}"))?;
        let manifest: PackageManifest = serde_json::from_str(&contents)
            .map_err(|e| format!("Invalid mypi-package.json manifest: {e}"))?;

        let target_dir = match scope {
            PackageScope::Global => self.global_dir.join("packages").join(&manifest.id),
            PackageScope::Project => {
                let proj = project_root.ok_or_else(|| "Project root required for project-scoped package".to_string())?;
                proj.join(".mypi/packages").join(&manifest.id)
            }
        };

        if target_dir.exists() {
            let _ = fs::remove_dir_all(&target_dir);
        }
        copy_dir_recursive(source_path, &target_dir)?;

        let rev = compute_dir_revision(&target_dir);
        Ok(PackageRecord {
            manifest,
            scope,
            root_dir: target_dir,
            enabled: true,
            revision: rev,
        })
    }

    pub fn remove_package(&self, package_id: &str, scope: PackageScope, project_root: Option<&Path>) -> Result<(), String> {
        let target_dir = match scope {
            PackageScope::Global => self.global_dir.join("packages").join(package_id),
            PackageScope::Project => {
                let proj = project_root.ok_or_else(|| "Project root required for project-scoped package".to_string())?;
                proj.join(".mypi/packages").join(package_id)
            }
        };

        if target_dir.exists() {
            fs::remove_dir_all(&target_dir)
                .map_err(|e| format!("Failed to remove package '{package_id}': {e}"))?;
        }
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn compute_dir_revision(dir: &Path) -> String {
    let mut total_bytes = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                if let Ok(data) = fs::read(entry.path()) {
                    total_bytes.extend_from_slice(&data);
                }
            }
        }
    }
    format!("{:x}", md5::compute(total_bytes))
}

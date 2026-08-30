use std::path::PathBuf;
use threadlane_protocol::capabilities::*;
use threadlane_wasi::{default_global_threadlane_dir, ExtensionManager};

#[derive(Clone, Default)]
pub struct ExtensionService;

impl ExtensionService {
    pub fn new() -> Self {
        Self
    }

    fn manager(project_path: Option<&str>) -> ExtensionManager {
        ExtensionManager::new(
            default_global_threadlane_dir(),
            project_path.map(PathBuf::from),
        )
    }

    fn record(record: &threadlane_wasi::ExtensionRecord) -> ExtensionRecord {
        ExtensionRecord {
            id: record.id().to_owned(),
            name: record.name().to_owned(),
            version: record.version().to_owned(),
            scope: match record.scope() {
                threadlane_wasi::ExtensionScope::Global => ExtensionScope::Global,
                threadlane_wasi::ExtensionScope::Project => ExtensionScope::Project,
            },
            module_path: record.module_path().to_path_buf(),
            enabled: record.is_enabled(),
            effective: record.is_effective(),
        }
    }

    fn wasi_scope(scope: ExtensionScope) -> threadlane_wasi::ExtensionScope {
        match scope {
            ExtensionScope::Global => threadlane_wasi::ExtensionScope::Global,
            ExtensionScope::Project => threadlane_wasi::ExtensionScope::Project,
        }
    }

    pub fn list(&self, req: ListExtensionsRequest) -> Result<ListExtensionsResponse, String> {
        let manager = Self::manager(req.project_path.as_deref());
        Ok(ListExtensionsResponse {
            extensions: manager.discover().iter().map(Self::record).collect(),
        })
    }

    pub fn install(
        &self,
        req: InstallExtensionRequest,
    ) -> Result<InstallExtensionResponse, String> {
        let path = std::env::temp_dir().join(format!(
            "threadlane-extension-{}-{}.wasm",
            std::process::id(),
            uuid_like()
        ));
        std::fs::write(&path, &req.wasm_bytes)
            .map_err(|e| format!("Failed to stage extension upload: {e}"))?;
        let result = Self::manager(req.project_path.as_deref())
            .install_from_wasm(&path, Self::wasi_scope(req.scope))
            .map(|record| InstallExtensionResponse {
                extension: Self::record(&record),
            });
        let _ = std::fs::remove_file(&path);
        result
    }

    pub fn set_enabled(&self, req: SetExtensionEnabledRequest) -> Result<(), String> {
        let manager = Self::manager(req.project_path.as_deref());
        let record = manager
            .discover()
            .into_iter()
            .find(|record| record.id() == req.id && Self::record(record).scope == req.scope)
            .ok_or_else(|| "Extension not found. Please refresh.".to_string())?;
        manager.set_enabled(&record, req.enabled)
    }

    pub fn remove(&self, req: RemoveExtensionRequest) -> Result<(), String> {
        let manager = Self::manager(req.project_path.as_deref());
        let record = manager
            .discover()
            .into_iter()
            .find(|record| record.id() == req.id && Self::record(record).scope == req.scope)
            .ok_or_else(|| "Extension not found. Please refresh.".to_string())?;
        manager.remove(&record)
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".into())
}

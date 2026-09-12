use std::path::PathBuf;

pub(crate) fn global_threadlane_dir() -> PathBuf {
    threadlane_project::default_global_threadlane_dir().unwrap_or_else(|| {
        threadlane_runtime::dirs_home()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".threadlane")
    })
}

pub(crate) use threadlane_project::load_project_registry;

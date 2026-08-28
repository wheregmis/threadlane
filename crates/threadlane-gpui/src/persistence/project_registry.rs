use std::path::PathBuf;

pub(crate) fn global_threadlane_dir() -> PathBuf {
    threadlane_protocol::project::default_global_threadlane_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".threadlane")
    })
}

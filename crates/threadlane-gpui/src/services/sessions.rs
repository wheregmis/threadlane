//! Session runtime client adapter for the GPUI frontend.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRuntimeStatus {
    Idle,
    Ready,
    Working,
    Generating,
    AwaitingApproval,
    Interrupted,
    Error(String),
}

pub struct SessionRuntime {
    pub session_id: String,
    pub work_dir: PathBuf,
    pub session_file: PathBuf,
    pub is_generating: AtomicBool,
    pub selected_model: String,
    pub system_prompt: Option<String>,
    pub harness_error: Option<String>,
}

impl SessionRuntime {
    pub fn new(session_id: String, work_dir: PathBuf, session_file: PathBuf) -> Self {
        Self {
            session_id,
            work_dir,
            session_file,
            is_generating: AtomicBool::new(false),
            selected_model: String::new(),
            system_prompt: None,
            harness_error: None,
        }
    }

    pub fn is_generating(&self) -> bool {
        self.is_generating.load(Ordering::Relaxed)
    }

    pub fn begin_generation(&self) -> Result<(), String> {
        self.is_generating.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn finish_generation(&self, _error: Option<String>) {
        self.is_generating.store(false, Ordering::Relaxed);
    }

    pub fn status(&self) -> SessionRuntimeStatus {
        if self.is_generating() {
            SessionRuntimeStatus::Working
        } else {
            SessionRuntimeStatus::Ready
        }
    }

    pub fn try_set_needle_enabled(&self, _enabled: bool) -> Result<(), String> {
        Ok(())
    }

    pub async fn set_model_roles(&self, _roles: threadlane_protocol::ModelRoles) {}
}


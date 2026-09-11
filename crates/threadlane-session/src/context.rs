use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub(crate) context_files: Vec<PathBuf>,
    pub(crate) memory_content: Option<String>,
}

impl ProjectContext {
    pub(crate) fn discover(start_dir: &Path) -> Self {
        let mut current = start_dir.to_path_buf();
        let mut context_files = Vec::new();

        let memory_candidate = start_dir.join(".threadlane").join("memory.md");
        let memory_content = if memory_candidate.is_file() {
            std::fs::read_to_string(&memory_candidate)
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
        } else {
            None
        };

        loop {
            for filename in &["AGENTS.md", "THREADLANE.md", ".threadlane/AGENTS.md"] {
                let candidate = current.join(filename);
                if candidate.is_file() {
                    context_files.push(candidate);
                }
            }

            if !current.pop() {
                break;
            }
        }

        Self {
            context_files,
            memory_content,
        }
    }
}

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub context_files: Vec<PathBuf>,
    pub combined_instructions: String,
}

impl ProjectContext {
    pub fn discover(start_dir: &Path) -> Self {
        let mut current = start_dir.to_path_buf();
        let mut context_files = Vec::new();
        let mut instructions = String::new();

        loop {
            for filename in &["AGENTS.md", "MYPI.md", ".mypi/AGENTS.md"] {
                let candidate = current.join(filename);
                if candidate.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&candidate) {
                        instructions.push_str(&format!(
                            "\n--- Context from {} ---\n{}\n",
                            candidate.display(),
                            content.trim()
                        ));
                        context_files.push(candidate);
                    }
                }
            }

            if !current.pop() {
                break;
            }
        }

        Self {
            context_files,
            combined_instructions: instructions,
        }
    }
}

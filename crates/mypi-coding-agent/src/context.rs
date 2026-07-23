use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInstruction {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub context_files: Vec<PathBuf>,
    pub instructions: Vec<ProjectInstruction>,
    pub combined_instructions: String,
}

impl ProjectContext {
    pub fn discover(start_dir: &Path) -> Self {
        let mut current = start_dir.to_path_buf();
        let mut context_files = Vec::new();
        let mut instruction_entries = Vec::new();
        let mut combined_instructions = String::new();

        loop {
            for filename in &["AGENTS.md", "MYPI.md", ".mypi/AGENTS.md"] {
                let candidate = current.join(filename);
                if candidate.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&candidate) {
                        let content = content.trim().to_string();
                        combined_instructions.push_str(&format!(
                            "\n--- Context from {} ---\n{}\n",
                            candidate.display(),
                            content
                        ));
                        context_files.push(candidate.clone());
                        instruction_entries.push(ProjectInstruction {
                            path: candidate,
                            content,
                        });
                    }
                }
            }

            if !current.pop() {
                break;
            }
        }

        Self {
            context_files,
            instructions: instruction_entries,
            combined_instructions,
        }
    }
}

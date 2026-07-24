//! Execution and tool safety policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    FullAccess,
    ReadOnly,
}

impl ToolPolicy {
    pub fn allows_writes(self) -> bool {
        matches!(self, ToolPolicy::FullAccess)
    }
}

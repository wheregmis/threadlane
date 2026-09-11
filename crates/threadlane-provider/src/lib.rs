pub mod antigravity;
pub mod convert;
pub mod credentials;
pub mod exec;
pub mod model_registry;
pub mod openai;
pub mod opencode;
pub mod router;
pub(crate) mod title_generator;
pub mod traits;

pub use convert::{
    compaction_checkpoint_text, compaction_summary_text, convert_to_codex_llm, convert_to_llm,
    MAX_CONTEXT_SNAPSHOT_INDEX_CHARS, MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES,
};
pub use credentials::{
    AntigravityCredentialSnapshot, AntigravityCredentialSource, CodexAccountResolver,
    CodexBackupAccount, NoopAntigravityCredentials, NoopCodexResolver, SharedAntigravityCredentials,
    SharedCodexResolver,
};
pub use exec::get_runtime;
pub use model_registry::{
    builtin_models, context_window_for, effective_api_effort, effective_effort, find_model,
    load_models_from_file, merge_models, registry_for_project, supported_efforts_for,
    update_discovered_models, ModelInfo,
};
pub use router::{
    is_antigravity_model, is_opencode_model, normalize_commit_message, ProviderClient,
};

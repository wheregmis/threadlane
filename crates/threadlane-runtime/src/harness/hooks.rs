use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    BeforeRun,
    BeforeResume,
    AfterRun,
    BeforeContext,
    BeforeRequest,
    AfterRequest,
    BeforePayload,
    AfterPayload,
    BeforeResponse,
    AfterResponse,
    BeforeTool,
    AfterTool,
    BeforeCompaction,
    AfterCompaction,
    BeforeNavigation,
    AfterNavigation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookContext {
    pub session_id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub resume_data: Option<String>,
    /// Tool-specific context populated for BeforeTool / AfterTool hooks.
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_arguments: Option<String>,
    /// Set for AfterTool hooks.
    pub tool_result_content: Option<String>,
    pub tool_result_is_error: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookEffect {
    pub(crate) override_content: Option<String>,
    /// Appended to an already successful tool result without replacing it.
    pub append_content: Option<String>,
    pub(crate) override_is_error: Option<bool>,
    pub(crate) terminate: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookRun {
    pub(crate) failures: Vec<HookFailure>,
    pub(crate) effect: HookEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    pub id: String,
    pub message: String,
}

pub type HookHandler = Arc<
    dyn Fn(HookContext) -> Pin<Box<dyn Future<Output = Result<HookEffect, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct RegisteredHook {
    kind: HookKind,
    id: String,
    handler: HookHandler,
}

#[derive(Default)]
struct HookRegistryState {
    hooks: RwLock<Vec<RegisteredHook>>,
    resume_data: RwLock<HashMap<String, String>>,
}

/// Session-scoped asynchronous hook registry.
///
/// Cloning the registry shares its registrations and resume data. This lets
/// the live agent loop and durable harness journal dispatch the same hooks,
/// including when the journal is reopened for an individual append.
#[derive(Clone, Default)]
pub struct HookRegistry {
    state: Arc<HookRegistryState>,
}

impl HookRegistry {
    pub fn register(
        &self,
        kind: HookKind,
        id: impl Into<String>,
        handler: HookHandler,
    ) -> Result<(), HookFailure> {
        let id = id.into();
        let mut hooks = self
            .state
            .hooks
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if id.trim().is_empty() || hooks.iter().any(|hook| hook.id == id) {
            return Err(HookFailure {
                id,
                message: "hook id must be unique and non-empty".into(),
            });
        }
        hooks.push(RegisteredHook { kind, id, handler });
        Ok(())
    }

    async fn run_handlers(
        &self,
        kind: HookKind,
        context: &HookContext,
        use_resume_data: bool,
    ) -> HookRun {
        let hooks = self
            .state
            .hooks
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|hook| hook.kind == kind)
            .cloned()
            .collect::<Vec<_>>();
        let resume_data = use_resume_data.then(|| {
            self.state
                .resume_data
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        });
        let mut run = HookRun::default();
        for hook in hooks {
            let mut context = context.clone();
            if let Some(resume_data) = resume_data.as_ref() {
                context.resume_data = resume_data.get(&hook.id).cloned();
            }
            match (hook.handler)(context).await {
                Ok(effect) => {
                    if effect.override_content.is_some() {
                        run.effect.override_content = effect.override_content;
                    }
                    if effect.append_content.is_some() {
                        run.effect.append_content = effect.append_content;
                    }
                    if effect.override_is_error.is_some() {
                        run.effect.override_is_error = effect.override_is_error;
                    }
                    if effect.terminate.is_some() {
                        run.effect.terminate = effect.terminate;
                    }
                }
                Err(message) => run.failures.push(HookFailure {
                    id: hook.id,
                    message,
                }),
            }
        }
        run
    }

    /// Registers a handler, replacing an existing handler with the same stable
    /// ID. Use this for built-in session services that are recreated on reload.
    pub(crate) fn replace(
        &self,
        kind: HookKind,
        id: impl Into<String>,
        handler: HookHandler,
    ) -> Result<(), HookFailure> {
        let id = id.into();
        let mut hooks = self
            .state
            .hooks
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if id.trim().is_empty() {
            return Err(HookFailure {
                id,
                message: "hook id must be unique and non-empty".into(),
            });
        }
        if let Some(existing) = hooks.iter_mut().find(|hook| hook.id == id) {
            existing.kind = kind;
            existing.handler = handler;
        } else {
            hooks.push(RegisteredHook { kind, id, handler });
        }
        Ok(())
    }

    pub async fn run(&self, kind: HookKind, context: &HookContext) -> Vec<HookFailure> {
        self.run_handlers(kind, context, false).await.failures
    }

    pub async fn run_before_tool(&self, context: &HookContext) -> Result<(), Vec<HookFailure>> {
        let failures = self.run(HookKind::BeforeTool, context).await;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    pub(crate) async fn run_after_tool(&self, context: &HookContext) -> HookRun {
        self.run_handlers(HookKind::AfterTool, context, false).await
    }

    pub fn set_resume_data(
        &self,
        hook_id: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<(), HookFailure> {
        let hook_id = hook_id.into();
        let hooks = self
            .state
            .hooks
            .read()
            .unwrap_or_else(|error| error.into_inner());
        if !hooks.iter().any(|hook| hook.id == hook_id) {
            return Err(HookFailure {
                id: hook_id,
                message: "resume data requires a registered hook".into(),
            });
        }
        drop(hooks);
        self.state
            .resume_data
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(hook_id, data.into());
        Ok(())
    }

    pub fn clear_resume_data(&self, hook_id: &str) {
        self.state
            .resume_data
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(hook_id);
    }

    pub(crate) fn restore_resume_data(
        &self,
        persisted: &std::collections::BTreeMap<String, String>,
    ) {
        let hooks = self
            .state
            .hooks
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let resume_data = persisted
            .iter()
            .filter(|(hook_id, _)| hooks.iter().any(|hook| &hook.id == *hook_id))
            .map(|(hook_id, data)| (hook_id.clone(), data.clone()))
            .collect();
        drop(hooks);
        *self
            .state
            .resume_data
            .write()
            .unwrap_or_else(|error| error.into_inner()) = resume_data;
    }

    pub async fn run_before_resume(&self, context: &HookContext) -> Vec<HookFailure> {
        self.run_handlers(HookKind::BeforeResume, context, true)
            .await
            .failures
    }
}

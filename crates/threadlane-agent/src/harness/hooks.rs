use std::collections::HashMap;
use std::sync::Arc;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookContext {
    pub session_id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub resume_data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    pub id: String,
    pub message: String,
}

pub type HookHandler = Arc<dyn Fn(&HookContext) -> Result<(), String> + Send + Sync>;

struct RegisteredHook {
    kind: HookKind,
    id: String,
    handler: HookHandler,
}

#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<RegisteredHook>,
    resume_data: HashMap<String, String>,
}

impl HookRegistry {
    pub fn register(
        &mut self,
        kind: HookKind,
        id: impl Into<String>,
        handler: HookHandler,
    ) -> Result<(), HookFailure> {
        let id = id.into();
        if id.trim().is_empty() || self.hooks.iter().any(|hook| hook.id == id) {
            return Err(HookFailure {
                id,
                message: "hook id must be unique and non-empty".into(),
            });
        }
        self.hooks.push(RegisteredHook { kind, id, handler });
        Ok(())
    }

    pub fn run(&self, kind: HookKind, context: &HookContext) -> Vec<HookFailure> {
        self.hooks
            .iter()
            .filter(|hook| hook.kind == kind)
            .filter_map(|hook| {
                (hook.handler)(context).err().map(|message| HookFailure {
                    id: hook.id.clone(),
                    message,
                })
            })
            .collect()
    }

    pub fn run_before_tool(&self, context: &HookContext) -> Result<(), Vec<HookFailure>> {
        let failures = self.run(HookKind::BeforeTool, context);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    pub fn set_resume_data(
        &mut self,
        hook_id: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<(), HookFailure> {
        let hook_id = hook_id.into();
        if !self.hooks.iter().any(|hook| hook.id == hook_id) {
            return Err(HookFailure {
                id: hook_id,
                message: "resume data requires a registered hook".into(),
            });
        }
        self.resume_data.insert(hook_id, data.into());
        Ok(())
    }

    pub fn clear_resume_data(&mut self, hook_id: &str) {
        self.resume_data.remove(hook_id);
    }

    pub fn restore_resume_data(&mut self, persisted: &std::collections::BTreeMap<String, String>) {
        self.resume_data = persisted
            .iter()
            .filter(|(hook_id, _)| self.hooks.iter().any(|hook| &hook.id == *hook_id))
            .map(|(hook_id, data)| (hook_id.clone(), data.clone()))
            .collect();
    }

    pub fn run_before_resume(&self, context: &HookContext) -> Vec<HookFailure> {
        self.hooks
            .iter()
            .filter(|hook| hook.kind == HookKind::BeforeResume)
            .filter_map(|hook| {
                let mut context = context.clone();
                context.resume_data = self.resume_data.get(&hook.id).cloned();
                (hook.handler)(&context).err().map(|message| HookFailure {
                    id: hook.id.clone(),
                    message,
                })
            })
            .collect()
    }
}

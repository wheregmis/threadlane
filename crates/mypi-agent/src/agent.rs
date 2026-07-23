use crate::compaction::{
    compact_messages, compact_messages_to_token_budget, should_auto_compact, CompactionOptions,
    AUTO_COMPACTION_KEEP_RECENT_TOKENS,
};
use crate::events::AgentEvent;
use crate::loop_engine::AgentLoop;
use crate::types::{AgentMessage, AgentState, ReasoningEffort, ToolExecutionMode};
use tokio::sync::broadcast;

pub struct Agent {
    pub loop_engine: AgentLoop,
}

impl Agent {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            loop_engine: AgentLoop::new(api_key, account_id, model),
        }
    }

    pub fn set_tool_execution_mode(&mut self, mode: ToolExecutionMode) {
        self.loop_engine.tool_execution_mode = mode;
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.loop_engine.set_prompt_cache_key(key);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.loop_engine.subscribe()
    }

    pub fn steer(&mut self, message: AgentMessage) {
        self.loop_engine.steer(message);
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        self.loop_engine.follow_up(message);
    }

    pub async fn prompt(&mut self, text: &str) {
        self.loop_engine.run_prompt(text).await;
    }

    /// Prompts with a complete user message, including any image attachments.
    pub async fn prompt_message(&mut self, message: AgentMessage) {
        self.loop_engine.run_prompt_message(message).await;
    }

    pub async fn run_follow_up(&mut self) {
        self.loop_engine.run_follow_up().await;
    }

    /// Updates both the cached prompt and the system message sent to the provider.
    pub async fn set_system_prompt(&mut self, system_prompt: String) {
        let mut state = self.loop_engine.state.lock().await;
        state.system_prompt = system_prompt.clone();

        if let Some(AgentMessage::System { content }) = state.messages.first_mut() {
            *content = system_prompt;
        } else {
            state.messages.insert(
                0,
                AgentMessage::System {
                    content: system_prompt,
                },
            );
        }
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.loop_engine.state.lock().await.reasoning_effort = effort;
    }

    pub async fn get_state(&self) -> AgentState {
        let st = self.loop_engine.state.lock().await;
        st.clone()
    }

    pub async fn compact_history(&self, options: Option<CompactionOptions>) -> bool {
        let mut st = self.loop_engine.state.lock().await;
        let compacted = match options {
            Some(options) => compact_messages(&st.messages, &options),
            None => {
                let by_tokens = compact_messages_to_token_budget(
                    &st.messages,
                    AUTO_COMPACTION_KEEP_RECENT_TOKENS,
                );
                if by_tokens.len() == st.messages.len() {
                    compact_messages(&st.messages, &CompactionOptions::default())
                } else {
                    by_tokens
                }
            }
        };
        let changed = compacted.len() != st.messages.len();
        st.messages = compacted;
        changed
    }

    pub async fn auto_compact_history(&self) -> bool {
        let mut st = self.loop_engine.state.lock().await;
        if !should_auto_compact(&st.messages) {
            return false;
        }
        let compacted =
            compact_messages_to_token_budget(&st.messages, AUTO_COMPACTION_KEEP_RECENT_TOKENS);
        let changed = compacted.len() != st.messages.len();
        st.messages = compacted;
        changed
    }
}

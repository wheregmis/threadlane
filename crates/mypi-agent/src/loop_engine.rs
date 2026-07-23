use crate::events::AgentEvent;
use crate::hooks::{
    AfterToolCallHook, BeforeToolCallHook, ShouldStopAfterTurnHook, ToolExecutor,
    TransformContextHook,
};
use crate::queue::PendingMessageQueue;
use crate::types::{
    AgentMessage, AgentState, AgentToolCall, AgentToolResult, QueueMode, ToolExecutionMode,
};
use mypi_provider::openai::{OpenAIClient, StreamEvent, ToolCall};
use mypi_tools::{execute_tool, execute_tool_in_workspace, get_available_tools, get_codex_tools};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};

pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|msg| match msg {
            AgentMessage::System { content } => Some(serde_json::json!({
                "role": "system",
                "content": content
            })),
            AgentMessage::User { content } => Some(serde_json::json!({
                "role": "user",
                "content": content
            })),
            AgentMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut map = serde_json::Map::new();
                map.insert("role".into(), "assistant".into());
                if let Some(c) = content {
                    map.insert("content".into(), c.clone().into());
                }
                if let Some(t) = tool_calls {
                    map.insert(
                        "tool_calls".into(),
                        serde_json::to_value(t).unwrap_or_default(),
                    );
                }
                Some(Value::Object(map))
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                ..
            } => {
                let id_str = if tool_call_id.is_empty() {
                    "call_0"
                } else {
                    tool_call_id
                };
                Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id_str,
                    "name": name,
                    "content": content
                }))
            }
            AgentMessage::Custom { .. } => None,
        })
        .collect()
}

pub fn convert_to_codex_llm(messages: &[AgentMessage]) -> (String, Vec<Value>) {
    let mut instructions = String::new();
    let mut items = Vec::new();

    for msg in messages {
        match msg {
            AgentMessage::System { content } => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(content);
            }
            AgentMessage::User { content } => {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": content }]
                }));
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(c) = content {
                    if !c.trim().is_empty() {
                        items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": c }]
                        }));
                    }
                }
                if let Some(t_calls) = tool_calls {
                    for tc in t_calls {
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": if tc.id.is_empty() { "call_0" } else { &tc.id },
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }));
                    }
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                let call_id = if tool_call_id.is_empty() {
                    "call_0"
                } else {
                    tool_call_id
                };
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content
                }));
            }
            AgentMessage::Custom { .. } => {}
        }
    }

    (instructions, items)
}

pub struct AgentLoop {
    pub state: Arc<Mutex<AgentState>>,
    pub api_key: String,
    pub account_id: Option<String>,
    pub tool_execution_mode: ToolExecutionMode,
    pub steering_queue: PendingMessageQueue,
    pub follow_up_queue: PendingMessageQueue,
    pub before_tool_call_hook: Option<Arc<dyn BeforeToolCallHook>>,
    pub after_tool_call_hook: Option<Arc<dyn AfterToolCallHook>>,
    pub transform_context_hook: Option<Arc<dyn TransformContextHook>>,
    pub should_stop_hook: Option<Arc<dyn ShouldStopAfterTurnHook>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub extension_manager: Option<Arc<dyn ToolExecutor>>,
    pub work_dir: Option<PathBuf>,
}

impl AgentLoop {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(500);
        let state = Arc::new(Mutex::new(AgentState::new(
            model,
            "You are mypi AI coding agent.",
        )));

        Self {
            state,
            api_key: api_key.into(),
            account_id,
            tool_execution_mode: ToolExecutionMode::Parallel,
            steering_queue: PendingMessageQueue::new(QueueMode::All),
            follow_up_queue: PendingMessageQueue::new(QueueMode::All),
            before_tool_call_hook: None,
            after_tool_call_hook: None,
            transform_context_hook: None,
            should_stop_hook: None,
            event_tx,
            extension_manager: None,
            work_dir: None,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn steer(&mut self, message: AgentMessage) {
        self.steering_queue.enqueue(message);
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        self.follow_up_queue.enqueue(message);
    }

    pub async fn run_prompt(&mut self, prompt: &str) {
        {
            let mut state = self.state.lock().await;
            state.messages.push(AgentMessage::User {
                content: prompt.to_string(),
            });
        }
        self.run_queued_turns().await;
    }

    /// Runs messages already placed in the follow-up queue without adding an
    /// artificial prompt. This lets host schedulers start queued work while
    /// the agent is idle.
    pub async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.has_items() {
            return;
        }
        let items = self.follow_up_queue.drain();
        let mut state = self.state.lock().await;
        state.messages.extend(items);
        drop(state);
        self.run_queued_turns().await;
    }

    async fn run_queued_turns(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        let mut turn_number = 0;

        loop {
            turn_number += 1;

            // Drain steering queue items
            if self.steering_queue.has_items() {
                let items = self.steering_queue.drain();
                let mut state = self.state.lock().await;
                state.messages.extend(items);
            }

            // Apply context transformation hook if set
            if let Some(ref hook) = self.transform_context_hook {
                let msgs = {
                    let state = self.state.lock().await;
                    state.messages.clone()
                };
                let transformed = hook.transform_context(msgs).await;
                let mut state = self.state.lock().await;
                state.messages = transformed;
            }

            let _ = self.event_tx.send(AgentEvent::TurnStart { turn_number });

            let (api_payload, codex_payload) = {
                let state = self.state.lock().await;
                let api_msgs = convert_to_llm(&state.messages);
                let (instructions, codex_msgs) = convert_to_codex_llm(&state.messages);
                let mut tools = get_available_tools();
                tools.extend(state.tools.clone());
                if let Some(ref ext_mgr) = self.extension_manager {
                    tools.extend(ext_mgr.get_tool_schemas());
                }
                let mut codex_tools = get_codex_tools();
                if let Some(ref ext_mgr) = self.extension_manager {
                    codex_tools.extend(ext_mgr.get_tool_schemas());
                }
                (
                    serde_json::json!({
                        "model": state.model,
                        "messages": api_msgs,
                        "tools": tools,
                        "stream": true
                    }),
                    serde_json::json!({
                        "model": state.model,
                        "instructions": instructions,
                        "input": codex_msgs,
                        "store": false,
                        "stream": true,
                        // Reasoning deltas are opt-in on the Responses API.
                        // The GUI already renders them as a live Thinking row.
                        "reasoning": { "summary": "auto" },
                        "tools": codex_tools
                    }),
                )
            };

            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let api_key = self.api_key.clone();
            let account_id = self.account_id.clone();

            tokio::spawn(async move {
                let client = OpenAIClient::new(api_key, account_id);
                client
                    .stream_chat_completion(api_payload, codex_payload, stream_tx)
                    .await;
            });

            let _ = self.event_tx.send(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            let mut current_turn_text = String::new();
            let mut current_turn_reasoning = String::new();
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();

            while let Some(evt) = stream_rx.recv().await {
                match evt {
                    StreamEvent::ContentToken(token) => {
                        current_turn_text.push_str(&token);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ReasoningToken(token) => {
                        current_turn_reasoning.push_str(&token);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { tool_calls } => {
                        captured_tool_calls = tool_calls;
                        break;
                    }
                    StreamEvent::Error(err) => {
                        let _ = self
                            .event_tx
                            .send(AgentEvent::AgentError { error: err.clone() });
                        return;
                    }
                }
            }

            let assistant_msg = AgentMessage::Assistant {
                content: if current_turn_text.is_empty() {
                    None
                } else {
                    Some(current_turn_text.clone())
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
            };

            {
                let mut state = self.state.lock().await;
                if !current_turn_reasoning.trim().is_empty() {
                    state.messages.push(AgentMessage::Custom {
                        custom_type: "thinking".into(),
                        payload: serde_json::json!({ "text": current_turn_reasoning }),
                    });
                }
                state.messages.push(assistant_msg.clone());
            }

            let _ = self.event_tx.send(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                let _ = self.event_tx.send(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                if self.follow_up_queue.has_items() {
                    let items = self.follow_up_queue.drain();
                    let mut state = self.state.lock().await;
                    state.messages.extend(items);
                    continue;
                }
                break;
            }

            // Tool Execution
            let tool_results = self.execute_tools(&captured_tool_calls).await;

            let should_terminate = tool_results.iter().any(|r| r.terminate);

            let mut state = self.state.lock().await;
            for r in &tool_results {
                state.messages.push(AgentMessage::Tool {
                    tool_call_id: r.tool_call_id.clone(),
                    name: r.name.clone(),
                    content: r.content.clone(),
                    is_error: r.is_error,
                });
            }
            drop(state);

            let _ = self.event_tx.send(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if let Some(ref hook) = self.should_stop_hook {
                let state = self.state.lock().await;
                if hook
                    .should_stop_after_turn(turn_number, &tool_results, &state)
                    .await
                {
                    break;
                }
            }

            if should_terminate {
                break;
            }
        }

        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            usage: Default::default(),
        });
    }

    async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        let mut results = Vec::new();

        if self.tool_execution_mode == ToolExecutionMode::Sequential {
            for tc in tool_calls {
                let res = self.execute_single_tool(tc).await;
                results.push(res);
            }
        } else {
            // Parallel execution
            let mut handles = Vec::new();
            for tc in tool_calls {
                let tc_clone = tc.clone();
                let before_hook = self.before_tool_call_hook.clone();
                let after_hook = self.after_tool_call_hook.clone();
                let event_tx = self.event_tx.clone();
                let state = self.state.clone();
                let extension_manager = self.extension_manager.clone();
                let work_dir = self.work_dir.clone();

                handles.push(tokio::spawn(async move {
                    Self::run_tool_with_hooks(
                        tc_clone,
                        before_hook,
                        after_hook,
                        event_tx,
                        state,
                        extension_manager,
                        work_dir,
                    )
                    .await
                }));
            }

            for handle in handles {
                if let Ok(res) = handle.await {
                    results.push(res);
                }
            }
        }

        results
    }

    async fn execute_single_tool(&self, tc: &ToolCall) -> AgentToolResult {
        Self::run_tool_with_hooks(
            tc.clone(),
            self.before_tool_call_hook.clone(),
            self.after_tool_call_hook.clone(),
            self.event_tx.clone(),
            self.state.clone(),
            self.extension_manager.clone(),
            self.work_dir.clone(),
        )
        .await
    }

    async fn run_tool_with_hooks(
        tc: ToolCall,
        before_hook: Option<Arc<dyn BeforeToolCallHook>>,
        after_hook: Option<Arc<dyn AfterToolCallHook>>,
        event_tx: broadcast::Sender<AgentEvent>,
        state: Arc<Mutex<AgentState>>,
        extension_manager: Option<Arc<dyn ToolExecutor>>,
        work_dir: Option<PathBuf>,
    ) -> AgentToolResult {
        let arguments = normalize_tool_arguments(
            &tc.function.name,
            &tc.function.arguments,
            work_dir.as_deref(),
        );
        let agent_tool_call = AgentToolCall {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        };

        if let Some(ref hook) = before_hook {
            let st = state.lock().await;
            let check = hook.before_tool_call(&agent_tool_call, &st).await;
            if check.block {
                let res = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: check
                        .reason
                        .unwrap_or_else(|| "Tool execution blocked by hook".into()),
                    is_error: true,
                    terminate: false,
                };
                let _ = event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result: res.clone(),
                });
                return res;
            }
        }

        let _ = event_tx.send(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        });

        let raw_result = extension_manager
            .as_ref()
            .and_then(|manager| manager.execute_tool(&tc.function.name, &arguments))
            .unwrap_or_else(|| {
                Ok(match work_dir.as_deref() {
                    Some(dir) => execute_tool_in_workspace(&tc.function.name, &arguments, dir),
                    None => execute_tool(&tc.function.name, &arguments),
                })
            })
            .unwrap_or_else(|error| format!("Extension tool error: {error}"));
        let mut final_result = AgentToolResult {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            content: raw_result,
            is_error: false,
            terminate: false,
        };

        if let Some(ref hook) = after_hook {
            let st = state.lock().await;
            let override_res = hook
                .after_tool_call(&agent_tool_call, &final_result, &st)
                .await;
            if let Some(c) = override_res.override_content {
                final_result.content = c;
            }
            if let Some(err) = override_res.override_is_error {
                final_result.is_error = err;
            }
            if let Some(term) = override_res.terminate {
                final_result.terminate = term;
            }
        }

        let _ = event_tx.send(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            result: final_result.clone(),
        });

        final_result
    }
}

fn normalize_tool_arguments(
    name: &str,
    arguments: &str,
    work_dir: Option<&std::path::Path>,
) -> String {
    let Some(work_dir) = work_dir else {
        return arguments.to_string();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return arguments.to_string();
    };

    let workspace = work_dir.to_string_lossy().to_string();
    match name {
        "read_file" | "write_file" | "edit_file" | "list_dir" => {
            if object
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(str::is_empty)
            {
                object.insert("path".into(), Value::String(workspace));
            }
        }
        "run_command" => {
            if object
                .get("cwd")
                .and_then(Value::as_str)
                .map_or(true, str::is_empty)
            {
                object.insert("cwd".into(), Value::String(workspace));
            }
        }
        _ => {}
    }

    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

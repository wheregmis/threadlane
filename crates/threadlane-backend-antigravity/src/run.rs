use crate::{Profile, failed, spawn_error};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{collections::VecDeque, fmt, process::Stdio, time::Duration};
use threadlane_core::backend::{
    BackendError, BackendEvent as Event, BackendRun, NativeSession, RunRequest,
};
use threadlane_core::time::{Clock, SystemClock};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
};

pub(super) fn result_error(result: &Value) -> BackendError {
    let error = result["error"].as_str().unwrap_or("Antigravity run failed");
    let lower = error.to_lowercase();
    if ["RESOURCE_EXHAUSTED", "QUOTA_EXCEEDED"]
        .contains(&result["finished_reason"].as_str().unwrap_or(""))
        || [
            "quota exceeded",
            "quota reached",
            "rate limit",
            "usage limit",
        ]
        .iter()
        .any(|s| lower.contains(s))
    {
        BackendError::UsageLimit {
            resets_at: resets_in(&lower).map(|wait| SystemClock.now() + wait),
        }
    } else {
        failed(error)
    }
}

/// The wait in agy's "resets in 37m3s", if the text gives one.
fn resets_in(error: &str) -> Option<Duration> {
    const PREFIX: &str = "resets in ";
    let rest = &error[error.find(PREFIX)? + PREFIX.len()..];
    let (mut secs, mut n, mut any) = (0u64, 0u64, false);
    for c in rest.chars() {
        let unit = match c {
            '0'..='9' => {
                n = n.checked_mul(10)?.checked_add(u64::from(c.to_digit(10)?))?;
                continue;
            }
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => break,
        };
        secs = secs.checked_add(n.checked_mul(unit)?)?;
        (n, any) = (0, true);
    }
    (any && n == 0).then(|| Duration::from_secs(secs))
}

pub(super) struct Run {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    _profile: Profile,
    resume: Option<String>,
    prompt: Option<String>,
    pending: VecDeque<Event>,
    done: bool,
}
impl fmt::Debug for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AntigravityRun")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}
impl Run {
    pub(super) fn start(
        mut cmd: Command,
        profile: Profile,
        r: RunRequest,
    ) -> Result<Self, BackendError> {
        let resume = match r.session {
            NativeSession::Start { .. } => None,
            NativeSession::Resume { id } => {
                cmd.arg("--conversation").arg(&id);
                Some(id)
            }
        };
        let mut child = cmd
            .args([
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--print-timeout",
            ])
            .arg(format!("{}ms", r.wall_time_limit.as_millis().max(1)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(spawn_error)?;
        let lines = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| failed("missing agy stdout"))?,
        )
        .lines();
        Ok(Self {
            child,
            lines,
            _profile: profile,
            resume,
            prompt: Some(r.prompt),
            pending: VecDeque::new(),
            done: false,
        })
    }

    async fn read(&mut self) -> Result<Event, BackendError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            let line = self
                .lines
                .next_line()
                .await
                .map_err(failed)?
                .ok_or_else(|| failed("agy exited without a result"))?;
            let row: Value = serde_json::from_str(&line).map_err(failed)?;
            match row["event"].as_str() {
                Some("init") => {
                    let id = row["conversation_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| failed("missing agy conversation id"))?;
                    // agy silently starts a fresh session for an unknown id. Check before sending work.
                    if self
                        .resume
                        .as_deref()
                        .is_some_and(|expected| expected != id)
                    {
                        return Ok(Event::SessionMissing);
                    }
                    let prompt = self
                        .prompt
                        .take()
                        .ok_or_else(|| failed("duplicate agy init"))?;
                    let message = json!({"event":"user","message":{"content":prompt}});
                    let mut stdin = self
                        .child
                        .stdin
                        .take()
                        .ok_or_else(|| failed("missing agy stdin"))?;
                    stdin
                        .write_all(format!("{message}\n").as_bytes())
                        .await
                        .map_err(failed)?;
                    return Ok(Event::SessionStarted {
                        native_session_id: id.to_owned(),
                    });
                }
                Some("step_update") => {
                    let step = &row["step_update"];
                    if step["step_type"] == "agent_response" {
                        if let Some(text) = step["text_delta"].as_str() {
                            return Ok(Event::Text { text: text.into() });
                        }
                    } else if step["state"] == "DONE" && step["tool_info"].is_object() {
                        let info = &step["tool_info"];
                        let name = info["name"]
                            .as_str()
                            .or_else(|| step["tool_name"].as_str())
                            .unwrap_or("tool");
                        let params = &info["parameters"];
                        let (name, input) =
                            if name == "call_mcp_tool" && params["ServerName"] == "threadlane" {
                                (
                                    params["ToolName"].as_str().unwrap_or(name),
                                    &params["Arguments"],
                                )
                            } else {
                                (name.strip_prefix("mcp_threadlane_").unwrap_or(name), params)
                            };
                        let call_id = step["step_index"].as_u64().map(|n| n.to_string());
                        let is_error = !info["error"].is_null();
                        self.pending.push_back(Event::ToolResult {
                            call_id: call_id.clone(),
                            output: info[if is_error { "error" } else { "output" }].clone(),
                            is_error,
                        });
                        return Ok(Event::ToolUse {
                            call_id,
                            name: name.into(),
                            input: input.clone(),
                        });
                    }
                }
                Some("result") => {
                    let result = &row["result"];
                    if result["status"] != "SUCCESS" {
                        return Err(result_error(result));
                    }
                    let status = self.child.wait().await.map_err(failed)?;
                    if !status.success() {
                        return Err(failed(format!("agy exited with {status}")));
                    }
                    return Ok(Event::Finished {
                        final_text: result["response"].as_str().map(str::to_owned),
                        usage: result["usage"].clone(),
                    });
                }
                _ => {}
            }
        }
    }
}
#[async_trait]
impl BackendRun for Run {
    async fn next_event(&mut self) -> Option<Event> {
        if self.done {
            return None;
        }
        let event = match self.read().await {
            Ok(event) => event,
            Err(BackendError::UsageLimit { resets_at }) => Event::UsageLimit { resets_at },
            Err(error) => Event::Failed {
                error: error.to_string(),
            },
        };
        if event.is_terminal() {
            let _ = self.stop().await;
        }
        Some(event)
    }
    async fn stop(&mut self) -> Result<(), BackendError> {
        self.done = true;
        self.child.kill().await.map_err(failed)
    }
}

#[cfg(test)]
mod quota_tests {
    use super::*;

    #[test]
    fn a_reached_quota_is_a_usage_limit_so_the_task_waits_for_the_reset() {
        let result = json!({ "error": "Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 37m3s." });
        assert!(matches!(
            result_error(&result),
            BackendError::UsageLimit { resets_at: Some(_) }
        ));
        assert!(matches!(
            result_error(&json!({ "error": "Individual quota reached." })),
            BackendError::UsageLimit { resets_at: None }
        ));
        assert!(matches!(
            result_error(&json!({ "error": "boom" })),
            BackendError::Failed(_)
        ));
    }

    #[test]
    fn the_reset_wait_is_read_from_the_quota_message() {
        let wait = |text: &str| resets_in(text).map(|wait| wait.as_secs());
        assert_eq!(
            wait("individual quota reached. resets in 37m3s."),
            Some(2223)
        );
        assert_eq!(wait("resets in 1h2m"), Some(3720));
        assert_eq!(wait("resets in 45s"), Some(45));
        assert_eq!(wait("resets in 37 minutes"), None);
        assert_eq!(wait("quota reached"), None);
    }
}

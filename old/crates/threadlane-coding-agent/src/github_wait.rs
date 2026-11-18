//! Token-free PR monitoring inside one cancellable tool invocation.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, path::PathBuf, time::Duration};
use threadlane_git::{PrWaitSettings, PrWorkflowRequest};
use tokio::time::{sleep_until, timeout_at, Instant};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) async fn execute(
    work_dir: PathBuf,
    request: PrWorkflowRequest,
) -> Result<String, String> {
    let settings = request.wait_settings()?;
    let fetch = || {
        let work_dir = work_dir.clone();
        let request = request.clone();
        async move {
            let output = tokio::task::spawn_blocking(move || {
                threadlane_git::execute_pr_workflow(&work_dir, request)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("GitHub operation failed: {error}"))??;
            Ok::<_, String>(output)
        }
    };
    if let Some(settings) = settings {
        wait_for_change(settings, || async {
            let output = fetch().await?;
            let mut snapshot: Value = serde_json::from_str(&output)
                .map_err(|error| format!("Invalid GitHub feedback: {error}"))?;
            add_cursor(&mut snapshot)?;
            Ok(snapshot)
        })
        .await
        .map(|result| result.to_string())
    } else {
        let output = fetch().await?;
        // Other operations may return plain text, and status has no feedback.
        if let Ok(mut snapshot) = serde_json::from_str::<Value>(&output) {
            if snapshot.get("conversation_comments").is_some() {
                add_cursor(&mut snapshot)?;
                return Ok(snapshot.to_string());
            }
        }
        Ok(output)
    }
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            let mut items: Vec<_> = items.iter().map(canonicalize).collect();
            items.sort_by_cached_key(Value::to_string);
            Value::Array(items)
        }
        Value::Object(fields) => {
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_by_key(|(key, _)| *key);
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonicalize(value)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

fn add_cursor(snapshot: &mut Value) -> Result<(), String> {
    if !snapshot.get("status").is_some_and(Value::is_object)
        || [
            "conversation_comments",
            "reviews",
            "inline_comments",
            "threads",
        ]
        .iter()
        .any(|key| !snapshot.get(key).is_some_and(Value::is_array))
    {
        return Err("Incomplete GitHub feedback; cannot watch for changes".into());
    }
    snapshot.as_object_mut().unwrap().remove("watch_cursor");
    let digest = Sha256::digest(canonicalize(snapshot).to_string().as_bytes());
    snapshot["watch_cursor"] = json!(format!("{digest:x}"));
    Ok(())
}

async fn wait_for_change<F, Fut>(settings: PrWaitSettings, mut fetch: F) -> Result<Value, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let deadline = Instant::now() + Duration::from_secs(settings.timeout_seconds);
    let mut cursor = settings.cursor.map(|cursor| cursor.to_ascii_lowercase());
    loop {
        let mut snapshot = timeout_at(deadline, fetch()).await.map_err(|_| {
            "GitHub wait timed out during a read; feedback coverage is incomplete".to_string()
        })??;
        let current = snapshot["watch_cursor"]
            .as_str()
            .ok_or("Missing GitHub watch cursor")?
            .to_owned();
        let reason = if snapshot["status"]["state"]
            .as_str()
            .is_some_and(|state| state != "OPEN")
        {
            Some("closed")
        } else if cursor.as_ref().is_some_and(|cursor| cursor != &current) {
            Some("changed")
        } else {
            None
        };
        if let Some(reason) = reason {
            snapshot["wake_reason"] = json!(reason);
            return Ok(snapshot);
        }
        cursor = Some(current);
        sleep_until((Instant::now() + POLL_INTERVAL).min(deadline)).await;
        if Instant::now() >= deadline {
            return Ok(json!({
                "wake_reason": "timeout",
                "watch_cursor": cursor,
                "message": "No change observed before the wait deadline. This is not a fresh readiness check; read feedback before claiming the PR is ready."
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{add_cursor, wait_for_change};
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use threadlane_git::PrWaitSettings;

    fn feedback() -> Value {
        let mut value = json!({
            "status": {"url":"https://github.com/owner/repo/pull/1", "headRefOid":"abc", "state":"OPEN", "statusCheckRollup":[]},
            "conversation_comments":[], "reviews":[], "inline_comments":[], "threads":[]
        });
        add_cursor(&mut value).unwrap();
        value
    }

    fn settings(cursor: Option<String>) -> PrWaitSettings {
        PrWaitSettings {
            cursor,
            timeout_seconds: 1800,
        }
    }

    #[test]
    fn cursor_ignores_order_but_detects_check_and_feedback_changes() {
        let original = feedback();
        for (field, value) in [
            (
                "conversation_comments",
                json!([{"id":1,"body":"new comment"}]),
            ),
            ("reviews", json!([{"id":2,"state":"CHANGES_REQUESTED"}])),
            ("inline_comments", json!([{"id":3,"body":"fix this"}])),
            ("threads", json!([{"id":"thread", "isResolved":true}])),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            add_cursor(&mut changed).unwrap();
            assert_ne!(changed["watch_cursor"], original["watch_cursor"]);
        }
        let mut checks = original.clone();
        checks["status"]["statusCheckRollup"] = json!([
            {"name":"build", "conclusion":"SUCCESS"},
            {"name":"test", "conclusion":"FAILURE"}
        ]);
        add_cursor(&mut checks).unwrap();
        assert_ne!(checks["watch_cursor"], original["watch_cursor"]);
        let mut reordered = checks.clone();
        reordered["status"]["statusCheckRollup"]
            .as_array_mut()
            .unwrap()
            .reverse();
        add_cursor(&mut reordered).unwrap();
        assert_eq!(checks["watch_cursor"], reordered["watch_cursor"]);
        reordered["status"]["headRefOid"] = json!("new-head");
        add_cursor(&mut reordered).unwrap();
        assert_ne!(checks["watch_cursor"], reordered["watch_cursor"]);
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_polls_stay_inside_tool_until_new_comment() {
        let original = feedback();
        let calls = AtomicUsize::new(0);
        let result = wait_for_change(settings(None), || {
            let mut snapshot = original.clone();
            if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                snapshot["conversation_comments"] = json!([{"id":1,"body":"review"}]);
                add_cursor(&mut snapshot).unwrap();
            }
            async move { Ok(snapshot) }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(result["wake_reason"], "changed");
    }

    #[tokio::test(start_paused = true)]
    async fn supplied_cursor_detects_change_before_wait_registration() {
        let original = feedback();
        let mut changed = original.clone();
        changed["status"]["headRefOid"] = json!("new-head");
        add_cursor(&mut changed).unwrap();
        let result = wait_for_change(
            settings(Some(original["watch_cursor"].as_str().unwrap().into())),
            || {
                let changed = changed.clone();
                async move { Ok(changed) }
            },
        )
        .await
        .unwrap();
        assert_eq!(result["wake_reason"], "changed");
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_does_not_present_cached_feedback_as_fresh() {
        let result = wait_for_change(
            PrWaitSettings {
                cursor: None,
                timeout_seconds: 90,
            },
            || async { Ok(feedback()) },
        )
        .await
        .unwrap();
        assert_eq!(result["wake_reason"], "timeout");
        assert!(result.get("status").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_read_cannot_outlive_wait_deadline() {
        let result = wait_for_change(settings(None), || std::future::pending()).await;
        assert!(result.unwrap_err().contains("coverage is incomplete"));
    }

    #[tokio::test(start_paused = true)]
    async fn closed_pr_and_failed_reads_wake_without_polling() {
        let result = wait_for_change(settings(None), || async {
            let mut snapshot = feedback();
            snapshot["status"]["state"] = json!("MERGED");
            add_cursor(&mut snapshot).unwrap();
            Ok(snapshot)
        })
        .await
        .unwrap();
        assert_eq!(result["wake_reason"], "closed");
        assert!(
            wait_for_change(settings(None), || async { Err("rate limited".into()) })
                .await
                .is_err()
        );
        assert!(add_cursor(&mut json!({"status":{}})).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_wait_stops_future_polls() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let task = tokio::spawn(async move {
            wait_for_change(settings(None), || {
                observed.fetch_add(1, Ordering::SeqCst);
                async { Ok(feedback()) }
            })
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::advance(Duration::from_secs(1800)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

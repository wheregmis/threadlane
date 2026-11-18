//! Ask the installed CLI for the same enabled skill names it offers a session.

use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use threadlane_core::backend::BackendError;
use threadlane_core::model::SkillBinding;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::{CodexConfig, PROBE_TIMEOUT};

pub(super) async fn list(
    config: &CodexConfig,
    cwd: Option<&Path>,
) -> Result<Vec<String>, BackendError> {
    let scratch = if cwd.is_none() {
        Some(tempfile::tempdir().map_err(failed)?)
    } else {
        None
    };
    let cwd = cwd
        .or_else(|| scratch.as_ref().map(tempfile::TempDir::path))
        .expect("a working directory");
    let mut command = Command::new(&config.program);
    command
        .args(["app-server", "--stdio"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(home) = &config.home {
        command.env("CODEX_HOME", home);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(failed(e)),
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let result = tokio::time::timeout(PROBE_TIMEOUT, async {
        let init = json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"threadlane","version":env!("CARGO_PKG_VERSION")}}});
        call(&mut stdin,&mut lines,init).await?;
        stdin.write_all(b"{\"method\":\"initialized\"}\n").await.map_err(failed)?;
        let list = call(&mut stdin,&mut lines,json!({"id":2,"method":"skills/list","params":{}})).await?;
        let skills = list["data"][0]["skills"].as_array().ok_or_else(||failed("invalid skill catalog"))?;
        // A repository skill can shadow a personal name in the actual run directory.
        let mut names = std::collections::BTreeMap::new();
        for skill in skills.iter().filter(|s| s["enabled"] == true) {
            if let Some(name) = skill["name"].as_str().filter(|name| SkillBinding::valid_name(name)) {
                names.entry(name).and_modify(|unique| *unique = false).or_insert(skill["scope"] != "repo");
            }
        }
        Ok(names.into_iter().filter(|(_,unique)| *unique).map(|(name,_)| name.to_owned()).collect())
    }).await;
    let _ = child.kill().await;
    result.map_err(|_| BackendError::Timeout)?
}

async fn call(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    request: Value,
) -> Result<Value, BackendError> {
    input
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(failed)?;
    while let Some(line) = output.next_line().await.map_err(failed)? {
        let mut reply: Value = serde_json::from_str(&line).map_err(failed)?;
        if reply["id"] == request["id"] {
            return reply
                .get_mut("result")
                .map(Value::take)
                .ok_or_else(|| failed("installed Codex rejected the catalog request"));
        }
    }
    Err(failed("unexpected EOF"))
}

fn failed(reason: impl std::fmt::Display) -> BackendError {
    BackendError::Failed(format!("could not list Codex skills: {reason}"))
}

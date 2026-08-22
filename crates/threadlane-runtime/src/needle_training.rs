use crate::needle_history_eval::{
    NeedleEvalSkipped, NeedleHistoryCall, NeedleHistoryTurn, extract_needle_history,
};
use crate::types::AgentToolDefinition;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const TRAIN_FILE: &str = "train.jsonl";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const ADAPTER_FILE: &str = "adapter.pkl";
pub const CANDIDATE_FILE: &str = "candidate.cact";
const BASE_CHECKPOINT_FILE: &str = "checkpoints/needle2.pkl";
pub const CURRENT_EVAL_FILE: &str = "current-eval.json";
pub const CANDIDATE_EVAL_FILE: &str = "candidate-eval.json";
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct NeedleDatasetConfig {
    pub sessions_dir: PathBuf,
    pub work_dir: PathBuf,
    pub replace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeedleTrainingManifest {
    pub version: u32,
    pub pilot: bool,
    pub eligible_turns: usize,
    pub train_turns: usize,
    pub holdout_turns: usize,
    pub train_sessions: Vec<PathBuf>,
    pub holdout_sessions: Vec<PathBuf>,
    pub skipped: NeedleEvalSkipped,
    pub redactions: BTreeMap<String, usize>,
    pub catalogue_sha256: String,
    pub dataset_sha256: String,
    pub needle_version: Option<String>,
    pub base_sha256: Option<String>,
    pub adapter_sha256: Option<String>,
    pub candidate_sha256: Option<String>,
}

#[derive(Serialize)]
struct NeedleTrainingExample<'a> {
    query: &'a str,
    tools: &'a [AgentToolDefinition],
    answers: &'a [NeedleHistoryCall],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSplit {
    train_sessions: Vec<PathBuf>,
    holdout_sessions: Vec<PathBuf>,
}

struct CredentialRule {
    name: &'static str,
    regex: Regex,
    capture: usize,
}

#[derive(Default)]
struct ExampleRedactor {
    placeholders: BTreeMap<String, String>,
    counts: BTreeMap<String, usize>,
}

impl ExampleRedactor {
    fn redact_text(&mut self, text: &str) -> String {
        let mut redacted = text.to_string();
        for rule in credential_rules() {
            let mut next = String::with_capacity(redacted.len());
            let mut last = 0;
            for captures in rule.regex.captures_iter(&redacted) {
                let Some(matched) = captures.get(rule.capture) else {
                    continue;
                };
                next.push_str(&redacted[last..matched.start()]);
                next.push_str(&self.placeholder(rule.name, matched.as_str()));
                last = matched.end();
            }
            next.push_str(&redacted[last..]);
            redacted = next;
        }
        redacted
    }

    fn placeholder(&mut self, rule_name: &'static str, secret: &str) -> String {
        if let Some(existing) = self.placeholders.get(secret) {
            return existing.clone();
        }
        let placeholder = format!("<REDACTED_{}>", self.placeholders.len() + 1);
        self.placeholders
            .insert(secret.to_string(), placeholder.clone());
        *self.counts.entry(rule_name.to_string()).or_default() += 1;
        placeholder
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.placeholders.len()
    }

    #[cfg(test)]
    fn counts(&self) -> &BTreeMap<String, usize> {
        &self.counts
    }
}

fn redact_value(value: &mut Value, redactor: &mut ExampleRedactor) {
    match value {
        Value::String(text) => *text = redactor.redact_text(text),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| redact_value(value, redactor)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| redact_value(value, redactor)),
        _ => {}
    }
}

pub fn export_needle_dataset(
    config: &NeedleDatasetConfig,
    definitions: &[AgentToolDefinition],
) -> Result<NeedleTrainingManifest, String> {
    let mut corpus = extract_needle_history(&config.sessions_dir, definitions)?;
    for turn in &mut corpus.turns {
        if let Ok(relative) = turn.session_file.strip_prefix(&config.sessions_dir) {
            turn.session_file = relative.to_path_buf();
        }
    }
    let split = split_sessions(&corpus.turns)?;
    std::fs::create_dir_all(&config.work_dir)
        .map_err(|_| "Needle training directory could not be created.".to_string())?;
    let train_path = config.work_dir.join(TRAIN_FILE);
    let manifest_path = config.work_dir.join(MANIFEST_FILE);
    if !config.replace && (train_path.exists() || manifest_path.exists()) {
        return Err("Needle training outputs already exist; set replace to overwrite.".into());
    }

    let train_bytes = training_jsonl(&corpus.turns, definitions, &split.train_sessions)?;
    let dataset_sha256 = sha256(&train_bytes);
    let catalogue_bytes = serde_json::to_vec(definitions)
        .map_err(|_| "Tool catalogue could not be serialized.".to_string())?;
    let redactions = redaction_counts(&corpus.turns, &split.train_sessions);
    let manifest = NeedleTrainingManifest {
        version: MANIFEST_VERSION,
        pilot: split
            .holdout_sessions
            .iter()
            .map(|path| session_turns(&corpus.turns, path))
            .sum::<usize>()
            < 200,
        eligible_turns: corpus.turns.len(),
        train_turns: split
            .train_sessions
            .iter()
            .map(|path| session_turns(&corpus.turns, path))
            .sum(),
        holdout_turns: split
            .holdout_sessions
            .iter()
            .map(|path| session_turns(&corpus.turns, path))
            .sum(),
        train_sessions: split.train_sessions,
        holdout_sessions: split.holdout_sessions,
        skipped: corpus.skipped,
        redactions,
        catalogue_sha256: sha256(&catalogue_bytes),
        dataset_sha256,
        needle_version: None,
        base_sha256: None,
        adapter_sha256: None,
        candidate_sha256: None,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| "Needle training manifest could not be serialized.".to_string())?;
    let train_tmp = write_temp(&train_path, &train_bytes)?;
    let manifest_tmp = write_temp(&manifest_path, &manifest_bytes)?;
    finalize_temp(&train_tmp, &train_path)?;
    finalize_temp(&manifest_tmp, &manifest_path)?;
    Ok(manifest)
}

pub fn load_training_manifest(work_dir: &Path) -> Result<NeedleTrainingManifest, String> {
    let bytes = std::fs::read(work_dir.join(MANIFEST_FILE))
        .map_err(|_| "Needle training manifest is unreadable.".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "Needle training manifest is invalid.".to_string())
}

pub fn save_training_manifest(
    work_dir: &Path,
    manifest: &NeedleTrainingManifest,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|_| "Needle training manifest could not be serialized.".to_string())?;
    write_atomic(&work_dir.join(MANIFEST_FILE), &bytes)
}

pub fn run_needle_finetune(
    work_dir: &Path,
    needle_executable: &OsStr,
) -> Result<NeedleTrainingManifest, String> {
    let result = run_needle_finetune_inner(work_dir, needle_executable);
    if result.is_err() {
        cleanup_finetune_temps(work_dir);
    }
    result
}

fn run_needle_finetune_inner(
    work_dir: &Path,
    needle_executable: &OsStr,
) -> Result<NeedleTrainingManifest, String> {
    let mut manifest = load_training_manifest(work_dir)?;
    let base_path = work_dir.join(BASE_CHECKPOINT_FILE);
    let base_bytes = std::fs::read(&base_path)
        .map_err(|_| "Needle base checkpoint is missing or unreadable.".to_string())?;
    let version = needle_output(needle_executable, work_dir, ["--version"])?;

    run_needle(
        needle_executable,
        work_dir,
        [
            "finetune",
            TRAIN_FILE,
            "--epochs",
            "10",
            "--out",
            "adapter.pkl.tmp",
        ],
    )?;
    let adapter_tmp = work_dir.join("adapter.pkl.tmp");
    if !adapter_tmp.is_file() {
        return Err("Needle finetune did not produce adapter.pkl.tmp.".into());
    }
    finalize_temp(&adapter_tmp, &work_dir.join(ADAPTER_FILE))?;

    run_needle(
        needle_executable,
        work_dir,
        [
            "build",
            BASE_CHECKPOINT_FILE,
            "--lora",
            ADAPTER_FILE,
            "--out",
            "candidate.cact.tmp",
        ],
    )?;
    let candidate_tmp = work_dir.join("candidate.cact.tmp");
    if !candidate_tmp.is_file() {
        return Err("Needle build did not produce candidate.cact.tmp.".into());
    }
    finalize_temp(&candidate_tmp, &work_dir.join(CANDIDATE_FILE))?;

    manifest.needle_version = Some(version.trim().to_string());
    manifest.base_sha256 = Some(sha256(&base_bytes));
    manifest.adapter_sha256 = Some(sha256(
        &std::fs::read(work_dir.join(ADAPTER_FILE))
            .map_err(|_| "Needle adapter is unreadable.".to_string())?,
    ));
    manifest.candidate_sha256 = Some(sha256(
        &std::fs::read(work_dir.join(CANDIDATE_FILE))
            .map_err(|_| "Needle candidate is unreadable.".to_string())?,
    ));
    if let Err(error) = save_training_manifest(work_dir, &manifest) {
        cleanup_finetune_outputs(work_dir);
        return Err(error);
    }
    Ok(manifest)
}

fn needle_output<const N: usize>(
    needle_executable: &OsStr,
    work_dir: &Path,
    args: [&str; N],
) -> Result<String, String> {
    let output = Command::new(needle_executable)
        .current_dir(work_dir)
        .args(args)
        .output()
        .map_err(needle_spawn_error)?;
    if !output.status.success() {
        return Err("Needle command failed.".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Needle command output was not UTF-8.".into())
}

fn run_needle<const N: usize>(
    needle_executable: &OsStr,
    work_dir: &Path,
    args: [&str; N],
) -> Result<(), String> {
    let status = Command::new(needle_executable)
        .current_dir(work_dir)
        .args(args)
        .status()
        .map_err(needle_spawn_error)?;
    if status.success() {
        Ok(())
    } else {
        Err("Needle command failed.".into())
    }
}

fn needle_spawn_error(error: std::io::Error) -> String {
    if error.kind() == ErrorKind::NotFound {
        "Needle executable was not found. Install it with `pip install cactus-needle` or, on Apple GPU machines, `pip install \"cactus-needle[metal]\"`."
            .into()
    } else {
        "Needle executable could not be started.".into()
    }
}

fn cleanup_finetune_temps(work_dir: &Path) {
    for file in ["adapter.pkl.tmp", "candidate.cact.tmp"] {
        let _ = std::fs::remove_file(work_dir.join(file));
    }
}

fn cleanup_finetune_outputs(work_dir: &Path) {
    for file in [ADAPTER_FILE, CANDIDATE_FILE] {
        let _ = std::fs::remove_file(work_dir.join(file));
    }
}

fn split_sessions(turns: &[NeedleHistoryTurn]) -> Result<SessionSplit, String> {
    let mut groups: BTreeMap<PathBuf, (u64, usize)> = BTreeMap::new();
    for turn in turns {
        let entry = groups
            .entry(turn.session_file.clone())
            .or_insert((turn.timestamp, 0));
        entry.0 = entry.0.min(turn.timestamp);
        entry.1 += 1;
    }
    if groups.len() < 2 {
        return Err("Needle training export requires at least two eligible sessions.".into());
    }
    let mut ordered = groups
        .into_iter()
        .map(|(path, (timestamp, count))| (timestamp, path, count))
        .collect::<Vec<_>>();
    ordered.sort();

    let total = turns.len();
    let target = total.saturating_mul(20).div_ceil(100);
    let mut holdout_turns = 0;
    let mut holdout_sessions = Vec::new();
    let mut train_sessions = ordered
        .iter()
        .map(|(_, path, _)| path.clone())
        .collect::<Vec<_>>();
    while holdout_turns < target && train_sessions.len() > 1 {
        let (_, path, count) = ordered[train_sessions.len() - 1].clone();
        train_sessions.pop();
        holdout_turns += count;
        holdout_sessions.push(path);
    }
    if holdout_turns < target {
        return Err(
            "Needle training export cannot satisfy a 20 percent holdout while preserving training sessions."
                .into(),
        );
    }
    holdout_sessions.sort_by_key(|path| groups_order(&ordered, path));
    Ok(SessionSplit {
        train_sessions,
        holdout_sessions,
    })
}

fn groups_order(ordered: &[(u64, PathBuf, usize)], path: &Path) -> (u64, PathBuf) {
    ordered
        .iter()
        .find(|(_, candidate, _)| candidate == path)
        .map(|(timestamp, candidate, _)| (*timestamp, candidate.clone()))
        .unwrap_or((u64::MAX, path.to_path_buf()))
}

fn training_jsonl(
    turns: &[NeedleHistoryTurn],
    definitions: &[AgentToolDefinition],
    train_sessions: &[PathBuf],
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for turn in turns
        .iter()
        .filter(|turn| train_sessions.contains(&turn.session_file))
    {
        let mut redactor = ExampleRedactor::default();
        let query = redactor.redact_text(&turn.prompt);
        let mut answers = turn.calls.clone();
        for answer in &mut answers {
            for value in answer.arguments.values_mut() {
                redact_value(value, &mut redactor);
            }
        }
        serde_json::to_writer(
            &mut bytes,
            &NeedleTrainingExample {
                query: &query,
                tools: definitions,
                answers: &answers,
            },
        )
        .map_err(|_| "Needle training example could not be serialized.".to_string())?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn redaction_counts(
    turns: &[NeedleHistoryTurn],
    train_sessions: &[PathBuf],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for turn in turns
        .iter()
        .filter(|turn| train_sessions.contains(&turn.session_file))
    {
        let mut redactor = ExampleRedactor::default();
        let _ = redactor.redact_text(&turn.prompt);
        for call in &turn.calls {
            let mut args = Value::Object(call.arguments.clone());
            redact_value(&mut args, &mut redactor);
        }
        for (name, count) in redactor.counts {
            *counts.entry(name).or_default() += count;
        }
    }
    counts
}

fn session_turns(turns: &[NeedleHistoryTurn], session: &Path) -> usize {
    turns
        .iter()
        .filter(|turn| turn.session_file == session)
        .count()
}

fn credential_rules() -> &'static [CredentialRule] {
    static RULES: OnceLock<Vec<CredentialRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            CredentialRule {
                name: "private_key",
                regex: Regex::new(
                    r"(?s)(-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----)",
                )
                .unwrap(),
                capture: 1,
            },
            CredentialRule {
                name: "bearer_token",
                regex: Regex::new(r"(?i)Bearer\s+([A-Za-z0-9._~+/=-]{20,})").unwrap(),
                capture: 1,
            },
            CredentialRule {
                name: "api_key_prefix",
                regex: Regex::new(
                    r"\b((?:sk|pk|rk|ghp|gho|github_pat|xox[baprs])[-_][A-Za-z0-9_=-]{12,})",
                )
                .unwrap(),
                capture: 1,
            },
            CredentialRule {
                name: "credential_assignment",
                regex: Regex::new(
                    r#"(?i)\b(?:token|secret|password|api[_-]?key)\b\s*[:=]\s*([^\s'",;}<]{8,})"#,
                )
                .unwrap(),
                capture: 1,
            },
        ]
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp_path = write_temp(path, bytes)?;
    finalize_temp(&tmp_path, path)
}

fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let tmp_path = path.with_extension("tmp");
    {
        let file = owner_only_create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(bytes)
            .map_err(|_| "Needle training artifact could not be written.".to_string())?;
        writer
            .flush()
            .map_err(|_| "Needle training artifact could not be flushed.".to_string())?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|_| "Needle training artifact could not be synced.".to_string())?;
    }
    Ok(tmp_path)
}

fn finalize_temp(tmp_path: &Path, path: &Path) -> Result<(), String> {
    std::fs::rename(&tmp_path, path)
        .map_err(|_| "Needle training artifact could not be finalized.".to_string())
}

#[cfg(unix)]
fn owner_only_create(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let _ = std::fs::remove_file(path);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "Needle training artifact could not be created.".to_string())
}

#[cfg(not(unix))]
fn owner_only_create(path: &Path) -> Result<File, String> {
    let _ = std::fs::remove_file(path);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "Needle training artifact could not be created.".to_string())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::PathBuf;

    fn turns_for_sessions(sessions: &[(&str, u64, usize)]) -> Vec<NeedleHistoryTurn> {
        sessions
            .iter()
            .flat_map(|(file, first_timestamp, count)| {
                (0..*count).map(move |index| NeedleHistoryTurn {
                    session_file: PathBuf::from(file),
                    timestamp: first_timestamp + index as u64,
                    prompt: "prompt".into(),
                    calls: vec![NeedleHistoryCall {
                        name: "read_file".into(),
                        arguments: serde_json::Map::new(),
                    }],
                })
            })
            .collect()
    }

    struct FixtureExport {
        _temp: tempfile::TempDir,
        work_dir: PathBuf,
    }

    fn tool() -> AgentToolDefinition {
        AgentToolDefinition::new("read_file", "Read", serde_json::json!({}))
    }

    fn write_session(path: &std::path::Path, prompt: &str, result: &str) {
        use crate::harness::{
            Entry, JsonlStore, SessionStore, ToolExecutionOutcome, ToolExecutionPhase, TraceString,
        };
        use crate::types::AgentMessage;
        use threadlane_protocol::{RuntimeToolCall, RuntimeToolCallFunction};

        let mut store = JsonlStore::open(path).unwrap();
        let user = Entry::new(
            "user",
            None,
            "main",
            1,
            1,
            AgentMessage::User {
                content: prompt.into(),
            },
            false,
        );
        store.append_entry(user).unwrap();
        let assistant = Entry::new(
            "assistant",
            Some("user".into()),
            "main",
            2,
            2,
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![RuntimeToolCall {
                    id: "call".into(),
                    r#type: "function".into(),
                    function: RuntimeToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{"path":"Cargo.toml"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            false,
        );
        store.append_entry(assistant).unwrap();
        store
            .append_entry(Entry::new(
                "tool",
                Some("assistant".into()),
                "main",
                3,
                3,
                AgentMessage::Tool {
                    tool_call_id: "call".into(),
                    name: "read_file".into(),
                    content: result.into(),
                    is_error: false,
                    terminate: false,
                },
                false,
            ))
            .unwrap();
        store.append_record(crate::harness::Record::ToolExecutionObserved {
            id: "observed".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run".into(),
            attempt: Some(1),
            tool_call_id: TraceString::new("call").unwrap(),
            tool_name: TraceString::new("read_file").unwrap(),
            executor_kind: TraceString::new("test").unwrap(),
            phase: ToolExecutionPhase::Finished,
            started_at_ms: None,
            duration_ms: None,
            outcome: Some(ToolExecutionOutcome::Succeeded),
            exit_code: Some(0),
            cancelled: false,
            is_error: Some(false),
            terminate: Some(false),
            output_sha256: None,
            output_bytes: None,
        });
    }

    fn export_fixture(holdout_turns: usize) -> NeedleTrainingManifest {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join("sessions");
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        for (index, count) in [1, holdout_turns].into_iter().enumerate() {
            for turn in 0..count {
                write_session(
                    &sessions_dir.join(format!("{index}-{turn}.jsonl")),
                    "read the file",
                    "result",
                );
            }
        }
        export_needle_dataset(
            &NeedleDatasetConfig {
                sessions_dir,
                work_dir,
                replace: false,
            },
            &[tool()],
        )
        .unwrap()
    }

    fn export_fixture_with_tool_result(result: &str) -> FixtureExport {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join("sessions");
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        write_session(&sessions_dir.join("a.jsonl"), "older", result);
        write_session(&sessions_dir.join("b.jsonl"), "newer", result);
        export_needle_dataset(
            &NeedleDatasetConfig {
                sessions_dir,
                work_dir: work_dir.clone(),
                replace: false,
            },
            &[tool()],
        )
        .unwrap();
        FixtureExport {
            _temp: temp,
            work_dir,
        }
    }

    fn training_fixture_with_manifest_and_base_checkpoint() -> FixtureExport {
        let export = export_fixture_with_tool_result("result");
        let checkpoint_dir = export.work_dir.join("checkpoints");
        std::fs::create_dir_all(&checkpoint_dir).unwrap();
        std::fs::write(checkpoint_dir.join("needle2.pkl"), "base checkpoint").unwrap();
        export
    }

    #[cfg(unix)]
    fn fake_needle(root: &std::path::Path, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("needle");
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "{version}"
  exit 0
fi
printf '%s\n' "$*" >> needle.calls
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out" ]; then
    shift
    mkdir -p "$(dirname "$1")"
    printf 'artifact:%s\n' "$*" > "$1"
    exit 0
  fi
  shift
done
exit 2
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    fn fake_needle_build_failure(root: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("needle-fail");
        std::fs::write(
            &path,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "2.0-test"
  exit 0
fi
printf '%s\n' "$*" >> needle.calls
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out" ]; then
    shift
    printf 'artifact\n' > "$1"
    break
  fi
  shift
done
if grep -q '^build ' needle.calls; then
  exit 1
fi
exit 0
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn finetune_runs_upstream_commands_and_records_artifact_hashes() {
        let export = training_fixture_with_manifest_and_base_checkpoint();
        let needle = fake_needle(&export.work_dir, "2.0-test");

        let manifest = run_needle_finetune(&export.work_dir, needle.as_os_str()).unwrap();

        let calls = std::fs::read_to_string(export.work_dir.join("needle.calls")).unwrap();
        assert!(calls.contains("finetune train.jsonl --epochs 10"));
        assert!(calls.contains("build checkpoints/needle2.pkl --lora"));
        assert_eq!(manifest.needle_version.as_deref(), Some("2.0-test"));
        assert_eq!(manifest.base_sha256, Some(sha256(b"base checkpoint")));
        assert!(manifest.adapter_sha256.is_some());
        assert!(manifest.candidate_sha256.is_some());
        assert_eq!(load_training_manifest(&export.work_dir).unwrap(), manifest);
    }

    #[test]
    #[cfg(unix)]
    fn finetune_removes_temp_artifacts_when_upstream_build_fails() {
        let export = training_fixture_with_manifest_and_base_checkpoint();
        let needle = fake_needle_build_failure(&export.work_dir);

        assert_eq!(
            run_needle_finetune(&export.work_dir, needle.as_os_str()).unwrap_err(),
            "Needle command failed."
        );

        assert!(!export.work_dir.join("adapter.pkl.tmp").exists());
        assert!(!export.work_dir.join("candidate.cact.tmp").exists());
        assert!(
            load_training_manifest(&export.work_dir)
                .unwrap()
                .candidate_sha256
                .is_none()
        );
    }

    #[test]
    #[cfg(unix)]
    fn finetune_removes_promoted_artifacts_when_manifest_save_fails() {
        let export = training_fixture_with_manifest_and_base_checkpoint();
        let needle = fake_needle(&export.work_dir, "2.0-test");
        std::fs::create_dir(export.work_dir.join("manifest.tmp")).unwrap();

        assert_eq!(
            run_needle_finetune(&export.work_dir, needle.as_os_str()).unwrap_err(),
            "Needle training artifact could not be created."
        );

        assert!(!export.work_dir.join(ADAPTER_FILE).exists());
        assert!(!export.work_dir.join(CANDIDATE_FILE).exists());
        assert!(
            load_training_manifest(&export.work_dir)
                .unwrap()
                .candidate_sha256
                .is_none()
        );
    }

    #[test]
    fn finetune_missing_executable_mentions_pip_install_options() {
        let export = training_fixture_with_manifest_and_base_checkpoint();
        let needle = PathBuf::from("/definitely-not-installed-threadlane-needle");

        let error = run_needle_finetune(&export.work_dir, needle.as_os_str()).unwrap_err();

        assert!(error.contains("pip install cactus-needle"));
        assert!(error.contains("pip install \"cactus-needle[metal]\""));
    }

    #[test]
    fn redacts_the_same_secret_in_query_and_nested_arguments() {
        let mut args = serde_json::json!({"env": {"token": "sk-test_1234567890123456"}});
        let mut redactor = ExampleRedactor::default();
        let query = redactor.redact_text("use sk-test_1234567890123456 for this");
        redact_value(&mut args, &mut redactor);
        assert_eq!(query, "use <REDACTED_1> for this");
        assert_eq!(args["env"]["token"], "<REDACTED_1>");
        assert_eq!(redactor.count(), 1);
    }

    #[test]
    fn assigns_newest_complete_sessions_to_holdout_without_overlap() {
        let turns = turns_for_sessions(&[("a.jsonl", 1, 8), ("b.jsonl", 2, 1), ("c.jsonl", 3, 2)]);
        let split = split_sessions(&turns).unwrap();
        assert_eq!(split.train_sessions, vec![PathBuf::from("a.jsonl")]);
        assert_eq!(
            split.holdout_sessions,
            vec![PathBuf::from("b.jsonl"), PathBuf::from("c.jsonl")]
        );
        assert!(
            split
                .train_sessions
                .iter()
                .all(|path| !split.holdout_sessions.contains(path))
        );
    }

    #[test]
    fn split_errors_when_newest_whole_sessions_cannot_reach_holdout_floor() {
        let turns = turns_for_sessions(&[("old.jsonl", 1, 10_000), ("new.jsonl", 2, 500)]);
        assert_eq!(
            split_sessions(&turns).unwrap_err(),
            "Needle training export cannot satisfy a 20 percent holdout while preserving training sessions."
        );
    }

    #[test]
    fn marks_fewer_than_two_hundred_holdout_turns_as_pilot() {
        let manifest = export_fixture(199);
        assert!(manifest.pilot);
    }

    #[test]
    fn exported_jsonl_contains_no_tool_results_or_session_metadata() {
        let export = export_fixture_with_tool_result("DO_NOT_EXPORT_THIS_RESULT");
        let jsonl = std::fs::read_to_string(export.work_dir.join(TRAIN_FILE)).unwrap();
        assert!(!jsonl.contains("DO_NOT_EXPORT_THIS_RESULT"));
        assert!(!jsonl.contains("session_file"));
        assert!(!jsonl.contains("timestamp"));
    }

    #[test]
    fn exported_jsonl_redacts_query_and_arguments_and_manifest_counts_rules() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join("sessions");
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        write_session(
            &sessions_dir.join("a.jsonl"),
            "use sk-test_1234567890123456",
            "result",
        );
        write_session(&sessions_dir.join("b.jsonl"), "newer", "result");

        let manifest = export_needle_dataset(
            &NeedleDatasetConfig {
                sessions_dir,
                work_dir: work_dir.clone(),
                replace: false,
            },
            &[tool()],
        )
        .unwrap();
        let jsonl = std::fs::read_to_string(work_dir.join(TRAIN_FILE)).unwrap();
        assert!(jsonl.contains("<REDACTED_1>"));
        assert!(!jsonl.contains("sk-test_1234567890123456"));
        assert_eq!(manifest.redactions.get("api_key_prefix"), Some(&1));
    }

    #[test]
    fn saves_loads_manifest_and_records_dataset_hash() {
        let export = export_fixture_with_tool_result("result");
        let manifest = load_training_manifest(&export.work_dir).unwrap();
        let jsonl = std::fs::read(export.work_dir.join(TRAIN_FILE)).unwrap();
        assert_eq!(manifest.dataset_sha256, sha256(&jsonl));

        let copy_dir = export.work_dir.join("copy");
        std::fs::create_dir_all(&copy_dir).unwrap();
        save_training_manifest(&copy_dir, &manifest).unwrap();
        assert_eq!(load_training_manifest(&copy_dir).unwrap(), manifest);
    }

    #[test]
    fn refuses_to_replace_existing_outputs_without_replace() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join("sessions");
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        write_session(&sessions_dir.join("a.jsonl"), "older", "result");
        write_session(&sessions_dir.join("b.jsonl"), "newer", "result");
        let config = NeedleDatasetConfig {
            sessions_dir,
            work_dir,
            replace: false,
        };
        export_needle_dataset(&config, &[tool()]).unwrap();
        assert_eq!(
            export_needle_dataset(&config, &[tool()]).unwrap_err(),
            "Needle training outputs already exist; set replace to overwrite."
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_dataset_and_manifest_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let export = export_fixture_with_tool_result("result");
        assert_eq!(
            std::fs::metadata(export.work_dir.join(TRAIN_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(export.work_dir.join(MANIFEST_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_broad_temp_files_do_not_make_sensitive_outputs_broad() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join("sessions");
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&work_dir).unwrap();
        write_session(&sessions_dir.join("a.jsonl"), "older", "result");
        write_session(&sessions_dir.join("b.jsonl"), "newer", "result");
        for file in ["train.tmp", "manifest.tmp"] {
            let path = work_dir.join(file);
            std::fs::write(&path, "stale").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        export_needle_dataset(
            &NeedleDatasetConfig {
                sessions_dir,
                work_dir: work_dir.clone(),
                replace: true,
            },
            &[tool()],
        )
        .unwrap();

        for file in [TRAIN_FILE, MANIFEST_FILE] {
            assert_eq!(
                std::fs::metadata(work_dir.join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!work_dir.join("train.tmp").exists());
        assert!(!work_dir.join("manifest.tmp").exists());
    }

    #[test]
    fn redacts_bearer_assignment_api_key_and_private_key_but_not_paths() {
        let private_key = "-----BEGIN PRIVATE KEY-----\nabc123\n-----END PRIVATE KEY-----";
        let mut args = serde_json::json!({
            "bearer": "Bearer abcdefghijklmnopqrstuvwxyz012345",
            "assignment": "password = supersecretvalue",
            "api_key": "sk-live_12345678901234567890",
            "key": private_key,
            "path": "/Users/example/project/src/lib.rs"
        });
        let mut redactor = ExampleRedactor::default();
        redact_value(&mut args, &mut redactor);
        assert_eq!(
            args["path"],
            Value::String("/Users/example/project/src/lib.rs".into())
        );
        assert_eq!(redactor.counts().get("bearer_token"), Some(&1));
        assert_eq!(redactor.counts().get("credential_assignment"), Some(&1));
        assert_eq!(redactor.counts().get("api_key_prefix"), Some(&1));
        assert_eq!(redactor.counts().get("private_key"), Some(&1));
    }

    #[test]
    fn assignment_rule_does_not_rematch_generated_placeholders() {
        let secret = "sk-test_1234567890123456";
        let mut args = serde_json::json!({"env": {"line": format!("api_key={secret}")}});
        let mut redactor = ExampleRedactor::default();
        let query = redactor.redact_text(&format!("use {secret}"));
        redact_value(&mut args, &mut redactor);
        assert_eq!(query, "use <REDACTED_1>");
        assert_eq!(args["env"]["line"], "api_key=<REDACTED_1>");
        assert_eq!(redactor.count(), 1);
        assert_eq!(redactor.counts().get("api_key_prefix"), Some(&1));
        assert_eq!(redactor.counts().get("credential_assignment"), None);
    }
}

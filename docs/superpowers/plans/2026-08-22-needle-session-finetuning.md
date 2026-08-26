# Needle Session Fine-Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a local, privacy-aware pipeline that exports successful Threadlane session turns, fine-tunes Needle 2, evaluates a candidate on untouched sessions, and explicitly promotes only a qualifying model.

**Architecture:** Keep canonical session interpretation and promotion policy in Rust, while invoking Needle's maintained CLI for LoRA training and `.cact` export. Extend the existing evaluator to share ordered labels and support explicit session/model inputs; expose the four approved workflows through one project-aware binary and thin `just` recipes.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, existing `regex` and `sha2` dependencies, canonical `JsonlStore`, Tokio project capability discovery, upstream `cactus-needle` CLI/JAX, Just.

**Spec:** `docs/superpowers/specs/2026-08-22-needle-session-finetuning-design.md`

## Global Constraints

- Optimize only strict top-five tool shortlist recall; Needle never supplies production tool arguments or executes tools.
- Export only successful first-assistant calls; never export tool results, system prompts, assistant reasoning, thought signatures, or images.
- Store generated artifacts only under the git-ignored `.threadlane/needle-training/` directory and treat `train.jsonl` as sensitive local data.
- Use a deterministic complete-session split: older sessions train, newest sessions form at least 20 percent holdout, and no session appears in both.
- Mark fewer than 200 eligible holdout turns as `pilot`; pilot runs may train/evaluate but may never promote.
- Promotion requires at least 200 holdout turns, at least 99 percent candidate top-five recall, a strict improvement over current weights, and matching hashes.
- Do not add crates, embed JAX, synthesize data, upload artifacts, install Python packages automatically, or add UI/online training.
- Preserve existing unstaged user edits in `crates/threadlane-runtime/src/tool_executor.rs` and `crates/threadlane-runtime/src/turn_driver.rs`.
- Prefix repository shell commands with `rtk`; run the narrowest test first and finish with the repository validation commands.

## File Structure

- Modify `crates/threadlane-runtime/src/needle_history_eval.rs`: shared ordered successful-call extraction, path-filtered/explicit-model evaluation, serializable aggregate reports.
- Create `crates/threadlane-runtime/src/needle_training.rs`: redaction, deterministic split/export, manifest/artifact hashing, Needle CLI orchestration, report comparison, and promotion policy.
- Modify `crates/threadlane-runtime/src/lib.rs`: export `needle_training` behind the existing `needle` feature.
- Create `crates/threadlane-session/src/bin/needle_project_train.rs`: project-aware `dataset`, `finetune`, `evaluate`, internal `evaluate-one`, and `promote` command dispatch.
- Modify `crates/threadlane-session/Cargo.toml`: register the feature-gated project training binary.
- Modify `Justfile`: add `needle_dataset`, `needle_finetune`, `needle_evaluate_candidate`, and `needle_promote` recipes.
- Modify `README.md`: replace the evaluation-only limitation with the local pilot/training/evaluation/promotion workflow and sensitive-data warning.

---

### Task 1: Share Ordered Successful Tool-Call Labels

**Files:**
- Modify: `crates/threadlane-runtime/src/needle_history_eval.rs`

**Interfaces:**
- Produces: `NeedleHistoryCall { name: String, arguments: serde_json::Map<String, Value> }`.
- Produces: `NeedleHistoryTurn { session_file: PathBuf, timestamp: u64, prompt: String, calls: Vec<NeedleHistoryCall> }`.
- Produces: `extract_needle_history(sessions_dir: &Path, definitions: &[AgentToolDefinition]) -> Result<NeedleHistoryCorpus, String>`.
- Preserves: `run_needle_history_eval_with_definitions(&Path, Vec<AgentToolDefinition>)` and all current skip/recall behavior.

- [ ] **Step 1: Write failing tests for ordered parsed calls and malformed arguments**

Add tests beside the existing extraction fixtures. Reuse their `JsonlStore`, assistant-call, and successful-outcome helpers; make the assistant call helper accept an arguments string.

```rust
#[test]
fn extracts_successful_calls_in_assistant_order_with_json_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ordered.jsonl");
    let mut store = JsonlStore::open(&path).unwrap();
    let user = store.append_message(None, AgentMessage::User { content: "inspect then search".into() });
    let assistant = append_assistant_calls(
        &mut store,
        Some(user),
        &[("read", "read_file", r#"{"path":"Cargo.toml"}"#),
          ("search", "grep_search", r#"{"query":"needle"}"#)],
    );
    store.append_message(Some(assistant.clone()), successful_tool("read", "read_file"));
    store.append_message(Some(assistant), successful_tool("search", "grep_search"));

    let corpus = extract_needle_history(temp.path(), &definitions(&["read_file", "grep_search"])).unwrap();
    assert_eq!(corpus.turns[0].calls[0].name, "read_file");
    assert_eq!(corpus.turns[0].calls[0].arguments["path"], "Cargo.toml");
    assert_eq!(corpus.turns[0].calls[1].name, "grep_search");
}

#[test]
fn counts_successful_call_with_non_object_arguments_as_malformed() {
    // Build one successful call whose arguments are `[]`.
    let corpus = extract_needle_history(temp.path(), &definitions(&["read_file"])).unwrap();
    assert!(corpus.turns.is_empty());
    assert_eq!(corpus.skipped.malformed_arguments, 1);
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_history_eval::tests::extracts_successful_calls_in_assistant_order_with_json_arguments -- --exact
rtk cargo test -p threadlane-runtime --features needle needle_history_eval::tests::counts_successful_call_with_non_object_arguments_as_malformed -- --exact
```

Expected: compilation fails because the public corpus/call types and `malformed_arguments` counter do not exist.

- [ ] **Step 3: Add the minimal ordered representation and extraction API**

Replace the private `EvalExample` extraction core with these types; retain a `BTreeSet` only when evaluation derives expected names.

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct NeedleHistoryCall {
    pub name: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NeedleHistoryTurn {
    pub session_file: PathBuf,
    pub timestamp: u64,
    pub prompt: String,
    pub calls: Vec<NeedleHistoryCall>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NeedleHistoryCorpus {
    pub turns: Vec<NeedleHistoryTurn>,
    pub skipped: NeedleEvalSkipped,
}
```

For each successful assistant call, parse `call.function.arguments` as `serde_json::Value`, require `Value::Object`, and push calls in the assistant vector's order. If any successful call in a turn has invalid/non-object arguments, exclude the whole turn and increment `malformed_arguments`; partial multi-call labels are not valid training data.

Add `malformed_arguments` to `NeedleEvalSkipped::total`, aggregation, display output, and serde round trips so aggregate reporting remains complete.

Keep `expected` construction inside evaluation:

```rust
let expected = turn.calls.iter().map(|call| call.name.clone()).collect::<BTreeSet<_>>();
```

- [ ] **Step 4: Run all evaluator tests**

Run: `rtk cargo test -p threadlane-runtime --features needle needle_history_eval::tests`

Expected: all current and new evaluator tests pass; existing strict multi-tool metrics remain unchanged.

- [ ] **Step 5: Commit Task 1**

```bash
rtk git add crates/threadlane-runtime/src/needle_history_eval.rs
rtk git commit -m "refactor(runtime): share ordered Needle history labels"
```

---

### Task 2: Export a Redacted, Session-Split Dataset and Manifest

**Files:**
- Create: `crates/threadlane-runtime/src/needle_training.rs`
- Modify: `crates/threadlane-runtime/src/lib.rs`

**Interfaces:**
- Consumes: `NeedleHistoryCorpus`, `NeedleHistoryTurn`, and `AgentToolDefinition` from Task 1.
- Produces: `export_needle_dataset(config: &NeedleDatasetConfig, definitions: &[AgentToolDefinition]) -> Result<NeedleTrainingManifest, String>`.
- Produces: `load_training_manifest(work_dir: &Path) -> Result<NeedleTrainingManifest, String>` and `save_training_manifest(...)` for later tasks.
- Produces artifact names through constants: `train.jsonl`, `manifest.json`, `adapter.pkl`, `candidate.cact`, `current-eval.json`, `candidate-eval.json`.

- [ ] **Step 1: Write failing unit tests for redaction and deterministic splitting**

```rust
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
    assert_eq!(split.holdout_sessions, vec![PathBuf::from("b.jsonl"), PathBuf::from("c.jsonl")]);
    assert!(split.train_sessions.iter().all(|path| !split.holdout_sessions.contains(path)));
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
```

- [ ] **Step 2: Run the new module tests and verify failure**

Run: `rtk cargo test -p threadlane-runtime --features needle needle_training::tests`

Expected: compilation fails because `needle_training` and its types do not exist.

- [ ] **Step 3: Define the serialized dataset and manifest contract**

```rust
pub const TRAIN_FILE: &str = "train.jsonl";
pub const MANIFEST_FILE: &str = "manifest.json";
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
```

Derive `Serialize`/`Deserialize` for `NeedleEvalSkipped` and `Serialize` for `NeedleHistoryCall`. Rename the call's `name` and `arguments` fields exactly as Needle expects; do not serialize session metadata.

- [ ] **Step 4: Implement narrow credential redaction**

Use the existing `regex` crate and a `OnceLock<Vec<CredentialRule>>`; do not add a secret-scanning dependency. Define explicit rules for private-key blocks, bearer tokens, common key prefixes, and credential assignment values. `ExampleRedactor` owns a per-example `BTreeMap<String, String>` so an exact matched value receives the same placeholder in query and recursively visited argument strings.

```rust
fn redact_value(value: &mut Value, redactor: &mut ExampleRedactor) {
    match value {
        Value::String(text) => *text = redactor.redact_text(text),
        Value::Array(values) => values.iter_mut().for_each(|v| redact_value(v, redactor)),
        Value::Object(values) => values.values_mut().for_each(|v| redact_value(v, redactor)),
        _ => {}
    }
}
```

Keep rule names in aggregate counts, but never store matched text. Include tests for a bearer token, assignment, API-key prefix, private-key block, and an ordinary path that must remain unchanged.

- [ ] **Step 5: Implement the deterministic session split and atomic export**

Group turns by relative session filename, sort groups by `(minimum timestamp, filename)`, then move newest groups to holdout until `holdout_turns >= ceil(total_turns * 0.20)`, never moving the final training group. Sort both final filename lists chronologically and return an error when fewer than two groups are eligible.

Serialize only training turns as one compact JSON object per line in a temporary owner-only file. On Unix, open with `OpenOptionsExt::mode(0o600)`; on other platforms use `OpenOptions` without widening existing permissions. Flush and `sync_all`, calculate SHA-256 over the exact bytes, write `manifest.json` the same way, then rename temporary files into place. Reject existing outputs unless `replace` is true.

- [ ] **Step 6: Run focused and full runtime tests**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_training::tests
rtk cargo test -p threadlane-runtime --features needle needle_history_eval::tests
```

Expected: all tests pass, including file mode `0o600` on Unix and unchanged evaluator metrics.

- [ ] **Step 7: Commit Task 2**

```bash
rtk git add crates/threadlane-runtime/src/needle_training.rs crates/threadlane-runtime/src/lib.rs crates/threadlane-runtime/src/needle_history_eval.rs
rtk git commit -m "feat(runtime): export Needle training datasets"
```

---

### Task 3: Add the Project-Aware Dataset Command

**Files:**
- Create: `crates/threadlane-session/src/bin/needle_project_train.rs`
- Modify: `crates/threadlane-session/Cargo.toml`
- Modify: `Justfile`

**Interfaces:**
- Consumes: `export_needle_dataset` from Task 2 and `CodingAgent::configured_tool_definitions()`.
- Produces CLI: `needle-project-train dataset --project <dir> --sessions <dir> --work-dir <dir> [--replace]`.
- Produces recipe: `just needle_dataset` with project-local defaults.

- [ ] **Step 1: Write failing parser tests**

```rust
#[test]
fn parses_dataset_with_project_defaults_and_replace() {
    let command = parse_args([
        "dataset", "--project", "/tmp/p", "--sessions", "/tmp/p/.threadlane/sessions",
        "--work-dir", "/tmp/p/.threadlane/needle-training", "--replace",
    ]).unwrap();
    assert!(matches!(command, Command::Dataset { replace: true, .. }));
}

#[test]
fn rejects_unknown_dataset_flags() {
    assert_eq!(parse_args(["dataset", "--upload"]).unwrap_err(), USAGE);
}
```

- [ ] **Step 2: Run the binary test and verify failure**

Run: `rtk cargo test -p threadlane-session --features needle --bin needle-project-train`

Expected: Cargo reports that the binary target does not exist.

- [ ] **Step 3: Register the binary and implement only `dataset`**

Add the feature-gated Cargo target:

```toml
[[bin]]
name = "needle-project-train"
path = "src/bin/needle_project_train.rs"
required-features = ["needle"]
```

Use the same `CodingAgent::new` and `refresh_mcp().await` catalogue setup as `needle_project_eval.rs`. Dispatch `dataset` to `export_needle_dataset`, print only aggregate counts and artifact paths, and return exit code `3` for invalid arguments/input.

- [ ] **Step 4: Add the first thin Just recipe**

```make
needle_dataset project="." sessions=".threadlane/sessions" work_dir=".threadlane/needle-training" replace="":
    cargo run --release -p threadlane-session --features needle --bin needle-project-train -- \
        dataset --project "{{project}}" --sessions "{{sessions}}" --work-dir "{{work_dir}}" {{replace}}
```

The optional `replace` value is empty or `--replace`; do not silently overwrite.

- [ ] **Step 5: Run parser tests and a help smoke test**

Run:

```bash
rtk cargo test -p threadlane-session --features needle --bin needle-project-train
rtk cargo run --release -p threadlane-session --features needle --bin needle-project-train -- --help
```

Expected: parser tests pass and help lists the dataset command without loading a model or modifying files.

- [ ] **Step 6: Commit Task 3**

```bash
rtk git add crates/threadlane-session/src/bin/needle_project_train.rs crates/threadlane-session/Cargo.toml Justfile
rtk git commit -m "feat(session): add Needle dataset command"
```

---

### Task 4: Orchestrate Upstream LoRA Training and Candidate Build

**Files:**
- Modify: `crates/threadlane-runtime/src/needle_training.rs`
- Modify: `crates/threadlane-session/src/bin/needle_project_train.rs`
- Modify: `Justfile`

**Interfaces:**
- Produces: `run_needle_finetune(work_dir: &Path, needle_executable: &OsStr) -> Result<NeedleTrainingManifest, String>`.
- Produces CLI: `needle-project-train finetune --work-dir <dir> [--needle <path>]`.
- Produces recipe: `just needle_finetune`.

- [ ] **Step 1: Write a failing fake-CLI orchestration test**

On Unix, create an executable shell fixture named `needle` that records arguments and creates the path passed after `--out`; the training fixture supplies `checkpoints/needle2.pkl` so base hashing never depends on a download.

```rust
#[test]
#[cfg(unix)]
fn finetune_runs_upstream_commands_and_records_artifact_hashes() {
    let temp = training_fixture_with_manifest_and_base_checkpoint();
    let needle = fake_needle(temp.path(), "2.0-test");
    let manifest = run_needle_finetune(temp.path(), needle.as_os_str()).unwrap();
    let calls = std::fs::read_to_string(temp.path().join("needle.calls")).unwrap();
    assert!(calls.contains("finetune train.jsonl --epochs 10"));
    assert!(calls.contains("build checkpoints/needle2.pkl --lora"));
    assert_eq!(manifest.needle_version.as_deref(), Some("2.0-test"));
    assert!(manifest.adapter_sha256.is_some());
    assert!(manifest.candidate_sha256.is_some());
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run: `rtk cargo test -p threadlane-runtime --features needle needle_training::tests::finetune_runs_upstream_commands_and_records_artifact_hashes -- --exact`

Expected: compilation fails because `run_needle_finetune` does not exist.

- [ ] **Step 3: Implement checked temporary-artifact orchestration**

Run `<needle> --version`, then execute from `work_dir`:

```rust
Command::new(needle)
    .current_dir(work_dir)
    .args(["finetune", TRAIN_FILE, "--epochs", "10", "--out", "adapter.pkl.tmp"])
    .status()
```

After success, rename the adapter temp file and execute:

```text
needle build checkpoints/needle2.pkl --lora adapter.pkl --out candidate.cact.tmp
```

Load and validate the existing manifest before running. On any missing executable, non-zero exit, or missing output, remove only the task's known temporary artifact and return a concise error. Hash the base checkpoint, adapter, and completed candidate, record the CLI version, atomically rewrite the manifest, and never invoke `pip` or upload flags.

- [ ] **Step 4: Add CLI dispatch and recipe**

```make
needle_finetune work_dir=".threadlane/needle-training" needle="needle":
    cargo run --release -p threadlane-session --features needle --bin needle-project-train -- \
        finetune --work-dir "{{work_dir}}" --needle "{{needle}}"
```

The missing-executable message must include `pip install cactus-needle` and the Apple GPU option `pip install "cactus-needle[metal]"`, but must not run either command.

- [ ] **Step 5: Run runtime and binary tests**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_training::tests
rtk cargo test -p threadlane-session --features needle --bin needle-project-train
```

Expected: fake CLI success/failure tests and command parsing pass without network or JAX.

- [ ] **Step 6: Commit Task 4**

```bash
rtk git add crates/threadlane-runtime/src/needle_training.rs crates/threadlane-session/src/bin/needle_project_train.rs Justfile
rtk git commit -m "feat: orchestrate Needle LoRA training"
```

---

### Task 5: Evaluate Current and Candidate Weights on Manifest Holdout

**Files:**
- Modify: `crates/threadlane-runtime/src/needle_history_eval.rs`
- Modify: `crates/threadlane-runtime/src/needle_training.rs`
- Modify: `crates/threadlane-session/src/bin/needle_project_train.rs`
- Modify: `Justfile`

**Interfaces:**
- Produces: `run_needle_eval_for_paths(paths: &[PathBuf], definitions: &[AgentToolDefinition], model_path: &Path) -> Result<NeedleEvalReport, String>`.
- Produces: `compare_candidate(manifest, current, candidate) -> NeedleCandidateComparison`.
- Produces CLI: public `evaluate` plus internal `evaluate-one`; the public command spawns the current executable twice so each model loads in a fresh process.
- Produces recipe: `just needle_evaluate_candidate`.

- [ ] **Step 1: Write failing report serialization and comparison tests**

```rust
#[test]
fn report_json_round_trips_without_prompt_data() {
    let report = passing_report(200, 199);
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(serde_json::from_str::<NeedleEvalReport>(&json).unwrap(), report);
    assert!(!json.contains("prompt"));
}

#[test]
fn comparison_requires_strict_top_five_improvement() {
    let manifest = non_pilot_manifest(200);
    let current = passing_report(200, 198);
    let equal = passing_report(200, 198);
    let better = passing_report(200, 199);
    assert!(!compare_candidate(&manifest, &current, &equal).promotable);
    assert!(compare_candidate(&manifest, &current, &better).promotable);
}
```

- [ ] **Step 2: Run focused tests and verify failure**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle report_json_round_trips_without_prompt_data -- --exact
rtk cargo test -p threadlane-runtime --features needle comparison_requires_strict_top_five_improvement -- --exact
```

Expected: compilation fails because report serde and comparison types are absent.

- [ ] **Step 3: Add explicit paths/model evaluation without changing runtime routing**

Derive `Serialize`/`Deserialize` for `NeedleEvalDecision`, `NeedleEvalSkipped`, and `NeedleEvalReport`. Extract the evaluator core so it accepts exact session paths and loads a `V2Engine` with `validate_needle_model_path(model_path)` instead of the runtime `OnceLock`. Keep the existing directory/environment entry points as wrappers so `evaluate_local` behavior and exit codes do not change.

Validate every holdout path by joining the manifest's relative filename to `sessions_dir`, rejecting absolute paths, parent components, missing files, duplicates, or files outside the canonical sessions directory.

- [ ] **Step 4: Implement pure comparison and report persistence**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeedleCandidateComparison {
    pub promotable: bool,
    pub reasons: Vec<String>,
}
```

Require equal eligible counts, matching catalogue hashes, report model hashes matching the current/candidate files, at least 200 holdout examples, candidate recall `candidate.top_five_passes * 100 >= candidate.eligible * 99`, and strict improvement. A pilot manifest always contributes the reason `dataset is a pilot`.

Write aggregate JSON reports atomically to `current-eval.json` and `candidate-eval.json`. Reports contain no prompt/session fields by type construction.

- [ ] **Step 5: Implement separate-process CLI evaluation**

The public `evaluate` command validates manifest/artifact hashes, then calls `std::env::current_exe()` twice with internal `evaluate-one` arguments: once for the repository-local current model and once for `candidate.cact`. Each child discovers the project catalogue, resolves only manifest holdout sessions, writes one report, and exits `0` for a valid aggregate report regardless of pass/inconclusive status. The parent loads both reports, prints them plus comparison reasons, and returns `0` for a valid comparison or `3` for command/input/model errors.

Add the recipe:

```make
needle_evaluate_candidate project="." sessions=".threadlane/sessions" work_dir=".threadlane/needle-training":
    cargo run --release -p threadlane-session --features needle --bin needle-project-train -- \
        evaluate --project "{{project}}" --sessions "{{sessions}}" --work-dir "{{work_dir}}"
```

- [ ] **Step 6: Run focused regression tests and the existing base evaluator**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_history_eval::tests
rtk cargo test -p threadlane-runtime --features needle needle_training::tests
rtk cargo test -p threadlane-session --features needle --bin needle-project-train
rtk just evaluate_local
```

Expected: automated tests pass. The local evaluator prints the same aggregate corpus size/metrics as before and may exit `2` because the current history is below 200 eligible turns; record that exit as expected, not a test failure.

- [ ] **Step 7: Commit Task 5**

```bash
rtk git add crates/threadlane-runtime/src/needle_history_eval.rs crates/threadlane-runtime/src/needle_training.rs crates/threadlane-session/src/bin/needle_project_train.rs Justfile
rtk git commit -m "feat: compare Needle candidate weights"
```

---

### Task 6: Add Recoverable, Gated Promotion

**Files:**
- Modify: `crates/threadlane-runtime/src/needle_training.rs`
- Modify: `crates/threadlane-session/src/bin/needle_project_train.rs`
- Modify: `Justfile`

**Interfaces:**
- Produces: `promote_needle_candidate(work_dir: &Path, model_path: &Path, definitions: &[AgentToolDefinition]) -> Result<(), String>`.
- Produces CLI: `needle-project-train promote --project <dir> --work-dir <dir>`; the model target is always `<project>/needle/needle2.cact`.
- Produces recipe: `just needle_promote`.

- [ ] **Step 1: Write failing policy and recovery tests**

```rust
#[test]
fn pilot_candidate_cannot_replace_current_model() {
    let fixture = promotion_fixture(true, 199, 199);
    let before = std::fs::read(&fixture.model).unwrap();
    let error = promote_needle_candidate(&fixture.work_dir, &fixture.model, &fixture.definitions).unwrap_err();
    assert!(error.contains("pilot"));
    assert_eq!(std::fs::read(&fixture.model).unwrap(), before);
}

#[test]
fn qualifying_candidate_replaces_model_and_preserves_backup() {
    let fixture = promotion_fixture(false, 198, 199);
    let old = std::fs::read(&fixture.model).unwrap();
    let candidate = std::fs::read(fixture.work_dir.join(CANDIDATE_FILE)).unwrap();
    promote_needle_candidate(&fixture.work_dir, &fixture.model, &fixture.definitions).unwrap();
    assert_eq!(std::fs::read(&fixture.model).unwrap(), candidate);
    assert_eq!(std::fs::read(fixture.model.with_extension("cact.bak")).unwrap(), old);
}
```

- [ ] **Step 2: Run focused tests and verify failure**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_training::tests::pilot_candidate_cannot_replace_current_model -- --exact
rtk cargo test -p threadlane-runtime --features needle needle_training::tests::qualifying_candidate_replaces_model_and_preserves_backup -- --exact
```

Expected: compilation fails because promotion is not implemented.

- [ ] **Step 3: Implement gate revalidation and staged replacement**

Load the manifest and both reports fresh. Recompute the supplied live catalogue plus dataset/base/adapter/candidate/current-model hashes and call the pure comparison from Task 5. Return all failed reasons before any write.

For a qualifying candidate:

1. Copy the candidate to `needle2.cact.tmp` beside the target and `sync_all` it.
2. Copy the current target to `needle2.cact.bak` and `sync_all` it.
3. Rename `needle2.cact.tmp` over `needle2.cact` on platforms where replacement is atomic; if the platform requires removing an existing destination, return an unsupported-operation error rather than creating a destructive gap.
4. On failure before rename, remove only the known staging file and leave the target intact.

Do not read `THREADLANE_NEEDLE_WEIGHTS`; the CLI constructs the fixed project-local target and uses the same `CodingAgent` capability discovery as dataset/evaluation before calling promotion.

- [ ] **Step 4: Add CLI dispatch and recipe**

```make
needle_promote project="." work_dir=".threadlane/needle-training":
    cargo run --release -p threadlane-session --features needle --bin needle-project-train -- \
        promote --project "{{project}}" --work-dir "{{work_dir}}"
```

Print the promoted candidate hash and backup path only after success. Invalid/pilot/non-improving candidates exit `3` and leave files unchanged.

- [ ] **Step 5: Run promotion, runtime, and binary tests**

Run:

```bash
rtk cargo test -p threadlane-runtime --features needle needle_training::tests
rtk cargo test -p threadlane-session --features needle --bin needle-project-train
```

Expected: every refusal test proves current bytes are unchanged; qualifying fixture proves both replacement and backup.

- [ ] **Step 6: Commit Task 6**

```bash
rtk git add crates/threadlane-runtime/src/needle_training.rs crates/threadlane-session/src/bin/needle_project_train.rs Justfile
rtk git commit -m "feat: gate Needle candidate promotion"
```

---

### Task 7: Document and Verify the Complete Local Pipeline

**Files:**
- Modify: `README.md`
- Modify if a durable new convention was discovered: `AGENTS.md`

**Interfaces:**
- Documents: upstream CLI prerequisite, private artifact location, four commands, pilot behavior, promotion gates, backup behavior, and no-upload guarantee.
- Verifies: all runtime/session/desktop integration remains green without touching user formatting changes.

- [ ] **Step 1: Update the README workflow**

Retain `just evaluate_local`, then add the approved sequence:

```bash
pip install cactus-needle              # or: pip install "cactus-needle[metal]"
just needle_dataset
just needle_finetune
just needle_evaluate_candidate
just needle_promote
```

State explicitly that `.threadlane/needle-training/train.jsonl` contains sensitive local prompts/arguments, is git-ignored and never uploaded, current history is expected to be a non-promotable pilot, and promotion requires 200 holdout turns plus 99 percent top-five recall and strict improvement.

- [ ] **Step 2: Run formatting and focused test suites**

Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p threadlane-runtime --features needle
rtk cargo test -p threadlane-session --features needle
rtk cargo check -p threadlane-gpui
rtk cargo test --workspace
rtk git diff --check
```

Expected: all tests/checks pass. Existing unrelated warnings may remain; no whitespace errors are introduced.

- [ ] **Step 3: Run the safe local acceptance path**

Run:

```bash
rtk just needle_dataset
rtk just needle_evaluate_candidate
```

`needle_evaluate_candidate` requires a manually produced candidate; if none exists, verify its exact actionable missing-candidate error instead of running training. Do not invoke `needle_finetune` automatically because it may download a checkpoint and perform expensive JAX work. Do not invoke promotion on pilot data.

Inspect only aggregate terminal output and `manifest.json`; never print `train.jsonl`.

- [ ] **Step 4: Confirm the final diff preserves user work**

Run:

```bash
rtk git status --short
rtk git diff --stat
```

Expected: the pre-existing unstaged changes in `tool_executor.rs` and `turn_driver.rs` remain unstaged and absent from task commits. Add an `AGENTS.md` edit only if implementation revealed a durable repository-specific rule not already documented.

- [ ] **Step 5: Commit documentation only**

```bash
rtk git add README.md AGENTS.md
rtk git commit -m "docs: explain local Needle fine-tuning"
```

If `AGENTS.md` was not changed, omit it from `git add`.

- [ ] **Step 6: Final verification summary**

Record the exact pass/fail counts, expected pilot status, aggregate holdout metrics if a candidate exists, commit list, and the unchanged user-owned files. Do not claim a real fine-tune or promotion occurred unless those commands were explicitly run and observed.

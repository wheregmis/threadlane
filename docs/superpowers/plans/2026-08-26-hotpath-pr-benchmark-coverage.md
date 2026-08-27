# Hotpath PR Benchmark Coverage Implementation Plan

> Historical plan: the implemented harnesses now live in `crates/threadlane-benchmarks`, and CI reports timing plus selective allocation metrics in one sticky PR comment. Commands and per-crate example paths below describe the original rollout and are superseded by the current design specification.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Report base-versus-head performance for deterministic runtime, tools, MCP, and terminal hot paths on every pull request.

**Architecture:** Each owning crate gets one Hotpath example with fixed data and multiple measured functions. A GitHub Actions matrix runs each example at the head and base revisions, then the existing Hotpath utility upserts one PR comment per stable suite ID.

**Tech Stack:** Rust 1.95, hotpath 0.24, Tokio, vt100, GitHub Actions, hotpath-utils.

**Spec:** `docs/superpowers/specs/2026-08-26-hotpath-pr-benchmark-coverage-design.md`

## Global Constraints

- Benchmark deterministic, headless paths only.
- Reuse Hotpath 0.24 and existing dependencies.
- Warm filesystem and subprocess setup outside steady-state measurements.
- A missing suite on the base revision skips only that suite.
- Finish with `cargo check -p threadlane-gpui` and `git diff --check`.

---

### Task 1: Expand the runtime benchmark

**Files:**
- Modify: `crates/threadlane-runtime/examples/hotpath_jsonl.rs`

**Interfaces:**
- Consumes: `MemoryStore::new`, `Reducer::reduce`, and the existing `fact` fixture.
- Produces: measured `reducer_replay(store: &MemoryStore)` alongside `append_scaling()` and `open_scaling()`.

- [ ] **Step 1: Capture the current report**

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/runtime-before.json \
  cargo run -p threadlane-runtime --release --example hotpath_jsonl --features hotpath
```

Expected: PASS; the JSON lacks `reducer_replay`.

- [ ] **Step 2: Add uncached reducer replay**

```rust
#[hotpath::measure]
fn reducer_replay(store: &MemoryStore) {
    std::hint::black_box(Reducer::reduce(&store).unwrap());
}
```

Import `MemoryStore` and `Reducer`. Build a 4,000-record memory store in `main`, outside measurement, then call `reducer_replay(&store)`. Keep `open_scaling` as the session reload measurement.

- [ ] **Step 3: Verify and commit**

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/runtime.json \
  cargo run -p threadlane-runtime --release --example hotpath_jsonl --features hotpath
hotpath-utils profile-pr --head-metrics /tmp/runtime.json --base-metrics /tmp/runtime.json \
  --benchmark-id runtime --dry-run
git add crates/threadlane-runtime/examples/hotpath_jsonl.rs
git commit -m "perf: expand runtime hotpath benchmark"
```

Expected: the dry run lists all three measured functions.

### Task 2: Add the tools search benchmark

**Files:**
- Modify: `crates/threadlane-tools/Cargo.toml`
- Create: `crates/threadlane-tools/examples/hotpath_search.rs`

**Interfaces:**
- Consumes: `grep_search(&Path, &str, Option<&str>)`.
- Produces: measured `search_warm_tree(root: &Path)`.

- [ ] **Step 1: Confirm the example is absent**

```bash
cargo run -p threadlane-tools --release --example hotpath_search --features hotpath
```

Expected: FAIL because the example and feature do not exist.

- [ ] **Step 2: Add the existing Hotpath manifest pattern**

```toml
[features]
default = []
hotpath = ["hotpath/hotpath"]

[dev-dependencies]
hotpath = "0.24"
tempfile = "3.8"
```

- [ ] **Step 3: Create the warm search example**

Create 200 fixed text files in a temp directory, call `grep_search` once to warm caches, then measure 20 calls:

```rust
#[hotpath::measure]
fn search_warm_tree(root: &Path) {
    for _ in 0..20 {
        std::hint::black_box(grep_search(root, "needle", Some("*.txt")).unwrap());
    }
}
```

Use `#[hotpath::main]`; setup stays in `main`, outside the measured function.

- [ ] **Step 4: Verify and commit**

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/tools.json \
  cargo run -p threadlane-tools --release --example hotpath_search --features hotpath
hotpath-utils profile-pr --head-metrics /tmp/tools.json --base-metrics /tmp/tools.json \
  --benchmark-id tools --dry-run
cargo test -p threadlane-tools search
git add crates/threadlane-tools/Cargo.toml crates/threadlane-tools/examples/hotpath_search.rs Cargo.lock
git commit -m "perf: benchmark warm repository search"
```

Expected: PASS; the report lists `search_warm_tree`.

### Task 3: Add the MCP steady-state benchmark

**Files:**
- Modify: `crates/threadlane-mcp/Cargo.toml`
- Create: `crates/threadlane-mcp/examples/hotpath_mcp.rs`

**Interfaces:**
- Consumes: `McpManager`, `McpSettings::save_global`, `McpToolExecutor`, and `ToolExecutor`.
- Produces: measured async `discover_repeat` and `tool_calls`.

- [ ] **Step 1: Confirm the example is absent**

```bash
cargo run -p threadlane-mcp --release --example hotpath_mcp --features hotpath
```

Expected: FAIL because the example and feature do not exist.

- [ ] **Step 2: Add benchmark dependencies**

Add the same `hotpath` feature and `hotpath = "0.24"` dev-dependency as Task 2; keep existing `tempfile`.

- [ ] **Step 3: Reuse the existing local stub shape**

Copy the minimal Unix shell stub and config fixture from `crates/threadlane-mcp/tests/perf_baseline.rs`; examples cannot import test modules. Warm the script once, do the initial discovery, then measure:

```rust
#[hotpath::measure]
async fn discover_repeat(manager: &McpManager) {
    std::hint::black_box(manager.discover_and_connect().await);
}

#[hotpath::measure]
async fn tool_calls(executor: &McpToolExecutor) {
    for _ in 0..20 {
        std::hint::black_box(
            executor.execute_tool("mcp__stub__echo", "{}").await.unwrap().unwrap(),
        );
    }
}
```

Use synchronous `#[hotpath::main]` with `tokio::runtime::Runtime::new().unwrap().block_on(async_main())`.

- [ ] **Step 4: Verify and commit**

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/mcp.json \
  cargo run -p threadlane-mcp --release --example hotpath_mcp --features hotpath
hotpath-utils profile-pr --head-metrics /tmp/mcp.json --base-metrics /tmp/mcp.json \
  --benchmark-id mcp --dry-run
cargo test -p threadlane-mcp --test session_reuse
git add crates/threadlane-mcp/Cargo.toml crates/threadlane-mcp/examples/hotpath_mcp.rs Cargo.lock
git commit -m "perf: benchmark MCP steady-state paths"
```

Expected: PASS; the report lists both measured functions.

### Task 4: Add the headless terminal benchmark

**Files:**
- Create: `crates/threadlane-gpui/examples/hotpath_terminal.rs`

**Interfaces:**
- Consumes: the existing `vt100` dependency directly.
- Produces: measured `parse_terminal_output` and `resize_and_scrollback`.

- [ ] **Step 1: Confirm the example is absent**

```bash
cargo run -p threadlane-gpui --release --example hotpath_terminal --features hotpath
```

Expected: FAIL because the example does not exist.

- [ ] **Step 2: Add fixed parser workloads**

Build 2,000 ANSI-colored lines outside measurement. `parse_terminal_output` creates a `vt100::Parser`, processes the bytes, and black-boxes `state_formatted()`. `resize_and_scrollback` parses once, applies `[(40, 120), (24, 80), (60, 160), (24, 80)]` through `screen_mut().set_size`, clamps scrollback exactly like the production worker, and black-boxes the formatted state.

```rust
#[hotpath::main]
fn main() {
    let bytes = fixture();
    parse_terminal_output(&bytes);
    resize_and_scrollback(&bytes);
}
```

- [ ] **Step 3: Verify and commit**

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/terminal.json \
  cargo run -p threadlane-gpui --release --example hotpath_terminal --features hotpath
hotpath-utils profile-pr --head-metrics /tmp/terminal.json --base-metrics /tmp/terminal.json \
  --benchmark-id terminal --dry-run
cargo check -p threadlane-gpui
git add crates/threadlane-gpui/examples/hotpath_terminal.rs
git commit -m "perf: benchmark terminal parser hot paths"
```

Expected: PASS; the report lists both measured functions.

### Task 5: Matrix the PR profiling workflow

**Files:**
- Modify: `.github/workflows/hotpath-profile.yml`

**Interfaces:**
- Consumes: all four example commands.
- Produces: `hotpath-metrics-<id>` artifacts containing `<id>/head.json`, optional `base.json`, and `pr_number.txt`.

- [ ] **Step 1: Prove matrix coverage is absent**

```bash
for id in runtime tools mcp terminal; do
  rg -q "id: $id" .github/workflows/hotpath-profile.yml || echo "missing $id"
done
```

Expected: all four IDs are reported missing.

- [ ] **Step 2: Add the include matrix**

Set `strategy.fail-fast: false`. Add `id`, `package`, `example`, and `example_path` for runtime/`hotpath_jsonl`, tools/`hotpath_search`, mcp/`hotpath_mcp`, and terminal/`hotpath_terminal`.

Both benchmark steps must set explicit paths:

```yaml
env:
  HOTPATH_OUTPUT_FORMAT: json
  HOTPATH_OUTPUT_PATH: /tmp/metrics/${{ matrix.id }}/head.json
```

Use `base.json` for base. Guard only the base command with `test -f '${{ matrix.example_path }}'`. Upload `/tmp/metrics/${{ matrix.id }}/` as `hotpath-metrics-${{ matrix.id }}`.

- [ ] **Step 3: Validate and commit**

```bash
for id in runtime tools mcp terminal; do rg -q "id: $id" .github/workflows/hotpath-profile.yml; done
rg -q 'base.json' .github/workflows/hotpath-profile.yml
git diff --check
git add .github/workflows/hotpath-profile.yml
git commit -m "ci: profile deterministic hotpath suites"
```

### Task 6: Comment once per suite

**Files:**
- Modify: `.github/workflows/hotpath-comment.yml`

**Interfaces:**
- Consumes: `hotpath-metrics-*` artifacts.
- Produces: one upserted comment keyed by each suite directory name.

- [ ] **Step 1: Prove multi-artifact download is absent**

```bash
rg -q 'pattern: hotpath-metrics-\*' .github/workflows/hotpath-comment.yml
```

Expected: FAIL.

- [ ] **Step 2: Download and compare every complete pair**

Set download inputs to:

```yaml
pattern: hotpath-metrics-*
path: /tmp/metrics/
merge-multiple: true
```

Loop over `/tmp/metrics/*`, skip non-directories and suites lacking `base.json`, and call `profile-pr` with suite-local head/base/PR files plus:

```bash
--benchmark-id "$(basename "$metrics")"
```

- [ ] **Step 3: Validate and commit**

```bash
rg -q 'pattern: hotpath-metrics-\*' .github/workflows/hotpath-comment.yml
rg -q -- '--benchmark-id "$(basename "$metrics")"' .github/workflows/hotpath-comment.yml
git diff --check
git add .github/workflows/hotpath-comment.yml
git commit -m "ci: comment each hotpath benchmark suite"
```

### Task 7: End-to-end verification

**Files:**
- Verify only; correct prior files only when a command exposes a defect.

**Interfaces:**
- Consumes: all examples and workflows.
- Produces: evidence that every report is comparable and project checks pass.

- [ ] **Step 1: Run all four release examples**

Run each Task 1-4 command with a final output path, then compare each JSON file against itself using `hotpath-utils profile-pr --dry-run --benchmark-id <id>`.

Expected: all four comparisons pass and list their intended functions.

- [ ] **Step 2: Run focused checks**

```bash
cargo test -p threadlane-tools search
cargo test -p threadlane-mcp --test session_reuse
cargo check -p threadlane-gpui
git diff --check
git status --short
```

Expected: tests/checks pass, no whitespace errors, and only intentional changes remain.

- [ ] **Step 3: Commit only if verification required a correction**

Stage exactly the files corrected during verification and commit them with `git commit -m "fix: make hotpath benchmarks reproducible"`. Make no empty commit when no correction was required.

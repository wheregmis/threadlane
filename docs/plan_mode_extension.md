# Plan Mode Extension Guide (`mypi` & `mypi-gui`)

Plan Mode is a read-only exploration and structured task planning mode for `mypi`. It allows the model to safely analyze codebases, propose step-by-step plans under a `Plan:` header, and track progress via TODO items without accidentally modifying files or running destructive shell commands.

---

## Key Features

1. **Read-Only Safety Guard**:
   - Disables modifying tools (`write_file`, `edit_file`) while Plan Mode is enabled.
   - Restricts shell execution (`run_command`) strictly to safe read-only commands (e.g. `ls`, `cat`, `grep`, `git diff`, `git status`, `cargo check`).

2. **Automatic Plan Parsing & Progress Tracking**:
   - Parses structured `Plan:` blocks in model output.
   - Extracts numbered plan steps into an interactive TODO checklist.
   - Tracks completion using `[DONE:n]` markers in assistant responses.

3. **Interactive Slash Commands**:
   - `/plan`: Toggle Plan Mode on/off.
   - `/todos`: View current plan items and completion progress.

4. **WASI WebAssembly Sandbox Integration**:
   - Extensions can be compiled to `.wasm` and loaded dynamically via `wasmi`.

---

## Installation & Setup

### 1. Built-In Integration
Plan Mode is natively supported inside `crates/mypi-agent`.

To enable Plan Mode on startup in your Rust code:
```rust
use mypi_agent::{CodingAgent, CodingAgentOptions};
use std::path::PathBuf;

let options = CodingAgentOptions {
    api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
    account_id: None,
    model: "gpt-4o".to_string(),
    work_dir: PathBuf::from("."),
    session_file: None,
    enable_plan_mode: true, // Enable Plan Mode at startup
};

let mut agent = CodingAgent::new(options);
```

### 2. WASI WebAssembly Extension Installation
To build and use Plan Mode as a standalone WASI WebAssembly extension:

1. **Compile the WASI extension crate**:
   ```bash
   cd extensions/plan_mode_ext
   cargo build --target wasm32-wasip1 --release
   ```

2. **Deploy the `.wasm` file**:
   Copy the generated `.wasm` binary into your workspace's `extensions/` directory:
   ```bash
   mkdir -p ./extensions
   cp target/wasm32-wasip1/release/plan_mode_ext.wasm ./extensions/
   ```

3. **Launch `mypi-gui` or CLI**:
   `mypi` will automatically discover `./extensions/plan_mode_ext.wasm`, register its tools and commands, and display:
   ```text
   Loaded 1 WASI extensions into sandboxed execution environment.
   ```

---

## Usage & Workflow

### Enabling Plan Mode
In `mypi-gui` or CLI prompt bar, type:
```text
/plan
```
Output:
```text
🟢 Plan Mode ENABLED (Read-only exploration active)
```

### Creating a Plan
Ask `mypi` to analyze a codebase task. In Plan Mode, `mypi` will output a structured plan:
```text
Plan:
1. Inspect src/main.rs for entry point configuration
2. Verify dependency versions in Cargo.toml
3. Run cargo check to validate syntax
```

### Checking Plan Progress
Type `/todos` to view active items and completion status:
```text
/todos
```
Output:
```text
📋 Current Plan Progress:
  ✅ 1. Inspect src/main.rs for entry point configuration
  ⏳ 2. Verify dependency versions in Cargo.toml
  ⏳ 3. Run cargo check to validate syntax

Progress: 1/3 steps completed.
```

### Disabling Plan Mode
Once you review and approve the plan, toggle off Plan Mode to restore write tools:
```text
/plan
```
Output:
```text
⚪ Plan Mode DISABLED (Full tool write access restored)
```

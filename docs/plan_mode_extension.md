# Plan Mode Extension Guide (`threadlane` & `threadlane-gui`)

Plan Mode is a session-scoped, read-only exploration and structured task-planning mode for `threadlane`. It lets the model analyze a codebase and propose step-by-step plans under a `Plan:` header without using the built-in file-writing tools.

---

## Key Features

1. **Read-Only Safety Guard**:
   - Disables modifying tools (`write_file`, `edit_file`) while Plan Mode is enabled.
   - Arbitrary shell commands are not currently restricted by Plan Mode; review commands before running them.

2. **Automatic Plan Parsing**:
   - Parses structured `Plan:` blocks in model output.
   - Extracts numbered plan steps into a TODO checklist.
   - Completion markers are not yet persisted by the reference extension.

3. **Interactive Slash Commands**:
   - `/plan`: Toggle Plan Mode on/off for the current session.
   - `/todos`: View the current session's plan items.

4. **WASI WebAssembly Sandbox Integration**:
   - Extensions can be compiled to `.wasm` and loaded dynamically via `wasmi`.

---

## Installation & Setup

### 1. Built-In Integration
Plan Mode is an ordinary API v2 WASI extension loaded by the host in
`crates/threadlane-coding-agent`. Use `/plan` to change the generic `tools.set_policy`
state for the active session; there is no plan-specific startup flag.

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

3. **Launch `threadlane-gui` or CLI**:
   `threadlane` will automatically discover `./extensions/plan_mode_ext.wasm`, register its tools and commands, and display:
   ```text
   Loaded 1 WASI extensions into sandboxed execution environment.
   ```

---

## Usage & Workflow

### Enabling Plan Mode
In `threadlane-gui` or CLI prompt bar, type:
```text
/plan
```
Output:
```text
🟢 Plan Mode ENABLED (Read-only exploration active)
```

### Creating a Plan
Ask `threadlane` to analyze a codebase task. In Plan Mode, `threadlane` will output a structured plan:
```text
Plan:
1. Inspect src/main.rs for entry point configuration
2. Verify dependency versions in Cargo.toml
3. Run cargo check to validate syntax
```

### Checking the Current Session Plan
Type `/todos` to view active items and completion status:
```text
/todos
```
Output:
```text
📋 Current Plan:
  ⏳ 1. Inspect src/main.rs for entry point configuration
  ⏳ 2. Verify dependency versions in Cargo.toml
  ⏳ 3. Run cargo check to validate syntax
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

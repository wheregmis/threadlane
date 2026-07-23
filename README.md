# Threadlane

**Threadlane** is a high-performance, GPU-accelerated desktop AI coding assistant built in Rust using the [Makepad](https://github.com/makepad/makepad) UI framework. Designed as a lightweight, native alternative for AI pair programming, Threadlane features a modern dark-mode interface, multi-project workspace navigation, background subagents, and WASI capability extensions.

---

## Key Features

- **GPU-Accelerated Native Interface**: Rendered with Makepad for smooth, low-latency performance and high frame rates.
- **Multi-Project Workspace Navigation**: Quickly attach, detach, and switch between project workspaces while retaining local drafts and context.
- **WASI Extension Architecture**: Extend agent capabilities dynamically via WebAssembly (WASI) modules importing the `threadlane_host` capability broker.
- **Subagents & Tool Execution**: Dispatch subagents for parallel exploration, plan execution, and code generation with real-time streaming updates.
- **Context Compaction & Session Trees**: Automatic branch-aware context pruning, token compaction, and durable session history.

---

## Quick Start

### Prerequisites

- **Rust**: Latest stable Rust toolchain (2021 edition or newer).
- **C Compiler & System Dependencies**: Standard C toolchain (`clang` or `gcc`) for compiling native dependencies and Makepad shaders.

### Building & Running

1. **Build WASI Extensions**:
   Compile bundled WASI extensions (such as subagents and capability brokers) and deploy them to `.threadlane/`:
   ```bash
   ./scripts/build_extensions.sh
   ```

2. **Run Threadlane Desktop App**:
   ```bash
   cargo run --bin threadlane
   ```

3. **Run Test Suite**:
   ```bash
   cargo test --workspace
   ```

---

## Packaging & Releases

Threadlane leverages [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager) and [`robius-packaging-commands`](https://github.com/project-robius/robius-packaging-commands) to bundle native desktop installers (`.dmg` on macOS, `.exe`/NSIS on Windows, and `.deb` on Linux).

### Packaging Locally

1. **Install Packaging Tooling**:
   ```bash
   cargo install --locked cargo-packager
   cargo install --locked --git https://github.com/project-robius/robius-packaging-commands.git
   ```

2. **Build Release Extensions & App Bundle**:
   ```bash
   ./scripts/build_extensions.sh
   cd crates/threadlane
   cargo packager --release
   ```
   The generated desktop packages will be placed in `crates/threadlane/dist/`.

### Signed Application Updates

Threadlane uses [`cargo-packager-updater`](https://crates.io/crates/cargo-packager-updater) to download, verify, install, and relaunch signed macOS application updates. Generate the updater key pair once and keep using the same key for every release:

```bash
cargo install --locked cargo-packager --version 0.11.8
cargo packager signer generate \
  --path threadlane-updater.key \
  --password 'a-strong-password'
```

The generated key files are ignored by Git. Configure GitHub Actions without committing them:

```bash
gh variable set THREADLANE_UPDATER_PUBLIC_KEY < threadlane-updater.key.pub
gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY < threadlane-updater.key
gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD
```

The release binary embeds `THREADLANE_UPDATER_PUBLIC_KEY`. The private key is available only to the release workflow and signs `Threadlane.app.tar.gz`; existing installations reject updates whose signatures do not match the embedded public key. Do not rotate or lose the private key without providing an explicit updater-key migration path.

### Automated Release Workflow

Threadlane uses GitHub Actions (`.github/workflows/release.yml`) for automated macOS builds. The release tag must exactly match the version in `crates/threadlane/Cargo.toml`. For example:

```bash
git tag v0.1.0
git push origin v0.1.0
```

A tagged build publishes the user-facing `.dmg`, the signed `.app.tar.gz` updater bundle, its `.sig`, and `latest.json` to the GitHub Release.

---

## Workspace Architecture

Threadlane is structured as a modular Cargo workspace:

| Crate | Description |
| :--- | :--- |
| [**`threadlane`**](crates/threadlane) | Desktop UI application built with Makepad. Manages windows, rendering, chat panels, sessions, and keyboard commands. |
| [**`threadlane-coding-agent`**](crates/threadlane-coding-agent) | High-level coding agent harness. Coordinates system prompts, skill discovery, subagents, and WASI extensions. |
| [**`threadlane-agent`**](crates/threadlane-agent) | Generic LLM agent execution engine. Handles session trees, context compaction, and tool execution loops. |
| [**`threadlane-provider`**](crates/threadlane-provider) | Streaming LLM client supporting OpenAI REST endpoints and Codex models with device authentication. |
| [**`threadlane-tools`**](crates/threadlane-tools) | Built-in workspace file system tools, pattern searching (`grep_search`), and sandboxed process execution. |

---

## Documentation & Resources

- [Makepad UI Reference](Makepad.md)
- [Crate Documentation & Architecture](crates/)
  - [`threadlane` (Desktop UI)](crates/threadlane/README.md)
  - [`threadlane-coding-agent` (Harness & Extensions)](crates/threadlane-coding-agent/README.md)
  - [`threadlane-agent` (Execution Engine & Session Tree)](crates/threadlane-agent/README.md)
  - [`threadlane-provider` (LLM Client & Authentication)](crates/threadlane-provider/README.md)
  - [`threadlane-tools` (Workspace Primitives)](crates/threadlane-tools/README.md)

---

## License

MIT License.

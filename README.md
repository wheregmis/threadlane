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

#### Testing Updates Locally

`cargo run` can exercise update checks, signature verification, downloads, and the complete updater UI. Installation and relaunch are restricted to a packaged `.app` so a development run can never replace `target/debug`.

For a quick UI test against the published GitHub Releases manifest, embed the local public key for that run:

```bash
THREADLANE_UPDATER_PUBLIC_KEY="$(cat threadlane-updater.key.pub)" \
cargo run -p threadlane
```

To test an unpublished update locally, also override the manifest endpoint at compile time:

```bash
export THREADLANE_UPDATER_PUBLIC_KEY="$(cat threadlane-updater.key.pub)"
export THREADLANE_UPDATER_ENDPOINT="http://127.0.0.1:8787/latest.json"
```

Build and preserve the lower-version app first, then increase `crates/threadlane/Cargo.toml` to the test update version and create a signed app archive:

```bash
./scripts/build_extensions.sh
cargo build --release --bin threadlane
cargo packager --release --formats app \
  --manifest-path crates/threadlane/Cargo.toml
mkdir -p "$HOME/Applications"
rm -rf "$HOME/Applications/Threadlane Test.app"
cp -R crates/threadlane/dist/Threadlane.app \
  "$HOME/Applications/Threadlane Test.app"

# Increase the threadlane package version before continuing.
rm -f crates/threadlane/dist/Threadlane.app.tar.gz*
cargo build --release --bin threadlane
CARGO_PACKAGER_SIGN_PRIVATE_KEY=threadlane-updater.key \
CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD='your-key-password' \
  cargo packager --release --formats app \
  --manifest-path crates/threadlane/Cargo.toml
```

Create `crates/threadlane/dist/latest.json`, using the higher test version and the generated signature:

```bash
TEST_VERSION=0.0.7
jq -n \
  --arg version "$TEST_VERSION" \
  --arg signature "$(cat crates/threadlane/dist/Threadlane.app.tar.gz.sig)" \
  '{
    version: $version,
    platforms: {
      "macos-aarch64": {
        url: "http://127.0.0.1:8787/Threadlane.app.tar.gz",
        signature: $signature,
        format: "app"
      }
    }
  }' > crates/threadlane/dist/latest.json

python3 -m http.server 8787 --directory crates/threadlane/dist
```

While that server is running, either run the app from the terminal to test check/download behavior:

```bash
cargo run -p threadlane
```

or open `$HOME/Applications/Threadlane Test.app` to test the complete install-and-relaunch flow. Restore the intended package version and unset `THREADLANE_UPDATER_ENDPOINT` afterward so normal builds use GitHub Releases.

### Unsigned macOS Distribution

Release bundles use an ad-hoc macOS signature so their internal resource seals are valid without an Apple Developer certificate. This does not establish Gatekeeper trust or notarize the application. After copying Threadlane to `/Applications`, users may need to allow it in **System Settings → Privacy & Security** or remove the quarantine attribute explicitly:

```bash
xattr -dr com.apple.quarantine /Applications/Threadlane.app
```

Only bypass quarantine for an artifact you trust. The release workflow verifies the ad-hoc `.app` and DMG signatures before publishing, while the separate Minisign updater signature authenticates automatic updates.

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

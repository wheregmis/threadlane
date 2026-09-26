<h1 align="center">
  <img src="assets/images/threadlane-logo.svg" width="48" align="top" style="vertical-align: top;" alt="Threadlane application icon">&nbsp;Threadlane
</h1>

<p align="center">A native desktop workspace for AI-assisted software development, built in Rust with GPUI.</p>

<p align="center">
  <a href="https://github.com/wheregmis/threadlane/actions/workflows/release.yml"><img alt="macOS release workflow" src="https://github.com/wheregmis/threadlane/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/wheregmis/threadlane/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/wheregmis/threadlane?display_name=tag&sort=semver"></a>
  <img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-d65d0e?logo=rust&logoColor=white">
  <img alt="GPUI" src="https://img.shields.io/badge/UI-GPUI-6f8cff">
</p>

Threadlane brings project workspaces, persistent conversation sessions, coding-agent execution, and developer tools into one native application. Its Rust workspace includes provider integrations, external ACP agents, MCP support, and sandboxed WASI extensions.

> **Release status:** The release workflow currently builds signed Apple Silicon macOS artifacts. The application can also be built from source on platforms supported by its dependencies.

<p align="center">
  <a href="assets/images/threadlane-workspace.png">
    <img src="assets/images/threadlane-workspace.png" width="100%" alt="Threadlane desktop workspace showing project sessions, rendered tool output, and slash-command completion">
  </a>
</p>

## Highlights

- **Native desktop workspace** — A Rust and GPUI application with multi-project workspaces, session trees, and integrated PTY terminals.
- **Coding-agent runtime** — Durable session orchestration, streamed agent activity, context compaction, plans, and execution history.
- **Provider and agent integrations** — Google Antigravity, OpenAI/Codex, OpenCode, and externally configured ACP agents.
- **Developer tooling** — Workspace file tools, ripgrep search, sandboxed process execution, MCP servers, and `line:hash`-anchored edits.
- **Extensibility** — Sandboxed WebAssembly System Interface (WASI) extensions and discovered skills.
- **Automations** — Recurring prompts with durable run history, fresh chats, optional isolated worktrees, and explicit permission handling.

## Quick Start

### Prerequisites

- Rust 1.95.0 or later. The repository pins this version in [`rust-toolchain.toml`](rust-toolchain.toml); CI and release packaging use the same pin.
- The WASI target: `rustup target add wasm32-wasip1`.
- A native C toolchain, such as Xcode Command Line Tools on macOS or `build-essential` on Ubuntu.

### Build and run

```bash
# Clone the repository
git clone https://github.com/wheregmis/threadlane.git
cd threadlane

# Build and install bundled WASI extensions for the local checkout
./scripts/build_extensions.sh

# macOS: build a development app bundle and run it
./scripts/run-gpui-macos.sh

# Linux and other supported environments
cargo run -p threadlane-gpui
```

On macOS, use `./scripts/run-gpui-macos.sh` rather than `cargo run -p threadlane-gpui`. Some framework calls require the application to run from an app bundle. The script creates `target/debug/Threadlane-dev.app`, preserves standard output and `RUST_LOG`, and accepts `--release` for a release build.

## Configure providers and agents

Threadlane supports the following connection methods:

- **Google Antigravity** — OAuth credentials with Cloud Code Assist endpoint discovery.
- **OpenAI/Codex** — Use the built-in PKCE device-authorization flow or configure an API key in Settings. Threadlane stores its credentials under `~/.threadlane` and can read Codex CLI credentials from `~/.codex/auth.json`.
- **External ACP agents** — Configure agent binaries in `~/.threadlane/acp.json` or `<project>/.threadlane/acp.json`, or use **Settings → ACP Agents**. Authenticate the external agent separately, then select it from the model picker or with `/model` as `acp/<id>`.

Example ACP configuration:

```jsonc
// ~/.threadlane/acp.json
{
  "agents": [
    {
      "id": "claude_code",
      "name": "Claude Code",
      // Applications launched from Finder do not inherit a shell PATH.
      // Use an absolute path for version-manager binaries such as npx.
      "command": "/Users/you/.nvm/versions/node/v22.0.0/bin/npx",
      "args": ["-y", "@zed-industries/claude-code-acp"]
    }
  ]
}
```

To add an API key, open **Settings → Providers** in Threadlane.

## Automations

Open **Automations** in the sidebar (or command palette), choose **New automation…**, and save a prompt, attached project, model, and schedule. Schedules support manual runs, intervals of at least one minute, daily, weekdays, and weekly times in an explicit IANA timezone. The editor previews the next three occurrences. **Run now** starts one run without changing a paused schedule.

You can also ask in a native-agent chat: “Every weekday at 9am America/Toronto, review this project's open changes and summarize risks.” The agent uses `create_automation` to save through the same service and confirms the next run. Project, model, and reasoning effort default to the chat; requests with an unclear schedule or timezone should be clarified first. Creation does not immediately execute the prompt, and retrying the same creation request does not duplicate it. Manage or pause it from the Automations sidebar.

Automations run while Threadlane is open and the computer is awake. After sleep or restart, missed occurrences are combined into one run; they are not replayed as a backlog. One automation runs at a time, including while it waits for a permission or answer. Open its chat to respond, inspect changes, or continue interactively. Existing-chat heartbeats, external ACP agents, and execution while Threadlane is closed are not supported yet.

Git projects default to a fresh worktree per run. Choosing the project checkout permits changes there. Failed worktree creation never falls back to the main checkout. **Pause** stops future scheduled dispatch; **Cancel run** stops the current run. Deleting a definition preserves chats, run history, and worktrees. Runs stop after one hour of active execution, and three consecutive failures pause the automation for review.

Definitions and run metadata live under `~/.threadlane/automations`; transcripts use the normal session storage. A file lock allows one Threadlane process to own the scheduler. Ambiguous execution after a crash is marked interrupted and requires a new explicit run rather than replaying possible side effects. Notifications appear in the app for requests and failures, with an option for every completion.

## Common commands

Type `/` in the composer to open command completion.

| Command | Description |
| --- | --- |
| `/model` | Inspect or change the active model or ACP agent. |
| `/compact` | Compact the active context while preserving session summaries. |
| `/session` | View session details, token usage, and lane statistics. |
| `/name` | Rename the current session. |
| `/tree` | Navigate branching conversation history. |
| `/fork` | Create an independent branch from the current conversation. |
| `/clone` | Clone the current session tree. |
| `/skill` | Load a discovered skill. |
| `/quit` | Exit the application. |

Discovered skills and WASI extension commands are included in command completion.

## Project layout

The workspace is organized as focused crates. Key entry points include:

| Area | Location | Responsibility |
| --- | --- | --- |
| Desktop application | [`crates/threadlane-gpui`](crates/threadlane-gpui) | GPUI application binary and window setup. |
| Workspace UI | [`crates/threadlane-ui-workspace`](crates/threadlane-ui-workspace) | Root workspace view, panels, terminals, settings, and event pumps. |
| Coding agent | [`crates/threadlane-coding-agent`](crates/threadlane-coding-agent) | Session orchestration, subagents, and ACP engine wiring. |
| Runtime | [`crates/threadlane-runtime`](crates/threadlane-runtime) | Agent state machine, reducer, and session trees. |
| Providers | [`crates/threadlane-provider`](crates/threadlane-provider) | Provider routing and streaming parsers. |
| Tools | [`crates/threadlane-tools`](crates/threadlane-tools) | Workspace file tools, search, and process execution. |
| Extensions | [`crates/threadlane-wasi`](crates/threadlane-wasi) | WASI host and extension execution. |

For repository conventions and the complete crate map, see [`AGENTS.md`](AGENTS.md).

## Development and verification

Run focused checks while developing, then use the full workspace suite before submitting broader changes:

```bash
# Desktop application
cargo check -p threadlane-gpui

# Focused tests
cargo nextest run -p threadlane-runtime
cargo nextest run -p threadlane-updater

# Full workspace test suite
cargo nextest run --workspace
```

## Packaging and releases

Releases use `cargo-packager`, GitHub Actions, and [Release Please](https://github.com/googleapis/release-please). To create a local release package:

```bash
# Install packaging tools
cargo install --locked cargo-packager --version 0.11.8
cargo install --locked --git https://github.com/project-robius/robius-packaging-commands.git

# Build bundled extensions and package the application
./scripts/build_extensions.sh
cargo build --release --bin threadlane-gpui
cargo packager --release --manifest-path crates/threadlane-gpui/Cargo.toml
```

Update artifacts are signed with Ed25519 keys through `cargo-packager-updater`.

## License

This repository does not currently include a license file.

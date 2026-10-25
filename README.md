<h1 align="center">
  <img src="assets/images/threadlane-logo.svg" width="48" align="top" style="vertical-align: top;" alt="Threadlane application icon">&nbsp;Threadlane
</h1>

<p align="center">A native desktop workspace for AI-assisted software development, built in Rust with GPUI.</p>

<p align="center">
  <a href="https://github.com/wheregmis/threadlane/actions/workflows/release.yml"><img alt="macOS release workflow" src="https://github.com/wheregmis/threadlane/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/wheregmis/threadlane/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/wheregmis/threadlane?display_name=tag&sort=semver"></a>
  <img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-d65d0e?logo=rust&logoColor=white">
  <img alt="GPUI" src="https://img.shields.io/badge/UI-GPUI-6f8cff">
  <a href="https://hotpath.rs/app/repos/wheregmis/threadlane"><img alt="hotpath-cloud" src="https://img.shields.io/endpoint?url=https://hotpath.rs/badge-cloud.json"></a>
</p>

Threadlane brings project workspaces, persistent conversation sessions, coding-agent execution, and developer tools into one native application. Its Rust workspace includes provider integrations, external ACP agents, MCP support, and sandboxed WASI extensions.

Explore the [website and live UI-Kit demo](https://wheregmis.github.io/threadlane/).

> **Platforms:** The release workflow builds Linux x86_64 and ARM64, Windows x86_64, and signed Apple Silicon macOS artifacts. An [experimental iOS client](mobile/README.md) can be built from this repository and paired with the desktop app. The embedded browser works on Linux/X11 via WebKitGTK.

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

## Editor language services

Source tabs use GPUI Kit for completion, hover, diagnostic underlines,
**Go to definition** (F12 or the editor options menu), and **Code actions**.
Language services receive the current unsaved buffer; saving is not required.
The active session must have a running Threadlane runtime in the same checkout.
The status in the editor footer shows availability and diagnostic counts; click
it, or choose **Refresh language services**, to retry after fixing an error.

Automatic editor language services require `lsp_ext` to be installed and enabled
in the daemon host's global extension scope, through Extensions settings or a
global installation. Project-only copies are not eligible, and a project-scoped
`lsp_ext` that shadows a global copy disables automatic editor services rather
than falling back to the project module. The local `scripts/build_extensions.sh`
workflow installs bundled extensions into the project checkout; developers must
also install the bundled `lsp_ext` globally to use editor language services.

Install the appropriate server on the **daemon host** and make it available on
its `PATH`: `rust-analyzer` for Rust, `typescript-language-server --stdio` for
JavaScript/JSX/TypeScript/TSX, `gopls` for Go, or `pyright-langserver --stdio` for
Python. Threadlane does not install servers automatically. Other languages retain
syntax highlighting and offline current-file word suggestions. Empty successful
LSP completions are authoritative; unavailable/failed servers fall back to words
when **Word suggestions (this file)** is enabled. Those words are not live LSP.

Local and remote editors use the same daemon/WASI bridge. Remote language
services require protocol v8; older or disconnected daemons never fall back to
client-side processes or filesystem access. Brokered process/filesystem
permissions still apply. Unsaved source and editor broker payloads are ephemeral,
not extension checkpoints or conversation events. Requests and results are
bounded to 1 MiB and guarded by document, buffer revision, session, worktree,
runtime, and connection identity; changing scope invalidates outstanding replies.

Diagnostics refresh after editing and while the active buffer is idle. Servers
without pull diagnostics must publish versioned diagnostics; pending/unsupported
diagnostics are not reported as a clean buffer. Definition targets must remain
inside the checkout and pass daemon validation. Existing dirty target tabs retain
their edits; cross-file definition locations are not imposed on dirty buffers.
Code actions apply undoable, current-buffer edits only. Commands, cross-file/resource
operations, confirmation annotations, overlapping edits, and stale versioned edits
are rejected. Completion snippets, commands, and additional edits (including
auto-import edits) are not applied.

After diagnostics settle for an unchanged buffer, idle polling backs off for up to
30 seconds while remaining active. Edits, scope changes, and manual refresh reopen
the normal cadence; pending or unsupported diagnostics continue polling every second.

## Find in files

Choose **Find in files…** in Files or the workspace command palette. Type literal,
case-sensitive, single-line text; spaces are significant. Use Up/Down and Enter,
or click a matching line, to open it in the existing editor. Escape closes the
dialog and restores focus. This does not run an agent or change the chat draft.

Search reads saved UTF-8 regular files in the active Git checkout, including the
session worktree rather than the primary checkout. Tracked files remain eligible
even when ignored; nonignored untracked files are included. Git ignore rules are
not a secret detector. `.git`, `.threadlane`, symlinks, binary/non-UTF-8 content,
and files larger than 2 MiB are excluded. Unsaved tabs retain their buffers;
saved-file line numbers may differ from unsaved content. Snippet match markers
`【…】` distinguish matches without relying on color.

Scans stop at 500 matching lines, 1 MiB of response data, 4 MiB of inventory,
64 MiB of file reads, or three seconds of work. Partial results show the exact
limit or skipped-file counts; they are not exhaustive “no matches.” Queries are
limited to 4096 UTF-8 bytes and are not saved or sent to a model. Remote search
requires a connected protocol-v6 daemon and never falls back to client disk.
Changing project, session, checkout, or daemon invalidates the dialog; reopen it
for the new scope. Refresh / Retry reruns a failed or outdated search.

## Search project conversations

Choose **Search project conversations…** in the workspace command palette; with
no attached project it stays disabled with the reason "Select a project first."
Type a literal, case-insensitive query of at least two non-whitespace
characters. The palette switches into a dedicated mode that lists one row per
matching session — title, project/branch context, and a bounded plain-text
excerpt from that session's first chronologically matching message — ordered by
session recency.

Search reads saved user and assistant message text across the attached
project's discovered sessions only: no other projects, no tool output,
reasoning, composer drafts, or pending queue text, and no model calls,
persistent index, or background indexing. Confirming a row selects that
session, waits for its transcript to load, seeds the existing **Find in
conversation** strip with the same query, and navigates to the first current
match recomputed in that destination. Escape cancels and restores focus;
leaving the Chat tab or switching sessions drops a pending handoff.

Missing, unreadable, corrupt, or oversized transcripts never count as "no
matches" — the footer reports scanned/total coverage and each skipped
category, capped scans say so, and the last row offers a retry. Scans stop at
100 conversation results, 32 MiB per session file, 256 MiB total reads, or five
seconds of work.

## Agent-owned GitHub work

**GitHub → Issue → Start Task** asks the agent to implement and verify the issue
in an isolated worktree, publish a draft PR with a closing issue reference, then
mark it ready for review after local checks. **Fix CI** and **Address reviews**
use the same follow-through policy: refresh checks and feedback after every push,
fix valid findings, reply on GitHub, and keep monitoring until the latest head is
green and mergeable or an explicit blocker needs attention. Auto-addressing also
picks up PR conversation comments, excluding self-replies and CI/status notices.

Native Git sessions expose the credential-aware `github_pr` tool for fresh status,
paginated feedback, failed Actions logs, replies, and ready-for-review transitions.
It uses Threadlane's configured GitHub credentials through the GitHub CLI; external
agents use their own available GitHub tools/authentication. Missing permissions,
required human approvals, and stalled checks must be reported as blocked, not ready.

These are agent instructions and tools, not an autonomous merge service or a
guarantee that CI will succeed. Agents must not merge without an explicit request.
Manual Git buttons, local-only tasks, read-only reviews, and editable reply drafts
retain their narrower scope. Custom system prompts remain authoritative; scheduled
tasks using the default native coding-agent prompt inherit the same policy when
their requested work involves publishing a PR.

## Background completion alerts

Settings → General → **Background completion alerts** is a device-local, opt-in
preference (Off by default). Enabling requests a clearly labeled test alert;
**Send test alert** requests another. These requests cannot confirm permission
or delivery: system notification settings, Focus/Do Not Disturb, or the desktop
notification service may block them.

While Threadlane remains open, newly completed successful interactive runs on
the local daemon can alert when all Threadlane workspace windows are inactive.
Startup/imported history, reconnect baselines, foreground results, automation
runs, remote sessions, failures/cancellations, approval/question prompts,
subagent-only completions, and terminal exits do not alert. Re-enabling does not
replay old results. Alerts do not replace the persistent **New result** marker.

Alerts say only **Session completed** / **Open Threadlane to review the result**.
No project/session names, prompts, paths, code, or response text are sent to the
OS; opaque in-memory tags resolve the exact project, session ID, and session
file. Clicking an alert selects that session through normal hydration and opens
Chat, without acknowledging its result before presentation. Open dialogs/sheets
are respected. Missing, ambiguous, or stale targets report **This session is no
longer available** instead of selecting a different session. Tags are not
restored after quitting; Threadlane must remain running.

For macOS notification validation, use `./scripts/run-gpui-macos.sh` (a packaged
`.app`, not bare `cargo run`). Check allow/deny permission, an inactive-window
completion and click-through, Focus mode, and permission revocation in System
Settings. Delivery is best effort on Windows/Linux too; those platform-specific
behaviors require separate packaged-app verification.

## Quick Start

### Prerequisites

- Rust 1.95.0 or later. The repository pins this version in [`rust-toolchain.toml`](rust-toolchain.toml); CI and release packaging use the same pin.
- The WASI target: `rustup target add wasm32-wasip1`.
- A native C toolchain, such as Xcode Command Line Tools on macOS or `build-essential` on Ubuntu.
- On Linux, the GPUI stack also needs the Wayland/X11, font, audio, and OpenSSL development packages. On Ubuntu:

  ```bash
  sudo apt-get install -y \
    build-essential pkg-config libssl-dev cmake libclang-dev \
    libfontconfig-dev libwayland-dev wayland-protocols \
    libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev \
    libxcb1-dev libxcb-render0-dev libxcb-shape0-dev \
    libxcb-xfixes0-dev libxcb-xkb-dev libxcb-randr0-dev \
    libxcb-image0-dev libxcb-icccm4-dev libxcb-keysyms1-dev \
    libxcb-util-dev libvulkan-dev libasound2-dev \
    libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev \
    libjavascriptcoregtk-4.1-dev
  ```

  The GTK/WebKitGTK packages power the embedded browser surface, which currently
  requires an X11 session (Wayland is not supported yet).

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

The extension installer builds all bundled extension packages in one Cargo invocation,
sharing dependency compilation and scheduling. It deploys their binaries, agent presets,
and prompts without removing user-installed modules or disabled markers.

On macOS, use `./scripts/run-gpui-macos.sh` rather than `cargo run -p threadlane-gpui`. Some framework calls require the application to run from an app bundle. The script creates `target/debug/Threadlane-dev.app`, preserves standard output and `RUST_LOG`, and accepts `--release` for a release build.

### Inspect token efficiency

The chat's Environment panel shows the active session's processed tokens, cache reads and writes, child usage, requests and failures, and context reductions. It loads the durable report in the background when opening a chat and refreshes after each run. While generating, it shows the last journal snapshot. Processed tokens include cache reads and are not a billed-cost estimate.

```bash
cargo run -p threadlane-gpui -- --token-efficiency /path/to/session.jsonl
```

This command prints a read-only JSON report without starting the UI or providers. It includes usage across main and child lanes, failed requests, repeated context by source, snapshot reloads, compactions, and estimated-versus-reported input tokens. Cache reads and writes are separate from uncached input. Repeated context is not automatically wasted context; compare reports from similar completed tasks. Requests without usage remain visible through the request and usage-coverage counts. Legacy run-level usage is a fallback when per-request usage is absent; partially traced runs may have incomplete accounting.

Native foreground requests keep one inline copy of repeated file reads and the three most recently used distinct file/range/digest snapshots. Older large reads become reloadable references only when the snapshot matches the current file and `manage_context` is available. The journal and continuation retain full results. This reduction currently applies to the foreground durable request boundary; child lanes retain their existing context path and are included in the report.

Checkpoints reserve space for bounded user-authored intent and the latest durable plan, with recent evidence and failure findings filling the remainder. Full instructions remain in the journal; checkpoint excerpts do not replace scoped instruction files. Delegation guidance favors narrow tasks, explicit context references, concise evidence, and continuing existing child lanes.

## Configure providers and agents

Threadlane supports the following connection methods:

- **Google Antigravity** — OAuth credentials with Cloud Code Assist endpoint discovery.
- **OpenAI/Codex** — Use the built-in PKCE device-authorization flow or configure an API key in Settings. Threadlane stores its credentials under `~/.threadlane` and can read Codex CLI credentials from `~/.codex/auth.json`.
- **Anthropic (Claude)** — Native Messages API provider, API keys only. Set `ANTHROPIC_API_KEY` in the environment before launching Threadlane; `anthropic/` models then appear in the model picker (`ANTHROPIC_BASE_URL` overrides the endpoint). Claude.ai subscription login and Claude Code credentials are not supported. Settings key storage is planned as a follow-up.
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

Definitions and run metadata live under `~/.threadlane/automations`; transcripts use the normal session storage. History keeps the newest 200 reviewed, finished runs per automation, plus every active or unreviewed run; pruning metadata never deletes chats or worktrees. A file lock allows one Threadlane process to own the scheduler. Ambiguous execution after a crash is marked interrupted and requires a new explicit run rather than replaying possible side effects. Notifications appear in the app for requests and failures, with an option for every completion.

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

Threadlane also bundles on-demand [agent workflow skills](docs/agent-workflows.md)
for planning before worker handoffs, debugging, implementation, and verification.

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

### Session-read benchmark

```bash
cargo run --release -p threadlane-benchmarks --bin session_reads
```

Compares `JsonlStore`'s indexed branch reads with the reference `SessionStore`
implementation on identical synthetic journals of 1,000, 10,000, and 50,000
entries. CSV output reports median microseconds per read over nine samples for
both a 32-entry tail and the full branch. Fixture creation and journal opening
are excluded; each case checks identical output before timing. These are
in-memory branch-read measurements, not end-to-end app startup timings.

## Packaging and releases

Prefer squash merges with Conventional Commit titles (`feat:`, `fix:`, etc.). Plain merges can make Release Please list both the implementation and its merge message. The release workflow removes only duplicates verified against the merge message and Git ancestry, from the current release's changelog and PR description. Distinct follow-up commits remain. Run `node scripts/clean-release-notes.test.cjs` when changing this cleanup.

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

Threadlane is licensed under the [MIT License](LICENSE). Third-party components
retain their respective licenses.

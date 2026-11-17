# Milestone 1 design: one lane, one node

This is the Rust mapping for Plan.md §15 "Milestone 1". Plan.md stays the source
of truth for behavior; this file fixes the crate layout, the contracts between
crates, and what was learned by testing the real tools. Section numbers written
as §4.3 refer to Plan.md; sections of this file are written as "M1 §3".

**Exit (Plan.md):** A message produces one reply. Three rapid messages produce
one reply. Killing the runner at every step boundary still yields exactly one
reply and no lost event. Invariants 1, 2, 3, 5, 6, 9, 10.

## 1. Decisions and what they rest on

Checked on 2026-11-16 against Claude Code 2.1.325 and codex-cli 0.167.0 on macOS.

| Question | Decision | Why |
|---|---|---|
| Runner to hub before iroh (milestone 4) | In-process: the hub hands out a `HubLink` implementation that calls it directly | Nothing above `HubLink` changes when iroh replaces it |
| How a backend reaches the system's tools | MCP over streamable HTTP on `127.0.0.1`, served inside the runner process, one server per run, with a random bearer token | Both CLIs take an HTTP MCP server (`claude --mcp-config`, `codex mcp add --url … --bearer-token-env-var`). A stdio server would need a helper binary that connects back to the runner over a socket anyway. Verified: Claude works against a server that answers every POST with plain `application/json` |
| Tool call identifier for at-most-once (§4.6) | `params._meta["claudecode/toolUseId"]` of the MCP `tools/call` request | Verified: Claude sends it and it equals the `tool_use` id in its event stream |
| Client to hub before iroh | Unix socket `hub.sock` in the hub's data directory, mode `0600`, newline-delimited JSON | Same computer only, which is all milestone 1 needs. `HubApi` is the abstraction; iroh replaces the socket later |
| Command-line client | A separate binary, `threadlane`, in the `threadlane-client` crate on top of the client core | §18.2: the core is exercised before any window exists |
| Who routes sessions and builds context | The hub, inside `HubLink::begin_run` | Both need hub state (§4.4, §8.5). The runner only executes |
| Local durable journal on the runner (§4.6) | Deferred to milestone 6, where Plan.md schedules it. Until then the runner appends each mirrored event to the hub directly and keeps unsent entries in memory | With an in-process link there is no network to drop |

Facts about Claude Code that the driver relies on (all observed, not assumed):

- `claude -p --output-format stream-json --verbose` prints one JSON object per line.
- `--session-id <uuid>` starts a session with a chosen id. Used again on an
  existing session it fails with `Error: Session ID <id> is already in use.` on
  stderr and a non-zero exit.
- `--resume <uuid>` keeps the same session id. For an unknown id it prints a
  `result` line with `"is_error": true` and
  `"errors": ["No conversation found with session ID: <id>"]`.
- Sessions are filed by working directory, so a resume must use the same one.
- `--tools ""` removes every built-in tool. `--strict-mcp-config` ignores the
  user's other MCP servers. `--max-turns <n>` is accepted although `--help`
  does not list it.
- MCP tools appear to the model as `mcp__<server>__<tool>`.
- The user's own hooks and plugins run in `-p` mode (a `SessionStart` hook
  changed the final text in the test). That is §5.7 "runs behave like your own
  sessions" and is left alone.
- Event shapes seen:
  - `{"type":"system","subtype":"init","session_id":…,"tools":[…],"mcp_servers":[{"name":"threadlane","status":"connected"}],…}`
  - `{"type":"assistant","message":{"content":[{"type":"text","text":…} | {"type":"tool_use","id":"toolu_…","name":"mcp__threadlane__send_message","input":{…}}]},…}`
  - `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_…","content":[{"type":"text","text":…}]}]},…}`
  - `{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794892800,…}}` (`resetsAt` is Unix seconds)
  - `{"type":"result","subtype":"success","is_error":false,"result":"<final text>","session_id":…,"num_turns":2,"total_cost_usd":…,"usage":{…},"api_error_status":null,…}`
  - other `system` subtypes (`hook_started`, `hook_response`) are ignored.
- MCP requests seen, in order: `server/discover` (answer with JSON-RPC error
  `-32601`; the client then falls back), `initialize` (protocol `2025-11-25`),
  `notifications/initialized` (no `id`; answer `202` with no body), a `GET`
  (answer `405`), `tools/list`, `tools/call`. Every request carries
  `Authorization: Bearer <token>`.

- `--append-system-prompt` has no effect on a resumed session: Claude records
  the system prompt on a conversation's first request and reuses it on every
  resume (`--system-prompt-snapshot`, on by default). Verified with a live
  test. So appended instructions (§8.5) are fixed for the life of a session,
  and a change reaches the agent only in a new session. Milestone 1 leaves
  this alone. Milestone 5 must choose: rotate the session when the appended
  instructions change, or pass `--system-prompt-snapshot off`.
- A `rate_limit_event` can carry `overageStatus: "rejected"` while its
  top-level `status` is `allowed`. Only the top-level `status` counts.

**Not verified:** how Claude reports a usage limit (§18.4 item 9). The driver
treats a `rate_limit_event` whose `status` is not `allowed`, or a `result` with
`api_error_status` 429, as a usage limit. Confirm when one is seen.

## 2. Crates

| Crate | Holds | Depends on |
|---|---|---|
| `threadlane-core` | Types and interfaces. No I/O | |
| `threadlane-store` | `HubDb`: every store on Turso | core |
| `threadlane-testkit` | `FakeClock`, `ScriptedBackend`, `CrashPlan`, fakes | core |
| `threadlane-mcp` | `McpServer`: a `ToolEndpoint` served over loopback HTTP | core |
| `threadlane-backend-claude` | `ClaudeBackend: AgentBackend` | core, mcp |
| `threadlane-transport-local` | `LocalClient: HubApi` and `serve` for the Unix socket | core |
| `threadlane-client` | `ClientCore` and the `threadlane` binary | core, transport-local |
| `threadlane-runner` | `Runner` over `HubLink` and `AgentBackend` | core |
| `threadlane-hub` | `HubCore` (handler, placement, begin-run, tool gateway, dispatcher, sweep), `HubLink` and `HubApi` implementations, the `threadlane-hub` binary with an embedded runner | core, store, runner, backend-claude, transport-local |

Rules for every crate:

- The contracts in `threadlane-core` are fixed. If one looks wrong, stop and
  report it instead of changing it.
- No component reads the system clock or sleeps on its own: time comes from
  `Clock` (`now`, `sleep`, `sleep_until`), so tests run on `FakeClock`.
  Do not use `tokio::time::sleep`/`timeout`/`interval` in non-test code that
  implements system behavior. (I/O timeouts inside the Claude driver and the
  socket transport are the exception.)
- Step boundaries call `StepHook::reach` with the constants in
  `threadlane_core::runtime::steps`. A `Killed` error means "this process just
  died": return it at once, do no cleanup, release nothing.
- Library crates return typed errors (`thiserror`); binaries use `anyhow`.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and the tests must
  be clean for the crate being worked on.

## 3. The run cycle, split between runner and hub

```text
Runner                                   Hub (through HubLink)
------                                   ---------------------
request_work()                        -> placement: grant one ready lane (§5.4)
reach runner.after_claim
pending(lease); if a user message
  is pending, clock.sleep(debounce)      (§4.8, default 2 s)
reach runner.after_debounce
loop:
  begin_run(lease)                    -> read pending        [begin_run.after_read_pending]
                                         initiative check    [begin_run.after_initiative_check]
                                         route to a session, start_run
                                                             [begin_run.after_start_run]
                                         assemble context    [begin_run.after_assemble_context]
     Idle  -> reach runner.after_recheck; break
     Run(a)-> reach runner.after_start_run
  backend.run(request)
  reach runner.after_backend_start
  for each backend event: append(lease, entry)
     tool calls arrive at the runner's ToolEndpoint
       call_tool(lease, run, id, …)   -> gate, begin_tool_call [tool_call.after_begin]
                                         execute               [tool_call.after_effect]
                                         finish_tool_call      [tool_call.after_finish]
  on Finished: reach runner.after_backend_run
     complete(lease, run, final_text) -> outcome, commit_run
     reach runner.after_commit
  otherwise: fail(lease, run, failure)-> fail_run / close session / cooldown
release(lease)
reach runner.after_release
```

The loop back to `begin_run` after each run is §4.3 step 6: it closes the gap
where a wake hint arrives while the runner is busy.

## 4. `threadlane-mcp`

```rust
pub struct McpServer { /* … */ }

impl McpServer {
    /// Binds 127.0.0.1 on a free port and serves `endpoint` until shut down or dropped.
    pub async fn start(endpoint: Arc<dyn ToolEndpoint>) -> Result<Self, McpError>;
    pub fn url(&self) -> &str;          // http://127.0.0.1:<port>/mcp
    pub fn token(&self) -> &str;        // random; required as `Authorization: Bearer <token>`
    pub async fn shutdown(self);
}

/// The name backends see the server under.
pub const SERVER_NAME: &str = "threadlane";
```

- `POST /mcp` with a JSON-RPC 2.0 body; always answer `application/json`, never SSE.
  - no `id` (a notification): `202`, empty body.
  - `initialize`: `{protocolVersion: <the client's requested version>, capabilities: {tools: {}}, serverInfo: {name: "threadlane", version}}`.
  - `ping`: `{}`.
  - `tools/list`: `{tools: [{name, description, inputSchema}]}` from `ToolEndpoint::definitions`.
  - `tools/call`: call `ToolEndpoint::call(call_id, name, arguments)` where
    `call_id` is `params._meta["claudecode/toolUseId"]` if it is a string, else
    `None`. Success: `{content: [{type: "text", text: <result as compact JSON>}], isError: false}`.
    A `ToolError`: the same shape with the error's display text and `isError: true`
    (a tool error is a result, not a JSON-RPC error, so the model can read it).
  - any other method: JSON-RPC error `-32601`.
  - a body that is not JSON-RPC: JSON-RPC error `-32700`/`-32600`.
- `GET /mcp`: `405`. `DELETE /mcp`: `200`.
- A missing or wrong bearer token: `401`, and the endpoint is not called.
- Dropping the server stops it.
- Tests drive it with raw HTTP over `tokio::net::TcpStream` (no HTTP client
  dependency) and a small in-crate `ToolEndpoint`.

## 5. `threadlane-backend-claude`

```rust
pub struct ClaudeConfig {
    pub program: PathBuf,              // default "claude"
    pub config_dir: Option<PathBuf>,   // sets CLAUDE_CONFIG_DIR for a separate login (§9.3)
}
pub struct ClaudeBackend { /* … */ }
impl ClaudeBackend { pub fn new(config: ClaudeConfig) -> Self; }
impl AgentBackend for ClaudeBackend { /* kind() == BackendKind::Claude */ }
```

**Probe.** `claude --version` (first token is the version) and
`claude auth status` (JSON; `loggedIn`). A missing program is
`Probe { installed: false, .. }`, not an error. `account_label` is `None`
(labels are yours to give; milestone 3).

**Run.** Start an `McpServer` for `request.tools` if there are any, write the
MCP config to a file with mode `0600` in a private temp location (not the
working directory), and spawn:

```text
claude -p --output-format stream-json --verbose
       --permission-mode bypassPermissions
       --session-id <id>            (NativeSession::Start)
       --resume <id>                (NativeSession::Resume)
       [--model <model>]
       [--append-system-prompt <appended_instructions>]   (when not empty)
       [--mcp-config <file>]                               (when there are tools)
       [--tools "" --strict-mcp-config]                    (remove_builtin_tools)
       [--disallowedTools <rule>…]                         (deny_rules)
       [--max-turns <n>]
```

with the working directory set to `request.working_dir`, the prompt written to
stdin and stdin then closed, stdout piped and read line by line, stderr
collected (keep the last few kilobytes), and `kill_on_drop(true)`.
`--mcp-config`, `--tools` and `--disallowedTools` take several values, so never
put a positional argument after them.

MCP config file:

```json
{"mcpServers":{"threadlane":{"type":"http","url":"<url>","headers":{"Authorization":"Bearer <token>"}}}}
```

**Events.** Map stdout lines to `BackendEvent`:

| Line | Event |
|---|---|
| `system`/`init` | `SessionStarted { native_session_id }`. If tools were given and the `threadlane` server's status is not `connected`: `Failed` |
| `assistant` | one event per content block: `Text`, or `ToolUse { call_id: id, name, input }` with the `mcp__threadlane__` prefix removed from `name` |
| `user` | `ToolResult { call_id: tool_use_id, output: content, is_error }` per `tool_result` block |
| `rate_limit_event` | remembered if `status != "allowed"`; no event |
| `result`, not an error | `Finished { final_text: result, usage }` where `usage` keeps `usage`, `total_cost_usd`, `num_turns`, `modelUsage` |
| `result`, error naming "No conversation found" | `SessionMissing` |
| `result`, error after a rate-limit rejection or with `api_error_status` 429 | `UsageLimit { resets_at }` |
| `result`, any other error | `Failed { error }` |
| unparseable line, unknown type | ignored (logged at debug) |
| stdout closed with no `result` | `Failed` with the exit status and the stderr tail |

Exactly one terminal event is produced, then `next_event` returns `None`.

**Start on an existing session.** If a `Start { id }` run exits with
"is already in use" before any `init` line, run again once with
`--resume <id>`. This is the recovery path of §4.6 when a process died after
the backend created the session.

**Stop.** Kill the child and shut the MCP server down. Dropping the run does the same.

**Ask.** Not needed until milestone 9. Return
`BackendError::Failed("ask is not implemented for the Claude backend yet")`.

**Tests.** No test may call the real `claude` by default. Unit-test the line
parser on the recorded shapes above. Test the process handling against a fake
`claude` (a shell script written into a temp directory that prints recorded
lines, and one that echoes its arguments and stdin so the command line can be
asserted). Add one live conformance test, `#[ignore]`d, that runs the real
`claude` with `--model haiku` and a `send_message` tool and asserts the event
sequence and that the tool endpoint received the call with the same id as the
`ToolUse` event.

## 6. `threadlane-transport-local`

Wire format: UTF-8, one JSON value per line. A connection carries any number of
requests; each `ClientRequest` line is answered by exactly one `ClientResponse`
line, in order. A line that does not parse is answered with
`ClientResponse::Error { error: ApiError::Invalid(..) }`.

```rust
/// Default socket file name inside a hub's data directory.
pub const SOCKET_FILE: &str = "hub.sock";

pub struct LocalClient { /* socket path */ }
impl LocalClient { pub fn new(socket: impl Into<PathBuf>) -> Self; }
impl HubApi for LocalClient { /* … */ }

/// Binds the socket (replacing a stale file), sets mode 0600, and serves
/// until `shutdown` completes. Removes the socket file on the way out.
pub async fn serve(
    socket: &Path,
    api: Arc<dyn HubApi>,
    shutdown: impl Future<Output = ()> + Send,
) -> std::io::Result<()>;
```

- The server answers with `threadlane_core::client::dispatch`. It caps a feed
  wait at 60 s. One task per connection; a slow feed wait on one connection
  must not delay another.
- `LocalClient` opens a connection per call. A connect failure or a broken
  connection is `ApiError::Unreachable`. For `feed` it waits for the response
  for the requested wait plus a margin.
- A socket path longer than the platform limit (about 100 bytes) must produce a
  clear error, not a panic.

## 7. `threadlane-client`

**Core.** `ClientCore` owns the list of paired hubs and one `HubApi` per hub,
and remembers the last feed position per hub.

```rust
pub struct ClientDirs { pub root: PathBuf }   // $THREADLANE_CLIENT_DIR, else <config dir>/threadlane
pub struct PairedHub { pub name: String, pub socket: PathBuf }

impl ClientCore {
    pub fn load(dirs: ClientDirs) -> Result<Self, ClientError>;       // reads hubs.toml and state.json
    pub fn with_hub(self, name: &str, api: Arc<dyn HubApi>) -> Self;  // for tests
    pub fn pair(&mut self, name: &str, socket: PathBuf) -> Result<(), ClientError>;
    pub fn unpair(&mut self, name: &str) -> Result<(), ClientError>;
    pub fn hubs(&self) -> &[PairedHub];
    pub async fn status(&self) -> Vec<HubStatus>;          // info per hub, or "unreachable"
    pub async fn send(&self, space: Option<&str>, project: Option<&str>, text: &str)
        -> Result<Sent, ClientError>;                       // makes the client_message_id (uuid v4)
    pub async fn poll_feed(&mut self, space: Option<&str>, wait: Duration)
        -> Result<Vec<SpaceFeedItem>, ClientError>;         // advances and saves the cursor
}
```

- `space: None` means "the only paired hub"; with several paired it is an error
  that lists them. Every item returned carries its space name (§11.3).
- `hubs.toml` and `state.json` live in the client directory; writes are atomic
  (write a temp file, rename).
- The cursor is saved after items are returned to the caller, so a crash shows
  an item twice rather than never.

**Command line.**

```text
threadlane hub add <name> <data-dir-or-socket>     pair (a directory means <dir>/hub.sock)
threadlane hub list
threadlane hub remove <name>
threadlane status                                   space, ceiling, reachable or not
threadlane projects [-s <space>]
threadlane send [-s <space>] [-p <project>] [--no-wait] [--timeout <secs>] <text>…
threadlane feed [-s <space>] [--follow] [--from-start]
```

`send` prints the acknowledgement, then (unless `--no-wait`) follows the feed
and prints what arrives, returning after the first message or the timeout
(default 120 s). Output is plain lines: `[<space>/<project>] <text>` for
messages, `[<space>] notice: <text>` for system notices. Use `clap` derive.

## 8. `threadlane-runner`

```rust
pub struct RunnerConfig {
    pub lanes_dir: PathBuf,        // working directory per lane: <lanes_dir>/<lane id>
    pub max_parallel: u32,         // lanes run at once (default 2)
    pub debounce: Duration,        // default 2 s (§4.8)
    pub heartbeat: Duration,       // default 20 s (§6.4)
    pub poll: Duration,            // default 30 s (§6.5)
}

pub struct Runner { /* … */ }
impl Runner {
    pub fn new(
        link: Arc<dyn HubLink>,
        backends: Vec<Arc<dyn AgentBackend>>,
        clock: Arc<dyn Clock>,
        hook: Arc<dyn StepHook>,
        config: RunnerConfig,
    ) -> Self;

    /// Probes the backends and sends `hello`.
    pub async fn hello(&self) -> Result<(), RunnerError>;
    /// Asks for one lane and, if granted, works it to completion on this task.
    /// Returns whether a lane was granted. For tests and simulation.
    pub async fn work_once(&self) -> Result<bool, RunnerError>;
    /// The long-running loop: hello, then claim on every nudge and every
    /// `poll`, run up to `max_parallel` lanes at once, heartbeat every
    /// `heartbeat`. Returns when `shutdown` completes, after stopping its runs.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<(), RunnerError>;
}

pub enum RunnerError { Killed(Killed), Link(LinkError), Backend(BackendError), Io(std::io::Error) }
```

Lane work follows M1 §3 exactly, with these rules:

- **Debounce** only when a pending event has kind `user_message`.
- **Working directory**: `<lanes_dir>/<lane id>`, created if missing, the same
  for every run in the lane.
- **Request**: built from the `RunAssignment` field for field. `tools` is a
  `ToolEndpoint` owned by the runner that forwards to `HubLink::call_tool` and
  exposes `assignment.tools` as its definitions.
- **Mirroring**: every backend event becomes a `JournalEntry` (`seq` from 1,
  `at` from the clock, `entry` the serialized event) appended in order. If the
  hub is `Unreachable`, keep the entries and retry with the next event; they
  must all be appended before `complete`.
- **`SessionStarted`**: if the id differs from the assignment's, call
  `HubLink::native_session`.
- **Wall time**: race the event stream against
  `clock.sleep_until(start + wall_time_limit)`. On the limit: `stop()` the
  backend, then `fail` with `LimitExceeded`.
- **Ending a run**: `Finished` → `complete`. `Failed` → `fail(Error)`.
  `UsageLimit` → `fail(UsageLimit)`. `SessionMissing` → `fail(SessionMissing)`.
  The stream ending with no terminal event → `fail(Error)`. `backend.run`
  returning an error → `fail(Error)`, or `fail(UsageLimit)` for
  `BackendError::UsageLimit`.
- **Losing the lease**: `LinkError::StaleLease` or `ToolError::LeaseLost` from
  any call, or a heartbeat that does not return the lease → stop the backend
  and abandon the lane without `fail` or `release` (they would be rejected).
- **Killed**: from the hook, from `LinkError::Killed`, or from a tool call
  (`ToolError::Killed`, which the runner's `ToolEndpoint` must record so the
  lane stops even though the backend only saw a tool error) → return
  `RunnerError::Killed` at once. Drop the backend run; do not `fail`, do not
  `release`.
- **Other link errors** inside a run (`Rejected`, `Unreachable` at `complete`):
  stop the backend, try `fail(Error)` and `release`, log, and return to the
  claim loop. The lease expiry is the safety net.
- **Heartbeats** (in `run`): every `heartbeat`, send the report with the
  current load and the leases held; replace the leases with the renewed ones.
- **Shutdown**: stop the backends of running lanes and `release` their leases
  (their events return to pending without using an attempt).

Tests use `FakeClock`, `ScriptedBackend`, `CrashPlan`, and a fake `HubLink`
written in the tests that records calls and can be told what to answer.

## 9. `threadlane-hub`

`HubCore` is `Clone` and holds `HubDb`, `Arc<dyn Clock>`, `Arc<dyn StepHook>`,
the settings below, and the in-process wake mechanism.

```rust
pub struct HubSettings {
    pub lease_ttl: Duration,              // 60 s
    pub poll: Duration,                   // 30 s: sweep and dispatcher fallback
    pub idle_window: Duration,            // 30 min (§4.4 rule 5)
    pub max_attempts: u32,                // 5 (§4.9)
    pub retry_backoff_base: Duration,     // 10 s, doubled per attempt
    pub retry_backoff_max: Duration,      // 10 min
    pub isolate_after_attempts: u32,      // 2: then retry events one at a time (§4.9)
    pub wall_time_limit: Duration,        // 10 min (§4.5)
    pub max_turns: Option<u32>,           // Some(40)
    pub recent_messages: u32,             // 5 (§8.5 item 5)
    pub usage_limit_fallback: Duration,   // 5 min when a limit gives no reset time
}

impl HubCore {
    pub async fn new(db: HubDb, clock: Arc<dyn Clock>, hook: Arc<dyn StepHook>, settings: HubSettings)
        -> Result<Self, HubError>;                 // also runs ensure_defaults
    pub fn api(&self) -> Arc<dyn HubApi>;
    pub fn link(&self, machine: MachineId) -> Arc<dyn HubLink>;
    pub async fn local_machine(&self) -> Result<MachineId, HubError>;   // the hub's own runner
    pub async fn sweep_once(&self) -> Result<Vec<LaneId>, HubError>;    // expire leases, wake runners
    pub async fn dispatch_once(&self) -> Result<u32, HubError>;         // deliver what is deliverable
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<(), HubError>;
                                                   // sweep and dispatcher loops
}
```

**Defaults (`ensure_defaults`).** A `Lead` agent exists (role `lead`, either
backend, a short role prompt), every project without a lead binding gets one
for it in mode `on_request`, and one machine row with `is_hub` exists
(endpoint id `local`, to be replaced by the transport key in milestone 4).
Idempotent.

**Wake.** One internal waker implementing `threadlane_core::runtime::Waker`.
`Runners` or `Lane(_)` pushes `HubMessage::Nudge` to every link's
`next_message`; `Dispatcher` wakes the dispatcher loop. Hints are best effort:
both loops also run every `poll`. Waiting uses the `Clock`.

**Handler (`HubApi::send_message`, §6.3).** Project by name, or General.
Unknown name → `ApiError::NotFound`. Empty text or id → `Invalid`. Insert
`NewEvent { source: "client", source_event_id: client_message_id, kind: UserMessage, payload: {"text": …}, trust: User, requested: true, lane: the project's lead lane }`,
wake `Lane(lane)`, acknowledge. A duplicate id is a success with `duplicate: true` and no wake.

**Feed (`HubApi::feed`).** `OutboxStore::delivered_after("client", after, …)`
mapped to `FeedItem { seq: delivery_seq, .. }`. If empty, wait for the next
delivery or for `wait` on the clock, then read once more.

**Placement (`request_work`, §5.4 reduced to one machine).** The machine must
exist, not be revoked or draining, and have reported at least one usable
backend (`threadlane_core::backend::usable_backends`). Take
`InboxStore::ready_lanes(now)` in order and return the first
`grant_lease` that succeeds. Reach `placement.after_select` and
`placement.after_grant`.

**`heartbeat`.** `MachineStore::report`, then `renew_lease` each lease; a
`StaleLease` drops it from the result.

**`begin_run`.** Under the lease:

1. `pending_events`. None → `Idle`.
2. Initiative (§4.3 step 3): if every pending event has `requested == false`,
   `skip_events` with a reason and return `Idle`. (Until milestone 9 every
   binding behaves as On request.)
3. Session routing (§4.4), first match wins per event: (1) `decision_ref` →
   that decision's session; (2) `session_ref` if that session is open; (3)
   `reply_to_ref` → the session of that outbox message, if open; (4) task
   lane → the lane's latest open session; (5) lead lane → the latest open
   session if `last_active_at` is within `idle_window`; (6) a new session.
   Group events by session; take the group holding the oldest event. If any
   event in it has `attempts >= isolate_after_attempts`, take only the oldest
   event of the group.
4. Backend: the first of the lead agent's `backends` (empty means all, in
   `BackendKind::ALL` order) that the machine reported usable. None →
   `LinkError::Rejected`.
5. Native session. A new session: choose a UUID v4, pass it as
   `StartRun.native_session_id`, and use `NativeSession::Start`. An existing
   session with a recorded native id, on the same machine and backend:
   `NativeSession::Resume`. An existing session on another machine or backend,
   or with no native id: close it and start a new one (§8.6).
6. `start_run`.
7. Context (§8.5; memory arrives in milestone 5). Appended instructions: the
   agent's instructions followed by the fixed lead rules, byte-identical
   between runs. Prompt: for a new session, the last `recent_messages`
   messages sent in the project; open decisions in the lane; then the events,
   each with its source, time, and trust, with `external` content delimited as
   data.
8. Return the `RunAssignment`: `tools` are the registry's definitions for a
   lead (`send_message`), `remove_builtin_tools: true` for a lead lane,
   `deny_rules` empty until milestone 3.

The fixed lead rules must say at least: the user only sees what is sent with
`send_message`; final text is recorded but never delivered; send nothing when
nothing needs saying; one message per reply.

**Tool gateway (`call_tool`, §4.6, §9.1).** Check the run is active under the
lease. Unknown tool → `ToolError::UnknownTool`. Ask the `PermissionPolicy`
(milestone 1 ships one that allows tier 0 and 1 and blocks the rest). Key: the
backend's `call_id`, or `"<tool>:<sha256 of the canonical arguments>"`.
`begin_tool_call`: `Finished(result)` → return it; `OutcomeUnknown` →
`ToolError::OutcomeUnknown`; `Perform` → execute, `finish_tool_call`, return.
A `StoreError::StaleLease` anywhere is `ToolError::LeaseLost`.

**`send_message`** (tier 1, not safe to repeat). Arguments
`{ "text": string, "urgency"?: "low" | "normal" | "high" }`. Enqueues one
`message` on channel `client` and wakes the dispatcher. Returns
`{ "outbox_id": n }`. Outbox idempotency key: `"s<session id>:<call id>"` when
the backend supplied the call id (a session resumed after a crash that repeats
the call sends nothing new), `"r<run id>:<derived key>"` when the key was
derived from the arguments.

**`complete`.** Outcome: `sent` if `OutboxStore::count_for_run > 0`, else
`deferred` if the run opened a decision still pending, else `silent`.
`commit_run` with `final_text` as the rationale.

**`fail`.**

| Failure | Effect |
|---|---|
| `Error`, `LimitExceeded` | `fail_run` counting an attempt; back off `min(base * 2^attempts, max)` where `attempts` is the highest attempt count among the run's events before this failure |
| `SessionMissing` | `close_session`, then `fail_run` without counting, no backoff |
| `UsageLimit` | `set_cooldown` when a label is given; `fail_run` without counting; back off until `resets_at` or `usage_limit_fallback` |

**Dispatcher (§10.2).** `deliverable(now)`, deliver each through the
`ChannelAdapter` named by its channel, `mark_sent`, wake feed waiters. Reach
`dispatcher.after_read`, `.after_deliver`, `.after_mark_sent`. The `client`
channel's adapter does nothing: being marked sent is what puts a message in
the feed. A crash between deliver and mark sends again (at-least-once, §10.2).

**Binary.** `threadlane-hub [--data-dir DIR] [run|check]`. `run` opens the
database under the directory lock, builds `HubCore`, serves `HubApi` on
`<data dir>/hub.sock`, starts the embedded `Runner` with `ClaudeBackend` and
`lanes_dir = <data dir>/lanes`, and runs until Ctrl-C. `hub.toml` gains the
settings above under `[timing]` and `[runs]`.

## 10. Exit tests (in `threadlane-hub/tests`)

On an in-memory `HubDb`, `FakeClock`, `ScriptedBackend`, the real `HubCore`
and the real `Runner`:

1. A message produces exactly one reply in the feed; the run is `sent`; the event is `done`.
2. Three messages sent before the debounce ends produce one run and one reply.
3. For every step in `steps::runner::LEAD`, `steps::begin_run::ALL`,
   `steps::tool_call::ALL` and `steps::dispatcher::ALL`: kill there, expire the
   lease, restart with fresh `Runner`/`HubCore` objects on the same database,
   and assert: the event is `done` exactly once, exactly one committed run
   consumed it, exactly one reply is in the feed, no lease is left.
4. A scripted run that sends nothing ends `silent` with its rationale stored.
5. A failing script retries with backoff and the event dies after
   `max_attempts`, producing one system notice.
6. A stale lease cannot append, call a tool, or complete (invariant 10).
7. A resumed session whose native session is gone starts a fresh session
   without using an attempt.

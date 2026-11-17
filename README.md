# Threadlane

Threadlane runs command-line coding agents (Claude Code for now) on your own
machines: a hub queues your messages per project, a runner works them through
the agent, and replies come back through a feed. `Plan.md` is the system
design, and `docs/milestone-1.md` maps milestone 1 (one lane, one node) onto
the crates below.

## Crates

| Crate | Holds |
|---|---|
| `threadlane-core` | Types and interfaces shared by every crate. No I/O |
| `threadlane-store` | `HubDb`: every store of a hub, on Turso |
| `threadlane-testkit` | `FakeClock`, `ScriptedBackend`, `CrashPlan`, in-memory fakes |
| `threadlane-mcp` | `McpServer`: serves a run's tools to a backend over loopback HTTP |
| `threadlane-backend-claude` | `ClaudeBackend`: drives `claude -p` |
| `threadlane-transport-local` | `LocalClient` and `serve`: the client interface over a Unix socket |
| `threadlane-transport-iroh` | `IrohClient`, `serve`, join codes: the client interface over iroh |
| `threadlane-client` | The client core and the `threadlane` command-line client |
| `threadlane-runner` | `Runner`: claims lanes from a hub and executes their runs |
| `threadlane-hub` | `HubCore` and the `threadlane-hub` binary, with an embedded runner |

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

`cargo test` runs nothing that calls a model. The milestone 1 exit tests are
in `crates/threadlane-hub/tests/exit/` and run the real hub and runner on an
in-memory database, a fake clock, and a scripted backend.

## Run a hub

```sh
cargo run -p threadlane-hub -- --data-dir ~/threadlane/work run
```

The data directory defaults to `$THREADLANE_HUB_DIR`, then
`./.threadlane-dev/hub`. It holds `hub.db`, the socket `hub.sock`, the hub's
iroh key `hub.key` (created on first start, readable only by you), and
`lanes/` (one working directory per lane). The hub serves its socket, runs
your messages through the logged-in `claude`, and stops on Ctrl-C.
`threadlane-hub --data-dir DIR check` checks the database and exits.

An optional `hub.toml` in the data directory configures it. Every table and
key is optional; unknown keys are errors. For example:

```toml
[space]
name = "Work"

[runner]
max_parallel = 2      # lanes run at once
debounce_secs = 2     # wait after a message so several get one reply

[backends.claude]
program = "claude"    # path or name on PATH
# config_dir = "/Users/me/.claude-work"   # a separate Claude login
model = "haiku"       # when the agent names no model; unset means Claude's default
```

`[log]`, `[timing]`, and `[runs]` are described in
`crates/threadlane-hub/src/config.rs`.

## Talk to it

```sh
cargo install --path crates/threadlane-client   # or: cargo run -p threadlane-client --
threadlane hub add work ~/threadlane/work        # pair by data directory or socket
threadlane status
threadlane send "What changed in the build yesterday?"
threadlane feed --follow
```

From another computer on the same network, pair over iroh instead. A new hub
prints a join code on start while no client is paired; a paired client gets
more with `threadlane pair-code`. A code works once, for ten minutes, and the
new client makes a key of its own for that hub alone:

```sh
threadlane hub join work tl1-…          # the code the hub printed
threadlane pair-code                     # a code for the next client
threadlane clients                       # the keys the hub has admitted
threadlane revoke client-2               # cut one off at once
```

Every other command works the same over either pairing; over iroh, `feed
--follow` is pushed by the hub. The `[iroh]` table of `hub.toml` takes
`enabled`, `local_discovery` (both on by default), and `bind` (for example
`"0.0.0.0:7720"`; by default a free port, which local discovery lets clients
follow). `THREADLANE_LOCAL_DISCOVERY=off` turns discovery off in the client.
No relay and no outside lookup is used, so client and hub must share a
network.

`send` waits for the reply (up to `--timeout`, 120 s by default) unless given
`--no-wait`. `-s <space>` picks a hub when several are paired and `-p
<project>` a project (General by default). The client keeps its pairings in
`$THREADLANE_CLIENT_DIR`, else `<config dir>/threadlane`.

Projects, agents, and their modes:

```sh
threadlane project add web --description "the website"   # gets the lead at once
threadlane agents
threadlane agent add Reviewer --role reviewer --instructions "Review every change."
threadlane bindings -p web
threadlane mode Lead off -p web   # off, on_request, assisted, autonomous
```

An agent set to `off` never runs in that project, and `send` to a project
whose lead is off is refused. Until milestone 9 every mode above `off` acts as
`on_request`.

## Long and live tests

The database gate of `Plan.md` §18.1 runs briefly in `cargo test`. The full
gate kills a writer thousands of times:

```sh
THREADLANE_CRASH_GATE_ITERATIONS=3000 cargo test --release -p threadlane-store \
    --test crash_gate -- --nocapture
```

The live tests run the real `claude` on your logged-in account with the
cheapest model and are ignored by default:

```sh
cargo test -p threadlane-backend-claude -- --ignored   # the driver against claude
cargo test -p threadlane-hub -- --ignored              # the hub binary end to end
```

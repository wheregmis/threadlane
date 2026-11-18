# Threadlane

Threadlane runs command-line coding agents (Claude Code and Codex) on your
own machines: a hub queues your messages per project, a runner works them through
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
| `threadlane-backend-antigravity` | Drives Google Antigravity CLI (`agy`) |
| `threadlane-backend-codex` | `CodexBackend`: drives `codex exec --json` |
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

### Hotpath benchmarks

Run either fixed workload locally with allocation and timing measurements:

```sh
HOTPATH_METRICS_SERVER_OFF=1 cargo run --locked --release -p threadlane-benchmarks --bin run_protocol --features hotpath,hotpath-alloc
HOTPATH_METRICS_SERVER_OFF=1 cargo run --locked --release -p threadlane-benchmarks --bin session_reads --features hotpath,hotpath-alloc
```

The suites measure run assignment JSON encoding and decoding, and reads from a
1,000-entry in-memory run journal. CI adds Callgrind instruction counts. Pushes
to main establish baselines, and pull requests produce reports for the Hotpath relay.

### Releases

Use Conventional Commit titles for squash merges. Release Please maintains a
release PR on main. Merging it tags the workspace and attaches the desktop app,
CLI and hub for Apple Silicon and Linux (x86_64 and aarch64), with SHA-256 checksums.

## Run a hub

```sh
cargo run -p threadlane-hub -- --data-dir ~/threadlane/work run
```

The data directory defaults to `$THREADLANE_HUB_DIR`, then
`./.threadlane-dev/hub`. It holds `hub.db`, the socket `hub.sock`, the hub's
iroh key `hub.key` (created on first start, readable only by you), and
`lanes/` (one working directory per lane). The hub serves its socket, runs
your messages through the selected CLI (`claude`, `codex`, or `agy`), and stops on Ctrl-C.
A run uses the first of its agent's backends (by default Claude, then Codex)
that is installed, logged in, and on an account not at its usage limit.
`threadlane-hub --data-dir DIR check` checks the database and exits.

Set a machine’s shared task limit in **Machines → Run slots**, or run
`threadlane at-once <machine-id> <slots>`. Agents can keep creating tasks while
all slots are occupied. The next eligible task starts when a slot opens.

An optional `hub.toml` in the data directory configures it. Every table and
key is optional; unknown keys are errors. For example:

```toml
[space]
name = "Work"

[runner]
max_parallel = 2      # shared slots on this machine across agents and projects; excess work stays queued
debounce_secs = 2     # wait after a message so several get one reply

[backends.claude]
program = "claude"    # path or name on PATH
# config_dir = "/Users/me/.claude-work"   # a separate Claude login
model = "haiku"       # when the agent names no model; unset means Claude's default

[backends.codex]
program = "codex"     # path or name on PATH
# home = "/Users/me/.codex-work"         # CODEX_HOME, a separate Codex login
# model = "..."       # unset means Codex's default

[backends.antigravity]
program = "agy"
# model = "..."       # use a slug from agy models

[timing]
session_max_age_secs = 86400   # a session older than this is closed and a fresh one started

[github]
gh = "gh"             # the gh the GitHub source and the runner run; path or name on PATH
```

`[log]`, the rest of `[timing]`, and `[runs]` are described in
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

Use `threadlane feed --follow --peek` to watch new items without moving the
saved feed position. Add `--from-start` to review earlier items as well.

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

**Upgrading an existing hub.** Earlier versions saved the default Lead as Claude-only.
To let that Lead fall back to Codex when Claude is unavailable, run
`threadlane agent edit Lead --backend claude,codex`. Use its current name if
you renamed it. Set `--backend codex` for a Codex-only Lead. Upgrades preserve
saved backend preferences because older hubs stored defaults and explicit choices identically.

To use Antigravity, sign in with `agy` on the runner machine, then select it
with `threadlane agent edit Lead --backend antigravity`. The runner needs `sh`
and `curl` on its PATH for Threadlane tool calls. Antigravity accepts text
input. Image attachments require another backend. Threadlane enforces the wall-time
limit for Antigravity runs. The CLI has no model-turn limit flag.

Starter agents (docs/agent-templates.md) come compiled into the hub: Chief of
Staff, Engineer, Fullstack Developer, Backend Developer, Frontend Developer,
Reviewer, Reporter and Architect. Adding one creates an
ordinary agent whose instructions you can edit; later changes to a template do
not touch agents already made from it. `--role`, `--instructions` and
`--backend` replace the template's values when given.

```sh
threadlane agent templates
threadlane agent add --template chief-of-staff --name Tudi
threadlane mode Tudi on_request -p web
threadlane agent add --template frontend-developer
threadlane mode "Frontend Developer" on_request -p web
```

### Agent skills

In **Agents**, open **New agent** or edit an existing profile. The Claude and Codex menus list skills on the paired hub. Antigravity does not support explicit skill bindings. Check a name to bind it, or uncheck it to remove it. An existing binding that disappears from the catalog remains removable.

```sh
threadlane agent skills --space work
threadlane agent add Reviewer --role reviewer --skill codex=review --skill claude=lint
threadlane agent edit Reviewer --skill claude=lint
threadlane agent edit Reviewer --clear-skills
```

Repeated `--skill` options replace the whole list. Omitting both edit options preserves it. `threadlane agents` shows saved bindings. Only provider and name travel with the profile. The runner resolves the chosen backend's names on each launch, including after fallback. If a name is missing, reinstall it on that runner or update the profile. Existing profiles start with an empty list and continue using ordinary provider discovery.

Codex uses the installed app server's `skills/list` and passes explicit `$name` mentions to `codex exec`. The picker offers unique, enabled, non-repository names. Repository skills remain available through Codex's usual discovery. An ambiguous name in the actual run directory fails resolution. [Codex skills](https://developers.openai.com/codex/skills/) · [Codex app server](https://developers.openai.com/codex/app-server/)

For Claude, the picker lists personal directories containing `SKILL.md` under the configured Claude home. With one binding, Threadlane invokes `/directory-name` directly and passes the task as its argument. With multiple bindings, it requests each by name and Claude decides whether to invoke its Skill tool. Its print-mode CLI expands only the first skill in a stack of leading slash commands. Project, managed, and plugin skills remain under Claude's normal discovery and precedence. [Claude Code skills](https://code.claude.com/docs/en/skills)

Explicit bindings require native tools. Runs that disable built-in tools, including lead and dreaming runs, report a launch error if their selected provider has bindings. Removing a binding affects future requests. Previously loaded instructions may persist in a resumed provider session.

An agent set to `off` never runs in that project, and `send` to a project
whose lead is off is refused. Until milestone 9 every mode above `off` acts as
`on_request`.

Routines wake a project's lead with an instruction on a cadence, as work you
asked for; follow-ups are one-time wakes an agent sets for itself:

```sh
threadlane routines -p web                               # routines and follow-ups not ended
threadlane routine add -p web --every 2h "Check the open pull requests."
threadlane routine add -p web --daily 09:00 "Summarize yesterday."
threadlane routine add -p web --weekdays mon,wed,fri --at 17:30 --zone Europe/Berlin "Plan tomorrow."
threadlane routine pause 3                               # its number, as routines lists it
threadlane routine resume 3                              # at its next occurrence
threadlane routine cancel 3                              # for good
```

`add` takes exactly one cadence: `--every` (minutes, or with a unit, such as
`90m` or `2h`), `--daily HH:MM`, or `--weekdays` with `--at HH:MM`. Times are
local to `--zone`, this computer's time zone by default. `pause` and `cancel`
work on follow-ups too.

Repositories a project's tasks work in:

```sh
threadlane repo add api git@github.com:me/api.git -p web --branch main
threadlane repos -p web
```

A remote can be registered once per space. A task names the repositories it
works in by these names; the hub's runner clones each one under
`<data dir>/workspaces` with your own `git` login and gives the task its own
worktree on the branch `threadlane/<task id>`. What a run leaves uncommitted is
committed there when it ends. Task agents are refused `git push`, `gh pr
create`, `gh pr merge`, `gh release`, and `gh repo delete`: Claude through
`--disallowedTools`, Codex through execution policy rules the runner writes to
`.codex/rules/threadlane-deny.rules` in the worktree for the length of a run.
Antigravity applies the command prefixes as session-scoped deny rules.
Tasks that work in repositories run only on a machine where `gh auth status`
succeeds; elsewhere they wait, and say why.

Publishing goes through you. A task agent asks with `publish_branch` (push its
branch) or `open_pull_request` (push it and open a draft pull request); each
call becomes a decision, shown in the feed as a card, and the task waits on
you:

```sh
threadlane decisions                    # what waits on you, in every space
threadlane answer D-0001 approve        # or: decline --note "add tests first"
```

`-s <space>` picks the space; decision ids are per space. Once approved, the
hub's runner performs exactly the call you approved, once, with your own
`git` and `gh` logins, and the agent carries on with the outcome; the branch
and pull request are recorded in the task's result. A decline reaches the
agent with your note.

Where the hub's machine has `gh` logged in, the hub reads GitHub through `gh
api`, with GET only: your notifications, and for each active task's branch
and pull request its check runs, reviews, and comments. It never writes and
never marks a notification read. An item about a task's branch or pull
request goes to that task's lane, as work you asked for; anything else about
a registered repository goes to its project's lead, recorded but starting no
run while the lead is `on_request`; items about other repositories are
dropped.

When every model account the space can use has reached its usage limit, tasks
wait as blocked instead of failing, the feed gets one notice saying when work
should resume, and one more when it does. `[timing] usage_limit_fallback_secs`
sets how long an account waits when the backend did not say.

## Desktop app

The app shows every paired space at once: Home with what needs you, what
runs and what finished; each project's task board, conversation with its
lead, and agents' modes; a task's timeline; and the machines of each space.
macOS shows its notifications only from an app bundle, so build one:

```sh
scripts/bundle-desktop.sh                  # target/Threadlane.app, ad-hoc signed
```

Start a hub and pair the app's client directory with it, as for the
command-line client, then open the app with that directory:

```sh
cargo run -p threadlane-hub -- --data-dir ~/threadlane/work run
threadlane hub add work ~/threadlane/work         # or: threadlane hub join work <code>
open --env THREADLANE_CLIENT_DIR="$HOME/.config/threadlane" target/Threadlane.app
```

Without `--env`, the app uses the same default client directory as
`threadlane`. `THREADLANE_DESKTOP_OPEN` opens a screen at start: `home`,
`agents`, `machines`, `<space>/<project>[/chat|/tasks|/memory|/setup]`, or
`<space>/T-0001`.

## Long and live tests

The database gate of `Plan.md` §18.1 runs briefly in `cargo test`. The full
gate kills a writer thousands of times:

```sh
THREADLANE_CRASH_GATE_ITERATIONS=3000 cargo test --release -p threadlane-store \
    --test crash_gate -- --nocapture
```

The live tests run the real `claude` and `codex` on your logged-in accounts
and are ignored by default:

```sh
cargo test -p threadlane-backend-claude -- --ignored   # the driver against claude
cargo test -p threadlane-backend-codex -- --ignored    # the driver against codex
cargo test -p threadlane-hub -- --ignored              # the hub binary end to end
```

The backend conformance suite (`Plan.md` §16, in
`threadlane_testkit::conformance`) checks what the system relies on from each
backend: an unattended run with full access, the event stream, the system's
tools, resume by id, each deny rule refusing its command, and the usage limit.
`cargo test` runs it against fake `claude` and `codex` scripts (they need
`curl`). Run it against the real tools whenever one of them is updated:

```sh
cargo test -p threadlane-backend-claude --test live -- --ignored real_claude_passes_the_conformance_suite
cargo test -p threadlane-backend-antigravity --test live -- --ignored
THREADLANE_CODEX_MODEL=<model> cargo test -p threadlane-backend-codex --test live -- --ignored real_codex_conforms
cargo test -p threadlane-backend-codex --test live -- --ignored the_deny_rules   # codex execpolicy check only; no model
```

A real usage limit cannot be provoked, so that row is checked only against
the fakes. `THREADLANE_CODEX_MODEL` is optional; unset uses Codex's default.

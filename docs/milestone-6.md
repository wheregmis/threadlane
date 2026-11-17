# Milestone 6 design: a second machine

Maps `Plan.md` §15 milestone 6 onto the crates. Read with `Plan.md` §3.5,
§5.3 to §5.6, §5.8, §8.6 and invariants 10, 11 and 18 of §13. It is built
in four stages, in order; each stage leaves `cargo test --workspace`,
clippy with `-D warnings` and `cargo fmt --check` passing. It starts after
milestone 5, because a resume on another machine needs the progress file.

## 1. Decisions

1. **The hub link crosses the transport as it is.** `HubLink` already is
   the whole runner-to-hub interface and the embedded runner already uses
   it. It gets request and reply enums and a `dispatch`, exactly as
   `HubApi` did in milestone 4, and a third ALPN, `threadlane/link/1`, in
   `threadlane-transport-iroh`. No call is added or reshaped for the wire
   unless a stage below needs it.
2. **The hub's own runner stays embedded** and keeps calling the hub in
   process. It is a machine like any other in the tables (`is_hub`).
3. **A machine is a membership directory.** `threadlane-runner --dir DIR
   join <code>` writes the key, the hub's id and address, and a
   `runner.toml`; `threadlane-runner --dir DIR run` serves it. Two
   memberships on one computer are two directories and two processes. No
   code path takes a directory other than its own, which is how "neither
   membership is given a path in the other's directory" holds.
4. **The optional machine-wide cap is not built.** Each membership has its
   own `max_parallel`. The plan calls the cap optional; it needs the two
   processes to see each other, which nothing else does.
5. **Bundles are files on the hub**, under `<data dir>/bundles/`, named by
   content hash, with the row in `checkpoints` pointing at one. They are
   project content at rest on the hub, which §3.6 already allows.
6. **A bundle is `git bundle create <file> <base>..<branch>`** made by the
   runner after the end-of-run commit. Restoring is: clone or fetch the
   base from the remote, `git fetch <bundle>`, create the worktree on the
   task branch. Only `git` is invoked.
7. **Machine join codes reuse the client join exchange** with a role in
   the code. An admitted key has one role, and a key admitted as a client
   is refused on the link ALPN and the reverse.
8. **Tests run two real runners against one hub in one process**, over the
   iroh transport on loopback with discovery off, scripted backends, the
   fake forge and local bare repositories. No test needs a second
   computer. The exit on two real computers is checked by hand and is not
   part of `cargo test`.
9. **No new dependencies** unless a stage cannot be done without one.
10. **Not built:** guests, `snapshot` and `policy` messages (milestones 7
    and 9), relays and reachability beyond one network (milestone 12).

## 2. Stage A: the link over the transport, joining, drain, revoke

- **Core.** `LinkRequest`, `LinkReply`, `dispatch` for `HubLink`; hub
  messages (`nudge`, `cancel`, `drain`) as a pushed stream on the same
  connection, as the feed is for clients.
- **Transport.** `threadlane/link/1`: `serve_link` on the hub side,
  `IrohLink` implementing `HubLink` on the runner side, reconnecting until
  it succeeds. A request that was sent but not answered when the
  connection dropped is sent again; every state-changing call is already
  idempotent on lease epoch and sequence, and a test proves each one is.
- **Joining.** `PairingRequest::IssueJoinCode` takes a role. `threadlane
  pair-code --machine`, `threadlane machines`, `threadlane drain <name>`,
  `threadlane revoke <name>` (the last already exists for clients).
- **The binary.** `threadlane-runner` with `join` and `run`, the
  membership directory of decision 3, `[backends.*]` and `max_parallel` in
  `runner.toml`.
- **Drain and revoke.** A draining machine is granted nothing new and
  finishes what it holds. Revoking closes its connections at once and
  releases its leases, so its lanes are free for another machine; a late
  message from it carries a stale epoch and is refused (invariant 10).

Tests: every link call round-trips over the transport; a key with an
unknown or revoked key, or with the wrong role, exchanges no application
data (invariant 18); a revoked machine is disconnected at once and its
lane runs elsewhere; the milestone 1 crash scenarios pass with the runner
on the far side of the transport.

## 3. Stage B: the local journal with ordered sync

- **Runner.** Journal entries are written to a file in the membership
  directory before they are sent, each with lease epoch and sequence
  number, and removed once the hub has acknowledged them. After a
  reconnect or a restart the runner sends what is left, in order, before
  anything new.
- **Hub.** `append` accepts the next sequence number for the lease,
  ignores one it already has, and refuses a gap or a stale epoch.

Tests: a connection dropped at each point of a run loses no entry and
duplicates none; a runner killed mid-run and restarted syncs its journal
before asking for work; entries from a lease that was taken over are
refused (invariants 10 and 11).

## 4. Stage C: placement and accounts across machines

- **Eligibility per machine** (§5.4 items 3 to 5): the machine has joined
  and is online and not draining; the project allows it
  (`project_machines`; none listed means every machine); it has one of the
  agent's backends installed and logged in on an account that is free; it
  is logged in to `gh` when the task has repositories.
- **Preference:** lanes that last ran on this machine, then lanes whose
  repositories it has cloned (`checkouts`, reported in `hello`), then
  priority, then age.
- **Patience.** A lane that last ran elsewhere is held for that machine
  for the window (`[timing] patience_secs`, default 120) while it is
  online with capacity; an offline machine holds nothing.
- **Accounts.** A machine reports the account label of each backend login
  (`runner.toml`, defaulting to the backend's name). One label is one
  account: one cooldown and one optional `max_parallel` across every
  machine. This also enforces the account `max_parallel` that milestone 3
  left out.
- **Narrowing.** `threadlane project machines <project> [names…]`.

Tests: placement tests for each rule on a fake clock; the usage-limit
scenario of milestone 3 with two machines on one label (no machine starts
another run on it until the cooldown ends; tasks go on on the other
backend) and on two labels.

## 5. Stage D: checkpoints, restore, memberships, the exit

- **Checkpoint.** After the end-of-run commit the runner makes the bundle
  of decision 6 for each repository of the task and uploads it with a new
  link call, `checkpoint`; the hub stores it (decision 5) and records the
  row. A run with no new commit uploads nothing.
- **Restore.** `Workspaces::prepare` on a machine without the task's
  worktree asks the hub for the latest checkpoint of each repository and
  applies it. A machine that has the worktree uses it.
- **Rotation.** A task resuming on a different machine closes its session
  and starts a fresh one from the progress file (the cause milestone 5
  left out).
- **Two memberships.** The exit test starts two hubs and one "computer"
  with two membership directories; it asserts that every path either
  membership opens is under its own directory and that neither hub's
  database holds the other's id, key or address.
- **The exit scenario**, sentence by sentence from the milestone's exit
  paragraph, with invariants 10, 11 and 18.

## 6. Not in this milestone

Guests, snapshots sent to machines, a hub moving to another machine,
relays, the machine-wide cap, machines in the desktop app.

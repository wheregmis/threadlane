# Milestone 10 design: the full client and outbox maturity

Maps `Plan.md` §15 milestone 10 onto the crates. Read with `Plan.md` §10,
§4.7, §4.8, §11 and §11.3. Most of the milestone's screens were built in
the UI stages alongside milestones 5 to 9; what is left is delivery,
pacing, your messages mid-run, stopping, and the controls for them. It is
built in two stages; each leaves `cargo test --workspace`, clippy with
`-D warnings` and `cargo fmt --check` passing.

## 1. Decisions

1. **The client feed is still the only channel.** `ChannelAdapter` stays
   the interface; no second channel is built. "More sources (job results)
   and channels" is left out until one is wanted.
2. **Delivery to the client feed cannot fail the way an outside channel
   can**, so retries and `delivery_failure` are built against the adapter
   interface and proven with a failing fake adapter. The real client
   channel keeps delivering by being read.
3. **Pacing is per space and is three settings**, kept in the hub's
   database and editable from the client: a rate limit (at most N
   non-urgent messages per hour, default none), quiet hours (a local time
   range in a named zone, default none), and a digest window (low-urgency
   messages that arrive within it are merged into one, default off).
   `high` urgency, decisions and system notices bypass all three.
4. **A digest is one outbox message made by the dispatcher** that lists
   the merged messages by project, in order; the originals are marked
   `suppressed` and point at the digest. Nothing is summarized by a model.
5. **Quiet hours hold, they do not drop.** A held message gets
   `not_before` set to the end of the quiet range and is delivered then,
   as a digest when several are waiting.
6. **Fold-in uses what each backend supports.** Where a backend accepts
   input during a run, your message to the running lane is given at the
   next turn boundary; where it does not, it waits for the next run (the
   default, already built). Each driver says which it is; if neither
   driver supports it in headless use, the stage builds the interface and
   the "next run" path only, and says so.
7. **Stop is an event of its own**: a stop from you for a lane interrupts
   its run within one turn (the runner stops the backend at the next
   event it reads), the run ends `stopped` without counting an attempt,
   its pending events stay, and the lane is not granted again until you
   send something or resume it. Stopping a task also keeps what it had
   written (the runner still commits the worktree).
8. **A mode changed in the app applies to that agent's next run.** This
   already holds because the gate reads the binding at each call; the
   stage adds the test that proves it, including mid-run for the next
   tool call.
9. **No new dependencies.**

## 2. Stage A: delivery, pacing, stop, fold-in (hub and runner)

- **Dispatcher.** Delivery through `ChannelAdapter` with at-least-once
  and the outbox's idempotency key; a transient failure retries with
  backoff (the timing settings that events use); a permanent failure or
  attempts exhausted marks the message `dead` and inserts one
  `delivery_failure` event in the originating lane.
- **Pacing** of decisions 3 to 5, applied by the dispatcher on the fake
  clock: rate limit, quiet hours across a daylight-saving change (reuse
  milestone 8's zone code), digests.
- **Client interface.** A `pacing` request pair in the `org` capability
  (read, set) and CLI `threadlane pacing [--rate N] [--quiet HH:MM-HH:MM
  --zone Z] [--digest MINUTES] [--off]`.
- **Stop.** `UserMessage` gains nothing; a new client request stops a
  project's lead lane or a task (`threadlane stop [-p project | --task
  T-0001]`), delivered to the runner as the existing `cancel` hub
  message; decision 7's behaviour in the runner and the store.
- **Fold-in** of decision 6: the runner offers a pending `user_message`
  for the running lane to the backend at a turn boundary through a new
  optional method on the backend's run handle; the scripted backend
  supports it so the behaviour is tested; each real driver implements it
  only if its CLI documents a way.
- **Mode change mid-run** of decision 8, as a test.

Tests: a failing fake adapter retries, then dies with exactly one
`delivery_failure`; a burst of low-urgency results arrives as one digest;
nothing non-urgent arrives in quiet hours and it arrives after, across a
daylight-saving change; a decision and a `high` message bypass; the rate
limit holds the excess and delivers it later in order; stop halts a
scripted run within one turn, uses no attempt, keeps the worktree's work,
and the lane stays stopped until a message; a kill at each new step; a
folded-in message is answered in the same run and not again in the next;
a mode lowered mid-run changes the next tool call's verdict; the exit
scenarios of the milestone that concern the hub.

## 3. Stage B: the controls in the app

- **Stop** in the conversation header while the lead runs and on a
  running task, with the run shown as stopped afterwards and "Resume" by
  sending a message.
- **Pacing** per space on the Settings screen: the three settings with
  plain statements of what each does, and a line on Home when messages
  are being held ("3 results held until 08:00").
- **Digests** drawn as one entry that expands to its messages, each
  linking to its project.
- **A dead delivery** shown on the conversation it belonged to.
- **What is left of the wireframes** after checking each screen against
  `wireframes.html` note by note; the stage lists what it found missing
  and builds it, or says why not.

Tests: state-layer tests for each of the above against the fake hub.

## 4. Not in this milestone

A second channel, job-result sources, the phone, anything on a second
machine.

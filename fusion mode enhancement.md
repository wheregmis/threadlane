# Fusion mode enhancement

**Status (2026-09-25):** The safe, deterministic Fusion path is implemented. Adaptive model selection and savings claims remain gated on measured task outcomes; they are not enabled by this document. Cognition's [Devin Fusion article](https://cognition.com/blog/devin-fusion) is a design reference, not evidence of Threadlane savings.

## Product contract

Threadlane has two modes. **Agent** runs one agent without subagent tools. **Fusion** keeps the selected main model responsible for planning, ambiguity, and final review, and delegates suitable exploration, implementation, and verification to durable child lanes. The project exposes **one Fusion model setting**. Fusion uses that model for children; there is no competing Subagent model control or hidden model pool. The main model does not downgrade onto the Fusion model after a clean run.

The main route uses versioned keyword triage. Clear mechanical work delegates. Judgment-heavy, conflicting, and unclassified prompts stay on main. The route is injected into the main directive and recorded before provider work. This is deterministic and offline; it is not a learned classifier or a guarantee that every prompt is classified correctly. For delegated work, the main asks the sidekick for focused code findings, makes the plan, delegates implementation and checks, reviews the diff, and can revive the same lane with edit requests. `subagent(wait=false)` and `hub` allow the main and sidekick to run concurrently; the runtime does not force background dispatch for every task. The sidekick reports ambiguity rather than guessing.

## Durable state and audit contract

`FusionState` version 2 persists in the existing session harness as the `fusion_state` fact. It includes the main and Fusion models, sidekick effort, classifier version, delegation and escalation counters, the consecutive failure streak, a pending structured escalation reason, and compaction generation. Restore accepts it only when its version and configured model roles match and the active model is one of those roles. A changed model or Fusion setting invalidates the saved router. Child lifecycle completions newer than the last state fact are replayed on restart, then a repaired state fact is written. Sessions previously saved with the sidekick on the main lane restore the selected main model.

Fusion audit events are append-only `FactSet` records with unique `fusion_audit:<sequence>` keys in the same JSONL journal. Schema version 1 currently records arm, route, delegation, child outcome, escalation request, compaction switch or acknowledgement, and lane revival. A route record carries decision, reason, classifier version, confidence, reason codes, abstention, active and Fusion models, compaction generation, and provider cache capabilities. Child records carry lane/run identity through the harness record, model, outcome, and any structured escalation reason. `SubagentLifecycle` links child runs to their parent run.

Join audit events to `ProviderRequestStarted` and `ProviderRequestFinished` by session, run, and lane for model, provider, request latency, and provider-reported cache token details. Use the existing `Usage` records for run token totals; summing both `Usage` and `ProviderRequestFinished.usage` would double count. Prices are not in the current model catalog, so `estimated_cost_usd` stays null. Zero cached tokens are **unknown**, not proof of a cache miss. A positive provider-reported cache-read count is evidence of a hit. Cache-key support and cached-token reporting are explicit provider capabilities; TTL is unknown for current providers and is never synthesized.

## Escalation and lane safety

Two consecutive failed child lanes still request escalation. A child question, permission request, or failed tool also emits a structured escalation reason without waiting for two lane failures. Main review owns the task immediately; compaction acknowledges the signal and clears its failure streak. The main stays on its selected model, while sidekick work stays on its own lane and model. Neither durability nor a compaction boundary proves provider cache retention.

`hub revive` keeps the same durable lane history only if its recorded model, workspace, isolation status, and compaction generation still match the current Fusion session. A stale or isolated lane is rejected with an instruction to start a new child. The lane journal does not imply a warm provider process or reusable provider cache.

## Verification

Focused Nextest coverage checks version/config rejection, conservative routing, structured escalation, router/model restoration, replay after a child completion without a state write, Fusion audit/usage replay on one child run, and stale-lane revival rejection. Run `cargo nextest run -p threadlane-orchestrator -p threadlane-coding-agent -p threadlane-protocol fusion`, `cargo check -p threadlane-gpui`, and `git diff --check` for the narrow validation path. No visual GPUI observation or provider cache TTL measurement is claimed.

## Evaluation gates and decisions

The proposed learned classifier is **not enabled**. Before adding one, label representative repository tasks by mechanical, mixed, and intent-heavy work; publish a confusion matrix, especially false-mechanical routes; test timeout/error fallback to the deterministic main-on-abstain baseline; and compare successful-task rate and rework against Agent mode. A tiny handpicked golden test set is a regression check, not that evaluation.

The proposed constrained model pool is **not implemented** because the product decision is one Fusion model setting. Revisit only if observed quality and cost data justify changing that decision. Provider-specific TTL-aware switching is also deferred until a provider exposes a reliable TTL/cache contract. Neither the journal nor cache keys alone justify a savings percentage.

Main review is directed by the Fusion prompt and child result flow, but the runtime does not certify that a human or main model inspected every diff. If review attestation becomes a product requirement, add an explicit acknowledgement tied to lane outcomes rather than treating a final answer as proof. Keep the existing permission and workspace guards for every child.

For an empirical rollout, compare matched Agent and Fusion tasks using median and tail wall time, successful-task rate, rework/rollback, intervention and escalation rate, provider-reported cache hits, and total cost **only where pricing is available**. Stratify mechanical, mixed, and judgment-heavy tasks. Report sample sizes and uncertainty; do not repeat the article's vendor percentages as Threadlane results.

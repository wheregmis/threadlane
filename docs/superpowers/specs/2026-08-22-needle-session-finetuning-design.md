# Needle Session Fine-Tuning Pipeline Design

## Summary

Threadlane will turn canonical local session history into a private Needle 2 LoRA training dataset, train a candidate model with Needle's upstream CLI, compare that candidate with the current model on untouched session-level holdout data, and require a separate explicit command to promote it.

The objective is only Threadlane's existing top-five tool shortlist recall. Needle will not generate provider tool arguments or execute tools in production. Training still uses Needle's native exact-call JSONL because that is the supported upstream LoRA interface; candidate value is determined solely by the existing contrastive-retrieval evaluation.

The first local run is expected to be a pilot. The current history has 113 eligible routing turns, below Threadlane's existing requirement of 200 eligible holdout turns, so it may prove the mechanics but cannot promote a model.

## Goals

- Export clean training labels from successful canonical session tool calls.
- Keep prompts and arguments local, git-ignored, minimally exposed, and redacted for common credentials.
- Prevent train/evaluation leakage by splitting complete session files.
- Reuse Needle's maintained LoRA trainer and `.cact` exporter instead of embedding training code in Threadlane.
- Compare current and candidate weights on the same untouched holdout data.
- Make promotion explicit, recoverable, and impossible when evidence is insufficient.

## Non-Goals

- Synthetic-data generation or cloud model/data upload.
- Training or calibrating Needle's confidence head.
- Reimplementing JAX, LoRA, quantization, or `.cact` export in Rust.
- Using Needle-generated arguments in Threadlane's provider loop.
- A desktop UI, scheduled training, online learning, or automatic promotion.
- Guaranteeing that heuristic redaction detects every possible secret.

## Chosen Approach

Threadlane owns session interpretation and Needle owns model training.

The Rust side will extend the current history-evaluation path so one canonical extractor can produce ordered successful calls for both evaluation labels and training examples. A project-aware command will obtain the same current tool catalogue already used by `needle-project-eval`, export Needle-native JSONL, and write a manifest. Thin `just` recipes will invoke the upstream `needle finetune` and `needle build` commands and then run Threadlane's evaluator in separate processes for the current and candidate weights.

This avoids two rejected alternatives:

- A standalone Python session reader would duplicate Threadlane's evolving JSONL semantics and execution-outcome rules.
- Training inside the Rust workspace would duplicate the upstream JAX pipeline without improving the routing contract.

## Architecture

```text
canonical session JSONL
        |
        v
shared successful-call extractor + current project tool catalogue
        |
        v
redaction + deterministic session-level split
        |
        +--> train.jsonl --> needle finetune --> adapter.pkl
        |                                      |
        |                                      v
        |                              needle build --> candidate.cact
        |
        +--> holdout manifest --> current/candidate aggregate evaluation
                                                   |
                                                   v
                                      explicit gated promotion
```

### Shared labeled-turn extraction

The current evaluator already identifies accepted user messages, the first assistant response, matching tool execution outcomes, and obsolete tools. Its private set-only label will become an ordered internal labeled turn containing:

- the source session identity needed for splitting;
- the user prompt text;
- the first assistant response's successful tool calls in source order;
- each call's tool name and parsed JSON arguments.

The extractor will retain only calls whose bounded canonical or legacy outcome is `Succeeded`. It will exclude failed, cancelled, declined, continuation-only, obsolete-tool, malformed, and over-five-label turns using the existing reason categories. Thought signatures, assistant prose/reasoning, images, tool results, system prompts, and unrelated records will not enter the training representation.

Evaluation will derive its expected-name set from this ordered representation, preserving the current strict per-turn recall semantics without maintaining a second extractor.

### Project tool catalogue

The exporter will reuse the project-aware capability discovery used by `needle-project-eval`. Every JSONL example receives the same complete, deterministic current `AgentToolDefinition` catalogue. Examples referring to tools absent from that catalogue are excluded as obsolete.

Threadlane will serialize the catalogue in Needle's current object form rather than inventing a provider-specific training schema. The manifest will record its SHA-256 digest so training and evaluation cannot silently use different definitions.

## Dataset Contract

Each line follows Needle's documented fine-tuning format:

```json
{
  "query": "read the project configuration",
  "tools": [{"name": "read_file", "description": "...", "parameters": {}}],
  "answers": [{"name": "read_file", "arguments": {"path": "..."}}]
}
```

`reasoning` and `system` are omitted. Threadlane does not persist a trustworthy short grounding explanation, and generating one would add an external model and leak risk. Text-only/off-topic examples are also omitted in version one because Threadlane is measuring retrieval recall rather than using Needle's generation result.

Tool-call argument strings must parse as JSON objects. A successful call with malformed or non-object arguments is excluded and counted in the manifest. Multi-call answers preserve their assistant-source order.

### Redaction

Before writing an example, a deterministic local redactor scans the query and string values nested in arguments for common high-risk forms:

- bearer tokens and common API-key prefixes;
- credential assignments whose names contain `token`, `secret`, `password`, or `api_key`;
- private-key blocks;
- other narrowly defined credential patterns covered by tests.

Each distinct match within an example receives a stable placeholder such as `<REDACTED_1>`. The same value is replaced consistently wherever it appears in the query or arguments so the example retains its grounding relationship. The redactor does not broadly remove paths, commands, or user text because those are useful routing signals and the user approved retaining them in a private local dataset.

Any redaction-processing failure aborts export. The manifest contains only counts by rule, never matched values. Documentation will state that the resulting JSONL remains sensitive local data even after redaction.

### Split

Complete session files are the indivisible split unit. Sessions are ordered by their earliest eligible entry timestamp, with a stable path tie-breaker. The newest sessions are assigned to holdout until they contain at least 20 percent of eligible examples; at least one eligible session must remain for training. Fewer than two eligible sessions is an export error.

This creates a deterministic older-history training set and newer-history holdout while preventing turns from one session appearing on both sides. Needle may still reserve its default validation fraction from `train.jsonl` to report training loss; that internal validation data is not used for Threadlane's promotion decision.

### Local artifacts

All generated state lives under the already ignored project directory:

```text
.threadlane/needle-training/
  train.jsonl
  manifest.json
  adapter.pkl
  candidate.cact
  current-eval.json
  candidate-eval.json
```

Dataset and manifest files are created with owner-only permissions. Export refuses to replace an existing dataset unless explicitly requested. Training and build stages write temporary names and rename completed artifacts, so interruption cannot create an apparently valid candidate.

The manifest records format version, aggregate counts, skip/redaction counts, ordered train and holdout session identities, catalogue hash, dataset hash, and pilot status. After training, it also records the Needle CLI version plus base-checkpoint, adapter, and candidate hashes. It contains no prompts, calls, results, or raw session records.

## Commands

### `just needle_dataset`

Discovers current project tools, reads `.threadlane/sessions`, and writes the training dataset and manifest. An explicit recipe argument permits replacement of an earlier export. The command prints aggregate counts and paths only.

### `just needle_finetune`

Checks for the upstream `needle` executable and prints the documented installation command when unavailable. It runs:

```text
needle finetune train.jsonl --epochs 10 --out <temporary-adapter>
needle build <base-checkpoint> --lora <adapter> --out <temporary-candidate>
```

The exact base-checkpoint path is resolved through Needle's supported default/download behavior rather than duplicated download logic. Successful artifacts are renamed to `adapter.pkl` and `candidate.cact`, and their hashes are recorded. Threadlane neither installs Python packages automatically nor uploads artifacts.

The initial ten epochs follow Needle's guidance for a few hundred examples. Further hyperparameter tuning is deliberately deferred until measured loss or holdout results justify it.

### `just needle_evaluate_candidate`

Runs the existing aggregate evaluator twice in separate processes, once with the current weights and once with `candidate.cact`, restricted to the manifest's untouched holdout sessions. Separate processes avoid model-singleton contamination.

Each run writes a machine-readable report and prints the existing human-readable metrics: eligible turns, top-one/top-three/top-five recall, latency percentiles, misses by tool, model hash, and catalogue hash. A comparison summary states whether the candidate satisfies promotion gates. No prompt, argument, session path, or raw record is printed.

### `just needle_promote`

Revalidates the manifest, reports, catalogue, candidate, and current model immediately before mutation. It refuses promotion unless:

1. the manifest is not marked `pilot`;
2. at least 200 holdout turns are eligible;
3. candidate top-five recall is at least 99 percent;
4. candidate top-five recall is strictly greater than the current model's result on the same holdout;
5. every recorded hash matches the current artifact.

Promotion copies the current `needle/needle2.cact` to one recoverable `.bak`, stages the candidate beside the target, and atomically renames it over the configured repository-local model. Any validation or copy failure leaves the current model untouched. Promotion never targets an arbitrary `THREADLANE_NEEDLE_WEIGHTS` path.

## Pilot Behavior

An export is marked `pilot` whenever its holdout has fewer than 200 eligible turns. Pilot data can be trained and evaluated so the complete workflow can be verified locally, but `needle_promote` always refuses it.

This separates pipeline feasibility from model evidence. Accumulating more canonical sessions and rerunning export is the only version-one path out of pilot status; synthetic augmentation does not count toward the holdout threshold.

## Failure Handling

- Unreadable session directories, invalid current tool catalogues, or fewer than two eligible sessions abort export.
- Individual malformed session files and excluded turns retain aggregate skip accounting consistent with evaluation.
- Redaction failures abort export rather than writing partially processed data.
- Missing Needle/Python/JAX support yields an actionable error and does not modify the environment.
- Interrupted training/build leaves only temporary artifacts, which are never accepted by evaluation or promotion.
- Missing, stale, or mismatched hashes block evaluation comparison and promotion.
- Candidate evaluation failure leaves the current runtime model and prior reports unchanged.
- Promotion rechecks all gates and uses a staged atomic replacement with a single backup.

## Testing

Focused Rust tests will cover:

- extraction of successful ordered calls and parsed nested arguments;
- exclusion of failed, cancelled, declined, continuation, obsolete, malformed, and over-five-label turns;
- reuse of the ordered representation by strict top-k evaluation;
- matching redaction across prompt and nested argument values without exposing matches in reports;
- deterministic chronological session splitting with no train/holdout overlap;
- dataset/manifest hashing and owner-only file creation;
- pilot, recall, evidence-count, comparison, and hash promotion gates;
- preservation of the current model when candidate staging or promotion fails.

Command tests will use a small fake `needle` executable to verify orchestration and failure propagation without downloading a checkpoint or running JAX. Existing ignored real-model evaluation remains the production-format smoke check. An actual local fine-tune is a manual acceptance step, not part of `cargo test`.

Required validation will include the narrow runtime/session tests, `cargo check -p threadlane-gpui`, and `git diff --check`. The README will document the local prerequisite, sensitive artifact location, four commands, pilot restriction, and absence of uploads.

## Rollout

1. Add shared ordered-call extraction, redaction, splitting, manifest creation, and their focused tests.
2. Add the project-aware dataset command and `just needle_dataset`.
3. Add thin upstream training/build orchestration and `just needle_finetune`.
4. Add manifest-restricted base/candidate reports and `just needle_evaluate_candidate`.
5. Add the gated recoverable `just needle_promote` command.
6. Run dataset export and base evaluation on current local history. Treat the result as a pilot and do not promote.
7. Run a manual local LoRA fine-tune only after the exporter and aggregate report have been reviewed.

## References

- [Needle 2 repository](https://github.com/cactus-compute/needle)
- [Needle fine-tuning format, commands, sizing, and limitations](https://github.com/cactus-compute/needle/blob/main/doc/finetuning.md)
- [Threadlane Needle tool-selection design](./2026-08-22-needle-tool-selection-design.md)

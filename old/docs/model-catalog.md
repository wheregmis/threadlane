# Model catalog maintenance

`resources/models.json` is the complete offline curated catalog (schema 1). To publish metadata changes, edit this file, preserve model ordering where practical, increment its numeric `revision`, run `python3 resources/validate_models.py` and the provider/daemon catalog tests, then merge the change to `main`. The app checks the fixed GitHub raw-content URL over HTTPS and caches a validated snapshot separately from user model overrides at `~/.threadlane/cache/model-catalog.json`. HTTPS/GitHub delivery is trusted but not cryptographically signed. The catalog contains inert metadata only; runtime behavior, endpoints and provider support must already exist in the app.

Curated unprefixed OpenAI models are offered when API-key routing is effective. With a ChatGPT/Codex login, the picker continues to rely on account-specific live discovery. Existing live API/Codex discovery is mixed and is not changed here; this catalog does not declare per-entry authentication capabilities. Adding models does not add adapter, context-window or reasoning support beyond existing backend capabilities. Do not infer new tool/vision flags or routing from metadata.

## Refresh and precedence

Startup loads the bundle or a validated, newer cached snapshot without waiting for the network. A background check uses a 24-hour successful-check TTL; failed checks are throttled for five minutes within the process. Settings' **Refresh models** bypasses this TTL. HTTP failures, invalid data and revision rollbacks leave the last good cache intact. A cache revision equal to the bundle must contain identical metadata. Publishing changed contents requires a revision bump.

Curated metadata takes precedence over discovery. `THREADLANE_MODELS_JSON`, global `~/.threadlane/models.json`, and project `.threadlane/models.json` override it, in that order; those files are never overwritten by refreshes. Discovery can still append unknown models and determine account-specific availability. A catalog replacement removes old curated entries, but a provider may still advertise them live. Empty effort lists are unspecified and may be filled by live discovery (otherwise legacy adapter defaults apply); explicit effort lists constrain the picker and existing adapter lookups. This migration does not remove all provider-specific name heuristics.

The updater sends no provider credentials, follows no redirects, limits downloads to 1 MiB and uses a 15-second timeout. The strict remote format rejects endpoint/header fields and unsupported providers. ACP agents remain locally configured. Updates refresh available options, not the saved selected model ID.

## Audit: October 14, 2026 (revision 2)

Reviewed all 48 existing entries; retained 47, removed retired Codex Spark, and added 11 entries (58 total). Sources below establish public availability and metadata, not successful authenticated inference. No provider credentials were used. Pricing and output limits are outside schema 1.

Codex subscription discovery now requests compatibility version `0.159.2`, the [CLI release accompanying GPT-6.1 Sol's launch](https://learn.chatgpt.com/docs/changelog), instead of `0.157.0`. The endpoint filters inventory by client compatibility; catalog metadata alone cannot add a Codex picker option. This changes compiled discovery behavior, so it requires a rebuilt/updated app and a provider refresh. Account/workspace eligibility still comes from the live response, and the `CODEX_CLIENT_VERSION` override remains supported.

### OpenAI

Model IDs below are unprefixed. Context and effort corrections come from each linked model page. `none` is the explicit API value, distinct from omitting reasoning on non-reasoning models. The existing GPT-6 Luna `high` preference is retained; its provider default is `medium`.

| Model / source | Audit result |
| --- | --- |
| [gpt-6-sol](https://developers.openai.com/api/docs/models/gpt-6-sol) | Fill 1,050,000 context and `none/low/medium/high/xhigh/max`; retain medium default. |
| [gpt-6-luna](https://developers.openai.com/api/docs/models/gpt-6-luna) | Fill 1,050,000 context and the same efforts; retain high preference. |
| [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | Fill 1,050,000 context, `low/medium/high/xhigh/max`, medium default; none/minimal unsupported. |
| [gpt-5.6-luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna) | Correct context to 1,050,000; fill `none/low/medium/high/xhigh/max`, medium default. |
| [gpt-5.6-sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol) | Same context/effort correction. |
| [gpt-5.6-terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra) | Same context/effort correction. |
| [gpt-5.4](https://developers.openai.com/api/docs/models/gpt-5.4) | Correct context to 1,050,000; fill `none/low/medium/high/xhigh`, none default. |
| [gpt-5.4-mini](https://developers.openai.com/api/docs/models/gpt-5.4-mini) | Correct 128,000 context to 400,000; same efforts/default as GPT-5.4. |
| [gpt-5.5](https://developers.openai.com/api/docs/models/gpt-5.5) | Correct context to 1,050,000; fill `none/low/medium/high/xhigh`, medium default. |
| [gpt-4o](https://developers.openai.com/api/docs/models/gpt-4o) | Retain 128,000 context and off-only effort. Deprecated, still available. |
| [gpt-4o-mini](https://developers.openai.com/api/docs/models/gpt-4o-mini) | Same retention. |
| gpt-5.3-codex-spark | Remove: [retired from Codex on September 14, 2026](https://learn.chatgpt.com/docs/models). |
| [chat-latest](https://developers.openai.com/api/docs/models/chat-latest) | Add Instant alias: 400,000 context, off-only effort; function calling supported. |

[OpenAI's deprecation schedule](https://developers.openai.com/api/docs/deprecations) retires GPT-4o and GPT-4o Mini on January 14, 2027; retain them until then. Do not add the already-deprecated GPT-5.3-Codex or specialized audio/image/restricted research models to this coding catalog.

[GPT-6 Astra](https://developers.openai.com/api/docs/models/gpt-6-astra) is available, but its tool use requires Responses. The API-key adapter still posts Chat Completions, so defer a curated Astra entry until that adapter supports it; Codex live discovery can already supply it. Existing GPT-6 Sol/Luna support Chat Completions tool calls only at `none`; GPT-6.1 Sol tool calls require Responses and already have a local steering error. Those are existing transport constraints, not capabilities supplied by a JSON change.

### Antigravity

[Antigravity's model inventory](https://www.antigravity.google/docs/models/) establishes plan-dependent availability. [Enterprise identifiers](https://antigravity.google/docs/enterprise/) establish the Claude 5.5 effort-suffixed IDs (`low/medium/high/max`, medium default). The existing resolver selects a suffixed ID only when live inventory advertises it; the picker filters unavailable entries.

| Catalog ID (`antigravity/` prefix) | Audit result |
| --- | --- |
| gemini-3.1-pro | Retain logical ID; reduce unsupported 2,000,000 cap to the [published input limit](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-pro-preview), 1,048,576. This is a conservative public-model cap, not a verified Antigravity gateway limit. |
| gemini-3.7-flash | Retain existing 1,000,000 cap; currently Enterprise-only. Gateway limit not independently verified. |
| claude-opus-4-6 | Retain existing 200,000 gateway cap; removal announced for November 2, 2026. |
| claude-sonnet-4-6 | Same retention/removal date. |
| gpt-oss-120b | Retain existing 128,000 cap; same removal date. |
| gemini-3.8-flash | Add current Flash generation; cap at the [published input limit](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash), 1,048,576. Existing tiered resolver supplies routing. |
| gemini-3.6-flash | Add missing Enterprise option; cap at the [published input limit](https://ai.google.dev/gemini-api/docs/models/gemini-3.6-flash), 1,048,576. Existing resolver supplies routing. |
| claude-opus-5-5 | Add logical entry, 1,000,000 cap and the four documented Antigravity efforts. Availability requires live suffix discovery and an eligible plan. |
| claude-sonnet-5-5 | Same addition and availability constraint. |

Gemini 4 Argon is listed by Antigravity, but compatibility with Threadlane's existing `generateContent` proxy transport was not established. Defer a curated entry until its runtime ID and request compatibility are verified. Keep actual gateway restrictions authoritative rather than copying another provider's maximums.

### OpenCode Go

Compared every existing Go ID with the [public endpoint used by Threadlane](https://opencode.ai/zen/go/v1/models) and the [current Go documentation](https://opencode.ai/v2/docs/console/go/). All 27 existing IDs are still advertised, including IDs omitted from the shorter documentation list. Retain them; omission from marketing docs is not a retirement notice.

| Existing IDs (`opencode-go/` prefix) | Result |
| --- | --- |
| grok-4.6, gpt-5.6-luna | Advertised; documented route is Responses. Existing Chat Completions adapter compatibility remains unverified. |
| glm-5.3-flash, glm-5.3, glm-5.2, glm-5.1 | Advertised. |
| kimi-k3, kimi-k2.7-code, kimi-k2.6 | Advertised. |
| longcat-2.0, mimo-v2.5, mimo-v2.5-pro | Advertised. |
| minimax-m3, minimax-m2.7 | Advertised; retain existing MiniMax M2.7 efforts. |
| muse-spark-1.3-contributor, muse-spark-1.2-contributor | Advertised; retain limited-region labels. |
| qwen3.8-max, qwen3.8-flash, qwen3.7-max, qwen3.7-plus, qwen3.6-plus | Advertised. |
| deepseek-v4.1-flash, deepseek-v4-pro, deepseek-v4-flash, deepseek-v4-flash-vision-exp | Advertised. |
| hy4-preview, hy3 | Advertised. |

Add the six newly documented Chat Completions options: `mimo-v2.6-flash`, `mimo-v2.6-pro`, `space-bunny`, `longcat-2.5-preview-free`, `step-5-preview-free`, and `cat-jam-free`. Free preview labels preserve the limited-time caveat. The gateway sources provide no token limits or effort enumerations: new entries leave those fields unspecified. Existing 128,000 caps remain legacy budgeting values, not newly verified provider maximums.

Defer new `grok-4.7` and `gpt-6-luna` (Responses), `claude-haiku-5-5` (Messages), and `gemini-4-argon-contributor` (Google format): this adapter posts Chat Completions only. Endpoint-only aliases/previews `deepseek-flash`, `minimax-m2.5`, and `omen-alpha` remain discoverable without adding redundant or undocumented curated options.

### Native Anthropic

All four current IDs and their 1,000,000 contexts match the [Claude model overview](https://platform.claude.com/docs/en/models/overview). Fill defaults and restrict the picker to the `low/medium/high` efforts that Threadlane's current adapter actually forwards. The [provider also supports xhigh/max](https://platform.claude.com/docs/en/build-with-claude/effort); enabling those requires a separate adapter change.

| Catalog ID (`anthropic/` prefix) | Default |
| --- | --- |
| claude-opus-5-5 | medium |
| claude-haiku-5-5 | medium |
| claude-fable-5-1 | high |
| claude-sonnet-5-5 | high |

No new native Claude IDs are needed for the current general-purpose lineup. Restricted and older model lines are not added merely because they appear in reference documentation.

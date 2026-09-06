# Provider-neutral launch selection

Issue #900 establishes one launch grammar and one model registry for direct
Claude, Codex, DeepSeek, Kimi, and OpenRouter launches and for the unified gateway introduced by
#898. This document owns the normalized identity and propagation contract;
[unified-gateway.md](unified-gateway.md) owns runtime request routing.

## Vocabulary

Four independent dimensions describe a launch:

| Dimension | Examples | Meaning |
|---|---|---|
| Routing mode | `direct`, `unified` | One provider owns the process, or one Claude process can route among providers |
| Provider | `claude`, `codex`, `deepseek`, `kimi`, `openrouter` | The API/model family billed for a request |
| Harness | `default`, `claude`, `codex` | The executable and interactive tool surface |
| Process launch mode | `subprocess`, `pty` | How clud hosts the selected harness process |

These are different Rust types. In particular, unified is not a
`ModelProvider`, and `RoutingMode` is not the existing `LaunchMode` used for
subprocess/PTY selection.

## Public grammar

The compatibility-preserving launch shape is:

```text
clud [--claude|--codex|--deepseek|--kimi|--openrouter|--provider NAME|--unified|--mode unified]
     [--harness default|claude|codex]
     [--model MODEL] [--effort LEVEL] [--context-window SIZE]
     [run|do|loop|fix|up|rebase|grind ...]
     [-- HARNESS_ARGS...]
```

`run` is optional and equivalent to bare `clud`. Provider flags remain
permanent provider-profile selectors. `--provider` is an additive spelling for
scripts. Newly claimed tokens belong to clud before `--` and remain literal
harness arguments after it.

Unified routing is carried through plans and repeat reconstruction and starts
the launch-scoped gateway for non-dry launches. `--dry-run` remains available
to inspect normalized intent without probing credentials, checking the Claude
Code discovery version, or starting the gateway.

## Three model identifiers

`provider_catalog.rs` is the only authority mapping among:

| Purpose | Example |
|---|---|
| Stable clud CLI/settings ID | `codex-terra` |
| Claude discovery compatibility ID | `clud-claude-codex-terra` |
| Provider wire ID | `gpt-5.6-terra` |

The catalog also owns display names, compatibility aliases, effort/context
capabilities, reviewed defaults, and harness-specific context metadata. Direct
launch parsing and gateways consume these rows instead of maintaining parallel
provider tables.

All three registered namespaces resolve through the same rows. Unknown custom
wire IDs remain reachable under an already resolved provider. The typed
selection stores that provider beside the original byte-for-byte wire ID, so
repeat reconstruction does not need to encode either value into a synthetic
model string.

Known compatibility spellings normalize immediately:

- `terra@high` -> model `codex-terra`, effort `high`;
- `gpt-5.6-terra` -> model `codex-terra`, wire ID unchanged;
- `deepseek-v4-pro[1m]` -> model `deepseek-v4-pro`, context window `1m`;
- `opus` -> model `claude-opus`, wire alias `opus`.

DeepSeek upgrades its stable API aliases in place. As of 2026-08-12,
DeepSeek's live Models & Pricing page identifies the stable alias as
`DeepSeek-V4-Pro-0813`; it continues to use `deepseek-v4-pro` (or
`deepseek-v4-pro[1m]` for Claude Code), rather than a version-suffixed API
model ID. The catalog display name and clud-owned discovery ID
(`clud-claude-deepseek-v4-pro-0813`) record the served checkpoint so stale
catalog metadata is visible in model-picker and settings UIs, while the CLI
and wire IDs stay on DeepSeek's documented stable aliases. The retired
`clud-claude-deepseek-v4-pro` discovery ID remains routable for cached or
already-selected picker rows.

An unknown future `gpt-*` wire ID remains directly reachable for backwards
compatibility. Its normalized selection records Codex as the provider while
the model and wire values remain byte-for-byte (for example,
`gpt-5.7-nova`).

### Adding a cataloged model

For an additional model of an existing provider, the only production model
mapping edit is one `CatalogModel` row. That row must declare its stable clud
ID, provider wire ID, optional Claude discovery ID, display name, legacy
aliases, effort/context capabilities and defaults, provider-default status,
and any Claude context/compaction metadata. Existing adapters then select the
appropriate namespace:

- clud settings and command lines use `cli_id`;
- the provider-native harness/API uses `wire_id`;
- a Claude gateway advertises `discovery_id` and resolves it back through the
  same row before contacting the provider.

Catalog conformance tests iterate the rows. They reject duplicate discovery
IDs or provider defaults, unresolved namespace round trips, and—in a
provider-scoped Claude picker—missing or inconsistent process-wide context
metadata. Bridge and command tests also iterate every Codex row, so adding a
fourth model cannot leave discovery, routing, or native-harness addressing on
an old three-element list.

Adding an entirely new harness transport is a separate adapter decision: it
must name which existing catalog namespace it consumes or add one explicit
namespace field to `CatalogModel`. It must not infer IDs from display names,
strip provider prefixes, or create a private model table.

## Resolution and validation

Provider inference and target resolution happen before bootstrap, credential
access, daemon dispatch, or child launch:

```text
explicit provider flag
-> provider inferred from a qualified/known model
-> saved direct-provider default
-> Claude built-in default
```

An explicit provider conflicting with the model's provider is a local error.
Unified mode does not import the saved direct-provider provider or harness; it
always uses the Claude harness, and a qualified model selects only its initial
route.

Model, effort, and context are normalized as separate fields. When a legacy
suffix and an explicit flag agree they coalesce. When they disagree the error
names both values. The model catalog rejects a known model capability mismatch
instead of silently downgrading or switching models.

## LaunchPlan and repeat ownership

`LaunchPlan` carries additive `routing_mode` and `model_selection` fields.
`model_selection` contains:

- resolved provider;
- canonical clud model ID and provider wire ID;
- independent effort and context values;
- source metadata for model, effort, and context.

Serde defaults allow a new worker to read old plans. Existing
`model_provider`, provider/harness source fields, and the legacy `codex_model`
field remain during the wire-compatibility window.

Repeat reconstruction emits the resolved routing mode, lossless provider wire
model, effort, and context rather than re-reading settings or replaying a
compound legacy spelling. A settings change therefore cannot retarget already
accepted work.

## Harness application

- Native Claude receives its normalized wire model through `--model` and an
  explicit effort through Claude Code's `--effort` session flag.
- Native Codex receives the wire model through `-m` and effort through
  the documented `model_reasoning_effort` config override.
- Codex through Claude emits a registered `clud-claude-codex-*` discovery ID
  and carries ordinary effort through Claude Code's independent `--effort`
  session flag. The bridge still accepts `wire@effort` as a compatibility
  input; only the provider wire ID reaches OpenAI.
- Direct DeepSeek keeps its reviewed no-override 1m child profile. Effort
  defaults to the catalog's `low` and travels on Claude Code's `--effort`
  session flag — an initial value, never a pinned `CLAUDE_CODE_EFFORT_LEVEL`,
  so `/effort` stays live (DD-059). Explicit model, effort, and context
  selections replace only their corresponding child-profile values.
- Direct OpenRouter uses the Claude harness with
  `openrouter-claude-sonnet` as its reviewed clud profile and
  `~anthropic/claude-sonnet-latest` as the wire ID. Because `anthropic/*` and
  `~anthropic/*` IDs do not identify a unique gateway, they never infer the
  OpenRouter provider; use `--openrouter`, `--provider openrouter`, or the
  provider-qualified clud model ID. OpenRouter's live `/v1/models` response,
  rather than clud's static catalog, owns additional picker inventory.

### Claude Code merges discovery with its built-in catalog

Enabling `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` does not replace Claude
Code's built-in model list; the harness *merges* the gateway's `/v1/models`
rows into it. Only the main turn routes through the selected gateway row.
Claude Code's side queries -- session-title generation and advisor ranking --
do not resolve against the gateway's advertised rows, so a synthetic
`clud-claude-*` ID is reported as `unrecognized_model` there (and disables the
advisor) even on a healthy session. That is upstream behaviour clud does not
own from the gateway.

Because the merge also means Claude Code can send IDs the gateway never
advertised, the Codex discovery route refuses any model it cannot resolve
instead of forwarding it, and it distinguishes the two reasons: an ID clud has
no catalog row for, versus a row clud does know that this gateway does not
serve (another provider's route, or simply not advertised here). Only an
ordinary `claude*` ID passes unresolved: the translator maps those onto the
launch-time selection when `--model` was given, falling back to the catalog
default.

### OpenRouter model-selection contract

OpenRouter is the routing gateway, not the interactive harness. Claude Code
remains the frontend and receives the resolved main-model wire ID through its
`--model` argument. Selection has three supported entry points:

1. `clud --openrouter` resolves the reviewed
   `~anthropic/claude-sonnet-latest` default.
2. `clud --openrouter --model <wire-id>` pins any explicit OpenRouter alias or
   catalog slug before launch. Unknown full IDs pass through losslessly because
   OpenRouter owns that namespace.
3. Claude Code's `/model` command offers the gateway-discovered live
   inventory after launch, alongside its own built-in rows. Clud enables
   discovery but does not implement a separate pre-launch OpenRouter picker,
   and cannot restrict the picker to the discovered set.

An explicit main-model selection replaces `ANTHROPIC_MODEL`; it does not
rewrite the provider descriptor's independent Fable, Opus, Sonnet, Haiku, or
subagent role mappings. Arbitrary non-Claude wire IDs remain syntactically
reachable but are best-effort through the Claude harness. `--dry-run` resolves
and reports all of the above without reading the OpenRouter vault, whereas a
live `/model` inventory or request requires a stored OpenRouter credential.

#### Unified mode advertises exactly one OpenRouter row

`--unified` makes clud the gateway, so OpenRouter's own live discovery is not
available to the harness. Unified therefore advertises the single reviewed row
(`clud-claude-openrouter-sonnet` -> `~anthropic/claude-sonnet-latest`), and only
when an OpenRouter credential is stored; an absent key omits the row rather than
advertising a route that cannot serve. This is deliberately *not* a mirror of
OpenRouter's changing inventory into the static catalog, and it adds no
clud-side picker: the picker is still Claude Code's `/model`. Live inventory
remains the direct `--openrouter` launch's story, exactly as above.

The point of the row is that in unified mode the model picker *is* the route
picker, so a session on a spent OpenRouter account can move to another provider
without a restart -- and can be failed over automatically. See
[provider-failover.md](provider-failover.md).

`~anthropic/*` wire IDs still never infer the OpenRouter provider, so
`non_claude_model_by_any_id` continues to exclude it.

No normalized field may contain credentials. Dry-run output exposes the
selection and its sources so routing can be audited without a paid request.

## Gateway discovery adds picker rows, it does not constrain them

Claude Code's `/model` picker belongs to the harness. Gateway discovery is one
**additive** source among several, and no gateway response can remove a row the
harness already has. Established against the Claude Code 2.1.233 binary for
zackees/clud#997, which asked whether discovery is ignored, merged, or simply
not arriving; the answer is merged:

- The picker's option list starts as Claude Code's built-in Anthropic lineup and
  is only ever appended to. Discovered rows are pushed in, labeled
  `From gateway`, when an equivalent row is not already present. The built-in
  rows are never filtered against the advertised set.
- Discovery runs only in the `firstParty` deployment mode, meaning no
  `CLAUDE_CODE_USE_*` provider variable is set. That is the same condition that
  populates the built-in lineup, so the two cannot be separated: declaring a
  different provider mode to shed the built-in rows also turns discovery off.
- The `availableModels` managed setting bounds what *discovery* may add and
  otherwise only adds `claude-*` and `anthropic.*` IDs of its own. It does not
  bound the built-in lineup, and setting it additionally makes the harness
  rewrite its alias rows (`opus[1m]`) into explicit first-party IDs
  (`claude-opus-5[1m]`).
- `additionalModelOptionsCache` and `modelAccessCache` in the user's global
  config are harness-owned caches of Anthropic's own bootstrap response,
  refreshed independently of clud. They are not extension points.

Anthropic's own [gateway protocol
reference](https://code.claude.com/docs/en/llm-gateway-protocol#model-discovery)
says the same from the other side: discovery "add[s] the returned models to the
`/model` picker", and when it fails "the picker falls back to the cached list
from the previous startup or to the built-in model list". The built-in list is
the floor, not something a gateway negotiates.

The advertised set does reach the client. After a `clud --codex --harness
claude` session, `~/.claude/cache/gateway-models.json` holds exactly the three
rows `serve_codex_catalog` serves, keyed by that launch's loopback base URL.
Because the cache is keyed by base URL and each launch binds a fresh ephemeral
port, a new session starts with no cached rows until its own refetch lands.

What the bridge does with an ID it never advertised is owned by
[Claude Code merges discovery with its built-in
catalog](#claude-code-merges-discovery-with-its-built-in-catalog) above. One
point belongs here because it is what this investigation could *not* establish:
a built-in Anthropic pick on the direct Codex route does **not** fail —
`resolve_selection` (`codex_translate.rs`) maps any `claude*` ID onto the
launch-time selection, so the turn runs on a Codex model, the substitution
[DD-038](../DESIGN_DECISIONS.md#dd-038-the-codex-picker-gets-one-honest-row-always-carrying-the-catalog)
already recorded. Since zackees/clud#1007 it is no longer *quiet*: the cross
route launches the harness with `--model <discovery-id>`, so a non-haiku
`claude*` main model can only have been chosen after launch — a `/model` pick,
or a subagent's `model: opus` / `model: sonnet` alias that the harness resolves
to its built-in id — and the bridge
(`codex_bridge::is_anthropic_main_model_pick`) prints one line per session
naming the model actually served and records an ambient `model_substituted`
event in the bridge log. The harness's own `claude-*-haiku*` side-model calls
are excluded by name. **The mechanism of the `claude-opus-5[1m]` session wedge in
zackees/clud#995 is therefore unrecorded**: that launch left nothing on disk but
`"exit_code": 1`. Making it observable is the point of zackees/clud#998 and
#999. Do not infer a cause from this document.

Clud's available remedy is to detect and report, not to constrain the picker.
Full rationale:
[DD-054](../DESIGN_DECISIONS.md#dd-054-the-model-picker-belongs-to-the-harness-and-discovery-only-adds-rows).

## Tests

Focused guardrails cover provider inference and conflicts, modifier
coalescing, Claude/Codex namespace separation, future wire-ID compatibility,
CLI ownership around `--`, unified saved-preference isolation, native harness
argv/config emission, catalog-driven discovery/routing matrices, provider
default/context uniqueness, serde defaults, and repeat reconstruction.

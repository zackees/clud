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
capabilities, and reviewed defaults. Direct launch parsing and the gateway
consume these rows instead of maintaining parallel provider tables.

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
- Codex through Claude retains the bridge-compatible `wire@effort` spelling
  while also carrying the independent normalized fields on `LaunchPlan`.
- Direct DeepSeek keeps its reviewed no-override max/1m child profile. Explicit
  model, effort, and context selections replace only their corresponding
  child-profile values.
- Direct OpenRouter uses the Claude harness with
  `openrouter-claude-sonnet` as its reviewed clud profile and
  `~anthropic/claude-sonnet-latest` as the wire ID. Because `anthropic/*` and
  `~anthropic/*` IDs do not identify a unique gateway, they never infer the
  OpenRouter provider; use `--openrouter`, `--provider openrouter`, or the
  provider-qualified clud model ID. OpenRouter's live `/v1/models` response,
  rather than clud's static catalog, owns additional picker inventory.

No normalized field may contain credentials. Dry-run output exposes the
selection and its sources so routing can be audited without a paid request.

## Tests

Focused guardrails cover provider inference and conflicts, modifier
coalescing, Claude/Codex namespace separation, future wire-ID compatibility,
CLI ownership around `--`, unified saved-preference isolation, native harness
argv/config emission, serde defaults, and repeat reconstruction.

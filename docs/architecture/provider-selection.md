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
- `deepseek-v4-flash` -> model `deepseek-flash`, wire ID `deepseek-flash[1m]`;
- `opus` -> model `claude-opus`, wire alias `opus`.

For a direct Codex launch with no explicit or saved provider selection, the
reviewed catalog default is `codex-sol` / `gpt-5.6-sol` at `low` effort
(#1254). The same normalized selection feeds both the native Codex harness and
Codex through the Claude harness. Explicit CLI values remain highest
precedence, followed by saved `providers.codex` values; unified mode does not
import this direct-provider default.

DeepSeek upgrades its API aliases in place and occasionally renames them. As
of 2026-09-14, DeepSeek's Models & Pricing page lists `deepseek-flash`
(DeepSeek-V4.1-Flash, 1M context) and `deepseek-v4-pro`
(`DeepSeek-V4-Pro-0813`). `deepseek-v4-flash` is a retired name that DeepSeek
still routes for compatibility. The `deepseek-flash` row is DeepSeek's reviewed
default, with the `deepseek-flash[1m]` wire ID in every Claude Code slot, including
haiku/subagent. DeepSeek's guide uses the auto-context `deepseek-flash` for
those two slots. clud uses the 1m model there too, because it is cheap enough
to use everywhere. The
retired `deepseek-v4-flash` spelling and `clud-claude-deepseek-v4-flash`
discovery ID remain aliases of that row, so saved profiles, failover ladders and
cached picker rows follow the rename. For Pro, the catalog display name and
clud-owned discovery ID (`clud-claude-deepseek-v4-pro-0813`) record the served
checkpoint, so stale catalog metadata is visible in model-picker and settings
UIs, while the CLI and wire IDs stay on DeepSeek's stable alias. The retired
`clud-claude-deepseek-v4-pro` discovery ID remains routable for cached or
already-selected picker rows.

### Served DeepSeek model names

DeepSeek's default and haiku/subagent model names come from the `deepseek`
section of the [server-side settings](server-settings.md) (#1192). A DeepSeek
rename therefore reaches installed builds without a release. Currently
`default_model` is `deepseek-flash`, which the catalog resolves to
`deepseek-flash[1m]`, and `subagent_model` is `deepseek-flash[1m]`. The section
may only name `deepseek-*` IDs
(`[A-Za-z0-9._-]{1,64}`, optionally suffixed `[1m]`).

For a direct DeepSeek launch, the default model is chosen in this order:

1. `--model`;
2. saved `providers.deepseek.model`;
3. the served `default_model`, reported as `model_source: server_default`;
4. the catalog's reviewed default.

The served settings are read only when neither of the first two is set.
`subagent_model` fills `ANTHROPIC_DEFAULT_HAIKU_MODEL` and
`CLAUDE_CODE_SUBAGENT_MODEL`. Unified-gateway discovery rows stay static catalog
data.

A served name that the catalog does not know still launches, but it gets no
catalog effort default or compaction window. Spell its context as `name[1m]`
in the served value. Adding a catalog row restores that metadata.

An unknown future `gpt-*` wire ID remains directly reachable for backwards
compatibility. Its normalized selection records Codex as the provider while
the model and wire values remain byte-for-byte (for example,
`gpt-5.7-nova`).

### Image support

A catalog row declares `supports_images` because the failure it guards against
is invisible: an endpoint can accept a request that carries an image and answer
`200` with the image replaced by a text placeholder, so neither the status nor
the stream reveals the loss (#1200). `false` means *verified to drop* — it is
evidence-backed from a live probe, and it is what raises the launch notice
(`image_capability_notice`, `foreground_runtime.rs`). `true` means no drop has
been observed for the row. The flag gates a warning, never a refusal: a
text-only turn is legitimate, and replayed history can carry images the user
did not just paste.

`deepseek-v4-pro` is the one row marked `false`. DeepSeek's
Anthropic-compatible endpoint replaces image blocks with a literal
`[Unsupported Image]` placeholder for that model, and the model then answers
that it cannot see the picture. `deepseek-flash[1m]`, the served default,
ingests the same request correctly.

At launch, a clud-routed Claude-harness launch warns when the model it
resolves is that row: a direct `--model deepseek-v4-pro` (including the
auto-context spelling, which is billed under the suffix-free name), a saved
`providers.deepseek.model`, and `--unified --model deepseek-v4-pro`.

Two shapes stay silent. A mid-session `/model` pick of the Pro row in an
otherwise Claude-provider unified session — no launch-time signal can see a
choice made after launch, so that gap needs a gateway-side check. And the
native DeepSeek harness (`--harness deepseek`, `dsh`), which owns its own
provider configuration and takes no `--model`; clud emits no notice there.

### Adding a cataloged model

For an additional model of an existing provider, the only production model
mapping edit is one `CatalogModel` row. That row must declare its stable clud
ID, provider wire ID, optional Claude discovery ID, display name, legacy
aliases, effort/context capabilities and defaults, provider-default status,
image support, and any Claude context/compaction metadata. Image support is a
capability like the others: see the section above. Existing adapters then
select the appropriate namespace:

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
-> DeepSeek only-authorized-provider fallback for bare live direct launches
-> Claude built-in default
```

The DeepSeek fallback runs only when no explicit provider/model/harness or
saved provider/harness overrides routing. It requires a well-formed DeepSeek
key in clud's native vault, a Claude harness route (bootstrapped if needed), and
negative read-only authentication status from both Claude Code and Codex.
An unavailable or unparseable external status is unknown, not unauthenticated,
so it cannot trigger the fallback. DeepSeek's ordinary provider preflight
still validates the key before work is accepted; dry runs never read credentials.

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
   discovery on unpinned launches but does not implement a separate
   pre-launch OpenRouter picker, and cannot restrict the picker to the
   discovered set.

A live `clud --openrouter --model <id>` also becomes OpenRouter's saved
default (`providers.openrouter.model` in `~/.clud/settings.json`), so the next
plain `clud --openrouter` resolves it with `model_source: provider_setting`
(#1304). A later explicit `--model` wins and replaces it; `--dry-run` never
writes it, and no other provider saves a model this way. Unlike the other
profiles, OpenRouter's may hold a gateway wire ID that is not in the static
catalog, because OpenRouter owns that namespace; another provider's wire ID is
still refused there.

The key follows one precedence: a key typed after the flag
(`clud --openrouter sk-or-...`) is lifted out of the harness argv, overwrites
the vault record, and is announced by its last four characters; otherwise the
vault record `clud auth login openrouter` wrote is used. `OPENROUTER_API_KEY`
is deliberately **not** a source: clud never reads it and scrubs it from the
child environment, so an ambient variable cannot silently bill a different
account than the one in the vault. The same flag-then-vault rule applies to
`--deepseek` and `--kimi`.

Every launch resolves a **model allowlist** that constrains every model the
launch can reach -- the main model, the Fable, Opus, Sonnet, Haiku, and
subagent role slots, the rows gateway discovery may advertise, and anything
a bridge will serve
([DD-077](../DESIGN_DECISIONS.md#dd-077-a-launch-time-model-pin-constrains-every-model-slot-not-just-the-main-model)).
The boundary is the launch's own model selection: `--model <id>` alone pins
exactly that id, `--allow-model` (repeatable) replaces it with an explicit
list, and a launch that names no model is pinned to the previous selection --
the wire id `--dry-run` reports as `model_selection` -- announced once as a
green `[clud] info: no --model given; pinned to previous model selection:
<id>` startup line. Under a pin each role slot receives the pinned wire id,
an ambient `CLAUDE_CODE_SUBAGENT_MODEL` wins the subagent slot only when it
is itself inside the allowlist, and gateway discovery is turned off for the
launch with a startup notice naming the boundary. With no pin and no
allowlist nothing is constrained: the descriptor's independent role mappings
apply unchanged, exactly as before #1257. An explicit allowlist outside
which the resolved selection falls fails at launch with exit code 2 naming
the allowed set. Arbitrary non-Claude wire IDs remain syntactically
reachable through an explicit pin but are best-effort through the Claude
harness. `--dry-run` resolves and reports `allowed_models` and
`pinned_from_previous_selection` alongside the selection without reading the
OpenRouter vault, whereas a live `/model` inventory or request requires a
stored OpenRouter credential.

Because unknown full IDs pass through losslessly, the harness cannot know
their context windows from its own catalog either; without guidance it clamps
auto-compact to its 200k unknown-model default. clud fills that gap from the
served [`model_contexts` section](server-settings.md#the-model_contexts-map-1258)
— OpenRouter's own datasheet, refreshed on a schedule — so an uncataloged wire
ID compacts at its real window (#1258). A reviewed catalog
`claude_max_context_tokens` still wins when one exists, and an ambient
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` is never overwritten.

#### Unified mode advertises exactly one OpenRouter row

`--unified` makes clud the gateway, so OpenRouter's own live discovery is not
available to the harness. Unified therefore advertises the single reviewed row
(`clud-claude-openrouter-sonnet` -> `~anthropic/claude-sonnet-latest`), and only
when an OpenRouter credential is stored; an absent key omits the row rather than
advertising a route that cannot serve. A launch allowlist (#1257) narrows this
further: unlike the direct route, where a pin disables discovery outright,
unified keeps discovery on because clud proxies and filters the catalog --
`/v1/models` advertises only rows the boundary admits. This is deliberately *not* a mirror of
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

### Anthropic-compatible credential preflight

Live DeepSeek, Kimi, and OpenRouter launches read their separate native-vault
records before accepting foreground or daemon-backed work. A missing key can
be entered only in an interactive foreground terminal; `--dry-run` performs
no vault access or network probe. Both command-line and terminal entry reject
keys that do not match the reviewed `sk-` shape, including leading/trailing
whitespace and invisible Unicode characters, before changing the vault. The
terminal echoes one asterisk per character and erases one on Backspace; it
never echoes the key itself.

Each live preflight performs one bounded, redirect-disabled authenticated GET
with the value read from the vault: DeepSeek's `/user/balance`, Kimi's
`/v1/models`, or OpenRouter's `/api/v1/key`. A 200 response proceeds unchanged;
401/403 stops the launch before the harness starts. The failure prints only a
sanitized provider message, a last-four mask, and the stored key's character
count plus a short SHA-256 digest. `clud auth status <provider>` uses the same
classification: `configured`, `malformed`, `rejected`, or `login required`.
A timeout, connection failure, or other inconclusive status prints one warning
and continues, so an offline machine can still launch. A malformed stored key
is detected locally and is never sent over the network.

Native-vault failure modes are the same for every provider and never fall
back to anything weaker. A locked or absent OS vault (for example, no Secret
Service on a headless Linux box) reports "the native credential vault is
unavailable; retry after unlocking it" and stops a live launch; clud never
stores the key in plaintext and never reads an ambient `MOONSHOT_API_KEY`,
`DEEPSEEK_API_KEY`, or `ANTHROPIC_*` value in its place. In `--unified`, an
unreadable record only omits that provider's rows and prints its
`clud auth login <provider>` notice. Each provider's record is separate, so
logging out of one never touches another.

### Kimi: known provider-side limitation

Kimi's Anthropic-compatible endpoint does not support Claude Code's
WebFetch tool; a WebFetch call in a `--kimi` session fails upstream. This is
a Moonshot limitation, and clud does not emulate or repair it. `/status` in
the child should report the Moonshot endpoint and `kimi-k3[1m]`; the opt-in
smoke procedure is [`bench/kimi_smoke.md`](../../bench/kimi_smoke.md).

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

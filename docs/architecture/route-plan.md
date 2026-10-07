# Route plan

`crates/clud-bin/src/route_plan.rs` owns **every routing decision a launch
makes**, once. #1852 introduced it because clud reached non-Anthropic models
through four backends that each re-derived the same decisions from
`LaunchPlan` and the provider descriptor, and the copies had already drifted.

`ResolvedRoute` is built once per launch (in
`command::build_launch_plan_for_target_at`), carried on `LaunchPlan.route`
(`#[serde(default, skip_serializing_if)]`, so a pre-#1855 daemon payload still
round-trips) and shown in `--dry-run` as `route`. Each backend is a
**renderer** of that value; none of them decides anything on its own.

## The value

| Field | What it decides |
|---|---|
| `backend` | Which renderer runs: `Native`, `Direct`, `Unified`, `CodexBridge`, `DeepSeekNative` |
| `provider` | The model family billed for the launch |
| `upstreams` | Every upstream this launch may reach, each with the base URL and a `CredentialSource` |
| `slots` | `main`/`opus`/`sonnet`/`haiku`/`fable`/`subagent`, each a `SlotModel` |
| `allowlist` | The #1257 boundary every slot, discovery row and bridge request is held to |
| `discovery` | `Off`, `On`, or `Filtered(allowlist)` |
| `context` | `CLAUDE_CODE_MAX_CONTEXT_TOKENS` and `CLAUDE_CODE_AUTO_COMPACT_WINDOW` |
| `effort` | The per-turn effort capability (`CLAUDE_CODE_MODEL_CAPABILITIES`) |
| `upstream_routing` | OpenRouter's `provider` object (`--provider-only`) |
| `timeout_ms` | The effective per-request timeout |
| `scrub` | The env keys this route owns, with the match mode for each |
| `checks` | Every launch check's applicability and verdict |

`SlotModel` carries both id namespaces on purpose: `wire_id` is what the direct
route writes verbatim, `discovery_id` is what the gateway writes, and both name
the same catalog row. `verbatim` marks the one case where the row's discovery
id must *not* be substituted — an ambient `CLAUDE_CODE_SUBAGENT_MODEL` the
allowlist admits, which is written exactly as the user spelled it.

`CredentialSource` names **where** a credential comes from, never what it is:
`Harness`, `CodexAuth`, `Vault(ModelProvider)` (the vault identifiers are read
from that provider's registry descriptor at spawn time, so there is one copy of
them) or `Env(var)` (the DeepSeek-native harness reads its key from the
environment). `route_plan::read_credential` is the single place a secret
enters a launch.

## Resolving

`resolve(plan, ambient)` is pure: the plan plus an `Ambient` (the inherited
`CLAUDE_CODE_SUBAGENT_MODEL`, the one piece of process state the #1257
precedence rule needs). `resolve_with_catalog` takes the catalog and the
per-turn-effort gate as arguments, so no test needs the daemon cache, a vault
or a child process. `route_of(plan, ambient)` returns the carried route, or
resolves one for a payload built before #1855.

## Rendering

| Renderer | Produces |
|---|---|
| `render_direct_env` | The direct child environment (`EnvOverlay`) |
| `render_unified_env` / `render_unified_config` | The gateway's child environment and its `UnifiedGatewayConfig` |
| `render_codex_bridge` / `render_codex_bridge_env` | The bridge's config and child environment |
| `render_dsh` | The DeepSeek-native harness's model, key variable and patch base URL |

`EnvOverlay::apply(&mut env)` is the one shared scrub-and-write path, so a key
a route owns is removed in exactly one place. `ScrubKey`/`ScrubMode` keep the
three real rules apart: `AnyCase` (the direct overlay's platform-independent,
security-motivated rule), `OsSemantics` (the bridge overlays' per-OS uniqueness
rule) and `Prefix` (`ANTHROPIC_CUSTOM_MODEL_OPTION*`).

## Launch checks

`evaluate(&ResolvedRoute, &Catalog)` is the one place a check's placement is
decided. Each verdict carries its `Applicability` — `Applies`,
`NotApplicable(reason)` or `Unsupported(reason)` — and, when it refuses, the
message the launch must fail with. `first_refusal` is what
`ForegroundRuntime::start_with_secret_store` refuses on, **before** the backend
branch, so a check cannot depend on which renderer was about to run.

Two checks exist today:

- **`openrouter_free_cost`** (#1833): a `:free` OpenRouter id is a cost
  promise. The offline catalog must prove the row is free; a priced or
  uncatalogued id is refused on every backend that can bill it. The network
  probe that follows a `Free` verdict stays on the direct route, where clud can
  afford to make it before spawn.
- **`provider_only`**: OpenRouter's upstream routing object. `Applies` on the
  direct route (rendered into `CLAUDE_CODE_EXTRA_BODY`) and on the unified
  gateway (injected into every forwarded OpenRouter request);
  `Unsupported` on the Codex bridge and the DeepSeek-native harness, which can
  neither receive nor inject it.

## Request-path features are gateway-side

Failover, retries, error translation and `/v1/models` filtering need clud **in
the request path**, so they live in the gateway and the bridge (see
[provider-failover.md](provider-failover.md) and
[unified-gateway.md](unified-gateway.md)). They read
`ResolvedRoute.upstreams` and `.allowlist` rather than recomputing them.

The direct route is the one launch shape where clud is not in the path: Claude
Code talks to the provider itself. That is a property worth keeping — it is the
only mode with no clud egress — and it is why a direct route reports those
features as not applicable rather than pretending to offer them. `--dry-run`
shows that verdict, which is how a user can see why `--unified` recovers from
an upstream error and `--openrouter` cannot.

## Design decisions

- [DD-165](../DESIGN_DECISIONS.md#dd-165-a-gateway-route-gets-the-same-context-window-and-per-turn-effort-as-a-direct-route)
  — the gateway writes the same context window and effort capability as the
  direct route.
- [DD-166](../DESIGN_DECISIONS.md#dd-166-an-unpinned-gateway-launch-uses-the-providers-role-mappings)
  — an unpinned gateway launch uses the provider's role mappings.
- [DD-167](../DESIGN_DECISIONS.md#dd-167-the-unified-gateway-injects---provider-only-itself)
  — the gateway injects `--provider-only` itself.

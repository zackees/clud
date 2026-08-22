# Unified gateway

`clud --unified` starts one authenticated, launch-scoped loopback gateway for a
Claude Code child. It is foreground-owned and is shut down with that child; it
is neither a sidecar nor a daemon.

Unified is an explicit routing mode, not a provider. It always uses the Claude
harness, rejects an explicit Codex harness before bootstrap, and requires
Claude Code 2.1.223 or newer. Older clients are rejected with their installed
version and the `claude update` remedy before the gateway or a paid request is
started.

## Discovery and authentication

The child receives a loopback `ANTHROPIC_BASE_URL`,
`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`, and an ephemeral
`X-Clud-Gateway-Token` merged into `ANTHROPIC_CUSTOM_HEADERS`. All loopback
routes require that header. The gateway token is never serialized into a
`LaunchPlan`, daemon payload, dry-run output, logs, or debug output.

Claude credentials remain owned by Claude Code: clud preserves its incoming
`Authorization`/`x-api-key` headers only on the native Claude route. The
Codex route constructs its own OpenAI request through the existing translator;
DeepSeek receives only the key from clud's native credential vault. Missing
optional credentials omit only their discovery rows and produce one sanitized,
actionable startup notice; native Claude remains usable.

## Routing

`GET /v1/models` returns catalog rows from `provider_catalog::MODELS` for
available Codex and DeepSeek routes. Synthetic IDs are in the reserved
`clud-claude-*` namespace. A selected synthetic ID is resolved before legacy
Codex compatibility parsing: Codex IDs are rewritten to their reviewed wire
model and translated to Responses; DeepSeek IDs are rewritten and proxied to
its Anthropic-compatible endpoint. A persisted or continued session can also
name a known provider by wire ID or CLI alias (`gpt-5.6-terra`,
`deepseek-v4-pro[1m]`); those resolve through the shared catalog to their own
provider instead of leaking to Anthropic. Unknown reserved IDs fail locally
rather than falling through to a paid provider. Ordinary Claude IDs are
proxied unchanged to Anthropic.

Each Claude session/subagent identity also owns an active route epoch. Crossing
a provider boundary clears Codex's provider-private canonical Responses items.
If that conversation later returns to Codex, the translator reseeds from the
complete Anthropic-visible transcript in the request. Opaque reasoning,
signatures, cache identifiers, and tool identifiers are never reused across
providers. Switching among Codex models can retain the current Codex epoch.
`/clear`, eviction, and gateway shutdown remove both transcript and route state.

`POST /v1/messages/count_tokens` is proxied for ordinary native Claude model
IDs. Synthetic Codex and DeepSeek routes return an explicit local 404 because
their upstream token-count contracts are not Anthropic-compatible; Claude Code
falls back to its documented local estimation. Streaming message responses
remain progressive; the proxy never buffers a complete upstream stream before
returning it.

## Session-wide effort

Claude Code resolves `/effort`, `--effort`, settings, environment, model-picker
controls, and request-specific skill/subagent overrides before sending a
Messages request. The gateway sees the final `output_config.effort` string but
no source marker. Unified mode therefore has one honest contract: the harness's
effective effort is session-wide, survives `/model` switches, and is consumed
independently on every request.

Unified child setup neither injects DeepSeek's direct-mode
`CLAUDE_CODE_EFFORT_LEVEL=max` nor deletes an ambient user value. The direct
`clud --deepseek` max profile is unchanged. `/effort auto` and a session with no
explicit setting are harness-resolved; the gateway cannot recover `auto` or
secretly restore Sol's `low`, Terra/Luna's `medium`, or direct DeepSeek's `max`
after the final request value exists.

| Route | Gateway behavior |
|---|---|
| Native Claude | Preserve the request body byte-for-byte, including all of `thinking` and `output_config`, and forward the required caller-owned Anthropic headers. |
| Codex Sol/Terra/Luna | Resolve the synthetic ID first, then use `codex_translate::effort_for`: `<model>@effort` > `output_config.effort` > stated thinking budget > catalog default. Unsupported stated values fail locally with zero upstream calls. |
| DeepSeek Pro/Flash | Rewrite only the model ID and preserve `thinking` plus the complete `output_config`; do not apply Codex validation. DeepSeek maps `low`/`medium` to effective `high`, `high` to `high`, and `xhigh`/`max` to `max`. |

The same level name is calibrated differently by each model. Diagnostics may
name the public provider/effort, but never credentials, prompts, reasoning
content, response bodies, or provider-private state.

## Acceptance matrix

| Contract | Guardrail |
|---|---|
| Native body/header fidelity and credential isolation | `unified_native_claude_preserves_effort_payload_and_required_headers_byte_for_byte` |
| Every Codex discovery model and accepted effort; suffix/budget/default precedence; local rejection | `unified_codex_models_and_efforts_reach_the_exact_responses_fields` |
| Both DeepSeek models, documented effective mapping, future provider value passthrough | `unified_deepseek_preserves_effort_for_both_models_without_codex_validation` |
| Claude -> Codex -> DeepSeek -> Claude switching, Codex reseed, child override isolation | `unified_provider_switch_reseeds_codex_and_keeps_main_and_agent_efforts_independent` |
| Every discovery ID routes to exactly its upstream with per-provider credential isolation | `unified_routes_all_five_ids_with_provider_credential_isolation` |
| Persisted wire IDs (`gpt-*`, `deepseek-*`) route to their own provider and never reach Anthropic | `unified_wire_ids_route_to_their_own_provider_not_anthropic` |
| Native token counting proxied with Claude auth; synthetic and wire-ID routes 404; unknown reserved IDs fail locally | `unified_native_count_tokens_is_proxied_with_claude_auth` |
| Ambient effort preservation and no global default injection | `unified_overlay_preserves_claude_credentials_and_enables_discovery`, `unified_overlay_does_not_inject_a_global_effort_default` |
| Missing optional credentials emit one sanitized, actionable notice | `unified_missing_provider_notices_are_sanitized_and_actionable` |
| Installed-client `--effort low|high|xhigh|max` request shape | `tests/test_real_claude_unified_effort.py` (opt in with `CLUD_REAL_CLAUDE_TESTS=1`) |

Gateway discovery requires Claude Code 2.1.223 or newer. For a release smoke,
run the opt-in fixture above, then launch `clud --unified` interactively and
verify `/model` shows the honestly labeled configured routes. Select a
synthetic row, open `/effort`, and verify the effort control (including the
`/model` slider where that client version exposes it) remains available after a
model switch. Discovery metadata does not guarantee slider presentation on
every client build; `/effort` and `--effort` remain the protocol-level controls.

The advertised DeepSeek Pro row maps to the reviewed
`deepseek-v4-pro[1m]` wire ID. Unified mode does not install direct DeepSeek's
global max-effort/1m overlay and does not suppress Claude plan mode for Codex;
Claude Code remains the per-turn policy owner.

## Credential commands

Credential management uses `clud auth login <provider>`, `clud auth status
[provider]`, and `clud auth logout <provider>`. `codex-auth` and
`deepseek-auth` remain hidden aliases for this major version and print their
exact replacement. Claude status is reported as externally managed; clud never
copies, refreshes, or deletes Claude credentials.

## Validation boundary

Focused protocol tests select every advertised synthetic ID against separate
Claude, Codex, and DeepSeek canary upstreams, assert the exact wire model and
credential boundary, exercise native token counting, reject unknown reserved
IDs before any upstream request, and switch Claude -> Codex -> DeepSeek ->
Claude in one conversation before verifying Codex is freshly seeded.

A design for surviving provider exhaustion mid-session — route health, a
configured failover ladder, and pre-commit request replay — is proposed in
[provider-failover.md](provider-failover.md) (#968, not yet implemented).

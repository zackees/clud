# Unified gateway

`clud --unified` starts one authenticated, launch-scoped loopback gateway for a
Claude Code child. It is foreground-owned and is shut down with that child; it
is neither a sidecar nor a daemon.

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
optional credentials omit only their discovery rows.

## Routing

`GET /v1/models` returns catalog rows from `provider_catalog::MODELS` for
available Codex and DeepSeek routes. Synthetic IDs are in the reserved
`clud-claude-*` namespace. A selected synthetic ID is resolved before legacy
Codex compatibility parsing: Codex IDs are rewritten to their reviewed wire
model and translated to Responses; DeepSeek IDs are rewritten and proxied to
its Anthropic-compatible endpoint. Unknown reserved IDs fail locally rather
than falling through to a paid provider. Ordinary Claude IDs are proxied
unchanged to Anthropic.

Unified mode never injects DeepSeek's direct-mode global effort overlay.
Claude Code's effective request effort is therefore preserved per request;
Codex continues to use the existing strict Responses effort mapping while
DeepSeek receives its Anthropic field unchanged.

## Credential commands

Credential management uses `clud auth login <provider>`, `clud auth status
[provider]`, and `clud auth logout <provider>`. `codex-auth` and
`deepseek-auth` remain hidden aliases for this major version and print their
exact replacement. Claude status is reported as externally managed; clud never
copies, refreshes, or deletes Claude credentials.

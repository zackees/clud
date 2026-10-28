# Standalone API gateway

`clud serve --claude` runs a foreground, loopback-only Anthropic Messages API
for independent local clients. `--openrouter` adds the OpenRouter route using
`clud auth login openrouter`; `--port` chooses a port (default `8317`, or `0`
to let the OS choose). Startup prints the base URL. Ctrl+C closes client and
connected upstream sockets; DNS resolution or connection setup may delay exit.

The current Claude credential path takes an Anthropic Console API key from
`ANTHROPIC_API_KEY`. The caller authenticates separately with a random token
of at least 32 printable non-space ASCII characters in `CLUD_SERVE_CLIENT_TOKEN`.
The server does not print either credential. Use a different, unpredictable
client token for each operator or service. For example, configure the two
variables in your shell or secret manager, then run:

```sh
clud serve --claude --openrouter
```

In another shell with `CLUD_SERVE_CLIENT_TOKEN` set to the same value:

```sh
curl -H "x-api-key: $CLUD_SERVE_CLIENT_TOKEN" http://127.0.0.1:8317/v1/models
curl http://127.0.0.1:8317/v1/messages \
  -H "x-api-key: $CLUD_SERVE_CLIENT_TOKEN" \
  -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' \
  -d '{"model":"CLAUDE_MODEL_ID","max_tokens":128,"messages":[{"role":"user","content":"Hello"}]}'
```

Choose a Claude model ID returned by `/v1/models`; the same list includes the
configured OpenRouter discovery IDs. Messages requests accept streaming and
tools through the Anthropic protocol. Client `x-api-key` or bearer credentials
are removed before forwarding; the gateway separately resolves the selected
provider's upstream credential. Missing OpenRouter credentials make startup
fail with a remedy. The server does not provide OpenAI-compatible endpoints.

Claude's model inventory is fetched using its upstream credential; all pages
are collected before OpenRouter rows are appended. If discovery fails, the
server returns a sanitized gateway error instead of advertising an incomplete
list. The existing launch-scoped gateway remains documented in
[unified-gateway.md](unified-gateway.md).

## Codex

`clud serve --codex` serves Codex through the same Anthropic Messages API,
including streaming and tool calls. It requires no Claude credential and
launches no interactive harness. Authenticate the upstream first with
`clud auth login codex --acknowledge-experimental` (clud-managed ChatGPT
subscription), or set `OPENAI_API_KEY` for metered OpenAI API access. An
existing clud subscription record takes precedence; its refresh and failure
handling are the same as the interactive Codex bridge. A native `codex login`
is not automatically imported by this unattended server.

Set a separate random `CLUD_SERVE_CLIENT_TOKEN` as above, then run:

```sh
clud serve --codex
# Optional additional routes:
clud serve --codex --openrouter
clud serve --codex --claude
```

Use `/v1/models` to discover the advertised Codex IDs (for example,
`clud-claude-codex-sol`) and pass one as `model` to `/v1/messages`.
Codex-only discovery does not contact Anthropic. This is an Anthropic-compatible
frontend; `/v1/responses` and `/v1/chat/completions` are not exposed.

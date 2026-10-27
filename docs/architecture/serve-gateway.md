# clud serve — the standalone API gateway

`clud serve` starts a durable, independently-runnable Anthropic Messages API
gateway (issue #2218). It is the standalone sibling of the launch-scoped,
child-owned `--unified` gateway described in
[unified-gateway.md](unified-gateway.md): the two share one route table and one
request path, but `clud serve` is not launched for a harness child and does not
exit with one.

```
clud serve --openrouter
clud serve --claude --openrouter        # --claude not yet available; see below
```

It does not launch an interactive harness and does not touch the clud daemon.

## What it serves

The same Anthropic-compatible surface as the unified gateway:

- `GET /v1/models` — the catalog rows for the routes this server holds.
- `POST /v1/messages` — Anthropic Messages, streaming and non-streaming,
  including tool use.

Routes are the vault-backed Anthropic-compatible providers clud already
supports. A provider is served only when its credential is present in clud's
own store (the OS vault, then the owner-only credential file, then the
provider's API-key environment variable); a requested provider with no
credential fails the start with an actionable message instead of silently
dropping the route.

| Flag | Route |
|---|---|
| `--deepseek` | DeepSeek (`api.deepseek.com/anthropic`) |
| `--kimi` | Kimi / Moonshot (`api.moonshot.ai/anthropic`) |
| `--openrouter` | OpenRouter (`openrouter.ai/api`) |
| `--claude` | Native Claude — **not yet available** (see below) |

## Endpoint and authentication

The listener binds a loopback address and prints its contract at startup:

```
[clud] serve: endpoint=http://127.0.0.1:PORT token=TOKEN providers=openrouter
[clud] serve: listening on http://127.0.0.1:PORT — send the `X-Clud-Gateway-Token` header to authenticate; press Ctrl+C to stop
```

Every request must carry `X-Clud-Gateway-Token: <TOKEN>`. The token is a fresh
256-bit client credential generated per process; it is the *client* credential,
never an upstream provider credential. A request without it is answered `401`.

```sh
# list the served models
curl -s http://127.0.0.1:PORT/v1/models \
  -H "X-Clud-Gateway-Token: $TOKEN"

# a Messages request
curl -s http://127.0.0.1:PORT/v1/messages \
  -H "X-Clud-Gateway-Token: $TOKEN" \
  -H "content-type: application/json" \
  -d '{"model":"clud-claude-openrouter-sonnet",
       "max_tokens":64,"messages":[{"role":"user","content":"hello"}]}'
```

`--bind` defaults to `127.0.0.1` and is refused for any non-loopback address;
`--port` defaults to `0` (an ephemeral port, printed at startup). Ctrl+C shuts
the server down cleanly and exits `0`.

## Deliberately out of this first slice

- **Native Claude (`--claude`).** clud does not own a Claude credential — it is
  externally managed by Claude Code (`auth.rs`, `unified-gateway.md`). Serving
  Claude to an arbitrary API client needs a decision on which credential flow
  `clud serve` should own; that is the open question on #2218. The flag parses
  and refuses with a pointer to the issue rather than guessing.
- **Remote (non-loopback) binding** and its access story. A long-lived listener
  has a different trust boundary from the ephemeral child-only bridge; the
  first slice stays on loopback.
- **Multi-account balancing**, and any control plane.

## Relationship to the launch-scoped gateway

The launch-scoped bridge is intentionally not a daemon (DD-027): it binds an
ephemeral loopback port with a per-launch bearer and dies with its Claude Code
child. `clud serve` is a *new, separate* surface: it reuses the route table and
the request path (`BridgeHandle::start_serving`, an additive sibling of
`BridgeHandle::start`) but owns its own listener and client credential. The
launch-scoped contract is unchanged.

# Session `gh` read broker (#1743, phase 1)

One agent session with a few subagents can spend GitHub's shared 5,000/hour
REST budget on its own: watchers, `gh api` polls and status checks re-fetch
the same objects over and over, and then every tool on the machine fails at
once. The read broker answers repeated in-session `gh api` GETs from a
durable cache in the clud daemon. Within a short TTL a repeat costs no
request; after it, the broker revalidates with the stored ETag, and GitHub
does not charge an authorized `304 Not Modified` against the rate limit.

Phase 1 brokers **only `gh api <endpoint>` GETs**. Porcelain commands
(`gh pr view`, `gh run view`, ...), incremental `since=` rewrites, merged
collections, subscriptions and the budget floor are later phases of #1743.

## Path of one read

```
agent: gh api repos/o/r/actions/runs/7 --jq .status
  │
  ▼  session gh alias (shim_main.rs gh_shim::relay)
  │   gh_broker::classify::api_read   → a brokerable GET?   no → real gh
  │   gh_broker::client::read         → POST /gh/read        miss → real gh
  │      (daemon.json: dashboard_port + capability cookie)
  ▼
daemon HTTP thread (daemon/http.rs → spawn_gh_read)
  │   gh_broker::service::GhBroker::read
  │     fresh within TTL? ─────────────────────────► cached body ("cache")
  │     single-flight: one leader per key, others wait ("cache")
  │     leader: real `gh api -i <endpoint> [-H If-None-Match: <etag>]`
  │        304 → cached body, TTL restarts ("304")
  │        200 → store body + ETag ("full")
  │        non-2xx → 409 to the shim ("passthrough")
  ▼
shim: gh_broker::client::serve_replay(body) on 127.0.0.1:<port>/clud-gh-replay/<random>
  │   real gh api http://127.0.0.1:<port>/clud-gh-replay/<random> --jq .status
  ▼
caller's stdout: what the real gh prints for that body
```

## Output contract

The shim never formats output itself. After the broker answers, it reruns
the caller's own `gh api` argv on the real `gh`, with only the endpoint word
replaced by a one-shot loopback URL that serves the brokered body with the
upstream headers (`gh` accepts an absolute URL as the endpoint). `--jq`,
`--template`, `--silent`, TTY pretty-printing and colors are therefore `gh`'s
own code over the same body bytes, so the output is identical by
construction. Go never proxies loopback; the only credential `gh` could
attach for the loopback host (`GH_ENTERPRISE_TOKEN`) reaches the shim's own
listener. The replay path is a 128-bit random token; any other path
gets a 404.

What would not be identical is never brokered. `classify::api_read` accepts
only `--jq`/`-q`, `--template`/`-t`, `--silent`, `-X GET`/`--method GET` and
`--hostname`. It passes through everything else to the real `gh` unchanged:
`-i`/`--include` (the headers would name the loopback), `--paginate`,
`--slurp`, `-H`, `-p`, `--cache`, `--verbose`, any body flag
(`-f`/`-F`/`--field`/`--raw-field`/`--input`), `graphql`, absolute URLs,
`{owner}`-style placeholders (`gh` fills them from the checkout), non-UTF-8
argv, and any flag it does not know. Only `2xx` responses are replayed. A
non-2xx upstream status makes the shim run the real `gh`, so error text and
exit codes are `gh`'s own. That costs a second request for errors only.

## Fail open

Every shim-side miss runs the real `gh` with the original argv:

- the setting is off, or the session predates it (`CLUD_GH_READ_BROKER` is
  absent or not `1`);
- `daemon.json` is missing or names a dead port (500 ms connect timeout);
- an older daemon without the route answers 404;
- the broker answers non-200: upstream non-2xx (409), a store or transport
  error (502), or a rejected request (400);
- the 45 s request deadline passes.

The daemon is lazy and can retire when idle. A foreground session holds a
lease on it, so in practice it is up while a session runs, but the shim never
starts one.

## Identity and the upstream `gh`

The broker reaches GitHub only through the real `gh` (`upstream::GhCli`),
so authentication, enterprise hosts and proxies stay `gh`'s. The shim sends
its `CLUD_GH_SHIM_TARGET` and the caller's values of
`gh_broker::FORWARDED_ENV`: `GH_HOST`, the token variables, `GH_CONFIG_DIR`,
the config-home and proxy variables. The daemon runs that `gh` with its own environment, where each of
those keys is replaced by the caller's value or removed. It strips debug,
pager and forced-color variables and adds `GH_PROMPT_DISABLED=1` and
`NO_COLOR=1`, so the `-i` output it parses is plain. It first revalidates the
request: the path must be named `gh` and pass `shim_registry::valid_target`
(absolute, executable, not a clud alias, not a copy of clud), the endpoint and
hostname must pass the classifier's checks, and env keys must be in
`FORWARDED_ENV`. The cache key hashes
endpoint, `--hostname`, `gh` path and the forwarded values, so two identities
never share a body. Tokens are hashed into the key and never stored or
logged. The route is behind the dashboard capability cookie and Host check,
like `/telemetry/log`.

## Freshness

| Endpoint | TTL |
| --- | --- |
| `.../actions/runs...`, `.../actions/jobs...` | 30 s |
| everything else | 60 s |

`CLUD_GH_FRESH=1` skips the TTL but still revalidates. Completed workflow
runs are **not** cached forever: `gh run rerun` reopens a completed run
under the same id, and a `304` revalidation is free, so phase 1 gives every
object a TTL.

**Write invalidation.** After any non-read `gh` call in a session, the shim
posts `/gh/invalidate`. That covers `gh api` with a non-GET method, a body
flag or `graphql`, and any porcelain subcommand that is not `view`, `list`,
`checks`, `diff`, `status`, `watch` or `download` (`classify::may_write`).
`gh auth login/switch/logout/refresh` count as writes too: they change the
identity behind the same env.
The broker records the time; an object counts as fresh only if its last
upstream request *started* after the newest invalidation, and in-flight
fetches are detached so a later read cannot join one that began before the
write. A false positive costs one free `304`. Writes made outside clud
sessions (the web UI, CI, another machine) are seen within one TTL.

## Store and ledger

`<state>/gh-broker.redb` (`~/.clud/state`, or `$CLUD_DAEMON_STATE_DIR`),
owned by the daemon:

| Table | Row |
| --- | --- |
| `object_meta` | key → label, status, headers, ETag, Last-Modified, `fetched_at_ms` |
| `object_body` | key → raw body (≤ 2 MiB; larger bodies are served, not cached) |
| `meta` | `invalidated_at_ms` |
| `ledger` | seq → `ts_ms`, `session_id`, `key` (`host/endpoint`), `outcome`, `upstream_requests`, `rate_remaining` |

`outcome` is `cache`, `304`, `full`, `passthrough` or `error`. Objects are
pruned oldest-first above 4,096 and ledger rows above 20,000. Calls the shim
never brokers are not in the ledger; they are in the per-invocation
`git-gh.jsonl` telemetry ([git-gh-telemetry-shim.md](git-gh-telemetry-shim.md)).
The design called for SQLite, but clud uses redb, not SQLite; see
[DD-150](../DESIGN_DECISIONS.md#dd-150-the-session-gh-read-broker-reruns-the-real-gh-over-a-loopback-replay).

## Setting

`git.gh_read_broker` in `~/.clud/settings.json` (default `true`, toggled
in `clud settings`). `shim_session::activate_rm` exports it at launch as
`CLUD_GH_READ_BROKER=1|0`; a running session keeps the value it launched
with.

## Code

| File | Role |
| --- | --- |
| `crates/clud-bin/src/gh_broker/classify.rs` | `api_read`, `may_write` |
| `crates/clud-bin/src/gh_broker/client.rs` | shim side: daemon request, replay server |
| `crates/clud-bin/src/gh_broker/service.rs` | TTL, single-flight, revalidation, invalidation, ledger, `/gh/read` body |
| `crates/clud-bin/src/gh_broker/store.rs` | redb tables |
| `crates/clud-bin/src/gh_broker/upstream.rs` | `gh api -i` transport and parser |
| `crates/clud-bin/src/shim_main.rs` | `gh_shim::brokered_read`, invalidation after writes |
| `crates/clud-bin/src/shim_main/dispatch.rs` | builds the `BrokerClient` from the session env |
| `crates/clud-bin/src/daemon/http.rs` | `/gh/read` (own thread) and `/gh/invalidate` routes |

Tests: `gh_broker` unit tests cover classification, TTL hits with zero
upstream requests, `304` revalidation, single-flight, invalidation, identity
separation, error passthrough, and the real `gh api -i` transport against a
fake `gh`. `tests/test_gh_read_broker.py` drives the real alias against a fake
daemon and a fake `gh`. It checks byte-identical output, the fallbacks
(no daemon, a daemon miss, the setting off) and write invalidation.

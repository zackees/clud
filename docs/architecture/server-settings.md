# Server-side settings

Some values in clud must be changeable without a release, so they are served
from this repository (#1192,
[DD-072](../DESIGN_DECISIONS.md#dd-072-server-side-settings-are-one-baked-in-json-document-with-per-section-last-known-good)).
Today the only such values are DeepSeek's model names, but the mechanism is
general.

## The document

`crates/clud-bin/assets/server-settings.json` is the only source. The build
embeds it as the **built-in copy**, and installed builds fetch the same path
from `main`:

```text
https://raw.githubusercontent.com/zackees/clud/main/crates/clud-bin/assets/server-settings.json
```

```json
{
  "schema_version": 1,
  "sections": {
    "deepseek": { "default_model": "deepseek-flash", "subagent_model": "deepseek-flash" }
  }
}
```

- **`schema_version`** is the document format. A build accepts documents up to
  its own `SCHEMA_VERSION`. Additive changes never bump it. A bump makes every
  older build ignore the whole document and keep its last good copy, so treat
  it as a last resort and prefer adding a new section key.
- **`sections`** holds one object per setting. Each section is validated on its
  own: first a typed decode that ignores unknown fields, then the section's
  `validate()`.
- **Any other top-level key** is ignored.

## Resolution

Each registered section takes the first valid value from this list:

| Order | Source | Notes |
| --- | --- | --- |
| 1 | The copy served now | Used only when a refresh finished within the launch's wait |
| 2 | The last cached valid copy | `~/.clud/cache/server-settings/server-settings.json` |
| 3 | The built-in copy | Always valid; the guard tests enforce this |

A process resolves its settings once, through `server_settings::snapshot()`. Every
consumer in that process then sees the same values, even if a refresh lands
later. Loading is lazy: a launch that never reads a setting never touches the
cache or the network.

### Merging a served document

| Served section | Result in the cache |
| --- | --- |
| Valid | Replaces the cached value |
| Present but invalid (wrong type, or `validate()` fails) | Keeps the previous valid value; with no previous value, the section is dropped and the built-in value applies |
| Absent or `null` | Dropped, so the built-in value applies. This is how you reset an override |
| Unknown to this build | Kept verbatim, so a newer clud sharing the cache still receives it |

A **whole-document** failure discards the served document and leaves the cache
untouched. That happens when:

- the body is not strict JSON;
- the root is not an object;
- `schema_version` is missing or unsupported;
- `sections` is missing or not an object.

The cached document is re-validated on every read. A corrupt cache is ignored
rather than trusted, and the next good refresh replaces it.

## Refresh timing

This lives in `server_settings/store.rs`.

- **Fresh cache.** A cache younger than 15 minutes is used with no request.
  `raw.githubusercontent.com` itself caches for five minutes, so polling more
  often would not surface edits any sooner.
- **Stale cache.** A named thread (`clud-server-settings-refresh`) fetches with a
  5 s timeout, merges, and writes the cache by staging `.json.partial` and
  renaming it. The launch waits at most 750 ms for that thread:
  - If the refresh finishes in time, this launch uses the result.
  - If not, this launch continues on its cache, and the refresh still lands in
    the cache for the next launch.
- **Backoff.** Each attempt touches `server-settings.json.last-attempt`. After a
  failed attempt, clud does not retry for 15 minutes. A network that cannot
  reach GitHub therefore costs at most 750 ms per 15 minutes.

## Strict JSON

`server_settings/json.rs` parses both the served body and the cache. It
enforces these rules:

- **Encoding and size.** The input must be UTF-8 and at most 64 KiB. A leading
  byte-order mark is stripped.
- **One value only.** The input must be exactly one JSON value. Trailing
  content, comments, trailing commas, single quotes, and `NaN`/`Infinity` are
  rejected.
- **No duplicate keys.** Duplicate object keys are rejected at any depth.
  Without this check, `serde_json` would silently keep the last one.
- **Bounded nesting.** Depth is capped by `serde_json`'s recursion limit, so a
  nesting bomb returns an error instead of overflowing the stack.

A non-2xx response, a transport error, or a timeout counts as a fetch failure.

## Controls

| Variable | Effect |
| --- | --- |
| `CLUD_SERVER_SETTINGS=0` (also `false`, `off`, `no`) | Use the built-in copy only: no cache, no network |
| `CLUD_SERVER_SETTINGS_URL=<url>` | Fetch a draft or a mirror instead. The cache is bypassed, so the real file's last good copy is never overwritten |
| `CLUD_VERBOSE_SERVER_SETTINGS=1` | Print refresh and fallback diagnostics to stderr, including why a section was rejected |

## Changing a served value

1. **Edit `crates/clud-bin/assets/server-settings.json` in a PR.** The guard
   tests fail if the document is not strict JSON, if a registered section is
   missing or invalid, or if a section is not registered.
2. **Merge to `main`.** Installed builds see the change within about 20
   minutes: up to 5 minutes of CDN caching plus up to 15 minutes of local cache
   freshness. It applies from the next launch after that.
3. **Optionally, try it first.** Serve the draft file and point
   `CLUD_SERVER_SETTINGS_URL` at it.

The same edit also becomes the built-in copy in the next release.

## Adding a setting

1. **Define the section type.** Write a `Deserialize` struct and implement
   `server_settings::Section` for it, providing `KEY` and `validate()`. Keep
   the values data-only: never serve credentials, code, or URLs that clud would
   fetch.
2. **Register it.** Add `SectionSpec::of::<YourSection>()` to `SECTIONS` in
   `server_settings/sections.rs`.
3. **Give it a built-in value.** Add the value under `sections` in the embedded
   JSON.
4. **Read it.** Call `server_settings::snapshot().section::<YourSection>()`, or add
   a cached accessor like `server_settings::deepseek()`.

## Testing

- **Unit tests** never read the cache or the network. Under `cfg(test)` the
  process snapshot is always the built-in copy. The cache and refresh logic is
  tested through `store::load` with an injected fetch and a temporary cache
  path.
- **Subprocess test harnesses** set `CLUD_SERVER_SETTINGS=0`.
- **The embedded file** is checked through `include_str!`, never by reading the
  source tree, because CI runs tests from a prebuilt bundle.

## Code map

| File | Owns |
| --- | --- |
| `crates/clud-bin/assets/server-settings.json` | The built-in and served document |
| `crates/clud-bin/src/server_settings/mod.rs` | `Section`, `SectionSpec`, document parsing, merge, `Snapshot`, process loading, controls |
| `crates/clud-bin/src/server_settings/json.rs` | The strict JSON parser |
| `crates/clud-bin/src/server_settings/store.rs` | Cache, refresh thread, backoff, fetch |
| `crates/clud-bin/src/server_settings/sections.rs` | The section registry and the section types (DeepSeek) |

Consumers:

- `main.rs` reads the direct-launch DeepSeek default through
  `provider_default_model`.
- `foreground_runtime.rs` fills DeepSeek's haiku and subagent slots through
  `provider_subagent_model`.

See [provider selection](provider-selection.md#served-deepseek-model-names).

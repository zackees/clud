# Codex model publication and selection

Issue #1476 separates current Codex model discovery from clud releases. The
strict, small `https://talos-hq.github.io/clud/models/manifest.json` document
contains the latest verified stable Sol, Luna, and Terra wire IDs, a Sol/low default,
source, and check time. It is not the installer `manifest.json` catalog.

`models/publish.py` reads OpenRouter's public OpenAI namespace and
models.dev's public OpenAI catalog; no key is needed. Only exact stable
`gpt-N[.N]-sol`, `gpt-N[.N]-luna`, and `gpt-N[.N]-terra` IDs can be selected.
`python -m models.refresh_openrouter` refreshes the checked-in
`crates/clud-bin/assets/codex-models.json` from the public OpenRouter models endpoint.
The runtime embeds this snapshot and uses its models only when the installed
Codex App Server lists them as locally available. A snapshot newer than the
published document advances an older model version; a later server publication
can still roll a model back. New observations are merged with the last published document, so one or both catalogs
disappearing cannot erase a family or fail the model check. A first
publication has the reviewed bundled snapshot and fallback IDs. The nightly
publisher also applies `models/selection-policy.json`: a model rejected by
the ChatGPT Codex backend is excluded even if public catalogs and the previous
manifest still list it. This permits a Pages rollback for installed clients;
public API availability alone does not prove ChatGPT backend availability. The
workflow skips when a successful manual publication is less than 24 hours old;
manual dispatch always checks. An unchanged selection does not redeploy. A
failed installer-site fetch or invalid prior document also leaves Pages
untouched. Its Pages artifact copies the installer renderer's tested published
file inventory byte-for-byte, including any future static assets.
Conversely, installer publication copies the existing validated model document
into its full-site artifact. The workflows use one deployment concurrency
group, and a model-only change never triggers the expensive release-history
build. Both deployments smoke their own and the other public manifest.

`codex_runtime.rs` fetches that document with a bounded request, parses it
strictly, and checks each advertised ID against the installed Codex App
Server's paginated `model/list` result for local, visible low-effort
availability. A row without valid low-effort metadata is ineligible without
invalidating other rows. The raw published document is cached at
`~/.clud/cache/models/manifest.json`; independently validated last-good Sol, Luna,
and Terra IDs live at `~/.clud/cache/models/last-good.json`. An older manifest
cache seeds that per-family state during migration. The effective choice is
immutable for a process. Each family independently selects an account-available published ID or
last-good ID. A newer snapshot can advance an older publication when its
version is higher and the account lists it; a later publication can roll back
the selection. If none is usable, the reviewed fallback is used. An
unavailable Luna cannot downgrade Sol. Explicit CLI and saved provider
selections continue to outrank this default. The stable Claude discovery ID
`clud-claude-codex-sol` maps to the current wire ID; the Claude argv remains
synthetic while bridge requests and native Codex argv carry the real model.
The direct Claude bridge preserves the launch's resolved wire ID when Claude
sends that selection's discovery ID, so `--model` does not get replaced by a
later dynamic family lookup.
`--dry-run` includes the selected family's actual `codex_model_source` for a
dynamic Sol, Luna, or Terra selection.

For local verification, run `bosn run --task model-manifest-test`,
`bosn run --task codex-all-lib-test`, and
`bosn ci run --workspace . --workflow .github/workflows/model-pages.yml --job build-site --trigger pr --wait`
(bosn stubs `actions/configure-pages`); only the Pages deployment API needs
GitHub.

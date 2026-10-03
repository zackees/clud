# Codex model publication and selection

Issue #1476 separates current Codex model discovery from clud releases. The
strict, small `https://zackees.github.io/clud/models/manifest.json` document
contains the latest verified stable Sol and Luna wire IDs, a Sol/low default,
source, and check time. It is not the installer `manifest.json` catalog.

`models/publish.py` reads OpenRouter's public OpenAI namespace and
models.dev's public OpenAI catalog; no key is needed. Only exact stable
`gpt-N[.N]-sol` and `gpt-N[.N]-luna` IDs can be selected. New observations
are merged monotonically with the last published document, so one or both
catalogs disappearing cannot erase a family or fail the model check. A first
publication has reviewed GPT-6 Sol/Luna baseline IDs. The nightly
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
`~/.clud/cache/models/manifest.json`; independently validated last-good Sol
and Luna IDs live at `~/.clud/cache/models/last-good.json`. An older manifest
cache seeds that per-family state during migration. The effective choice is
immutable for a process. Each family independently prefers the published ID,
then locally usable last-good, then the reviewed GPT-6 built-in fallback;
an unavailable Luna cannot downgrade Sol. Explicit CLI and saved provider
selections continue to outrank this default. The stable Claude discovery ID
`clud-claude-codex-sol` maps to the current wire ID; the Claude argv remains
synthetic while bridge requests and native Codex argv carry the real model.
`--dry-run` includes the selected family's actual `codex_model_source` for a
dynamic Sol or Luna selection.

For local verification, run `bosn run --task model-manifest-test`,
`bosn run --task codex-all-lib-test`, and
`bosn ci run --workspace . --workflow .github/workflows/model-pages.yml --job build-site --trigger pr --wait`
(bosn stubs `actions/configure-pages`); only the Pages deployment API needs
GitHub.

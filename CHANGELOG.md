# Changelog

## Unreleased

- The native `bad-cmd` PreToolUse hook is renamed `cmd-scan` (new `clud-cmd-scan`
  binary; `clud-block-bad-cmd` ships unchanged for one release and existing
  hook configs are migrated forward automatically, mirroring the earlier
  python-shim rollout). `cmd-scan` now also eagerly hands `git clone`/`git
  worktree add` destinations to the clud daemon's GC registry as soon as the
  command is allowed to run, instead of waiting on `WorktreeScanner`'s passive
  poll, and denies `git clone` outside a repo's `.extern-repos/` by default
  (bypass via `CLUD_BAD_CMD_OVERRIDE`, same mechanism as other rules).
  See zackees/clud#532.

## 2.4.0 - 2026-07-10

- Daemon GC now reclaims two disk sinks its redb registry never tracked: the
  backend agent's OS temp scatter and stale Rust `target/` output. Session temp
  is redirected to `~/.clud/tmp` at launch (`TMPDIR` on Unix, `TMP`+`TEMP` on
  Windows; default on, `CLUD_SESSION_TMP=0` opts out) and swept of entries older
  than 48h. `target/` reclamation is opt-in via `CLUD_GC_TARGET_ROOTS`
  (`CLUD_GC_TARGET_STALE_DAYS`, default 14). Both sweeps run on a detached
  background thread that prioritizes by disk pressure, else defers until system
  CPU is under `CLUD_GC_SWEEP_MAX_CPU_PCT` (default 60%). See zackees/clud#509,
  zackees/clud#510, zackees/clud#511.

## 2.3.0 - 2026-07-07

- Native `clud-block-bad-cmd` rollout is now end-to-end: install scripts verify
  the helper after `uv tool install --force`, startup warns on stale installs
  missing the helper, exact old `clud tool run hooks/block-bad-cmd.py` hook
  commands migrate to the native helper when safe, and bundled tools now have a
  retired-tool purge lifecycle for future shim removal. See zackees/clud#499.
- `clud --codex` now configures Codex to use `CODEX.md`, then `CLAUDE.md`, as
  project instruction fallbacks when `AGENTS.md` is absent at the repository
  root. The injected `project_doc_fallback_filenames` override is visible in
  `--dry-run` output. See zackees/clud#485.
- `clud --codex` / `clud --claude` global launch setup now uses an inline
  terminal selector with a visible selection cursor, hides the hardware cursor
  while active, supports Esc/Ctrl-C cancellation paths, and persists the
  selected backend as the bare `clud` default until the opposite backend is
  selected globally.
- New `shell.disable_powershell` toggle in `~/.clud/settings.json` (default
  `false`, per-backend overrides under `shell.claude` / `shell.codex`). When
  enabled for the active backend, clud now injects `CLUD_DISABLE_POWERSHELL=1`
  into the child env so skills can branch on it. For Claude specifically,
  clud also injects `CLAUDE_CODE_USE_POWERSHELL_TOOL=0` and points
  `CLAUDE_CODE_GIT_BASH_PATH` at a lazily-fetched portable Git Bash bundle
  vendored from `zackees/zcmds_win32` (~9 MB, sha256-pinned, cached at
  `~/.clud/vendor/win32/git-bash-bin-<sha[..12]>/`). Driven by FastLED #3336:
  Claude on Windows defaults to PowerShell, which silently breaks
  bash-native tooling (DTR/RTS-less serial, `&&` parser error, `.py`
  file-association semantics). See zackees/clud#447.

## 2.0.19 - 2026-06-07

- `/clud-pr` and other bundled skills now install under `~/.codex/skills/`
  for `clud --codex`, matching the skill path Codex itself loads from.
- Old clud-managed bundled skill copies under `~/.agents/skills/` are removed
  on first Codex global setup after upgrade. User-authored content and
  unrelated skill directories are preserved.

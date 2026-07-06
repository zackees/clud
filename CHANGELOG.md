# Changelog

## Unreleased

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

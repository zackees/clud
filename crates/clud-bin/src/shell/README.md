# shell/

Backend-shell selection plumbing for [issue #447](https://github.com/zackees/clud/issues/447) — the
"disable PowerShell on Windows" toggle.

## Why this module exists

Both Claude Code and Codex CLI default to PowerShell on Windows. PowerShell
silently breaks bash-native tooling:

- `System.IO.Ports.SerialPort` drops bytes without explicit DTR/RTS
  (FastLED [#3336](https://github.com/FastLED/FastLED/issues/3336)).
- The Windows PowerShell 5.1 parser rejects `&&` / `||` chains.
- `.py` invocations use file-association semantics rather than running
  through `python`.
- `2>&1` on native exes wraps stderr in `NativeCommandError` and flips `$?`.

The user-facing toggle lives in `clud_settings::shell.disable_powershell`.
The two enforcement layers live elsewhere:

- **Layer 1 (Claude env-var kill-switch)** — `runner.rs::child_env_for_backend`
  reads the toggle and, for Claude, injects `CLAUDE_CODE_USE_POWERSHELL_TOOL=0`
  + `CLAUDE_CODE_GIT_BASH_PATH=<resolved>` into the child env. Both env vars
  are undocumented but verified in the strings of the bundled `claude.exe`.
- **Layer 2 (PreToolUse hooks)** — load-bearing for Codex (which has no
  env-var equivalent — openai/codex#16717 is closed) and belt-and-suspenders
  for Claude. Lands in a follow-up PR via `hook_health/repairs.rs` +
  `codex_hook_normalize.rs`.

## What's here

- `mod.rs` — module root.
- `git_bash_resolver.rs` — lazy fetch + sha256 verify + extract of a
  portable Git Bash bundle sourced from `zackees/zcmds_win32` (9.4 MB,
  pinned). Cache layout:
  `~/.clud/vendor/win32/git-bash-bin-<sha256[..12]>/git-bash-bin/bash.exe`
  with a sibling `.complete` sentinel written **last** so a partial
  extraction is never advertised as ready. Public API:
  ```rust
  pub fn resolve_or_fetch_git_bash(home: &Path) -> Result<PathBuf, FetchError>
  ```

## Manifest location

`crates/clud-bin/vendor/win32/git-bash-bin.toml`. Embedded at compile time
via `include_str!` so the resolver works regardless of where the binary is
launched from. Bumping the manifest means bumping both `sha256` and
`upstream_commit_sha` in lockstep — see the file's own comments for the
recompute command.

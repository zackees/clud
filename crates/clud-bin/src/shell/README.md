# shell/

Backend-shell selection plumbing for [issue #447](https://github.com/zackees/clud/issues/447) — the
"disable PowerShell on Windows" toggle, plus the login-shell environment policy
for [issue #753](https://github.com/zackees/clud/issues/753).

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
- `completion_guard.rs` — keeps Git-Bash completion functions out of the
  backend's **shell snapshot** (#753). Unrelated to shell *selection*: this is
  about the login environment the chosen shell starts in. Claude Code snapshots
  a login shell once per session and replays every captured function into every
  later `Bash` tool call; Git's `__git_*` completions survive its
  double-underscore filter and each costs two process spawns per replay (~170
  per tool call, 4.4 s → 20 s of CPU depending on load). Exporting
  `WINELOADERNOEXEC=1` makes `/etc/profile.d/git-prompt.sh` skip
  `git-completion.bash`: 85 captured functions → 1, 4,413 ms → 49 ms. Public
  API:
  ```rust
  pub fn env_overrides() -> Vec<(String, String)>          // policy
  pub fn env_overrides_for(is_windows: bool, opted_out: bool) -> Vec<(String, String)>  // test seam
  ```
  Applied by **both** `runner::child_env` and `daemon::io_helpers::child_env` —
  those builders are duplicates and the daemon one has drifted before, so a
  policy added to one belongs in both. Opt out with
  `CLUD_GIT_BASH_COMPLETIONS=1`. Guardrail:
  `tests/shell_completion_guard.rs` asserts a real login shell's **function
  count**, not the env var, because the lever is a Git-for-Windows
  implementation detail that could change silently. Rationale:
  [DD-031](../../../../docs/DESIGN_DECISIONS.md#dd-031-git-bash-completions-are-suppressed-in-the-backends-login-shell).
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

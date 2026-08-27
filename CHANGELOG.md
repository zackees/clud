# Changelog

## Unreleased

- Native Codex one-shot built-ins (`do`, `up`, `rebase`, and `fix`) now seed the
  interactive TUI instead of routing through `codex exec`, keeping progress and
  follow-up input live. `clud do` also accepts an optional URL or free-form goal:
  a foreground TTY prompts when it is omitted, while dry-run, piped, and
  background invocations fail clearly rather than blocking. Explicit `-p`,
  `loop`, and `grind` remain non-interactive. See zackees/clud#1036.
- The Windows wheel now ships `clud-cmd-scan.exe`. 2.5.5's hook rollout
  migrated `~/.claude/settings.json` PreToolUse configs to the renamed
  `clud-cmd-scan` binary, but the hand-packed win_amd64 wheel only carried the
  three pre-rename executables — every Bash hook call on Windows errored
  `clud-cmd-scan: command not found` and the bad-cmd scan was silently off.
  `REQUIRED_SCRIPTS` (the single list the Windows packer and both wheel/install
  verifiers iterate) gains the binary, and a new guardrail test reads
  `NEW_COMMAND` out of `block_bad_cmd_rollout.rs` and asserts the rollout's
  target is always a shipped script, so the next binary rename cannot repeat
  this. Affected 2.5.5 Windows installs can meanwhile set the hook command
  back to `clud-block-bad-cmd`. See zackees/clud#862.
- Doc-tests run again. The `Doc tests` step in `_build-target.yml` was keyed
  on `strategy == 'native'`, which no lane passes since the all-soldr matrix
  (#859), and it is the only doc-test runner in CI — coverage had silently
  dropped to zero. The step is now keyed on the x86_64 Linux triple, and
  `test_doc_tests_run_on_a_live_matrix_triple` asserts the condition names a
  triple that exists in the matrix. The dead-keyed `soldr-cook` dependency
  prebuild is replaced with an explicit `none` (a cook before `soldr prepare`
  fingerprints against the wrong toolchain env; re-enabling means cooking
  after prepare), and the retired `zigbuild` vocabulary is swept:
  `Strategy` literal, `maturin[zig]`/`ziglang` in the dev deps, and
  `build_wheel.py`'s vestigial zig-era `SOLDR_LINKER` override.
  See zackees/clud#863.
- `clud` no longer prints `[clud] updated /clud-pr` and `[clud] updated
  /clud-issue` on every single launch. Two skill installers used to embed two
  forked copies of those skills from two different source trees and both wrote
  `~/.claude/skills/<name>/SKILL.md`, so each launch overwrote the other's bytes
  and reported an update that changed nothing. Worse, Claude got one fork and
  Codex the other, so multi-forge support (#396) and PR Drive Mode (#395) never
  reached Claude users. There is now one installer (`src/skills.rs`) over one
  source tree (`crates/clud-bin/assets/skills/`); `src/skill_install.rs` and the
  top-level `skills/` tree are removed. `[clud] updated /<name>` prints only
  when the installed copy genuinely diverges from the bundled one, and a
  current install performs no write at all. Comparison is modulo whitespace, so
  a CRLF-vs-LF checkout is not treated as a change. See zackees/clud#844 and
  DD-039.
- The `clud-pr`, `clud-fix`, `clud-do` and `clud-pr-merge` skills are retired.
  Their orchestration — lock in a deliverable and block stopping until it is
  met — is what the harness's `/goal` Stop-hook command now does natively.
  Existing installs are cleaned out of every backend's skills dir on the next
  launch; a copy you took ownership of (the `managed-by: clud` marker removed)
  is left alone. They may be restored later.
- The cross-toolchain linter (`ci/banned_cross_tools.py`) now actually enforces
  what it documents. `cargo xwin`, the bare `xwin` CLI, `XWIN_*` env vars,
  `osxcross` and `cross` are banned **unconditionally** — they are MSVC- or
  Apple-only by construction, so the old "only if a literal triple is on this
  line" rule let `cargo xwin build --target $TARGET` through, and `cargo xwin`
  re-splats the MSVC CRT on every cold cache. `cargo zigbuild` / `maturin --zig`
  / `zig cc` stay target-conditional, so the Linux and manylinux lanes are
  untouched. Install patterns now match across lines (the `taiki-e/install-action`
  + `tool: cargo-xwin` shape could never fire before), and cover `cargo binstall`,
  `brew`, `apt`/`dnf`/`apk`/`choco`, `houseabsolute/actions-rust-cross`,
  `cross-rs/cross`, `tpoechtrager/osxcross` and hand-rolled `[target.*] linker =`
  overrides. Scope widens from `.github/` + `ci/` (48 files) to `bench/`,
  `crates/`, `dylints/`, `testbins/`, `tests/`, `.claude/hooks/`,
  Rust sources, Dockerfiles and the root entrypoints the old list missed
  (`install.sh`, `install.ps1`, `publish`) — 302 files. Prose that a
  comment-stripper cannot see can opt out with a line-scoped
  `cross-lint: allow` marker. See zackees/clud#714.

## 2.4.1 - 2026-07-25

- Windows PTY input now uses running-process's native terminal translator
  instead of clud's narrower duplicate, so arrow, Home/End,
  Insert/Delete, and Page Up/Down keys reach Codex as complete escape
  sequences. Clud's existing Shift+Enter, Ctrl+C, and Ctrl+V behavior is
  preserved. See zackees/clud#575.
- New `clud settings` interactive TUI: a small, cross-platform checkbox menu
  over global booleans in `~/.clud/settings.json` (space toggles, q quits
  and prompts to save if anything changed). `clud settings --list` prints
  current values non-interactively. First setting: `git.pr_wait_fail_fast`
  (off by default) gates the PR-wait fail-fast git command improvements —
  previously always-on — behind an explicit opt-in.
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

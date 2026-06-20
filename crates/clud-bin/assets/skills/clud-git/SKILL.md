---
name: clud-git
description: Worktree, branch, process-audit, and quarantine playbook extracted from /clud-pr so /clud-fix-quick and other skills can reuse it. Use this when the user asks for worktree cleanup, stale-branch teardown, or a process audit on a leftover dir.
triggers:
  - When the user asks to clean up worktrees, leftover .claude/worktrees/ directories, or stale feature branches
  - When the user reports "shells appear stuck" or worktree removal failing with file-lock errors on Windows
  - When the user invokes "/clud-git"
  - When another skill needs the worktree-create + teardown + audit playbook (delegated from /clud-pr, /clud-fix-quick, /clud-do)
---
<!-- managed-by: clud -->

# /clud-git

Single source of truth for the worktree / branch / process-audit / quarantine playbook. Today the same procedure is duplicated as prose inside `/clud-pr`'s Worktree Workspace section; this skill extracts it so `/clud-fix-quick`, `/clud-do`, and any future skill can delegate without copying.

The three hard rules from `/clud-pr` carry over verbatim — this skill exists to centralize them, not to relax them.

## Three hard rules

1. **`.gitignore` gate first.** Before creating any worktree under `.claude/worktrees/`, confirm `.gitignore` covers `.claude/`. If it doesn't, refuse to create the worktree inside the repo — use a sibling path (`../<repo>-wt-<branch>/`) or ask to add `.claude/` to `.gitignore`.
2. **Process audit before destructive removal.** Windows file locks routinely cause `git worktree remove` to fail with `Permission denied` / `Access is denied`. Always audit which processes hold the directory before resorting to `--force`, then prefer `clud trash` over `rm -rf` retry loops.
3. **Never blind-loop `rm -rf`.** A `rm -rf` retry loop hides the actual problem (a process holding a file open) instead of fixing it. Audit, stop the specific offender, OR quarantine via `clud trash`.

## Worktree creation playbook

1. **Sync main.** `git fetch origin main` so the new worktree starts from current main, not stale local state.
2. **Stale worktree prune.** `git worktree prune` to clear git's view of any worktrees deleted on disk without proper removal.
3. **`.gitignore` gate.** Grep `.gitignore` for one of `.claude/`, `.claude/**`, `/.claude/`. If absent, refuse — see hard rule 1.
4. **Create.** `git worktree add -b <branch> .claude/worktrees/<short-name> origin/main`. The path component is freely chosen — convention is `<slice-or-feature>-<issue-number>` so leftover dirs are searchable.
5. **RED -> GREEN inside the worktree.** All edits, lint, tests, and commits happen inside the worktree path. Never edit the main checkout for worktree-scoped work.

## Worktree teardown playbook

1. **`git status` clean check.** Inside the worktree, confirm no uncommitted changes — refuse to tear down a dirty worktree without explicit user OK.
2. **Process audit.** Before removal, identify what holds the worktree path open:
   - **Windows**: `Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like "*<branch>*" -or $_.CommandLine -like "*<worktree-path>*" }`. Common holders: `zccache.exe`, `rust-analyzer.exe`, the agent's own PowerShell session (cwd anchored inside the dir).
   - **Unix**: `lsof +D .claude/worktrees/<name>` or `fuser -m .claude/worktrees/<name>`.
3. **Stop only specific abandoned holders.** Never kill unrelated processes. If the holder is the agent's own shell session (cwd anchored), cd to an absolute path outside the worktree first.
4. **`git worktree remove --force`.** Pass `--force` only after the audit shows no useful holders. If it still fails with "Access is denied" / "device or resource busy", fall through to step 5.
5. **`clud trash` quarantine fallback.** `clud trash .claude/worktrees/<name>` moves the path to `~/.clud/trash/<timestamp>/` for the daemon's GC sweeper to reap when handles release. This is the documented fallback — see `/clud-pr` for the precedent.
6. **`git worktree prune`** + verify `git worktree list` no longer mentions the path.
7. **Delete the local branch** if it was created for the worktree: `git branch -D <branch>` for already-merged work; `git branch -d` for non-merged (which will refuse and force you to confirm).

## Process-audit playbook (standalone)

When the user reports "my shell appears stuck" or `git` commands hang on a Windows path:

1. **Identify the anchored cwd.** A Bash or PowerShell session whose cwd is inside a deleted-or-moved dir will fail every command because the hook system can't resolve its own scripts. Look for the error pattern `python .claude/hooks/check-soldr.py: No such file or directory`.
2. **Cd to a known-good absolute path** before running any other command: `cd "C:/Users/niteris/dev/clud2"` (or your repo root).
3. **Re-audit for stale holders** of the original problem path. The shell session unanchoring usually clears the immediate issue; lingering processes like zccache or rust-analyzer may still need explicit attention.

## Standard recovery patterns

| Symptom | Fix |
|---|---|
| `git worktree remove` → "Access is denied" | Process audit → stop specific holders → retry. If still failing → `clud trash`. |
| 10+ leftover dirs under `.claude/worktrees/` | Batch `git worktree remove --force` for each git-tracked one, then `clud trash` for the orphan dirs. Delete the merged branches with `git branch -D` afterward. |
| "Shells appear stuck" after a teardown | Bash cwd is anchored on a deleted dir. Cd to absolute path. PowerShell tracks cwd independently and is usually the safer escape. |
| Local branch list cluttered with old feature branches | `git branch --merged main \| grep feat/` to find merged ones; `git branch -D` to delete. Non-merged branches need explicit `git branch -D` after confirming the work landed via squash. |

## Delegation contract

When called from `/clud-pr`, `/clud-fix-quick`, `/clud-do`, or any other skill:

- The caller provides the `<branch>` / `<short-name>` and the operation (create / teardown / audit).
- This skill executes the playbook and returns structured evidence: what was created, what was torn down, what was quarantined, what processes were stopped.
- This skill does NOT manage `/goal` lifecycle — the caller owns its own goal hook.
- This skill does NOT push or merge PRs — those belong to the caller.

## Hard rules summary

1. **`.gitignore` gate before creating worktrees inside the repo.**
2. **Process audit before destructive removal.**
3. **`clud trash` is the documented Windows fallback, not `rm -rf` retry loops.**
4. **RED -> GREEN** for any code edits made inside a worktree (same as `/clud-pr`).
5. **Never kill unrelated processes** during a stale-holder cleanup.
6. **Cd to absolute path** before running git commands when shell behavior gets weird.
7. **Delete merged branches after squash-merge** so the local branch list doesn't accumulate.

## When NOT to use this

- The caller wants to ship a PR — that's `/clud-pr`'s job (which uses this skill internally).
- The caller wants to drive an existing open PR to merge — `/clud-pr` PR Drive Mode.
- The caller wants to investigate or design — this skill is mechanical; design discussion is the parent skill's responsibility.

## Related

- `/clud-pr` SKILL.md — original source of the worktree playbook; will delegate here once skill-to-skill calls are wired.
- `clud trash` subcommand — quarantines paths under `~/.clud/trash/<timestamp>/` for daemon GC.
- `/clud-fix-quick` — speed-mode skill that wants the same worktree teardown without the full `/clud-pr` ceremony.

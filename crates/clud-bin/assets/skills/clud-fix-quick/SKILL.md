---
name: clud-fix-quick
description: Local-first speed mode. Edits in place on the current branch, runs lint + targeted test + /clud-review on source-code changes only, skips all three for docs/config/SKILL.md. Pushes direct; falls back to PR + admin-merge without waiting for CI when the branch is protected. Sibling of /clud-fix; trades rigor for iteration speed.
triggers:
  - When the user says "/clud-fix-quick <issue-or-task>"
  - When the user asks to "quickly fix" / "just push" / "iterate fast" / "speed mode"
  - When the user wants a small, well-understood change shipped without the full worktree/CI scaffold
  - When iterating on docs, SKILL.md, README, or other non-source files
---
<!-- managed-by: clud -->

# /clud-fix-quick

Speed-mode sibling of [[clud-fix]]. Edits files in place on the current
branch (no disposable worktree), runs the smallest set of gates that
still catches regressions, then pushes — directly when the branch
allows, or via a PR that gets admin-merged immediately when the branch
is protected. The targeted-test step (when source files change) is the
GREEN signal that the change works; this skill delegates the RED ->
GREEN cycle to that focused test rather than running a full ritual,
trading some rigor for iteration speed.

Not a replacement for `/clud-fix` — that's the right tool when the
change matters or when the user wants paper trail through CI. Use
`/clud-fix-quick` when you already know what to change and want it on
main / your branch in seconds.

## Input

- A GitHub issue URL or number — the task to implement.
- A bare description of a small fix when no issue exists (e.g.
  `/clud-fix-quick fix the typo in docs/architecture/crash-reports.md
  line 27`).
- Optional flags:
  - `--no-review` skips the `/clud-review` step on source changes
    (use sparingly — for mass typo fixes, generated-code updates,
    revert-style commits where review can't help).
  - `--no-test` skips the targeted test (use even more sparingly —
    only for changes that genuinely have no exercisable test).

For multi-forge inputs, the same forge-recognition rules from
[[clud-fix]] / [[clud-pr]] / [[clud-do]] apply — read the URL prefix
or infer from `git remote get-url origin` for bare-number inputs.

## Hard Rules

1. **Current-branch warning is non-skippable.** If the active branch
   (`git rev-parse --abbrev-ref HEAD`) is not the repo's default
   branch, print the following warning verbatim in ALL CAPS at the
   top of the run, before any edit:

   ```
   ⚠ WARNING: COMMITTING DIRECTLY TO FEATURE BRANCH `<branch-name>`.
   THIS IS NOT A DISPOSABLE WORKTREE — A BAD EDIT WILL LIVE ON YOUR
   BRANCH. PROCEED ONLY IF YOU ARE SOLE AUTHOR OF THIS BRANCH.
   ```

   Continue the run; the warning is a heads-up, not a gate.

2. **No full test suite.** Run only the targeted test that exercises
   the change. Targeted-test selection rules per language:
   - **Rust**: `soldr cargo test -p <crate> --lib <module>::<test-name>`
     or `--test <file>` matching the changed file.
   - **Python**: `pytest -k <pattern>` matching the changed file or
     test name.
   - **JS/TS**: `npm test -- -t <pattern>` matching the changed file
     or test name.
   - **Shell**: no test framework standard; skip with a one-line
     notice and continue.

   If no targeted test exists for the changed file, print
   `no targeted test found for <file>; proceeding without regression
   check` and continue. Do NOT fall back to the full suite — that
   would defeat the whole point of this skill.

3. **Lint scoped to the touched files.** Use `bash lint --files
   <changed-files>` if the script supports per-file linting;
   otherwise run the project's default lint and accept the time cost.
   Lint failure blocks the push.

4. **`/clud-review` runs on source-code changes only.** Skip for
   docs/config/SKILL.md/markdown. When it runs, use the same
   delegated-mode contract as `/clud-fix` does — [[clud-review]]
   returns structured findings and the agent decides whether to
   address CRITICAL/HIGH ones before pushing. MEDIUM/LOW are
   advisory.

5. **No worktree, no `/goal`, no Stop hook installation.** This skill
   doesn't own a goal; it just does the work in the current checkout
   and reports.

6. **Direct push first, PR + admin-merge fallback second.** Try
   `git push origin <current-branch>`. If it succeeds, surface the
   commit SHA and stop. If it fails with `protected branch` / `remote
   rejected`, open a PR (`gh pr create --base <default-branch>`),
   then `gh pr merge <num> --admin --squash --delete-branch`
   immediately — no `--auto`, no waiting for CI. The skill trusts
   the local gates that already passed (lint + targeted test +
   `/clud-review`).

7. **Never `--no-verify`, never skip hooks.** Same as [[clud-fix]] and
   [[clud-pr]]. Hook failures are real signals; fix them, don't
   bypass them.

8. **No force-push.** If the local branch has diverged from the
   remote, surface the divergence and stop. Speed mode doesn't mean
   destructive.

## Target Classification

The skill classifies the touched files and runs the matching gates.
Mixed-class changes run the union (so any source file in the diff
triggers all three source gates).

| File extensions / paths | Class | Lint | Targeted test | `/clud-review` |
|---|---|---|---|---|
| `.rs`, `.py`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`, `.c`, `.cpp`, `.h`, `.hpp`, `.java`, `.kt`, `.swift` | source | yes | yes | yes |
| `.sh`, `.bash`, `.ps1` | source (shell) | yes (shellcheck if available) | none | yes |
| `Cargo.toml`, `pyproject.toml`, `package.json` | source-adjacent | yes (cargo check / pip check / npm install --dry-run) | yes (touched crate's tests) | yes |
| `.md`, `.txt`, `.rst`, `.adoc` | docs | no | no | no |
| `.yaml`, `.yml`, `.toml`, `.json`, `.ini` (NOT package manifests) | config | no | no | no |
| `SKILL.md`, `CLAUDE.md`, `AGENTS.md`, `README.md` | docs (skill/guidance) | no — but RUN the bundled-skill guardrail tests (`skills::` and `skill_install::`) if any `crates/clud-bin/assets/skills/*/SKILL.md` was touched | bundled-skill guardrails | no |
| `.github/workflows/*.yml`, `.github/instructions/*.md` | CI / instructions | no | no | no |
| everything else | other | no | no | no |

A polyglot change touching `.rs` and `.md` files is a source change
for review purposes; run the source gates.

## Workflow

1. **Branch check + warning.** Run `git rev-parse --abbrev-ref HEAD`.
   Compare against the default branch (`gh repo view --json
   defaultBranchRef --jq .defaultBranchRef.name`). If they differ,
   print the ALL CAPS warning from Hard Rule 1.
2. **Read the task.** Resolve the issue URL/number via `gh issue
   view` (or the equivalent native CLI per the forge-recognition
   rules), or use the freeform sentence directly.
3. **Implement in place.** Edit files directly in the current
   checkout — no worktree, no branch creation, no `.claude/`
   directory work.
4. **Classify the touched files.** Use the table above. Compute the
   set of gates to run as the union across all touched files.
5. **Lint** (if any source file in the diff). Block on failure.
6. **Targeted test** (if any source file in the diff). Block on
   failure. If `--no-test` was passed AND the change set is small
   AND no targeted test exists, skip with the documented one-line
   notice.
7. **`/clud-review`** (if any source file in the diff, unless
   `--no-review` was passed). Address CRITICAL/HIGH findings before
   push; surface MEDIUM/LOW as advisory.
8. **Commit.** Conventional commit message. Reference the issue
   when one exists (`Closes #<num>` in the commit body so auto-close
   fires after the merge).
9. **Push.** Try direct push to the current branch
   (`git push origin <current-branch>`). On `protected branch` /
   `remote rejected` / `non-fast-forward` (for a protected branch),
   fall back to the PR path: `gh pr create --base
   <default-branch>` then `gh pr merge <num> --admin --squash
   --delete-branch` immediately. Do NOT use `--auto` and do NOT wait
   for CI — the local gates already validated.
10. **Surface result.** One-line summary: commit SHA on the target
    branch, push mode (`direct` or `pr-admin-merged-<pr-num>`), and
    any non-default test/lint/review notes.

For code changes specifically, the targeted-test step IS the
RED -> GREEN signal: identify or write the smallest test that
exercises the change (RED before the edit if one didn't exist),
make the change, then rerun that focused test until it passes
(GREEN). The full-suite version of RED -> GREEN belongs to
[[clud-fix]]; this skill's targeted-only variant trades coverage
breadth for iteration speed.

## Failure Modes To Avoid

- **Editing files outside the user's stated scope.** Speed mode
  doesn't license drive-by refactoring.
- **Suppressing lint or test failures to "just push it."** That
  defeats the purpose of running them at all.
- **Calling a SKILL.md change "docs" and skipping the bundled-skill
  guardrails.** A SKILL.md edit IS a guardrail-tested change — run
  `soldr cargo test -p clud --lib skills::` and `skill_install::`.
- **Force-pushing the current branch to win a race with another
  contributor.** If your push is rejected for non-fast-forward
  reasons unrelated to branch protection, that's a real conflict;
  stop and surface it.
- **Admin-merging a PR with failing local gates.** The PR-fallback
  path only fires when direct push is rejected for protection
  reasons; lint / test / review must already be green.
- **Mis-classifying a polyglot change.** A PR touching `.rs` and
  `.md` files is a source-code change for review purposes; run the
  source gates.
- **Falling back to the full test suite when no targeted test
  exists.** Print the documented notice and continue; if the change
  is risky enough that "no test" feels wrong, use [[clud-fix]]
  instead.

## When NOT to use

- **The change is large or risky.** Use [[clud-fix]] (full rigor:
  worktree, RED -> GREEN, full suite, CI).
- **The change touches code paths you don't fully understand.** Use
  [[clud-fix]]'s investigation-report branch first.
- **You're not the sole author of the current feature branch.** Use
  [[clud-pr]] (creates a fresh disposable worktree, opens a PR for
  review).
- **You're on a third-party repo where admin-merge isn't allowed.**
  Use [[clud-pr]] (lets CI do its job; merges only after green).
- **You need to drive an existing open PR to merge.** Use [[clud-pr]]
  PR Drive Mode (#395).

## Related

- [[clud-fix]] — the rigor sibling. Use when the change matters or
  when the user wants the paper trail of full CI + a separate
  worktree.
- [[clud-pr]] — handles the PR + worktree path; also owns PR Drive
  Mode for actively driving open PRs to merge.
- [[clud-review]] — invoked on source-code changes by Hard Rule 4
  and Workflow step 7.
- [[clud-do]] — could route to `/clud-fix-quick` when the user's
  invocation hints at speed mode (out of scope for this skill's
  introduction; potential follow-up).

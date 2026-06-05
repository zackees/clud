---
name: clud-tag-release
description: Tag a release and let the auto-release workflow build it. Validates version match, clean main, no duplicate tag — then pushes the tag and surfaces the workflow URL.
triggers:
  - When the user types "/clud-tag-release" with or without a version arg
  - When the user says "cut a release", "tag a release", "ship a version"
  - When the user wants to publish a new version of a Rust/Python crate
---
<!-- managed-by: clud -->

# /clud-tag-release

Tag a release the way `zackees/zccache` does it: the tag is the trigger, the workflow does the rest. Five hard rules:

1. **Tag must equal workspace version.** Read the version from `Cargo.toml [workspace.package].version` (workspace) or `Cargo.toml [package].version` (single crate) or `pyproject.toml [project].version`. The tag pushed must match — `1.2.3` and `v1.2.3` are both fine, but `1.2.3` ≠ `1.2.4`. If they don't match, stop and tell the user to bump first; do not silently retag.
2. **Tag from clean default branch only.** Working tree clean, on `main`/`master`, in sync with `origin`. Never tag from a feature branch or a dirty tree.
3. **No duplicates.** Tag must not exist locally or on origin; no GitHub release record for that tag yet. If it does, stop — re-tagging a published release is destructive.
4. **Push the tag — don't `gh release create`.** `gh release create` creates the release record directly and bypasses the auto-release workflow's preflight (version validation, idempotency check, registry skip-existing). The deliverable is an annotated tag pushed to origin; the workflow does everything else.
5. **Verify the workflow kicked off.** After push, find the run, surface its URL, and confirm it transitioned to `in_progress` (or queued). A push that doesn't trigger a run means the workflow isn't wired up — surface that.

## Code Change Rule

This skill should not implement fixes or features. If the release is blocked by a code bug or missing feature, stop and send the user to `/clud-pr`; that work must use RED -> GREEN before this tag-release flow resumes.

## Workflow

1. **Detect the version source.** In repo root, in this order:
   - `Cargo.toml` with `[workspace.package].version` → that's the version.
   - `Cargo.toml` with `[package].version` (no workspace) → that's the version.
   - `pyproject.toml` with `[project].version` → that's the version.
   - None of the above → stop with a message; this skill is for Rust/Python projects with a tag-driven release flow.
2. **Detect the auto-release workflow.** Look for `.github/workflows/release-auto.yml` or any workflow whose `on.push.tags` pattern matches version-style tags (`v*`, `[0-9]*`). If none exists, stop and tell the user — point them at `zackees/zccache` (`.github/workflows/release-auto.yml`) for the canonical pattern. Don't try to push a tag with no workflow waiting.
3. **Resolve the tag to push.**
   - No arg → use the detected version verbatim. Tell the user: "I'll tag `<version>` (or `v<version>` if your existing tags use that prefix)." Pick the prefix that matches the most recent existing release tag; default to bare (no `v`) if there are no prior releases.
   - Arg `<version>` or `v<version>` → use as given, but validate it matches the workspace version. On mismatch: stop and tell the user to bump (`crates/.../Cargo.toml` or `pyproject.toml`) first.
4. **Set the goal.** With the resolved tag known, invoke `/goal Tag <tag> and confirm the auto-release workflow transitioned to in_progress or queued; report the run URL.` so the harness Stop hook blocks until the workflow run is located and surfaced. If any pre-flight gate (step 5) fails the skill aborts before pushing — clear the goal then.
5. **Pre-flight gates.** Every gate is blocking. Run them in parallel where possible (`git fetch` and `gh api` calls). Report the first failure and stop:
   - `git rev-parse --abbrev-ref HEAD` is `main` (or the repo's default — `gh api repos/<owner>/<repo> --jq .default_branch`).
   - `git status --porcelain` is empty.
   - `git fetch origin && git rev-list HEAD...origin/<default>` is empty (in sync, no diverging commits).
   - `git tag -l <tag>` and `git tag -l v<tag>` are both empty (no local tag).
   - `git ls-remote --tags origin <tag>` and same with `v<tag>` are empty (no remote tag).
   - `gh api repos/<owner>/<repo>/releases/tags/<tag>` returns 404 (no published release).
   - Last CI run on `<default>` is green: `gh run list --branch <default> --limit 1 --json conclusion,headSha`. If red or in progress, surface and ask before proceeding — don't silently ship a broken main.
6. **Confirm with the user.** Show:
   - Tag to push: `<tag>`
   - HEAD SHA: `<sha>` (and one-line commit subject)
   - Workflow that will fire: `<release-workflow-path>`
   - Last CI status on `<default>`: green/red/in-progress
   Wait for explicit confirmation. Don't proceed on silence.
7. **Tag and push.**
   - `git tag -a <tag> -m "Release <version>"` — **annotated**, not lightweight, so `git describe` works downstream.
   - `git push origin <tag>` — push *just the tag*, not `--tags` (which would push every local tag).
8. **Locate the triggered run.** Poll `gh run list --workflow=<release-workflow-file> --event=push --limit 5 --json databaseId,headSha,status,url` for up to 30 seconds, looking for a run whose `headSha` matches the tagged commit's SHA. The workflow can take a few seconds to register. If none appears after 30s, surface that — the workflow may not be configured to trigger on this tag pattern.
9. **Report.** Output exactly:
   - Tag pushed: `<tag>` → `<sha>`
   - Workflow run: `<url>` (status: `<queued|in_progress>`)
   - One line on what the workflow will do (build, publish to PyPI/crates.io, attach release artifacts) — pulled from the workflow file's job names, not invented.
   - Nothing else. Don't `gh run watch` unless the user asked.

## Failure modes to avoid

- **Mismatched tag and workspace version.** Re-tagging to a version that doesn't exist in `Cargo.toml`/`pyproject.toml` will fail in the workflow's preflight anyway — better to catch it locally and tell the user to bump.
- **`gh release create v1.2.3`.** This creates the release record directly; the auto-release workflow won't fire (or will see the release already exists and skip steps). Always use `git tag` + `git push origin <tag>`.
- **`git push --tags`.** Pushes every local tag, including stale ones from old branches. Push exactly the one you just created.
- **Lightweight tag.** `git tag <tag>` (no `-a`/`-m`) creates a lightweight ref with no metadata. Use annotated tags so `git describe` and release notes have something to anchor on.
- **Tagging from a feature branch.** Even if the commit is identical to main's HEAD, the branch context muddies the audit trail. Switch to main first.
- **Tagging on a red main.** The auto-release workflow may publish artifacts before realizing tests are failing. Confirm the last CI on main is green; if red, ask the user before tagging.
- **Re-pushing a deleted tag.** If a tag was previously published and then deleted, the GitHub release may still exist (just untagged). The skill checks `releases/tags/<tag>`; respect the result.
- **Skipping the workflow detection step.** Pushing a tag with no auto-release workflow is a silent no-op — the user will think they shipped, nothing happens. Always confirm a workflow is wired up to the tag pattern.

## When NOT to use this

- The repo has no auto-release workflow → set one up first (model on `zackees/zccache` `.github/workflows/release-auto.yml`).
- The user wants to publish a hotfix from a branch other than main → that's a different flow; do a backport PR or use the workflow's `workflow_dispatch` input directly.
- The user wants to bump the version → that's `/clud-bump` (or a manual edit + commit + this skill). This skill assumes the version is already bumped and committed.
- The release publishes via a non-tag-driven path (e.g. `cargo publish` from CI on every main push) — the skill's pre-flight checks don't apply.

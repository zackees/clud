---
name: clud-review
description: Pre-push code review gate. Inventories the changed files, groups them by language bucket, dispatches one review pass per non-empty bucket to a matching repo subagent or canned per-language template, and aggregates findings. Discovers .coderabbit.yaml, repo review skills/agents, CLAUDE.md/AGENTS.md guidance, and optionally runs the originating issue's focused test for RED -> GREEN validation. Read-only; called by /clud-fix or /clud-pr before `gh pr create`.
triggers:
  - When the user says "/clud-review" or "/clud-review <commit-range>"
  - When /clud-fix or /clud-pr is about to push a PR and the user wants a pre-flight review (called as a delegated step)
  - When the user says "review this before pushing" with an active worktree
---
<!-- managed-by: clud -->

# /clud-review

Pre-push review gate that inventories the worktree's local diff,
classifies the changed files by language family, runs one focused
review per non-empty bucket using the project's actual rules, and
aggregates findings into a single Markdown table. Optionally runs
the originating issue's focused test for local RED -> GREEN validation.

`/clud-review` is the loop step the human reviewer would run if they
could be summoned at every push — same content they'd see, same rules
they'd apply, returned in seconds instead of days.

For code changes, the caller ([[clud-fix]] / [[clud-pr]]) preserves
RED -> GREEN: identify or add the focused failing test/repro first,
implement the scoped change, then rerun that focused signal until it
passes before broad gates. `/clud-review` ITSELF never edits code —
it surfaces findings; the caller's existing fix loop turns them GREEN.
Where the originating issue's focused test is discoverable, the
review explicitly runs it and reports pass/fail (RED -> GREEN
validation at the local level).

## Input

- **Bare** `/clud-review` — review `git diff origin/main...HEAD` from
  the current working directory. The intended invocation point: a
  `/clud-fix` or `/clud-pr` agent runs this as the last step before
  `gh pr create`.
- **`/clud-review <commit-range>`** — review the specified range
  (e.g. `HEAD~3..HEAD`).
- **`/clud-review --issue <N>`** — explicit issue number for the
  issue-test discovery step below; otherwise inferred from branch /
  commit messages.

## Forge support

This skill defaults to GitHub and the `gh` CLI for backwards compatibility. URL inputs from other forges are classified by URL prefix and routed to the matching native CLI. Bare numbers (`#<N>`) without an explicit prefix resolve their forge from the current worktree's `git remote get-url origin`.

### Multi-forge URL recognition

| Forge | URL prefix(es) | Native CLI | Vocabulary |
|---|---|---|---|
| GitHub | `github.com/<o>/<r>/(issues\|pull)/<N>` | `gh` | issue / PR (`#N`) |
| GitLab | `gitlab.com/<g>/<p>/-/(issues\|merge_requests)/<N>` and self-hosted variants | `glab` | issue / **merge request (MR)** (`!N`) |
| Bitbucket | `bitbucket.org/<o>/<r>/(issues\|pull-requests)/<N>` | none official; REST API | issue / PR (`#N`) |
| Gitea | `<host>/<o>/<r>/(issues\|pulls)/<N>` | `tea` | issue / PR (`#N`) |
| Forgejo | `<host>/<o>/<r>/(issues\|pulls)/<N>` (same patterns as Gitea) | `forgejo-cli` (early) or `tea` | issue / PR (`#N`) |
| Self-hosted GitLab / Gitea / Forgejo | same patterns under custom domains | same CLI | same vocabulary |

The classifier returns `{forge, kind, owner, repo, number, host}` for any URL input.

### Bare-number resolution

When the input is a bare `#<N>` or `<N>`:

1. Run `git remote get-url origin` in the current worktree.
2. Match the remote URL against the forge patterns above.
3. Use the resolved forge for the bare-number probe (`gh pr view` for GitHub, `glab mr view` for GitLab, etc.).

### Explicit prefix override

Prefixes in the invocation force a specific forge and skip remote inference: `github:<N>` / `gitlab:<N>` / `bitbucket:<N>` / `gitea:<N>` / `forgejo:<N>`.

### CLI abstraction

All `gh` examples elsewhere in this skill are GitHub-specific. Substitute the matching native CLI per forge:

- `gh issue view <N>` ↔ `glab issue view <N>` ↔ `tea issues show <N>` ↔ Bitbucket REST: `curl ... /repositories/<o>/<r>/issues/<N>`
- `gh pr view <N>` ↔ `glab mr view <N>` ↔ `tea pulls show <N>` ↔ Bitbucket REST: `curl ... /pullrequests/<N>`
- `gh pr merge <N> --squash` ↔ `glab mr merge <N> --squash` ↔ `tea pulls merge <N>` ↔ Bitbucket REST: `PUT /pullrequests/<N>/merge`
- `gh issue create` ↔ `glab issue create` ↔ `tea issues create` ↔ Bitbucket REST: `POST /repositories/<o>/<r>/issues`

### Vocabulary translation

Internal skill logic can keep saying "PR" generically. User-facing output uses the forge's native vocabulary:

- GitHub user sees `PR #123 merged` — unchanged.
- GitLab user sees `MR !123 merged` (note the `!` sigil GitLab uses instead of `#` for MR references).
- Bitbucket / Gitea / Forgejo users see `PR #123 merged`.

Never silently translate vocabulary in error messages — if a GitLab MR is mentioned, the message says `MR !123`, not `PR #123`.

### Auth-token discovery

Each forge has its own auth model:

- **GitHub**: `gh auth status` or `GITHUB_TOKEN` env var (default).
- **GitLab**: `glab auth status` or `GITLAB_TOKEN` / `GL_TOKEN`.
- **Bitbucket**: App password or workspace token (e.g. `BITBUCKET_TOKEN`).
- **Gitea / Forgejo**: per-host token (`GITEA_TOKEN`, `FORGEJO_TOKEN`).

If the required CLI or token is missing, emit a clear refusal and stop:

```
forge-cli-missing: install <cli> to use clud against <forge>
forge-auth-missing: authenticate to <forge> via <cli> auth login
```

Don't log or persist tokens; rely on the user's existing auth.

### Hard rules

1. **No bundled CLIs.** Discover whether `gh` / `glab` / `tea` / etc. is on PATH; refuse if not. Don't bundle tooling.
2. **GitHub stays the path of least resistance.** Users on GitHub see no behavior change. The forge classifier only kicks in when the URL matches a non-GitHub pattern (or the user passes an explicit non-GitHub prefix).
3. **No silent vocabulary translation in error messages.** If a GitLab MR is mentioned, the message says `MR !123`, not `PR #123`.
4. **No cross-forge operations.** Never move an issue between forges, link a PR to an MR, etc. Single forge per invocation.

## Hard Rules

1. **Pre-push only.** `/clud-review` reads the local diff; it does NOT
   open the PR, post comments to GitHub, or modify any source. The
   caller decides whether to act on findings.
2. **Use the repo's rules, not generic ones.** If the repo has a
   `.coderabbit.yaml`, use its settings to bound and prioritize the
   check. If the repo has review skills or subagents, dispatch to them.
   If the repo has `CLAUDE.md` / `AGENTS.md`, include the relevant
   sections. Generic feedback only fires as a *fallback* and is
   labeled as such.
3. **Empty config = no-rules verdict.** If no review config or
   per-language template applies to any bucket, emit
   `clud-review: no-rules` and exit 0. Do not invent feedback.
4. **Read-only.** No file edits, no commits, no pushes.
5. **Structured findings.** Output is a Markdown table (Severity,
   Bucket, File, Line, Finding, Suggested fix) plus a one-line
   `clud-review: <status>` summary.

## Discovery — what to scan, where

### CodeRabbit config

Look at the repo root for, in priority order:

1. `.coderabbit.yaml` (canonical name per CodeRabbit docs)
2. `.coderabbit.yml`

The schema lives at https://coderabbit.ai/integrations/schema.v2.json
(noted for reference; do not fetch). Use these fields to shape the
review per bucket:

- `language` — language-specific rules per file extension; merge into
  the matching bucket prompt.
- `reviews.path_filters` — files to include / exclude; apply BEFORE
  bucketing so excluded paths never reach a review pass.
- `reviews.path_instructions` — per-glob review instructions; merge
  into the prompt of whichever bucket the glob matches.
- `reviews.tools` — static-analysis tools the user expects; mention
  them in the prompt so the agent can lean on their conventions.
- `chat.*` — irrelevant to a local review; ignore.

### Repo-level review skills

Scan:

- `.claude/skills/*/SKILL.md`
- `.codex/skills/*/SKILL.md`
- `skills/*/SKILL.md` (when the project keeps skills at the repo root)

Read each frontmatter. Include the skill when `name` or `description`
matches `/review|code\s*review/i`. Include the full `SKILL.md`
content in the matching bucket's prompt — when the skill is
language-scoped (e.g. its description mentions `rust` or `frontend`),
attach it only to that bucket; otherwise attach it to every bucket.

### Repo-level review subagents

Scan:

- `.claude/agents/*.md`

Read each frontmatter `description`. Include the subagent when it
matches `/review|code\s*review/i`. Subagents are *executed* (via the
Agent tool) when a bucket matches them — see "Per-bucket agent
selection" below.

### Guidance files

Scan:

- `CLAUDE.md` at the repo root
- `AGENTS.md` at the repo root (if present)
- Per-directory `CLAUDE.md` for directories that have changes in the
  diff under review.

Extract sections matching `/code\s*(review|quality|standards?)/i` or
that read as rule-bullets (`must` / `should` / `never`). Attach to the
prompt of every bucket whose files live under the matching directory.

### Explicit skip rules

DO NOT scan:

- `~/.claude/` or `~/.codex/` — those are the agent's own user-level
  configuration, not project rules.
- `~/.claude/rules/` — same reason (`code-review.md`, `security.md`,
  `git-workflow.md` typically live there but encode the human user's
  preferences, not the repo's).
- Third-party submodules.
- `target/`, `node_modules/`, `dist/`, or any other build artifact.

## File classification and per-language review dispatch

`/clud-review` does NOT generate one giant cross-language prompt. It
first inventories the changed files via
`git diff --name-status <base>...HEAD` (fallback:
`git status --porcelain` when no base is given), groups them by
language family, and runs one review pass per non-empty bucket.

### Bucket table

| Bucket | File extensions / names | Rationale |
|---|---|---|
| `rust` | `.rs`, `Cargo.toml`, `Cargo.lock` | Rust source + crate manifests |
| `cpp` | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.h++` | C++ source/header |
| `c` | `.c`, `.h` (when no `.cpp`/`.hpp` sibling) | Pure C — header `.h` alone defaults here; if a same-stem `.cpp` is in the diff, both go to `cpp` |
| `python` | `.py`, `pyproject.toml`, `setup.py`, `requirements*.txt` | Python source + packaging |
| `frontend` | `.html`, `.htm`, `.js`, `.jsx`, `.ts`, `.tsx`, `.css`, `.scss`, `.less`, `.vue`, `.svelte`, `package.json`, `tsconfig*.json` | Browser/Node UI stack — reviewed together because changes typically cross files |
| `go` | `.go`, `go.mod`, `go.sum` | Go source + modules |
| `shell` | `.sh`, `.bash`, `.zsh`, `.ps1`, `.fish` | Shell scripts |
| `ci` | files under `.github/workflows/`, `.gitlab-ci.yml`, `Jenkinsfile`, `.circleci/` | CI/CD configuration |
| `config` | `.yaml`, `.yml`, `.toml`, `.json`, `.ini`, `.env*` (when NOT already claimed above) | Pure config that didn't claim a language |
| `docs` | `.md`, `.rst`, `.txt`, `.adoc` | Documentation and changelogs |
| `other` | everything else | Unclassified — reviewed with a generic prompt and a "best effort" disclaimer |

Bucket precedence is top-to-bottom: a file matches the **first** bucket
whose rule fires. So `Cargo.toml` is `rust` (not `config`), and
`.github/workflows/foo.yml` is `ci` (not `config`).

### Per-bucket agent selection

For each non-empty bucket, in priority order:

1. **Repo subagent match.** Scan `.claude/agents/*.md`. Match any
   agent whose frontmatter `description` mentions the bucket name or
   its primary language tooling (e.g. `rust` / `cargo` / `clippy` →
   rust bucket; `frontend` / `react` / `tsx` / `css` → frontend
   bucket; `cpp` / `c++` / `cmake` → cpp bucket). If a match exists,
   dispatch the bucket's review to that subagent via the Agent tool
   with the assembled bucket prompt.
2. **Repo skill match.** Otherwise, look for review skills whose
   frontmatter narrows them to that language. Inline their
   `SKILL.md` into the bucket's prompt as additional rules and use the
   general agent.
3. **Canned per-language template.** If neither exists, use the canned
   template for that bucket — a short language-specific review prompt
   so the fallback isn't generic-shaped feedback. The canned templates
   live in this skill's prose; representative examples:
   - **rust**: idiomatic borrow semantics; `Result` over panics;
     no `unwrap()` / `expect()` in non-test code; clippy `pedantic`
     guidance; `unsafe` blocks must be justified with a `// SAFETY:`
     comment.
   - **cpp**: RAII; no raw `new` / `delete`; smart pointer ownership;
     `const`-correctness; no implicit narrowing conversions.
   - **python**: type hints; explicit exception handling; no
     mutable default arguments; `pathlib` over string paths.
   - **frontend**: accessibility (semantic HTML, ARIA); strict null
     checks in TS; effect dependency arrays in React hooks; CSS
     specificity; no inline event handlers in HTML.
   - **shell**: `set -euo pipefail`; quoted variable expansions;
     shellcheck-style rules.
   - **ci**: pinned action versions; least-privilege tokens; secret
     handling; matrix completeness.
   - **config**: schema validity; no committed secrets; comment
     intent on non-obvious values.
   - **docs**: link integrity; example correctness; clear voice.
   - **c**, **go**, **other**: brief generic guidance.

### Per-bucket prompt structure

Each bucket's assembled prompt is:

```text
You are reviewing local changes to <bucket-name> files before they
get pushed as a PR.

## Files in this bucket
<list of <bucket>-bucket file paths>

## Diff (filtered to bucket files)
git diff <base>...HEAD -- <bucket-paths>

## Repo-level rules (applicable to this bucket)
- From .coderabbit.yaml `language.<bucket>` or
  `reviews.path_instructions` matching <bucket-paths>
- From any matched repo subagent (.claude/agents/<name>.md)
- From any matched review skill (.claude/skills/<name>/SKILL.md)
- From CLAUDE.md sections referencing <bucket-name> rules
- From the canned per-language template (only when none of the above
  matched)

## Output format
Markdown table:
| Severity | File | Line | Finding | Suggested fix |
Severity ∈ {CRITICAL, HIGH, MEDIUM, LOW}.
```

### Aggregation across buckets

After all bucket reviews return, `/clud-review` aggregates them into a
single Markdown findings table with a `Bucket` column and a verdict
footer that breaks counts down per-bucket:

```text
clud-review: findings (3 CRITICAL [rust:2, frontend:1], 2 HIGH [rust:1, ci:1])
```

Empty buckets contribute nothing. If every bucket returns clean, the
verdict is `clud-review: clean`. If no bucket had any applicable rules
(no `.coderabbit.yaml`, no matched skills/agents, no relevant
`CLAUDE.md` sections, no canned template fired because the bucket was
`other`), the verdict is `clud-review: no-rules`.

### Bucket skip rules

- Empty bucket → skip entirely (no prompt, no agent call).
- `docs` bucket → only review if the docs change touches public
  user-facing surfaces (e.g. `README.md`, `docs/`, `CHANGELOG.md`).
  Skip per-directory `README.md` updates that mirror code-side
  changes; those are documentation hygiene, not review-worthy.
- `config` bucket with only formatting changes (whitespace, comment
  edits) → skip with a `clud-review: skipped config formatting` note.

## Issue-test discovery and execution

If `/clud-review` was invoked from `/clud-fix` or `/clud-pr` (or from a
worktree whose branch name encodes an issue number), it additionally
tries to find a focused test that **covers the originating issue** and
run it. Intent: RED -> GREEN validation at the local level — before
pushing, prove the issue-specific test passes.

### Discovering the issue number

In priority order, take the first hit:

1. **Caller-supplied** — `/clud-review --issue <N>` from the parent
   skill's delegated call.
2. **Branch name** — if the current branch matches `feat/...-pr<N>` or
   `fix/issue-<N>-*` or similar, extract `<N>`.
3. **Commit message** — scan `git log <base>..HEAD` for `Closes #<N>`,
   `Fixes #<N>`, `Resolves #<N>`, or a `#<N>` in the first commit
   subject.
4. **Diff content** — scan added test files for a literal `#<N>` in a
   comment or docstring near a test definition.

If no issue number resolves, skip this section; review the diff
without an issue-test step.

### Finding tests that cover the issue

Once `<N>` is known, search across the standard test paths for tests
that reference it:

- **Rust**: `grep -rn "issue.*<N>\|#<N>" crates/*/tests/ crates/*/src/`
  matching `#[test]` items.
- **Python**: `grep -rn "issue.*<N>\|#<N>" tests/` matching
  `def test_*`.
- **JS/TS**: `grep -rn "issue.*<N>\|#<N>" __tests__/ test/ spec/`
  matching `it(` / `test(` / `describe(`.

Rank candidates: tests added in the current diff > tests with the
issue number in their name > tests with the issue number in their
docstring or comment > tests in files the diff touched.

If multiple candidates rank equal, run all of them. If none match, log
`clud-review: no issue-specific test found for #<N>` and skip.

### Running the test

Use the project's standard test runner, inferred from the file
extension and the repo's CI script (`ci/test*.sh`, `bash test`,
`scripts/test`, etc.):

- Rust: `soldr cargo test -p <crate> --lib <test-name>` or
  `--test <file>`.
- Python: `pytest tests/ -k <test-name>`.
- JS/TS: `npm test -- -t <test-name>` or `pnpm test -t <test-name>`.

Run with output capture. Two outcomes:

- **Test passes**: include `clud-review: issue-test #<N> passed
  (<test-name>)` in the summary; review verdict is unaffected by this
  alone (test pass is necessary but not sufficient for a clean review).
- **Test fails**: this is a CRITICAL finding. The diff failed to make
  the issue test green. Surface the failure output as the finding body;
  the verdict becomes `clud-review: findings (1 CRITICAL, ...)`.

### Don't run anything else

`/clud-review` does NOT run the full test suite — that's `bash test`'s
job. It runs *one* focused test (or a small set) that targets the
issue, then stops. Full-suite runs belong in the parent skill's
existing lint/test step.

## Invocation modes

### Bare `/clud-review`

Reviews `git diff origin/main...HEAD` from the current working
directory. The intended use: a `/clud-fix` or `/clud-pr` agent
invokes this as the last step before `gh pr create`. If findings are
non-empty (CRITICAL or HIGH), the caller should fix them and
re-review before pushing.

### `/clud-review <commit-range>`

Reviews the specified range (e.g. `HEAD~3..HEAD`). Useful for
reviewing a specific subset of work without pushing.

### Delegated from `/clud-fix` or `/clud-pr`

The two parent skills already have a "lint and test" step before push.
`/clud-review` slots in between "tests pass" and `gh pr create`:

```text
6. Lint and test.
6a. /clud-review (NEW STEP). If findings non-empty, address them,
    re-lint, re-test, re-review. Loop until clean or until 3 review
    cycles complete, then either push (clean) or surface the findings
    and stop (not clean after 3 cycles).
7. Clean tree gate.
```

Wiring `/clud-review` into `/clud-fix` and `/clud-pr` is OUT OF SCOPE
for this skill's introduction — that's a follow-up PR. The SKILL.md
documents the delegated mode for forward compatibility.

## Generated prompt structure

The skill assembles **one prompt per non-empty bucket** (see "File
classification and per-language review dispatch" above). The
single-prompt shape described in "Per-bucket prompt structure" is the
full template; the aggregation step described in "Aggregation across
buckets" combines results into the user-visible output.

## Failure Modes To Avoid

- **Single cross-language prompt.** Don't lump rust + frontend + ci
  into one review; the rules are too different and the agent's
  attention fragments. Bucket first, then review.
- **Generic fallback masquerading as project rules.** When no repo
  config matches, the canned per-language template fires — but the
  output should clearly indicate it was a fallback so the user knows
  to add `.coderabbit.yaml` or a review skill if they want richer
  feedback.
- **Reviewing build artifacts.** `target/`, `node_modules/`, `dist/`,
  build outputs from generated code (e.g. prost-generated `.rs` files)
  should be excluded. Honor `.gitignore` and `.coderabbit.yaml` path
  filters.
- **Blocking the push on advisory findings.** Only CRITICAL and HIGH
  findings should block; MEDIUM and LOW are advisory. The caller's
  fix loop decides whether to address them now or in a follow-up.
- **Mutating the repo.** `/clud-review` is read-only. No `git commit`,
  no `git add`, no edits. Even fixing typos in CLAUDE.md is out of
  scope.
- **Calling external services.** No network calls. No CodeRabbit
  remote API, no `curl` of the schema URL. The local rules are the
  source of truth.
- **Issue-test discovery false positives.** A grep for `#<N>` can
  match unrelated test names or comments. Require the match to be
  near a `#[test]` / `def test_` / `it(` definition, not just anywhere
  in the file.

## When Not To Use This

- The user wants a post-PR / post-merge review of someone else's work
  — that's [[clud-pr]] PR triage mode (look for CodeRabbit findings on
  the existing PR) plus the user's own judgment.
- The user wants to file new issues from CodeRabbit findings on an
  already-merged PR — that's [[clud-issue-triage]].
- The user wants the FULL test suite run — that's `bash test` or the
  parent skill's lint/test step, not the focused issue-test runner here.
- The repo has no review configuration of any kind AND the user does
  not want generic feedback — `/clud-review` will emit
  `clud-review: no-rules` and exit; respect that as the answer.

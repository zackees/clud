---
name: grind-intake
description: "Turn a /grind argument (meta issue URL, issue numbers or URLs, free-form goal, or nothing for the repo's issues page) into an ordered goal list."
triggers:
  - When /grind needs its goal list
  - When the user asks which goals a meta issue or issue list expands to
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-intake

Use the prompt's **GitHub repository** (`OWNER/REPO`) as `<github-repo>`. Keep the checkout path separate. For direct use, resolve an explicit GitHub issue/PR URL or `git remote get-url origin` first; report an unresolved identity before running repository commands.

Produces the ordered goal list the `/grind` router passes to the workflow:
`[{id, title, brief}]`, plus the meta issue number (`meta`), the converted
issue (`original`, only after a conversion), the repository root, the resolved
GitHub repository (`github_repo`, `OWNER/REPO`) and default branch. It
changes no code, so the RED -> GREEN rule applies later, in
`/grind-integrate`.

## Sources

**Resolve repository identity first.** Take `OWNER/REPO` from the user's
explicit GitHub issue/PR/issues-page URL; for bare numbers, lists or prompts,
read `git remote get-url origin`. Recognize GitHub HTTPS, SSH and
`git@github.com:OWNER/REPO.git` URLs, removing the transport suffix `.git`.
Keep this `<github-repo>` separate from the local checkout path. Report an
unresolved or conflicting identity before creating or attaching anything.
Pass `--repo <github-repo>` to repository-scoped `gh` commands and bundled
GitHub tools; bind `{owner}/{repo}` in API endpoints to that same identity.
Do not infer it from `gh repo view` or a fork's default upstream selection.

`/grind` always runs on a meta issue. Route the argument in this order:

1. **Normalize.** `https://github.com/<o>/<r>/issues/N` and a bare `N` route
   identically: pass the URL (or `N`) to the tool as given, and use `N`
   everywhere after that.
2. **One issue.** Run `clud tool run github/is_meta_issue.py -- <N> --repo <github-repo>` (a plain
   `clud`: clud's hook refuses a `"$CLUD_EXE"` program word). It
   prints JSON `{"meta": bool, "sub_issues": [{number, state}],
   "task_list_refs": [...]}`; exit 0 answered, 1 usage error, 2 a `gh` call
   failed (nothing on stdout). The tool alone decides meta-ness; never
   re-derive it from the body. On exit 1 or 2, report the tool's error
   verbatim and stop: do not guess "not meta", create nothing.
   - **meta true:** each open child is a goal. `sub_issues` carry their
     state; a `task_list_refs` entry does not, so read its state with
     `gh issue view --repo <github-repo> <ref> --json state` (a ticked `- [x]` box is not
     "closed"). No conversion question. If every child is closed, print
     `Nothing to do: every sub-issue of #N is closed.` and stop, creating
     nothing.
   - **meta false, multi-part** (your only judgment: the body and comments
     hold several independent deliverables, e.g. separate sections each with
     its own acceptance criteria, numbered parts touching different
     subsystems, "also..." / "and separately...", or a phased PR split): ask
     ONE AskUserQuestion, `Convert #N into a meta issue?`, whose question
     text lists the proposed children (title and one-line scope each), with
     the options **Convert to a meta issue** and **Abort**.
     - **Convert:** first create every child, one per part: `gh issue
       create --repo <github-repo>` with a body that quotes that part of #N (markdown `>` quote)
       and says `Split from #N`. Only when every child exists, create the
       meta issue: its body starts with `Tracks #N` and links each child.
       Attach each child as a native sub-issue (see *Attach and verify*).
       Then `gh issue comment --repo <github-repo> N --body "Split into meta #<meta>; closes when
       #<meta> closes."`. Do not close #N. Return `original: N`; the run
       proceeds on the new meta issue.
     - **Abort:** print the refusal below and stop.
   - **meta false, single change:** no question and no refusal. Wrap it in a
     one-child plan exactly as step 4 does ("One cohesive change is still one
     child under a meta"): create a meta issue whose body is `Tracks #N`,
     attach #N as its only native sub-issue (see *Attach and verify*), then
     `gh issue comment --repo <github-repo> N --body "Wrapped in meta #<meta>;
     closes when #<meta> closes."`. Print `Wrapped #N in meta #M` to the user.
     Return `original: N`; the run continues the normal bug stage with #N as
     the single goal. Create no new child issue, edit nothing else on #N, and
     do not close it.

   The refusal below applies only to **Abort** on a multi-part issue. It
   creates nothing (no issues, comments, labels, run facts, worktrees or
   branches), with `N` filled in verbatim:

   > `clud grind` works on meta issues. #N has several independent parts and
   > you chose not to convert it; run `/do N` (or `clud do N`) on one part
   > at a time instead.
3. **Issue list** (two or more numbers/URLs, space or comma separated).
   Create a meta issue whose body is `Tracks #a, #b, ...` listing the given
   issues, and attach each given issue as a sub-issue. Create no new child
   issues. Goals are the open given issues.
4. **Free-form prompt.** Split it into independent deliverables. Create one
   child issue per deliverable first; only when all exist, create the meta
   issue (`Tracks` list of the children) and attach them. Goals are the
   children. One cohesive change is still one child under a meta.
5. **Nothing.** List the repo's open issues (`gh issue list --repo <github-repo>`), excluding
   ones with an open linked PR (an issue whose fix sits on a grind feature
   branch is linked by the feature PR's `Closes` line), ask the user which
   to take, then route the chosen set via 2 (one issue) or 3 (several). Single
   issues are valid candidates: never stop because none is a meta issue.
   A follow-up (its body carries `<!-- grind:followup ... -->`) is eligible
   only when either:
   - its body marker `<!-- grind:followup ... stage=bugs ... -->` names the
     bugs stage, or
   - the marker's `feature-pr=<n>` PR is merged
     (`gh pr view --repo <github-repo> <n> --json state -q .state` prints `MERGED`).
   Otherwise leave it out. Grind never labels an issue (#1958). Follow-ups are never
   sub-issues of the meta issue and do not count for the no-overlap check.

`/grind` never closes the meta issue (or any issue) itself. Issues close
only through a merged PR's `Closes` line into the default branch (for the
feature stage, the feature PR), through `clud grind reconcile` (after a
feature PR merged into the default branch, or for the top meta issue of a
meta of metas once every sub-issue has landed), or by the user. GitHub never
closes a parent issue on its own when its sub-issues close.

### Attach and verify

Follow `/clud-issue`'s hierarchy mode. Get each child's numeric `id` with
`gh api repos/{owner}/{repo}/issues/<child> --jq .id` (the API takes the id,
not the issue number), then
`gh api -X POST repos/{owner}/{repo}/issues/<meta>/sub_issues -F sub_issue_id=<id>`.
A given issue that already has a different parent is a conflict: report it
and stop; never set `replace_parent` here. Verify both ways:
`gh api repos/{owner}/{repo}/issues/<meta>/sub_issues` lists every child,
and each child's `gh api repos/{owner}/{repo}/issues/<child>/parent` names
the meta issue.

## Failure mid-creation

If any create, attach or verify step fails (e.g. child 2 of 3), stop. Report
the failure plus the partial state: every issue number created and every
attachment made so far. Do not proceed to the run and do not retry silently.
Because children are created before the meta issue, a failed child never
leaves a meta issue behind without its children.

## Each goal

- `id`: the issue number, or `g1`, `g2`, … for free-form goals.
- `title`: the issue title or a short name.
- `brief`: the issue URL (agents read it themselves) or a self-contained
  description of the deliverable with its acceptance criteria.

Order goals so that anything another goal builds on comes first; the
planner makes the dependencies explicit.

Resolve the repository root with `git rev-parse --show-toplevel` and the
default branch from `origin/HEAD` (`main` or `master`).

## Repo-state facts

For the router's preflight (`/grind` section 1c), intake also returns, read
only: the starting branch, the `git status --porcelain` output, the stash
list, and how far the checkout is ahead of or behind `origin/<main>`. It
asks nothing about them; its only question stays the conversion (or pick)
question above.

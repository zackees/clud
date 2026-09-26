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

Produces the ordered goal list the `/grind` router passes to the workflow:
`[{id, title, brief}]`, plus the meta issue number (`meta`), the repository
root and default branch. It
changes no code, so the RED -> GREEN rule applies later, in
`/grind-integrate`.

## Sources

`/grind` always runs on a meta issue. Route the argument in this order:

1. **Normalize.** `https://github.com/<o>/<r>/issues/N` and a bare `N` route
   identically.
2. **One issue.** Run `"$CLUD_EXE" tool run github/is_meta_issue.py <N>`. It
   prints JSON `{"meta": bool, "sub_issues": [{number, state}], ...}` plus
   task-list refs; exit 0 answered, 1 usage error, 2 a `gh` call failed. The
   tool alone decides meta-ness; never re-derive it from the body. On exit 1
   or 2, report the tool's error verbatim and stop; do not guess, create
   nothing.
   - **meta true:** each open child (sub-issue or task-list ref) is a goal.
     No conversion question. If every child is closed, print
     `Nothing to do: every sub-issue of #N is closed.` and stop, creating
     nothing.
   - **meta false, multi-part** (your only judgment: the body holds several
     independent deliverables): ask ONE AskUserQuestion
     `Convert #N into a meta issue?` (Convert / Don't convert).
     - **Convert:** per part, `gh issue create` a child whose body quotes
       that part of #N (markdown `>` quote) and says `Split from #N`. Create a
       meta issue whose body starts with `Tracks #N` and lists the children.
       Attach each child as a native sub-issue (see *Attach and verify*).
       Then `gh issue comment N` with `Split into meta #<meta>: #a, #b, #c`.
       Do not close #N. The run proceeds on the new meta issue.
     - **Don't convert:** print ``clud grind works on meta issues. #N is a
       single change; run `/do N` `` and stop: no issues, comments, labels or
       worktrees.
   - **meta false, single change:** no question; print the same
     ``run `/do N` `` refusal and stop, creating nothing.
3. **Issue list** (two or more numbers/URLs, space or comma separated).
   Create a meta issue whose body is `Tracks #a, #b, ...` listing the given
   issues, and attach each given issue as a sub-issue. Create no new child
   issues. Goals are the open given issues.
4. **Free-form prompt.** Split it into independent deliverables, create one
   child issue per deliverable, create a meta issue (`Tracks` list of the
   children), and attach them. Goals are the children. One cohesive change
   is still one child under a meta.
5. **Nothing.** List the repo's open issues (`gh issue list`), excluding
   ones with an open linked PR and ones labelled `grind:on-feature`
   (`gh issue list --search '-label:grind:on-feature'`), ask the user which
   to take, then route the chosen set via 3 or 4.
   A `grind:followup` issue is eligible only when either:
   - its body marker `<!-- grind:followup ... stage=bugs ... -->` names the
     bugs stage, or
   - the marker's `feature-pr=<n>` PR is merged
     (`gh pr view <n> --json state -q .state` prints `MERGED`).
   Otherwise leave it out. When a follow-up is picked up, remove its label
   (`gh issue edit <n> --remove-label grind:followup`). Follow-ups are never
   sub-issues of the meta issue and do not count for the no-overlap check.

`/grind` never closes the meta issue (or any issue) itself. Issues close
only through a merged PR's `Closes` line into the default branch (for the
feature stage, the feature PR), through GitHub's sub-issue tracking, or by
the user.

### Attach and verify

Get the child's numeric `id` from `gh api repos/{owner}/{repo}/issues/<child>`,
then `gh api -X POST repos/{owner}/{repo}/issues/<meta>/sub_issues -F
sub_issue_id=<id>`. Verify both ways: `gh api
repos/{owner}/{repo}/issues/<meta>/sub_issues` lists every child, and each
child's `gh api repos/{owner}/{repo}/issues/<child>` shows the parent.

## Failure mid-creation

If any create, attach or verify step fails (e.g. child 2 of 3), stop. Report
the failure plus the partial state: every issue number created and every
attachment made so far. Do not proceed to the run and do not retry silently.

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
only: the starting branch, the `git status --porcelain` output, and how far
the checkout is ahead of or behind `origin/<main>`. It asks nothing about
them; its only question stays the conversion (or pick) question above.

---
name: do
description: "Implement one issue URL or free-form goal all the way to merged PRs: RED -> GREEN, review, test, push, watch CI to green, merge, and leave a clean checkout rebased to where you started. Seeded by `clud do` as `/goal /do <target>`."
triggers:
  - When clud do seeds /goal /do <target>
  - When the user asks to implement an issue or goal through to a merged PR
---
<!-- managed-by: clud -->

# /do

`clud do <target>` seeds `/goal /do <target>`: `/goal` keeps the session going
until the contract below is met, and this skill is the contract. A meta issue
(one with open sub-issues) never reaches here; `clud do` seeds `/grind` for it
instead.

Every code change keeps a RED -> GREEN focused regression: first show the
failure or reproduction, then make that signal pass before the broader
repository gates.

## When the target is an issue URL

Read the issue and implement it. Record the starting branch.

If the issue turns out to have several independent children after all, use
`/grind` to delegate them. Each child needs its own branch and one or more
PRs; never combine children in one PR. Only a child's final PR closes it, and
child PRs must not close the parent. Record child → PR links, then close the
parent after all children are resolved.

For every PR: show RED -> GREEN, review, test, push, watch CI to green, and
merge. Merge separately, and update remaining branches as needed. Follow the
repository's worktree rules, return to the starting branch, and leave a clean
status.

The goal is satisfied when all PRs are merged and all referenced issues are
closed as complete. No cheating. No files left behind. Rebase to origin main
or master when done.

## When the target is a free-form goal

The goal is resolved when the requested work lands in one or more PRs, each
validated, tested, pushed and merged. Wait for the PR's GitHub Actions to go
green, then merge it; add a watch. No cheating, no files left behind.

All work must be done for this repository. Use a git worktree or sibling
checkout only when `/grind` and the repository's guidance allow it; work can
only land here. Find out the starting branch right now. When you are done,
run `git status` and make sure it's clean, and make sure the local repo is
rebased to the branch you started from.

If the goal contains several independent deliverables, invoke `/grind` to
delegate them; otherwise keep the normal `/goal` workflow.

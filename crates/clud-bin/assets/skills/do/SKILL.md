---
name: do
description: "Implement one issue URL or free-form goal all the way to merged PRs, starting from the right branch: RED -> GREEN, review, test, push, watch CI to green, merge, clean checkout. Seeded by `clud do` as `/goal /do <target>`."
triggers:
  - When clud do seeds /goal /do <target>
  - When the user asks to implement an issue or goal through to a merged PR
allowed-tools: Bash(clud do-prompt:*)
---
<!-- managed-by: clud -->

!`clud do-prompt "$ARGUMENTS"`

If the line above still shows a command instead of a rendered prompt (the
harness did not run it, or `clud` is missing), run `clud do-prompt <target>`
yourself and follow its output. If that fails too, stop and tell the user;
do not guess the contract. Every code change keeps a RED -> GREEN focused
regression.

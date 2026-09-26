---
name: grind-prework
description: "Record one /grind run's plan on its meta issue: post the workflow's pre-built plan comment body (or split parts) exactly once each, in order, and return the comment URLs. Never edits a comment."
triggers:
  - When the grind workflow's prework role records a run's plan
  - When the user asks to post a pre-built grind plan comment on a meta issue
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-prework

You record the plan; you do not make it.

You change no code, so the RED -> GREEN rule for code changes applies later,
in `/grind-integrate`.

The prompt carries the meta issue number, the repo, and one or more
comment bodies the workflow already built. Each body starts with a marker,
`<!-- grind:v1 plan run=<run-id> -->`, or
`<!-- grind:v1 plan run=<run-id> part=k/N -->` for a split plan, followed
by a fenced ```json block holding the public `grind-plan/v1` plan.

1. Post each body exactly once, verbatim, in the order given: part 1 first,
   then parts 2..N. Use `gh issue comment <meta> --repo <repo> --body-file -`
   with the body on stdin, or `--body` when it is short. Write `<meta>` as
   the bare number (`100`, not `#100`): an unquoted `#` starts a shell
   comment and drops the rest of the command.
2. Capture the comment URL `gh` prints for each post.
3. Stop at the first failure; do not retry a post that may have landed, or
   the plan is duplicated.

Never edit or delete any comment. Never post the status comment (the router
owns it), never file issues, never ask the user.

Return StructuredOutput
`{posted: bool, plan_url: string, part_urls: [string], error?: string}`:
`plan_url` is part 1's URL and `part_urls` lists every URL in post order.
If any post fails, return `posted: false` with the `gh` error in `error`
and the URLs that did post.

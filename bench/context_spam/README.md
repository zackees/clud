# Context spam scan

Read-only diagnostic for #2312 / #2315: how much Bash tool output lands in
local Claude Code transcripts, and how much of a repeated command's output is
identical to its previous result (CI polling).

From the repository root:

```text
python -m bench.context_spam.scan families               # calls and MB per command family
python -m bench.context_spam.scan polls                  # gh polling redundancy
python -m bench.context_spam.scan polls --since 2026-10-30   # only sessions after a change
```

Paths default to `~/.claude/projects`; pass one project directory to narrow
the scan. `polls` prints truncated command text, so keep its output local.

Baseline (2026-10-29, all projects): `gh pr checks` streaks of x75–x152 with
95–97% of each repeat identical to the previous result; `gh run view` x93 at
81%. #2312's skill text (#2329) should shrink these. Re-run `polls --since`
with a date after it merged and compare.

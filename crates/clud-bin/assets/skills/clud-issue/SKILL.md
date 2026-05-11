<!-- managed-by: clud -->
---
name: clud-issue
description: File a deeply-researched GitHub issue via investigate → interview → investigate → post, returning a summary plus the issue URL.
triggers:
  - When the user types "/clud-issue" with a topic or problem statement
  - When the user asks to "file an issue with research" or "open an issue after investigating"
  - When the user wants to draft an issue but needs the agent to clarify scope first
---

# /clud-issue

File a GitHub issue informed by real research and a real conversation. Four hard rules:

1. **Two investigation rounds, one interview in between** — never skip the interview, never post after a single pass.
2. **Post the issue** — finish with `gh issue create`. The deliverable is an issue URL, not a draft in chat.
3. **Surface strong duplicates only** — search existing issues; mention related issues *only* when similarity is strong (same component + overlapping intent). Don't pad the report with weak matches.
4. **Summary + URL last.** End with a short summary of what was filed and the URL — nothing else.

## Workflow

1. **Round 1 investigation (silent prep).** Read the topic. Skim the relevant code paths, existing docs, and recent git history to form a working model: what the user likely means, what's already in place, what's missing, what the obvious unknowns are. One round — don't spiral.
2. **Interview mode.** Ask the user clarifying questions to nail scope, constraints, and acceptance criteria. For each question you can answer well from the code or from public knowledge, *offer your best-judgement answer* alongside the question and ask the user to confirm or override. Batch related questions; don't drip-feed. Keep going until ambiguity is resolved — don't post a vague issue.
3. **Round 2 investigation.** With the user's answers in hand, do the deeper dig: confirm file paths, identify touch points, note prior art, list risks/edge cases. This round informs the actual issue body.
4. **Search for duplicates.** `gh issue list --search "<keywords>" --state all` (open + closed). Only flag issues with strong similarity — same component *and* overlapping intent. Weak keyword matches don't count.
5. **Draft the issue.** Title in conventional style (`feat:`, `fix:`, `chore:`, etc.). Body sections: **Context**, **Proposal**, **Acceptance criteria**, **Open questions** (if any remain), **Related issues** (only if strong matches found). No filler.
6. **Post.** `gh issue create --title "..." --body "$(cat <<'EOF' ... EOF)"`. Use a heredoc so formatting survives.
7. **Report.** Give the user: a 2-3 sentence summary of what was filed, then the issue URL. If strong related issues exist, mention them in one line above the URL. Nothing else.

## Failure modes to avoid

- **Skipping the interview.** Even "obvious" topics have hidden constraints. The interview is mandatory.
- **Posting before round 2.** Round 1 is for forming questions. Round 2 is for forming the issue. Don't conflate them.
- **Padding "Related issues" with weak matches.** A keyword overlap isn't a related issue. Only flag genuine overlap.
- **Vague acceptance criteria.** If the issue can't be closed objectively, the criteria are wrong. Rewrite them.
- **Posting without confirming.** Show the user the drafted title + body before `gh issue create` and let them adjust.

## When NOT to use this

- The user already has a clear, well-scoped issue draft — just file it directly.
- The work is trivial enough to implement immediately (skip the issue, do `/clud-pr` style work).
- The topic is a question, not a request for tracked work — answer it instead of filing.

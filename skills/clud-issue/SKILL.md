<!-- managed-by: clud -->
---
name: clud-issue
description: File a researched GitHub issue without interrogating the user — investigate, decide on best-guess defaults, post, and list those defaults in the issue body so the user can edit on GitHub.
triggers:
  - When the user types "/clud-issue" with a topic or problem statement
  - When the user asks to file or open a GitHub issue
---

# /clud-issue

File a researched GitHub issue and ship it. Four hard rules:

1. **Default to action, not interrogation.** Do the research, make best-guess decisions for ambiguity, post the issue. Never paraphrase the user's clear request as ambiguous.
2. **Question budget: 0 by default, 1 only if blocking.** Ask iff: without the answer, you cannot file *any* sensible issue (e.g., the user genuinely didn't name a repo or topic). Priority, scope edges, exact wording, fix location → not blocking. Decide and document.
3. **Decisions go in the issue body, not the chat.** Any judgment calls land under a `## Decisions` section in the issue body so the user can edit on GitHub. Never ratify-by-chat.
4. **Post the issue.** Finish with `gh issue create`. Deliverable is the issue URL.

## Workflow

1. **Read the prompt at face value.** If the user named the repo, that's the repo. If they named the bug, that's the bug. Do NOT re-derive specifics the user already gave you.
2. **Investigate (one pass, silent).** Skim relevant code, recent git history, existing issues. Build the working model internally — don't narrate.
3. **Search for strong duplicates.** `gh issue list --repo <repo> --search "<keywords>" --state all`. Strong overlap only (same component + overlapping intent). Weak keyword matches don't count.
4. **Decide on defaults.** For each judgment call (priority, severity, scope edges, fix-location guess, acceptance criteria) pick a sensible default. Each default gets a one-line justification in the issue body's `Decisions` section.
5. **At most one blocking question.** Ask only if step 1 left you genuinely unable to file. Skip otherwise.
6. **Draft.** Conventional title (`feat:`, `fix:`, `chore:` ...). Body sections:
   - **Context** — observation, what research found.
   - **Proposal** — what should change.
   - **Acceptance criteria** — concrete, objectively closable bullets.
   - **Decisions** — your defaults, one line each (e.g. *Priority: P2 — migration unblocked by GET-fallback*). User edits this on GitHub if they disagree.
   - **Related issues** — only if strong duplicates were found.
7. **Post.** `gh issue create --repo <repo> --title "..." --body "$(cat <<'EOF' ... EOF)"`. Heredoc preserves formatting.
8. **Report.** Two lines: one-sentence summary of what was filed, then the URL. If strong duplicates exist, one extra line above the URL listing them. Nothing else.

## What counts as a blocking question

**Blocking (ask):**
- User said "file an issue" with no topic AND no repo.
- User named two repos and the right one isn't derivable from context.
- Topic is genuinely ambiguous between bug / feature / question and the framing materially changes the body.

**Not blocking (decide, document, ship):**
- Priority / severity → pick P2 by default; downgrade if non-urgent, upgrade if blocking shipping work.
- Acceptance criteria wording → write them; user edits on GitHub.
- Where the fix should live → state your guess in the body; triage can re-route.
- Whether to mention a related issue → use the strong-duplicates filter.

## Failure modes to avoid

- **Asking what the user already told you.** If the prompt names the repo / topic / scope, don't ask "where should this go?" — they answered.
- **"Best guess + please confirm" anti-pattern.** If you have a best guess, ship it. The Decisions section documents it. Don't make the user ratify each call in chat.
- **Padding "Related issues" with weak keyword matches.** Strong overlap only.
- **Posting a draft to chat for approval.** Chat report is summary + URL. Drafts go straight to `gh issue create`.
- **Vague acceptance criteria.** If the issue can't be objectively closed, rewrite them before posting.

## When NOT to use this

- User pasted a complete issue draft — just `gh issue create` it.
- Work is trivial enough to do directly — `/clud-pr` style is faster than tracking it.
- Topic is a question, not tracked work — answer it.

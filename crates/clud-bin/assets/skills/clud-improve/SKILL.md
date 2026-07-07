---
name: clud-improve
description: File a specific clud improvement, feature request, bug report, or rough-edge report as a GitHub issue against zackees/clud; ask for details only for a bare manual /clud-improve invocation without an argument string.
triggers:
  - When the user types "/clud-improve <specific report>"
  - When the user wants to suggest a specific clud improvement, feature, bug, or rough edge
  - When an agent auto-selects this skill because the conversation already contains a concrete clud report
---
<!-- managed-by: clud -->

# /clud-improve

Capture one specific improvement idea for clud and ship it as a GitHub issue against `zackees/clud`.

Three hard rules:

1. **Concrete report means file directly.** If the invocation has an argument string or this skill was auto-selected from a user message that already states the improvement, bug, rough edge, expected behavior, or observed failure, do not ask "how can clud improve?". Treat the user's provided words as the report and file the issue.
2. **Bare manual invocation asks once.** Only when the user manually invokes bare `/clud-improve` with no argument string and no concrete report in the current request, ask exactly: `how can clud improve? be as specific as possible`. Wait for the answer before continuing.
3. **One issue, no chat draft.** Check `gh auth status`, post directly with `gh issue create --repo zackees/clud`, and return the issue URL. If auth fails, tell the user to run `gh auth login` and stop.

## Code Change Rule

This skill does not change code; it files an improvement request against `zackees/clud`. If the resulting work is a bug fix or feature, that downstream PR must follow RED -> GREEN via `/clud-pr`: a focused failing test before the implementation that turns it green.

## Workflow

1. **Classify the input.**
   - If the user wrote `/clud-improve <text>`, use `<text>` as the report.
   - If the skill was auto-selected and the current user message already contains a concrete clud report, use that message as the report.
   - If the user manually invoked bare `/clud-improve` without details, ask exactly `how can clud improve? be as specific as possible` and wait.
   - If the report is still too vague after the user answers ("make it faster", "fix the bugs"), ask one follow-up to pin down a concrete scenario, command, file, or observed behavior. Stop after one follow-up.
2. **Check auth.** Run `gh auth status`. If it exits non-zero or reports not logged in:
   - Tell the user: "You are not signed in to GitHub via gh. Run `gh auth login` and re-run /clud-improve."
   - Stop. Do not draft or stash an issue body.
3. **Draft the issue body.** The user's words are the source of truth. Conventional title: `feat: <short summary>`, `fix: <short summary>`, or `chore: <short summary>`. Body sections:
   - **Reported by user** - quote the user's provided report verbatim.
   - **Context** - terse facts the agent can confirm from local state (clud version via `clud --version`, OS, relevant file paths). Do not invent.
   - **Proposed direction** - a one-paragraph best-guess of what the change might look like, marked "for triage; not a commitment."
   - **Acceptance criteria** - objectively closable bullets. For a bug, include a reproduction. For a feature, include the observable behavior change.
4. **Search for strong duplicates.** `gh issue list --repo zackees/clud --search "<keywords>" --state all`. Flag only genuine overlap (same component + same intent). Weak keyword matches do not count.
5. **Post.** `gh issue create --repo zackees/clud --title "..." --body "$(cat <<'EOF' ... EOF)"`. Use a heredoc so formatting survives.
6. **Report.** Two lines: a one-sentence summary of what was filed, then the URL. If a strong duplicate exists, add one line above the URL listing it. Nothing else.

## Failure modes to avoid

- **Asking despite concrete details.** Do not ask the generic prompt when the invocation or auto-selected context already contains the report.
- **Losing the user's words.** Preserve the user's report verbatim under "Reported by user"; summarize only in the title and context.
- **Filing anonymously.** If `gh auth status` fails, stop. Do not open a browser, web form, or file to a different repo.
- **Wrong repo.** Always `zackees/clud`. Do not infer the repo from the working directory; this skill is specifically for upstream clud feedback.
- **Vague title and body.** Title must name what changes. Body must reproduce the user's words verbatim under "Reported by user" so the maintainer reads the real feedback, not a paraphrase.
- **Padding "Related issues" with weak matches.** Strong overlap only.
- **Posting a draft to chat for approval.** Chat report is summary + URL. The draft goes straight to `gh issue create`.

## When NOT to use this

- The improvement is in scope of the current coding task; just make the change.
- The user already has a well-formed issue draft; use `/clud-issue` instead.
- The report is a security issue; escalate privately to the maintainer rather than filing a public issue.

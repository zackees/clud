---
name: clud-improve
description: Ask the user for a specific clud improvement and file it as a GitHub issue against zackees/clud when gh auth is present.
triggers:
  - When the user types "/clud-improve"
  - When the user wants to suggest a clud improvement, feature, or report rough edges
  - When the user asks "how can clud get better" or wants to send feedback upstream
---
<!-- managed-by: clud -->

# /clud-improve

Capture one specific improvement idea for clud and ship it as a GitHub issue against `zackees/clud`. Three hard rules:

1. **Ask first, file second.** Open with the literal prompt: `how can clud improve? be as specific as possible`. Do not pre-fill, paraphrase, or guess from prior conversation — wait for the user.
2. **Check `gh auth status` before posting.** If the user is not authenticated, stop and tell them to run `gh auth login`. Do not fall back to a browser, web form, or a different transport.
3. **One issue, no chat draft.** When authenticated, post directly with `gh issue create --repo zackees/clud`. Deliverable is the issue URL.

## Code Change Rule

This skill does not change code — it files an improvement request against `zackees/clud`. If the resulting work is a bug fix or feature, that downstream PR must follow RED -> GREEN via `/clud-pr`: a focused failing test before the implementation that turns it green.

## Workflow

1. **Ask.** Send exactly this question to the user, verbatim: `how can clud improve? be as specific as possible`. Wait for their reply. If the answer is too vague to file ("make it faster", "fix the bugs"), ask exactly one follow-up to pin down a concrete scenario, command, file, or observed behavior. Stop after one follow-up — do not interrogate.
2. **Check auth.** Run `gh auth status`. If it exits non-zero or reports not logged in:
   - Tell the user: "You are not signed in to GitHub via gh. Run `gh auth login` and re-run /clud-improve."
   - Stop. Do not draft or stash an issue body.
3. **Draft the issue body.** The user's words are the source of truth. Conventional title: `feat: <short summary>`, `fix: <short summary>`, or `chore: <short summary>`. Body sections:
   - **Reported by user** — the literal quote of what the user said.
   - **Context** — terse facts the agent can confirm from local state (clud version via `clud --version`, OS, relevant file paths). Do not invent.
   - **Proposed direction** — a one-paragraph best-guess of what the change might look like, marked "for triage; not a commitment."
   - **Acceptance criteria** — objectively closable bullets. For a bug, include a reproduction. For a feature, include the observable behavior change.
4. **Search for strong duplicates.** `gh issue list --repo zackees/clud --search "<keywords>" --state all`. Flag only genuine overlap (same component + same intent). Weak keyword matches do not count.
5. **Post.** `gh issue create --repo zackees/clud --title "..." --body "$(cat <<'EOF' ... EOF)"`. Use a heredoc so formatting survives.
6. **Report.** Two lines: a one-sentence summary of what was filed, then the URL. If a strong duplicate exists, add one line above the URL listing it. Nothing else.

## Failure modes to avoid

- **Skipping the ask.** Never paraphrase, guess, or fill in the user's idea from earlier conversation context. The user must speak.
- **Filing anonymously.** If `gh auth status` fails, stop. Do not open a browser, web form, or file to a different repo.
- **Wrong repo.** Always `zackees/clud`. Do not infer the repo from the working directory — this skill is specifically for upstream clud feedback.
- **Vague title and body.** Title must name what changes. Body must reproduce the user's words verbatim under "Reported by user" so the maintainer reads the real feedback, not a paraphrase.
- **Padding "Related issues" with weak matches.** Strong overlap only.
- **Posting a draft to chat for approval.** Chat report is summary + URL. The draft goes straight to `gh issue create`.

## When NOT to use this

- The improvement is in scope of the current coding task — just make the change.
- The user already has a well-formed issue draft — use `/clud-issue` instead.
- The report is a security issue — escalate privately to the maintainer rather than filing a public issue.

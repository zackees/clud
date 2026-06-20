---
name: clud-git-diff
description: Show a git diff in a native OS webview window (Beyond Compare-style dual-pane with file picker on the left) via the bundled git/clud-git-diff.py tool. Invoke when the user asks to visually review changes between revisions.
triggers:
  - When the user asks to see a git diff visually, in a window, or in a viewer
  - When the user asks "show me the diff between X and Y" / "what changed since N commits ago" / "diff HEAD..HEAD~10"
  - When the user references file pickers, side-by-side diffs, Beyond Compare, GUI diff, or webview diff
  - When the user says "/clud-diff", "diff history", or "review changes"
---
<!-- managed-by: clud -->

# /clud-git-diff

Open a native OS-level webview window that shows a git diff with:

- **Left panel** — file picker listing every file that changed in the diff. Click a file to load it in the right panel.
- **Right panel** — Beyond Compare-style **dual-pane** view (before/after columns) of the selected file, with synchronized scrolling between the two columns, per-line numbers, and per-hunk headers.

The viewer is the only window the user has to manage. Closing it (via the OS X button) returns control to the agent.

Three hard rules:

1. **Always invoke the bundled tool**, never write an ad-hoc HTML file or open the default browser. The bundled tool guarantees the side-by-side layout, the synchronized scroll, and the native window — those are the contract.
2. **Default the revision range to `HEAD~10..HEAD`** when the user doesn't specify one. They almost always want "recent history" not the full repo lifetime.
3. **The agent BLOCKS on the tool until the window closes.** The Python script's `webview.start()` is a blocking call; do not background it or move on prematurely. When the user closes the window, the tool returns and the agent picks up.

## How to invoke

```bash
clud tool run git/clud-git-diff.py [LEFT [RIGHT]]
```

Argument shape:

- `LEFT` (positional, default `HEAD~10`) — git revision spec for the "before" side. Anything `git rev-parse` accepts: SHA, branch, tag, `HEAD~N`, `<branch>@{N}`.
- `RIGHT` (positional, default `HEAD`) — same shape, the "after" side.

Example invocations and what they show:

| User asks | Invoke with |
|---|---|
| "Show me the diff of the last 10 commits" | `clud tool run git/clud-git-diff.py HEAD~10 HEAD` |
| "What changed since main?" | `clud tool run git/clud-git-diff.py main HEAD` |
| "Diff this branch against main" | `clud tool run git/clud-git-diff.py main HEAD` |
| "Show me what's in PR #N" (after fetching) | `clud tool run git/clud-git-diff.py main pr/N` |
| "Diff abc123 and def456" | `clud tool run git/clud-git-diff.py abc123 def456` |
| "Show me the diff" (no range) | `clud tool run git/clud-git-diff.py` — defaults to `HEAD~10..HEAD` |

## Workflow

1. **Recognize the trigger.** The user wants to *see* a diff — they used words like "show", "view", "look at", "visual", or referenced specific revisions. They're not asking for a text dump (that's `git diff`).
2. **Resolve the range.** Pick `LEFT` and `RIGHT` from what they said. If ambiguous, prefer `HEAD~10..HEAD`. Don't ask — surface what you chose in your one-line acknowledgment so they can correct on the next message if needed.
3. **Invoke the tool.** `clud tool run git/clud-git-diff.py <left> <right>`.
4. **Block until the window closes.** The tool exits when the user closes the native window; the agent's subprocess wait returns at that point. Do not poll, do not assume — just wait.
5. **Pick up cleanly.** After the tool returns, briefly acknowledge: "diff viewer closed" and stand ready for the next message.

## Trigger phrases (be generous)

- "Show me a diff of the last N commits"
- "What's the diff between X and Y"
- "Open a diff viewer for…"
- "Show me what changed in [range]"
- "Visual diff of …"
- "I want to review the changes from …"
- "Beyond Compare style" (this skill's UX is explicitly modeled on Beyond Compare)
- "Diff with a file picker"

When in doubt: invoke the tool. The viewer is cheap to dismiss; not invoking it when the user wanted visual review is the worse failure.

## Hard rules

1. **Bundled tool only.** No ad-hoc HTML, no browser launches, no `git diff` text dumps when the user asked for a viewer.
2. **Default to `HEAD~10..HEAD`** when the range isn't specified.
3. **Don't background the tool.** It's blocking by design.
4. **Don't write to stdout while the viewer is open.** The user is in the window, not the terminal.
5. **Don't prefetch or speculate ranges** the user didn't ask for. If they say "last 5 commits" you use `HEAD~5..HEAD`, not `HEAD~10..HEAD`.
6. **One viewer at a time.** Don't spawn multiple windows in parallel.
7. **RED -> GREEN** still applies for any code changes the user makes after reviewing — the viewer doesn't modify anything, but the agent's follow-up edits do.

## Failure modes to avoid

- **Falling back to `git diff` text** when the user asked to *see* the diff. They asked for a viewer; give them the viewer.
- **Opening the OS default browser** instead of the bundled webview tool. The bundled tool gives the side-by-side + file picker layout — `webbrowser.open()` on raw HTML does not.
- **Resolving the range to something the user didn't ask for.** If they say "last 5 commits" don't show 10. Echo back what you chose.
- **Continuing while the viewer is open.** The user is reviewing; wait for them.
- **Reinventing the renderer.** Beyond Compare-style alignment, line numbers, sync-scroll, file picker — those are non-trivial. The bundled tool already does them.

## When NOT to use this

- The user asked for a one-line diff stat (`git diff --stat`) — that's a text op, no viewer needed.
- The user is in a non-interactive environment (CI, headless container) — the webview needs a display.
- The user explicitly asked for "the text" or "the raw diff" — they want stdout, not a window.
- The diff is for a single file and the user already named it — `git diff <file>` to terminal is fine.

## Related

- `crates/clud-bin/assets/tools/git/clud-git-diff.py` — the bundled tool source.
- Issue #445 — original request that motivated this skill.
- `clud tool run` — the dispatch entrypoint (sets `UV_CACHE_DIR` per the three-layer enforcement from #408).
- `pywebview` upstream — native OS webview wrapper used by the tool. WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux.

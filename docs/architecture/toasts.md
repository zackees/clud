# Toasts

How clud shows short, expiring messages (a CPU episode, "cpu back to normal")
while a harness TUI owns the terminal. Introduced by #1189; the design choice
is recorded in [DD-071](../DESIGN_DECISIONS.md#dd-071-toasts-are-composited-inside-the-terminal-stream-not-drawn-in-an-external-window).

## The problem

Before #1189 the CPU banner (`cpu_banner.rs`) `eprintln!`ed `[clud] cpu …`
lines onto the same terminal the child (Claude Code, Codex) was drawing on.
Nothing positioned, erased, or expired them, and nothing serialised them with
the child's own redraws, so a shorter line landed over part of a longer one
and the child's footer repaint cleared the wrong rows.

## Model

`crates/clud-bin/src/toast/mod.rs`

- A **toast** is `(key, text, severity, expires_at?)`. Publishing an existing
  key replaces it in place; `expires_at = None` means the producer closes it.
- **`ToastBoard`** holds one toast per key and picks the visible one
  (highest severity, then most recent).
- **`ToastHub`** is the thread-safe board plus a version counter. Producers
  publish to it; the compositor polls the version on each writer wake-up. The
  hub lives for the launch while compositors are rebuilt per PTY iteration.
- **`ToastSink`** is what producers hold: `Hub` (PTY mode), `StatusFile`
  (subprocess mode), `Discard`, or `Recorder` (tests).

The CPU banner is the only producer today. `cpu_banner::toast_events` maps its
state machine to events: a live `cpu` toast refreshed every tick while an
episode runs, "cpu back to normal" expiring after 10 s, and a close when an
episode ends below the clear threshold.

## Surfaces

| Launch | Surface | Where it renders |
|---|---|---|
| PTY, kitty graphics terminal | **Kitty tier** | semi-transparent image over the top-right cells |
| PTY, other terminal, child on the alternate screen | **Text-cell tier** | styled cells, top-right row |
| PTY, other terminal, child on the main screen | **Fallback** | Claude: status line. Others: terminal title |
| Subprocess, Claude | **Status line** | Claude Code's `statusLine` row |
| Subprocess, other harness | none | toasts are dropped |

Tier selection is `toast/tier.rs::decide`:

- kitty, Ghostty and WezTerm (probe, or `TERM`/`TERM_PROGRAM`/`KITTY_WINDOW_ID`/
  `WEZTERM_PANE`/`GHOSTTY_RESOURCES_DIR`) get the kitty tier;
- Konsole and iTerm2 report kitty graphics but implement it partially, so they
  stay on text cells until validated;
- Sixel-only terminals get text cells. Sixel adds nothing a text toast lacks:
  1-bit transparency, no blending over text, and the same repaint to remove;
- while clud's Sixel header owns a scroll region, in-grid tiers are off;
- `CLUD_TOAST_TIER=kitty|text|fallback|off` overrides the decision.

On the Linux/macOS default launch Claude runs in subprocess mode, so Claude
users see the status line unless they opt into PTY mode (`CLUD_PTY_DEFAULT=1`,
`--pty`; the default flip is #691). Codex already uses PTY there (DD-070).
On Windows both harnesses default to subprocess; PTY sessions there (`loop`,
`grind`, `--pty`) get the kitty tier under WezTerm and text cells or the title
elsewhere.

## Compositor

`crates/clud-bin/src/toast/compositor.rs`, run on the PTY pump's writer thread
(`session_output.rs::run_output_writer_composited`, extending DD-018's
reader/writer split). The writer channel carries `OutputMsg::Child` bursts and
`OutputMsg::Resize`; with a toast pending or visible the writer also wakes
every 100 ms to handle expiry.

For every child burst the compositor forwards the bytes, feeds a `vt100`
shadow of the child's intended screen and an `EscapeTracker`, then injects
toast bytes only when safe:

- **Between sequences.** `toast/tracker.rs` tracks CSI/OSC/DCS/APC/SOS/PM
  state and UTF-8 continuations; injecting mid-sequence corrupts both streams.
- **Outside synchronized updates.** Claude Code and Codex wrap frames in
  DECSET 2026; a block left open longer than 250 ms is drawn over anyway.
- **Not in pending-wrap.** With the cursor in the last column a CUP would
  cancel the terminal's pending wrap, so injection waits.
- **Cursor restored by CUP from the shadow**, never DECSC/DECRC (the child may
  own that single save slot). Origin mode is switched off for the toast and
  the restore is relative to the scroll region.
- **Every kitty command carries `q=2`**, or the terminal's reply would reach
  the child's stdin as typed input.

### Kitty tier

`toast/raster.rs` renders a rounded translucent panel, a severity accent bar,
the text (bundled DejaVu Sans Mono via `fontdue`, no system font library) and
a close X into a PNG at a nominal cell size; `toast/kitty.rs` uploads it once
(`a=t`) and places it with `c=`/`r=` so the terminal scales it to the real
cell grid. The placement is re-sent after every burst because images scroll
with text, and the image is re-uploaded after `ED 2`, `RIS` or an
alternate-screen switch, which drop images. Removal deletes the placement:
the child's cells were never touched, so nothing is repainted.

### Text-cell tier

`toast/text_tier.rs` paints a one-row toast. Removal repaints the rectangle
from the shadow (ECH to true blanks, then `rows_formatted`), widened so it
never splits a wide character. Before each child burst the toast is lifted so
scrolls move the child's real content; if the burst begins mid-sequence the
lift is impossible and the whole screen is repainted afterwards. The tier is
limited to the alternate screen because on the main screen a scroll would
push toast cells into the terminal's native scrollback permanently.

### Click to dismiss

clud never enables mouse tracking itself; that would steal selection and
wheel scrollback from inline TUIs. When the shadow shows the child already
reports SGR mouse events (Claude Code fullscreen), the compositor publishes
the close button's rectangle and `toast/mouse.rs` filters stdin: a left press
inside it and its release are swallowed and dismiss the toast. Everything
else passes through byte-exact, and a held partial report (a lone Esc) is
released on the next idle poll.

## Claude status line

`crates/clud-bin/src/toast/statusline.rs`, wired by
`ForegroundRuntime::start_with_statusline`.

- `StatusStateWriter` keeps `<state_dir>/toasts/<pid>.json` in step with the
  session's toasts (`updated_ms`, text, severity, `expires_ms`) and removes it
  on drop.
- clud composes a `statusLine` whose command is
  `"<clud>" statusline --session-pid <pid> --state-dir "<dir>" [--chain-b64 …]`
  with `refreshInterval: 2` (never slower than the user's own).
- **The user's status line is chained, not replaced.** clud discovers the
  effective one (explicit `--settings`, then the project's
  `.claude/settings.local.json` / `settings.json`, then
  `~/.claude/settings.json`), base64url-encodes its command, and
  `clud statusline` runs it first with Claude's session JSON on stdin
  (`sh -c`; Git Bash or `cmd` on Windows), then appends the toast.
- The setting rides the same single launch-scoped `--settings` source hooks
  use: merged into that file, or into the user's own `--settings` document,
  which then replaces the argument.
- A non-expiring toast whose `updated_ms` is older than 90 s is ignored, so a
  crashed session cannot leave a stuck alert.

`clud statusline` is dispatched first thing in `main.rs`, before any daemon,
runtime-cache or launch work, because Claude runs it every couple of seconds.

## Configuration

```json
{ "foreground": { "toasts": { "enabled": true, "claude_statusline": true } } }
```

- `enabled = false`: no compositor, no status-line injection.
- `claude_statusline = false`: keep in-grid toasts, never compose a
  `statusLine` (restores the pre-#1189 Claude argv).
- Toasts are off whenever the CPU banner is off (`--no-cpu-banner`,
  `[foreground.cpu_banner] enabled = false`, `--dry-run`, detached launches).
- `CLUD_TOAST_TIER` forces a tier; `CLUD_TOAST_DEMO=<text>` (with optional
  `CLUD_TOAST_DEMO_SECS`) shows a toast at launch for manual validation.

## Limits

- `vt100` does not model strikethrough, underline style/colour, overline or
  OSC 8, so a text-tier repaint drops those inside the toast rectangle.
- Feeding the shadow costs a pass over every child byte while toasts are
  enabled in PTY mode.
- Konsole and iTerm2 kitty-graphics support is unvalidated (text tier).
- Daemon-attached sessions do not composite toasts.
- An external overlay window anchored to the terminal was evaluated and
  deferred (DD-071).

## Tests

| Layer | Where |
|---|---|
| Model, tracker, kitty encoding, raster, text tier, mouse, tier matrix, status line | `src/toast/*` unit tests (all CI lanes) |
| Compositor byte streams (every tier, deferral, origin mode, dismiss, resize) | `src/toast/compositor_tests.rs` |
| Banner never writes to the terminal; banner → toast events | `src/cpu_banner_tests.rs` |
| Status-line injection into Claude settings | `src/foreground_runtime.rs` tests |
| Real PTY sessions: kitty tier, title fallback (Linux, macOS, Windows), alternate-screen text tier (Unix) | `tests/pty/toast_pty.rs` |

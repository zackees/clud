---
name: video-use
description: "Edit videos with browser-use/video-use from any session, on demand. Explicit-only: invoke as /video-use; it binds clud's pinned video-use checkout."
triggers:
  - When the user types /video-use
disable-model-invocation: true
---
<!-- managed-by: clud -->

# video-use (on demand)

This bridges to clud's pinned copy of browser-use/video-use. `clud video` is
the dedicated session; this skill loads the same workflow into the current one.

1. Run `clud video --path` and use its single stdout line as `CHECKOUT`. It
   installs the pinned checkout on first use. If it exits non-zero, show the
   user its message (for example missing `ffmpeg` or `uv`) and stop.
2. Read `$CHECKOUT/SKILL.md` and follow it for the rest of this task, with the
   user's request as its input: `$ARGUMENTS`.
3. Run its helpers as
   `uv run --project "$CHECKOUT" python "$CHECKOUT/helpers/<name>.py" ...`,
   never with a bare `python`, so they use the checkout's own environment.
4. ElevenLabs key: use an ambient `ELEVENLABS_API_KEY`. If it is unset, tell
   the user to run `clud video` once (it stores the key in clud's vault and
   provides it to that session) or to export the variable and relaunch. Never
   ask the user to paste a key into the chat.

If the task turns into a code change rather than an edit of media, keep the
RED -> GREEN rule: a focused failing test first, then the fix.

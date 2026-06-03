---
name: clud-windows-trash
description: Quarantine Windows-locked DLL/EXE/PYD artifacts with clud trash instead of retrying deletes or killing processes.
triggers:
  - "When deleting *.dll, *.pyd, or *.exe on Windows fails with os error 32 or being used by another process"
  - "When a build fails because an output DLL, PYD, or EXE is locked"
  - "When a directory containing Windows build artifacts cannot be removed"
---
<!-- managed-by: clud -->

# /clud-windows-trash

Use `clud trash` when Windows refuses to delete freshly built native artifacts.

## Code Change Rule

If trashing a locked artifact unblocks a bug fix or feature implementation, continue with RED -> GREEN: prove the focused test/repro fails before the code change, implement the scoped fix, then rerun that signal until it passes.

## Triggers

- A delete of `*.dll`, `*.pyd`, or `*.exe` fails with `os error 32`, `being used by another process`, `Access is denied`, or similar loader/Defender lock symptoms.
- A build script cannot overwrite an output file because the previous artifact is still locked.
- A directory removal fails and the tree contains Windows native build outputs.

## Rules

1. Do not retry `del`, `Remove-Item`, or `rm` in a loop.
2. Do not run `taskkill` for guessed parent processes.
3. Run `clud trash <path>` for the locked file or containing directory, then continue the build or cleanup.
4. If `clud trash` reports a source/trash volume mismatch, pass `--cross-volume` only when the copy cost is acceptable: `clud trash --cross-volume <path>`.

## Workflow

1. Identify the smallest locked path that unblocks the build. Prefer the exact file; use the containing build-output directory when many siblings are blocked.
2. Run `clud trash <path>`.
3. If the command succeeds, proceed immediately. The clud daemon will retry deletion from `~/.clud/trash/` until Windows releases the lock.
4. If cross-volume copy is required and acceptable, rerun with `--cross-volume`. Source removal after the copy is best-effort.
5. To inspect pending cleanup, run `clud gc list --kind trash`.

## Exit Guidance

After a successful trash, keep going. Do not wait for the daemon to reap the quarantined path unless the user explicitly asks to verify cleanup.

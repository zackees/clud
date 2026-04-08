# Codex App-Server Plan

This document captures the intended Codex app-server integration shape for `clud`.

## Goal

Use `codex app-server` as the primary Codex integration path so `clud` can:

- detect writes from structured Codex events instead of PTY scraping
- run post-edit hooks after a write-producing turn completes
- keep reads as pass-through unless a stricter policy becomes necessary later
- reserve PTY prompt injection for fallback-only notification behavior

Authentication remains owned by the Codex CLI. `clud` should reuse the existing `codex login` session instead of implementing its own auth flow.

## Write Detection Strategy

The app-server protocol exposes multiple write-adjacent signals:

- `item/fileChange/requestApproval`
- legacy `applyPatchApproval`
- `TurnDiffUpdatedNotification`

The current plan is:

1. Treat `item/fileChange/requestApproval` as a coarse "this turn intends to write" signal.
2. Track the latest non-empty turn diff from `TurnDiffUpdatedNotification`.
3. On turn completion, if the turn both requested a write and accumulated a non-empty diff, emulate a post-edit event and run configured post-edit hooks.

This keeps the logic entirely within the app-server integration path.

## Read Handling

Reads are not the primary concern for this integration. They should pass through by default.

If read policy becomes necessary later, it should be layered on top of command approval handling rather than blocking the initial write-hook implementation.

## Command Handling

Command execution approval remains a separate policy surface.

Short term:

- allow normal command execution to continue
- keep policy decisions focused on write events and post-edit hooks

Later:

- add command classification and selective denial for destructive commands
- optionally log command approvals as audit events

## Post-Edit Hook Emulation

Codex app-server does not expose a direct "post-edit hook" event equivalent to Claude's hook model.

`clud` should emulate this event by:

1. tracking that a turn requested file changes
2. tracking the aggregated turn diff
3. running hook commands when the turn finishes and the final diff is non-empty

Hook failures should:

- capture stdout/stderr
- write a failure artifact to disk
- return structured failure details to the caller

PTY injection should only be used if a legacy interactive path still needs user-facing error surfacing and no structured app-server path is available.

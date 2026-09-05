# `grind` Execution Contract

This document is the single owner of the required behavior of `clud grind`.
It deliberately describes the required implementation while the historical
external-loop implementation is being removed; current behavior that conflicts
with this document is a bug, not a compatibility contract.

## Required behavior

`clud grind [url]` starts exactly one normal, foreground, interactive PTY
session for the selected harness. With no URL, clud resolves the repository's
`origin` remote to its forge issues page. An explicit URL is used verbatim.
Before the session starts, clud forms the normal `grind` prompt beginning:

```text
/loop look at <resolved issues URL> and select the next issue and then perform the task.
```

clud passes that prompt to the harness's normal interactive entrypoint. It
does not use a headless prompt flag or subcommand (`-p`, `exec`, or their
equivalent), and it does not relaunch the backend or issue an external repeat
prompt after the session begins. Ordinary helper processes remain outside this
restriction.
The PTY is the ordinary user-visible session: the harness renders its own UI,
the user can interact with it, and the harness receives terminal input and
signals normally.

The harness owns every repetition decision, including its completion and
blocked behavior. `clud grind` has no clud-side completion protocol and no
clud-side scheduler. In particular, it must not inject or inspect DONE or
BLOCKED marker paths, scan completion tokens, create loop artifacts, set a
fixed iteration count (including 200), re-prompt/relaunch the harness, use the
daemon repeat worker, or add headless stream-json rendering.

## Harness support

`grind` is available only when the selected harness can accept `/loop` in a
normal interactive PTY prompt. If a harness lacks that capability, clud must
report that `grind` is unsupported for that harness before launch. It must not
silently substitute `clud loop`, headless prompting, marker polling, or any
other external loop.

## Boundary with `clud loop`

`clud loop` remains a separate command with its own external runner,
DONE/BLOCKED contract, iteration budget, artifacts, and optional repeat
scheduler. Those mechanisms belong only to
[the loop subsystem](loop-subsystem.md). Sharing task text or launch-plan
plumbing does not permit `grind` to inherit the loop subsystem's lifecycle.

## Historical guidance and tests

Issue #897 and PRs #950 and #1045 are superseded where they prescribe or
preserve clud-managed grind iteration, markers, headless execution, or output
streaming. They are historical context only and must not be used as authority
for a future `grind` fix. Legacy tests that assert an external iteration count
or DONE/BLOCKED behavior must be replaced when the runtime changes. The
replacement tests must prove one interactive PTY backend launch, a `/loop`
prompt, and the absence of markers, external iterations, and stream-json
setup.

## Implementation review checklist

When changing `grind`, verify all of the following:

- One backend harness session is launched for one `clud grind` invocation.
- Launch mode is interactive PTY and the generated prompt begins `/loop look
  at <resolved issues URL>`.
- The argv uses the harness's ordinary interactive entrypoint.
- The plan carries no loop markers, repeat schedule, external iteration count,
  or stream-json progress setting.
- Unsupported harnesses fail before a backend process is spawned.

See [DD-068](../DESIGN_DECISIONS.md#dd-068-grind-delegates-looping-to-the-interactive-harness) for the rationale.

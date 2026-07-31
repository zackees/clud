# Test-runtime memory (design proposal — issue #405)

**Status: proposal.** This document is the deliverable of #405, whose terminal
state is "design proposal accepted by reviewer." Implementation lives in #407
and must not merge until this design is accepted.

## The problem

The agent's run-all-vs-targeted choice is binary and uninformed. "Run
everything" is correct but can take minutes; "run targeted" is fast but risks
missing regressions. Nothing records that `integration` on this checkout has
historically taken four minutes while `unit` takes two seconds, so the decision
is made by vibes.

Linting is deliberately excluded: `bash lint` finishes in seconds, so the right
answer is always "run it," and histogramming it would be modelling a decision
nobody needs to make.

---

## Q1. Storage format and concurrency

**Chosen: append-only JSONL under `.clud/test-runtime/runs.jsonl`, compacted
opportunistically.**

> **The seed schema on #405 proposes SQLite, and that option no longer exists
> in this repo.** `rusqlite` was removed deliberately — `crates/clud-bin/Cargo.toml`
> records the swap to `redb` "to cut cold-build time and drop the C-toolchain
> pressure on CI." Re-adding a bundled-C dependency to store a few hundred
> integers would trade a real, paid-for build-time win for a rounding error of
> convenience. The seed's *content* survives this change almost entirely; only
> its container does.

That leaves redb and a flat file. Redb is already a dependency and is the
obvious reflex — but it takes an **exclusive per-process file lock**, which is
exactly the problem [DD-006](../DESIGN_DECISIONS.md) needed a whole daemon and
an `fs4` advisory lockfile to solve for the GC registry. Two `clud test`
invocations from two shells is not an exotic case here; it is Tuesday. Standing
up daemon IPC, or a second lockfile protocol, to record a duration is a large
mechanism for a small fact.

Append-only JSONL avoids the coordination entirely:

- **Writes are single lines appended with `O_APPEND` / `FILE_APPEND_DATA`.** A
  record is ~120 bytes, far below the size at which append atomicity stops
  holding, so concurrent writers interleave records but never corrupt one.
- **Readers need no lock at all** — they read what is there. A record being
  written concurrently is either fully present or absent.
- **A torn or unparseable line is skipped, not fatal.** This is a histogram; one
  lost sample out of hundreds changes no decision. Failing closed on a malformed
  line would let a single bad write disable the feature permanently.
- **It matches existing precedent.** `reap.jsonl` and `daemon-events.jsonl` are
  the same shape, so the buffered-writer and rotation habits already exist in
  this codebase.

The cost is that queries scan rather than index. At the sample counts this
design targets (§Q4: steady state ≈ 600 rows) a full scan is well under a
millisecond, and the read tool's budget is 100 ms.

## Q2. Bucket taxonomy

**Chosen: explicit `--bucket` at invocation, with a path heuristic as the
fallback and `unknown` as the floor.**

Explicit wins because the wrapper is already being invoked deliberately
(§Q8) and the caller always knows what it is running. The heuristic exists so
an un-annotated call still records something useful:

| Path shape | Bucket |
| --- | --- |
| `tests/integration/**` | `integration` |
| `tests/e2e/**` | `e2e` |
| `tests/smoke/**`, `*_smoke*` | `smoke` |
| anything else under `tests/`, or `--lib` | `unit` |
| nothing matched | `unknown` |

`unknown` is a real bucket rather than a silent drop: a growing `unknown` count
is a visible signal that the heuristic needs a case, whereas discarded samples
are invisible.

## Q3. CPU-load normalization

**Chosen: store raw `(duration_ms, cpu_load_pct)`; normalize at query time.**

Normalization is a formula that will be tuned, and **raw data is reversible
while normalized data is not.** Baking a multiplier in at write time would make
every historical sample unusable the first time the formula changed. Cost of
keeping both: 4 bytes per row.

- **Metric:** instantaneous total CPU utilization percent, sampled over a ~1 s
  window immediately before the run starts. Load average is rejected: it does
  not exist natively on Windows, and its 1-minute smoothing lags a test run that
  is about to start.
- **"Idle" is `cpu_load_pct <= 25`.** A threshold, not a curve, because the
  decision it feeds is coarse.
- **Query-time rule:** samples taken at `cpu_load_pct > 75` are *down-weighted
  to half* when computing percentiles, not discarded. A slow run under
  contention is still evidence, just weaker — and on a busy developer box,
  discarding contended samples can leave a bucket with no data at all.

`sysinfo` (already a dependency) provides this cross-platform, so no new
per-OS code is needed.

## Q4. GC trigger

**Chosen: count-based, opportunistic, on write.** Every 64th append checks the
file; if it exceeds `MAX_ROWS = 1000`, rewrite it retaining the newest 500 rows
per bucket and dropping anything older than 30 days.

Count-based beats time-based because it is deterministic and testable without a
clock. Doing it on write rather than on read keeps the read path — the one with
a 100 ms budget the agent hits before *every* test invocation — free of
rewrites. Compaction writes a sibling temp file and renames, so a concurrent
reader sees either the old file or the new one.

**Worked example.** A developer running tests 20 times a day across 4 buckets
appends ~20 rows/day. The 30-day age bound alone gives a steady state of
**≈ 600 rows** (~72 KB). The 500-per-bucket cap does not bind at that rate; it
exists for the burst case — a CI-like loop appending hundreds of runs in an
afternoon — where it caps the file at 2000 rows (~240 KB) regardless of age.
Compaction then runs at most once per 64 appends, i.e. roughly every three days
of ordinary use.

## Q5. Granularity

**Chosen: `(bucket, target, host_os)` recorded; `bucket` alone is the default
query key.**

Record the finer keys because they cost ~10 bytes and cannot be recovered
later; query on the coarse one because per-bucket is where the statistical power
is. `target` is `null` for a whole-bucket run and the test filter for a targeted
one, so both flavours coexist in one file — that is what makes "which single
test is the slow one" answerable later without a schema change now.

`host_os` is recorded but is not a query key in v1. The store is per-checkout
and gitignored (§Q9), so a single file rarely spans machines; the column is
insurance against the WSL/native-Windows case where it does.

## Q6. Decision policy

**Chosen: thresholds are constants in the tool, overridable from
`.clud/settings.json`; the *recommendation* is data, and the agent may override
it with a stated reason.**

The tool answers "how expensive is this bucket," which is a fact. Whether to
pay that cost depends on blast radius, which only the agent knows. So the tool
recommends and never decides.

Inputs: weighted `p90`, sample count `n`, and the caller-supplied change blast
radius (`narrow` | `broad`).

| Condition | Recommendation |
| --- | --- |
| `n < 5` | `run-all` — insufficient history, prefer correctness (§cold start) |
| `p90 <= 30s` | `run-all` — cheap enough that targeting is false economy |
| `p90 > 30s` and blast radius `broad` | `run-all` — the change can break anything |
| `p90 > 30s` and blast radius `narrow` | `targeted` |
| `p90 > 5min` and blast radius `narrow` | `targeted`, and surface the p90 so the agent can offer to background it |

**Worked example.** `integration` has `n=12`, weighted `p90 = 4m10s`. The pending
change edits one bundled skill's markdown — blast radius `narrow`. Rule 4
applies: **recommend targeted**, because 4+ minutes is a real cost and a
markdown edit cannot plausibly break the daemon IPC suite. Had the same change
touched `args.rs` (which every subcommand routes through), blast radius would be
`broad`, rule 3 would apply, and the recommendation would flip to `run-all`
despite the identical p90 — the cost did not change, the risk did.

## Q7. Read-tool surface

**Chosen: `clud test stats [--bucket <name>] [--json]`.**

A subcommand, because `clud` already owns `.clud/` state and adding a binary
means another artifact to ship and PATH-resolve. Human output is one line per
bucket; `--json` is the agent's path.

```
unit         p50=1.9s   p90=6.4s    n=43  → cheap, run all
integration  p50=3m52s  p90=4m10s   n=12  → expensive, prefer targeted
e2e          n=0                          → no history, run all
```

Registering a new subcommand means the three places CLAUDE.md names —
the `Command` enum in `args.rs`, the dispatch arm in `main.rs`, **and** the
hardcoded `subcommands` array in `split_known_unknown`. Missing the third
routes `clud test` to the backend agent as passthrough.

## Q8. Write/wrapper surface

**Chosen: `clud test run --bucket <name> -- <command…>` is canonical.**
`bash test` may later call it, but the wrapper is the contract.

The wrapper samples CPU, runs the command inheriting stdio, records
`(duration, load, exit_code)`, and **propagates the child's exit code
unchanged**. That last point is what makes it safe to adopt: anything that runs
tests today keeps working if the wrapper is prefixed onto it.

Recording is best-effort. An unwritable `.clud/` must never fail a test run —
the histogram is a convenience, and a tool that can break the build to record a
statistic about the build will be removed.

## Q9. Cross-machine portability

**Chosen: purely local-observed. No CI bootstrap.**

CI machines have different core counts, different contention, and a cold cache;
seeding local percentiles from them would encode a number that is wrong in a way
the user cannot see. An empty store answering "no history, run all" is honest.
The cold-start policy (§below) already makes that safe.

---

## Schema sketch

One JSON object per line. Field names are short because they repeat every row.

```jsonc
// A fresh record: unit tests, whole bucket, on an idle box, passing.
{"v":1,"b":"unit","t":null,"ms":1913,"cpu":11,"at":1785470000,"rc":0,"os":"windows"}

// Near GC eviction: 29 days old, so the next compaction after it crosses 30
// days will drop it. Targeted integration run, taken under heavy contention —
// which is why its 6m41s is down-weighted rather than treated as typical.
{"v":1,"b":"integration","t":"daemon_persistence","ms":401220,"cpu":93,"at":1782878000,"rc":0,"os":"windows"}
```

| Field | Meaning |
| --- | --- |
| `v` | schema version; a reader ignores rows whose `v` it does not know |
| `b` | bucket (`unit`/`integration`/`e2e`/`smoke`/`unknown`) |
| `t` | target (test filter), or `null` for a whole-bucket run |
| `ms` | wall-clock duration |
| `cpu` | total CPU % sampled in the ~1 s before start, 0–100 |
| `at` | unix epoch seconds at start |
| `rc` | child exit code; stats default to `rc == 0` |
| `os` | `windows`/`linux`/`macos` |

`v` is the migration story: a v2 reader skips nothing, and a v1 reader skips v2
rows rather than misreading them. There is no migration script — the file is a
cache, and the correct response to an unreadable one is to start over.

## Cold start / no prior data

- **`n == 0`:** report `no history`, recommend `run-all`. The first run is also
  the one that creates the data.
- **`n < 5`:** recommend `run-all` regardless of p90. Percentiles over four
  samples are noise, and being wrong toward "ran too much" costs minutes while
  being wrong the other way costs a missed regression.
- **Never block.** A missing, empty, or corrupt store behaves exactly like
  `n == 0`.

## Non-goals

Stated as boundaries, not omissions:

- **Implementation.** This document is the deliverable; #407 owns the code.
- **Lint histograms.** Lint is fast; always run it.
- **Cross-developer or cross-machine sharing.** The store is per-checkout and
  gitignored. These are one developer's lived experience, not shared truth.
- **Per-test pass/fail tracking.** `rc` is recorded to *exclude* failed runs from
  timing stats, not to build a flaky-test tracker. That is a different problem
  with different retention needs.
- **Coverage data.** Orthogonal.
- **Cost estimates** ($/minute of CI). Possibly a later extension.
- **Predicting which tests a change affects.** The tool reports what things
  cost; blast radius is the agent's judgement and is an *input* here, not an
  output.

## Open risks

- **Blast radius is caller-supplied**, so a caller that always says `narrow`
  gets `targeted` forever. Mitigation is that the recommendation is advisory and
  the p90 is always shown; a stronger mitigation would need change-impact
  analysis, which is a non-goal.
- **Append atomicity across network filesystems** is weaker than on local disks.
  The store lives under the checkout, so a repo on a network share could
  interleave badly. Acceptable: the failure mode is a skipped malformed line.

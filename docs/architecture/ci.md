# CI architecture — build-once / run-everywhere

Status: implemented (supersedes the 24 per-platform leaf workflows + `_lint.yml` /
`_unit-test.yml` / `_integration-test.yml`).

## The problem

Every push fans out to **12 heavy workflows** (6 platforms x {unit-test,
integration-test}), each of which is a *from-source native build*:

| Workflow | Full workspace compiles it performs |
| --- | --- |
| `_unit-test.yml` | `cargo clippy --workspace --all-targets` (1) + `cargo build -p clud -p mock-agent` + `cargo test --workspace --no-run` (1) |
| `_integration-test.yml` | `cargo build -p clud -p mock-agent` + `maturin build` dev wheel (1) |

So per push: **~12–18 full workspace compiles**, spread across **12 mutually
invisible cache namespaces** — every job pays its own cold-cache tax and none
of them warms another. Four of the six
platforms are macOS/Windows runners, which are the scarcest and slowest in the
pool, so the fan-out converts directly into queue depth.

On top of that, three genuinely platform-independent checks run **six times
each**: `ruff`, `cargo fmt --check`, and `ci/banned_imports.py`
(`ci/lint.py:37-43`).

## The shape of the fix

Split the two things CI conflates — *producing artifacts* and *executing them* —
and make the producer side live on Linux.

```
  ┌──────────┐  ┌──────────┐   per triple, independently:
  │  static  │  │  dylint  │
  │  ubuntu  │  │  ubuntu  │   ┌──────────────┐   bundle-<triple>   ┌────────────┐
  │ ruff/fmt │  │ non-PR   │   │ build-<trip> │ ─────────────────►  │ test-<trip>│
  │ /banned  │  │  only    │   │  ubuntu-24   │  .tar.gz artifact   │   NATIVE   │
  └────┬─────┘  └────┬─────┘   │  clippy +    │                     │  unit +    │
       │             │         │  bins +      │                     │ integration│
       │             │         │  test bins   │                     │ no cargo,  │
       │             │         │  + wheel     │                     │ no rustc   │
       │             │         └──────────────┘                     └─────┬──────┘
       └─────────────┴────────────────────────────────────────────────────┤
                                                                          ▼
                                                                   ┌────────────┐
                                                                   │   ci-ok    │
                                                                   └────────────┘
```

Each triple gets its **own** build job and its **own** test job, rather than one
build matrix feeding one test matrix. That is not stylistic: `needs:` on a
matrix job is all-or-nothing in GitHub Actions, so a single `test` matrix
depending on a single `build` matrix would make the Linux tests — ready first —
wait for the slowest cross-build in the set. GitHub exposes no per-leg
dependency edge, so the lanes are written out longhand. `ci/ci_matrix.py`
remains the source of truth for the triple table and
`tests/test_ci_matrix.py::test_ci_yml_covers_exactly_the_targets_table` fails if
the YAML drifts from it.

There is deliberately no `plan` job. Computing the matrix in a preceding job
would put a checkout + `setup-python` (~40 s of pure latency) at the head of
every run and add a dependency edge to every lane; the tier gating is instead a
job-level `if:` on each optional lane.

Three structural claims, in the order they matter:

1. **One build per triple, not three.** `clippy --all-targets`, the workspace
   binaries, the `cargo test --no-run` harness binaries, and the dev wheel are
   produced in *one job with one `target/` directory*. They already share
   ~95% of their compilation graph (every dependency rlib); today that graph
   is recompiled on three separate machines. This is the single largest win
   and it requires no cross-compilation at all.
2. **The build host is always Linux.**
   Linux runners are the cheapest and least contended, and — critically — all
   targets then share one runner class, so cache behaviour is uniform.
3. **macOS/Windows runners never compile.** They download a bundle and execute
   it. Their job duration collapses from "cold C++ build + test" to "test",
   which is what makes using them sparingly viable.

## Target tiers — using scarce runners sparingly

`ci/ci_matrix.py` defines the target inventory consumed by the workflow. Not
every push needs all six targets.

| Tier | Triples | Trigger |
| --- | --- | --- |
| `core` | `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, and `aarch64-apple-darwin` | every PR push |
| `full` | core + `aarch64-unknown-linux-gnu`, `aarch64-pc-windows-msvc`, `x86_64-apple-darwin` | `push` to `main`, `merge_group`, `ci:full` PR label, `workflow_dispatch` |

Rationale: `core` covers one triple per *operating system*, which is where
essentially all platform-specific behaviour lives (the platform-gated tests are
`#![cfg(windows)]` / `#![cfg(unix)]`, not arch-gated). The second architecture
of each OS is an ABI/codegen check, not a behaviour check, so it belongs on the
merge queue and `main`, not on every intermediate PR push. `x86_64-apple-darwin`
(`macos-15-intel`) in particular is the slowest runner in the pool and is
demoted to `full` only.

macOS ARM is always core. `soldr prepare --target aarch64-apple-darwin`
provisions the target-shaped Apple SDK on the Linux builder, so the old
`MACOS_SDK_URL` gate and native macOS fallback no longer exist. The macOS
runners only execute the resulting bundle.

Two trigger-level notes:

- `pull_request` subscribes to `labeled` on top of the default event types.
  Without it, adding `ci:full` to an already-pushed PR would not re-trigger
  anything and the opt-in would silently do nothing.
- Push coverage narrows from "every branch" to `main`. Branches with no open PR
  no longer get CI. That was a large share of the duplicated fan-out, but it is
  a behaviour change worth knowing about.

## Cross-compilation, per triple, honestly

The workspace's cross-compile surface is small. Two crates compile native code
(`Cargo.lock`): `ring` and `blake3` (both `cc`, routine to cross).
`crates/clud-bin/build.rs` is pure Rust (`protox` + `prost-build`, no `protoc`
binary), so it is not a factor. `whisper-rs-sys` (`bindgen` + a full CMake
project) used to be the third and by far the most cross-compile-hostile —
but `whisper-rs` was removed entirely (voice transcription is stubbed; see
`crates/clud-bin/src/voice/README.md`) after its vendored CMake build
repeatedly broke Windows host builds.

| Triple | Strategy | Notes |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | **native** | the build host; also the clippy/dylint host |
| `aarch64-pc-windows-msvc` | **`soldr build`** | `soldr prepare` provisions the catalogued ARM64 MSVC CRT/SDK and the clang shim that `ring` requires. |
| `x86_64-pc-windows-msvc` | **`soldr build`** | The blessed soldr path provisions the MSVC CRT/SDK and LLVM toolchain. |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | **`soldr build` + target-shaped Apple SDK** | `soldr prepare` fetches the matching SDK and exports `SDKROOT`; there is no repo secret/variable and no native-builder fallback. |
| `aarch64-unknown-linux-gnu` | **`cargo-zigbuild`** | cleanest cross. |

### Invariant: soldr owns Apple and Windows-MSVC cross builds

**No workflow, build helper or release script may install or invoke a cross
compiler directly for a `*-apple-darwin` or `*-pc-windows-msvc` target.** That
means no `cargo xwin`, no `cargo-xwin`, no `cargo zigbuild` / `cargo-zigbuild`,
no `maturin --zig`, no `zig cc`, no `cross`, no `osxcross`, and no hand-rolled
install of any of them. `soldr prepare --target <triple>` provisions the
toolchain and `soldr build --target <triple>` links against it. Nothing else.

**Zig stays correct for Linux.** `aarch64-unknown-linux-gnu` crosses through
`cargo-zigbuild`, and the manylinux wheel links through `maturin --zig`. The
rule is target-aware, not a blanket ban — a rule that broke the Linux lanes
would be reverted within a day.

`cargo xwin` and `cargo zigbuild --target *-apple-darwin` remain *technically*
reachable, and soldr's own `docs/CROSS_COMPILE.md` documents them as legacy
passthroughs. That is the whole reason this is written down: the fast path and
the slow path are one word apart in a YAML file.

Three things enforce it, because no one of them is sufficient:

| Guard | Catches | Blind to |
| --- | --- | --- |
| `ci/banned_cross_tools.py` (runs in `bash lint` and CI's static job) | a literal command in YAML, Python, shell, PowerShell, TOML, Rust or a Dockerfile — including the argv-list form `["cargo", "xwin", ...]` and multi-line install steps | a *conditional* tool at a target held in a variable |
| `ci/xbuild.py::cargo_argv` raises on `zigbuild` + an Apple/MSVC triple | the dispatch itself, whatever the target's provenance | a caller that bypasses `cargo_argv` |
| `tests/test_ci_matrix.py` | every matrix triple's `strategy`, and the argv `cargo_argv` actually returns for it | a command path outside the matrix |

The linter's failure names the file, line, rejected tool, the target family, and
the `soldr build --target ...` replacement — a reader who has never seen this
rule should not have to go looking.

##### Two rule classes (#714)

The linter's original form required a **literal** Apple/MSVC triple on the same
line as the tool. That is right for Zig and wrong for everything else, so the
table is split:

- **Unconditional** — `cargo xwin`, the bare `xwin` CLI, `XWIN_*` /
  `CARGO_XWIN_*` env vars, `osxcross`, `cross` (build/test/run/check/rustc/
  bench/clippy), `Cross.toml`. These name an MSVC- or Apple-only toolchain by
  construction, so there is no target that makes them legal here.
  `cargo xwin build --target $TARGET` and `cargo xwin build --release` both
  fail, where before neither did. This is also the throughput fix: `cargo xwin`
  re-downloads and splats the MSVC CRT/SDK on every cold cache.

  Two shapes worth knowing about, because getting them wrong is how this rule
  goes quietly useless in one direction and gets reverted in the other. The
  `xwin` CLI pattern does **not** require the subcommand to be adjacent to the
  binary — `xwin --accept-license splat --output ...` is the form xwin's own
  README uses, and an adjacency rule would be green in the fixtures and blind
  in production. Conversely `cross` **is** anchored at a command position
  (start of line, a shell/Dockerfile continuation, a YAML `run:`, or a quoted
  argv list), because it is an ordinary English word: unanchored, `name: cross
  build matrix` and `let msg = "cross build failed";` both fail the lint.
- **Conditional on a soldr-owned target** — `cargo zigbuild`, `maturin --zig`,
  `zig cc`. Correct for `*-unknown-linux-*`, rejected at Apple/MSVC.
- **Installs**, at any target, matched against the whole file rather than line
  by line so the GitHub Actions shape (`taiki-e/install-action` with
  `tool: cargo-xwin` two lines below) is caught. Also `cargo binstall`, `brew`,
  `apt`/`dnf`/`apk`/`choco`, `pip install ziglang`, `houseabsolute/actions-rust-cross`,
  `cross-rs/cross`, `tpoechtrager/osxcross`, and a `linker =` override under a
  `[target.<apple-or-msvc-triple>]` section of a Cargo config (quoted or
  unquoted key; `linker` need not be the section's first entry).

An install **suppresses the invocation rules across every line its match
spans**. `cargo install cargo-xwin` is an install, and is also — to the
invocation rules — a mention of `cargo xwin`; reporting both counts one mistake
twice and makes the fix look bigger than it is. Spanning matters: the
`taiki-e/install-action` shape puts `uses:` and `tool: cargo-xwin` on different
lines, so suppressing only the line the match *starts* on would leave the
`tool:` line to be reported a second time. For spanning to be safe the install
match must not reach past the end of its own YAML step, so the window stops at
the next `- ` sequence item: otherwise an install-action for an unrelated tool
searches forward into the *following* step for its `cargo-xwin`, and every
genuine violation in between is silenced on the way. With that bound, a
violation elsewhere in the file is still reported.

One further trap, since it took down the whole linter rather than one rule:
line splitting must use `split("\n")`, never `splitlines()`. `splitlines()`
also breaks on form feed, vertical tab, a lone CR and U+0085/U+2028, none of
which the comment scanner preserves — so a form feed inside a Rust comment (an
Emacs page break) made the original and stripped line lists differ in length,
and `bash lint` died with a `ValueError` traceback instead of printing a
finding.

**Scope.** `.github/`, `ci/`, `bench/`, `crates/`, `dylints/`,
`testbins/`, `tests/`, `.claude/hooks/`, plus the root entrypoints `build lint
test install install.sh install.ps1 publish` and `.cargo/config.toml` — 302
files as of this writing. `crates/` covers the product source and not just the
asset scripts under it: clud shells out to build commands, so a `cargo xwin` in
Rust is a real vector. `vendor/` stays out — third-party source we do not
author, where `whisper-rs-sys/build.rs` legitimately reasons about zig's C++
runtime for the Linux lanes. `.claude/hooks` rather than `.claude` because the
latter also holds `worktrees/`, an ignored second checkout.

**Escape hatch.** Comments are stripped so prose explaining the ban stays legal.
For `#` languages that is a split; Rust gets a real character scanner, because
every cheap regex is wrong somewhere that matters: Rust block comments **nest**,
a `//` inside a URL must survive (a link to `cargo-xwin` is a real reference and
should be reported), and a stripper with no string awareness turns `let open =
"/*";` … `let close = "*/";` into a one-line bypass for everything between them.
It also understands char literals (`let q = '"';` must not flip its phase, while
`&'a str` is a lifetime, not a delimiter) and **raw strings** — `r"\\?\"` has no
escapes, so a scanner honouring that backslash reads past the closing quote and
treats the rest of the file as string, hiding every violation after it. Five
such literals live under `hook_health/`, and they desynced the scanner for two
review rounds without any fixture noticing; the invariant is now asserted over
every Rust file the linter walks, not just over fixtures. The scanner blanks
comment characters in place, so offsets and line numbers are unchanged by
construction. A module docstring is prose no stripper sees, so a
line carrying `cross-lint: allow` is skipped outright — read from the *original*
line, so a trailing `// cross-lint: allow` in Rust is not itself blanked before
it can be seen. It is
verbose on purpose: `rg 'cross-lint: allow'` lists every escape in the tree
(there are two, both in `tests/test_ci_matrix.py`, where the tool names *are*
the assertion's data). The marker is **strictly line-scoped**, including inside
a multi-line docstring — it must sit on the same line as the tool name, not on
the docstring's closing line.

#### Timing evidence

The blessed path is the one that has run in CI since the matrix moved to
`strategy = soldr`, and the superseded direct-wrapper path is no longer
reachable — deliberately, since making it reachable is the thing this section
forbids. So there is no honest A/B measurement to record here, and a synthetic
one would mean re-introducing the very command path the lint rejects. What can
be said from the workflow runs: the crossed Apple lanes complete in ~5 minutes
and the MSVC lane in ~20–26, all on `ubuntu-24.04`, with native runners doing
no compilation at all. Anyone wanting a genuine comparison should take it from
soldr's own benchmarks rather than from a temporary regression here.

### macOS: SDK provisioning

`build.rs:27-28` emits `cargo:rustc-link-lib=framework=Accelerate`
unconditionally for any `target.contains("apple")`, with **no feature to turn it
off**. Linking `-framework Accelerate` requires a real macOS SDK. `GGML_BLAS=OFF`
stops CMake from *building* the BLAS backend, but does not remove that hardcoded
link directive — the SDK is still required for the framework stub, and for every
other system framework `cpal`/`rodio`/`arboard` pull in on darwin.

Linux→macOS cross therefore needs a real SDK on the runner. soldr 0.8.28's
blessed Apple-target path resolves the target-shaped SDK from its toolchain
catalogue. The setup composite runs `soldr prepare --target <apple-triple>`
and exports the resulting environment to later workflow steps.

That environment includes `SDKROOT` plus target-scoped compiler/linker
settings. `ci/xbuild.py` forwards the same path as `CMAKE_OSX_SYSROOT`, then
routes link-producing builds through `soldr build`. Both Darwin triples now
build on `ubuntu-24.04`; native macOS runners only download and execute the
bundles.

## Cache model

The old design's caches were fragmented by *job type*; the new one is fragmented
only by *target triple*, which is the minimum possible:

- `setup-soldr` with `cache-key-suffix: build-<triple>`. Six namespaces total,
  down from twelve, and each is now written by exactly one job per run rather
  than raced by three.
- The native lane runs `soldr-cook` with flags matching the requested profile.
  Cross lanes skip the cook because setup happens before their target SDK is
  prepared; a host cook cannot be reused by a foreign target. Their per-triple
  build caches still persist the real target artifacts.
- All six build jobs run the same runner image (`ubuntu-24.04`), so host-side
  artifacts — proc-macro crates, build scripts, `protox`/`prost-build` — have
  identical fingerprints across targets. They are still stored per-triple, but
  they compile against a warm toolchain and identical glibc.
- The venv cache key drops its `runs-on` component for build jobs (one OS) and
  keeps it for exec jobs (six OSes).
- `main` pushes run the `full` tier, so every triple's cache is refreshed on
  every merge; PR jobs restore from it via `restore-keys`.

## Bundles: what crosses the wire

`build` uploads one artifact per triple, `bundle-<triple>`:

```
bundle/
  manifest.json         # triple, profile, git sha, test-binary list
  bin/                  # clud, clud-shim, clud-block-bad-cmd, clud-cmd-scan,
                        # clud-ctrlc-probe, mock-agent, probe-*, scan_zombies
  tests/                # every `cargo test --no-run` harness binary
  dist/                 # the dev wheel (test_trampoline.py needs it)
```

`tests/` is populated from `cargo test --workspace --no-run --message-format=json`,
filtering `reason == "compiler-artifact"` entries that carry an `executable`.

The exec job reconstructs the layout the test code expects rather than requiring
the test code to learn a new one:

- `CLUD_TEST_BINARY`, `CLUD_TEST_BLOCK_BAD_CMD_BINARY`,
  `CLUD_TEST_MOCK_AGENT_BINARY` → `bundle/bin/*`. Every Python test honours
  these first (`tests/test_hello.py:58-60`, `tests/integration/conftest.py:181-200`,
  `tests/test_hook_stdin.py:36-48`), so no `cargo` fallback fires.
- `CARGO_TARGET_DIR` → a synthesized `bundle/target/debug/` containing the
  binaries. `crates/clud-bin/tests/common/mod.rs:33` reads `CARGO_TARGET_DIR` at
  *runtime*, so `mock_agent_path()` resolves for `pty_pump.rs` (13 tests),
  `pty_behavior.rs` (6) and `orphan_reap.rs` (1) with **no source change**, and
  the zombie-scan autouse fixture (`tests/integration/conftest.py:326-355`) stops
  silently degrading to a no-op.

### The one required source change

`env!("CARGO_BIN_EXE_*")` bakes the **builder's absolute path** into the test
binary at compile time, with no runtime override. That breaks 13 tests when the
binary is executed on a different machine:

- `crates/clud-bin/tests/symbols.rs:35` (4 tests)
- `crates/clud-bin/tests/telemetry_endpoint.rs:33` (4 tests)
- `crates/clud-bin/tests/ctrlc_signal_kinds.rs:17` (4 tests, unix)
- `crates/clud-bin/tests/ctrlc_windows_events.rs:30` (1 test, windows)

Fix: a shared `common::bin_path("clud")` helper that prefers a runtime
`CLUD_TEST_BIN_DIR` env var and falls back to the `env!` constant, so local
`cargo test` is unchanged. This is the only production/test source edit the
redesign requires; everything else is CI plumbing.

## Release profile containment

Requirement: nothing builds `--release` except the release pipeline.

- `_build-target.yml` takes `profile` (`dev` | `release`), defaulting to `dev`.
- `ci.yml` never passes `profile`, and has no input that could set it.
- `_build-target.yml` opens with a guard step that fails when
  `profile == 'release'` and `github.workflow != 'Auto Release'`. A reusable
  workflow sees the *calling* workflow's name, so this is enforceable in YAML
  and cannot be bypassed by a `workflow_dispatch` on the template.
- The 24 deleted leaf workflows each carried a `build-mode: [dev, release]`
  dispatch choice — six user-reachable paths to a release build outside the
  release pipeline. Deleting them closes that surface.
- `--zig --compatibility manylinux2014` (`ci/build_wheel.py:48-51`) stays on the
  release path only; CI dev wheels are plain `--profile dev`.

### Debug info goes to a sidecar, not into the wheel

PyPI caps a project's individual files at 100 MB. `[profile.release]` set
`debug = "line-tables-only"` with no `split-debuginfo`, and on ELF that DWARF is
embedded in the binary while Windows/macOS write it to a `.pdb` / `.dSYM` the
wheel never sees. That asymmetry made the manylinux wheel ~7x the others; it
crossed 100 MB at 2.7.2 and killed the PyPI upload for 2.7.2, 2.7.3 and 2.7.4.
Because `publish-pypi` failed, the dependent `publish-release` job was skipped,
so those tags produced no GitHub release either — the pipeline was silently
broken for three tags.

`split-debuginfo = "packed"` fixes it at the source: it emits a `.dwp` on ELF
and drops `.debug_info` / `.debug_str` from the binary. Measured against the
2.7.4 artifact: 72–91 MB of the 137 MB `clud` binary moves out, projecting the
wheel from 105 MB to ~40 MB.
`.debug_line` stays embedded, so a panic still resolves file:line unaided (and
function names still come from `.symtab`). The inlined-subroutine DIEs are what
move, so without the `.dwp` a backtrace collapses to one file:line per physical
frame -- the sidecar is not optional for full-fidelity traces.

Only Linux changes. `packed` is already MSVC's default. On Apple it would select
the `.dSYM` bundle, which means rustc runs `dsymutil` at link time -- the one
way this setting could plausibly break a cross lane. It cannot: `soldr prepare`
probes for dsymutil and, when it is missing, exports `-Csplit-debuginfo=off` in
both `CARGO_TARGET_<T>_RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` for Apple
triples, which outranks the profile key. Verified by inspecting `soldr prepare
--github-env` output per triple: the override is emitted for
`aarch64-apple-darwin` and for neither `x86_64-unknown-linux-gnu` nor
`x86_64-pc-windows-msvc`. That asymmetry is load-bearing -- if soldr ever
emitted it for linux-gnu, this fix would silently stop working and the wheel
would grow back.

The `.dwp` is attached to the GitHub release, and the routing is the part to not
break:

- `ci/xbuild.py::collect_debuginfo` stages it under `dist-debuginfo/`,
  **never** `dist/`. `_build-target.yml` uploads `dist/*` as the `wheels-*`
  artifact and `publish-pypi` hands that to twine as `packages-dir`; a
  non-package file there is the exact 400 this change removes.
- It ships as its own `debuginfo-<triple>` artifact, which `wheels-*` cannot
  match. Only `publish-release` downloads that pattern.
- Every hop is non-fatal (`if-no-files-found: ignore`, `continue-on-error`,
  `fail_on_unmatched_files: false`), because targets where `packed` writes no
  `.dwp` must not block a release. The guarantee `fail_on_unmatched_files` used
  to give is asserted explicitly instead: the checksums step runs
  `ls dist/*.whl`.

Only `clud` itself gets a `.dwp`. It is the sole binary that installs the crash
reporter, and the shims currently share its whole dep tree, so publishing all
five would put ~900 MB of near-duplicate DWARF on every release page.

### The manylinux glibc floor is `--compatibility`, not `--target`

`ci/xbuild.py::manylinux_wheel_env` owns this and is the only place that should.
Three traps, all of which shipped a red release run before being understood:

1. **The floor cannot be spelled on `--target`.** `cargo zigbuild` accepts
   `x86_64-unknown-linux-gnu.2.17`; maturin does not — it hands `--target`
   straight to `target-lexicon`, which rejects the suffix as an unknown triple.
   maturin *derives* `<triple>.2.17` itself from the manylinux platform tag and
   passes that to zig, so `--compatibility manylinux2014 --zig` is the entire
   mechanism. Neither does `soldr prepare` take the suffix (soldr#2139 is on
   `main`, not in the pinned 0.8.30).
2. **`soldr prepare`'s exports outrank zig's.** cargo-zigbuild installs its
   2.17-floored `cc`/`c++`/`ar`/`ranlib` shims and the linker with
   `add_env_if_missing`, which also consults the ambient environment. Because
   `soldr prepare` exports `CC_<triple>`, `CARGO_TARGET_<T>_LINKER` and friends
   for the compile and test steps, the wheel build silently split its
   toolchain — Rust at 2.17, the whisper.cpp C/C++ at soldr's default — and the
   audit rejected it for `GLIBC_2.25/2.27/2.28`. The fix is to drop those
   variables for the wheel build, which is safe only because no `*-sys` crate
   needs the prepared sysroot on Linux (`cpal` is cfg'd off there).
3. **Only the cross lanes get a zig on PATH.** cargo-zigbuild resolves zig as
   `which(python3) -m ziglang` then `which(zig)`. On the native `x86_64` lane
   `python3` is the hosted-tool interpreter (no `ziglang`) and `soldr prepare`
   is skipped, so the release wheel died with "Failed to find zig" while the
   ARM lane sailed past it. `CARGO_ZIGBUILD_PYTHON_PATH=sys.executable` names
   the venv interpreter that does have `ziglang` (via the `maturin[zig]` dev
   dep), uniformly on every lane.

None of this is exercised by `ci.yml` — `_build-target.yml` refuses
`profile: release` outside Auto Release — so the release wheel path is only
ever proven by a real tag. Treat `manylinux_wheel_env`'s unit tests in
`tests/test_ci_xbuild.py` as the standing contract.

## Deduplicated checks

| Check | Before | After |
| --- | --- | --- |
| `ruff` | 6x | 1x (`static`) |
| `cargo fmt --check` | 6x | 1x (`static`) |
| `ci/banned_imports.py` | 6x | 1x (`static`) |
| `ci/banned_cross_tools.py` | — (#637; new) | 1x (`static`) |
| `cargo clippy --workspace --all-targets` | 6x native | 2x, both on Linux |
| dylint | 2x per PR (`push` + `pull_request` both fire) | 0x per PR; 1x on merge/main, Linux only |
| Rust doc-tests | 6x | 1x (host triple) |

`ci/lint.py` gains `--static-only`, and the checks inside it are reordered
cheapest-first (ruff → banned imports → banned cross tools → `cargo fmt`) so
the most common failure
reds out in seconds instead of behind a cargo subprocess. `bash lint` with no
flags still runs the whole suite, unchanged.

**Clippy runs on two triples, not six.** It is worth stating why, because the
obvious intuition is wrong: clippy is *not* nearly free once the dependency
graph is warm. `cargo clippy` builds a **Check-mode** unit graph, emitting
`.rmeta` under a different unit hash than the `.rlib` that `cargo build` and
`cargo test --no-run` need. Cargo's unit mode is part of the fingerprint, so
there is no reuse in either direction and reordering the steps does not help —
clippy is a second full pass over all ~429 dependencies. Since the platform
gating in this workspace is by OS rather than architecture, `x86_64-unknown-linux-gnu`
plus `x86_64-pc-windows-msvc` type-check every `cfg(windows)` / `cfg(unix)`
branch. The other four triples would pay a full extra pass for no new coverage.

**dylint is off the PR path.** It is Linux-only by construction — it needs a
nightly toolchain with `rustc-dev` and `llvm-tools` and builds a cdylib driver
for the host — so requirement (3) ("no dylint off Linux") is satisfied
structurally: `_dylint.yml` pins `ubuntu-24.04` and nothing else can reach it.
But it is also ~25 minutes of cold nightly work, which would make a
slash-normalization style lint the longest pole in every PR. It now runs on
`merge_group` / `main` / manual dispatch and gates the merge rather than the PR.
The old `dylint.yml` also fired on both `push` and `pull_request`, so it ran
twice per PR.

**Doc-tests run once.** They were covered by the old `cargo test --workspace`
but produce no harness binary, so they cannot ride along in a bundle. They are
OS- and architecture-independent, so one run on the host triple is full
coverage rather than a reduction.

## Template inventory

Deliberately small, to keep the ~60 lines of soldr/uv/mold boilerplate that is
currently copy-pasted four times in exactly one place.

| File | Role |
| --- | --- |
| `.github/actions/setup-build/action.yml` | composite: python + uv + venv cache + mold + `setup-soldr` + `uv sync`. Used only by build-side jobs. |
| `.github/actions/setup-exec/action.yml` | composite: python + uv + `uv sync --group test`, then **deletes** the Rust toolchain. Used by exec jobs. |
| `.github/workflows/_build-target.yml` | reusable: one triple → one bundle (+ optional wheel/sdist artifact). Called by `ci.yml` and `auto-release.yml`. |
| `.github/workflows/_run-tests.yml` | reusable: one triple × one suite → test execution. |
| `.github/workflows/_dylint.yml` | reusable + dispatchable: the nightly Linux lint. |
| `.github/workflows/ci.yml` | the only push/PR entrypoint. |
| `.github/workflows/auto-release.yml` | unchanged triggers; now the sole caller that may pass `profile: release`. |
| `ci/ci_matrix.py` | the triple table, shared by CI and the release matrix. |
| `ci/xbuild.py` | every cargo/maturin invocation + the per-strategy cross environment. |
| `ci/bundle.py`, `ci/run_bundle.py` | pack the bundle / execute it on the exec runner. |

Deleted: 24 `{linux,macos,windows}-{x86,arm}-{build,lint,unit-test,integration-test}.yml`,
plus `_lint.yml`, `_unit-test.yml`, `_integration-test.yml`, `_build.yml`,
`dylint.yml`.

### Two traps worth naming

**Never use `uv run` in a workflow step.** `pyproject.toml` sets
`build-backend = "soldr"`, so `uv run` syncs the *project*, which triggers a
full PEP 517 maturin build of the Rust binary before your command starts. On a
build job that is a wasted host-wheel build; on an exec job it hits the removed
toolchain and fails every test. Both composite actions export `$VENV_PY`
pointing at the synced interpreter — use that. The repo's `lint` script
(`lint:8-24`) already worked around this for the same reason.

**Shadowing cargo on PATH is not enough on Windows.** Rust's
`Command::new("cargo")` goes through `CreateProcess`, which only appends
`.exe` — a `cargo.cmd` shim is skipped and the real `cargo.exe` found. Since the
test suite spawns cargo from Rust (`crates/clud-bin/tests/common/mod.rs:78-116`),
`setup-exec` deletes the toolchain binaries outright and then installs failing
shims for the error message. Runner VMs are ephemeral, so this is safe.

## Expected effect

Per PR push, `core` tier:

| | Before | After |
| --- | --- | --- |
| Workflows triggered | 12 | 1 |
| Full workspace compiles | ~12–18 | 3 |
| Clippy passes | 6 | 2 |
| Cache namespaces | 12 | 3 (of 6) |
| macOS runner jobs | 4 cold builds | 2 exec only |
| Windows runner jobs | 4 cold builds | 2 exec only |
| Platform-independent lint runs | 18 | 3 |
| dylint runs | 2 | 0 |

Critical path is `build-<triple>` → `test-<triple>` per lane, in parallel across
lanes, with `static` failing fast alongside. The Linux lane reports red/green
roughly 15 minutes before the slowest lane finishes, because no lane waits on
another.

### Known remaining costs

Not fixed here, recorded so they are not rediscovered:

- **Cross-lane cold starts.** The native lane can reuse `soldr-cook`, but cross
  lanes prepare their SDK after setup-soldr and deliberately skip a host-only
  cook. A miss in the per-triple target/build cache still means compiling the
  foreign dependency graph once.
- **The 10 GB per-repo Actions cache quota.** Six per-triple `target/` caches
  plus the dylint cache plus the venv caches plausibly exceed it, and eviction
  is silent — `restore-keys` simply miss and the job rebuilds cold.
  `CARGO_INCREMENTAL=0` (set in `_build-target.yml`) is the cheap mitigation
  already applied; splitting deps from workspace crates in the cache key, and
  sharing the host-side proc-macro/build-script units across all six triples,
  are the next steps.
- **The linux-x86 lane still round-trips through an artifact** even though its
  build and exec runners are the same class. Running its suites inline would
  save ~3–5 minutes at the cost of the uniform template and the structural
  "exec cannot compile" guarantee.
- **Bundle size.** Each harness statically links the whole workspace; per-OS
  filtering is applied in `ci/bundle.py`, but `split-debuginfo = "packed"` plus
  excluding the debug files from the bundle would cut substantially more.

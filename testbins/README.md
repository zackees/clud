# testbins/

Rust auxiliary binaries used only by the `clud` test suite. These crates are not shipped with the Python wheel or distributed to end users; they exist solely to support integration testing.

They live outside `crates/` to keep production code clearly separated from test-only fixtures.

## Binaries

- [`mock-agent/`](mock-agent/README.md) — Stand-in for the `claude` and `codex` backends, used by Python integration tests to exercise `clud`'s command-building and execution paths without invoking a real agent.
- [`daemon-stub/`](daemon-stub/README.md) — Long-lived daemon stand-in for the reaper survival suite (#674). Reproduces the three *signal shapes* clud's spare-list keys on — job breakaway, the cooperative daemon marker, and an own-detach process owning a listening socket (the sccache shape) — so the suite never depends on a real sccache/docker/soldr install.
- [`probe-target/`](probe-target/README.md) — Windows-only target process for the ignored #468 Win32 hooking feasibility probe.
- [`probe-dll/`](probe-dll/README.md) — Minimal Windows `cdylib` loaded by the ignored #468 injection probe.

## How they're built

Each subdirectory is a regular Cargo workspace member declared in the root [`Cargo.toml`](../Cargo.toml). Build any of them explicitly with:

```
soldr cargo build -p <crate-name>
```

The test harness (`ci/test.py` and the Python `conftest.py`) builds the required testbins on demand before running integration tests, so manual builds are usually unnecessary.

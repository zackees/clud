# testbins/

Rust auxiliary binaries used only by the `clud` test suite. These crates are not shipped with the Python wheel or distributed to end users; they exist solely to support integration testing.

They live outside `crates/` to keep production code clearly separated from test-only fixtures.

## Binaries

- [`mock-agent/`](mock-agent/README.md) — Stand-in for the `claude` and `codex` backends, used by Python integration tests to exercise `clud`'s command-building and execution paths without invoking a real agent.

## How they're built

Each subdirectory is a regular Cargo workspace member declared in the root [`Cargo.toml`](../Cargo.toml). Build any of them explicitly with:

```
soldr cargo build -p <crate-name>
```

The test harness (`ci/test.py` and the Python `conftest.py`) builds the required testbins on demand before running integration tests, so manual builds are usually unnecessary.

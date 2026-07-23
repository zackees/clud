# Benchmarks

Standalone, opt-in benchmarks live here rather than under `tests/`: they may
intentionally exceed the repository's 90-second pytest timeout and must never
be collected by default CI. See [idle_cpu](idle_cpu/README.md) for the
idle-session CPU harness used by #542.

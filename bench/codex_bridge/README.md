# Codex-via-Claude bridge benchmark

This opt-in benchmark exercises the real loopback request path against the
in-process Responses-protocol fake: Messages parsing, translation, bounded
upstream read, SSE translation, chunked downstream write, and orderly bridge
shutdown. It neither contacts OpenAI nor accepts credentials.

Run from the repository root on a quiet machine:

```bash
soldr cargo test --release -p clud --lib codex_bridge::tests::benchmark_loopback_bridge_overhead -- --ignored --nocapture
```

The JSON line records request count, elapsed time, throughput, RSS before and
after the run, OS, and architecture. It deliberately has no normal-CI
wall-clock threshold: hardware, antivirus, allocator behavior, and concurrent
host load make one flaky. Treat a sustained throughput regression or RSS growth
that fails to settle across repeated quiet runs as a finding; attach the raw
secret-free JSON report to the PR that changes the bridge.

For a baseline, run the command three times after boot or after an idle period,
record the median throughput and the largest RSS growth, and compare later
changes on the same OS/architecture. The benchmark is not collected by default
CI because it is marked `#[ignore]`.

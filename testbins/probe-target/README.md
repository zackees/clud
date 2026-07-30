# probe-target/

Deterministic Windows-only target process for
`crates/clud-bin/tests/win32_hooking_probe.rs`.

The binary is test-only and is not shipped with `clud`. It exposes small
subcommands for sleeping, holding a file handle, spawning a process chain, and
attempting `CREATE_BREAKAWAY_FROM_JOB` so the ignored Win32 probe can validate
job-object lifecycle, handle enumeration, and injection primitives.

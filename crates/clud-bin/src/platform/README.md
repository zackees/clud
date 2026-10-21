# Platform-specific kernel fixtures

`process_identity_zombie_tests.rs` contains the Linux-only `waitid(WNOWAIT)`
regression for zombie liveness (#1993). `process_identity.rs` includes it as
a test module so the native API and host selection stay inside this boundary.
The raw child remains unreaped during direct and batched identity observations,
then is reaped before the assertions. No production wait API is introduced.

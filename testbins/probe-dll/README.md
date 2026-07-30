# probe-dll/

Minimal Windows `cdylib` used only by
`crates/clud-bin/tests/win32_hooking_probe.rs`.

When injected with `CreateRemoteThread(LoadLibraryW)`, `DllMain` appends a
single `INJECTED pid=...` line to the path in `CLUD_PROBE_DLL_SINK`. The DLL is
not shipped with `clud` and has no production call path.

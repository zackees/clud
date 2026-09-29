# RM protection container checks

The manual/reusable `rm-protection-docker.yml` workflow builds the current
`clud` binary, links its `clud-cmd-scan` and `clud-shim` aliases
(`clud __link-aliases`) and copies them into a
disposable Ubuntu image. It runs two checks:

1. `tests/test_rm_shim.py::test_old_stub_corpus_maps_to_new_floor_and_hook`
   maps every case in `stub_cases.json` to the catastrophe-only shim verdict
   and the agent-hook rewrite. The next PATH entry is a recording stub, so a
   refused operand cannot reach a real removal executable.
2. `tests/test_mount_floor.py` runs against a read-only bind of a disposable
   Docker volume inside a writable tmpfs. A mounted operand is refused before
   handoff, and deleting its parent cannot remove the mounted content.

The image runs unprivileged, with no network, a read-only root filesystem, and
no host-writable bind mounts. Only the second check may invoke real GNU `rm`,
and then only with paths inside the disposable mount probe. The local bosn
tasks `mount-probe`, `rm-corpus-alpine`, and `rm-busybox` cover the corresponding
GNU, corpus, and musl/BusyBox cases with warm managed build volumes.

The former variable-proof stress corpus and CI/Docker execution gate were
removed with the old agent-authored `rm` analyzer. See
[the deletion architecture](../../../docs/architecture/rm-protection.md) for
the current shim and hook boundaries.

<!-- managed-by: clud -->

# clud-docker-linux-build skill

Source asset for the bundled `/clud-docker-linux-build` Claude skill. See [SKILL.md](SKILL.md) for the agent-facing prose.

Registered in `crates/clud-bin/src/skills.rs`'s `BUNDLED_SKILLS` array; installed into the user's skills directory by the standard install lifecycle.

Pairs with the `docker/` bundled tools at `crates/clud-bin/assets/tools/docker/`:

| Component | Purpose |
|---|---|
| `clud-docker-linux-build` (this skill) | Agent-facing prose: when to reach for the tool, the one-rule volume contract, path-conversion table, mtime gotchas, when NOT to use. |
| `docker/docker-build.py` + `docker/docker_build_<stack>.py` (bundled tools) | The actual implementation — Dockerfiles, entry scripts, subcommand surface. |

Origin: zackees/clud#416 (design), zackees/clud#421 (implementation slice).

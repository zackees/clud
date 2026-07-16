<!-- managed-by: clud -->

# clud-docker-recover skill

Source asset for the bundled `/clud-docker-recover` skill. It pairs the
agent-facing safety and restart workflow in [SKILL.md](SKILL.md) with the
standard-library diagnostic tool at
[`../../tools/docker/docker_recover.py`](../../tools/docker/docker_recover.py).

The skill is registered in `crates/clud-bin/src/skills.rs`; the tool is
registered in `crates/clud-bin/src/tools.rs`. Both are embedded into the
binary and installed through the normal Clud lifecycle.

Origin: zackees/clud#531.

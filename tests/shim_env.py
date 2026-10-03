"""Session env for driving `clud-shim` aliases in process tests (#1546).

Every clud alias fails open outside a valid session, so a test that wants the
in-session behavior (the rm floor, the gh watch upgrade, the python target)
must build the session the way `shim_session::activate_rm` does. The stamp's
name and value come from the built binary's own registry, never a literal.
"""

from __future__ import annotations

import functools
import json
from pathlib import Path

from tests import process


@functools.cache
def _registry(shim: str) -> dict:
    result = process.run(
        [shim, "--registry"], capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 0, result
    return json.loads(result.stdout)


def registry(shim: Path) -> dict:
    """`clud-shim --registry`: the ABI stamp and every registered alias."""
    return _registry(str(shim))


def session_env(shim: Path, shim_dir: Path) -> dict[str, str]:
    """The keys every in-session alias needs: the ABI stamp and alias dir."""
    info = registry(shim)
    return {
        info["abi_key"]: info["abi"],
        info["session_dir_key"]: str(shim_dir),
        # A session without this key reads the user's settings and reaches
        # the user's own daemon (#1743); a test opts in explicitly.
        "CLUD_GH_READ_BROKER": "0",
    }


def session_key_names(shim: Path) -> set[str]:
    """Every session key any alias reads, for building an empty session."""
    info = registry(shim)
    names = {info["abi_key"], info["session_dir_key"]}
    for spec in info["shims"]:
        names.update(spec["session_keys"])
    return names

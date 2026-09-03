"""Integration: every clud-launched bash runs under `set -u` (issue #1066).

Phase 2 of #1064. `rm -rf "$SP"/` with an unset `$SP` expands to `rm -rf /`
in a stock shell. Neither the `block_bad_cmd` hook nor the `tap` gate can
catch the general case, because **redirections are performed by the shell,
not the wrapped program**: `tap cmd > "$VAR/out"` truncates whatever the shell
resolves and no argv inspection ever sees it. DD-056 records that residual
risk and names `set -u` as the proportionate mitigation.

These run the probe from inside the mock agent rather than from the test
process. The agent is the process clud launched, so the shell it spawns
inherits exactly the environment clud built -- which is the claim under test.
Probing from pytest would only prove that bash honours `BASH_ENV`.

Per #1066: the incident class is verified with synthetic payloads and mock
agents. A live `rm -rf` is never an acceptable test, container or not, so the
probe is an `echo` of the same expansion.
"""

from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

import pytest

from tests import process

from ._daemon_helpers import copy_launcher, run_clud

# Skipped on Windows for lane hygiene, NOT because the policy is absent there.
# nounset is not cfg-gated: Claude Code runs Git Bash for Bash tool calls on
# Windows -- that is the whole premise of #753's completion_guard, which this
# is wired alongside -- so `BASH_ENV` arms there too. What this file asserts is
# not Windows-specific, the Windows integration lanes already carry several
# persistent unrelated reds, and the Windows-side guarantee is covered by the
# builder parity tests in `daemon::io_helpers`, which run on the Windows lib
# harness. Claiming the policy no-ops here would be the same
# coverage-that-is-not-there failure this change exists to prevent.
pytestmark = [
    pytest.mark.integration,
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX bash only"),
]

# 30s for the same reason as `test_mock_agents` (#994): a real clud launch
# through the mock agent on a shared runner, where 15s is close enough to the
# observed spread to fail on load alone.
_TIMEOUT = 30


def _probe(
    clud: Path, env: dict[str, str], report: Path
) -> tuple[process.CompletedProcess[str], dict]:
    """Launch clud so the mock agent runs one bash command, and read back
    what that shell did."""
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
        launch = Path(temp_dir) / clud.name
        copy_launcher(clud, launch)
        result = run_clud(
            [str(launch), "-p", "hello", "--", "--mock-bash-nounset-probe", str(report)],
            timeout=_TIMEOUT,
            env=env,
        )
    assert report.is_file(), (
        f"the mock agent never wrote its report; clud may not have launched it.\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return result, json.loads(report.read_text(encoding="utf-8"))["bash_nounset_probe"]


def test_unset_variable_aborts_the_shell(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    _, probe = _probe(clud_binary, mock_env, tmp_path / "armed.json")

    assert probe["spawned"], probe
    assert probe["bash_env"], "clud must have set BASH_ENV on the backend"
    # The failure has to be loud and name the variable. A nonzero exit alone
    # would also be satisfied by a bash that broke for some unrelated reason.
    assert probe["exit_code"] not in (0, None), probe
    assert "unbound variable" in probe["stderr"], probe
    assert "SP" in probe["stderr"], probe
    # And it must not have run: an empty expansion reaching the command is the
    # entire incident.
    assert probe["stdout"] == "", probe


def test_opt_out_restores_stock_shell_behaviour(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """The escape hatch, proven end to end rather than at the unit seam.

    This is a behaviour change for every clud-launched session, so the way out
    has to work from the environment a user can actually set."""
    env = {**mock_env, "CLUD_NO_BASH_NOUNSET": "1"}

    _, probe = _probe(clud_binary, env, tmp_path / "opted-out.json")

    assert probe["spawned"], probe
    assert probe["exit_code"] == 0, probe
    assert probe["stdout"] == "[]", probe
    assert "unbound variable" not in probe["stderr"], probe

"""The gate, end to end: a gated session never executes an unset-variable removal.

#1067 acceptance criterion 3 -- "a gated session plus an unset-variable
removal is denied at the hook (unwrapped) or at `tap` (wrapped), never
executed."

Two halves of one guarantee, and the point is that *both* doors are shut:

* **Unwrapped.** `rm -rf "$SP"/` without the wrapper prefix is refused by the
  hook, before a shell ever expands it.
* **Wrapped.** `tap rm -rf "$SP"/` satisfies the hook's prefix requirement, so
  the hook allows it through -- and `tap` then sees the *expanded* argv, where
  `$SP` has already become nothing and the target is `/`, and refuses there.

The hook cannot catch the second case (that is why `tap` exists) and `tap`
never sees the first (nothing ran it). Testing only one would leave the other
door open and look like coverage.

Nothing here removes anything: the hook decides from text, and `tap` decides
from argv. Per #1067, "Never validate a removal guard by performing a removal
-- not on a host, not in a container."
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _tool_binary(name: str) -> Path | None:
    """Resolve a built workspace binary the way the hook tests do."""
    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary:
        sibling = Path(clud_binary).with_name(_binary_name(name))
        if sibling.is_file():
            return sibling

    bin_dir = os.environ.get("CLUD_TEST_BIN_DIR")
    if bin_dir:
        candidate = Path(bin_dir) / _binary_name(name)
        if candidate.is_file():
            return candidate

    resolved = shutil.which(_binary_name(name))
    return Path(resolved) if resolved else None


def _require(name: str) -> Path:
    found = _tool_binary(name)
    if found is None:
        pytest.skip(f"{name} not built")
    return found


def _hook(tmp_path: Path, repo: Path, command: str) -> process.CompletedProcess[str]:
    """Run the cmd-scan hook on `command` with the gate enforcing."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env.pop("CLUD_BAD_CMD_OVERRIDE", None)
    # The gate is opt-in; a session is only gated where clud set this.
    env["CLUD_CMD_GATE"] = "enforce"
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "cwd": str(repo),
            "tool_input": {"command": command},
        }
    )
    return process.run(
        [str(_require("clud-cmd-scan"))],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )


# What the agent actually typed in #1064: a removal whose target is a variable
# that is never set. The shell expands it to nothing, so this becomes
# `rm -rf /`.
UNSET_VARIABLE_REMOVAL = 'rm -rf "$SP"/'


def test_the_unwrapped_removal_is_denied_at_the_hook(tmp_path: Path) -> None:
    """Door one: no wrapper prefix, so the hook refuses before any shell runs.

    Exit 2 is the hook protocol's "deny"; anything else means the command would
    have reached a shell."""
    repo = tmp_path / "repo"
    repo.mkdir()

    result = _hook(tmp_path, repo, UNSET_VARIABLE_REMOVAL)

    assert result.returncode == 2, (
        f"the gate must deny an unwrapped command: rc={result.returncode} "
        f"stdout={result.stdout} stderr={result.stderr}"
    )


def test_the_gate_denies_even_a_harmless_unwrapped_command(tmp_path: Path) -> None:
    """The gate is an allowlist, not a denylist (DD-056).

    `rm -rf ./scratch` is fine by every other guard and is still refused
    unwrapped -- being an allowlist is the property, and a test that only used
    dangerous commands could not tell the two designs apart."""
    repo = tmp_path / "repo"
    repo.mkdir()

    result = _hook(tmp_path, repo, "rm -rf ./scratch")

    assert result.returncode == 2, (
        f"unwrapped is unwrapped, however safe: rc={result.returncode} stderr={result.stderr}"
    )


def test_the_wrapped_removal_rewrites_and_tap_still_denies_outside_paths(tmp_path: Path) -> None:
    """Agent deletion rewrites to the profile tool; tap independently rejects outside paths."""
    repo = tmp_path / "repo"
    repo.mkdir()

    # The agent hook rewrites direct deletion to the profile-scoped tool.
    # `tap` remains a separate guard for scripts that explicitly call it.
    hook = _hook(tmp_path, repo, "tap rm -rf /etc/passwd")
    assert hook.returncode == 0, hook
    output = json.loads(hook.stdout)["hookSpecificOutput"]
    assert output["permissionDecision"] == "allow"
    assert output["updatedInput"]["command"] == "safe-rm -rf /etc/passwd"

    tap = _require("tap")
    env = os.environ.copy()
    env["CLUD_SESSION_ROOT"] = str(repo)
    refused = process.run(
        [str(tap), "rm", "-rf", "/etc/passwd"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(repo),
        timeout=30,
    )

    assert refused.returncode == 126, (
        f"tap must refuse a target outside the session root: "
        f"rc={refused.returncode} stderr={refused.stderr}"
    )
    assert "outside the session root" in refused.stderr, refused.stderr
    assert refused.stdout == "", refused.stdout


def test_the_expanded_root_removal_is_refused_by_tap(tmp_path: Path) -> None:
    """The #1064 shape as `tap` receives it.

    By the time the wrapper runs, `rm -rf "$SP"/` with an unset `SP` *is*
    `rm -rf /`. No variable remains to reason about, which is what makes this
    check exact rather than heuristic."""
    repo = tmp_path / "repo"
    repo.mkdir()

    tap = _require("tap")
    env = os.environ.copy()
    env["CLUD_SESSION_ROOT"] = str(repo)
    refused = process.run(
        [str(tap), "rm", "-rf", "/"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(repo),
        timeout=30,
    )

    assert refused.returncode == 126, refused.stderr
    assert "filesystem root" in refused.stderr, refused.stderr
    assert refused.stdout == "", refused.stdout


def test_the_wrapped_unset_variable_is_rewritten_by_the_hook(tmp_path: Path) -> None:
    """The hook rewrites the agent command; operand safety is checked later."""
    repo = tmp_path / "repo"
    repo.mkdir()

    result = _hook(tmp_path, repo, f"tap {UNSET_VARIABLE_REMOVAL}")

    assert result.returncode == 0, result
    output = json.loads(result.stdout)["hookSpecificOutput"]
    assert output["permissionDecision"] == "allow"
    assert output["updatedInput"]["command"] == 'safe-rm -rf "$SP"/'


def test_the_gate_is_off_unless_the_session_enables_it(tmp_path: Path) -> None:
    """Coverage is per-session, and that is deliberate (DD-056).

    Without `CLUD_CMD_GATE`, the same unwrapped command is not denied *by the
    gate*. This pins the revert story documented in DD-056: unsetting the
    variable restores post-#1064 behaviour, so a stale export cannot be what
    makes the suite pass."""
    repo = tmp_path / "repo"
    repo.mkdir()
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env.pop("CLUD_BAD_CMD_OVERRIDE", None)
    env.pop("CLUD_CMD_GATE", None)
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "cwd": str(repo),
            "tool_input": {"command": "ls -la"},
        }
    )
    result = process.run(
        [str(_require("clud-cmd-scan"))],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )

    assert result.returncode == 0, (
        f"an ungated session must not be gated: rc={result.returncode} stderr={result.stderr}"
    )


def test_an_ordinary_wrapped_command_is_allowed_through_both(tmp_path: Path) -> None:
    """The gate has to pass legitimate work, or its false-positive rate is 100%.

    #1067 makes measuring that rate a precondition for enabling the gate more
    widely; this is the smallest version of the same question."""
    repo = tmp_path / "repo"
    repo.mkdir()

    hook = _hook(tmp_path, repo, "tap ls -la")
    assert hook.returncode == 0, hook.stderr

    tap = _require("tap")
    env = os.environ.copy()
    env["CLUD_SESSION_ROOT"] = str(repo)
    allowed = process.run(
        [str(tap), "rm", "-rf", "scratch-that-does-not-exist"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(repo),
        timeout=30,
    )
    assert "tap: refusing" not in allowed.stderr, allowed.stderr
    assert allowed.returncode != 126, "126 is tap's refusal code"

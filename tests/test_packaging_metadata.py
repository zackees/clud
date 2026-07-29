"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import re
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent


def _pyproject() -> dict:
    with (ROOT / "pyproject.toml").open("rb") as handle:
        return tomllib.load(handle)


def test_declared_versions_move_in_lockstep() -> None:
    """`pyproject.toml`, the Cargo workspace, and `clud.__version__` agree.

    This exists because they did not. `src/clud/__init__.py` sat at 2.2.0
    while the other two were at 2.4.1 — five releases of drift — so
    `import clud; clud.__version__` reported a version that had not shipped in
    months. Nothing caught it, because nothing compared them.

    The Python shim is the easy one to forget: neither maturin nor cargo ever
    reads it, so a stale value breaks no build and produces no error anywhere.
    Only a human importing the package sees the wrong number.
    """
    pyproject_version = _pyproject()["project"]["version"]

    cargo_match = re.search(
        r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(encoding="utf-8"), re.M
    )
    assert cargo_match, "no version in Cargo.toml"

    shim_match = re.search(
        r'^__version__ = "([^"]+)"',
        (ROOT / "src" / "clud" / "__init__.py").read_text(encoding="utf-8"),
        re.M,
    )
    assert shim_match, "no __version__ in src/clud/__init__.py"

    assert pyproject_version == cargo_match.group(1) == shim_match.group(1), (
        "version drift: "
        f"pyproject.toml={pyproject_version}, "
        f"Cargo.toml={cargo_match.group(1)}, "
        f"clud.__version__={shim_match.group(1)}"
    )


def test_pip_build_uses_soldr_pep517_backend() -> None:
    build_system = _pyproject()["build-system"]
    requirements = [requirement.lower() for requirement in build_system["requires"]]

    assert build_system["build-backend"] == "soldr"
    assert "backend-path" not in build_system
    assert requirements == ["soldr>=0.8.27"]


#: Every place a `zackees/setup-soldr` step can live. Since the CI redesign the
#: build-side pin lives in a composite action rather than a workflow, so a glob
#: over `.github/workflows` alone would silently stop checking the pin that
#: matters most.
def _setup_soldr_sources() -> list[Path]:
    paths = [
        *sorted((ROOT / ".github" / "workflows").glob("*.yml")),
        *sorted((ROOT / ".github" / "actions").glob("*/action.yml")),
    ]
    return [path for path in paths if "zackees/setup-soldr" in path.read_text(encoding="utf-8")]


def test_ci_setup_soldr_pins_backend_compatible_soldr() -> None:
    """Every soldr pin must satisfy pyproject's `soldr>=0.8.27`.

    The PEP 517 backend and the CI cargo shims share one daemon socket, and
    mixing protocol versions across it resets the connection (#580). Asserted
    as a version-ordering invariant rather than by matching literal substrings
    of one blessed version, which is what previously made this test depend on
    a single workflow file continuing to exist.
    """
    minimum = (0, 8, 27)
    sources = _setup_soldr_sources()
    assert sources

    action_pins: list[str] = []
    for path in sources:
        text = path.read_text(encoding="utf-8")
        action_pins.extend(re.findall(r"uses:\s*zackees/setup-soldr@(\S+)", text))
        for raw in re.findall(r"^\s*version:\s*\"?(\d+\.\d+\.\d+)\"?\s*$", text, re.MULTILINE):
            parts = tuple(int(part) for part in raw.split("."))
            assert parts >= minimum, f"{path}: soldr {raw} is below the required 0.8.27"

    assert action_pins
    assert all(pin == "v0.9.66" for pin in action_pins)


def test_ci_setup_soldr_only_cooks_reusable_native_dependencies() -> None:
    """Cross builds must not pay for a host-profile dependency cook.

    setup-soldr runs before `soldr prepare` provisions the foreign SDK. A cook
    in a zigbuild/soldr lane therefore produces host artifacts that the target
    build cannot reuse. The first PR validation run spent six minutes doing
    exactly that and then reported 393 misses with a 0% hit rate.
    """
    setup_workflows = _setup_soldr_sources()
    assert setup_workflows

    expected = "prebuild-deps: ${{ inputs.strategy == 'native' && 'soldr-cook' || 'none' }}"
    build_setup = ROOT / ".github" / "actions" / "setup-build" / "action.yml"
    assert expected in build_setup.read_text(encoding="utf-8")
    for path in setup_workflows:
        text = path.read_text(encoding="utf-8")
        assert "prebuild-deps: soldr-cook" not in text


def test_ci_setup_soldr_cook_profile_matches_the_real_build() -> None:
    setup = ROOT / ".github" / "actions" / "setup-build" / "action.yml"
    build = ROOT / ".github" / "workflows" / "_build-target.yml"

    setup_text = setup.read_text(encoding="utf-8")
    build_text = build.read_text(encoding="utf-8")
    assert "prebuild-deps-flags: ${{ inputs.profile == 'release' && '--release' || '' }}" in (
        setup_text
    )
    assert "profile: ${{ inputs.profile }}" in build_text

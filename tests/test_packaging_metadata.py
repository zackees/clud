"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import re
from pathlib import Path

import pytest
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


def _pinned_backend_soldr() -> str:
    """The exact soldr version `pyproject.toml` builds against."""
    build_system = _pyproject()["build-system"]
    requirements = [
        requirement.lower().replace(" ", "") for requirement in build_system["requires"]
    ]

    assert len(requirements) == 1, f"expected exactly one build requirement, got {requirements}"
    match = re.fullmatch(r"soldr==([\d.]+)", requirements[0])
    assert match, (
        f"build-system.requires must pin soldr exactly (soldr==X.Y.Z), got {requirements[0]!r}. "
        "An unbounded or floored requirement adopts every new soldr release "
        "without review — see DD-020 and issue #591."
    )
    return match.group(1)


def _workflow_paths() -> list[Path]:
    """Every workflow file. GitHub accepts `.yml` and `.yaml` equally."""
    directory = ROOT / ".github" / "workflows"
    return sorted(set(directory.glob("*.yml")) | set(directory.glob("*.yaml")))


def _setup_soldr_steps_in_text(
    workflow_name: str, text: str
) -> list[tuple[str, str, str]]:
    """Parse setup-soldr steps while requiring a literal ``with.version``."""
    steps: list[tuple[str, str, str]] = []
    lines = text.splitlines()
    uses_pattern = re.compile(
        r"^(?P<indent> *)(?P<dash>-\s+)?uses:\s*"
        r"zackees/setup-soldr@(?P<ref>\S+)\s*$"
    )

    for index, line in enumerate(lines):
        uses = uses_pattern.fullmatch(line)
        if uses is None:
            continue

        content_indent = len(uses.group("indent")) + len(uses.group("dash") or "")
        step_indent = content_indent - 2
        assert step_indent >= 0, f"{workflow_name}: malformed setup-soldr step indentation"

        step_end = len(lines)
        for candidate_index in range(index + 1, len(lines)):
            candidate = lines[candidate_index]
            stripped = candidate.lstrip()
            if not stripped or stripped.startswith("#"):
                continue
            indent = len(candidate) - len(stripped)
            if indent <= step_indent:
                step_end = candidate_index
                break

        with_index = next(
            (
                candidate_index
                for candidate_index in range(index + 1, step_end)
                if lines[candidate_index].strip() == "with:"
                and len(lines[candidate_index]) - len(lines[candidate_index].lstrip())
                == content_indent
            ),
            None,
        )

        version: re.Match[str] | None = None
        if with_index is not None:
            for candidate in lines[with_index + 1 : step_end]:
                stripped = candidate.lstrip()
                if not stripped or stripped.startswith("#"):
                    continue
                indent = len(candidate) - len(stripped)
                if indent <= content_indent:
                    break
                if indent != content_indent + 2:
                    continue
                version = re.fullmatch(
                    r"""version:\s*(?P<quote>["']?)(?P<version>[\d.]+)(?P=quote)\s*""",
                    stripped,
                )
                if version is not None:
                    break

        assert version, (
            f"{workflow_name}: setup-soldr step has no literal `with.version` input. "
            "A missing or computed value (a `${{ }}` expression, a matrix input) "
            "cannot be checked against the build-backend pin, which is the "
            "invariant DD-020 exists for."
        )
        steps.append((workflow_name, uses.group("ref"), version.group("version")))
    return steps


def _setup_soldr_steps() -> list[tuple[str, str, str]]:
    """Every `zackees/setup-soldr` step as (workflow name, action ref, version).

    Parsed without a YAML dependency, but indentation still matters: only a
    literal ``version`` that is a direct child of the action's ``with`` mapping
    counts. An unrelated ``env.version`` must not satisfy the lockstep guard.
    """
    steps: list[tuple[str, str, str]] = []
    for path in _workflow_paths():
        text = path.read_text(encoding="utf-8")
        steps.extend(_setup_soldr_steps_in_text(path.name, text))
    return steps


def test_setup_soldr_parser_rejects_version_outside_with() -> None:
    workflow = """
jobs:
  test:
    steps:
      - uses: zackees/setup-soldr@v0.9.66
        env:
          version: 0.8.28
        with:
          cache: true
"""
    with pytest.raises(AssertionError, match=r"literal `with\.version`"):
        _setup_soldr_steps_in_text("bad.yml", workflow)


def test_pip_build_uses_soldr_pep517_backend() -> None:
    build_system = _pyproject()["build-system"]

    assert build_system["build-backend"] == "soldr"
    assert "backend-path" not in build_system
    # No literal version asserted here on purpose: pinning one would make this
    # file a third edit site, contradicting the "bump these together" rule the
    # test below enforces. `_pinned_backend_soldr` already rejects any
    # non-exact requirement.
    assert _pinned_backend_soldr()


def _install_script_soldr() -> str:
    """The soldr version `./install` puts on a developer's PATH by default."""
    text = (ROOT / "install").read_text(encoding="utf-8")
    match = re.search(r'^VERSION="\$\{SOLDR_VERSION:-([\d.]+)\}"', text, re.M)
    assert match, 'install: no `VERSION="${SOLDR_VERSION:-X.Y.Z}"` default found'
    return match.group(1)


def test_soldr_versions_move_in_lockstep() -> None:
    """The build-backend pin, every CI toolchain pin, and `./install` agree.

    Issue #591: `setup-soldr`'s `version:` pins the *toolchain* soldr, while
    `pyproject.toml`'s `build-system.requires` resolves the *build backend*
    from PyPI independently at build time. When those drift, CI is testing a
    soldr that no build actually uses — which is how a branch pinned to a
    known-good 0.8.25 still ran 0.8.26's broken shim. `./install` is the
    third spelling: a developer whose local soldr trails CI's cannot
    reproduce a CI failure.

    Bumping soldr therefore means editing all three in the same commit.
    """
    steps = _setup_soldr_steps()
    assert steps, "no zackees/setup-soldr steps found in .github/workflows"

    expected = _pinned_backend_soldr()
    drift = [(name, version) for name, _, version in steps if version != expected]
    installed = _install_script_soldr()
    if installed != expected:
        drift.append(("install", installed))

    assert not drift, (
        f"soldr version drift: pyproject.toml pins {expected}, but "
        + ", ".join(f"{name} pins {version}" for name, version in drift)
        + ". Bump every site in the same commit — see DD-020."
    )

    action_pins = {ref for _, ref, _ in steps}
    assert action_pins == {"v0.9.66"}, (
        f"setup-soldr action refs disagree: {sorted(action_pins)}"
    )


def test_install_script_uses_wheel_with_legacy_fallback() -> None:
    """`./install` must handle the asset format the pinned release ships.

    soldr 0.8.x stopped publishing `.tar.gz`/`.zip` and ships `.tar.zst`,
    which needs a `zstd` binary the script cannot assume (git-bash on Windows
    has GNU tar but no zstd, so even `tar --zstd` fails with "Cannot exec").
    The script therefore prefers the release's wheel — a plain zip carrying
    the same binary — and keeps the legacy archive only as a fallback for
    0.7.x, which published no wheels.

    Without this, the lockstep test above is satisfiable by a script that
    404s on every download.
    """
    text = (ROOT / "install").read_text(encoding="utf-8")

    assert 'wheel_asset="soldr-${VERSION}-py3-none-${wheel_tag}.whl"' in text, (
        "install must resolve the wheel asset; the .tar.zst archives soldr "
        "0.8.x publishes cannot be extracted without a zstd binary"
    )
    assert "legacy_asset=" in text, "install must keep the 0.7.x archive fallback"
    assert 'if [[ "$VERSION" == 0.7.* ]]; then' in text, (
        "only soldr 0.7.x should use the legacy archive path"
    )
    assert '"$target_dir/python.exe" -c' in text
    assert '"$target_dir/python" -c' in text
    assert "command -v python3" in text


def test_ci_setup_soldr_skips_dependency_cook_on_windows() -> None:
    """Windows must never run the soldr dependency cook.

    Asserted as an invariant rather than as one exact literal. Two
    settings satisfy it: the usual `runner.os == 'Windows'` conditional,
    and a blanket `none` that disables the cook everywhere.

    The conditional form is in force. A blanket `none` was in force for a
    while as a workaround for zackees/soldr#1880 (hydrate restored
    `build-script-build` binaries without the executable bit, so cargo died
    with "Permission denied (os error 13)" on a rotating, arbitrary crate).
    That was fixed upstream by soldr#1889 and soldr#1914, shipped in
    v0.8.25/v0.8.26 — both below the v0.8.28 this repo pins — so the cook is
    enabled again everywhere except Windows.

    Both forms stay acceptable here because this test guards the Windows
    invariant, not which of the two ways it is currently achieved.
    """
    conditional = "prebuild-deps: ${{ runner.os == 'Windows' && 'none' || 'soldr-cook' }}"
    blanket = "prebuild-deps: none"

    setup_workflows = [
        path
        for path in _workflow_paths()
        if path.name.startswith("_") and "zackees/setup-soldr" in path.read_text(encoding="utf-8")
    ]

    assert setup_workflows
    for path in setup_workflows:
        text = path.read_text(encoding="utf-8")
        assert conditional in text or blanket in text, (
            f"{path.name}: prebuild-deps must either exempt Windows via the "
            f"runner.os conditional or disable the cook entirely with 'none'"
        )
        # Either way, no workflow may hand Windows an unconditional cook.
        assert "prebuild-deps: soldr-cook" not in text, (
            f"{path.name}: unconditional 'soldr-cook' would run the dependency "
            f"cook on Windows"
        )

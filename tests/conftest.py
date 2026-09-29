"""Shared pytest configuration for clud tests."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

import pytest

from ci import multicall_aliases, tracked_files
from ci.env import scrub_clud_session_env

# A clud session exports state such as CLUD_SKIP_RM_IDENTITY (auto-on in
# clud's own repo) and CLUD_ROUTE_CONTEXT. Inherited by the hook binaries
# under test, they flip the behavior those tests assert (#1423). Tests set
# only the CLUD_* variables they need; everything else starts clean.
scrub_clud_session_env(os.environ)

_REPO_ROOT = Path(__file__).resolve().parents[1]


def _materialize_multicall_aliases() -> None:
    """#1551: `clud` is multicall, so `clud-shim`, `clud-cmd-scan` and
    `clud-block-bad-cmd` are argv[0] aliases, not build outputs. Tests invoke
    them by path beside the `clud` under test; put them there once."""
    exe = ".exe" if sys.platform == "win32" else ""
    candidates = [
        os.environ.get("CLUD_TEST_BINARY"),
        _REPO_ROOT / "target" / "debug" / f"clud{exe}",
        Path("/build/target/debug") / f"clud{exe}",
    ]
    hook = os.environ.get("CLUD_TEST_BLOCK_BAD_CMD_BINARY")
    if hook:
        candidates.append(Path(hook).with_name(f"clud{exe}"))
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            try:
                multicall_aliases.materialize(Path(candidate))
            except OSError:
                # A read-only install dir: tests that need the alias say so.
                pass


_materialize_multicall_aliases()


@pytest.fixture(scope="session", autouse=True)
def checkout_tracked_files_unchanged():
    """Fail the run if any test wrote a tracked file in the checkout (#1426).

    Compares before vs after, so a checkout that was already dirty passes.
    Skipped when the tests do not run from a git checkout.
    """
    before = tracked_files.snapshot(_REPO_ROOT)
    yield
    if before is None:
        return
    after = tracked_files.snapshot(_REPO_ROOT)
    if after is None:
        return
    changed = tracked_files.changed_paths(before, after)
    if changed:
        pytest.fail(tracked_files.describe(changed, during="the pytest run"), pytrace=False)


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Skip integration tests unless CLUD_INTEGRATION_TESTS=1."""
    if os.environ.get("CLUD_INTEGRATION_TESTS", "").lower() in {"1", "true", "yes"}:
        return
    skip_integration = pytest.mark.skip(reason="set CLUD_INTEGRATION_TESTS=1 to run")
    for item in items:
        if "integration" in item.keywords:
            item.add_marker(skip_integration)


@pytest.fixture
def bridge():
    """Load `src/clud/mcp_server.py` as an importable module.

    Shared here (not in the test module) so both the original bridge tests and
    the session-surface tests drive the same live module.
    """
    name = "clud_test_mcp_server"
    script = Path(__file__).resolve().parents[1] / "src" / "clud" / "mcp_server.py"
    spec = importlib.util.spec_from_file_location(name, script)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


@pytest.fixture(autouse=True)
def session_rm_path(request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch):
    """Hook process tests run in the same trusted-rm PATH contract as sessions.

    Identity-negative tests supply their own explicit PATH. Keep this scoped to
    hook consumers so unrelated subprocess tests retain their original env.
    """
    modules = {
        "test_hook_stdin.py",
        "test_block_cd_hook.py",
        "test_cwd_changed_hook.py",
        "test_extern_hook_trust.py",
        "test_uv_run_hook_guard.py",
        "test_clud_hooks_dispatch.py",
        "test_gated_session.py",
    }
    if request.path.name not in modules:
        return
    import shutil

    tmp_path = request.getfixturevalue("tmp_path")
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    hook = os.environ.get("CLUD_TEST_BLOCK_BAD_CMD_BINARY")
    sibling = (
        Path(clud or hook).with_name("clud-shim" + suffix)
        if clud or hook
        else Path(__file__).resolve().parents[1] / "target/debug" / ("clud-shim" + suffix)
    )
    assert sibling.is_file(), f"build packaged shim for hook tests: {sibling}"
    directory = tmp_path / "session-rm-shim"
    directory.mkdir()
    shutil.copy2(sibling, directory / ("rm" + suffix))
    tap = sibling.with_name("tap" + suffix)
    if tap.is_file():
        shutil.copyfile(tap, directory / ("tap" + suffix))
        (directory / ("tap" + suffix)).chmod(0o755)
    monkeypatch.setenv("PATH", str(directory) + os.pathsep + os.environ.get("PATH", ""))

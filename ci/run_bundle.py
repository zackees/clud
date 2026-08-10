"""Execute a prebuilt CI bundle on a native runner. Compiles nothing.

This is the exec-side counterpart to `ci.bundle`. It replaces the cargo-driven
orchestration in `ci/test.py` for CI only -- `bash test` still goes through
`ci/test.py` locally, where a toolchain and a `target/` tree exist.

The whole job of this module is to reconstruct the environment the existing
tests already look for, so that no test has to learn a new convention and no
test falls back to building from source:

  CLUD_TEST_BINARY / _BLOCK_BAD_CMD_BINARY / _MOCK_AGENT_BINARY
      checked first by every Python consumer (tests/test_hello.py:58-60,
      tests/integration/conftest.py:181-200, tests/test_hook_stdin.py:36-48)
  CARGO_TARGET_DIR
      read at *runtime* by crates/clud-bin/tests/common/mod.rs:33, so the
      Rust harnesses in pty_pump.rs / pty_behavior.rs / orphan_reap.rs resolve
      mock-agent without a source change
  CLUD_TEST_BIN_DIR
      runtime override for the CARGO_BIN_EXE_* paths that symbols.rs:35,
      telemetry_endpoint.rs:33 and the ctrlc probes bake in at compile time

Design: docs/architecture/ci.md
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
from pathlib import Path

from ci import process

ROOT = Path(__file__).resolve().parent.parent

_PYTEST_NO_TESTS_COLLECTED = 5


def _pytest_ok(returncode: int) -> bool:
    return returncode in (0, _PYTEST_NO_TESTS_COLLECTED)


def _bin_dir(bundle: Path, manifest: dict) -> Path:
    return bundle / "target" / manifest["profile_dir"]


def _exe(name: str, manifest: dict) -> str:
    return f"{name}.exe" if "windows" in manifest["target"] else name


def bundle_env(bundle: Path, manifest: dict) -> dict[str, str]:
    bin_dir = _bin_dir(bundle, manifest)
    env = os.environ.copy()
    env.pop("VIRTUAL_ENV", None)
    env.setdefault("PYTHONUTF8", "1")
    env["RUST_BACKTRACE"] = "1"
    env["CARGO_TARGET_DIR"] = str((bundle / "target").resolve())
    env["CLUD_TEST_BIN_DIR"] = str(bin_dir.resolve())
    env["CLUD_TEST_BINARY"] = str((bin_dir / _exe("clud", manifest)).resolve())
    env["CLUD_TEST_BLOCK_BAD_CMD_BINARY"] = str(
        (bin_dir / _exe("clud-block-bad-cmd", manifest)).resolve()
    )
    env["CLUD_TEST_MOCK_AGENT_BINARY"] = str((bin_dir / _exe("mock-agent", manifest)).resolve())
    return env


def stage_wheel(bundle: Path) -> None:
    """Put the bundled wheel where `ci.build_wheel.latest_wheel()` looks.

    tests/integration/test_trampoline.py:27-44 hard-`fail`s (not skips) when
    dist/ has no wheel, so this is required coverage, not a nicety.
    """
    src = bundle / "dist"
    if not src.is_dir():
        return
    dest = ROOT / "dist"
    dest.mkdir(parents=True, exist_ok=True)
    for wheel in src.glob("*.whl"):
        shutil.copy2(wheel, dest / wheel.name)


def install_wheel(bundle: Path, env: dict[str, str]) -> int:
    """Install the bundled wheel and smoke-test its console scripts.

    The old `_integration-test.yml` got this for free because it ran
    `ci.build_wheel --dev`, which installs and then calls
    `verify_installed_scripts` (ci/build_wheel.py:115-174). The exec runner does
    not build, so the install + smoke test is done explicitly here to keep that
    coverage.
    """
    from ci.build_wheel import install_wheel as do_install
    from ci.build_wheel import latest_wheel

    wheel = latest_wheel()
    print(f"installing bundled wheel: {wheel.name}", flush=True)
    return do_install(wheel, env=env)


def run_harnesses(bundle: Path, manifest: dict, env: dict[str, str]) -> int:
    """Run every `cargo test --no-run` harness binary shipped in the bundle."""
    tests_dir = bundle / "tests"
    harnesses = sorted(tests_dir.glob("*"))
    if not harnesses:
        print("bundle contains no test harnesses", file=sys.stderr)
        return 1

    failures: list[str] = []
    for harness in harnesses:
        if not harness.is_file():
            continue
        argv = [str(harness)]
        # Mirrors ci/test.py:138-139 -- the Rust suite is not parallel-safe on
        # Windows (shared console/PTY state).
        if sys.platform == "win32":
            argv += ["--test-threads=1"]
        print(f"::group::{harness.name}", flush=True)
        rc = process.run(argv, cwd=ROOT, env=env).returncode
        print("::endgroup::", flush=True)
        if rc != 0:
            failures.append(f"{harness.name} (rc={rc})")

    if failures:
        print(f"::error::failing Rust harnesses: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


def run_pytest(marker: str, env: dict[str, str], extra: list[str]) -> int:
    argv = [sys.executable, "-m", "pytest", "-m", marker, *extra]
    return process.run(argv, cwd=ROOT, env=env).returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run a prebuilt CI test bundle")
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--suite", choices=("unit", "integration"), required=True)
    parser.add_argument("pytest_args", nargs="*")
    args = parser.parse_args(argv)

    bundle = args.bundle.resolve()
    manifest = json.loads((bundle / "manifest.json").read_text(encoding="utf-8"))
    env = bundle_env(bundle, manifest)
    stage_wheel(bundle)

    if args.suite == "unit":
        if run_harnesses(bundle, manifest, env) != 0:
            return 1
        return 0 if _pytest_ok(run_pytest("not integration", env, args.pytest_args)) else 1

    if install_wheel(bundle, env) != 0:
        return 1
    env = env.copy()
    # Point the integration suite at the *installed console scripts*, not the
    # raw bundle binaries. ci/test.py:74-87,130-131 did this deliberately
    # (`prefer_installed_clud`) so integration exercises the packaged
    # trampoline path rather than a bare target/ binary. mock-agent is not
    # part of the wheel, so it keeps pointing into the bundle.
    for name, var in (
        ("clud", "CLUD_TEST_BINARY"),
        ("clud-block-bad-cmd", "CLUD_TEST_BLOCK_BAD_CMD_BINARY"),
    ):
        installed = Path(sys.executable).parent / _exe(name, manifest)
        if installed.is_file():
            env[var] = str(installed)
    env["CLUD_INTEGRATION_TESTS"] = "1"
    # See ci/test.py:154-159 (#37): the Windows exe-unlock rename+copy+GC dance
    # keeps stdout/stderr pipe handles alive on Windows CI and wedges
    # process.run in a pipe-EOF wait. Tests do not need hot-reload
    # protection.
    env["CLUD_NO_UNLOCK"] = "1"
    # `-v` prints each test name before it runs, so a hang is pinned to an exact
    # test rather than showing up as silent dead air before the job timeout.
    return 0 if _pytest_ok(run_pytest("integration", env, ["-v", *args.pytest_args])) else 1


if __name__ == "__main__":
    sys.exit(main())

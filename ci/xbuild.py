"""Cross-compile driver for CI builds.

Every cargo/maturin invocation in `.github/workflows/_build-target.yml` goes
through here, so the per-strategy environment lives in one testable place
instead of being smeared across YAML `if:` conditions.

Three strategies:

    native    the build host's own triple (x86_64-unknown-linux-gnu)
    zigbuild  cargo-zigbuild -- Linux -> aarch64-linux and Linux -> *-apple-darwin
    xwin      cargo-xwin     -- Linux -> *-pc-windows-msvc

Working around `vendor/whisper-rs-sys/build.rs`
----------------------------------------------
That build script uses `cfg!(target_os = ...)`, which evaluates against the
**host**, not the target. Two of those checks break cross-compiles, and both are
fixed from the environment rather than by patching vendored source:

  build.rs:212-215  `cfg!(target_os = "windows")` gates `/utf-8` and
                    `cargo:rustc-link-lib=advapi32`. Cross to windows-msvc
                    silently drops advapi32 -> undefined symbols at link.
                    Fix: inject the link flag via RUSTFLAGS.
  build.rs:342      `cfg!(target_os = "macos")` gates linking `ggml-blas`.
                    CMake still *builds* it (Apple BLAS defaults ON,
                    ggml/CMakeLists.txt:92-95) but the flag is never emitted.
                    Fix: GGML_BLAS=OFF, which also drops the Accelerate
                    dependency we cannot satisfy without a full SDK.

`build.rs:298-306` forwards any `WHISPER_*` / `GGML_*` / `CMAKE_*` env var
straight into `cmake -D`, which is what makes both fixes possible from CI.

Design: docs/architecture/ci.md
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: Rust triple -> zig target triple. Zig pins a glibc version explicitly so the
#: produced binary does not accidentally require the builder's newer glibc.
ZIG_TARGETS = {
    "aarch64-unknown-linux-gnu": "aarch64-linux-gnu.2.28",
    "x86_64-unknown-linux-gnu": "x86_64-linux-gnu.2.28",
    "aarch64-apple-darwin": "aarch64-macos",
    "x86_64-apple-darwin": "x86_64-macos",
}

CMAKE_PROCESSOR = {"aarch64": "aarch64", "x86_64": "x86_64"}

SDK_ROOT = ROOT / "build" / "macos-sdk"


def _arch(target: str) -> str:
    return target.split("-", 1)[0]


def _is_windows(target: str) -> bool:
    return "windows" in target


def _is_darwin(target: str) -> bool:
    return "apple" in target


def _limit_address_space() -> None:
    """Cap RLIMIT_AS at ~13 GB of the runner's 14-16 GB.

    Defensive: when the build genuinely runs out of memory, exceeding this kills
    the offending process with a clean mmap/alloc failure naming the crate,
    instead of an unexplained `137` from the kernel OOM killer. This replaces
    the `ulimit -v 13000000` that the old workflow YAML applied per step.
    """
    if platform.system() != "Linux":
        return
    import resource

    limit = 13_000_000 * 1024
    _soft, hard = resource.getrlimit(resource.RLIMIT_AS)
    if hard != resource.RLIM_INFINITY and hard < limit:
        return
    resource.setrlimit(resource.RLIMIT_AS, (limit, hard))


def whisper_env(target: str, strategy: str, env: dict[str, str]) -> dict[str, str]:
    env = env.copy()
    # build.rs:127-129 -- use the checked-in src/bindings.rs instead of running
    # bindgen against target headers we do not have. build.rs:169-182 already
    # falls back to these on failure; this makes it deterministic.
    if strategy != "native":
        env["WHISPER_DONT_GENERATE_BINDINGS"] = "1"
        env["GGML_NATIVE"] = "OFF"  # no -march=native for a foreign CPU
        env["CMAKE_SYSTEM_PROCESSOR"] = CMAKE_PROCESSOR.get(_arch(target), _arch(target))

    if _is_darwin(target):
        # See module docstring, build.rs:342.
        env["GGML_BLAS"] = "OFF"
        env["GGML_METAL"] = "OFF"
        sdk = env.get("SDKROOT") or (str(SDK_ROOT) if SDK_ROOT.is_dir() else "")
        if sdk:
            env["SDKROOT"] = sdk
            env["CMAKE_OSX_SYSROOT"] = sdk
        env["CMAKE_SYSTEM_NAME"] = "Darwin"
    elif _is_windows(target) and strategy == "xwin":
        env["CMAKE_SYSTEM_NAME"] = "Windows"
        env["CC"] = "clang-cl"
        env["CXX"] = "clang-cl"
    elif strategy == "zigbuild":
        env["CMAKE_SYSTEM_NAME"] = "Linux"

    if _is_windows(target):
        # See module docstring, build.rs:212-215.
        rustflags = env.get("RUSTFLAGS", "")
        env["RUSTFLAGS"] = f"{rustflags} -C link-arg=advapi32.lib".strip()
    return env


def build_env(target: str, strategy: str) -> dict[str, str]:
    from ci.env import build_env as base_env

    env = whisper_env(target, strategy, base_env())
    env["CARGO_BUILD_TARGET"] = target
    # Fresh-checkout CI has no prior incremental state to reuse; incremental
    # only costs compile time and target/ size here.
    env.setdefault("CARGO_INCREMENTAL", "0")
    if strategy == "zigbuild":
        # cargo-zigbuild delegates linking to zig; soldr's fast-linker shim
        # would otherwise force host clang/mold, which cannot see zig's
        # cross sysroot. Same reasoning as ci/build_wheel.py:24-31.
        env["SOLDR_LINKER"] = "default"
    return env


def cargo_argv(subcommand: list[str], target: str, strategy: str) -> list[str]:
    """Return the cargo argv for this strategy.

    clippy is deliberately NOT routed through cargo-zigbuild: it does not link,
    so it needs the target sysroot only for `cfg` resolution, which plain
    `cargo clippy --target` already provides.
    """
    verb = subcommand[0]
    if strategy == "xwin":
        return ["cargo", "xwin", *subcommand, "--target", target]
    if strategy == "zigbuild" and verb != "clippy":
        return ["cargo", "zigbuild", *subcommand, "--target", target]
    return ["cargo", *subcommand, "--target", target]


def run(argv: list[str], env: dict[str, str]) -> int:
    print(f"+ {' '.join(argv)}", flush=True)
    return subprocess.run(argv, cwd=ROOT, env=env, preexec_fn=_preexec()).returncode


def _preexec():
    if platform.system() != "Linux":
        return None
    return _limit_address_space


def cmd_clippy(args: argparse.Namespace) -> int:
    env = build_env(args.target, args.strategy)
    base = cargo_argv(["clippy", "--workspace", "--all-targets"], args.target, args.strategy)
    return run([*base, "--", "-D", "warnings"], env)


def cmd_compile(args: argparse.Namespace) -> int:
    """Build the workspace binaries and, optionally, the test harnesses.

    Both happen in one job against one `target/` directory, so the dependency
    graph -- including the whisper.cpp static libs, which dominate the build -- is
    compiled exactly once. The old layout spread these across three machines.
    """
    env = build_env(args.target, args.strategy)
    profile_args = ["--release"] if args.profile == "release" else []

    build = cargo_argv(
        ["build", "--workspace", "--bins", *profile_args], args.target, args.strategy
    )
    if run(build, env) != 0:
        return 1

    if not args.with_tests:
        return 0

    # The zombie-scan fixture (tests/integration/conftest.py:326-355) silently
    # no-ops without this example binary, so build it explicitly rather than
    # letting that coverage vanish.
    examples = cargo_argv(
        ["build", "--workspace", "--examples", *profile_args], args.target, args.strategy
    )
    run(examples, env)  # best-effort: not every workspace member has examples

    # `--message-format=json` is how we learn where cargo put each harness
    # binary; there is no stable path convention (they carry a hash suffix).
    harness = cargo_argv(
        ["test", "--workspace", "--no-run", "--message-format=json", *profile_args],
        args.target,
        args.strategy,
    )
    print(f"+ {' '.join(harness)}", flush=True)
    proc = subprocess.run(harness, cwd=ROOT, env=env, capture_output=True, text=True)
    sys.stderr.write(proc.stderr)
    if proc.returncode != 0:
        return 1

    executables: list[str] = []
    for line in proc.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") == "compiler-artifact" and record.get("executable"):
            if record.get("profile", {}).get("test"):
                executables.append(record["executable"])

    out = ROOT / "target" / args.target / "test-harnesses.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(sorted(set(executables)), indent=2), encoding="utf-8")
    print(f"recorded {len(set(executables))} test harnesses -> {out}")
    return 0


def cmd_doctest(args: argparse.Namespace) -> int:
    """Run doc-tests.

    The old `cargo test --workspace` (ci/test.py:137) included these, but
    doc-tests produce no harness binary, so they cannot ride along in the
    bundle and be executed on an exec runner. They are architecture- and
    OS-independent, so running them once on the native host triple is full
    coverage rather than a reduction.
    """
    env = build_env(args.target, args.strategy)
    return run(cargo_argv(["test", "--workspace", "--doc"], args.target, args.strategy), env)


def cmd_wheel(args: argparse.Namespace) -> int:
    """Build the wheel into dist/ for this triple.

    maturin is invoked with `--target` so it reuses the same `target/<triple>/`
    directory the compile step already populated, rather than relinking from a
    cold graph.
    """
    from ci.env import maturin_argv

    env = build_env(args.target, args.strategy)
    if args.profile == "release" and args.target.endswith("-unknown-linux-gnu"):
        # `maturin --zig` delegates the final link to cargo-zigbuild's target
        # linker; soldr's fast-linker shim would force host clang/mold, which
        # cannot see zig's Linux C++ runtime during a manylinux build. Mirrors
        # ci/build_wheel.py:24-31, which this path replaces.
        env["SOLDR_LINKER"] = "default"
    subcommand = [
        "build",
        "--target",
        args.target,
        "--interpreter",
        sys.executable,
        "--out",
        str(ROOT / "dist"),
    ]
    if args.profile == "dev":
        subcommand += ["--profile", "dev"]
    else:
        subcommand.append("--release")
        if args.target.endswith("-unknown-linux-gnu"):
            subcommand += ["--zig", "--compatibility", "manylinux2014"]
        else:
            subcommand += ["--compatibility", "pypi"]
    if args.strategy == "zigbuild":
        subcommand.append("--zig")

    if run(maturin_argv(subcommand, env=env), env) != 0:
        return 1

    from ci.build_wheel import built_wheels, verify_wheel_scripts
    from ci.wheel_repair import repair_windows_gnu_wheel

    wheels = built_wheels()
    if not wheels:
        print("build completed but produced no wheel", file=sys.stderr)
        return 1
    for wheel in wheels:
        repair_windows_gnu_wheel(wheel)
        if verify_wheel_scripts(wheel) != 0:
            return 1
    return 0


def cmd_provision_macos_sdk(_: argparse.Namespace) -> int:
    """Fetch the macOS SDK used for Linux -> darwin cross-compilation.

    `vendor/whisper-rs-sys/build.rs:27-28` emits `-framework Accelerate` for any
    apple target with no feature to disable it, and cpal/rodio/arboard pull in
    further system frameworks, so an SDK is unavoidable for a darwin cross.
    Sourcing it is an Apple-licensing decision, which is why the URL is a repo
    variable rather than something this script hardcodes -- when it is unset,
    ci/ci_matrix.py falls back to native macOS builders instead.
    """
    url = os.environ.get("MACOS_SDK_URL", "").strip()
    if not url:
        print("MACOS_SDK_URL is unset; expected a native macOS builder", file=sys.stderr)
        return 1
    SDK_ROOT.parent.mkdir(parents=True, exist_ok=True)
    archive = SDK_ROOT.parent / "macos-sdk.tar.xz"
    print(f"fetching macOS SDK from {url}", flush=True)
    # URL comes from the repo-owned `MACOS_SDK_URL` variable, not user input.
    urllib.request.urlretrieve(url, archive)
    if SDK_ROOT.exists():
        shutil.rmtree(SDK_ROOT)
    SDK_ROOT.mkdir(parents=True)
    with tarfile.open(archive) as tar:
        tar.extractall(SDK_ROOT, filter="data")
    # Collapse a single top-level MacOSX*.sdk directory so SDKROOT is stable.
    entries = list(SDK_ROOT.iterdir())
    if len(entries) == 1 and entries[0].is_dir():
        for child in entries[0].iterdir():
            shutil.move(str(child), SDK_ROOT / child.name)
        entries[0].rmdir()
    print(f"SDKROOT={SDK_ROOT}")
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as handle:
        handle.write(f"SDKROOT={SDK_ROOT}\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Cross-compile driver for clud CI")
    sub = parser.add_subparsers(dest="command", required=True)

    def add_common(p: argparse.ArgumentParser) -> None:
        p.add_argument("--target", required=True)
        p.add_argument("--strategy", default="native", choices=("native", "zigbuild", "xwin"))
        p.add_argument("--profile", default="dev", choices=("dev", "release"))

    clippy = sub.add_parser("clippy")
    add_common(clippy)
    clippy.set_defaults(func=cmd_clippy)

    compile_parser = sub.add_parser("compile")
    add_common(compile_parser)
    compile_parser.add_argument("--with-tests", action="store_true")
    compile_parser.set_defaults(func=cmd_compile)

    doctest = sub.add_parser("doctest")
    add_common(doctest)
    doctest.set_defaults(func=cmd_doctest)

    wheel = sub.add_parser("wheel")
    add_common(wheel)
    wheel.set_defaults(func=cmd_wheel)

    sdk = sub.add_parser("provision-macos-sdk")
    sdk.set_defaults(func=cmd_provision_macos_sdk)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())

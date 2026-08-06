"""Cross-compile driver for CI builds.

Every cargo/maturin invocation in `.github/workflows/_build-target.yml` goes
through here, so the per-strategy environment lives in one testable place
instead of being smeared across YAML `if:` conditions.

Three strategies:

    native    the build host's own triple (x86_64-unknown-linux-gnu)
    zigbuild  cargo-zigbuild -- Linux -> *-unknown-linux-gnu
    soldr     `soldr build`  -- Linux -> *-pc-windows-msvc and -> *-apple-darwin

`soldr` is the blessed surface (soldr docs/CROSS_COMPILE.md): `soldr prepare`
provisions the sysroot in .github/actions/setup-build and exports the
target-scoped Cargo/cc-rs/linker env, then `soldr build --target <triple>`
links against it. The legacy xwin / zigbuild-at-Apple passthroughs documented
there are deliberately unused, and `ci/banned_cross_tools.py` enforces that.

Only the link-producing `build` verb goes through `soldr build`. clippy, `cargo
test --no-run` and maturin stay on the plain cargo front door: `soldr prepare`
already exported the env they need, and `soldr <verb>` for an unknown verb falls
into soldr's tool-fetch mode (it would try to resolve "test" on crates.io).

Working around `vendor/whisper-rs-sys/build.rs`
----------------------------------------------
That build script uses `cfg!(target_os = ...)`, which evaluates against the
**host**, not the target. Two of those checks break cross-compiles, and both are
fixed from the environment rather than by patching vendored source:

  build.rs:212-215  `cfg!(target_os = "windows")` gates `/utf-8` and
                    `cargo:rustc-link-lib=advapi32`. Cross to windows-msvc
                    silently drops advapi32 -> undefined symbols at link.
                    Fix: inject the link flag via target-scoped RUSTFLAGS,
                    preserving soldr's Xwin SDK /LIBPATH flags.
  build.rs:342      `cfg!(target_os = "macos")` gates linking `ggml-blas`.
                    CMake still *builds* it (Apple BLAS defaults ON,
                    ggml/CMakeLists.txt:92-95) but the flag is never emitted.
                    Fix: GGML_BLAS=OFF. The hardcoded Accelerate link directive
                    still requires the SDK that `soldr prepare` provisions.

`build.rs:298-306` forwards any `WHISPER_*` / `GGML_*` / `CMAKE_*` env var
straight into `cmake -D`, which is what makes both fixes possible from CI.

Design: docs/architecture/ci.md
"""

from __future__ import annotations

import argparse
import json
import platform
import shlex
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

CMAKE_PROCESSOR = {"aarch64": "aarch64", "x86_64": "x86_64"}
PACKAGE_MANAGER = "cargo"


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
    # Cross targets default to the checked-in bindings when their SDK headers
    # are unavailable. Windows soldr overrides this below once it finds the
    # target-scoped Xwin header flags exported by `soldr prepare`.
    if strategy != "native":
        env["WHISPER_DONT_GENERATE_BINDINGS"] = "1"
        env["GGML_NATIVE"] = "OFF"  # no -march=native for a foreign CPU
        env["CMAKE_SYSTEM_PROCESSOR"] = CMAKE_PROCESSOR.get(_arch(target), _arch(target))

    if _is_darwin(target):
        # See module docstring, build.rs:342.
        env["GGML_BLAS"] = "OFF"
        env["GGML_METAL"] = "OFF"
        # SDKROOT is exported by `soldr prepare --target <apple-triple>
        # --github-env "$GITHUB_ENV"` in .github/actions/setup-build; CMake
        # needs the same path under its own variable name.
        sdk = env.get("SDKROOT", "")
        if sdk:
            env["CMAKE_OSX_SYSROOT"] = sdk
        env["CMAKE_SYSTEM_NAME"] = "Darwin"
        env["CMAKE_INSTALL_NAME_TOOL"] = "llvm-install-name-tool"
    elif _is_windows(target) and strategy == "soldr":
        env["CMAKE_SYSTEM_NAME"] = "Windows"
        env["CC"] = "clang-cl"
        env["CXX"] = "clang-cl"
        target_suffix = target.replace("-", "_")
        bindgen_key = f"BINDGEN_EXTRA_CLANG_ARGS_{target_suffix}"
        bindgen_args = env.get(f"BINDGEN_EXTRA_CLANG_ARGS_{target}") or env.get(bindgen_key)
        if not bindgen_args:
            target_cflags = env.get(f"CFLAGS_{target_suffix}", "")
            include_args = [
                f"-I{arg.removeprefix('/imsvc')}"
                for arg in shlex.split(target_cflags)
                if arg.startswith("/imsvc") and arg != "/imsvc"
            ]
            if include_args:
                bindgen_args = shlex.join(include_args)
                env[bindgen_key] = bindgen_args
        if bindgen_args:
            env.pop("WHISPER_DONT_GENERATE_BINDINGS", None)
        # clang-cl disables C++ exceptions unless an /EH mode is selected,
        # while ggml's gguf.cpp uses try/catch. Keep this in CXXFLAGS so the
        # cc/cmake crates merge it with soldr's target-specific SDK includes.
        cxx_flags = env.get("CXXFLAGS", "")
        env["CXXFLAGS"] = f"{cxx_flags} /EHsc".strip()
        # CMake's executable compiler probe needs Windows rc/mt tools that are
        # not part of this Linux-hosted toolchain. Whisper builds static libs,
        # so a static-library probe validates the compiler without those tools.
        env["CMAKE_TRY_COMPILE_TARGET_TYPE"] = "STATIC_LIBRARY"
    elif strategy == "zigbuild":
        env["CMAKE_SYSTEM_NAME"] = "Linux"

    if _is_windows(target):
        # See module docstring, build.rs:212-215.
        # Setting generic RUSTFLAGS here would override the target-scoped
        # /LIBPATH arguments exported by `soldr prepare`, leaving lld-link
        # unable to find even kernel32.lib. Append our missing library to the
        # existing target-specific value instead. Migrate any pre-existing
        # generic flags too: leaving RUSTFLAGS set would make Cargo ignore the
        # target-specific value we need to preserve.
        rustflags_key = f"CARGO_TARGET_{target.upper().replace('-', '_')}_RUSTFLAGS"
        generic_rustflags = env.pop("RUSTFLAGS", "")
        target_rustflags = env.get(rustflags_key, "")
        if "CARGO_ENCODED_RUSTFLAGS" in env:
            # soldr >= 0.8.30 exports the prepared MSVC link configuration --
            # `-Clinker-flavor=lld-link` plus the SDK /LIBPATH arguments --
            # through CARGO_ENCODED_RUSTFLAGS rather than the target-scoped
            # variable. 0.8.28 used the target-scoped one.
            #
            # This used to raise. That was right when the SDK paths lived
            # *only* in the target-scoped variable: CARGO_ENCODED_RUSTFLAGS
            # outranks both RUSTFLAGS and target.<triple>.rustflags, so its
            # mere presence meant those paths were about to be dropped and
            # lld-link would fail to find even kernel32.lib. Now that soldr
            # puts them in the winning variable, refusing to proceed rejects
            # the correct configuration.
            #
            # Merging instead of failing is also strictly safer than either
            # branch alone: if a future toolchain sets the encoded variable
            # *without* the SDK paths, folding the target-scoped value in
            # preserves them rather than erroring out.
            encoded = [part for part in env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if part]
            encoded += shlex.split(target_rustflags)
            encoded += shlex.split(generic_rustflags)
            encoded.append("-Clink-arg=advapi32.lib")
            env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(encoded)
            # Drop the now-shadowed variable so nothing reads a stale value.
            env.pop(rustflags_key, None)
        else:
            env[rustflags_key] = " ".join(
                filter(
                    None,
                    (target_rustflags, generic_rustflags, "-C link-arg=advapi32.lib"),
                )
            )
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


def is_soldr_owned(target: str) -> bool:
    """Does soldr own this triple's cross toolchain end to end? (#637)

    Apple and Windows-MSVC targets do. Linux does not — Zig is the supported
    cross there, and for the manylinux wheel.
    """
    return target.endswith("-apple-darwin") or target.endswith("-pc-windows-msvc")


def cargo_argv(subcommand: list[str], target: str, strategy: str) -> list[str]:
    """Return the cargo argv for this strategy.

    clippy is deliberately NOT routed through cargo-zigbuild: it does not link,
    so it needs the target sysroot only for `cfg` resolution, which plain
    `cargo clippy --target` already provides.
    """
    verb = subcommand[0]
    if strategy == "zigbuild" and is_soldr_owned(target):
        # #637: the matrix assigns `soldr` to every Apple/MSVC triple, and
        # `tests/test_ci_matrix.py` pins that. But the strategy is not what
        # runs the compiler -- this function is -- so pinning the matrix value
        # alone left the alternate command path reachable by a one-word edit.
        # Refuse structurally instead, so the invariant does not depend on
        # anyone remembering it.
        raise ValueError(
            f"{target} must cross through soldr's blessed surface, not "
            "cargo-zigbuild. soldr owns the Apple/MSVC toolchain "
            "(`soldr prepare` / `soldr build`); Zig is correct for "
            "*-unknown-linux-* only. See docs/architecture/ci.md."
        )
    if strategy == "soldr" and verb == "build":
        # The blessed cross surface. Everything else under this strategy rides
        # on the env `soldr prepare` exported -- see the module docstring.
        return ["soldr", "build", *subcommand[1:], "--target", target]
    if strategy == "zigbuild" and verb == "build":
        return [PACKAGE_MANAGER, "zigbuild", *subcommand[1:], "--target", target]
    if strategy == "zigbuild" and verb == "test":
        # This wrapper is a build subcommand, not a transparent replacement
        # for the test subcommand. Building `--tests` produces the same harness
        # executables without trying to run foreign-architecture binaries.
        options = [arg for arg in subcommand[1:] if arg != "--no-run"]
        return [PACKAGE_MANAGER, "zigbuild", "--tests", *options, "--target", target]
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


def _project_version() -> str:
    import tomllib

    with (ROOT / "pyproject.toml").open("rb") as project:
        return tomllib.load(project)["project"]["version"]


def cmd_wheel(args: argparse.Namespace) -> int:
    """Build the wheel into dist/ for this triple.

    Maturin normally reuses the same `target/<triple>/` directory the compile
    step populated. Release GNU Linux wheels are the exception: their
    manylinux link must not reuse a host-default glibc artifact.
    """
    from ci.env import maturin_argv

    env = build_env(args.target, args.strategy)
    if _is_windows(args.target) and args.strategy == "soldr":
        target_dir = ROOT / "target"
        profile = "release" if args.profile == "release" else "debug"
        from ci.build_wheel import build_windows_wheel_from_binaries

        try:
            wheel = build_windows_wheel_from_binaries(
                target=args.target,
                profile=profile,
                target_dir=target_dir,
                dist_dir=ROOT / "dist",
                version=_project_version(),
            )
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return 1
        print(f"packaged soldr-built Windows wheel: {wheel}")
        return 0
    if args.profile == "release" and args.target.endswith("-unknown-linux-gnu"):
        # `maturin --zig` delegates the final link to cargo-zigbuild's target
        # linker; soldr's fast-linker shim would force host clang/mold, which
        # cannot see zig's Linux C++ runtime during a manylinux build. Mirrors
        # ci/build_wheel.py:24-31, which this path replaces.
        env["SOLDR_LINKER"] = "default"
        # The link cache key does not include the requested glibc floor. The
        # preceding compile uses the toolchain default, while the manylinux
        # wheel needs a fresh 2.17 link. Isolate this final wheel build.
        env["CARGO_TARGET_DIR"] = str(ROOT / "target" / "release-wheel" / args.target)
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
        if args.target.endswith("-unknown-linux-gnu"):
            # Dev wheels are CI artifacts, not distributables, and must not be
            # audited for manylinux compliance.
            #
            # maturin audits by default on Linux even with no --compatibility
            # flag. The release path opts into manylinux2014 explicitly and
            # pairs it with --zig, which is what actually supplies the 2.17
            # floor -- maturin hands the glibc version down to zigbuild. Dev
            # passes neither, so it inherited the audit without the mechanism
            # that satisfies it, and once the blessed Linux prep started
            # linking at zig's default floor the wheel step died with:
            #
            #   Error ensuring manylinux_2_17 compliance ... too-recent
            #   versioned symbols: ["libm.so.6 offending versions: GLIBC_2.27"]
            #
            # This wheel is only ever installed on the exec runner for the same
            # triple, whose glibc is far newer, so the property being asserted
            # is one nothing downstream consumes. Release keeps its audit.
            subcommand += ["--compatibility", "linux"]
    else:
        subcommand.append("--release")
        if args.target.endswith("-unknown-linux-gnu"):
            subcommand += ["--zig", "--compatibility", "manylinux2014"]
        else:
            subcommand += ["--compatibility", "pypi"]
    if args.strategy == "zigbuild" and "--zig" not in subcommand:
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Cross-compile driver for clud CI")
    sub = parser.add_subparsers(dest="command", required=True)

    def add_common(p: argparse.ArgumentParser) -> None:
        p.add_argument("--target", required=True)
        p.add_argument("--strategy", default="native", choices=("native", "zigbuild", "soldr"))
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

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())

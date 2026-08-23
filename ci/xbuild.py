"""Cross-compile driver for CI builds.

Every cargo/maturin invocation in `.github/workflows/_build-target.yml` goes
through here, so the per-strategy environment lives in one testable place
instead of being smeared across YAML `if:` conditions.

One strategy now: `soldr`. Since soldr#2299 every target -- Apple, Windows-MSVC,
and Linux (via the catalogue GNU toolchain gcc-13.3.0-glibc-2.17-1, soldr#2238)
-- crosses through soldr's blessed surface. The `native`/`zigbuild` enum values
remain inert pending removal; zig is banned for every target by
`ci/banned_cross_tools.py`.

`soldr` is the blessed surface (soldr docs/CROSS_COMPILE.md): `soldr prepare`
provisions the sysroot in .github/actions/setup-build and exports the
target-scoped Cargo/cc-rs/linker env, then `soldr build --target <triple>`
links against it.

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
import sys
from pathlib import Path

from ci import process

ROOT = Path(__file__).resolve().parent.parent

CMAKE_PROCESSOR = {"aarch64": "aarch64", "x86_64": "x86_64"}
PACKAGE_MANAGER = "cargo"


def _arch(target: str) -> str:
    return target.split("-", 1)[0]


def _is_windows(target: str) -> bool:
    return "windows" in target


def _is_darwin(target: str) -> bool:
    return "apple" in target


def _run_argv(argv: list[str]) -> list[str]:
    """Apply Linux's build memory limit without bypassing running-process.

    `running_process` intentionally rejects Python's `preexec_fn`. Use a small
    argv-only Bash wrapper on Linux instead: it applies the same 13 GB virtual
    memory cap the old pre-exec hook supplied, then replaces itself with cargo.
    """
    if platform.system() != "Linux":
        return argv
    return ["bash", "-c", "ulimit -v 13000000; exec \"$@\"", "clud-xbuild", *argv]


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
    elif "linux" in target:
        # Blessed soldr Linux prep (and the legacy zigbuild path) cross to a
        # Linux target from a Linux host; CMake still needs the target OS named.
        # soldr's catalogue toolchain supplies the pinned glibc-2.17 sysroot and
        # compiler, so no host headers/libraries leak into the artifact.
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
    """Does soldr own this triple's cross toolchain end to end? (#637, soldr#2299)

    Apple, Windows-MSVC, and now GNU/Linux all do. soldr 0.8.39's catalogue GNU
    toolchain (gcc-13.3.0-glibc-2.17-1, soldr#2238) replaced zig for
    *-unknown-linux-gnu preparation and the manylinux wheel, so zig is no longer
    the supported Linux cross -- routing a linux-gnu build through zigbuild is
    now refused in cargo_argv, exactly as for Apple/MSVC.
    """
    return (
        target.endswith("-apple-darwin")
        or target.endswith("-pc-windows-msvc")
        or target.endswith("-unknown-linux-gnu")
    )


def cargo_argv(subcommand: list[str], target: str, strategy: str) -> list[str]:
    """Return the cargo argv for this strategy.

    clippy is deliberately NOT routed through the blessed `soldr build` surface:
    it does not link, so it needs the target sysroot only for `cfg` resolution,
    which plain `cargo clippy --target` already provides on the env `soldr
    prepare` exported.
    """
    verb = subcommand[0]
    if strategy == "zigbuild" and is_soldr_owned(target):
        # Every clud triple is soldr-owned (Apple, MSVC, and Linux since
        # soldr#2299), so this covers every real target -- the strategy is not
        # what runs the compiler, this function is, so refuse structurally here
        # rather than relying on the matrix value alone. zig is not used for any
        # target; `ci/banned_cross_tools.py` bans it everywhere.
        raise ValueError(
            f"{target} must cross through soldr's blessed surface "
            "(`soldr prepare` / `soldr build`), not the zig cross wrapper, "
            "which is banned for every target. See docs/architecture/ci.md."
        )
    if strategy == "soldr" and verb == "build":
        # The blessed cross surface. Everything else under this strategy rides
        # on the env `soldr prepare` exported -- see the module docstring.
        return ["soldr", "build", *subcommand[1:], "--target", target]
    return ["cargo", *subcommand, "--target", target]


def run(argv: list[str], env: dict[str, str]) -> int:
    command = _run_argv(argv)
    print(f"+ {' '.join(command)}", flush=True)
    return process.run(command, cwd=ROOT, env=env).returncode


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
    proc = process.run(harness, cwd=ROOT, env=env, capture_output=True, text=True)
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


#: Rustc flags that link the C++ and unwind runtimes statically into clud's
#: binary. In CARGO_ENCODED_RUSTFLAGS each is one NUL/US-delimited token.
_STATIC_CXX_RUSTFLAGS = ("-Clink-arg=-static-libstdc++", "-Clink-arg=-static-libgcc")


def static_cxx_runtime_env(target: str, env: dict[str, str]) -> dict[str, str]:
    """Bundle the C++ runtime so a manylinux wheel carries no external libstdc++.

    soldr's blessed catalogue GNU toolchain (soldr#2238) pins the *glibc* floor
    at 2.17 via its sysroot, so libc/libm/libpthread symbols audit clean. It
    does not, however, pin the *C++* runtime: the catalogue compiler is
    gcc-13.3.0, whose libstdc++.so.6 exports GLIBCXX_3.4.20-3.4.29 and
    CXXABI_1.3.9 -- far newer than manylinux_2_17 permits. whisper.cpp's C++
    (which dominates this build) links against it, so the wheel failed the audit
    with, e.g.:

        not manylinux_2_17 ... too-recent versioned symbols:
        ["libstdc++.so.6 offending symbols: _ZSt28__throw_bad_array_new_lengthv
          @GLIBCXX_3.4.29, _ZdlPvm@CXXABI_1.3.9, ..."]

    `-static-libstdc++`/`-static-libgcc` link the C++ standard library and the
    GCC unwind runtime *into* clud's binary (maturin `bindings = "bin"`), so
    those versioned symbols become internal and only glibc -- pinned 2.17 --
    is imported dynamically. This is the blessed-path replacement for the old
    zig-based wheel build, which bundled an old C++ runtime the same way.

    soldr enforcing this floor itself is tracked upstream (see soldr#2299); the
    static-link decision is legitimately clud's since whisper.cpp is clud's
    dependency. Applied by appending to soldr's exported
    CARGO_ENCODED_RUSTFLAGS so the sysroot flags it set are preserved.

    WHISPER_LINK_CXX_STATIC is the load-bearing half: whisper-rs-sys otherwise
    emits `cargo:rustc-link-lib=dylib=stdc++`, an explicit dynamic link that
    `-static-libstdc++` (a driver flag for the *implicit* libstdc++) cannot
    override. The env var flips that directive to `static=stdc++`
    (vendor/whisper-rs-sys/build.rs). `-static-libgcc` still bundles the GCC
    unwind runtime, which is added implicitly.
    """
    env = env.copy()
    env["WHISPER_LINK_CXX_STATIC"] = "1"
    encoded = env.get("CARGO_ENCODED_RUSTFLAGS")
    if encoded is not None:
        parts = [part for part in encoded.split("\x1f") if part]
        parts += _STATIC_CXX_RUSTFLAGS
        env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(parts)
    else:
        key = f"CARGO_TARGET_{target.upper().replace('-', '_')}_RUSTFLAGS"
        existing = env.get(key, "")
        spelled = " ".join(
            flag.replace("-Clink-arg=", "-C link-arg=") for flag in _STATIC_CXX_RUSTFLAGS
        )
        env[key] = " ".join(filter(None, (existing, spelled)))
    return env


#: Where `cmd_wheel` stages sidecar debug info. Deliberately NOT `dist/`:
#: `_build-target.yml` uploads `dist/*` as the `wheels-*` artifact, which
#: `publish-pypi` feeds straight to twine as `packages-dir`. A non-package file
#: there fails the upload, which is the exact blocker split-debuginfo fixes.
DEBUGINFO_DIR = ROOT / "dist-debuginfo"


def collect_debuginfo(target: str, profile: str) -> list[Path]:
    """Stage this triple's sidecar debug info for the release job.

    `[profile.release] split-debuginfo = "packed"` (Cargo.toml) writes the bulk
    of the DWARF beside each binary instead of inside it. On ELF that is a
    `.dwp`; on MSVC/Apple `packed` is already the default and the sidecar is a
    `.pdb` / `.dSYM`, neither of which this collects -- only the `.dwp` is a
    single file that is not already produced today, and only ELF embedded its
    debug info in the shipped wheel.

    Only `clud` itself, not the shipped shims. `crates/clud-bin/src/main.rs` is
    the sole binary that installs the crash reporter, and each shim currently
    shares clud-bin's whole dep tree, so their `.dwp`s are ~91 MB of duplicate
    DWARF apiece -- five per Linux triple would put ~900 MB on every release
    page to symbolicate crashes nothing reports. Their line tables stay
    embedded regardless, so a shim panic still resolves file:line unaided.

    Best-effort by contract: a target that produces no `.dwp` returns an empty
    list and logs it. Missing symbols must never block a release -- the wheel
    shrinking is the hard requirement, attaching symbols the secondary one.
    """
    source = ROOT / "target" / target / profile
    found = [path for path in (source / "clud.dwp",) if path.is_file()]
    if not found:
        print(f"no clud.dwp under {source} (expected off ELF targets)", flush=True)
        return []
    DEBUGINFO_DIR.mkdir(parents=True, exist_ok=True)
    staged: list[Path] = []
    for dwp in found:
        # Suffix the triple so `merge-multiple: true` cannot collide the
        # per-target artifacts on top of each other in the release job.
        dest = DEBUGINFO_DIR / f"{dwp.stem}-{target}.dwp"
        dest.write_bytes(dwp.read_bytes())
        print(f"debug info: {dest} ({dest.stat().st_size:,} bytes)", flush=True)
        staged.append(dest)
    return staged


def cmd_wheel(args: argparse.Namespace) -> int:
    """Build the wheel into dist/ for this triple.

    Maturin normally reuses the same `target/<triple>/` directory the compile
    step populated. Release GNU Linux wheels are the exception: their
    manylinux link must not reuse a host-default glibc artifact.
    """
    from ci.env import maturin_argv
    from ci.webterm_wheel import add_companion, companion_name, desktop_target

    env = build_env(args.target, args.strategy)
    profile = "release" if args.profile == "release" else "debug"
    companion = (
        ROOT / "clud-webterm" / "target" / args.target / profile / companion_name(args.target)
    )
    if desktop_target(args.target):
        command = [
            "soldr",
            "build",
            "--manifest-path",
            str(ROOT / "clud-webterm" / "Cargo.toml"),
            "--target",
            args.target,
        ]
        if args.profile == "release":
            command.append("--release")
        if run(command, env) != 0:
            return 1
    if _is_windows(args.target) and args.strategy == "soldr":
        target_dir = ROOT / "target"
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
        add_companion(wheel, companion, args.target)
        print(f"packaged soldr-built Windows wheel: {wheel}")
        collect_debuginfo(args.target, profile)
        return 0
    if args.profile == "release" and args.target.endswith("-unknown-linux-gnu"):
        # Bundle the C++ runtime so the manylinux_2_17 audit sees only glibc.
        env = static_cxx_runtime_env(args.target, env)
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
            # audited for manylinux compliance. maturin audits by default on
            # Linux even with no --compatibility flag; --compatibility linux
            # opts out. The release path below asserts the floor properly.
            subcommand += ["--compatibility", "linux"]
    else:
        subcommand.append("--release")
        if args.target.endswith("-unknown-linux-gnu"):
            # No --zig. soldr's blessed catalogue GNU toolchain
            # (gcc-13.3.0-glibc-2.17-1, soldr#2238) is prepared by `soldr
            # prepare --target <triple>` in setup-build and supplies the pinned
            # glibc-2.17 floor for every object -- Rust and the whisper.cpp
            # C/C++ alike -- so maturin's manylinux2014 audit passes without
            # zig. This replaces the `maturin --zig` path and the env-scrub
            # denylist it required. See docs/architecture/ci.md and soldr#2299.
            subcommand += ["--compatibility", "manylinux2014"]
        else:
            subcommand += ["--compatibility", "pypi"]

    if run(maturin_argv(subcommand, env=env), env) != 0:
        return 1

    from ci.build_wheel import built_wheels, verify_wheel_scripts
    from ci.wheel_repair import repair_windows_gnu_wheel

    wheels = built_wheels()
    if not wheels:
        print("build completed but produced no wheel", file=sys.stderr)
        return 1
    for wheel in wheels:
        if desktop_target(args.target):
            add_companion(wheel, companion, args.target)
        repair_windows_gnu_wheel(wheel)
        if verify_wheel_scripts(wheel) != 0:
            return 1
    collect_debuginfo(args.target, profile)
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

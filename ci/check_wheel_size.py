"""Report built wheel sizes, and fail when one would be refused by PyPI.

Issue #1017. Three releases -- 2.7.2, 2.7.3, 2.7.4 -- were tagged and built
green, and none of them published. The only red step was the upload, and
because `publish-release` is skipped when `publish-pypi` fails, a failed
release looked like nothing had happened: the repo quietly kept showing
2.7.1 as `Latest`. The cause was a linux wheel that had grown past PyPI's
100 MB per-file limit, 7x the Windows and macOS wheels beside it.

Nothing measured the wheels, so nothing said so. This does.

Two behaviours, deliberately separate:

  * **Always report.** Every wheel's size is printed, largest first, pass
    or fail. The 7x gap that made the regression obvious in hindsight is
    only obvious if someone can see the numbers, and a build log is where
    they will be looking.
  * **Fail only against a cap the caller sets.** `--max-mb` is opt-in
    because the limit is PyPI's, and PyPI only ever sees release wheels.
    Applying a release-shaped cap to a dev wheel would be asserting
    something about a number nobody has measured; a check that fails for a
    reason its author guessed at teaches people to raise the number.

Usage:
    python -m ci.check_wheel_size --dist-dir dist/
    python -m ci.check_wheel_size --dist-dir dist/ --max-mb 90
    python -m ci.check_wheel_size dist/clud-2.7.9-py3-none-win_amd64.whl
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# PyPI refuses any single file larger than this. It is a hard limit on the
# upload, not a warning, and a project-specific increase has to be requested
# from the PyPI admins.
PYPI_FILE_LIMIT_MB = 100

# What the release lane passes. The headroom is deliberate: at the time this
# was written the largest release wheel was 47.6 MB (linux x86_64) against
# macOS at 40.8 and Windows at 34.2, so 90 leaves room for ordinary growth
# while still catching a return to the ~101 MB wheel that caused #1017.
# A cap set just above today's size would fail on the next dependency bump
# and get raised without anyone thinking about it.
RELEASE_MAX_MB = 90

_MB = 1024 * 1024


def wheel_size_mb(path: Path) -> float:
    """Size in MB of the wheel as PyPI measures it.

    The compressed archive on disk, not the sum of its members: the limit
    applies to the uploaded file.
    """
    return path.stat().st_size / _MB


def collect_wheels(wheels: list[Path], dist_dir: Path | None) -> list[Path]:
    """Wheel paths from explicit args and/or a directory, deduped, in order."""
    paths = list(wheels)
    if dist_dir:
        paths.extend(sorted(dist_dir.glob("*.whl")))
    seen: set[str] = set()
    unique: list[Path] = []
    for path in paths:
        key = str(path.resolve())
        if key not in seen:
            seen.add(key)
            unique.append(path)
    return unique


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Report wheel sizes and fail when one exceeds --max-mb"
    )
    parser.add_argument("wheels", nargs="*", help="wheel paths to check")
    parser.add_argument(
        "--dist-dir",
        type=Path,
        default=None,
        help="check every *.whl in this directory",
    )
    parser.add_argument(
        "--max-mb",
        type=float,
        default=None,
        help=(
            "fail if any wheel is at least this many MB. Omit to report "
            f"sizes without failing. The release lane passes {RELEASE_MAX_MB}, "
            f"under PyPI's {PYPI_FILE_LIMIT_MB} MB per-file limit."
        ),
    )
    args = parser.parse_args(argv)

    paths = collect_wheels([Path(p) for p in args.wheels], args.dist_dir)
    if not paths:
        # Matches `check_windows_wheel`: a lane that built no wheel is not a
        # failure, it is a lane that had nothing to check.
        print("no wheels to check", file=sys.stderr)
        return 0

    missing = [p for p in paths if not p.is_file()]
    for path in missing:
        print(f"FAIL: {path}: not a file", file=sys.stderr)
    present = [p for p in paths if p.is_file()]

    sized = sorted(
        ((path, wheel_size_mb(path)) for path in present),
        key=lambda item: item[1],
        reverse=True,
    )
    for path, size in sized:
        print(f"  {size:7.1f} MB  {path.name}", file=sys.stderr)

    if missing:
        return 1
    if args.max_mb is None:
        print(
            f"{len(sized)} wheel(s) measured; no --max-mb given, so nothing "
            "was enforced.",
            file=sys.stderr,
        )
        return 0

    over = [(path, size) for path, size in sized if size >= args.max_mb]
    if over:
        print(
            f"\nFAIL: {len(over)} wheel(s) at or above the {args.max_mb:g} MB "
            f"limit (PyPI refuses anything over {PYPI_FILE_LIMIT_MB} MB):",
            file=sys.stderr,
        )
        for path, size in over:
            print(f"  {path.name}: {size:.1f} MB", file=sys.stderr)
        print(
            "\nA wheel this size will be rejected at upload, and a failed "
            "upload skips the release job -- so the tag would publish "
            "nothing and look like it succeeded. That is #1017; fix the "
            "size rather than raising the limit.",
            file=sys.stderr,
        )
        return 1

    largest = sized[0]
    print(
        f"OK: largest wheel {largest[0].name} at {largest[1]:.1f} MB, "
        f"under the {args.max_mb:g} MB limit.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

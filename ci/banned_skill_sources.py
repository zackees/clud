"""One source of truth for bundled skills (#847).

clud once shipped **two** bundled-skill registries with two independent
installers writing the same `~/.claude/skills/<name>/SKILL.md`. Each pass
classified the other's output as drift and rewrote it, so every launch
reported `updated /clud-pr` and `updated /clud-issue`, and the newer
`assets/skills/` bodies were silently reverted to older root-`skills/`
copies on every run. Neither installer was wrong on its own — nothing
enforced that the two registries owned disjoint names. See DD-039.

Three rules keep that from coming back:

1. Skill bodies may only be embedded from `crates/clud-bin/assets/skills/`.
   A second source tree is what let the two registries diverge.
2. Only the modules named in `SKILL_WRITER_ALLOWLIST` may build a path into
   a backend's skills directory. One writer, statically enforced.
3. No second skill source tree may reappear at the repo root.

Rule 1 alone would have caught the original bug: the retired installer
embedded five of its twelve skills from root `skills/`.

Run via `bash lint` (see `ci/lint.py`), alongside `banned_imports.py` and
`banned_cross_tools.py`, which enforce chokepoint rules the same way.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ci.banned_cross_tools import _strip_rust_comments

ROOT = Path(__file__).resolve().parents[1]

#: The only tree bundled skill bodies may be embedded from.
CANONICAL_SKILL_DIR = "assets/skills/"

#: Modules permitted to construct a backend skills-directory path. Keep this
#: as small as possible — every entry is another place a second installer
#: could grow. `skills_home.rs` is here because it resolves the home dir that
#: `skills.rs` joins onto.
SKILL_WRITER_ALLOWLIST = frozenset({"skills.rs", "skills_home.rs", "skills_tests.rs"})

#: Per-line escape hatch, mirroring `banned_cross_tools.ALLOW_MARKER`. Prose
#: explaining these rules necessarily names the banned shapes, and a module
#: docstring is prose no comment-stripper sees. Strictly line-scoped:
#: `rg 'skill-source-lint: allow'` lists every escape in the tree.
ALLOW_MARKER = "skill-source-lint: allow"

#: `include_str!("…/SKILL.md")` — any path, so the check below can decide
#: whether it points at the canonical tree rather than matching only bad ones.
SKILL_INCLUDE_RE = re.compile(r"""include_str!\s*\(\s*"([^"]*SKILL\.md)"\s*\)""")

#: A backend *skills* dir being assembled. Deliberately narrow: an earlier
#: draft matched any `.claude` / `.codex` literal and produced 58 hits on
#: `settings.json`, `hooks.json`, `config.toml` and worktree paths — none of
#: which install skills. A lint that noisy gets suppressed rather than
#: obeyed, so this matches only two shapes:
#:
#:   * a combined literal path, `".claude/skills"` / `".codex\\skills"`
#:   * a `.join("skills")` segment, which is how the dir is built piecewise
SKILLS_PATH_RE = re.compile(
    r"""["'][^"']*\.(?:claude|codex)[/\\]skills|\.join\(\s*["']skills["']\s*\)"""
)


def _rs_files() -> list[Path]:
    crates = ROOT / "crates"
    if not crates.is_dir():
        return []
    return [p for p in sorted(crates.rglob("*.rs")) if p.is_file()]


def scan_skill_includes(text: str) -> list[tuple[int, str]]:
    """Skill bodies embedded from anywhere but the canonical assets tree."""
    violations: list[tuple[int, str]] = []
    stripped = _strip_rust_comments(text)
    for number, (raw, line) in enumerate(
        # strict=True: `_strip_rust_comments` blanks comments character for
        # character, so the line counts must match. A silent mismatch would
        # mean scanning fewer lines than the file has — missed violations.
        zip(text.splitlines(), stripped.splitlines(), strict=True),
        start=1,
    ):
        if ALLOW_MARKER in raw:
            continue
        for included in SKILL_INCLUDE_RE.findall(line):
            # Collapse runs of either separator: Rust source escapes Windows
            # separators (`..\\assets\\skills\\`), and a naive backslash swap
            # turns that into `..//assets//skills//`, which no longer matches
            # the canonical prefix — a false positive on a legitimate path.
            normalized = re.sub(r"[\\/]+", "/", included)
            if CANONICAL_SKILL_DIR not in normalized:
                violations.append((number, raw.strip()))
    return violations


def scan_skill_writers(path: Path, text: str) -> list[tuple[int, str]]:
    """Backend skills-dir paths built outside the allowlisted modules."""
    if path.name in SKILL_WRITER_ALLOWLIST:
        return []
    violations: list[tuple[int, str]] = []
    stripped = _strip_rust_comments(text)
    for number, (raw, line) in enumerate(
        # strict=True: `_strip_rust_comments` blanks comments character for
        # character, so the line counts must match. A silent mismatch would
        # mean scanning fewer lines than the file has — missed violations.
        zip(text.splitlines(), stripped.splitlines(), strict=True),
        start=1,
    ):
        if ALLOW_MARKER in raw:
            continue
        if SKILLS_PATH_RE.search(line):
            violations.append((number, raw.strip()))
    return violations


def scan_second_source_tree() -> list[str]:
    """A second skill source tree at the repo root."""
    root_skills = ROOT / "skills"
    if not root_skills.is_dir():
        return []
    # Posix separators so the message reads the same on every platform and
    # tests do not have to branch on the host OS.
    found = sorted(p.relative_to(ROOT).as_posix() for p in root_skills.rglob("SKILL.md"))
    return found or [root_skills.relative_to(ROOT).as_posix()]


def main() -> int:
    total = 0

    for path in _rs_files():
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        rel = path.relative_to(ROOT)

        for number, line in scan_skill_includes(text):
            print(
                f"{rel}:{number}: BANNED — embed skill bodies from "
                f"crates/clud-bin/{CANONICAL_SKILL_DIR} only (one source of "
                f"truth, DD-039)",
                file=sys.stderr,
            )
            print(f"  {line}", file=sys.stderr)
            total += 1

        for number, line in scan_skill_writers(path, text):
            allowed = ", ".join(sorted(SKILL_WRITER_ALLOWLIST))
            print(
                f"{rel}:{number}: BANNED — only {allowed} may build a backend "
                f"skills path; a second installer is what #847 removed",
                file=sys.stderr,
            )
            print(f"  {line}", file=sys.stderr)
            total += 1

    for found in scan_second_source_tree():
        print(
            f"{found}: BANNED — bundled skills live in "
            f"crates/clud-bin/{CANONICAL_SKILL_DIR}; a second source tree is "
            f"what caused the #847 install ping-pong",
            file=sys.stderr,
        )
        total += 1

    if total:
        print(
            f"\n{total} banned skill-source violation(s) found. "
            "Bundled skills have exactly one source of truth — see DD-039.",
            file=sys.stderr,
        )
        return 1

    print("No banned skill sources found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

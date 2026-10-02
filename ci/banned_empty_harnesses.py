"""Every cargo target that builds a test harness must contain a test (#1714).

`cargo test --workspace --no-run` builds one harness per target whose `test`
setting is on, and each harness statically links the whole workspace. zccache
never caches harness link products (zackees/zccache#1525), so an empty harness
costs its full compile and link on every CI run, hosted or local, and ships in
every test bundle for nothing. A target with no `#[test]` in its module tree
must say `test = false` in its manifest.

The scan is textual: it follows `mod name;` declarations (and `#[path]`) from
the target's root file and looks for a test attribute (`#[test]`,
`#[tokio::test]`, ...). A cfg-gated test still counts, because the harness
runs it on some platform.

Run via `bash lint` (see `ci/lint.py`).
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]

#: `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = "...")]`; never `#[cfg(test)]`.
TEST_ATTR = re.compile(r"#\[\s*(?:[A-Za-z_]\w*\s*::\s*)*test\s*[\]\(]")
#: `#[path = "x.rs"]` immediately before a `mod name;` declaration.
PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
MOD_DECL = re.compile(
    r'((?:#\[\s*path\s*=\s*"[^"]+"\s*\]\s*|#\[[^\]]*\]\s*)*)'
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;"
)
LINE_COMMENT = re.compile(r"//[^\n]*")

REASON = (
    "this target builds a test harness with no tests; set `test = false` on it "
    "in its Cargo.toml (#1714: every harness costs a full uncached compile and "
    "link on every CI run)."
)


@dataclass(frozen=True)
class Target:
    """One harness-producing cargo target: its manifest, kind, name and root file."""

    manifest: Path
    kind: str
    name: str
    root: Path


def _mod_file(owner: Path, name: str, explicit: str | None) -> Path | None:
    """Resolve `mod name;` declared in `owner` the way rustc does."""
    if explicit is not None:
        candidate = owner.parent / explicit
        return candidate if candidate.is_file() else None
    if owner.name in {"main.rs", "lib.rs", "mod.rs"}:
        base = owner.parent
    else:
        base = owner.parent / owner.stem
    for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
        if candidate.is_file():
            return candidate
    return None


def has_test(root: Path) -> bool:
    """Whether any file in `root`'s module tree carries a test attribute."""
    seen: set[Path] = set()
    stack = [root]
    while stack:
        path = stack.pop()
        if path in seen:
            continue
        seen.add(path)
        text = LINE_COMMENT.sub("", path.read_text(encoding="utf-8"))
        if TEST_ATTR.search(text):
            return True
        for attrs, name in MOD_DECL.findall(text):
            explicit = PATH_ATTR.search(attrs)
            child = _mod_file(path, name, explicit.group(1) if explicit else None)
            if child is not None:
                stack.append(child)
    return False


def _auto_tests(pkg_dir: Path) -> list[tuple[str, Path]]:
    tests = pkg_dir / "tests"
    found: list[tuple[str, Path]] = []
    if not tests.is_dir():
        return found
    for entry in sorted(tests.iterdir()):
        if entry.is_file() and entry.suffix == ".rs":
            found.append((entry.stem, entry))
        elif entry.is_dir() and (entry / "main.rs").is_file():
            found.append((entry.name, entry / "main.rs"))
    return found


def _auto_bins(pkg_dir: Path, package: str) -> list[tuple[str, Path]]:
    found: list[tuple[str, Path]] = []
    if (pkg_dir / "src" / "main.rs").is_file():
        found.append((package, pkg_dir / "src" / "main.rs"))
    bin_dir = pkg_dir / "src" / "bin"
    if bin_dir.is_dir():
        for entry in sorted(bin_dir.iterdir()):
            if entry.is_file() and entry.suffix == ".rs":
                found.append((entry.stem, entry))
            elif entry.is_dir() and (entry / "main.rs").is_file():
                found.append((entry.name, entry / "main.rs"))
    return found


def _merge(
    explicit: list[dict], auto: list[tuple[str, Path]], enabled: bool, pkg_dir: Path
) -> list[tuple[str, Path, bool]]:
    """Explicit entries override auto-discovered ones of the same name or path."""
    out: dict[str, tuple[str, Path, bool]] = {}
    if enabled:
        for name, path in auto:
            out[name] = (name, path, True)
    discovered = dict(auto)
    for entry in explicit:
        if "path" in entry:
            path = pkg_dir / entry["path"]
        elif entry["name"] in discovered:
            path = discovered[entry["name"]]
        else:
            continue
        for key, (_, auto_path, _) in list(out.items()):
            if auto_path == path:
                del out[key]
        out[entry["name"]] = (entry["name"], path, entry.get("test", True))
    return list(out.values())


def package_targets(manifest: Path) -> list[Target]:
    """The test-enabled lib, bin and integration-test targets of one package."""
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    pkg = data["package"]
    pkg_dir = manifest.parent
    targets: list[Target] = []
    lib = data.get("lib", {})
    lib_root = pkg_dir / lib.get("path", "src/lib.rs")
    if lib_root.is_file() and lib.get("test", True):
        targets.append(Target(manifest, "lib", lib.get("name", pkg["name"]), lib_root))
    bins = _merge(
        data.get("bin", []), _auto_bins(pkg_dir, pkg["name"]), pkg.get("autobins", True), pkg_dir
    )
    tests = _merge(data.get("test", []), _auto_tests(pkg_dir), pkg.get("autotests", True), pkg_dir)
    for kind, entries in (("bin", bins), ("test", tests)):
        for name, root, enabled in entries:
            if enabled and root.is_file():
                targets.append(Target(manifest, kind, name, root))
    return targets


def workspace_manifests(root: Path = ROOT) -> list[Path]:
    data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    return [root / member / "Cargo.toml" for member in data["workspace"]["members"]]


def empty_harnesses(root: Path = ROOT) -> list[Target]:
    return [
        target
        for manifest in workspace_manifests(root)
        for target in package_targets(manifest)
        if not has_test(target.root)
    ]


def main() -> int:
    failures = empty_harnesses()
    for target in failures:
        where = target.manifest.relative_to(ROOT).as_posix()
        print(f"{where}: {target.kind} `{target.name}`: BANNED -- {REASON}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

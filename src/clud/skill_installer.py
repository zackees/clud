"""Auto-installer for bundled skills, agents, and rules.

Copies curated markdown assets from the clud package to ~/.claude/
on first run or when the bundled version is upgraded. Respects
user-edited files by checking for a managed-by header.
"""

import json
import logging
import sys
from pathlib import Path

from .settings_manager import get_settings_file

logger = logging.getLogger(__name__)

# Bump this when bundled assets change — triggers auto-update on next run.
CURRENT_SKILLS_VERSION = "1.0.0"

# Files created by clud start with this line. If the user removes it,
# we treat the file as user-owned and never overwrite it.
MANAGED_HEADER = "<!-- managed-by: clud -->"

# Asset categories that map to subdirectories in both
# src/clud/assets/ and ~/.claude/
_CATEGORIES = ("agents", "skills", "rules")


def _get_claude_dir() -> Path:
    """Return ~/.claude/, creating it if needed."""
    d = Path.home() / ".claude"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _get_assets_dir() -> Path:
    """Return the bundled assets directory inside the clud package."""
    return Path(__file__).parent / "assets"


def is_clud_managed(path: Path) -> bool:
    """Check whether *path* was installed (and not edited) by clud."""
    try:
        first_line = path.read_text(encoding="utf-8").split("\n", 1)[0]
        return first_line.strip() == MANAGED_HEADER
    except (OSError, UnicodeDecodeError):
        return False


def needs_install() -> bool:
    """Return True if skills need to be installed or upgraded."""
    settings_file = get_settings_file()
    if not settings_file.exists():
        return True
    try:
        data = json.loads(settings_file.read_text(encoding="utf-8"))
        return data.get("skills_version") != CURRENT_SKILLS_VERSION
    except (json.JSONDecodeError, OSError):
        return True


def install_skills(quiet: bool = False) -> None:
    """Copy bundled agents/skills/rules to ``~/.claude/``.

    - New files are created with the managed header.
    - Existing files that still have the managed header are overwritten
      (upgrade path).
    - Files where the user removed the header are left untouched.
    """
    assets_dir = _get_assets_dir()
    claude_dir = _get_claude_dir()

    for category in _CATEGORIES:
        src_dir = assets_dir / category
        if not src_dir.exists():
            continue

        dst_dir = claude_dir / category
        dst_dir.mkdir(parents=True, exist_ok=True)

        for src_file in src_dir.rglob("*.md"):
            rel = src_file.relative_to(src_dir)
            dst_file = dst_dir / rel
            dst_file.parent.mkdir(parents=True, exist_ok=True)

            # Read source content and prepend managed header
            raw = src_file.read_text(encoding="utf-8")
            managed_content = MANAGED_HEADER + "\n" + raw

            if dst_file.exists() and not is_clud_managed(dst_file):
                # User has edited this file — don't clobber
                continue

            dst_file.write_text(managed_content, encoding="utf-8")

    # Record the installed version
    settings_file = get_settings_file()
    try:
        data = json.loads(settings_file.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        data = {}
    data["skills_version"] = CURRENT_SKILLS_VERSION
    settings_file.write_text(json.dumps(data, indent=2), encoding="utf-8")

    if not quiet:
        print(
            f"Installed clud skills v{CURRENT_SKILLS_VERSION} (5 agents, 5 skills, 3 rules) to {claude_dir}",
            file=sys.stderr,
        )


def uninstall_skills(quiet: bool = False) -> None:
    """Remove all clud-managed files from ``~/.claude/`` and clear the version."""
    claude_dir = _get_claude_dir()

    removed = 0
    for category in _CATEGORIES:
        cat_dir = claude_dir / category
        if not cat_dir.exists():
            continue

        for md_file in list(cat_dir.rglob("*.md")):
            if is_clud_managed(md_file):
                md_file.unlink()
                removed += 1

        # Clean up empty directories
        for dirpath in sorted(cat_dir.rglob("*"), reverse=True):
            if dirpath.is_dir() and not any(dirpath.iterdir()):
                dirpath.rmdir()

    # Clear the version from settings
    settings_file = get_settings_file()
    try:
        data = json.loads(settings_file.read_text(encoding="utf-8"))
        data.pop("skills_version", None)
        settings_file.write_text(json.dumps(data, indent=2), encoding="utf-8")
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        pass

    if not quiet:
        print(f"Removed {removed} clud-managed files from {claude_dir}", file=sys.stderr)

"""Info command handler for clud agent."""

import platform
import sys

from clud.claude_installer import (
    find_claude_code,
    find_npm_executable,
    get_claude_version,
    get_clud_bin_dir,
    get_clud_npm_dir,
    get_local_claude_path,
)


def handle_info_command() -> int:
    """Handle the --info command by displaying Claude Code installation information."""
    print("Claude Code Installation Information", file=sys.stderr)
    print("=" * 70, file=sys.stderr)
    print(file=sys.stderr)

    # Find Claude Code executable
    claude_path = find_claude_code()

    if not claude_path:
        print("Status: NOT FOUND", file=sys.stderr)
        print(file=sys.stderr)
        print("Claude Code is not installed or not in PATH.", file=sys.stderr)
        print("Install with: clud --install-claude", file=sys.stderr)
        return 1

    # Display installation path
    print("Status: INSTALLED", file=sys.stderr)
    print(file=sys.stderr)
    print("Executable Path:", file=sys.stderr)
    print(f"  {claude_path}", file=sys.stderr)
    print(file=sys.stderr)

    # Get and display version
    version = get_claude_version(claude_path)
    if version:
        print("Version:", file=sys.stderr)
        print(f"  {version}", file=sys.stderr)
    else:
        print("Version: Unable to determine", file=sys.stderr)
    print(file=sys.stderr)

    # Display installation type
    local_path = get_local_claude_path()
    if local_path and str(local_path) == claude_path:
        print("Installation Type: Local (clud-managed)", file=sys.stderr)
        print(f"  Installed in: {get_clud_npm_dir()}", file=sys.stderr)
    else:
        print("Installation Type: System/Global", file=sys.stderr)
        print("  Found in PATH", file=sys.stderr)
    print(file=sys.stderr)

    # Display npm information
    npm_path = find_npm_executable()
    if npm_path:
        print("npm Executable:", file=sys.stderr)
        print(f"  {npm_path}", file=sys.stderr)
    else:
        print("npm Executable: NOT FOUND", file=sys.stderr)
    print(file=sys.stderr)

    # Display clud directories
    print("clud Directories:", file=sys.stderr)
    print(f"  npm packages: {get_clud_npm_dir()}", file=sys.stderr)
    print(f"  binaries: {get_clud_bin_dir()}", file=sys.stderr)
    print(file=sys.stderr)

    # Display platform information
    print("Platform Information:", file=sys.stderr)
    print(f"  OS: {platform.system()}", file=sys.stderr)
    print(f"  Python: {sys.version.split()[0]}", file=sys.stderr)
    print(file=sys.stderr)

    print("=" * 70, file=sys.stderr)
    return 0

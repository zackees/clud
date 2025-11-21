"""Install Claude command handler for clud agent."""

import sys

from clud.claude_installer import (
    find_claude_code,
    install_claude_local,
    is_claude_installed_locally,
)


def handle_install_claude_command() -> int:
    """Handle the --install-claude command by installing Claude Code locally."""
    print("Installing Claude Code to ~/.clud/npm...", file=sys.stderr)
    print(file=sys.stderr)

    # Check if already installed
    if is_claude_installed_locally():
        print("Claude Code is already installed locally.", file=sys.stderr)
        claude_path = find_claude_code()
        if claude_path:
            print(f"Location: {claude_path}", file=sys.stderr)

        sys.stdout.flush()
        response = input("\nReinstall? [y/N]: ").strip().lower()
        if response not in ["y", "yes"]:
            print("Installation cancelled.", file=sys.stderr)
            return 0

    # Install Claude Code
    if install_claude_local(verbose=True):
        print(file=sys.stderr)
        print("✓ Installation complete!", file=sys.stderr)
        print("You can now use 'clud' to run Claude Code.", file=sys.stderr)
        return 0
    else:
        print(file=sys.stderr)
        print("✗ Installation failed.", file=sys.stderr)
        print(file=sys.stderr)
        print("You can try installing globally instead:", file=sys.stderr)
        print("  npm install -g @anthropic-ai/claude-code@latest", file=sys.stderr)
        return 1

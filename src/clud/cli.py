"""Minimal CLI entry point for clud - routes to agent module."""

import sys

from .agent_cli import main as agent_main


def main(args: list[str] | None = None) -> int:
    """Main entry point - delegate to agent."""
    return agent_main(args)


if __name__ == "__main__":
    sys.exit(main())

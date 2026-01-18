"""
Agent package for clud.

This package contains modularized components for agent execution,
extracted from the monolithic agent_cli.py file.
"""

from clud.agent.claude_finder import _find_claude_path
from clud.agent.config import get_clud_config_dir
from clud.agent.exceptions import CludError, ConfigError, ValidationError
from clud.agent.hooks import register_hooks_from_config, trigger_hook_sync
from clud.agent.lint_runner import _check_agent_artifacts, _find_and_run_lint_test
from clud.agent.task_manager import _handle_existing_loop, _print_red_banner

__all__ = [
    # Exceptions
    "CludError",
    "ValidationError",
    "ConfigError",
    # Config
    "get_clud_config_dir",
    # Claude Finder
    "_find_claude_path",
    # Hooks
    "register_hooks_from_config",
    "trigger_hook_sync",
    # Task Manager
    "_handle_existing_loop",
    "_print_red_banner",
    # Lint Runner
    "_find_and_run_lint_test",
    "_check_agent_artifacts",
]

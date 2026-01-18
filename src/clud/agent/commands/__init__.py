"""Command handlers for clud agent.

This package contains all command handler functions extracted from agent_cli.py.
Each command handler is in its own module for better organization and maintainability.
"""

from clud.agent.commands.codeup import (
    handle_codeup_command,
    handle_codeup_publish_command,
)
from clud.agent.commands.fix import handle_fix_command
from clud.agent.commands.info import handle_info_command
from clud.agent.commands.init_loop import handle_init_loop_command
from clud.agent.commands.install_claude import handle_install_claude_command
from clud.agent.commands.lint import handle_lint_command
from clud.agent.commands.test import handle_test_command

__all__ = [
    "handle_codeup_command",
    "handle_codeup_publish_command",
    "handle_fix_command",
    "handle_info_command",
    "handle_init_loop_command",
    "handle_install_claude_command",
    "handle_lint_command",
    "handle_test_command",
]

"""Plan command handler for clud agent."""

import sys

from clud.agent.subprocess import run_clud_subprocess

PLAN_PROMPT_TEMPLATE = (
    "You MUST use plan mode for this task. Enter plan mode immediately by using the EnterPlanMode tool, "
    "then create a comprehensive plan for the following task:\n\n"
    "{prompt}\n\n"
    "After the plan is created, accept it and execute every step to completion. "
    "Do not stop after planning - carry out the full implementation."
)


def handle_plan_command(prompt: str | None) -> int:
    """Handle the plan command by running clud with a plan-mode prompt.

    Args:
        prompt: The task description to plan and execute.

    Returns:
        Exit code from clud subprocess.
    """
    if not prompt:
        print("Error: plan command requires a prompt argument", file=sys.stderr)
        print('Usage: clud plan "your task description"', file=sys.stderr)
        return 2

    plan_prompt = PLAN_PROMPT_TEMPLATE.format(prompt=prompt)
    return run_clud_subprocess(plan_prompt)

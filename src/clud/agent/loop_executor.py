"""Loop execution logic for multi-iteration agent runs."""

import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from running_process import RunningProcess

from ..json_formatter import StreamJsonFormatter

if TYPE_CHECKING:
    from ..agent_args import Args
from .command_builder import (
    _build_claude_command,
    _get_model_from_args,
    _print_debug_info,
    _print_model_message,
    _wrap_command_for_git_bash,
)
from .lint_runner import _find_and_run_lint_test
from .loop_logger import LoopLogger, create_logging_formatter_callback
from .motivation import write_motivation_file
from .subprocess import _execute_command
from .task_info import TaskInfo
from .task_manager import _handle_existing_loop, _print_loop_banner, _print_red_banner
from .user_input import _open_file_in_editor

# ANSI color codes for yellow warning
YELLOW = "\033[93m"
RESET = "\033[0m"


def _ensure_loop_in_gitignore() -> None:
    """
    Check if .loop or ./.loop is in .gitignore and add it if missing.

    Only performs the check if both .gitignore and .git exist in the current directory.
    Displays a yellow warning message when adding .loop to .gitignore.
    """
    # Check if .git directory exists (confirming we're in a git repo)
    git_dir = Path(".git")
    if not git_dir.exists() or not git_dir.is_dir():
        return

    # Check if .gitignore exists
    gitignore_path = Path(".gitignore")
    if not gitignore_path.exists():
        return

    # Read .gitignore contents
    try:
        gitignore_content = gitignore_path.read_text(encoding="utf-8")
    except Exception:
        # If we can't read .gitignore, silently return
        return

    # Check if .loop or ./.loop is already in .gitignore
    lines = gitignore_content.splitlines()
    for line in lines:
        stripped = line.strip()
        # Check for .loop or ./.loop (with or without leading /)
        if stripped in (".loop", "./.loop", "/.loop"):
            # Already present, nothing to do
            return

    # Not found - add .loop to .gitignore
    try:
        # Add .loop to .gitignore (with newline if file doesn't end with one)
        if gitignore_content and not gitignore_content.endswith("\n"):
            gitignore_content += "\n"
        gitignore_content += ".loop\n"
        gitignore_path.write_text(gitignore_content, encoding="utf-8")

        # Print warning message in yellow
        print(f"{YELLOW}Warning: .loop was added to .gitignore{RESET}", file=sys.stderr)
    except Exception:
        # If we can't write to .gitignore, silently fail
        # This ensures we don't break the loop mode if there's a permission issue
        pass


def _generate_done_summary(claude_path: str, args: "Args") -> str | None:
    """Generate a two-sentence summary of DONE.md using Claude.

    Args:
        claude_path: Path to Claude executable
        args: Command-line arguments (used for model preference)

    Returns:
        Summary string or None if generation failed
    """
    try:
        # Build command to summarize DONE.md
        summary_cmd = [
            claude_path,
            "--dangerously-skip-permissions",
            "-p",
            "Read DONE.md and return a two sentence summary of what was achieved.",
        ]

        # Add model flag from args if specified
        if args.claude_args:
            summary_cmd.extend(args.claude_args)

        # Run command and capture output
        result = subprocess.run(
            summary_cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            encoding="utf-8",
            errors="replace",  # Replace undecodable bytes instead of raising exception
            check=False,
        )

        if result.returncode == 0 and result.stdout:
            # Extract the summary from stdout (strip whitespace)
            summary = result.stdout.strip()
            if summary:
                return summary

        return None

    except Exception as e:
        # If summary generation fails, just log and continue
        print(f"Warning: Failed to generate DONE.md summary: {e}", file=sys.stderr)
        return None


def _run_loop(args: "Args", claude_path: str, loop_count: int) -> int:
    """Run Claude in a loop, checking for DONE.md after each iteration."""
    loop_dir = Path(".loop")

    # Handle existing session from previous run
    should_continue, start_iteration = _handle_existing_loop(loop_dir)
    if not should_continue:
        return 2  # User cancelled

    # Create .loop directory if it doesn't exist (may have been deleted)
    loop_dir.mkdir(exist_ok=True)

    # Ensure .loop is in .gitignore (warn if added)
    _ensure_loop_in_gitignore()

    # Write motivation file for iterations 2+ (always overwrite to ensure fresh content)
    write_motivation_file(str(loop_dir))

    # Handle loop file if specified (e.g., LOOP.md or custom TASK.md)
    # Extract the file path from loop_value if it's a file
    loop_file_path: Path | None = None
    working_loop_file: Path | None = None

    if args.loop_value:
        # Check if loop_value is a file path (not just an integer or message)
        try:
            int(args.loop_value)
            # It's an integer, not a file path
        except ValueError:
            # Not an integer, check if it's a file that exists
            potential_file = Path(args.loop_value)
            if potential_file.exists() and potential_file.is_file():
                loop_file_path = potential_file
                # Create working copy in .loop/ with same name
                working_loop_file = loop_dir / loop_file_path.name

                # Copy the loop file to working location (only if doesn't exist yet)
                # This preserves the original file as read-only from agent's perspective
                if not working_loop_file.exists():
                    try:
                        shutil.copy2(loop_file_path, working_loop_file)
                    except Exception as e:
                        # If copy fails, we can't proceed with loop mode
                        print(f"Error: Failed to create working copy of {loop_file_path}: {e}", file=sys.stderr)
                        return 1

    # Initialize loop logger for appending all output to log.txt
    log_file = loop_dir / "log.txt"

    # DONE.md lives at project root, not .loop/
    done_file = Path("DONE.md")

    # Initialize or load task info
    info_file = loop_dir / "info.json"
    user_prompt = args.prompt if args.prompt else args.message
    task_info = TaskInfo.load(info_file)

    if task_info is None:
        # Create new task info for fresh session
        task_info = TaskInfo(
            session_id=str(uuid.uuid4()),
            start_time=time.time(),
            prompt=user_prompt,
            total_iterations=loop_count,
        )
        task_info.save(info_file)
    else:
        # Update existing task info for continuation
        task_info.total_iterations = loop_count
        task_info.save(info_file)

    # Print loop banner to explain file structure
    _print_loop_banner()

    # Start from determined iteration (may be > 1 if continuing previous session)
    with LoopLogger(log_file) as logger:
        for i in range(start_iteration - 1, loop_count):
            iteration_num = i + 1
            logger.print_stderr(f"\n--- Iteration {iteration_num}/{loop_count} ---")

            # Check if DONE.md was already validated in a previous iteration
            done_validated_marker = loop_dir / "done_validated"
            if done_validated_marker.exists():
                logger.print_stderr("✅ DONE.md was already validated. Halting immediately.")

                # Generate and display summary before opening
                logger.print_stderr("\n📝 Generating summary of completed work...")
                summary = _generate_done_summary(claude_path, args)
                if summary:
                    logger.print_stderr("\n" + "=" * 80)
                    logger.print_stderr("SUMMARY:")
                    logger.print_stderr(summary)
                    logger.print_stderr("=" * 80 + "\n")

                logger.print_stderr(f"Opening {done_file}...")
                _open_file_in_editor(done_file)
                return 0

            # Mark iteration start
            task_info.start_iteration(iteration_num)
            task_info.save(info_file)

            # Print the user's prompt for this iteration
            user_prompt = args.prompt if args.prompt else args.message
            if user_prompt:
                logger.print_stderr(f"Prompt: {user_prompt}")
                logger.print_stderr()  # Empty line for spacing

            # Build command with prompt injection, including iteration context
            # Pass the working file path so injected instructions reference the correct file
            working_file_str = str(working_loop_file) if working_loop_file else None
            cmd = _build_claude_command(
                args,
                claude_path,
                inject_prompt=True,
                iteration=iteration_num,
                total_iterations=loop_count,
                working_file=working_file_str,
            )
            # Wrap command in git-bash on Windows if available
            cmd = _wrap_command_for_git_bash(cmd)

            # Detect and print model message (for display only)
            model_flag = _get_model_from_args(args.claude_args)
            _print_model_message(model_flag)

            # Print debug info
            _print_debug_info(claude_path, cmd, args.verbose)

            # Execute the command with streaming if prompt is present
            if args.prompt:
                if args.plain:
                    # Plain mode: no JSON formatting, just pass through output
                    # TODO: Capture plain output to log file
                    returncode = RunningProcess.run_streaming(cmd)
                else:
                    # Create JSON formatter for beautiful output in loop mode
                    formatter = StreamJsonFormatter(
                        show_system=args.verbose,
                        show_usage=True,
                        show_cache=args.verbose,
                        verbose=args.verbose,
                    )
                    stdout_callback = create_logging_formatter_callback(formatter, logger)
                    returncode = RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
            else:
                returncode = _execute_command(cmd, use_shell=False, verbose=args.verbose)

            # Mark iteration end
            error_msg = f"Exit code: {returncode}" if returncode != 0 else None
            task_info.end_iteration(returncode, error_msg)
            task_info.save(info_file)

            if returncode != 0 and args.verbose:
                logger.print_stderr(f"Warning: Iteration {iteration_num} exited with code {returncode}")

            # Check if DONE.md was created (at project root)
            # FSM State: DONE.md exists → enter validation/fix loop (never delete DONE.md)
            if done_file.exists():
                # Validate that lint and test pass before accepting DONE.md
                logger.print_stderr(f"\n📋 DONE.md detected at project root after iteration {iteration_num}.")
                logger.print_stderr("Validating with `lint-test`...")

                # Error log file for validation failures
                error_log_file = loop_dir / "ERROR.log"

                # Run lint-test and capture output
                try:
                    # Find and run lint-test using shutil.which for validation
                    lint_test_returncode, lint_test_output = _find_and_run_lint_test()

                    # Display output to user and log it
                    logger.print_stdout(lint_test_output)

                    if lint_test_returncode != 0:
                        # FSM State: Validation failed → enter fix loop (keep DONE.md)
                        logger.print_stderr("❌ lint-test failed. Keeping DONE.md and attempting to fix...")

                        # Save full output to ERROR.log (with tee-like behavior - already printed above)
                        error_log_file.write_text(
                            f"# Lint-Test Validation Errors\n\nTimestamp: {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\n\n```\n{lint_test_output}\n```\n",
                            encoding="utf-8",
                        )
                        logger.print_stderr(f"  Saved validation output to {error_log_file}")

                        # FSM State: Fix loop (max 3 attempts, not 5)
                        max_fix_attempts = 3
                        retest_returncode: int = 1  # Initialize as failed
                        for fix_attempt in range(1, max_fix_attempts + 1):
                            logger.print_stderr(f"\n🔧 Fix attempt {fix_attempt}/{max_fix_attempts}...")

                            # Build fix prompt referencing ERROR.log and lint-test command
                            fix_prompt = (
                                "Read .loop/ERROR.log to see the linting and testing errors."
                                "Fix all the errors listed in ERROR.log. "
                                "You can run the `lint-test` command yourself to validate the errors and confirm they are fixed. "
                                "After fixing, the system will automatically re-run lint-test to verify."
                            )

                            # Build fix command (using -p flag for non-interactive)
                            fix_cmd = [claude_path, "--dangerously-skip-permissions", "-p", fix_prompt]

                            # Add model flag from args if specified (no default model)
                            fix_model_flag = _get_model_from_args(args.claude_args)
                            if args.claude_args:
                                fix_cmd.extend(args.claude_args)

                            if not args.plain:
                                fix_cmd.extend(["--output-format", "stream-json", "--verbose"])

                            # Print model message for fix attempt
                            _print_model_message(fix_model_flag)

                            # Execute fix command
                            if args.plain:
                                # TODO: Capture plain output to log file
                                RunningProcess.run_streaming(fix_cmd)
                            else:
                                formatter = StreamJsonFormatter(
                                    show_system=args.verbose,
                                    show_usage=True,
                                    show_cache=args.verbose,
                                    verbose=args.verbose,
                                )
                                stdout_callback = create_logging_formatter_callback(formatter, logger)
                                RunningProcess.run_streaming(fix_cmd, stdout_callback=stdout_callback)

                            # Re-run lint-test to check if fixed
                            logger.print_stderr(f"\n🔍 Re-running lint-test after fix attempt {fix_attempt}...")
                            retest_returncode, retest_output = _find_and_run_lint_test()

                            # Display retest output and log it
                            logger.print_stdout(retest_output)

                            if retest_returncode == 0:
                                # FSM State: Validation passed → mark as complete and halt
                                logger.print_stderr(f"✅ lint-test passed after {fix_attempt} fix attempt(s)!")

                                # Clean up ERROR.log since validation passed
                                if error_log_file.exists():
                                    error_log_file.unlink()
                                    logger.print_stderr(f"  Removed {error_log_file}")

                                # Accept DONE.md and mark as validated
                                task_info.mark_completed()
                                task_info.save(info_file)
                                done_validated_marker.write_text(
                                    f"DONE.md validated successfully on {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\nFix attempts: {fix_attempt}\n",
                                    encoding="utf-8",
                                )
                                break
                            else:
                                # FSM State: Still failing → update ERROR.log for next attempt
                                logger.print_stderr(f"❌ lint-test still failing after fix attempt {fix_attempt}")

                                # Update ERROR.log with latest output
                                error_log_file.write_text(
                                    f"# Lint-Test Validation Errors (Attempt {fix_attempt})\n\n"
                                    f"Timestamp: {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                                    f"Iteration: {iteration_num}/{loop_count}\n"
                                    f"Fix Attempt: {fix_attempt}/{max_fix_attempts}\n\n"
                                    f"```\n{retest_output}\n```\n",
                                    encoding="utf-8",
                                )

                                if fix_attempt == max_fix_attempts:
                                    # FSM State: Max attempts reached → halt with warning (keep DONE.md)
                                    _print_red_banner("LINTING/TESTING ISSUES REMAIN UNRESOLVED AFTER 3 FIX ATTEMPTS")
                                    logger.print_stderr(f"\nERROR: Failed to fix lint/test errors after {max_fix_attempts} attempts.")
                                    logger.print_stderr("Please review .loop/ERROR.log manually.")
                                    logger.print_stderr("DONE.md is kept at project root for review.")
                                    logger.print_stderr("Halting loop - linting & testing could not pass.")
                                    # NEVER delete DONE.md - keep it along with ERROR.log for manual review

                        # If we get here and retest_returncode == 0, we fixed it successfully
                        if retest_returncode == 0:
                            break  # Exit main loop - validation passed
                        else:
                            # FSM State: Still broken after max attempts - HALT (keep DONE.md)
                            # This prevents infinite loops and wasted API credits
                            logger.print_stderr(f"\n⚠️  Halting loop after {max_fix_attempts} failed fix attempts.")
                            logger.print_stderr("Review DONE.md and .loop/ERROR.log to understand the issues.")
                            break
                    else:
                        # FSM State: Validation passed on first attempt → accept DONE.md and halt
                        logger.print_stderr("✅ lint-test passed. Accepting DONE.md and halting early.")
                        task_info.mark_completed()
                        task_info.save(info_file)
                        done_validated_marker.write_text(
                            f"DONE.md validated successfully on {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\nValidated on first attempt (no fixes needed)\n",
                            encoding="utf-8",
                        )
                        break
                except KeyboardInterrupt:
                    raise  # Always re-raise KeyboardInterrupt
                except Exception as e:
                    # Validation failed due to exception (e.g., encoding error)
                    logger.print_stderr(f"Error during lint-test validation: {e}")
                    _print_red_banner("DONE.md VALIDATION FAILED DUE TO ERROR")
                    logger.print_stderr(f"Exception type: {type(e).__name__}")
                    logger.print_stderr(f"Exception details: {e}")
                    logger.print_stderr()
                    logger.print_stderr("⚠️  DONE.md will be KEPT and loop will HALT to prevent infinite loop.")
                    logger.print_stderr("Review the error above and fix the issue manually.")
                    logger.print_stderr("Possible causes:")
                    logger.print_stderr("  - Encoding errors in lint-test output (see BUG_CHARMAP.md)")
                    logger.print_stderr("  - lint-test command not found or failed to execute")
                    logger.print_stderr("  - Permission errors reading/writing files")
                    logger.print_stderr()
                    logger.print_stderr(f"DONE.md location: {done_file.absolute()}")

                    # Mark as completed (even though validation failed) to prevent re-running
                    task_info.mark_completed(error=f"Validation failed with exception: {e}")
                    task_info.save(info_file)

                    # Create a marker file to indicate validation was attempted but failed
                    validation_failed_marker = loop_dir / "done_validation_failed"
                    validation_failed_marker.write_text(
                        f"DONE.md validation failed on {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                        f"Iteration: {iteration_num}/{loop_count}\n"
                        f"Exception: {type(e).__name__}: {e}\n"
                        f"\n"
                        f"IMPORTANT: DONE.md was KEPT to prevent infinite loop.\n"
                        f"Fix the validation issue manually and re-run if needed.\n",
                        encoding="utf-8",
                    )

                    # HALT the loop (do NOT continue to next iteration)
                    break

        logger.print_stderr("\nAll iterations complete or halted early.")

        # Mark completion if all iterations finish without DONE.md
        if not done_file.exists():
            task_info.mark_completed(error="Completed all iterations without DONE.md")
            task_info.save(info_file)

        # Open DONE.md if it exists
        if done_file.exists():
            # Generate and display summary before opening
            logger.print_stderr("\n📝 Generating summary of completed work...")
            summary = _generate_done_summary(claude_path, args)
            if summary:
                logger.print_stderr("\n" + "=" * 80)
                logger.print_stderr("SUMMARY:")
                logger.print_stderr(summary)
                logger.print_stderr("=" * 80 + "\n")

            logger.print_stderr(f"Opening {done_file}...")
            _open_file_in_editor(done_file)

    return 0

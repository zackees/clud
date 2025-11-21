"""Loop execution logic for multi-iteration agent runs."""

import sys
import time
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from running_process import RunningProcess

from ..json_formatter import StreamJsonFormatter, create_formatter_callback

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
from .subprocess import _execute_command
from .task_info import TaskInfo
from .task_manager import _handle_existing_agent_task, _print_red_banner
from .user_input import _open_file_in_editor


def _run_loop(args: "Args", claude_path: str, loop_count: int) -> int:
    """Run Claude in a loop, checking for DONE.md after each iteration."""
    agent_task_dir = Path(".agent_task")

    # Handle existing session from previous run
    should_continue, start_iteration = _handle_existing_agent_task(agent_task_dir)
    if not should_continue:
        return 2  # User cancelled

    # Create .agent_task directory if it doesn't exist (may have been deleted)
    agent_task_dir.mkdir(exist_ok=True)

    # DONE.md lives at project root, not .agent_task/
    done_file = Path("DONE.md")

    # Initialize or load task info
    info_file = agent_task_dir / "info.json"
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

    # Start from determined iteration (may be > 1 if continuing previous session)
    for i in range(start_iteration - 1, loop_count):
        iteration_num = i + 1
        print(f"\n--- Iteration {iteration_num}/{loop_count} ---", file=sys.stderr)

        # Check if DONE.md was already validated in a previous iteration
        done_validated_marker = agent_task_dir / "done_validated"
        if done_validated_marker.exists():
            print("✅ DONE.md was already validated. Halting immediately.", file=sys.stderr)
            print(f"Opening {done_file}...", file=sys.stderr)
            _open_file_in_editor(done_file)
            return 0

        # Mark iteration start
        task_info.start_iteration(iteration_num)
        task_info.save(info_file)

        # Print the user's prompt for this iteration
        user_prompt = args.prompt if args.prompt else args.message
        if user_prompt:
            print(f"Prompt: {user_prompt}", file=sys.stderr)
            print(file=sys.stderr)  # Empty line for spacing

        # Build command with prompt injection, including iteration context
        cmd = _build_claude_command(
            args,
            claude_path,
            inject_prompt=True,
            iteration=iteration_num,
            total_iterations=loop_count,
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
                returncode = RunningProcess.run_streaming(cmd)
            else:
                # Create JSON formatter for beautiful output in loop mode
                formatter = StreamJsonFormatter(
                    show_system=args.verbose,
                    show_usage=True,
                    show_cache=args.verbose,
                    verbose=args.verbose,
                )
                stdout_callback = create_formatter_callback(formatter)
                returncode = RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
        else:
            returncode = _execute_command(cmd, use_shell=False, verbose=args.verbose)

        # Mark iteration end
        error_msg = f"Exit code: {returncode}" if returncode != 0 else None
        task_info.end_iteration(returncode, error_msg)
        task_info.save(info_file)

        if returncode != 0 and args.verbose:
            print(f"Warning: Iteration {iteration_num} exited with code {returncode}", file=sys.stderr)

        # Check if DONE.md was created (at project root)
        # FSM State: DONE.md exists → enter validation/fix loop (never delete DONE.md)
        if done_file.exists():
            # Validate that lint and test pass before accepting DONE.md
            print(f"\n📋 DONE.md detected at project root after iteration {iteration_num}.", file=sys.stderr)
            print("Validating with `lint-test`...", file=sys.stderr)

            # Error log file for validation failures
            error_log_file = agent_task_dir / "ERROR.log"

            # Run lint-test and capture output
            try:
                # Find and run lint-test using shutil.which for validation
                lint_test_returncode, lint_test_output = _find_and_run_lint_test()

                # Display output to user
                print(lint_test_output)

                if lint_test_returncode != 0:
                    # FSM State: Validation failed → enter fix loop (keep DONE.md)
                    print("❌ lint-test failed. Keeping DONE.md and attempting to fix...", file=sys.stderr)

                    # Save full output to ERROR.log (with tee-like behavior - already printed above)
                    error_log_file.write_text(
                        f"# Lint-Test Validation Errors\n\nTimestamp: {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\n\n```\n{lint_test_output}\n```\n",
                        encoding="utf-8",
                    )
                    print(f"  Saved validation output to {error_log_file}", file=sys.stderr)

                    # FSM State: Fix loop (max 3 attempts, not 5)
                    max_fix_attempts = 3
                    retest_returncode: int = 1  # Initialize as failed
                    for fix_attempt in range(1, max_fix_attempts + 1):
                        print(f"\n🔧 Fix attempt {fix_attempt}/{max_fix_attempts}...", file=sys.stderr)

                        # Build fix prompt referencing ERROR.log and lint-test command
                        fix_prompt = (
                            "Read .agent_task/ERROR.log to see the linting and testing errors. "
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
                            RunningProcess.run_streaming(fix_cmd)
                        else:
                            formatter = StreamJsonFormatter(
                                show_system=args.verbose,
                                show_usage=True,
                                show_cache=args.verbose,
                                verbose=args.verbose,
                            )
                            stdout_callback = create_formatter_callback(formatter)
                            RunningProcess.run_streaming(fix_cmd, stdout_callback=stdout_callback)

                        # Re-run lint-test to check if fixed
                        print(f"\n🔍 Re-running lint-test after fix attempt {fix_attempt}...", file=sys.stderr)
                        retest_returncode, retest_output = _find_and_run_lint_test()

                        # Display retest output
                        print(retest_output)

                        if retest_returncode == 0:
                            # FSM State: Validation passed → mark as complete and halt
                            print(f"✅ lint-test passed after {fix_attempt} fix attempt(s)!", file=sys.stderr)

                            # Clean up ERROR.log since validation passed
                            if error_log_file.exists():
                                error_log_file.unlink()
                                print(f"  Removed {error_log_file}", file=sys.stderr)

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
                            print(f"❌ lint-test still failing after fix attempt {fix_attempt}", file=sys.stderr)

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
                                print(f"\nERROR: Failed to fix lint/test errors after {max_fix_attempts} attempts.", file=sys.stderr)
                                print("Please review .agent_task/ERROR.log manually.", file=sys.stderr)
                                print("DONE.md is kept at project root for review.", file=sys.stderr)
                                print("Halting loop - linting & testing could not pass.", file=sys.stderr)
                                # NEVER delete DONE.md - keep it along with ERROR.log for manual review

                    # If we get here and retest_returncode == 0, we fixed it successfully
                    if retest_returncode == 0:
                        break  # Exit main loop - validation passed
                    else:
                        # FSM State: Still broken after max attempts - HALT (keep DONE.md)
                        # This prevents infinite loops and wasted API credits
                        print(f"\n⚠️  Halting loop after {max_fix_attempts} failed fix attempts.", file=sys.stderr)
                        print("Review DONE.md and .agent_task/ERROR.log to understand the issues.", file=sys.stderr)
                        break
                else:
                    # FSM State: Validation passed on first attempt → accept DONE.md and halt
                    print("✅ lint-test passed. Accepting DONE.md and halting early.", file=sys.stderr)
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
                print(f"Error during lint-test validation: {e}", file=sys.stderr)
                _print_red_banner("DONE.md VALIDATION FAILED DUE TO ERROR")
                print(f"Exception type: {type(e).__name__}", file=sys.stderr)
                print(f"Exception details: {e}", file=sys.stderr)
                print(file=sys.stderr)
                print("⚠️  DONE.md will be KEPT and loop will HALT to prevent infinite loop.", file=sys.stderr)
                print("Review the error above and fix the issue manually.", file=sys.stderr)
                print("Possible causes:", file=sys.stderr)
                print("  - Encoding errors in lint-test output (see BUG_CHARMAP.md)", file=sys.stderr)
                print("  - lint-test command not found or failed to execute", file=sys.stderr)
                print("  - Permission errors reading/writing files", file=sys.stderr)
                print(file=sys.stderr)
                print(f"DONE.md location: {done_file.absolute()}", file=sys.stderr)

                # Mark as completed (even though validation failed) to prevent re-running
                task_info.mark_completed(error=f"Validation failed with exception: {e}")
                task_info.save(info_file)

                # Create a marker file to indicate validation was attempted but failed
                validation_failed_marker = agent_task_dir / "done_validation_failed"
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

    print("\nAll iterations complete or halted early.", file=sys.stderr)

    # Mark completion if all iterations finish without DONE.md
    if not done_file.exists():
        task_info.mark_completed(error="Completed all iterations without DONE.md")
        task_info.save(info_file)

    # Open DONE.md if it exists
    if done_file.exists():
        print(f"Opening {done_file}...", file=sys.stderr)
        _open_file_in_editor(done_file)

    return 0

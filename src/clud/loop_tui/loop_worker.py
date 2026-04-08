"""Loop worker that executes loop iterations within the TUI context."""

import contextlib
from pathlib import Path
from typing import TYPE_CHECKING

from textual import work

from ..util import handle_keyboard_interrupt
from .app import CludLoopTUI

if TYPE_CHECKING:
    from ..agent_args import Args


class LoopWorkerApp(CludLoopTUI):
    """TUI app that executes loop iterations in a background worker."""

    def __init__(
        self,
        args: "Args",
        claude_path: str,
        loop_count: int,
        update_file: Path,
        start_iteration: int = 1,
    ) -> None:
        """Initialize loop worker app.

        Args:
            args: Command-line arguments
            claude_path: Path to Claude executable
            loop_count: Number of iterations to run
            update_file: Path to UPDATE.md file
            start_iteration: Iteration number to start from (1 for fresh)
        """
        # Track halt request
        self._halt_requested = False
        self._exit_code = 0
        self._start_iteration = start_iteration

        def on_exit() -> None:
            """Handle exit: kill active subprocess and halt worker."""
            self._halt_requested = True
            self._kill_active_subprocess()

        def on_halt() -> None:
            """Handle halt request from menu."""
            self._halt_requested = True
            self._kill_active_subprocess()
            self.log_message("> Loop halt requested by user")

        def on_edit() -> None:
            """Handle edit request from menu."""
            self.log_message("> UPDATE.md edit completed")

        # Initialize parent TUI with callbacks
        super().__init__(
            on_exit=on_exit,
            on_halt=on_halt,
            on_edit=on_edit,
            update_file=update_file,
        )

        # Store loop execution parameters
        self.args = args
        self.claude_path = claude_path
        self.loop_count = loop_count

    def on_mount(self) -> None:
        """Called when app is mounted - start the loop worker."""
        super().on_mount()
        self.log_message("Starting loop execution...")
        self.log_message("")

        # Start the loop execution in background worker
        self.run_loop_worker()

    def _kill_active_subprocess(self) -> None:
        """Kill the active Claude subprocess immediately for fast exit."""
        from running_process import kill_process_tree

        from ..agent.process_launcher import _active_process, _active_process_lock

        with _active_process_lock:
            proc = _active_process
        if proc is not None and proc.poll() is None:
            with contextlib.suppress(Exception):
                kill_process_tree(proc.pid)

    @work(exclusive=True, thread=True)
    def run_loop_worker(self) -> None:
        """Execute loop iterations in a background thread.

        This worker runs the standard _run_loop() function in a separate thread,
        capturing its output and sending it to the TUI via call_from_thread().
        """
        # Import here to avoid circular imports
        import shutil
        import time
        import uuid
        from pathlib import Path

        from ..agent.command_builder import (
            _build_claude_command,
            _get_effective_backend,
            _get_model_from_args,
            _inject_completion_prompt,
            _wrap_command_for_git_bash,
        )
        from ..agent.lint_runner import _find_and_run_lint_test
        from ..agent.loop_executor import (
            _cleanup_on_interrupt,
            _ensure_loop_in_gitignore,
            _generate_done_summary,
            _is_interrupt_exit_code,
        )
        from ..agent.loop_logger import LoopLogger
        from ..agent.motivation import write_motivation_file
        from ..agent.process_launcher import run_claude_process
        from ..agent.task_info import TaskInfo
        from ..agent.user_input import _open_file_in_editor
        from ..json_formatter import StreamJsonFormatter

        # Replicate _run_loop logic but with TUI output
        loop_dir = Path(".loop")

        # _handle_existing_loop was already called before the TUI started
        # (in integration.py) so we use the pre-resolved start_iteration.
        start_iteration = self._start_iteration

        # Create .loop directory if needed
        loop_dir.mkdir(exist_ok=True)

        # Ensure .loop in gitignore
        _ensure_loop_in_gitignore()

        # Write motivation file
        write_motivation_file(str(loop_dir))

        # Handle loop file (LOOP.md or custom file)
        loop_file_path: Path | None = None
        working_loop_file: Path | None = None

        if self.args.loop_value:
            try:
                int(self.args.loop_value)
            except ValueError:
                potential_file = Path(self.args.loop_value)
                if potential_file.exists() and potential_file.is_file():
                    loop_file_path = potential_file
                    working_loop_file = loop_dir / loop_file_path.name
                    if not working_loop_file.exists():
                        try:
                            shutil.copy2(loop_file_path, working_loop_file)
                        except Exception as e:
                            self._exit_code = 1
                            self.call_from_thread(
                                self.log_message,
                                f"Error: Failed to create working copy: {e}",
                            )
                            self.call_from_thread(lambda: self._exit_with_code(self._exit_code))
                            return
                else:
                    working_loop_file = loop_dir / "LOOP.md"
                    if not working_loop_file.exists():
                        try:
                            working_loop_file.write_text(self.args.loop_value, encoding="utf-8")
                        except Exception as e:
                            self._exit_code = 1
                            self.call_from_thread(self.log_message, f"Error: Failed to write LOOP.md: {e}")
                            self.call_from_thread(lambda: self._exit_with_code(self._exit_code))
                            return

        # Set up logging to .loop/log.txt
        log_file = loop_dir / "log.txt"
        done_file = Path("DONE.md")
        info_file = loop_dir / "info.json"

        user_prompt = self.args.prompt if self.args.prompt else self.args.message
        task_info = TaskInfo.load(info_file)

        if task_info is None:
            task_info = TaskInfo(
                session_id=str(uuid.uuid4()),
                start_time=time.time(),
                prompt=user_prompt,
                total_iterations=self.loop_count,
            )
            task_info.save(info_file)
        else:
            task_info.total_iterations = self.loop_count
            task_info.save(info_file)

        # Print loop banner (to TUI)
        self.call_from_thread(self.log_message, "Loop mode initialized")
        self.call_from_thread(self.log_message, f"Iterations: {self.loop_count}")
        self.call_from_thread(self.log_message, f"Loop directory: {loop_dir.absolute()}")
        self.call_from_thread(self.log_message, "")

        # Execute loop iterations
        try:
            with LoopLogger(log_file) as logger:
                for i in range(start_iteration - 1, self.loop_count):
                    # Check for halt request
                    if self._halt_requested:
                        self.call_from_thread(self.log_message, "")
                        self.call_from_thread(self.log_message, "⚠️  Loop halted by user")
                        break

                    iteration_num = i + 1
                    self.call_from_thread(
                        self.log_message,
                        f"\n--- Iteration {iteration_num}/{self.loop_count} ---",
                    )
                    logger.print_stderr(f"\n--- Iteration {iteration_num}/{self.loop_count} ---")

                    # Check for done_validated marker
                    done_validated_marker = loop_dir / "done_validated"
                    if done_validated_marker.exists():
                        self.call_from_thread(
                            self.log_message,
                            "✅ DONE.md was already validated. Halting immediately.",
                        )
                        logger.print_stderr("✅ DONE.md was already validated. Halting immediately.")

                        # Generate summary
                        self.call_from_thread(self.log_message, "\n📝 Generating summary...")
                        summary = _generate_done_summary(self.claude_path, self.args)
                        if summary:
                            self.call_from_thread(self.log_message, "\n" + "=" * 80)
                            self.call_from_thread(self.log_message, "SUMMARY:")
                            self.call_from_thread(self.log_message, summary)
                            self.call_from_thread(self.log_message, "=" * 80 + "\n")

                        self.call_from_thread(self.log_message, f"Opening {done_file}...")
                        _open_file_in_editor(done_file)
                        self._exit_code = 0
                        self.call_from_thread(lambda: self._exit_with_code(self._exit_code))
                        return

                    # Mark iteration start
                    task_info.start_iteration(iteration_num)
                    task_info.save(info_file)

                    # Build prompt with injection
                    working_file_str = str(working_loop_file) if working_loop_file else None
                    if user_prompt:
                        full_prompt = _inject_completion_prompt(
                            user_prompt,
                            iteration=iteration_num,
                            total_iterations=self.loop_count,
                            working_file=working_file_str,
                        )
                        self.call_from_thread(self.log_message, f"Prompt: {full_prompt}")
                        self.call_from_thread(self.log_message, "")
                        logger.print_stderr(f"Prompt: {full_prompt}")
                        logger.print_stderr()

                    # Build command
                    cmd = _build_claude_command(
                        self.args,
                        self.claude_path,
                        inject_prompt=True,
                        iteration=iteration_num,
                        total_iterations=self.loop_count,
                        working_file=working_file_str,
                    )
                    backend = _get_effective_backend(self.args)
                    if backend == "claude":
                        cmd = _wrap_command_for_git_bash(cmd)

                    # Print model info
                    model_flag = _get_model_from_args(self.args.claude_args, backend=backend)
                    if model_flag:
                        self.call_from_thread(self.log_message, f"Model: {model_flag}")

                    # Execute command with streaming output to TUI
                    try:
                        if self.args.prompt:
                            if self.args.plain or backend == "codex":
                                returncode = run_claude_process(cmd)
                            else:
                                # Create formatter and callback for TUI
                                stream_formatter = StreamJsonFormatter(
                                    show_system=self.args.verbose,
                                    show_usage=True,
                                    show_cache=self.args.verbose,
                                    verbose=self.args.verbose,
                                )

                                # Create callback that sends to both TUI and logger
                                def tui_callback(line: str, fmt: StreamJsonFormatter = stream_formatter) -> None:
                                    formatted = fmt.format_line(line)
                                    if formatted:
                                        # Send to TUI (thread-safe)
                                        self.call_from_thread(self.log_message, formatted.rstrip())
                                        # Send to log file
                                        logger.write_stdout(formatted)

                                returncode = run_claude_process(cmd, stdout_callback=tui_callback)
                        else:
                            returncode = run_claude_process(cmd)

                        # Check for interrupt
                        if _is_interrupt_exit_code(returncode):
                            _cleanup_on_interrupt(logger, task_info, info_file, iteration_num, returncode)
                            break

                    except KeyboardInterrupt as e:
                        _cleanup_on_interrupt(logger, task_info, info_file, iteration_num)
                        handle_keyboard_interrupt(e)
                        break  # Worker thread: suppressed

                    # Mark iteration end
                    error_msg = f"Exit code: {returncode}" if returncode != 0 else None
                    task_info.end_iteration(returncode, error_msg)
                    task_info.save(info_file)

                    if returncode != 0 and self.args.verbose:
                        logger.print_stderr(f"Warning: Iteration {iteration_num} exited with code {returncode}")

                    # Check for DONE.md and handle validation
                    if done_file.exists():
                        self.call_from_thread(
                            self.log_message,
                            f"\n📋 DONE.md detected after iteration {iteration_num}.",
                        )
                        self.call_from_thread(self.log_message, "Validating with `lint-test`...")
                        logger.print_stderr(f"\n📋 DONE.md detected at project root after iteration {iteration_num}.")
                        logger.print_stderr("Validating with `lint-test`...")

                        try:
                            lint_test_returncode, lint_test_output = _find_and_run_lint_test()
                            logger.print_stdout(lint_test_output)
                            self.call_from_thread(self.log_message, lint_test_output)

                            if lint_test_returncode == 0:
                                # Validation passed
                                self.call_from_thread(self.log_message, "✅ lint-test passed. Accepting DONE.md.")
                                logger.print_stderr("✅ lint-test passed. Accepting DONE.md and halting early.")
                                task_info.mark_completed()
                                task_info.save(info_file)
                                done_validated_marker.write_text(
                                    f"DONE.md validated successfully on {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                                    f"Iteration: {iteration_num}/{self.loop_count}\n"
                                    f"Validated on first attempt (no fixes needed)\n",
                                    encoding="utf-8",
                                )
                                break
                            else:
                                # Validation failed - show error and continue with standard fix loop
                                self.call_from_thread(
                                    self.log_message,
                                    "❌ lint-test failed. This would trigger fix loop...",
                                )
                                self.call_from_thread(
                                    self.log_message,
                                    "Note: Full fix loop logic not yet implemented in TUI mode.",
                                )
                                # For now, just halt
                                break

                        except KeyboardInterrupt as e:
                            handle_keyboard_interrupt(e)
                            break  # Worker thread: suppressed
                        except Exception as e:
                            self.call_from_thread(self.log_message, f"Error during validation: {e}")
                            break

                self.call_from_thread(self.log_message, "")
                self.call_from_thread(self.log_message, "All iterations complete or halted.")
                logger.print_stderr("\nAll iterations complete or halted early.")

                # Mark completion if no DONE.md
                if not done_file.exists():
                    task_info.mark_completed(error="Completed all iterations without DONE.md")
                    task_info.save(info_file)

                # Open DONE.md if it exists
                if done_file.exists():
                    self.call_from_thread(self.log_message, "\n📝 Generating summary...")
                    summary = _generate_done_summary(self.claude_path, self.args)
                    if summary:
                        self.call_from_thread(self.log_message, "\n" + "=" * 80)
                        self.call_from_thread(self.log_message, "SUMMARY:")
                        self.call_from_thread(self.log_message, summary)
                        self.call_from_thread(self.log_message, "=" * 80 + "\n")

                    self.call_from_thread(self.log_message, f"Opening {done_file}...")
                    _open_file_in_editor(done_file)

        except Exception as e:
            self._exit_code = 1
            self.call_from_thread(self.log_message, f"Loop execution error: {e}")

        # Exit the TUI when loop completes
        self.call_from_thread(lambda: self._exit_with_code(self._exit_code))

    def _exit_with_code(self, return_code: int) -> None:
        """Exit the app with the specified exit code.

        Args:
            return_code: Exit code to use
        """
        self._exit_code = return_code
        self.exit()

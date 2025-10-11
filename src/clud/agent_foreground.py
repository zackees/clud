import contextlib
import os
import platform
import shutil
import subprocess
import sys
import traceback
from pathlib import Path
from typing import Any

from .agent_completion import detect_agent_completion
from .agent_foreground_args import Args, parse_args
from .json_formatter import StreamJsonFormatter, create_formatter_callback
from .output_filter import OutputFilter
from .running_process import RunningProcess
from .secrets import get_credential_store
from .streaming_parser import StreamingParser
from .streaming_ui import StreamingUI

# Get credential store once at module level
keyring = get_credential_store()


# Exception classes
class CludError(Exception):
    """Base exception for clud errors."""

    pass


class ValidationError(CludError):
    """User/validation error."""

    pass


class ConfigError(CludError):
    """Configuration error."""

    pass


# API key management functions
def validate_api_key(api_key: str | None) -> bool:
    """Validate API key format."""
    if not api_key:
        return False

    # Clean the API key
    api_key = api_key.strip()

    # Remove any BOM characters that might be present
    if api_key.startswith("\ufeff"):
        api_key = api_key[1:]

    # Basic validation: should start with sk-ant- and have reasonable length
    if not api_key.startswith("sk-ant-"):
        return False

    # Should be at least 20 characters (conservative minimum)
    return len(api_key) >= 20


def get_api_key_from_keyring(keyring_name: str) -> str | None:
    """Get API key from OS keyring or fallback credential store."""
    if keyring is None:
        raise ConfigError("No credential storage available. Install with: pip install keyring, keyrings.cryptfile, or cryptography")

    try:
        api_key = keyring.get_password("clud", keyring_name)
        if not api_key:
            raise ConfigError(f"No API key found in credential store for '{keyring_name}'")
        return api_key
    except Exception as e:
        raise ConfigError(f"Failed to retrieve API key from credential store: {e}") from e


def get_clud_config_dir() -> Path:
    """Get or create the .clud config directory."""
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir


def save_api_key_to_config(api_key: str, key_name: str = "anthropic-api-key") -> None:
    """Save API key to .clud config directory."""
    try:
        config_dir = get_clud_config_dir()
        key_file = config_dir / f"{key_name}.key"

        # Write API key to file with restrictive permissions
        # Ensure no trailing newlines or spaces
        key_file.write_text(api_key.strip(), encoding="utf-8")

        # Set restrictive permissions (owner read/write only)
        if platform.system() != "Windows":
            key_file.chmod(0o600)
        else:
            # On Windows, try to set file as hidden
            try:
                import ctypes

                FILE_ATTRIBUTE_HIDDEN = 0x02
                ctypes.windll.kernel32.SetFileAttributesW(str(key_file), FILE_ATTRIBUTE_HIDDEN)
            except Exception:
                pass  # Not critical if hiding fails

    except Exception as e:
        raise ConfigError(f"Failed to save API key to config: {e}") from e


def load_api_key_from_config(key_name: str = "anthropic-api-key") -> str | None:
    """Load API key from .clud config directory."""
    try:
        config_dir = get_clud_config_dir()
        key_file = config_dir / f"{key_name}.key"

        if key_file.exists():
            # Read and thoroughly clean the API key
            api_key = key_file.read_text(encoding="utf-8").strip()
            # Remove any BOM characters that might be present on Windows
            if api_key.startswith("\ufeff"):
                api_key = api_key[1:]
            return api_key if api_key else None
        return None

    except Exception as e:
        # Log the error for debugging but don't crash
        print(f"Warning: Could not load API key from config: {e}", file=sys.stderr)
        return None


def handle_login() -> int:
    """Handle the --login command to configure API key."""
    print("Configure Claude API Key")
    print("-" * 40)

    # Check if we already have a saved key
    existing_key = load_api_key_from_config()
    if existing_key:
        print("An API key is already configured.")
        sys.stdout.flush()
        overwrite = input("Do you want to replace it? (y/N): ").strip().lower()
        if overwrite not in ["y", "yes"]:
            print("Keeping existing API key.")
            return 0

    # Prompt for new key
    while True:
        try:
            sys.stdout.flush()
            api_key = input("Please enter your Anthropic API key: ").strip()
            if not api_key:
                print("API key cannot be empty. Please try again.")
                continue

            # Clean the API key
            if api_key.startswith("\ufeff"):
                api_key = api_key[1:]

            if not validate_api_key(api_key):
                print("Invalid API key format. API keys should start with 'sk-ant-' and be at least 20 characters.")
                continue

            # Save the key
            try:
                save_api_key_to_config(api_key)
                print("\n✓ API key saved successfully to ~/.clud/anthropic-api-key.key")
                print("You can now use 'clud' to launch Claude-powered development containers.")
                return 0
            except ConfigError as e:
                print(f"\nError: Could not save API key: {e}", file=sys.stderr)
                return 1

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            return 2


def prompt_for_api_key() -> str:
    """Interactively prompt user for API key."""
    print("No Claude API key found.")

    while True:
        try:
            # Flush output to ensure prompt is displayed before input
            sys.stdout.flush()
            api_key = input("Please enter your Anthropic API key: ").strip()
            if not api_key:
                print("API key cannot be empty. Please try again.")
                continue

            if not validate_api_key(api_key):
                print("Invalid API key format. API keys should start with 'sk-ant-' and be at least 20 characters.")
                continue

            # Ask if user wants to save to config
            sys.stdout.flush()
            save_choice = input("Save this key to ~/.clud/ for future use? (y/N): ").strip().lower()
            if save_choice in ["y", "yes"]:
                try:
                    save_api_key_to_config(api_key)
                    print("API key saved to ~/.clud/anthropic-api-key.key")
                except ConfigError as e:
                    print(f"Warning: Could not save API key: {e}")

            return api_key

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def get_api_key(args: Any) -> str:
    """Get API key following priority order: --api-key, --api-key-from, env var, saved config, prompt."""
    api_key = None

    # Priority 0: --api-key command line argument
    if hasattr(args, "api_key") and args.api_key:
        api_key = args.api_key.strip()

    # Priority 1: --api-key-from keyring entry (if keyring is available)
    if not api_key and hasattr(args, "api_key_from") and args.api_key_from:
        with contextlib.suppress(ConfigError):
            api_key = get_api_key_from_keyring(args.api_key_from) if keyring is not None else load_api_key_from_config(args.api_key_from)

    # Priority 2: Environment variable
    if not api_key:
        env_key = os.environ.get("ANTHROPIC_API_KEY")
        if env_key:
            api_key = env_key.strip()

    # Priority 3: Saved config file
    if not api_key:
        api_key = load_api_key_from_config()

    # Priority 4: Interactive prompt
    if not api_key:
        api_key = prompt_for_api_key()

    # Clean the API key before validation
    if api_key:
        api_key = api_key.strip()
        # Remove any BOM characters
        if api_key.startswith("\ufeff"):
            api_key = api_key[1:]

    # Validate the final API key
    if not validate_api_key(api_key):
        raise ValidationError("Invalid API key format")

    # Type checker note: validate_api_key ensures api_key is not None
    assert api_key is not None
    return api_key


def _find_claude_path() -> str | None:
    """Find the path to the Claude executable."""
    # Try to find claude in PATH, prioritizing Windows executables
    if platform.system() == "Windows":
        # On Windows, prefer .cmd and .exe extensions
        claude_path = shutil.which("claude.cmd") or shutil.which("claude.exe")
        if claude_path:
            return claude_path

    # Fall back to generic "claude" (for Unix or git bash on Windows)
    claude_path = shutil.which("claude")
    if claude_path:
        return claude_path

    # Check common Windows npm global locations
    if platform.system() == "Windows":
        possible_paths = [
            os.path.expanduser("~/AppData/Roaming/npm/claude.cmd"),
            os.path.expanduser("~/AppData/Roaming/npm/claude.exe"),
            "C:/Users/" + os.environ.get("USERNAME", "") + "/AppData/Roaming/npm/claude.cmd",
        ]
        for path in possible_paths:
            if os.path.exists(path):
                return path

    return None


def _inject_completion_prompt(message: str) -> str:
    """Inject the DONE.md completion prompt into the user's message."""
    injection = " If you see that the task is 100 percent complete, then write out DONE.md and halt"
    return message + injection


def _build_claude_command(args: Args, claude_path: str, inject_prompt: bool = False) -> list[str]:
    """Build the Claude command with all arguments."""
    cmd = [claude_path, "--dangerously-skip-permissions"]

    if args.continue_flag:
        cmd.append("--continue")

    if args.prompt:
        prompt_text = args.prompt
        if inject_prompt:
            prompt_text = _inject_completion_prompt(prompt_text)
        cmd.extend(["-p", prompt_text])
        # Enable streaming JSON output for -p flag by default
        # Note: stream-json requires --verbose when used with --print/-p
        cmd.extend(["--output-format", "stream-json", "--verbose"])

    if args.message:
        message_text = args.message
        if inject_prompt:
            message_text = _inject_completion_prompt(message_text)

        # If idle timeout is set, force non-interactive mode with -p
        # to avoid TUI redraws being detected as activity
        if args.idle_timeout is not None:
            cmd.extend(["-p", message_text])
        else:
            cmd.append(message_text)

    cmd.extend(args.claude_args)

    return cmd


def _print_debug_info(claude_path: str | None, cmd: list[str], verbose: bool = False) -> None:
    """Print debug information about Claude execution."""
    if not verbose:
        return

    if claude_path:
        print(f"DEBUG: Found claude at: {claude_path}", file=sys.stderr)
        print(f"DEBUG: Platform: {platform.system()}", file=sys.stderr)
        print(f"DEBUG: File exists: {os.path.exists(claude_path)}", file=sys.stderr)

    if cmd:
        print(f"DEBUG: Executing command: {cmd}", file=sys.stderr)


def _print_error_diagnostics(claude_path: str | None, cmd: list[str]) -> None:
    """Print comprehensive error diagnostics."""
    print(f"DEBUG: Current working directory: {os.getcwd()}", file=sys.stderr)
    print(f"DEBUG: Command attempted: {subprocess.list2cmdline(cmd) if cmd else 'command not yet built'}", file=sys.stderr)
    print(f"DEBUG: Claude path used: {claude_path if claude_path else 'path not yet determined'}", file=sys.stderr)
    print("DEBUG: Claude search results:", file=sys.stderr)

    if platform.system() == "Windows":
        print(f"  - shutil.which('claude.cmd'): {shutil.which('claude.cmd')}", file=sys.stderr)
        print(f"  - shutil.which('claude.exe'): {shutil.which('claude.exe')}", file=sys.stderr)

    print(f"  - shutil.which('claude'): {shutil.which('claude')}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.cmd exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.cmd'))}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.exe exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.exe'))}", file=sys.stderr)


def _execute_command(cmd: list[str], use_shell: bool = False, verbose: bool = False) -> int:
    """Execute a command and return its exit code."""
    if use_shell:
        # Convert command list to shell string and execute through shell
        cmd_str = subprocess.list2cmdline(cmd)
        if verbose:
            print(f"DEBUG: Retrying with shell=True: {cmd_str}", file=sys.stderr)
        result = subprocess.run(cmd_str, shell=True)
    else:
        result = subprocess.run(cmd)

    return result.returncode


def _prompt_for_loop_count() -> int:
    """Prompt user for loop count."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Loop count: ").strip()
            if not response:
                print("Loop count cannot be empty. Please enter a positive number.")
                continue

            count = int(response)
            if count <= 0:
                print("Loop count must be greater than 0.")
                continue

            return count

        except ValueError:
            print("Invalid input. Please enter a valid number.")
            continue
        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _prompt_for_message() -> str:
    """Prompt user for agent message/prompt."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Prompt for agent: ").strip()
            if not response:
                print("Prompt cannot be empty. Please try again.")
                continue

            return response

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _open_file_in_editor(file_path: Path) -> None:
    """Open a file in the default text editor (cross-platform)."""
    try:
        system = platform.system()
        if system == "Darwin":  # macOS
            subprocess.run(["open", str(file_path)], check=False)
        elif system == "Windows":
            # Try sublime first, then fall back to default
            sublime_paths = [
                "C:\\Program Files\\Sublime Text\\sublime_text.exe",
                "C:\\Program Files\\Sublime Text 3\\sublime_text.exe",
                os.path.expanduser("~\\AppData\\Local\\Programs\\Sublime Text\\sublime_text.exe"),
            ]
            sublime_found = False
            for sublime_path in sublime_paths:
                if os.path.exists(sublime_path):
                    subprocess.run([sublime_path, str(file_path)], check=False)
                    sublime_found = True
                    break

            if not sublime_found:
                # Fall back to default Windows opener
                os.startfile(str(file_path))  # type: ignore
        else:  # Linux and other Unix-like systems
            # Try common editors in order
            editors = ["sublime_text", "subl", "xdg-open"]
            for editor in editors:
                if shutil.which(editor):
                    subprocess.run([editor, str(file_path)], check=False)
                    break
    except Exception as e:
        print(f"Warning: Could not open {file_path}: {e}", file=sys.stderr)


def _create_streaming_json_callback() -> tuple[Any, Any]:
    """Create a callback function for parsing streaming JSON output.

    Returns:
        Tuple of (stdout_callback, stderr_callback) for RunningProcess
    """
    parser = StreamingParser()
    ui = StreamingUI(use_colors=sys.stdout.isatty())

    def stdout_callback(line: str) -> None:
        """Parse and render JSON events from stdout."""
        line = line.rstrip()
        if not line:
            return

        # Try to parse as JSON event
        event = parser.parse_line(line)
        if event:
            # Render the event using the UI
            ui.render_event(event)
        else:
            # Not a JSON event, print as-is
            print(line, flush=True)

    def stderr_callback(line: str) -> None:
        """Pass through stderr without parsing."""
        print(line.rstrip(), file=sys.stderr, flush=True)

    return stdout_callback, stderr_callback


def _run_loop(args: Args, claude_path: str, loop_count: int) -> int:
    """Run Claude in a loop, checking for DONE.md after each iteration."""
    done_file = Path("DONE.md")

    for i in range(loop_count):
        print(f"\n--- Run {i + 1}/{loop_count} ---", file=sys.stderr)

        # Build command with prompt injection
        cmd = _build_claude_command(args, claude_path, inject_prompt=True)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Execute the command with streaming if prompt is present
        if args.prompt:
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

        if returncode != 0 and args.verbose:
            print(f"Warning: Run {i + 1} exited with code {returncode}", file=sys.stderr)

        # Check if DONE.md was created
        if done_file.exists():
            print(f"\n✅ DONE.md detected after run {i + 1} — halting early.", file=sys.stderr)
            break

    print("\nAll runs complete or halted early.", file=sys.stderr)

    # Open DONE.md if it exists
    if done_file.exists():
        print(f"Opening {done_file}...", file=sys.stderr)
        _open_file_in_editor(done_file)

    return 0


def run(args: Args) -> int:
    """
    Launch Claude Code with dangerous mode (--dangerously-skip-permissions).
    This bypasses all permission prompts for a more streamlined workflow.

    WARNING: This mode removes all safety guardrails. Use with caution.
    """
    # Initialize variables for exception handler access
    claude_path: str | None = None
    cmd: list[str] = []

    try:
        # If --cmd is provided, execute the command directly instead of launching Claude
        if args.cmd:
            result = subprocess.run(args.cmd, shell=True)
            return result.returncode

        # Handle dry-run mode
        if args.dry_run:
            cmd_parts = ["claude", "--dangerously-skip-permissions"]
            if args.continue_flag:
                cmd_parts.append("--continue")
            if args.prompt:
                cmd_parts.extend(["-p", args.prompt])
                # Enable streaming JSON output for -p flag by default
                # Note: stream-json requires --verbose when used with --print/-p
                cmd_parts.extend(["--output-format", "stream-json", "--verbose"])
            if args.message:
                cmd_parts.append(args.message)
            cmd_parts.extend(args.claude_args)
            print("Would execute:", " ".join(cmd_parts))
            return 0

        # Find Claude executable
        claude_path = _find_claude_path()
        if not claude_path:
            print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
            print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
            return 1

        # Handle loop mode - parse loop_value flexibly
        if args.loop_value is not None:
            loop_count = None
            loop_message = None

            # Try to parse loop_value
            if args.loop_value == "":
                # --loop with no value: prompt for both
                pass
            else:
                # Try parsing as integer first
                try:
                    loop_count = int(args.loop_value)
                    if loop_count <= 0:
                        print("Error: --loop count must be greater than 0", file=sys.stderr)
                        return 1
                except ValueError:
                    # Not an integer, treat as message
                    loop_message = args.loop_value

            # Prompt for missing values
            # Check if we have a message from loop_value, -m, or -p
            if not args.prompt and not args.message and not loop_message:
                loop_message = _prompt_for_message()

            if loop_count is None:
                loop_count = _prompt_for_loop_count()

            # Set the prompt if we got it from loop_value (uses -p instead of -m)
            if loop_message and not args.message and not args.prompt:
                args.prompt = loop_message

            return _run_loop(args, claude_path, loop_count)

        # Build command
        cmd = _build_claude_command(args, claude_path)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Execute Claude with the dangerous permissions flag
        # Use idle detection if timeout is specified
        if args.idle_timeout is not None:
            # Create output filter to suppress terminal capability responses
            output_filter = OutputFilter()

            # Output callback to print data to stdout (with filtering)
            def output_callback(data: str) -> None:
                # Filter out terminal capability responses to prevent corrupting parent terminal
                filtered_data = output_filter.filter_terminal_responses(data)
                if filtered_data:
                    sys.stdout.write(filtered_data)
                    sys.stdout.flush()

            detect_agent_completion(cmd, args.idle_timeout, output_callback)
            return 0
        elif args.prompt:
            # Use RunningProcess for streaming output when using -p flag
            # This ensures stream-json output is displayed line-by-line in real-time
            # Create JSON formatter for beautiful output
            formatter = StreamJsonFormatter(
                show_system=args.verbose,
                show_usage=True,
                show_cache=args.verbose,
                verbose=args.verbose,
            )
            stdout_callback = create_formatter_callback(formatter)
            return RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
        else:
            return _execute_command(cmd, use_shell=False, verbose=args.verbose)

    except FileNotFoundError as e:
        print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
        print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
        print(f"DEBUG: FileNotFoundError details: {e}", file=sys.stderr)
        traceback.print_exc()
        return 1

    except KeyboardInterrupt:
        print("\nInterrupted by user", file=sys.stderr)
        return 130

    except OSError as e:
        print(f"Error launching Claude: {e}", file=sys.stderr)
        print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)

        # Try backup method with shell=True
        if cmd and claude_path:
            try:
                print("\nAttempting backup method (shell=True)...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()

        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()
        return 1

    except Exception as e:
        print(f"Error launching Claude: {e}", file=sys.stderr)
        print(f"DEBUG: Exception type: {type(e).__name__}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)

        # Try backup method with shell=True
        if cmd and claude_path:
            try:
                print("\nAttempting backup method (shell=True)...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()

        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()
        return 1


def main(args: list[str] | None = None) -> int:
    """
    Launch Claude Code with dangerous mode (--dangerously-skip-permissions).
    This bypasses all permission prompts for a more streamlined workflow.

    WARNING: This mode removes all safety guardrails. Use with caution.
    """
    parsed_args = parse_args(args)
    return run(parsed_args)


if __name__ == "__main__":
    sys.exit(main())

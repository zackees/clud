"""Chat agent for processing messages with Claude Code."""

import contextlib
import os
import subprocess
import sys
import tempfile


def process_chat_message(message: str, chat_id: str, cwd: str) -> str:
    """Process a chat message using Claude Code agent.

    Args:
        message: User's message to process
        chat_id: Chat ID for context (for future multi-chat support)
        cwd: Current working directory

    Returns:
        Agent's response as a string
    """
    try:
        # Use a temporary file to capture Claude's response
        with tempfile.NamedTemporaryFile(mode="w+", suffix=".txt", delete=False, encoding="utf-8") as tmp_output:
            output_file = tmp_output.name

        try:
            # Build command to run Claude with the message
            cmd = [sys.executable, "-m", "clud", "-p", message]

            # Run Claude in the original working directory
            result = subprocess.run(
                cmd,
                cwd=cwd,
                capture_output=True,
                text=True,
                timeout=60,  # 60 second timeout
            )

            # Get output from stdout
            response = result.stdout.strip()

            if not response:
                # If stdout is empty, check stderr
                response = f"Error: {result.stderr.strip()}" if result.stderr else "I processed your request but have no output to show."

            # If response is too long, truncate it
            max_length = 4000
            if len(response) > max_length:
                response = response[:max_length] + "\n\n... (truncated)"

            return response

        except subprocess.TimeoutExpired:
            return "The request timed out. Please try a simpler question or check the logs."
        except Exception as e:
            return f"Error processing message: {str(e)}"
        finally:
            # Clean up temp file
            if os.path.exists(output_file):
                with contextlib.suppress(Exception):
                    os.remove(output_file)

    except Exception as e:
        return f"Failed to create temporary file: {str(e)}"

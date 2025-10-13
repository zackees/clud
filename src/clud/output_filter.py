"""Content-aware output filtering for idle detection.

This module provides filtering logic to distinguish between meaningful Claude activity
and TUI noise (escape codes, cursor movements, keep-alive newlines, etc.).
"""

import re
from re import Pattern


class OutputFilter:
    """Filter Claude output to detect meaningful activity vs. TUI noise."""

    # ANSI escape code patterns
    ANSI_ESCAPE: Pattern[str] = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")
    CURSOR_MOVEMENT: Pattern[str] = re.compile(r"\x1b\[[\d;]*[ABCDEFGHJKST]")
    CURSOR_POSITION: Pattern[str] = re.compile(r"\x1b\[\d+;\d+[Hf]")
    CLEAR_SCREEN: Pattern[str] = re.compile(r"\x1b\[[\d;]*[JK]")
    COLOR_CODES: Pattern[str] = re.compile(r"\x1b\[\d+(;\d+)*m")
    SAVE_RESTORE_CURSOR: Pattern[str] = re.compile(r"\x1b[\[\(]?[78su]")

    # Terminal capability query responses (must be suppressed from stdout)
    # DA2 (Device Attributes Secondary) responses: CSI > Pp ; Pv ; Pc c
    DA2_RESPONSE: Pattern[str] = re.compile(r"\x1b\[>[\d;]*c")
    # DCS (Device Control String) sequences including XTGETTCAP responses: DCS ... ST
    # ST can be ESC \ or just the C1 control character
    DCS_SEQUENCE: Pattern[str] = re.compile(r"\x1bP[^\x1b]*(?:\x1b\\|\x9c)", re.DOTALL)
    # CPR (Cursor Position Report) responses: CSI Pl ; Pc R
    CPR_RESPONSE: Pattern[str] = re.compile(r"\x1b\[\d+;\d+R")
    # DSR (Device Status Report) responses
    DSR_RESPONSE: Pattern[str] = re.compile(r"\x1b\[\d*n")

    # Claude-specific activity indicators (these indicate real work)
    TOOL_INVOCATION: Pattern[str] = re.compile(r"(<function_calls>|<invoke|</invoke>|Bash\(|Read\(|Write\(|Edit\()", re.IGNORECASE)
    THINKING_BLOCK: Pattern[str] = re.compile(r"<thinking>|</thinking>", re.IGNORECASE)
    ASSISTANT_MESSAGE: Pattern[str] = re.compile(r"(^|\n)assistant:|responding", re.IGNORECASE)

    # Progress indicators (still noise, but different from cursor movement)
    PROGRESS_INDICATOR: Pattern[str] = re.compile(r"\.\.\.|━|▓|█|Waiting|Loading", re.IGNORECASE)

    def __init__(self, min_text_length: int = 5) -> None:
        """Initialize the output filter.

        Args:
            min_text_length: Minimum length of cleaned text to consider meaningful
        """
        self.min_text_length = min_text_length

    def is_meaningful(self, data: str) -> bool:
        """Determine if output data represents meaningful activity.

        Args:
            data: Raw output data to analyze

        Returns:
            True if the output indicates meaningful Claude activity, False for TUI noise
        """
        if not data:
            return False

        # Check for Claude activity indicators first (these are always meaningful)
        if self.TOOL_INVOCATION.search(data):
            return True
        if self.THINKING_BLOCK.search(data):
            return True
        if self.ASSISTANT_MESSAGE.search(data):
            return True

        # Strip all ANSI escape codes to get actual text content
        cleaned = self.ANSI_ESCAPE.sub("", data)
        cleaned = cleaned.strip()

        # Empty or whitespace-only (including newlines) is not meaningful
        if not cleaned or cleaned.replace("\n", "").replace("\r", "").strip() == "":
            return False

        # Check if it's just newlines
        if all(c in "\n\r\t " for c in cleaned):
            return False

        # Check if it's just progress indicators
        without_progress = self.PROGRESS_INDICATOR.sub("", cleaned).strip()
        if not without_progress:
            return False

        # If we have substantive text after cleaning, it's meaningful
        if len(cleaned) >= self.min_text_length:
            return True

        # Short text might be meaningful if it contains alphanumeric characters
        return bool(any(c.isalnum() for c in cleaned))

    def should_suppress(self, data: str) -> bool:
        """Determine if output should be suppressed from stdout.

        Terminal capability query responses should not be written to stdout
        as they will corrupt the parent terminal's state.

        Args:
            data: Raw output data to analyze

        Returns:
            True if the output should be suppressed (not written to stdout)
        """
        if not data:
            return False

        # Check for terminal capability query responses
        return bool(self.DA2_RESPONSE.search(data) or self.DCS_SEQUENCE.search(data) or self.CPR_RESPONSE.search(data) or self.DSR_RESPONSE.search(data))

    def filter_terminal_responses(self, data: str) -> str:
        """Remove terminal capability responses from data.

        Args:
            data: Raw output data

        Returns:
            Data with terminal responses removed
        """
        if not data:
            return data

        # Remove each type of terminal response
        result = self.DA2_RESPONSE.sub("", data)
        result = self.DCS_SEQUENCE.sub("", result)
        result = self.CPR_RESPONSE.sub("", result)
        result = self.DSR_RESPONSE.sub("", result)

        return result

    def strip_ansi(self, data: str) -> str:
        """Strip ANSI escape codes from data.

        Args:
            data: Raw output data

        Returns:
            Data with ANSI codes removed
        """
        return self.ANSI_ESCAPE.sub("", data)

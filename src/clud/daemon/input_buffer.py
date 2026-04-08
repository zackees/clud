"""Track best-effort terminal draft input from raw keystrokes."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(slots=True)
class InputSnapshot:
    """A point-in-time view of the tracked draft input."""

    draft: str
    reliable: bool


class TerminalInputTracker:
    """Track a simple line-oriented draft buffer from terminal input bytes.

    This is intentionally conservative. If we see escape/control sequences that
    imply cursor movement or in-terminal editing beyond simple append/backspace
    operations, the tracker marks itself unreliable and callers should avoid
    destructive draft rewriting.
    """

    def __init__(self) -> None:
        self._draft = ""
        self._reliable = True

    def observe(self, data: str) -> None:
        """Consume raw terminal input data."""
        index = 0
        while index < len(data):
            char = data[index]

            if char == "\x1b":
                self._reliable = False
                break
            if char in {"\r", "\n"}:
                self._draft = ""
                index += 1
                continue
            if char in {"\x08", "\x7f"}:
                self._draft = self._draft[:-1]
                index += 1
                continue
            if char == "\x15":
                self._draft = ""
                index += 1
                continue
            if char == "\x17":
                stripped = self._draft.rstrip()
                split_at = stripped.rfind(" ")
                self._draft = "" if split_at < 0 else stripped[: split_at + 1]
                index += 1
                continue
            if char == "\t":
                self._draft += "\t"
                index += 1
                continue
            if char.isprintable():
                self._draft += char
                index += 1
                continue

            self._reliable = False
            break

    def snapshot(self) -> InputSnapshot:
        """Return the current tracked draft state."""
        return InputSnapshot(draft=self._draft, reliable=self._reliable)

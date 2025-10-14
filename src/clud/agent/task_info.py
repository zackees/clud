"""Agent task information tracking with JSON persistence."""

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


@dataclass
class IterationInfo:
    """Information about a single iteration."""

    iteration: int
    start_time: float
    end_time: float | None = None
    exit_code: int | None = None
    duration_seconds: float | None = None
    error: str | None = None


@dataclass
class TaskInfo:
    """Information about an agent task session."""

    # Session metadata
    session_id: str
    start_time: float
    prompt: str | None = None
    total_iterations: int | None = None

    # State tracking
    current_iteration: int = 0
    completed: bool = False
    end_time: float | None = None

    # Iteration history
    iterations: list[IterationInfo] = field(default_factory=lambda: [])

    # Error tracking
    error: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "session_id": self.session_id,
            "start_time": self.start_time,
            "start_time_readable": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(self.start_time)),
            "prompt": self.prompt,
            "total_iterations": self.total_iterations,
            "current_iteration": self.current_iteration,
            "completed": self.completed,
            "end_time": self.end_time,
            "end_time_readable": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(self.end_time)) if self.end_time else None,
            "duration_seconds": self.end_time - self.start_time if self.end_time else None,
            "iterations": [
                {
                    "iteration": it.iteration,
                    "start_time": it.start_time,
                    "start_time_readable": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(it.start_time)),
                    "end_time": it.end_time,
                    "end_time_readable": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(it.end_time)) if it.end_time else None,
                    "duration_seconds": it.duration_seconds,
                    "exit_code": it.exit_code,
                    "error": it.error,
                }
                for it in self.iterations
            ],
            "error": self.error,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TaskInfo":
        """Load from dictionary."""
        iterations: list[IterationInfo] = [
            IterationInfo(
                iteration=it["iteration"],
                start_time=it["start_time"],
                end_time=it.get("end_time"),
                duration_seconds=it.get("duration_seconds"),
                exit_code=it.get("exit_code"),
                error=it.get("error"),
            )
            for it in data.get("iterations", [])
        ]

        return cls(
            session_id=data["session_id"],
            start_time=data["start_time"],
            prompt=data.get("prompt"),
            total_iterations=data.get("total_iterations"),
            current_iteration=data.get("current_iteration", 0),
            completed=data.get("completed", False),
            end_time=data.get("end_time"),
            iterations=iterations,
            error=data.get("error"),
        )

    def save(self, path: Path) -> None:
        """Save to JSON file."""
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8") as f:
            json.dump(self.to_dict(), f, indent=2)

    @classmethod
    def load(cls, path: Path) -> "TaskInfo | None":
        """Load from JSON file. Returns None if file doesn't exist."""
        if not path.exists():
            return None

        try:
            with path.open("r", encoding="utf-8") as f:
                data = json.load(f)
            return cls.from_dict(data)
        except (json.JSONDecodeError, KeyError):
            # Return None on parse errors - treat as missing file
            return None

    def start_iteration(self, iteration: int) -> None:
        """Mark the start of a new iteration."""
        self.current_iteration = iteration
        iter_info = IterationInfo(iteration=iteration, start_time=time.time())
        self.iterations.append(iter_info)

    def end_iteration(self, exit_code: int, error: str | None = None) -> None:
        """Mark the end of the current iteration."""
        if self.iterations:
            current = self.iterations[-1]
            current.end_time = time.time()
            current.exit_code = exit_code
            current.error = error
            if current.start_time and current.end_time:
                current.duration_seconds = current.end_time - current.start_time

    def mark_completed(self, error: str | None = None) -> None:
        """Mark the task as completed."""
        self.completed = True
        self.end_time = time.time()
        self.error = error

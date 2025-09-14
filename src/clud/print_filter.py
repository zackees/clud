"""Print filtering utilities for clud."""

from abc import ABC, abstractmethod
from typing import Any


class PrintFilter(ABC):
    """Abstract base class for print filters."""

    @abstractmethod
    def __call__(self, *args: Any, **kwargs: Any) -> None:
        """Filter and print the given arguments."""
        pass


class PrintFilterDefault(PrintFilter):
    """Default print filter that passes through to print."""

    def __call__(self, *args: Any, **kwargs: Any) -> None:
        """Print the arguments directly."""
        print(*args, **kwargs)

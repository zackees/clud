"""TUI for clud --loop mode."""

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path


@dataclass
class TUIConfig:
    """Configuration for loop TUI."""

    on_exit: Callable[[], None] | None = None
    on_halt: Callable[[], None] | None = None
    on_edit: Callable[[], None] | None = None
    update_file: Path | None = None


class LoopTUI:
    """Proxy class for loop TUI with lazy-loaded implementation."""

    @staticmethod
    def run(config: TUIConfig | None = None) -> None:
        """Run the loop TUI.

        Args:
            config: TUI configuration with callbacks
        """
        from clud.loop_tui.app import CludLoopTUI

        app = CludLoopTUI(
            on_exit=config.on_exit if config else None,
            on_halt=config.on_halt if config else None,
            on_edit=config.on_edit if config else None,
            update_file=config.update_file if config else None,
        )
        app.run()


__all__ = [
    "LoopTUI",
    "TUIConfig",
]

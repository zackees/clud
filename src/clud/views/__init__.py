"""Views module for different display contexts (terminal, webui, vscode, diff)."""

from .diff_tree import DiffTreeView
from .diff_view import DiffView

__all__ = ["DiffTreeView", "DiffView"]

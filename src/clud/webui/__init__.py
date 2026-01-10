"""Web UI module for Claude Code interaction via browser interface."""


class WebUI:
    """Proxy class for WebUI operations with lazy-loaded implementation."""

    @staticmethod
    def run_server(port: int | None = None) -> int:
        """Start FastAPI server for Web UI.

        Args:
            port: Server port. If None, auto-detect.

        Returns:
            Exit code (0 for success)
        """
        from clud.webui.server import run_server as _run_server

        return _run_server(port)


__all__ = ["WebUI"]

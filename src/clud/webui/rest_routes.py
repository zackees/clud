"""REST API route handlers for Claude Code Web UI."""

import asyncio
import json
import logging
import os
import urllib.request

from fastapi import FastAPI
from fastapi.responses import JSONResponse

from .api import BacklogHandler, DiffHandler, HistoryHandler, ProjectHandler
from .telegram_api import TelegramAPIHandler

logger = logging.getLogger(__name__)


def register_rest_routes(
    app: FastAPI,
    project_handler: ProjectHandler,
    history_handler: HistoryHandler,
    diff_handler: DiffHandler,
    backlog_handler: BacklogHandler,
    telegram_handler: TelegramAPIHandler,
) -> None:
    """Register all REST API routes.

    Args:
        app: FastAPI application instance
        project_handler: Handler for project operations
        history_handler: Handler for conversation history
        diff_handler: Handler for diff operations
        backlog_handler: Handler for backlog tasks
        telegram_handler: Handler for Telegram API operations
    """
    # ============================================================================
    # Project Routes
    # ============================================================================

    @app.get("/api/projects")
    async def get_projects(base_path: str | None = None) -> JSONResponse:
        """List available projects."""
        projects = project_handler.list_projects(base_path)
        return JSONResponse(content={"projects": projects})

    @app.get("/api/projects/validate")
    async def validate_project(path: str) -> JSONResponse:
        """Validate a project path."""
        is_valid = project_handler.validate_project_path(path)
        return JSONResponse(content={"valid": is_valid, "path": path})

    # ============================================================================
    # History Routes
    # ============================================================================

    @app.get("/api/history")
    async def get_history() -> JSONResponse:
        """Get conversation history."""
        history = history_handler.get_history()
        return JSONResponse(content={"history": history})

    @app.post("/api/history")
    async def add_message(data: dict[str, str]) -> JSONResponse:
        """Add a message to history."""
        role = data.get("role", "user")
        content = data.get("content", "")
        history_handler.add_message(role, content)
        return JSONResponse(content={"status": "ok"})

    @app.delete("/api/history")
    async def clear_history() -> JSONResponse:
        """Clear conversation history."""
        history_handler.clear_history()
        return JSONResponse(content={"status": "ok"})

    # ============================================================================
    # Diff Routes
    # ============================================================================

    @app.get("/api/diff/tree")
    async def get_diff_tree(path: str) -> JSONResponse:
        """Get tree structure of files with pending diffs.

        Args:
            path: Root project path

        Returns:
            JSON tree structure containing only modified files with diff stats
        """
        try:
            tree_data = diff_handler.get_diff_tree(path)
            return JSONResponse(content=tree_data)
        except Exception as e:
            logger.exception("Error getting diff tree")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.get("/api/diff/file")
    async def get_file_diff(path: str, project_path: str) -> JSONResponse:
        """Get unified diff for a specific file.

        Args:
            path: File path (relative to project)
            project_path: Project path

        Returns:
            Unified diff string (plain text)
        """
        try:
            diff_text = diff_handler.get_file_diff(project_path, path)
            return JSONResponse(content={"diff": diff_text})
        except ValueError as e:
            return JSONResponse(content={"error": str(e)}, status_code=404)
        except Exception as e:
            logger.exception("Error getting file diff")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.post("/api/diff")
    async def render_diff(data: dict[str, str]) -> JSONResponse:
        """Render diff between old and new content.

        Args:
            data: Dict with 'project_path', 'file_path', 'old_content', 'new_content'

        Returns:
            HTML diff rendered with diff2html
        """
        try:
            project_path = data.get("project_path", "")
            file_path = data.get("file_path", "")
            old_content = data.get("old_content", "")
            new_content = data.get("new_content", "")

            if not file_path:
                return JSONResponse(content={"error": "file_path is required"}, status_code=400)

            # Add diff to tree
            diff_handler.add_diff(project_path, file_path, old_content, new_content)

            # Render HTML
            html = diff_handler.render_diff_html(project_path, file_path)
            return JSONResponse(content={"html": html})
        except Exception as e:
            logger.exception("Error rendering diff")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.delete("/api/diff")
    async def remove_diff(path: str, project_path: str) -> JSONResponse:
        """Remove a diff from the tree.

        Args:
            path: File path (relative to project)
            project_path: Project path

        Returns:
            Status response
        """
        try:
            diff_handler.remove_diff(project_path, path)
            return JSONResponse(content={"status": "ok"})
        except Exception as e:
            logger.exception("Error removing diff")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.delete("/api/diff/all")
    async def clear_all_diffs(project_path: str) -> JSONResponse:
        """Clear all diffs for a project.

        Args:
            project_path: Project path

        Returns:
            Status response
        """
        try:
            diff_handler.clear_diffs(project_path)
            return JSONResponse(content={"status": "ok"})
        except Exception as e:
            logger.exception("Error clearing diffs")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.post("/api/diff/scan")
    async def scan_git_changes(data: dict[str, str]) -> JSONResponse:
        """Scan git working directory for changes and populate diff tree.

        Args:
            data: Dict with 'project_path'

        Returns:
            Status response with count of files found
        """
        try:
            project_path = data.get("project_path")
            if not project_path:
                return JSONResponse(content={"error": "project_path is required"}, status_code=400)

            count = diff_handler.scan_git_changes(project_path)
            return JSONResponse(content={"status": "ok", "count": count, "message": f"Found {count} changed files"})
        except RuntimeError as e:
            return JSONResponse(content={"error": str(e)}, status_code=400)
        except Exception as e:
            logger.exception("Error scanning git changes")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    # ============================================================================
    # Telegram API Routes
    # ============================================================================

    @app.post("/api/telegram/credentials")
    async def save_telegram_credentials(data: dict[str, str | None]) -> JSONResponse:
        """Save Telegram bot credentials.

        Args:
            data: Dict with 'bot_token' and optional 'chat_id'

        Returns:
            Status response
        """
        try:
            bot_token = data.get("bot_token")
            chat_id = data.get("chat_id")

            if not bot_token:
                return JSONResponse(content={"error": "bot_token is required"}, status_code=400)

            success = telegram_handler.save_credentials(bot_token, chat_id)  # type: ignore[arg-type]

            if success:
                return JSONResponse(content={"status": "ok"})
            else:
                return JSONResponse(content={"error": "Failed to save credentials"}, status_code=500)
        except Exception as e:
            logger.exception("Error saving Telegram credentials")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.post("/api/telegram/test")
    async def test_telegram_connection(data: dict[str, str]) -> JSONResponse:
        """Test Telegram bot connection.

        Args:
            data: Dict with 'bot_token'

        Returns:
            Bot info if successful
        """
        try:
            bot_token = data.get("bot_token")

            if not bot_token:
                return JSONResponse(content={"error": "bot_token is required"}, status_code=400)

            bot_info = await telegram_handler.test_bot_connection(bot_token)

            if bot_info:
                return JSONResponse(content={"status": "ok", "bot_info": bot_info})
            else:
                return JSONResponse(
                    content={
                        "error": "Failed to connect to bot. Please check your bot token and network connection.",
                        "details": "Check server logs for more information.",
                    },
                    status_code=400,
                )
        except Exception as e:
            logger.exception("Error testing Telegram connection")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.get("/api/telegram/status")
    async def get_telegram_status() -> JSONResponse:
        """Get Telegram connection status.

        Returns:
            Connection status and bot info if connected
        """
        try:
            # Check if Telegram bot server is running via daemon
            server_running = False
            try:
                status_url = "http://127.0.0.1:7565/telegram/status"
                with urllib.request.urlopen(status_url, timeout=2) as response:
                    daemon_status = json.loads(response.read())
                    server_running = daemon_status.get("running", False)
            except Exception:
                # Daemon not running or no bot server
                pass

            # Check both Web UI handler and system keyring
            connected = telegram_handler.is_connected()
            bot_token, chat_id = telegram_handler.get_credentials()

            # Fall back to system keyring if not found in Web UI handler
            if not bot_token:
                from ..agent.config import load_telegram_credentials

                bot_token, chat_id = load_telegram_credentials()

            # Treat empty string as no token (credentials were cleared)
            if bot_token and bot_token.strip():
                # Get bot info - with timeout protection
                try:
                    bot_info = await asyncio.wait_for(telegram_handler.test_bot_connection(bot_token), timeout=10.0)
                except (asyncio.TimeoutError, Exception) as e:
                    logger.warning("Bot connection test failed (%s), using fallback to extract bot_id from token", type(e).__name__)
                    bot_info = None

                # If bot test fails or times out, ALWAYS extract bot_id from token as fallback
                # This ensures UI can show at least partial info even when API is unavailable
                if bot_info is None:
                    bot_id = telegram_handler.extract_bot_id_from_token(bot_token)
                    if bot_id:
                        bot_info = {
                            "id": bot_id,
                            "username": None,  # Can't get username without API
                            "first_name": None,
                            "deep_link": None,
                            "from_token": True,  # Flag indicating this is partial info from token
                        }
                    else:
                        # Token format is invalid - log warning but still report credentials saved
                        logger.warning("Could not extract bot_id from token - token may be invalid")

                # Return credentials_saved flag even if bot test fails
                # This allows UI to show "credentials configured" vs "connection verified"
                return JSONResponse(
                    content={
                        "connected": bot_info is not None and not bot_info.get("from_token", False),  # True only if bot test succeeds
                        "credentials_saved": True,  # True if credentials exist
                        "bot_info": bot_info,
                        "chat_id": chat_id,
                        "from_keyring": not connected,
                        "server_running": server_running,  # Whether bot server is polling Telegram
                    }
                )
            else:
                return JSONResponse(
                    content={
                        "connected": False,
                        "credentials_saved": False,
                        "bot_info": None,
                        "chat_id": None,
                        "from_keyring": False,
                        "server_running": server_running,  # Whether bot server is polling Telegram
                    }
                )
        except Exception as e:
            logger.exception("Error getting Telegram status")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.get("/api/telegram/bot_info")
    async def get_telegram_bot_info() -> JSONResponse:
        """Get Telegram bot information.

        Returns:
            Bot info if available
        """
        try:
            bot_token, _ = telegram_handler.get_credentials()

            # Fall back to system keyring if not found
            if not bot_token:
                from ..agent.config import load_telegram_credentials

                bot_token, _ = load_telegram_credentials()

            if not bot_token:
                return JSONResponse(content={"error": "No bot token configured"}, status_code=404)

            bot_info = await telegram_handler.test_bot_connection(bot_token)

            if bot_info:
                return JSONResponse(content={"status": "ok", "bot_info": bot_info})
            else:
                return JSONResponse(content={"error": "Failed to get bot info"}, status_code=500)
        except Exception as e:
            logger.exception("Error getting bot info")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.post("/api/telegram/start_server")
    async def start_telegram_server() -> JSONResponse:
        """Start the Telegram bot server via daemon.

        Returns:
            Status response with server URL
        """
        try:
            from ..service import ensure_telegram_running

            # Start Telegram service via daemon
            success = ensure_telegram_running()

            if success:
                return JSONResponse(
                    content={
                        "status": "ok",
                        "message": "Telegram server started",
                        "url": "http://127.0.0.1:8889",
                    }
                )
            else:
                return JSONResponse(content={"error": "Failed to start Telegram server"}, status_code=500)
        except Exception as e:
            logger.exception("Error starting Telegram server")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.post("/api/telegram/send")
    async def send_telegram_message(data: dict[str, str]) -> JSONResponse:
        """Send message to Telegram chat.

        Args:
            data: Dict with 'chat_id' and 'message'

        Returns:
            Status response
        """
        try:
            chat_id = data.get("chat_id")
            message = data.get("message")

            if not chat_id or not message:
                return JSONResponse(content={"error": "chat_id and message are required"}, status_code=400)

            success = await telegram_handler.send_message(chat_id, message)

            if success:
                return JSONResponse(content={"status": "ok"})
            else:
                return JSONResponse(content={"error": "Failed to send message"}, status_code=500)
        except Exception as e:
            logger.exception("Error sending Telegram message")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    @app.delete("/api/telegram/credentials")
    async def delete_telegram_credentials() -> JSONResponse:
        """Clear Telegram credentials.

        Returns:
            Status response
        """
        try:
            success = telegram_handler.clear_credentials()

            if success:
                return JSONResponse(content={"status": "ok"})
            else:
                return JSONResponse(content={"error": "Failed to clear credentials"}, status_code=500)
        except Exception as e:
            logger.exception("Error clearing Telegram credentials")
            return JSONResponse(content={"error": str(e)}, status_code=500)

    # ============================================================================
    # Miscellaneous Routes
    # ============================================================================

    @app.get("/health")
    async def health_check() -> JSONResponse:
        """Health check endpoint."""
        return JSONResponse(content={"status": "ok"})

    @app.get("/api/cwd")
    async def get_cwd() -> JSONResponse:
        """Get current working directory."""
        return JSONResponse(content={"cwd": os.getcwd()})

    @app.get("/api/backlog")
    async def get_backlog(project_path: str | None = None) -> JSONResponse:
        """Get backlog tasks from Backlog.md.

        Args:
            project_path: Project directory path (defaults to cwd)

        Returns:
            JSON response with tasks array
        """
        # Use current working directory if no project_path provided
        if not project_path:
            project_path = os.getcwd()

        try:
            tasks = backlog_handler.get_backlog_tasks(project_path)
            return JSONResponse(content={"tasks": tasks})
        except Exception:
            logger.exception("Error getting backlog tasks")
            # Return empty tasks array on error (graceful degradation)
            return JSONResponse(content={"tasks": []})

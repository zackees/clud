"""Static file serving routes for Web UI.

This module handles serving static files, including:
- robots.txt at root
- SPA (Single Page Application) fallback routing
- MIME type configuration for Windows compatibility
"""

import mimetypes
from pathlib import Path

from fastapi import FastAPI
from fastapi.responses import FileResponse, Response


def init_mime_types() -> None:
    """Initialize MIME types for JavaScript modules on Windows.

    This is required for ES6 modules to load correctly, especially on Windows
    where the registry may have incorrect MIME types configured.
    """
    mimetypes.init()
    mimetypes.add_type("application/javascript", ".js")
    mimetypes.add_type("text/css", ".css")
    mimetypes.add_type("image/svg+xml", ".svg")


def register_static_routes(app: FastAPI, static_dir: Path) -> None:
    """Register static file serving routes.

    Note: We don't use StaticFiles middleware because it doesn't properly set
    MIME types for .js files on Windows. Instead, we handle all file serving
    through explicit routes with proper media_type settings.

    Args:
        app: FastAPI application instance
        static_dir: Directory containing static files
    """

    @app.get("/robots.txt")
    async def robots() -> FileResponse:
        """Serve robots.txt."""
        robots_file = static_dir / "robots.txt"
        if robots_file.exists():
            return FileResponse(robots_file)
        return FileResponse(static_dir / "index.html")

    @app.get("/{full_path:path}")
    async def serve_spa(full_path: str) -> Response:
        """Serve SPA index.html for all routes (SvelteKit SPA mode).

        Args:
            full_path: Requested path

        Returns:
            Response with appropriate content and MIME type
        """
        # Try to serve the file directly if it exists
        file_path = static_dir / full_path
        if file_path.is_file():
            # Determine correct media type based on file extension
            # IMPORTANT: Use Response with explicit content reading to avoid
            # Windows registry MIME type issues with FileResponse
            suffix = file_path.suffix.lower()
            if suffix == ".js":
                media_type = "application/javascript"
            elif suffix == ".css":
                media_type = "text/css"
            elif suffix == ".svg":
                media_type = "image/svg+xml"
            elif suffix == ".json":
                media_type = "application/json"
            elif suffix == ".html":
                media_type = "text/html"
            elif suffix in (".png", ".jpg", ".jpeg", ".gif", ".webp"):
                media_type = f"image/{suffix[1:]}"
            else:
                # Default to octet-stream for unknown types
                media_type = "application/octet-stream"

            # Read file content and return with explicit media type
            with open(file_path, "rb") as f:
                content = f.read()
            return Response(content=content, media_type=media_type)

        # Try to serve as HTML file (for SvelteKit prerendered pages)
        # Check if path.html exists (e.g., /terminal -> terminal.html)
        html_file_path = static_dir / f"{full_path}.html"
        if html_file_path.is_file():
            with open(html_file_path, "rb") as f:
                content = f.read()
            return Response(content=content, media_type="text/html")

        # Fall back to index.html for client-side routing
        index_file = static_dir / "index.html"
        with open(index_file, "rb") as f:
            content = f.read()
        return Response(content=content, media_type="text/html")

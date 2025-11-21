"""Server configuration and initialization utilities."""

import logging
import os
import socket
import subprocess
import sys
from pathlib import Path

logger = logging.getLogger(__name__)


def is_port_available(port: int) -> bool:
    """Check if a port is available for binding."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.bind(("localhost", port))
            return True
    except OSError:
        return False


def find_available_port(start_port: int = 8888) -> int:
    """Find an available port starting from start_port."""
    for port_candidate in range(start_port, start_port + 100):
        if is_port_available(port_candidate):
            return port_candidate
    raise RuntimeError(f"No available ports found starting from {start_port}")


def ensure_frontend_built() -> bool:
    """Ensure the frontend is built, building it if necessary.

    Returns:
        True if the frontend build exists (either already or after building), False otherwise.
    """
    frontend_dir = Path(__file__).parent / "frontend"
    build_dir = frontend_dir / "build"
    src_dir = frontend_dir / "src"
    package_json = frontend_dir / "package.json"

    # Check if frontend source exists
    if not frontend_dir.exists() or not package_json.exists():
        logger.debug("Frontend source directory not found - skipping auto-build")
        return False

    # Check if build directory exists
    if not build_dir.exists():
        logger.info("Frontend build directory not found - building frontend...")
        return _build_frontend(frontend_dir)

    # Check if source is newer than build
    try:
        build_time = build_dir.stat().st_mtime
        src_time = src_dir.stat().st_mtime if src_dir.exists() else 0

        # Find newest file in src directory
        if src_dir.exists():
            for root, _dirs, files in os.walk(src_dir):
                for file in files:
                    file_path = Path(root) / file
                    file_time = file_path.stat().st_mtime
                    if file_time > src_time:
                        src_time = file_time

        if src_time > build_time:
            logger.info("Frontend source is newer than build - rebuilding frontend...")
            return _build_frontend(frontend_dir)
    except OSError as e:
        logger.warning("Failed to check frontend timestamps: %s", e)

    # Build exists and is up-to-date
    return True


def _build_frontend(frontend_dir: Path) -> bool:
    """Build the frontend using npm.

    Args:
        frontend_dir: Path to the frontend directory

    Returns:
        True if build succeeded, False otherwise.
    """
    try:
        print("🔨 Building frontend... (this may take a moment)")

        # Check if node_modules exists, if not run npm install first
        node_modules = frontend_dir / "node_modules"
        if not node_modules.exists():
            logger.info("Installing frontend dependencies...")
            print("📦 Installing frontend dependencies...")
            result = subprocess.run(
                ["npm", "install"],
                cwd=frontend_dir,
                capture_output=True,
                text=True,
                timeout=300,  # 5 minute timeout
            )
            if result.returncode != 0:
                logger.error("Failed to install frontend dependencies: %s", result.stderr)
                print(f"❌ Failed to install frontend dependencies:\n{result.stderr}", file=sys.stderr)
                return False

        # Run npm build
        logger.info("Building frontend...")
        result = subprocess.run(
            ["npm", "run", "build"],
            cwd=frontend_dir,
            capture_output=True,
            text=True,
            timeout=300,  # 5 minute timeout
        )

        if result.returncode == 0:
            print("✅ Frontend build complete!")
            return True
        else:
            logger.error("Frontend build failed: %s", result.stderr)
            print(f"❌ Frontend build failed:\n{result.stderr}", file=sys.stderr)
            return False

    except FileNotFoundError:
        logger.error("npm not found - please install Node.js")
        print("❌ Error: npm not found. Please install Node.js.", file=sys.stderr)
        return False
    except subprocess.TimeoutExpired:
        logger.error("Frontend build timed out")
        print("❌ Error: Frontend build timed out after 5 minutes.", file=sys.stderr)
        return False
    except Exception as e:
        logger.exception("Unexpected error building frontend")
        print(f"❌ Error building frontend: {e}", file=sys.stderr)
        return False

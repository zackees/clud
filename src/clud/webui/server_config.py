"""Server configuration and initialization utilities."""

import hashlib
import logging
import shutil
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


def _get_package_version() -> str:
    """Get the clud package version.

    Returns:
        Version string (e.g., "1.0.34")
    """
    try:
        from importlib.metadata import version

        return version("clud")
    except Exception:
        # Fallback if package not installed properly
        return "dev"


def _get_frontend_cache_dir() -> Path:
    """Get the global cache directory for frontend builds.

    Returns:
        Path to cache directory (creates if doesn't exist)
    """
    import appdirs

    cache_dir = Path(str(appdirs.user_cache_dir("clud", "clud"))) / "webui-frontend"  # type: ignore[reportUnknownMemberType, reportUnknownArgumentType]
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir


def _get_frontend_version_hash() -> str:
    """Get a hash of the current frontend source to detect changes.

    Returns:
        SHA256 hash of key frontend files
    """
    frontend_dir = Path(__file__).parent / "frontend"
    package_json = frontend_dir / "package.json"

    # Include package version and package.json content in hash
    hasher = hashlib.sha256()
    hasher.update(_get_package_version().encode())

    if package_json.exists():
        hasher.update(package_json.read_bytes())

    return hasher.hexdigest()[:16]


def get_frontend_build_dir() -> Path | None:
    """Get the frontend build directory, building if necessary.

    Uses a global cache location with locking to prevent concurrent builds.
    Multiple installations can share the same cached build.

    Returns:
        Path to frontend build directory, or None if unavailable
    """
    import fasteners

    frontend_dir = Path(__file__).parent / "frontend"
    local_build_dir = frontend_dir / "build"

    # Check if frontend source exists
    if not frontend_dir.exists():
        logger.debug("Frontend source directory not found")
        return None

    # Get global cache directory
    cache_dir = _get_frontend_cache_dir()
    version_hash = _get_frontend_version_hash()
    cached_build_dir = cache_dir / version_hash / "build"
    lock_file = cache_dir / f"{version_hash}.lock"

    # Use lock to prevent concurrent builds
    lock = fasteners.InterProcessLock(str(lock_file))

    try:
        # Try to acquire lock (wait up to 60 seconds for other builds to complete)
        acquired = lock.acquire(blocking=True, timeout=60)

        if not acquired:
            logger.warning("Could not acquire lock for frontend build - using local build if available")
            # Fall back to local build if it exists
            return local_build_dir if local_build_dir.exists() else None

        try:
            # Check if cached build already exists and is valid
            if cached_build_dir.exists() and (cached_build_dir / "index.html").exists():
                logger.debug("Using cached frontend build: %s", cached_build_dir)
                return cached_build_dir

            # Need to build - check if we have local pre-built version
            if local_build_dir.exists() and (local_build_dir / "index.html").exists():
                logger.info("Copying pre-built frontend to cache...")
                # Copy local build to cache
                cached_build_dir.parent.mkdir(parents=True, exist_ok=True)
                if cached_build_dir.exists():
                    shutil.rmtree(cached_build_dir)
                shutil.copytree(local_build_dir, cached_build_dir)
                logger.info("Frontend cached at: %s", cached_build_dir)
                return cached_build_dir

            # No pre-built version - need to build from source
            logger.info("Building frontend from source...")
            if _build_frontend_to_cache(frontend_dir, cached_build_dir):
                return cached_build_dir
            else:
                # Build failed - try to use local build as fallback
                logger.warning("Frontend build failed - checking for local build")
                return local_build_dir if local_build_dir.exists() else None

        finally:
            lock.release()

    except Exception as e:
        logger.exception("Error managing frontend cache: %s", e)
        # Fall back to local build
        return local_build_dir if local_build_dir.exists() else None


def _build_frontend_to_cache(frontend_dir: Path, target_dir: Path) -> bool:
    """Build the frontend and place it in the cache directory.

    Args:
        frontend_dir: Source frontend directory
        target_dir: Target cache directory for build output

    Returns:
        True if build succeeded, False otherwise
    """
    try:
        print("🔨 Building frontend... (this may take a moment)")

        # Create a temporary build directory
        temp_build_dir = frontend_dir / "build"
        node_modules = frontend_dir / "node_modules"

        # Check if we can write to frontend_dir (might be read-only in site-packages)
        try:
            test_file = frontend_dir / ".write_test"
            test_file.touch()
            test_file.unlink()
        except (OSError, PermissionError):
            logger.warning("Frontend directory is read-only - cannot build from source")
            return False

        # Install dependencies if needed
        if not node_modules.exists():
            logger.info("Installing frontend dependencies...")
            print("📦 Installing frontend dependencies...")
            result = subprocess.run(
                ["npm", "install"],
                cwd=frontend_dir,
                capture_output=True,
                text=True,
                timeout=300,
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
            timeout=300,
        )

        if result.returncode != 0:
            logger.error("Frontend build failed: %s", result.stderr)
            print(f"❌ Frontend build failed:\n{result.stderr}", file=sys.stderr)
            return False

        # Copy build output to cache
        if temp_build_dir.exists():
            target_dir.parent.mkdir(parents=True, exist_ok=True)
            if target_dir.exists():
                shutil.rmtree(target_dir)
            shutil.copytree(temp_build_dir, target_dir)
            print("✅ Frontend build complete and cached!")
            return True
        else:
            logger.error("Build directory not created after build")
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

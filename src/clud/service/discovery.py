"""
Central server discovery and optional auto-spawn logic.

This module handles:
1. Discovery of clud-central server (env var, config file, default localhost:9876)
2. Health probing of central server
3. Optional auto-spawn of central via uvx (dev mode only)
4. Automatic reconnection with exponential backoff
"""

import logging
import os
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import httpx

logger = logging.getLogger(__name__)


@dataclass
class CentralInfo:
    """Information about the central server."""

    url: str
    auto_spawned: bool = False
    process: subprocess.Popen[bytes] | None = None


def _get_config_dir() -> Path:
    """Get the configuration directory for clud."""
    config_dir = Path.home() / ".config" / "clud"
    config_dir.mkdir(parents=True, exist_ok=True)
    return config_dir


def discover_central_url() -> str:
    """
    Discover the central server URL using priority order:
    1. CLUD_CENTRAL_URL environment variable
    2. ~/.config/clud/central.yaml config file
    3. Default to http://localhost:9876

    Returns:
        Central server URL
    """
    # 1. Check environment variable
    env_url = os.environ.get("CLUD_CENTRAL_URL")
    if env_url:
        logger.info(f"Using central URL from CLUD_CENTRAL_URL: {env_url}")
        return env_url

    # 2. Check config file
    config_file = _get_config_dir() / "central.yaml"
    if config_file.exists():
        try:
            import yaml

            with open(config_file) as f:
                config = yaml.safe_load(f)
                if config and "central_url" in config:
                    url = str(config["central_url"])
                    logger.info(f"Using central URL from config file: {url}")
                    return url
        except Exception as e:
            logger.warning(f"Failed to parse config file {config_file}: {e}")

    # 3. Default to localhost:9876
    default_url = "http://localhost:9876"
    logger.info(f"Using default central URL: {default_url}")
    return default_url


def probe_central_health(url: str, timeout: float = 2.0) -> bool:
    """
    Probe the central server health endpoint.

    Args:
        url: Central server URL
        timeout: Request timeout in seconds

    Returns:
        True if central is reachable and healthy, False otherwise
    """
    try:
        health_url = f"{url}/health"
        response = httpx.get(health_url, timeout=timeout)
        if response.status_code == 200:
            logger.debug(f"Central health check passed: {health_url}")
            return True
        else:
            logger.debug(f"Central health check failed with status {response.status_code}: {health_url}")
            return False
    except Exception as e:
        logger.debug(f"Central health check failed: {e}")
        return False


def _try_spawn_via_uvx() -> subprocess.Popen[bytes] | None:
    """
    Try to spawn clud-central via uvx.

    Returns:
        Subprocess if successful, None otherwise
    """
    # Check if uvx is available
    if not shutil.which("uvx"):
        logger.debug("uvx not found in PATH")
        return None

    try:
        log_file = _get_config_dir() / "central.log"
        logger.info(f"Spawning clud-central via uvx (logs: {log_file})")

        # First time might take longer to download and install
        logger.info("Downloading clud-central (first time only)...")

        with open(log_file, "a") as f:
            process = subprocess.Popen(
                ["uvx", "clud-central", "serve"],
                stdout=f,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                start_new_session=True,  # Detach from parent on Unix
            )

        logger.info(f"Auto-spawned clud-central (dev mode) - logs: {log_file}")
        return process
    except Exception as e:
        logger.warning(f"Failed to spawn via uvx: {e}")
        return None


def _try_spawn_via_pipx() -> subprocess.Popen[bytes] | None:
    """
    Try to spawn clud-central via pipx.

    Returns:
        Subprocess if successful, None otherwise
    """
    # Check if pipx is available
    if not shutil.which("pipx"):
        logger.debug("pipx not found in PATH")
        return None

    try:
        log_file = _get_config_dir() / "central.log"
        logger.info(f"Spawning clud-central via pipx (logs: {log_file})")

        with open(log_file, "a") as f:
            process = subprocess.Popen(
                ["pipx", "run", "clud-central", "serve"],
                stdout=f,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                start_new_session=True,  # Detach from parent on Unix
            )

        logger.info(f"Auto-spawned clud-central (dev mode) - logs: {log_file}")
        return process
    except Exception as e:
        logger.warning(f"Failed to spawn via pipx: {e}")
        return None


def _try_spawn_direct() -> subprocess.Popen[bytes] | None:
    """
    Try to spawn clud-central directly if it's in PATH.

    Returns:
        Subprocess if successful, None otherwise
    """
    # Check if clud-central is available
    if not shutil.which("clud-central"):
        logger.debug("clud-central not found in PATH")
        return None

    try:
        log_file = _get_config_dir() / "central.log"
        logger.info(f"Spawning clud-central directly (logs: {log_file})")

        with open(log_file, "a") as f:
            process = subprocess.Popen(
                ["clud-central", "serve"],
                stdout=f,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                start_new_session=True,  # Detach from parent on Unix
            )

        logger.info(f"Auto-spawned clud-central (dev mode) - logs: {log_file}")
        return process
    except Exception as e:
        logger.warning(f"Failed to spawn directly: {e}")
        return None


def auto_spawn_central(url: str) -> subprocess.Popen[bytes] | None:
    """
    Attempt to auto-spawn clud-central if CLUD_AUTO_SPAWN=1.

    Tries in priority order:
    1. uvx clud-central serve (auto-installs if needed)
    2. pipx run clud-central serve (if pipx available)
    3. clud-central serve (if installed in PATH)
    4. Fail with clear error message

    Args:
        url: Central server URL to wait for

    Returns:
        Subprocess if successful, None otherwise
    """
    # Check if auto-spawn is enabled
    auto_spawn = os.environ.get("CLUD_AUTO_SPAWN", "0")
    if auto_spawn != "1":
        logger.debug("Auto-spawn disabled (CLUD_AUTO_SPAWN != 1)")
        return None

    logger.info("Auto-spawn enabled (dev mode)")

    # Try each method in order
    process = _try_spawn_via_uvx() or _try_spawn_via_pipx() or _try_spawn_direct()

    if not process:
        error_msg = "Failed to auto-spawn clud-central. Install uvx or clud-central:\n  pip install uv  # For uvx support\n  pip install clud-central  # For direct execution"
        logger.error(error_msg)
        print(error_msg)
        return None

    # Wait for health check with retries (max 30s, longer on first install)
    max_wait = 30.0
    start_time = time.time()
    retry_count = 0

    while time.time() - start_time < max_wait:
        if probe_central_health(url, timeout=2.0):
            logger.info(f"Central health check passed after {time.time() - start_time:.1f}s")
            return process

        retry_count += 1
        wait_time = min(2**retry_count, 5.0)  # Exponential backoff, max 5s
        logger.debug(f"Waiting for central to start (retry {retry_count}, wait {wait_time}s)...")
        time.sleep(wait_time)

        # Check if process died
        if process.poll() is not None:
            logger.error(f"Central process died with exit code {process.returncode}")
            return None

    logger.error(f"Central failed to start within {max_wait}s")
    # Kill the process if it's still running
    try:
        process.terminate()
        process.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
    return None


def ensure_central(url: str | None = None) -> CentralInfo | None:
    """
    Ensure central server is available.

    1. Discover central URL (if not provided)
    2. Probe health endpoint
    3. If not reachable and CLUD_AUTO_SPAWN=1, attempt auto-spawn
    4. If still not reachable, fail with clear error message

    Args:
        url: Optional central URL (if None, uses discovery)

    Returns:
        CentralInfo if successful, None otherwise
    """
    # Discover URL if not provided
    if url is None:
        url = discover_central_url()

    # Probe health
    if probe_central_health(url):
        logger.info(f"Central is reachable at {url}")
        return CentralInfo(url=url, auto_spawned=False)

    # Try auto-spawn if enabled
    logger.warning(f"Central not reachable at {url}")
    process = auto_spawn_central(url)

    if process:
        return CentralInfo(url=url, auto_spawned=True, process=process)

    # Failed to reach or spawn central
    error_msg = (
        f"Cannot reach clud-central at {url}\n\n"
        "To fix this:\n"
        "1. Start central manually: clud-central serve\n"
        "2. Or enable auto-spawn (dev mode): export CLUD_AUTO_SPAWN=1\n"
        "3. Or set custom URL: export CLUD_CENTRAL_URL=http://your-server:9876\n"
    )
    logger.error(error_msg)
    print(error_msg)
    return None


class CentralConnection:
    """
    Manages connection to central server with automatic reconnection.
    """

    def __init__(self, central_info: CentralInfo):
        self.central_info = central_info
        self.client = httpx.Client(timeout=10.0)
        self._retry_count = 0
        self._max_backoff = 30.0

    def _get_backoff_time(self) -> float:
        """Calculate exponential backoff time."""
        # 1s, 2s, 4s, 8s, 16s, 30s (max)
        backoff = min(2**self._retry_count, self._max_backoff)
        return backoff

    def _reset_backoff(self) -> None:
        """Reset backoff counter after successful request."""
        self._retry_count = 0

    def request(self, method: str, path: str, **kwargs: object) -> httpx.Response | None:
        """
        Make a request to central with automatic retry and exponential backoff.

        Args:
            method: HTTP method (GET, POST, etc.)
            path: Request path (e.g., /api/agents/register)
            **kwargs: Additional arguments for httpx request

        Returns:
            Response if successful, None otherwise
        """
        url = f"{self.central_info.url}{path}"

        while True:
            try:
                response = self.client.request(method, url, **kwargs)  # type: ignore[arg-type]
                response.raise_for_status()
                self._reset_backoff()
                return response
            except Exception as e:
                self._retry_count += 1
                backoff = self._get_backoff_time()
                logger.warning(f"Request to {url} failed (retry {self._retry_count}, backoff {backoff}s): {e}")
                time.sleep(backoff)

                # Check if we should give up
                if self._retry_count > 10:  # After ~17 minutes of retrying
                    logger.error(f"Giving up on request to {url} after {self._retry_count} retries")
                    return None

    def post_json(self, path: str, data: dict[str, object]) -> httpx.Response | None:
        """
        POST JSON data to central.

        Args:
            path: Request path
            data: JSON data to send

        Returns:
            Response if successful, None otherwise
        """
        return self.request("POST", path, json=data)

    def get_json(self, path: str) -> dict[str, object] | None:
        """
        GET JSON data from central.

        Args:
            path: Request path

        Returns:
            JSON response if successful, None otherwise
        """
        response = self.request("GET", path)
        if response:
            return response.json()  # type: ignore[no-any-return]
        return None

    def close(self) -> None:
        """Close the HTTP client."""
        self.client.close()

        # Terminate auto-spawned central if we own the process
        if self.central_info.auto_spawned and self.central_info.process:
            logger.info("Terminating auto-spawned central")
            try:
                self.central_info.process.terminate()
                self.central_info.process.wait(timeout=5.0)
                logger.info("Central terminated gracefully")
            except subprocess.TimeoutExpired:
                logger.warning("Central did not terminate, killing...")
                self.central_info.process.kill()
                self.central_info.process.wait()
                logger.info("Central killed")

    def __enter__(self) -> "CentralConnection":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:  # type: ignore[no-untyped-def]
        self.close()

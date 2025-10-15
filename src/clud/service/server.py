"""Local daemon server for agent coordination and telegram service management."""

import asyncio
import http.server
import json
import logging
import os
import socket
import socketserver
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

from .models import AgentInfo, AgentStatus
from .registry import AgentRegistry

logger = logging.getLogger(__name__)

# Daemon configuration
DAEMON_HOST = "127.0.0.1"
DAEMON_PORT = 7565
DAEMON_PID_FILE = Path.home() / ".config" / "clud" / "daemon.pid"


class DaemonRequestHandler(http.server.BaseHTTPRequestHandler):
    """HTTP request handler for daemon endpoints."""

    registry: AgentRegistry  # Set by server
    telegram_manager: Any  # TelegramServiceManager, set by server

    def log_message(self, format: str, *args: Any) -> None:
        """Override to use logging instead of stderr."""
        logger.debug(f"{self.address_string()} - {format % args}")

    def _send_json_response(self, data: dict[str, Any], status: int = 200) -> None:
        """Send JSON response."""
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data).encode("utf-8"))

    def _send_error_response(self, message: str, status: int = 400) -> None:
        """Send error response."""
        self._send_json_response({"error": message}, status)

    def _read_json_body(self) -> dict[str, Any] | None:
        """Read and parse JSON body."""
        content_length = int(self.headers.get("Content-Length", 0))
        if content_length == 0:
            return None

        body = self.rfile.read(content_length)
        try:
            return json.loads(body.decode("utf-8"))
        except json.JSONDecodeError as e:
            logger.warning(f"Invalid JSON in request: {e}")
            return None

    def do_GET(self) -> None:
        """Handle GET requests."""
        if self.path == "/health":
            self._handle_health()
        elif self.path == "/telegram/status":
            self._handle_telegram_status()
        elif self.path == "/agents":
            self._handle_list_agents()
        elif self.path.startswith("/agents/"):
            agent_id = self.path.split("/")[-1]
            self._handle_get_agent(agent_id)
        else:
            self._send_error_response("Not found", 404)

    def do_POST(self) -> None:
        """Handle POST requests."""
        if self.path == "/telegram/start":
            self._handle_telegram_start()
        elif self.path == "/telegram/stop":
            self._handle_telegram_stop()
        elif self.path == "/agents/register":
            self._handle_register_agent()
        elif self.path.startswith("/agents/") and self.path.endswith("/heartbeat"):
            agent_id = self.path.split("/")[-2]
            self._handle_heartbeat(agent_id)
        elif self.path.startswith("/agents/") and self.path.endswith("/stop"):
            agent_id = self.path.split("/")[-2]
            self._handle_stop_agent(agent_id)
        else:
            self._send_error_response("Not found", 404)

    def _handle_health(self) -> None:
        """Handle health check."""
        agent_count = len(self.registry.list_all())
        running_count = len(self.registry.list_by_status(AgentStatus.RUNNING))
        stale_count = len(self.registry.list_stale())

        self._send_json_response(
            {
                "status": "ok",
                "pid": os.getpid(),
                "agents": {"total": agent_count, "running": running_count, "stale": stale_count},
            }
        )

    def _handle_register_agent(self) -> None:
        """Handle agent registration."""
        logger.debug("Received agent registration request")

        data = self._read_json_body()
        if not data:
            logger.warning("Registration request missing body")
            self._send_error_response("Missing request body")
            return

        logger.debug(f"Registration data: {data}")

        required_fields = ["agent_id", "cwd", "pid", "command"]
        if not all(field in data for field in required_fields):
            logger.warning(f"Registration missing required fields: {[f for f in required_fields if f not in data]}")
            self._send_error_response(f"Missing required fields: {required_fields}")
            return

        try:
            agent = AgentInfo(
                agent_id=data["agent_id"],
                cwd=data["cwd"],
                pid=data["pid"],
                command=data["command"],
                status=AgentStatus.STARTING,
                capabilities=data.get("capabilities", {}),
            )
            logger.debug(f"Created AgentInfo: agent_id={agent.agent_id}, pid={agent.pid}")
            self.registry.register(agent)
            logger.info(f"Registered agent: {agent.agent_id}")
            self._send_json_response({"status": "registered", "agent_id": agent.agent_id}, 201)
        except Exception as e:
            logger.error(f"Error registering agent: {e}", exc_info=True)
            self._send_error_response(f"Registration failed: {e}", 500)

    def _handle_heartbeat(self, agent_id: str) -> None:
        """Handle agent heartbeat."""
        logger.debug(f"Received heartbeat for agent: {agent_id}")

        data = self._read_json_body() or {}
        logger.debug(f"Heartbeat data: {data}")

        # Extract optional status update
        status = None
        if "status" in data:
            try:
                status = AgentStatus(data["status"])
                logger.debug(f"Heartbeat includes status update: {status.value}")
            except ValueError:
                logger.warning(f"Invalid status in heartbeat: {data['status']}")
                self._send_error_response(f"Invalid status: {data['status']}")
                return
            # Remove status from data to avoid overwriting with raw string
            data = {k: v for k, v in data.items() if k != "status"}

        # Update heartbeat
        success = self.registry.update_heartbeat(agent_id, status=status, **data)

        if success:
            logger.debug(f"Heartbeat updated successfully for agent: {agent_id}")
            self._send_json_response({"status": "ok"})
        else:
            logger.warning(f"Heartbeat failed - agent not found: {agent_id}")
            self._send_error_response("Agent not found", 404)

    def _handle_get_agent(self, agent_id: str) -> None:
        """Handle get agent by ID."""
        agent = self.registry.get(agent_id)
        if agent:
            self._send_json_response(agent.to_dict())
        else:
            self._send_error_response("Agent not found", 404)

    def _handle_list_agents(self) -> None:
        """Handle list all agents."""
        agents = self.registry.list_all()
        self._send_json_response({"agents": [agent.to_dict() for agent in agents]})

    def _handle_stop_agent(self, agent_id: str) -> None:
        """Handle stop agent request."""
        logger.debug(f"Received stop request for agent: {agent_id}")

        data = self._read_json_body() or {}
        exit_code = data.get("exit_code", 0)
        logger.debug(f"Stop request data: exit_code={exit_code}")

        success = self.registry.mark_stopped(agent_id, exit_code)
        if success:
            logger.info(f"Agent stopped successfully: {agent_id} (exit_code={exit_code})")
            self._send_json_response({"status": "stopped"})
        else:
            logger.warning(f"Stop failed - agent not found: {agent_id}")
            self._send_error_response("Agent not found", 404)

    def _handle_telegram_status(self) -> None:
        """Handle telegram service status request."""
        status = self.telegram_manager.get_status()
        self._send_json_response(status)

    def _handle_telegram_start(self) -> None:
        """Handle telegram service start request."""
        logger.debug("Received telegram start request")

        data = self._read_json_body() or {}
        config_path = data.get("config_path")
        port = data.get("port")

        try:
            success = self.telegram_manager.start_service(config_path=config_path, port=port)
            if success:
                self._send_json_response({"status": "started"}, 201)
            else:
                self._send_error_response("Failed to start telegram service", 500)
        except Exception as e:
            logger.error(f"Error starting telegram service: {e}")
            self._send_error_response(f"Failed to start telegram service: {e}", 500)

    def _handle_telegram_stop(self) -> None:
        """Handle telegram service stop request."""
        logger.debug("Received telegram stop request")

        try:
            success = self.telegram_manager.stop_service()
            if success:
                self._send_json_response({"status": "stopped"})
            else:
                self._send_error_response("Telegram service not running", 400)
        except Exception as e:
            logger.error(f"Error stopping telegram service: {e}")
            self._send_error_response(f"Failed to stop telegram service: {e}", 500)


class TelegramServiceManager:
    """Manages telegram service lifecycle within the daemon."""

    def __init__(self) -> None:
        """Initialize telegram service manager."""
        self.is_running = False
        self.server_thread: threading.Thread | None = None
        self.telegram_server: Any = None  # TelegramServer instance
        self.asyncio_loop: asyncio.AbstractEventLoop | None = None
        self.config: Any = None  # TelegramIntegrationConfig
        logger.debug("TelegramServiceManager initialized")

    def get_status(self) -> dict[str, Any]:
        """Get telegram service status.

        Returns:
            Status dictionary with running state and config info
        """
        status: dict[str, Any] = {"running": self.is_running}
        if self.is_running and self.config:
            status["port"] = self.config.web.port
            status["host"] = self.config.web.host
            status["bot_configured"] = bool(self.config.telegram.bot_token)
        return status

    def start_service(self, config_path: str | None = None, port: int | None = None) -> bool:
        """Start the telegram service.

        Args:
            config_path: Optional path to telegram config file
            port: Optional port override

        Returns:
            True if started successfully, False otherwise
        """
        if self.is_running:
            logger.warning("Telegram service already running")
            return False

        logger.info("Starting telegram service...")

        try:
            # Import telegram modules (lazy import to avoid dependency issues)
            from clud.telegram.config import TelegramIntegrationConfig
            from clud.telegram.server import TelegramServer

            # Load configuration
            self.config = TelegramIntegrationConfig.load(config_file=config_path)

            # Override port if provided
            if port is not None:
                self.config.web.port = port

            # Validate configuration
            validation_errors = self.config.validate()
            if validation_errors:
                logger.error(f"Telegram configuration errors: {validation_errors}")
                return False

            # Create telegram server
            self.telegram_server = TelegramServer(self.config)

            # Start in separate thread with its own event loop
            def run_telegram_service() -> None:
                """Run telegram service in its own thread."""
                import uvicorn

                # Create new event loop for this thread
                self.asyncio_loop = asyncio.new_event_loop()
                asyncio.set_event_loop(self.asyncio_loop)

                try:
                    # Start telegram server (bot + web)
                    self.asyncio_loop.run_until_complete(self.telegram_server.start())

                    # Run uvicorn server
                    if self.telegram_server.app:
                        uvicorn_config = uvicorn.Config(
                            self.telegram_server.app,
                            host=self.config.web.host,
                            port=self.config.web.port,
                            log_level=self.config.logging.level.lower(),
                        )
                        uvicorn_server = uvicorn.Server(uvicorn_config)
                        self.asyncio_loop.run_until_complete(uvicorn_server.serve())
                except Exception as e:
                    logger.error(f"Telegram service error: {e}", exc_info=True)
                finally:
                    # Cleanup
                    if self.telegram_server:
                        self.asyncio_loop.run_until_complete(self.telegram_server.stop())
                    self.asyncio_loop.close()
                    self.is_running = False

            # Start thread
            self.server_thread = threading.Thread(target=run_telegram_service, daemon=True)
            self.server_thread.start()
            self.is_running = True

            logger.info(f"Telegram service started on {self.config.web.host}:{self.config.web.port}")
            return True

        except Exception as e:
            logger.error(f"Failed to start telegram service: {e}", exc_info=True)
            return False

    def stop_service(self) -> bool:
        """Stop the telegram service.

        Returns:
            True if stopped successfully, False if not running
        """
        if not self.is_running:
            logger.warning("Telegram service not running")
            return False

        logger.info("Stopping telegram service...")

        try:
            # Signal the event loop to stop
            if self.asyncio_loop and self.telegram_server:
                # Schedule stop coroutine in the telegram service's event loop
                asyncio.run_coroutine_threadsafe(self.telegram_server.stop(), self.asyncio_loop)

            # Wait for thread to finish (with timeout)
            if self.server_thread:
                self.server_thread.join(timeout=5.0)

            self.is_running = False
            self.server_thread = None
            self.telegram_server = None
            self.asyncio_loop = None

            logger.info("Telegram service stopped")
            return True

        except Exception as e:
            logger.error(f"Error stopping telegram service: {e}", exc_info=True)
            return False


class DaemonServer:
    """Local daemon server for agent coordination and telegram service."""

    def __init__(self, host: str = DAEMON_HOST, port: int = DAEMON_PORT, db_path: Path | None = None) -> None:
        """Initialize daemon server.

        Args:
            host: Host to bind to (default: 127.0.0.1)
            port: Port to bind to (default: 7565)
            db_path: Optional path to SQLite database for persistence
        """
        self.host = host
        self.port = port
        self.registry = AgentRegistry(db_path=db_path, use_persistence=db_path is not None)
        self.telegram_manager = TelegramServiceManager()
        self.server: socketserver.TCPServer | None = None
        self.cluster_client: Any = None  # ClusterClient instance (optional)

    def start(self) -> None:
        """Start the daemon server."""
        logger.debug("Starting daemon server")

        # Create request handler class with registry and telegram manager access
        handler_class = DaemonRequestHandler
        handler_class.registry = self.registry
        handler_class.telegram_manager = self.telegram_manager
        logger.debug("Request handler class configured with registry and telegram manager")

        # Try to connect to cluster (non-blocking)
        logger.debug("Attempting to connect to clud-cluster")
        try:
            from .cluster_client import ClusterClient

            self.cluster_client = ClusterClient(daemon_port=self.port)
            if self.cluster_client.start(self.registry):
                logger.info("Connected to clud-cluster")
            else:
                logger.info("Running in offline mode (cluster not available)")
                self.cluster_client = None
        except Exception as e:
            logger.warning(f"Failed to connect to cluster: {e}")
            self.cluster_client = None

        # Create and start server
        logger.debug(f"Creating TCP server on {self.host}:{self.port}")
        self.server = socketserver.TCPServer((self.host, self.port), handler_class)
        self.server.allow_reuse_address = True

        logger.info(f"Daemon server starting on {self.host}:{self.port}")
        print(f"clud daemon listening on {self.host}:{self.port}")

        try:
            logger.debug("Entering server main loop")
            self.server.serve_forever()
        except KeyboardInterrupt:
            logger.info("Daemon server shutting down")
            self.shutdown()

    def shutdown(self) -> None:
        """Shutdown the daemon server."""
        # Stop telegram service if running
        if self.telegram_manager.is_running:
            logger.info("Stopping telegram service...")
            self.telegram_manager.stop_service()

        # Stop cluster client
        if self.cluster_client:
            self.cluster_client.stop()

        # Shutdown HTTP server
        if self.server:
            self.server.shutdown()
            self.server.server_close()

        # Close registry
        self.registry.close()


def is_daemon_running() -> bool:
    """Check if daemon is already running.

    Returns:
        True if daemon is running, False otherwise
    """
    logger.debug(f"Checking if daemon is running on {DAEMON_HOST}:{DAEMON_PORT}")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(1.0)
        result = sock.connect_ex((DAEMON_HOST, DAEMON_PORT))
        sock.close()
        is_running = result == 0
        logger.debug(f"Daemon running check: {is_running} (result={result})")
        return is_running
    except Exception as e:
        logger.debug(f"Daemon running check failed with exception: {e}")
        return False


def probe_daemon_health() -> dict[str, Any] | None:
    """Probe daemon health endpoint.

    Returns:
        Health response data if daemon is running, None otherwise
    """
    import urllib.request

    health_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/health"
    logger.debug(f"Probing daemon health at {health_url}")

    try:
        with urllib.request.urlopen(health_url, timeout=2.0) as response:
            health_data = json.loads(response.read().decode("utf-8"))
            logger.debug(f"Daemon health response: {health_data}")
            return health_data
    except Exception as e:
        logger.debug(f"Daemon health probe failed: {e}")
        return None


def spawn_daemon() -> bool:
    """Spawn daemon as a background process.

    Returns:
        True if daemon was spawned successfully, False otherwise
    """
    logger.debug("Attempting to spawn daemon process")

    # Ensure config directory exists
    config_dir = DAEMON_PID_FILE.parent
    logger.debug(f"Config directory: {config_dir}")
    config_dir.mkdir(parents=True, exist_ok=True)
    logger.debug("Config directory created/verified")

    # Create daemon command
    daemon_cmd = [sys.executable, "-m", "clud.service.server"]
    logger.debug(f"Daemon command: {daemon_cmd}")

    # Spawn daemon as background process
    try:
        # On Windows, use DETACHED_PROCESS
        if sys.platform == "win32":
            logger.debug("Using Windows DETACHED_PROCESS flags")
            creation_flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
            process = subprocess.Popen(
                daemon_cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                stdin=subprocess.DEVNULL,
                creationflags=creation_flags,
            )
        else:
            # On Unix, use nohup-like approach
            logger.debug("Using Unix start_new_session approach")
            process = subprocess.Popen(
                daemon_cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
            )

        # Save PID
        logger.debug(f"Saving daemon PID to {DAEMON_PID_FILE}")
        DAEMON_PID_FILE.write_text(str(process.pid))
        logger.info(f"Spawned daemon process (pid={process.pid})")
        return True

    except Exception as e:
        logger.error(f"Failed to spawn daemon: {e}", exc_info=True)
        return False


def ensure_daemon_running(max_wait: float = 5.0) -> bool:
    """Ensure daemon is running, spawning if necessary.

    Args:
        max_wait: Maximum time to wait for daemon to start (seconds)

    Returns:
        True if daemon is running, False if failed to start
    """
    logger.debug(f"Ensuring daemon is running (max_wait={max_wait}s)")

    # Check if already running
    if is_daemon_running():
        logger.debug("Daemon already running")
        return True

    # Spawn daemon
    logger.info("Daemon not running, spawning...")
    if not spawn_daemon():
        logger.error("Failed to spawn daemon")
        return False

    # Wait for daemon to start
    logger.debug(f"Waiting up to {max_wait}s for daemon to start")
    start_time = time.time()
    attempts = 0
    while time.time() - start_time < max_wait:
        attempts += 1
        logger.debug(f"Checking if daemon is running (attempt {attempts})")
        if is_daemon_running():
            elapsed = time.time() - start_time
            logger.info(f"Daemon started successfully after {elapsed:.2f}s ({attempts} attempts)")
            return True
        time.sleep(0.2)

    elapsed = time.time() - start_time
    logger.error(f"Daemon failed to start within {elapsed:.2f}s timeout ({attempts} attempts)")
    return False


def ensure_telegram_running(config_path: str | None = None, port: int | None = None, max_wait: float = 10.0) -> bool:
    """Ensure telegram service is running via daemon, starting if necessary.

    Args:
        config_path: Optional path to telegram config file
        port: Optional port override
        max_wait: Maximum time to wait for telegram to start (seconds)

    Returns:
        True if telegram service is running, False otherwise
    """
    import urllib.request

    logger.debug("Ensuring telegram service is running")

    # First ensure daemon is running
    if not ensure_daemon_running():
        logger.error("Failed to ensure daemon is running")
        return False

    # Check telegram status
    status_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/telegram/status"
    try:
        with urllib.request.urlopen(status_url, timeout=2.0) as response:
            status_data = json.loads(response.read().decode("utf-8"))
            if status_data.get("running"):
                logger.debug("Telegram service already running")
                return True
    except Exception as e:
        logger.debug(f"Telegram status check failed: {e}")

    # Start telegram service
    logger.info("Starting telegram service via daemon...")
    start_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/telegram/start"
    request_data = {}
    if config_path:
        request_data["config_path"] = config_path
    if port:
        request_data["port"] = port

    try:
        req = urllib.request.Request(
            start_url,
            data=json.dumps(request_data).encode("utf-8") if request_data else None,
            headers={"Content-Type": "application/json"},
        )

        with urllib.request.urlopen(req, timeout=5.0) as response:
            result = json.loads(response.read().decode("utf-8"))
            if result.get("status") == "started":
                logger.info("Telegram service started successfully")

                # Wait for service to be ready
                start_time = time.time()
                while time.time() - start_time < max_wait:
                    try:
                        with urllib.request.urlopen(status_url, timeout=2.0) as status_response:
                            status_data = json.loads(status_response.read().decode("utf-8"))
                            if status_data.get("running"):
                                logger.info("Telegram service is ready")
                                return True
                    except Exception:
                        pass
                    time.sleep(0.5)

                logger.warning(f"Telegram service started but not ready within {max_wait}s")
                return True  # Service started, even if not fully ready

            logger.error(f"Failed to start telegram service: {result}")
            return False

    except Exception as e:
        logger.error(f"Failed to start telegram service: {e}", exc_info=True)
        return False


def main() -> int:
    """Main entry point for running daemon directly."""
    # Set up logging
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )

    # Create daemon server
    config_dir = DAEMON_PID_FILE.parent
    config_dir.mkdir(parents=True, exist_ok=True)
    db_path = config_dir / "agents.db"

    server = DaemonServer(db_path=db_path)

    try:
        server.start()
        return 0
    except Exception as e:
        logger.error(f"Daemon error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())

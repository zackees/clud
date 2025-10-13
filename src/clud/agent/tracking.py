"""Agent tracking and telemetry."""

import json
import logging
import os
import threading
import uuid

from ..service.models import AgentStatus
from ..service.server import DAEMON_HOST, DAEMON_PORT, ensure_daemon_running

logger = logging.getLogger(__name__)


class AgentTracker:
    """Tracks a single agent and sends telemetry to daemon."""

    def __init__(self, command: str, agent_id: str | None = None):
        """Initialize agent tracker.

        Args:
            command: Command being executed
            agent_id: Optional agent ID (generated if not provided)
        """
        self.agent_id = agent_id or str(uuid.uuid4())
        self.command = command
        self.cwd = os.getcwd()
        self.pid = os.getpid()
        self.status = AgentStatus.STARTING
        self.daemon_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}"
        self._heartbeat_thread: threading.Thread | None = None
        self._stop_heartbeat = threading.Event()
        self._registered = False

    def start(self) -> bool:
        """Start tracking - ensure daemon and register agent.

        Returns:
            True if tracking started successfully, False otherwise
        """
        # Ensure daemon is running
        if not ensure_daemon_running():
            logger.error("Failed to ensure daemon is running")
            return False

        # Register with daemon
        if not self._register():
            logger.error("Failed to register with daemon")
            return False

        # Start heartbeat thread
        self._start_heartbeat()

        print(f"✓ Tracking enabled (agent_id: {self.agent_id})")
        return True

    def _register(self) -> bool:
        """Register agent with daemon.

        Returns:
            True if registration successful, False otherwise
        """
        import urllib.request

        try:
            data: dict[str, str | int | dict[str, str]] = {
                "agent_id": self.agent_id,
                "cwd": self.cwd,
                "pid": self.pid,
                "command": self.command,
                "capabilities": {},
            }

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/register",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            with urllib.request.urlopen(req, timeout=5.0) as response:
                result = json.loads(response.read().decode("utf-8"))
                self._registered = result.get("status") == "registered"
                logger.info(f"Agent registered: {self.agent_id}")
                return True

        except Exception as e:
            logger.error(f"Failed to register agent: {e}")
            return False

    def _start_heartbeat(self) -> None:
        """Start heartbeat thread."""
        self._stop_heartbeat.clear()
        self._heartbeat_thread = threading.Thread(target=self._heartbeat_loop, daemon=True)
        self._heartbeat_thread.start()

    def _heartbeat_loop(self) -> None:
        """Heartbeat loop - sends status every 5 seconds."""
        while not self._stop_heartbeat.is_set():
            self._send_heartbeat()
            self._stop_heartbeat.wait(5.0)  # 5 second interval

    def _send_heartbeat(self) -> None:
        """Send heartbeat to daemon."""
        import urllib.request

        try:
            data = {"status": self.status.value}

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/{self.agent_id}/heartbeat",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            with urllib.request.urlopen(req, timeout=5.0) as response:
                result = json.loads(response.read().decode("utf-8"))
                logger.debug(f"Heartbeat sent: {result}")

        except Exception as e:
            logger.warning(f"Failed to send heartbeat: {e}")

    def update_status(self, status: AgentStatus) -> None:
        """Update agent status.

        Args:
            status: New status
        """
        self.status = status
        logger.info(f"Agent status updated: {status.value}")

    def stop(self, exit_code: int = 0) -> None:
        """Stop tracking and notify daemon.

        Args:
            exit_code: Exit code of the agent
        """
        # Stop heartbeat thread
        if self._heartbeat_thread:
            self._stop_heartbeat.set()
            self._heartbeat_thread.join(timeout=2.0)

        # Notify daemon of stop
        if self._registered:
            self._notify_stopped(exit_code)

        logger.info(f"Agent tracking stopped (exit_code={exit_code})")

    def _notify_stopped(self, exit_code: int) -> None:
        """Notify daemon that agent has stopped.

        Args:
            exit_code: Exit code of the agent
        """
        import urllib.request

        try:
            data = {"exit_code": exit_code}

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/{self.agent_id}/stop",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            with urllib.request.urlopen(req, timeout=5.0) as response:
                result = json.loads(response.read().decode("utf-8"))
                logger.info(f"Agent stop notified: {result}")

        except Exception as e:
            logger.warning(f"Failed to notify agent stop: {e}")


def create_tracker(command: str) -> AgentTracker:
    """Create and start an agent tracker.

    Args:
        command: Command being executed

    Returns:
        AgentTracker instance
    """
    tracker = AgentTracker(command)
    tracker.start()
    return tracker

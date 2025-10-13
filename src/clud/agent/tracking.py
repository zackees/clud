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

    def __init__(self, command: str, agent_id: str | None = None) -> None:
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

        logger.debug(f"Initialized AgentTracker: agent_id={self.agent_id}")
        logger.debug(f"  Command: {self.command}")
        logger.debug(f"  CWD: {self.cwd}")
        logger.debug(f"  PID: {self.pid}")
        logger.debug(f"  Daemon URL: {self.daemon_url}")

    def start(self) -> bool:
        """Start tracking - ensure daemon and register agent.

        Returns:
            True if tracking started successfully, False otherwise
        """
        logger.debug("Starting agent tracker")
        logger.debug(f"Checking if daemon is running at {self.daemon_url}")

        # Ensure daemon is running
        if not ensure_daemon_running():
            logger.error("Failed to ensure daemon is running")
            return False

        logger.debug("Daemon is running, proceeding with registration")

        # Register with daemon
        if not self._register():
            logger.error("Failed to register with daemon")
            return False

        logger.debug("Registration successful, starting heartbeat")

        # Start heartbeat thread
        self._start_heartbeat()

        logger.info(f"Tracking enabled successfully (agent_id: {self.agent_id})")
        print(f"✓ Tracking enabled (agent_id: {self.agent_id})")
        return True

    def _register(self) -> bool:
        """Register agent with daemon.

        Returns:
            True if registration successful, False otherwise
        """
        import urllib.request

        logger.debug("Attempting to register agent with daemon")

        try:
            data: dict[str, str | int | dict[str, str]] = {
                "agent_id": self.agent_id,
                "cwd": self.cwd,
                "pid": self.pid,
                "command": self.command,
                "capabilities": {},
            }

            logger.debug(f"Registration data: {data}")

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/register",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            logger.debug(f"Sending POST request to {self.daemon_url}/agents/register")

            with urllib.request.urlopen(req, timeout=5.0) as response:
                response_data = response.read().decode("utf-8")
                logger.debug(f"Registration response: {response_data}")
                result = json.loads(response_data)
                self._registered = result.get("status") == "registered"
                logger.info(f"Agent registered successfully: {self.agent_id}")
                logger.debug(f"Registered status: {self._registered}")
                return True

        except Exception as e:
            logger.error(f"Failed to register agent: {e}", exc_info=True)
            return False

    def _start_heartbeat(self) -> None:
        """Start heartbeat thread."""
        logger.debug("Starting heartbeat thread")
        self._stop_heartbeat.clear()
        self._heartbeat_thread = threading.Thread(target=self._heartbeat_loop, daemon=True)
        self._heartbeat_thread.start()
        logger.debug("Heartbeat thread started")

    def _heartbeat_loop(self) -> None:
        """Heartbeat loop - sends status every 5 seconds."""
        logger.debug("Heartbeat loop starting")
        while not self._stop_heartbeat.is_set():
            logger.debug("Sending heartbeat")
            self._send_heartbeat()
            logger.debug("Heartbeat sent, waiting 5 seconds")
            self._stop_heartbeat.wait(5.0)  # 5 second interval
        logger.debug("Heartbeat loop stopped")

    def _send_heartbeat(self) -> None:
        """Send heartbeat to daemon."""
        import urllib.request

        try:
            data = {"status": self.status.value}

            logger.debug(f"Preparing heartbeat: status={self.status.value}")

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/{self.agent_id}/heartbeat",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            logger.debug(f"Sending heartbeat to {self.daemon_url}/agents/{self.agent_id}/heartbeat")

            with urllib.request.urlopen(req, timeout=5.0) as response:
                response_data = response.read().decode("utf-8")
                result = json.loads(response_data)
                logger.debug(f"Heartbeat response: {result}")

        except Exception as e:
            logger.warning(f"Failed to send heartbeat: {e}")

    def update_status(self, status: AgentStatus) -> None:
        """Update agent status.

        Args:
            status: New status
        """
        logger.debug(f"Updating status from {self.status.value} to {status.value}")
        self.status = status
        logger.info(f"Agent status updated: {status.value}")

    def stop(self, exit_code: int = 0) -> None:
        """Stop tracking and notify daemon.

        Args:
            exit_code: Exit code of the agent
        """
        logger.debug(f"Stopping agent tracker with exit_code={exit_code}")

        # Stop heartbeat thread
        if self._heartbeat_thread:
            logger.debug("Stopping heartbeat thread")
            self._stop_heartbeat.set()
            self._heartbeat_thread.join(timeout=2.0)
            logger.debug("Heartbeat thread stopped")

        # Notify daemon of stop
        if self._registered:
            logger.debug("Notifying daemon of agent stop")
            self._notify_stopped(exit_code)
        else:
            logger.debug("Agent not registered, skipping stop notification")

        logger.info(f"Agent tracking stopped (exit_code={exit_code})")

    def _notify_stopped(self, exit_code: int) -> None:
        """Notify daemon that agent has stopped.

        Args:
            exit_code: Exit code of the agent
        """
        import urllib.request

        logger.debug(f"Preparing to notify daemon of stop: exit_code={exit_code}")

        try:
            data = {"exit_code": exit_code}

            logger.debug(f"Stop notification data: {data}")

            req = urllib.request.Request(
                f"{self.daemon_url}/agents/{self.agent_id}/stop",
                data=json.dumps(data).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )

            logger.debug(f"Sending POST request to {self.daemon_url}/agents/{self.agent_id}/stop")

            with urllib.request.urlopen(req, timeout=5.0) as response:
                response_data = response.read().decode("utf-8")
                result = json.loads(response_data)
                logger.info(f"Agent stop notified successfully: {result}")
                logger.debug(f"Stop response: {result}")

        except Exception as e:
            logger.warning(f"Failed to notify agent stop: {e}", exc_info=True)


def create_tracker(command: str) -> AgentTracker:
    """Create and start an agent tracker.

    Args:
        command: Command being executed

    Returns:
        AgentTracker instance
    """
    logger.debug(f"Creating tracker for command: {command}")
    tracker = AgentTracker(command)
    logger.debug("Tracker created, starting...")
    tracker.start()
    logger.debug("Tracker started successfully")
    return tracker

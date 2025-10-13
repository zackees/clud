"""Client for daemon to communicate with clud-central."""

import logging
import platform
import socket
import threading
import uuid
from typing import Any

from .discovery import CentralConnection, ensure_central
from .models import AgentInfo

logger = logging.getLogger(__name__)


class CentralClient:
    """
    Client for daemon to communicate with clud-central.

    Responsibilities:
    - Register daemon with central
    - Forward agent registrations to central
    - Send heartbeats with agent status
    - Handle reconnection with exponential backoff
    """

    def __init__(self, daemon_id: str | None = None, daemon_port: int = 7565):
        """
        Initialize central client.

        Args:
            daemon_id: Daemon ID (generated if not provided)
            daemon_port: Daemon port for bind address
        """
        self.daemon_id = daemon_id or str(uuid.uuid4())
        self.daemon_port = daemon_port
        self.hostname = socket.gethostname()
        self.platform = platform.system().lower()
        self.central_conn: CentralConnection | None = None
        self._heartbeat_thread: threading.Thread | None = None
        self._stop_heartbeat = threading.Event()
        self._agent_registry_ref: Any = None  # Reference to agent registry

    def start(self, agent_registry: Any) -> bool:
        """
        Start central client.

        Args:
            agent_registry: Reference to agent registry for status updates

        Returns:
            True if started successfully, False otherwise
        """
        self._agent_registry_ref = agent_registry

        # Ensure central is available
        central_info = ensure_central()
        if not central_info:
            logger.warning("Central not available - daemon will run in offline mode")
            return False

        # Create connection
        self.central_conn = CentralConnection(central_info)

        # Register daemon with central
        if not self._register_daemon():
            logger.error("Failed to register daemon with central")
            self.central_conn.close()
            self.central_conn = None
            return False

        # Start heartbeat thread
        self._start_heartbeat()

        logger.info(f"Central client started (daemon_id: {self.daemon_id})")
        return True

    def _register_daemon(self) -> bool:
        """
        Register daemon with central.

        Returns:
            True if registration successful, False otherwise
        """
        if not self.central_conn:
            return False

        try:
            data: dict[str, object] = {
                "daemon_id": self.daemon_id,
                "hostname": self.hostname,
                "platform": self.platform,
                "version": "0.1.0",
                "bind_address": f"127.0.0.1:{self.daemon_port}",
            }

            response = self.central_conn.post_json("/api/v1/daemons/register", data)
            if response:
                logger.info(f"Daemon registered with central: {self.daemon_id}")
                return True
            else:
                logger.error("Failed to register daemon with central")
                return False

        except Exception as e:
            logger.error(f"Error registering daemon: {e}")
            return False

    def _start_heartbeat(self) -> None:
        """Start heartbeat thread."""
        self._stop_heartbeat.clear()
        self._heartbeat_thread = threading.Thread(target=self._heartbeat_loop, daemon=True)
        self._heartbeat_thread.start()

    def _heartbeat_loop(self) -> None:
        """Heartbeat loop - sends daemon and agent status every 30 seconds."""
        while not self._stop_heartbeat.is_set():
            self._send_heartbeat()
            self._stop_heartbeat.wait(30.0)  # 30 second interval

    def _send_heartbeat(self) -> None:
        """Send heartbeat to central with daemon and agent status."""
        if not self.central_conn or not self._agent_registry_ref:
            return

        try:
            from datetime import datetime

            # Get all agents from registry
            agents = self._agent_registry_ref.list_all()
            agent_data: list[dict[str, object]] = []

            for agent in agents:
                agent_data.append(
                    {
                        "agent_id": agent.agent_id,
                        "status": agent.status.value,
                        "last_heartbeat": datetime.fromtimestamp(agent.last_heartbeat).isoformat(),
                    }
                )

            data: dict[str, object] = {
                "daemon_id": self.daemon_id,
                "agent_count": len(agents),
                "agents": agent_data,
            }

            response = self.central_conn.post_json(f"/api/v1/daemons/{self.daemon_id}/heartbeat", data)
            if response:
                logger.debug(f"Heartbeat sent to central (agents: {len(agents)})")
            else:
                logger.warning("Failed to send heartbeat to central")

        except Exception as e:
            logger.warning(f"Error sending heartbeat: {e}")

    def register_agent(self, agent: AgentInfo) -> bool:
        """
        Register agent with central.

        Args:
            agent: Agent info to register

        Returns:
            True if registration successful, False otherwise
        """
        if not self.central_conn:
            logger.debug("Central not connected - skipping agent registration")
            return False

        try:
            data: dict[str, object] = {
                "agent_id": agent.agent_id,
                "daemon_id": self.daemon_id,
                "hostname": self.hostname,
                "pid": agent.pid,
                "cwd": agent.cwd,
                "command": agent.command,
                "status": agent.status.value,
                "capabilities": list(agent.capabilities.keys()) if agent.capabilities else [],
            }

            response = self.central_conn.post_json("/api/v1/agents/register", data)
            if response:
                logger.info(f"Agent registered with central: {agent.agent_id}")
                return True
            else:
                logger.warning(f"Failed to register agent with central: {agent.agent_id}")
                return False

        except Exception as e:
            logger.warning(f"Error registering agent with central: {e}")
            return False

    def update_agent_heartbeat(self, agent: AgentInfo) -> bool:
        """
        Send agent heartbeat to central.

        Args:
            agent: Agent info with updated status

        Returns:
            True if heartbeat sent successfully, False otherwise
        """
        if not self.central_conn:
            return False

        try:
            from datetime import datetime

            data: dict[str, object] = {
                "status": agent.status.value,
                "last_heartbeat": datetime.fromtimestamp(agent.last_heartbeat).isoformat(),
            }

            response = self.central_conn.post_json(f"/api/v1/agents/{agent.agent_id}/heartbeat", data)
            if response:
                logger.debug(f"Agent heartbeat sent to central: {agent.agent_id}")
                return True
            else:
                logger.debug(f"Failed to send agent heartbeat to central: {agent.agent_id}")
                return False

        except Exception as e:
            logger.debug(f"Error sending agent heartbeat: {e}")
            return False

    def notify_agent_stopped(self, agent_id: str, exit_code: int) -> bool:
        """
        Notify central that an agent has stopped.

        Args:
            agent_id: Agent ID
            exit_code: Agent exit code

        Returns:
            True if notification sent successfully, False otherwise
        """
        if not self.central_conn:
            return False

        try:
            data: dict[str, object] = {"exit_code": exit_code, "status": "stopped" if exit_code == 0 else "failed"}

            response = self.central_conn.post_json(f"/api/v1/agents/{agent_id}/stop", data)
            if response:
                logger.info(f"Agent stop notified to central: {agent_id}")
                return True
            else:
                logger.warning(f"Failed to notify agent stop to central: {agent_id}")
                return False

        except Exception as e:
            logger.warning(f"Error notifying agent stop: {e}")
            return False

    def stop(self) -> None:
        """Stop central client and cleanup."""
        # Stop heartbeat thread
        if self._heartbeat_thread:
            self._stop_heartbeat.set()
            self._heartbeat_thread.join(timeout=2.0)

        # Close connection
        if self.central_conn:
            self.central_conn.close()
            self.central_conn = None

        logger.info("Central client stopped")

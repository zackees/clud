"""Client for daemon to communicate with clud-cluster."""

import logging
import platform
import socket
import threading
import uuid
from typing import Any

from .discovery import ClusterConnection, ensure_cluster
from .models import AgentInfo

logger = logging.getLogger(__name__)


class ClusterClient:
    """
    Client for daemon to communicate with clud-cluster.

    Responsibilities:
    - Register daemon with cluster
    - Forward agent registrations to cluster
    - Send heartbeats with agent status
    - Handle reconnection with exponential backoff
    """

    def __init__(self, daemon_id: str | None = None, daemon_port: int = 7565) -> None:
        """
        Initialize cluster client.

        Args:
            daemon_id: Daemon ID (generated if not provided)
            daemon_port: Daemon port for bind address
        """
        self.daemon_id = daemon_id or str(uuid.uuid4())
        self.daemon_port = daemon_port
        self.hostname = socket.gethostname()
        self.platform = platform.system().lower()
        self.cluster_conn: ClusterConnection | None = None
        self._heartbeat_thread: threading.Thread | None = None
        self._stop_heartbeat = threading.Event()
        self._agent_registry_ref: Any = None  # Reference to agent registry

    def start(self, agent_registry: Any) -> bool:
        """
        Start cluster client.

        Args:
            agent_registry: Reference to agent registry for status updates

        Returns:
            True if started successfully, False otherwise
        """
        self._agent_registry_ref = agent_registry

        # Ensure cluster is available
        cluster_info = ensure_cluster()
        if not cluster_info:
            logger.warning("Cluster not available - daemon will run in offline mode")
            return False

        # Create connection
        self.cluster_conn = ClusterConnection(cluster_info)

        # Register daemon with cluster
        if not self._register_daemon():
            logger.error("Failed to register daemon with cluster")
            self.cluster_conn.close()
            self.cluster_conn = None
            return False

        # Start heartbeat thread
        self._start_heartbeat()

        logger.info(f"Cluster client started (daemon_id: {self.daemon_id})")
        return True

    def _register_daemon(self) -> bool:
        """
        Register daemon with cluster.

        Returns:
            True if registration successful, False otherwise
        """
        if not self.cluster_conn:
            return False

        try:
            data: dict[str, object] = {
                "daemon_id": self.daemon_id,
                "hostname": self.hostname,
                "platform": self.platform,
                "version": "0.1.0",
                "bind_address": f"127.0.0.1:{self.daemon_port}",
            }

            response = self.cluster_conn.post_json("/api/v1/daemons/register", data)
            if response:
                logger.info(f"Daemon registered with cluster: {self.daemon_id}")
                return True
            else:
                logger.error("Failed to register daemon with cluster")
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
        """Send heartbeat to cluster with daemon and agent status."""
        if not self.cluster_conn or not self._agent_registry_ref:
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

            response = self.cluster_conn.post_json(f"/api/v1/daemons/{self.daemon_id}/heartbeat", data)
            if response:
                logger.debug(f"Heartbeat sent to cluster (agents: {len(agents)})")
            else:
                logger.warning("Failed to send heartbeat to cluster")

        except Exception as e:
            logger.warning(f"Error sending heartbeat: {e}")

    def register_agent(self, agent: AgentInfo) -> bool:
        """
        Register agent with cluster.

        Args:
            agent: Agent info to register

        Returns:
            True if registration successful, False otherwise
        """
        if not self.cluster_conn:
            logger.debug("Cluster not connected - skipping agent registration")
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

            response = self.cluster_conn.post_json("/api/v1/agents/register", data)
            if response:
                logger.info(f"Agent registered with cluster: {agent.agent_id}")
                return True
            else:
                logger.warning(f"Failed to register agent with cluster: {agent.agent_id}")
                return False

        except Exception as e:
            logger.warning(f"Error registering agent with cluster: {e}")
            return False

    def update_agent_heartbeat(self, agent: AgentInfo) -> bool:
        """
        Send agent heartbeat to cluster.

        Args:
            agent: Agent info with updated status

        Returns:
            True if heartbeat sent successfully, False otherwise
        """
        if not self.cluster_conn:
            return False

        try:
            from datetime import datetime

            data: dict[str, object] = {
                "status": agent.status.value,
                "last_heartbeat": datetime.fromtimestamp(agent.last_heartbeat).isoformat(),
            }

            response = self.cluster_conn.post_json(f"/api/v1/agents/{agent.agent_id}/heartbeat", data)
            if response:
                logger.debug(f"Agent heartbeat sent to cluster: {agent.agent_id}")
                return True
            else:
                logger.debug(f"Failed to send agent heartbeat to cluster: {agent.agent_id}")
                return False

        except Exception as e:
            logger.debug(f"Error sending agent heartbeat: {e}")
            return False

    def notify_agent_stopped(self, agent_id: str, exit_code: int) -> bool:
        """
        Notify cluster that an agent has stopped.

        Args:
            agent_id: Agent ID
            exit_code: Agent exit code

        Returns:
            True if notification sent successfully, False otherwise
        """
        if not self.cluster_conn:
            return False

        try:
            data: dict[str, object] = {"exit_code": exit_code, "status": "stopped" if exit_code == 0 else "failed"}

            response = self.cluster_conn.post_json(f"/api/v1/agents/{agent_id}/stop", data)
            if response:
                logger.info(f"Agent stop notified to cluster: {agent_id}")
                return True
            else:
                logger.warning(f"Failed to notify agent stop to cluster: {agent_id}")
                return False

        except Exception as e:
            logger.warning(f"Error notifying agent stop: {e}")
            return False

    def stop(self) -> None:
        """Stop cluster client and cleanup."""
        # Stop heartbeat thread
        if self._heartbeat_thread:
            self._stop_heartbeat.set()
            self._heartbeat_thread.join(timeout=2.0)

        # Close connection
        if self.cluster_conn:
            self.cluster_conn.close()
            self.cluster_conn = None

        logger.info("Cluster client stopped")

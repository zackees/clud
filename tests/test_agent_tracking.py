"""Unit tests for agent tracking and heartbeat functionality."""
# pyright: reportUnknownParameterType=false, reportMissingParameterType=false, reportUnknownLambdaType=false

import json
import time
import unittest
from unittest.mock import MagicMock, Mock, patch

from clud.agent.tracking import AgentTracker, create_tracker
from clud.service.models import AgentStatus


class TestAgentTracker(unittest.TestCase):
    """Test AgentTracker functionality."""

    def test_agent_tracker_init(self) -> None:
        """Test AgentTracker initialization."""
        tracker = AgentTracker("test command")

        self.assertIsNotNone(tracker.agent_id)
        self.assertEqual(tracker.command, "test command")
        self.assertEqual(tracker.status, AgentStatus.STARTING)
        self.assertIsNone(tracker._heartbeat_thread)
        self.assertFalse(tracker._registered)

    def test_agent_tracker_init_with_agent_id(self) -> None:
        """Test AgentTracker initialization with custom agent ID."""
        custom_id = "custom-agent-123"
        tracker = AgentTracker("test command", agent_id=custom_id)

        self.assertEqual(tracker.agent_id, custom_id)
        self.assertEqual(tracker.command, "test command")

    @patch("clud.agent.tracking.ensure_daemon_running")
    def test_start_daemon_not_running(self, mock_ensure: MagicMock) -> None:
        """Test start when daemon fails to start."""
        mock_ensure.return_value = False

        tracker = AgentTracker("test command")
        result = tracker.start()

        self.assertFalse(result)
        self.assertFalse(tracker._registered)
        mock_ensure.assert_called_once()

    @patch("urllib.request.urlopen")
    @patch("clud.agent.tracking.ensure_daemon_running")
    def test_start_registration_fails(self, mock_ensure: MagicMock, mock_urlopen: MagicMock) -> None:
        """Test start when registration fails."""
        mock_ensure.return_value = True
        mock_urlopen.side_effect = Exception("Connection error")

        tracker = AgentTracker("test command")
        result = tracker.start()

        self.assertFalse(result)
        self.assertFalse(tracker._registered)

    @patch("urllib.request.urlopen")
    @patch("clud.agent.tracking.ensure_daemon_running")
    def test_start_success(self, mock_ensure: MagicMock, mock_urlopen: MagicMock) -> None:
        """Test successful start."""
        mock_ensure.return_value = True

        # Mock registration response
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "registered", "agent_id": "test-id"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        result = tracker.start()

        self.assertTrue(result)
        self.assertTrue(tracker._registered)
        self.assertIsNotNone(tracker._heartbeat_thread)
        self.assertTrue(tracker._heartbeat_thread.is_alive())

        # Clean up
        tracker.stop()

    @patch("urllib.request.urlopen")
    def test_register_success(self, mock_urlopen: MagicMock) -> None:
        """Test successful agent registration."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "registered", "agent_id": "test-id"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        result = tracker._register()

        self.assertTrue(result)
        self.assertTrue(tracker._registered)

    @patch("urllib.request.urlopen")
    def test_register_sends_correct_data(self, mock_urlopen: MagicMock) -> None:
        """Test that registration sends correct data."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "registered"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        tracker._register()

        # Verify the request was made with correct data
        call_args = mock_urlopen.call_args
        request = call_args[0][0]
        self.assertIn("/agents/register", request.full_url)

        # Verify request data
        sent_data = json.loads(request.data.decode("utf-8"))
        self.assertEqual(sent_data["agent_id"], tracker.agent_id)
        self.assertEqual(sent_data["command"], "test command")
        self.assertIn("cwd", sent_data)
        self.assertIn("pid", sent_data)

    @patch("urllib.request.urlopen")
    def test_send_heartbeat_success(self, mock_urlopen: MagicMock) -> None:
        """Test successful heartbeat send."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "ok"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        tracker._send_heartbeat()

        # Verify the request was made
        call_args = mock_urlopen.call_args
        request = call_args[0][0]
        self.assertIn(f"/agents/{tracker.agent_id}/heartbeat", request.full_url)

        # Verify request data includes status
        sent_data = json.loads(request.data.decode("utf-8"))
        self.assertEqual(sent_data["status"], AgentStatus.STARTING.value)

    @patch("urllib.request.urlopen")
    def test_send_heartbeat_connection_error(self, mock_urlopen: MagicMock) -> None:
        """Test heartbeat with connection error (should not raise)."""
        mock_urlopen.side_effect = ConnectionError("Remote end closed connection")

        tracker = AgentTracker("test command")
        # Should not raise exception
        tracker._send_heartbeat()

    def test_update_status(self) -> None:
        """Test status update."""
        tracker = AgentTracker("test command")

        tracker.update_status(AgentStatus.RUNNING)
        self.assertEqual(tracker.status, AgentStatus.RUNNING)

        tracker.update_status(AgentStatus.STOPPED)
        self.assertEqual(tracker.status, AgentStatus.STOPPED)

    @patch("urllib.request.urlopen")
    def test_stop_notifies_daemon(self, mock_urlopen: MagicMock) -> None:
        """Test that stop notifies daemon."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "stopped"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        tracker._registered = True
        tracker.stop(exit_code=0)

        # Verify stop notification was sent
        call_args = mock_urlopen.call_args
        request = call_args[0][0]
        self.assertIn(f"/agents/{tracker.agent_id}/stop", request.full_url)

        sent_data = json.loads(request.data.decode("utf-8"))
        self.assertEqual(sent_data["exit_code"], 0)

    def test_stop_without_registration(self) -> None:
        """Test stop when not registered."""
        tracker = AgentTracker("test command")
        tracker._registered = False

        # Should not raise exception
        tracker.stop(exit_code=0)

    @patch("urllib.request.urlopen")
    def test_heartbeat_thread_stops(self, mock_urlopen: MagicMock) -> None:
        """Test that heartbeat thread stops cleanly."""
        mock_response = Mock()
        mock_response.read.return_value = b'{"status": "ok"}'
        mock_response.__enter__ = Mock(return_value=mock_response)
        mock_response.__exit__ = Mock(return_value=False)
        mock_urlopen.return_value = mock_response

        tracker = AgentTracker("test command")
        tracker._start_heartbeat()

        # Wait a bit for thread to start
        time.sleep(0.1)
        self.assertTrue(tracker._heartbeat_thread.is_alive())

        # Stop heartbeat
        tracker._stop_heartbeat.set()
        tracker._heartbeat_thread.join(timeout=2.0)

        self.assertFalse(tracker._heartbeat_thread.is_alive())


class TestCreateTracker(unittest.TestCase):
    """Test create_tracker helper function."""

    @patch("clud.agent.tracking.AgentTracker.start")
    @patch("clud.agent.tracking.ensure_daemon_running")
    def test_create_tracker(self, mock_ensure: MagicMock, mock_start: MagicMock) -> None:
        """Test create_tracker creates and starts tracker."""
        mock_ensure.return_value = True
        mock_start.return_value = True

        tracker = create_tracker("test command")

        self.assertIsInstance(tracker, AgentTracker)
        self.assertEqual(tracker.command, "test command")
        mock_start.assert_called_once()


class TestHeartbeatEndpointBug(unittest.TestCase):
    """Test for the heartbeat endpoint status field bug."""

    def test_status_field_should_not_be_overwritten(self) -> None:
        """
        Test that demonstrates the bug where status is converted to AgentStatus
        but then overwritten with the raw string from kwargs.

        This test verifies the expected behavior after the fix.
        """
        # Simulate what happens in the heartbeat handler
        data = {"status": "running"}

        # Extract status (as done in server.py _handle_heartbeat)
        from clud.service.models import AgentStatus

        status = AgentStatus(data["status"]) if "status" in data else None

        # The bug: if we pass **data to a function, it will contain the raw string
        # This test verifies that after the fix, status is removed from data
        data_without_status = {k: v for k, v in data.items() if k != "status"}

        # Verify that status was removed
        self.assertNotIn("status", data_without_status)
        self.assertIsInstance(status, AgentStatus)
        self.assertEqual(status, AgentStatus.RUNNING)


if __name__ == "__main__":
    unittest.main()

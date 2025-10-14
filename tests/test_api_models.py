"""Unit tests for API models."""

import unittest
from datetime import datetime

from clud.api.models import (
    ClientType,
    ExecutionResult,
    ExecutionStatus,
    InstanceInfo,
    MessageRequest,
    MessageResponse,
)


class TestClientType(unittest.TestCase):
    """Tests for ClientType enum."""

    def test_client_type_values(self) -> None:
        """Test that ClientType has all expected values."""
        self.assertEqual(ClientType.API, "api")
        self.assertEqual(ClientType.TELEGRAM, "telegram")
        self.assertEqual(ClientType.WEB, "web")
        self.assertEqual(ClientType.WEBHOOK, "webhook")


class TestExecutionStatus(unittest.TestCase):
    """Tests for ExecutionStatus enum."""

    def test_execution_status_values(self) -> None:
        """Test that ExecutionStatus has all expected values."""
        self.assertEqual(ExecutionStatus.PENDING, "pending")
        self.assertEqual(ExecutionStatus.RUNNING, "running")
        self.assertEqual(ExecutionStatus.COMPLETED, "completed")
        self.assertEqual(ExecutionStatus.FAILED, "failed")


class TestMessageRequest(unittest.TestCase):
    """Tests for MessageRequest dataclass."""

    def test_message_request_creation(self) -> None:
        """Test creating a MessageRequest with required fields."""
        request = MessageRequest(
            message="test message",
            session_id="session-123",
            client_type=ClientType.API,
            client_id="client-456",
        )

        self.assertEqual(request.message, "test message")
        self.assertEqual(request.session_id, "session-123")
        self.assertEqual(request.client_type, ClientType.API)
        self.assertEqual(request.client_id, "client-456")
        self.assertIsNone(request.working_directory)
        self.assertEqual(request.metadata, {})

    def test_message_request_with_optional_fields(self) -> None:
        """Test creating a MessageRequest with optional fields."""
        request = MessageRequest(
            message="test message",
            session_id="session-123",
            client_type=ClientType.TELEGRAM,
            client_id="client-456",
            working_directory="/home/user/project",
            metadata={"key": "value"},
        )

        self.assertEqual(request.working_directory, "/home/user/project")
        self.assertEqual(request.metadata, {"key": "value"})

    def test_message_request_to_dict(self) -> None:
        """Test converting MessageRequest to dictionary."""
        request = MessageRequest(
            message="test message",
            session_id="session-123",
            client_type=ClientType.WEB,
            client_id="client-456",
            working_directory="/path/to/dir",
            metadata={"foo": "bar"},
        )

        result = request.to_dict()

        self.assertEqual(result["message"], "test message")
        self.assertEqual(result["session_id"], "session-123")
        self.assertEqual(result["client_type"], "web")
        self.assertEqual(result["client_id"], "client-456")
        self.assertEqual(result["working_directory"], "/path/to/dir")
        self.assertEqual(result["metadata"], {"foo": "bar"})

    def test_message_request_from_dict(self) -> None:
        """Test creating MessageRequest from dictionary."""
        data = {
            "message": "test message",
            "session_id": "session-123",
            "client_type": "telegram",
            "client_id": "client-456",
            "working_directory": "/path/to/dir",
            "metadata": {"key": "value"},
        }

        request = MessageRequest.from_dict(data)

        self.assertEqual(request.message, "test message")
        self.assertEqual(request.session_id, "session-123")
        self.assertEqual(request.client_type, ClientType.TELEGRAM)
        self.assertEqual(request.client_id, "client-456")
        self.assertEqual(request.working_directory, "/path/to/dir")
        self.assertEqual(request.metadata, {"key": "value"})

    def test_message_request_validate_success(self) -> None:
        """Test validation of a valid MessageRequest."""
        request = MessageRequest(
            message="test message",
            session_id="session-123",
            client_type=ClientType.API,
            client_id="client-456",
        )

        is_valid, error = request.validate()

        self.assertTrue(is_valid)
        self.assertIsNone(error)

    def test_message_request_validate_empty_message(self) -> None:
        """Test validation with empty message."""
        request = MessageRequest(
            message="",
            session_id="session-123",
            client_type=ClientType.API,
            client_id="client-456",
        )

        is_valid, error = request.validate()

        self.assertFalse(is_valid)
        self.assertEqual(error, "Message cannot be empty")

    def test_message_request_validate_whitespace_message(self) -> None:
        """Test validation with whitespace-only message."""
        request = MessageRequest(
            message="   ",
            session_id="session-123",
            client_type=ClientType.API,
            client_id="client-456",
        )

        is_valid, error = request.validate()

        self.assertFalse(is_valid)
        self.assertEqual(error, "Message cannot be empty")

    def test_message_request_validate_empty_session_id(self) -> None:
        """Test validation with empty session_id."""
        request = MessageRequest(
            message="test message",
            session_id="",
            client_type=ClientType.API,
            client_id="client-456",
        )

        is_valid, error = request.validate()

        self.assertFalse(is_valid)
        self.assertEqual(error, "Session ID cannot be empty")

    def test_message_request_validate_empty_client_id(self) -> None:
        """Test validation with empty client_id."""
        request = MessageRequest(
            message="test message",
            session_id="session-123",
            client_type=ClientType.API,
            client_id="",
        )

        is_valid, error = request.validate()

        self.assertFalse(is_valid)
        self.assertEqual(error, "Client ID cannot be empty")


class TestMessageResponse(unittest.TestCase):
    """Tests for MessageResponse dataclass."""

    def test_message_response_creation(self) -> None:
        """Test creating a MessageResponse."""
        response = MessageResponse(
            instance_id="instance-123",
            session_id="session-456",
            status=ExecutionStatus.COMPLETED,
        )

        self.assertEqual(response.instance_id, "instance-123")
        self.assertEqual(response.session_id, "session-456")
        self.assertEqual(response.status, ExecutionStatus.COMPLETED)
        self.assertIsNone(response.message)
        self.assertIsNone(response.error)
        self.assertEqual(response.metadata, {})

    def test_message_response_with_optional_fields(self) -> None:
        """Test creating a MessageResponse with optional fields."""
        response = MessageResponse(
            instance_id="instance-123",
            session_id="session-456",
            status=ExecutionStatus.FAILED,
            message="error occurred",
            error="detailed error message",
            metadata={"exit_code": 1},
        )

        self.assertEqual(response.message, "error occurred")
        self.assertEqual(response.error, "detailed error message")
        self.assertEqual(response.metadata, {"exit_code": 1})

    def test_message_response_to_dict(self) -> None:
        """Test converting MessageResponse to dictionary."""
        response = MessageResponse(
            instance_id="instance-123",
            session_id="session-456",
            status=ExecutionStatus.RUNNING,
            message="processing",
            metadata={"progress": 50},
        )

        result = response.to_dict()

        self.assertEqual(result["instance_id"], "instance-123")
        self.assertEqual(result["session_id"], "session-456")
        self.assertEqual(result["status"], "running")
        self.assertEqual(result["message"], "processing")
        self.assertIsNone(result["error"])
        self.assertEqual(result["metadata"], {"progress": 50})

    def test_message_response_from_dict(self) -> None:
        """Test creating MessageResponse from dictionary."""
        data = {
            "instance_id": "instance-123",
            "session_id": "session-456",
            "status": "completed",
            "message": "success",
            "error": None,
            "metadata": {"result": "ok"},
        }

        response = MessageResponse.from_dict(data)

        self.assertEqual(response.instance_id, "instance-123")
        self.assertEqual(response.session_id, "session-456")
        self.assertEqual(response.status, ExecutionStatus.COMPLETED)
        self.assertEqual(response.message, "success")
        self.assertIsNone(response.error)
        self.assertEqual(response.metadata, {"result": "ok"})


class TestInstanceInfo(unittest.TestCase):
    """Tests for InstanceInfo dataclass."""

    def test_instance_info_creation(self) -> None:
        """Test creating an InstanceInfo."""
        now = datetime.now()
        info = InstanceInfo(
            instance_id="instance-123",
            session_id="session-456",
            client_type=ClientType.API,
            client_id="client-789",
            status=ExecutionStatus.RUNNING,
            created_at=now,
            last_activity=now,
        )

        self.assertEqual(info.instance_id, "instance-123")
        self.assertEqual(info.session_id, "session-456")
        self.assertEqual(info.client_type, ClientType.API)
        self.assertEqual(info.client_id, "client-789")
        self.assertEqual(info.status, ExecutionStatus.RUNNING)
        self.assertEqual(info.created_at, now)
        self.assertEqual(info.last_activity, now)
        self.assertIsNone(info.working_directory)
        self.assertEqual(info.message_count, 0)
        self.assertEqual(info.metadata, {})

    def test_instance_info_with_optional_fields(self) -> None:
        """Test creating an InstanceInfo with optional fields."""
        now = datetime.now()
        info = InstanceInfo(
            instance_id="instance-123",
            session_id="session-456",
            client_type=ClientType.TELEGRAM,
            client_id="client-789",
            status=ExecutionStatus.COMPLETED,
            created_at=now,
            last_activity=now,
            working_directory="/home/user/project",
            message_count=5,
            metadata={"custom": "data"},
        )

        self.assertEqual(info.working_directory, "/home/user/project")
        self.assertEqual(info.message_count, 5)
        self.assertEqual(info.metadata, {"custom": "data"})

    def test_instance_info_to_dict(self) -> None:
        """Test converting InstanceInfo to dictionary."""
        now = datetime.now()
        info = InstanceInfo(
            instance_id="instance-123",
            session_id="session-456",
            client_type=ClientType.WEB,
            client_id="client-789",
            status=ExecutionStatus.RUNNING,
            created_at=now,
            last_activity=now,
            working_directory="/path/to/dir",
            message_count=3,
            metadata={"key": "value"},
        )

        result = info.to_dict()

        self.assertEqual(result["instance_id"], "instance-123")
        self.assertEqual(result["session_id"], "session-456")
        self.assertEqual(result["client_type"], "web")
        self.assertEqual(result["client_id"], "client-789")
        self.assertEqual(result["status"], "running")
        self.assertEqual(result["created_at"], now.isoformat())
        self.assertEqual(result["last_activity"], now.isoformat())
        self.assertEqual(result["working_directory"], "/path/to/dir")
        self.assertEqual(result["message_count"], 3)
        self.assertEqual(result["metadata"], {"key": "value"})

    def test_instance_info_from_dict(self) -> None:
        """Test creating InstanceInfo from dictionary."""
        now = datetime.now()
        data = {
            "instance_id": "instance-123",
            "session_id": "session-456",
            "client_type": "telegram",
            "client_id": "client-789",
            "status": "completed",
            "created_at": now.isoformat(),
            "last_activity": now.isoformat(),
            "working_directory": "/path/to/dir",
            "message_count": 7,
            "metadata": {"foo": "bar"},
        }

        info = InstanceInfo.from_dict(data)

        self.assertEqual(info.instance_id, "instance-123")
        self.assertEqual(info.session_id, "session-456")
        self.assertEqual(info.client_type, ClientType.TELEGRAM)
        self.assertEqual(info.client_id, "client-789")
        self.assertEqual(info.status, ExecutionStatus.COMPLETED)
        self.assertEqual(info.working_directory, "/path/to/dir")
        self.assertEqual(info.message_count, 7)
        self.assertEqual(info.metadata, {"foo": "bar"})


class TestExecutionResult(unittest.TestCase):
    """Tests for ExecutionResult dataclass."""

    def test_execution_result_creation(self) -> None:
        """Test creating an ExecutionResult."""
        result = ExecutionResult(
            instance_id="instance-123",
            status=ExecutionStatus.COMPLETED,
        )

        self.assertEqual(result.instance_id, "instance-123")
        self.assertEqual(result.status, ExecutionStatus.COMPLETED)
        self.assertIsNone(result.output)
        self.assertIsNone(result.error)
        self.assertIsNone(result.exit_code)
        self.assertEqual(result.metadata, {})

    def test_execution_result_with_optional_fields(self) -> None:
        """Test creating an ExecutionResult with optional fields."""
        result = ExecutionResult(
            instance_id="instance-123",
            status=ExecutionStatus.FAILED,
            output="some output",
            error="error message",
            exit_code=1,
            metadata={"duration": 5.2},
        )

        self.assertEqual(result.output, "some output")
        self.assertEqual(result.error, "error message")
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.metadata, {"duration": 5.2})

    def test_execution_result_to_dict(self) -> None:
        """Test converting ExecutionResult to dictionary."""
        result = ExecutionResult(
            instance_id="instance-123",
            status=ExecutionStatus.COMPLETED,
            output="command output",
            exit_code=0,
            metadata={"time": 1.5},
        )

        result_dict = result.to_dict()

        self.assertEqual(result_dict["instance_id"], "instance-123")
        self.assertEqual(result_dict["status"], "completed")
        self.assertEqual(result_dict["output"], "command output")
        self.assertIsNone(result_dict["error"])
        self.assertEqual(result_dict["exit_code"], 0)
        self.assertEqual(result_dict["metadata"], {"time": 1.5})

    def test_execution_result_from_dict(self) -> None:
        """Test creating ExecutionResult from dictionary."""
        data = {
            "instance_id": "instance-123",
            "status": "failed",
            "output": "partial output",
            "error": "execution failed",
            "exit_code": 2,
            "metadata": {"attempts": 3},
        }

        result = ExecutionResult.from_dict(data)

        self.assertEqual(result.instance_id, "instance-123")
        self.assertEqual(result.status, ExecutionStatus.FAILED)
        self.assertEqual(result.output, "partial output")
        self.assertEqual(result.error, "execution failed")
        self.assertEqual(result.exit_code, 2)
        self.assertEqual(result.metadata, {"attempts": 3})


if __name__ == "__main__":
    unittest.main()

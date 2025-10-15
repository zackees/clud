"""Unit tests for telegram/session_manager.py."""

# pyright: reportUnknownMemberType=false, reportUnknownVariableType=false, reportAttributeAccessIssue=false
# Mock objects from unittest.mock have incomplete type stubs

from datetime import datetime, timedelta
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from clud.api.instance_manager import CludInstance, InstancePool
from clud.telegram.models import TelegramMessage, TelegramSession
from clud.telegram.session_manager import SessionManager

pytestmark = pytest.mark.anyio


@pytest.fixture
def instance_pool() -> InstancePool:
    """Create mock instance pool."""
    pool = MagicMock(spec=InstancePool)
    pool.get_or_create_instance = AsyncMock()
    pool.get_session_instance = MagicMock()
    pool.delete_instance = AsyncMock()
    return pool


@pytest.fixture
def mock_instance() -> CludInstance:
    """Create mock clud instance."""
    instance = MagicMock(spec=CludInstance)
    instance.instance_id = "test-instance-id"
    instance.execute = AsyncMock(return_value={"output": "Test response", "exit_code": 0})
    return instance


@pytest.fixture
def session_manager(instance_pool: InstancePool, mock_instance: CludInstance) -> SessionManager:
    """Create session manager with mocked dependencies."""
    instance_pool.get_or_create_instance.return_value = mock_instance
    instance_pool.get_session_instance.return_value = mock_instance
    return SessionManager(
        instance_pool=instance_pool,
        max_sessions=10,
        session_timeout_seconds=3600,
        message_history_limit=100,
    )


def test_init(session_manager: SessionManager) -> None:
    """Test SessionManager initialization."""
    assert session_manager.max_sessions == 10
    assert session_manager.session_timeout_seconds == 3600
    assert session_manager.message_history_limit == 100
    assert len(session_manager.sessions) == 0
    assert len(session_manager.user_to_session) == 0
    assert len(session_manager.web_clients) == 0


async def test_get_or_create_session_new(session_manager: SessionManager) -> None:
    """Test creating a new session."""
    session = await session_manager.get_or_create_session(
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
        telegram_last_name="User",
    )

    # Verify session created
    assert isinstance(session, TelegramSession)
    assert session.telegram_user_id == 12345
    assert session.telegram_username == "testuser"
    assert session.telegram_first_name == "Test"
    assert session.telegram_last_name == "User"
    assert session.instance_id == "test-instance-id"

    # Verify storage
    assert len(session_manager.sessions) == 1
    assert session.session_id in session_manager.sessions
    assert 12345 in session_manager.user_to_session
    assert session.session_id in session_manager.web_clients


async def test_get_or_create_session_existing(session_manager: SessionManager) -> None:
    """Test getting an existing session."""
    # Create initial session
    session1 = await session_manager.get_or_create_session(
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
        telegram_last_name="User",
    )

    # Get same session again
    session2 = await session_manager.get_or_create_session(
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
        telegram_last_name="User",
    )

    # Verify same session returned
    assert session1.session_id == session2.session_id
    assert len(session_manager.sessions) == 1


async def test_get_or_create_session_max_limit(session_manager: SessionManager) -> None:
    """Test max sessions limit."""
    # Create max sessions
    for i in range(10):
        await session_manager.get_or_create_session(
            telegram_user_id=i,
            telegram_username=f"user{i}",
            telegram_first_name=f"User{i}",
        )

    # Try to create one more (should fail)
    with pytest.raises(RuntimeError, match="Maximum session limit reached"):
        await session_manager.get_or_create_session(
            telegram_user_id=999,
            telegram_username="toomany",
            telegram_first_name="Too",
        )


async def test_get_or_create_session_with_working_directory(session_manager: SessionManager, instance_pool: InstancePool) -> None:
    """Test creating session with working directory."""
    working_dir = "/tmp/test"
    _session = await session_manager.get_or_create_session(
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
        working_directory=working_dir,
    )

    # Verify instance pool called with Path
    call_args = instance_pool.get_or_create_instance.call_args
    assert call_args.kwargs["working_directory"] == Path(working_dir)


def test_get_session(session_manager: SessionManager) -> None:
    """Test getting session by ID."""
    # Create a session manually
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session

    # Get by ID
    result = session_manager.get_session("test-session-id")
    assert result == session

    # Get non-existent
    result = session_manager.get_session("non-existent")
    assert result is None


def test_get_user_session(session_manager: SessionManager) -> None:
    """Test getting session by user ID."""
    # Create a session manually
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session
    session_manager.user_to_session[12345] = session.session_id

    # Get by user ID
    result = session_manager.get_user_session(12345)
    assert result == session

    # Get non-existent
    result = session_manager.get_user_session(99999)
    assert result is None


def test_get_all_sessions(session_manager: SessionManager) -> None:
    """Test getting all sessions."""
    # Create multiple sessions
    sessions = []
    for i in range(3):
        session = TelegramSession(
            session_id=f"session-{i}",
            telegram_user_id=i,
            telegram_username=f"user{i}",
            telegram_first_name=f"User{i}",
        )
        session_manager.sessions[session.session_id] = session
        sessions.append(session)

    # Get all
    result = session_manager.get_all_sessions()
    assert len(result) == 3
    # Compare session IDs instead of objects
    assert {s.session_id for s in result} == {s.session_id for s in sessions}


async def test_add_message(session_manager: SessionManager) -> None:
    """Test adding a message to session history."""
    # Create a session
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session
    session_manager.web_clients[session.session_id] = set()

    # Create a message
    message = TelegramMessage.create_user_message(
        session_id=session.session_id,
        telegram_message_id=1,
        content="Hello",
    )

    # Add message
    await session_manager.add_message(session.session_id, message)

    # Verify message added
    assert len(session.message_history) == 1
    assert session.message_history[0] == message


async def test_add_message_session_not_found(session_manager: SessionManager) -> None:
    """Test adding message to non-existent session."""
    message = TelegramMessage.create_user_message(
        session_id="non-existent",
        telegram_message_id=1,
        content="Hello",
    )

    with pytest.raises(ValueError, match="Session non-existent not found"):
        await session_manager.add_message("non-existent", message)


async def test_add_message_history_limit(session_manager: SessionManager) -> None:
    """Test message history trimming."""
    # Create a session with 5 existing messages
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session
    session_manager.web_clients[session.session_id] = set()
    session_manager.message_history_limit = 3

    # Add 5 messages
    for i in range(5):
        message = TelegramMessage.create_user_message(
            session_id=session.session_id,
            telegram_message_id=i,
            content=f"Message {i}",
        )
        await session_manager.add_message(session.session_id, message)

    # Verify only last 3 messages kept
    assert len(session.message_history) == 3
    assert session.message_history[0].content == "Message 2"
    assert session.message_history[2].content == "Message 4"


async def test_process_user_message(session_manager: SessionManager, mock_instance: CludInstance) -> None:
    """Test processing a user message."""
    # Create a session
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session.instance_id = "test-instance-id"
    session_manager.sessions[session.session_id] = session
    session_manager.web_clients[session.session_id] = set()

    # Process message
    response = await session_manager.process_user_message(
        session_id=session.session_id,
        message_content="Hello bot",
        telegram_message_id=1,
    )

    # Verify response
    assert response == "Test response"

    # Verify messages added (user + bot)
    assert len(session.message_history) == 2
    assert session.message_history[0].sender == "user"
    assert session.message_history[0].content == "Hello bot"
    assert session.message_history[1].sender == "bot"
    assert session.message_history[1].content == "Test response"

    # Verify instance executed
    mock_instance.execute.assert_called_once_with("Hello bot")


async def test_process_user_message_error(session_manager: SessionManager, mock_instance: CludInstance) -> None:
    """Test processing message with execution error."""
    # Create a session
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session.instance_id = "test-instance-id"
    session_manager.sessions[session.session_id] = session
    session_manager.web_clients[session.session_id] = set()

    # Make instance execution fail
    mock_instance.execute.side_effect = Exception("Execution failed")

    # Process message (should raise RuntimeError)
    with pytest.raises(RuntimeError, match="Failed to process message"):
        await session_manager.process_user_message(
            session_id=session.session_id,
            message_content="Hello bot",
            telegram_message_id=1,
        )

    # Verify error message added to history
    assert len(session.message_history) == 2
    assert "error occurred" in session.message_history[1].content.lower()


async def test_register_web_client(session_manager: SessionManager) -> None:
    """Test registering a web client."""
    # Create a session
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session
    session_manager.web_clients[session.session_id] = set()

    # Create mock websocket
    websocket = AsyncMock()
    websocket.send_json = AsyncMock()

    # Register
    await session_manager.register_web_client(session.session_id, websocket)

    # Verify added
    assert websocket in session_manager.web_clients[session.session_id]
    assert session.web_client_count == 1

    # Verify confirmation sent
    assert websocket.send_json.call_count == 2  # connected + history


async def test_unregister_web_client(session_manager: SessionManager) -> None:
    """Test unregistering a web client."""
    # Create a session with a web client
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session_manager.sessions[session.session_id] = session

    websocket = AsyncMock()
    session_manager.web_clients[session.session_id] = {websocket}
    session.web_client_count = 1

    # Unregister
    await session_manager.unregister_web_client(session.session_id, websocket)

    # Verify removed
    assert websocket not in session_manager.web_clients[session.session_id]
    assert session.web_client_count == 0


async def test_cleanup_idle_sessions(session_manager: SessionManager) -> None:
    """Test cleaning up idle sessions."""
    # Create sessions with different activity times
    old_session = TelegramSession(
        session_id="old-session",
        telegram_user_id=1,
        telegram_username="old",
        telegram_first_name="Old",
    )
    old_session.last_activity = datetime.now() - timedelta(seconds=7200)  # 2 hours ago
    old_session.instance_id = "old-instance"

    new_session = TelegramSession(
        session_id="new-session",
        telegram_user_id=2,
        telegram_username="new",
        telegram_first_name="New",
    )
    new_session.last_activity = datetime.now()  # Just now
    new_session.instance_id = "new-instance"

    session_manager.sessions[old_session.session_id] = old_session
    session_manager.sessions[new_session.session_id] = new_session
    session_manager.user_to_session[1] = old_session.session_id
    session_manager.user_to_session[2] = new_session.session_id
    session_manager.web_clients[old_session.session_id] = set()
    session_manager.web_clients[new_session.session_id] = set()

    # Cleanup (timeout is 3600 seconds = 1 hour)
    count = await session_manager.cleanup_idle_sessions()

    # Verify old session deleted, new session kept
    assert count == 1
    assert old_session.session_id not in session_manager.sessions
    assert new_session.session_id in session_manager.sessions


async def test_delete_session(session_manager: SessionManager, instance_pool: InstancePool) -> None:
    """Test deleting a session."""
    # Create a session with web clients
    session = TelegramSession(
        session_id="test-session-id",
        telegram_user_id=12345,
        telegram_username="testuser",
        telegram_first_name="Test",
    )
    session.instance_id = "test-instance-id"

    websocket = AsyncMock()
    websocket.close = AsyncMock()

    session_manager.sessions[session.session_id] = session
    session_manager.user_to_session[12345] = session.session_id
    session_manager.web_clients[session.session_id] = {websocket}

    # Delete
    result = await session_manager.delete_session(session.session_id)

    # Verify deleted
    assert result is True
    assert session.session_id not in session_manager.sessions
    assert 12345 not in session_manager.user_to_session
    assert session.session_id not in session_manager.web_clients

    # Verify websocket closed
    websocket.close.assert_called_once()

    # Verify instance deleted
    instance_pool.delete_instance.assert_called_once_with("test-instance-id")


async def test_delete_session_not_found(session_manager: SessionManager) -> None:
    """Test deleting non-existent session."""
    result = await session_manager.delete_session("non-existent")
    assert result is False


async def test_shutdown(session_manager: SessionManager) -> None:
    """Test shutting down all sessions."""
    # Create multiple sessions
    for i in range(3):
        session = TelegramSession(
            session_id=f"session-{i}",
            telegram_user_id=i,
            telegram_username=f"user{i}",
            telegram_first_name=f"User{i}",
        )
        session.instance_id = f"instance-{i}"
        session_manager.sessions[session.session_id] = session
        session_manager.user_to_session[i] = session.session_id
        session_manager.web_clients[session.session_id] = set()

    # Shutdown
    await session_manager.shutdown()

    # Verify all sessions deleted
    assert len(session_manager.sessions) == 0
    assert len(session_manager.user_to_session) == 0
    assert len(session_manager.web_clients) == 0


if __name__ == "__main__":
    import sys
    import unittest

    # Create a simple test runner for direct execution
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()

    # Note: pytest-asyncio tests won't run with unittest directly
    print("Note: This test file uses pytest-asyncio.")
    print("Run with: pytest tests/test_telegram_session_manager.py")
    sys.exit(0)

"""
Database configuration and ORM models for CLUD-CLUSTER.

Uses SQLAlchemy 2.0 async API with SQLite (default) or PostgreSQL.
"""

import uuid
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from typing import Any

from sqlalchemy import JSON, DateTime, Integer, String, Text, select
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker, create_async_engine
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column


class Base(DeclarativeBase):
    """Base class for all ORM models."""

    pass


def generate_uuid() -> uuid.UUID:
    """Generate a new UUID v4."""
    return uuid.uuid4()


class AgentDB(Base):
    """Agent database model."""

    __tablename__ = "agents"

    id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), primary_key=True, default=generate_uuid)
    daemon_id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), index=True)
    hostname: Mapped[str] = mapped_column(String(255))
    pid: Mapped[int] = mapped_column(Integer)
    cwd: Mapped[str] = mapped_column(Text)
    command: Mapped[str] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(50), index=True)
    capabilities: Mapped[list[str]] = mapped_column(JSON, default=list)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc))
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc), onupdate=lambda: datetime.now(timezone.utc))
    last_heartbeat: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc), index=True)
    stopped_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)

    # Freshness tracking
    staleness: Mapped[str] = mapped_column(String(50), default="fresh")

    # Daemon-reported state (ground truth)
    daemon_reported_status: Mapped[str] = mapped_column(String(50))
    daemon_reported_at: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc))

    # Metrics (stored as JSON for flexibility)
    metrics: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)


class DaemonDB(Base):
    """Daemon database model."""

    __tablename__ = "daemons"

    id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), primary_key=True, default=generate_uuid)
    hostname: Mapped[str] = mapped_column(String(255), index=True)
    platform: Mapped[str] = mapped_column(String(50))
    version: Mapped[str] = mapped_column(String(50))
    bind_address: Mapped[str] = mapped_column(String(100))
    status: Mapped[str] = mapped_column(String(50), index=True)
    agent_count: Mapped[int] = mapped_column(Integer, default=0)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc))
    last_seen: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc), index=True)


class TelegramBindingDB(Base):
    """Telegram binding database model."""

    __tablename__ = "telegram_bindings"

    id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), primary_key=True, default=generate_uuid)
    chat_id: Mapped[int] = mapped_column(Integer, index=True)
    agent_id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), index=True)
    operator_id: Mapped[str] = mapped_column(String(255))
    mode: Mapped[str] = mapped_column(String(50))
    created_at: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc))


class SessionDB(Base):
    """Session database model."""

    __tablename__ = "sessions"

    id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), primary_key=True, default=generate_uuid)
    operator_id: Mapped[str] = mapped_column(String(255), index=True)
    type: Mapped[str] = mapped_column(String(50))
    token: Mapped[str] = mapped_column(String(500), unique=True, index=True)
    expires_at: Mapped[datetime] = mapped_column(DateTime, index=True)
    scopes: Mapped[list[str]] = mapped_column(JSON, default=list)


class AuditEventDB(Base):
    """Audit event database model."""

    __tablename__ = "audit_events"

    id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), primary_key=True, default=generate_uuid)
    operator_id: Mapped[str] = mapped_column(String(255), index=True)
    event_type: Mapped[str] = mapped_column(String(100), index=True)
    agent_id: Mapped[uuid.UUID | None] = mapped_column(PGUUID(as_uuid=True), nullable=True, index=True)
    payload: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
    result: Mapped[str] = mapped_column(String(50))
    timestamp: Mapped[datetime] = mapped_column(DateTime, default=lambda: datetime.now(timezone.utc), index=True)


# Database connection management


class Database:
    """Database connection manager."""

    def __init__(self, database_url: str = "sqlite+aiosqlite:///./clud_cluster.db") -> None:
        """
        Initialize database connection.

        Args:
            database_url: SQLAlchemy database URL
                Examples:
                - SQLite: "sqlite+aiosqlite:///./clud_cluster.db"
                - PostgreSQL: "postgresql+asyncpg://user:pass@localhost/clud_cluster"
        """
        self.database_url = database_url
        self.engine = create_async_engine(
            database_url,
            echo=False,  # Set to True for SQL query logging
            pool_pre_ping=True,  # Test connections before use
            pool_recycle=3600,  # Recycle connections after 1 hour
        )
        self.async_session = async_sessionmaker(
            self.engine,
            class_=AsyncSession,
            expire_on_commit=False,
        )

    async def create_tables(self) -> None:
        """Create all database tables."""
        async with self.engine.begin() as conn:
            await conn.run_sync(Base.metadata.create_all)

    async def drop_tables(self) -> None:
        """Drop all database tables (WARNING: destructive!)."""
        async with self.engine.begin() as conn:
            await conn.run_sync(Base.metadata.drop_all)

    @asynccontextmanager
    async def get_session(self) -> AsyncIterator[AsyncSession]:
        """Get an async database session."""
        async with self.async_session() as session:
            try:
                yield session
                await session.commit()
            except Exception:
                await session.rollback()
                raise
            finally:
                await session.close()

    async def close(self) -> None:
        """Close database connections."""
        await self.engine.dispose()


# Helper functions for common queries


async def get_agent_by_id(session: AsyncSession, agent_id: uuid.UUID) -> AgentDB | None:
    """Get agent by ID."""
    result = await session.execute(select(AgentDB).where(AgentDB.id == agent_id))
    return result.scalar_one_or_none()


async def get_daemon_by_id(session: AsyncSession, daemon_id: uuid.UUID) -> DaemonDB | None:
    """Get daemon by ID."""
    result = await session.execute(select(DaemonDB).where(DaemonDB.id == daemon_id))
    return result.scalar_one_or_none()


async def list_agents(session: AsyncSession, daemon_id: uuid.UUID | None = None) -> list[AgentDB]:
    """List all agents, optionally filtered by daemon."""
    query = select(AgentDB)
    if daemon_id:
        query = query.where(AgentDB.daemon_id == daemon_id)
    result = await session.execute(query.order_by(AgentDB.created_at.desc()))
    return list(result.scalars().all())


async def list_daemons(session: AsyncSession) -> list[DaemonDB]:
    """List all daemons."""
    result = await session.execute(select(DaemonDB).order_by(DaemonDB.last_seen.desc()))
    return list(result.scalars().all())


async def update_agent_staleness(session: AsyncSession, agent: AgentDB) -> None:
    """
    Update agent staleness based on last_heartbeat.

    Staleness rules:
    - Fresh: last_heartbeat < 15s ago
    - Stale: 15s <= last_heartbeat < 90s ago
    - Disconnected: last_heartbeat >= 90s ago
    """
    now = datetime.now(timezone.utc)
    age = (now - agent.last_heartbeat).total_seconds()

    if age < 15:
        agent.staleness = "fresh"
    elif age < 90:
        agent.staleness = "stale"
    else:
        agent.staleness = "disconnected"

    agent.updated_at = now
    await session.flush()


# Session CRUD operations


async def create_session(session: AsyncSession, session_obj: Any) -> SessionDB:
    """Create a new session."""
    db_session = SessionDB(
        id=session_obj.id,
        operator_id=session_obj.operator_id,
        type=session_obj.type.value,
        token=session_obj.token,
        expires_at=session_obj.expires_at,
        scopes=session_obj.scopes,
    )
    session.add(db_session)
    await session.flush()
    return db_session


async def get_session_by_token(session: AsyncSession, token: str) -> SessionDB | None:
    """Get session by token."""
    result = await session.execute(select(SessionDB).where(SessionDB.token == token))
    return result.scalar_one_or_none()


async def get_session_by_id(session: AsyncSession, session_id: uuid.UUID) -> SessionDB | None:
    """Get session by ID."""
    result = await session.execute(select(SessionDB).where(SessionDB.id == session_id))
    return result.scalar_one_or_none()


async def delete_session(session: AsyncSession, session_id: uuid.UUID) -> None:
    """Delete a session."""
    db_session = await get_session_by_id(session, session_id)
    if db_session:
        await session.delete(db_session)
        await session.flush()


async def list_sessions(session: AsyncSession, operator_id: str | None = None) -> list[SessionDB]:
    """List all sessions, optionally filtered by operator."""
    query = select(SessionDB)
    if operator_id:
        query = query.where(SessionDB.operator_id == operator_id)
    result = await session.execute(query.order_by(SessionDB.expires_at.desc()))
    return list(result.scalars().all())


# TelegramBinding CRUD operations


async def create_telegram_binding(session: AsyncSession, binding: Any) -> TelegramBindingDB:
    """Create a new Telegram binding."""
    db_binding = TelegramBindingDB(
        id=binding.id,
        chat_id=binding.chat_id,
        agent_id=binding.agent_id,
        operator_id=binding.operator_id,
        mode=binding.mode.value,
        created_at=binding.created_at,
    )
    session.add(db_binding)
    await session.flush()
    return db_binding


async def get_telegram_binding_by_id(session: AsyncSession, binding_id: uuid.UUID) -> TelegramBindingDB | None:
    """Get Telegram binding by ID."""
    result = await session.execute(select(TelegramBindingDB).where(TelegramBindingDB.id == binding_id))
    return result.scalar_one_or_none()


async def get_telegram_binding_by_chat_and_agent(session: AsyncSession, chat_id: int, agent_id: uuid.UUID) -> TelegramBindingDB | None:
    """Get Telegram binding by chat_id and agent_id."""
    result = await session.execute(select(TelegramBindingDB).where(TelegramBindingDB.chat_id == chat_id).where(TelegramBindingDB.agent_id == agent_id))
    return result.scalar_one_or_none()


async def list_telegram_bindings(session: AsyncSession, agent_id: uuid.UUID | None = None, chat_id: int | None = None) -> list[TelegramBindingDB]:
    """List Telegram bindings, optionally filtered by agent or chat."""
    query = select(TelegramBindingDB)
    if agent_id:
        query = query.where(TelegramBindingDB.agent_id == agent_id)
    if chat_id:
        query = query.where(TelegramBindingDB.chat_id == chat_id)
    result = await session.execute(query.order_by(TelegramBindingDB.created_at.desc()))
    return list(result.scalars().all())


async def delete_telegram_binding(session: AsyncSession, binding_id: uuid.UUID) -> None:
    """Delete a Telegram binding."""
    db_binding = await get_telegram_binding_by_id(session, binding_id)
    if db_binding:
        await session.delete(db_binding)
        await session.flush()


# AuditEvent CRUD operations


async def create_audit_event(session: AsyncSession, event: Any) -> AuditEventDB:
    """Create a new audit event."""
    db_event = AuditEventDB(
        id=event.id,
        operator_id=event.operator_id,
        event_type=event.event_type,
        agent_id=event.agent_id,
        payload=event.payload,
        result=event.result.value,
        timestamp=event.timestamp,
    )
    session.add(db_event)
    await session.flush()
    return db_event


async def list_audit_events(
    session: AsyncSession,
    operator_id: str | None = None,
    agent_id: uuid.UUID | None = None,
    event_type: str | None = None,
    limit: int = 100,
) -> list[AuditEventDB]:
    """List audit events with optional filters."""
    query = select(AuditEventDB)
    if operator_id:
        query = query.where(AuditEventDB.operator_id == operator_id)
    if agent_id:
        query = query.where(AuditEventDB.agent_id == agent_id)
    if event_type:
        query = query.where(AuditEventDB.event_type == event_type)
    query = query.order_by(AuditEventDB.timestamp.desc()).limit(limit)
    result = await session.execute(query)
    return list(result.scalars().all())

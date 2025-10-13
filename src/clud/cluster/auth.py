"""
Authentication and authorization module for CLUD-CLUSTER.

Provides JWT token generation/validation, password hashing, and API key management.
Supports multiple authentication methods: password, Telegram, and API keys.
"""

from datetime import datetime, timedelta, timezone
from uuid import UUID

from jose import JWTError, jwt
from passlib.context import CryptContext
from pydantic import BaseModel

from .config import settings
from .models import Session, SessionType

# Password hashing context
pwd_context = CryptContext(schemes=["bcrypt"], deprecated="auto")


class TokenData(BaseModel):
    """JWT token payload data."""

    operator_id: str
    session_id: str
    session_type: SessionType
    scopes: list[str] = []
    exp: int  # Expiration timestamp


class Credentials(BaseModel):
    """User credentials for authentication."""

    username: str
    password: str


def hash_password(password: str) -> str:
    """Hash a password using bcrypt."""
    return pwd_context.hash(password)


def verify_password(plain_password: str, hashed_password: str) -> bool:
    """Verify a password against its hash."""
    return pwd_context.verify(plain_password, hashed_password)


def create_access_token(
    operator_id: str,
    session_id: UUID,
    session_type: SessionType = SessionType.WEB,
    scopes: list[str] | None = None,
    expires_delta: timedelta | None = None,
) -> str:
    """
    Create a JWT access token.

    Args:
        operator_id: Unique identifier for the operator (username, telegram_id, etc.)
        session_id: Session UUID
        session_type: Type of session (web, telegram, api)
        scopes: List of permission scopes
        expires_delta: Custom expiration time (defaults to settings)

    Returns:
        Encoded JWT token string
    """
    if scopes is None:
        scopes = ["agent:read"]  # Default: read-only

    if expires_delta is None:
        expires_delta = timedelta(minutes=settings.access_token_expire_minutes)

    expire = datetime.now(timezone.utc) + expires_delta
    to_encode = {
        "operator_id": operator_id,
        "session_id": str(session_id),
        "session_type": session_type.value,
        "scopes": scopes,
        "exp": int(expire.timestamp()),
    }

    encoded_jwt = jwt.encode(to_encode, settings.secret_key, algorithm=settings.jwt_algorithm)
    return encoded_jwt


def decode_access_token(token: str) -> TokenData | None:
    """
    Decode and validate a JWT access token.

    Args:
        token: JWT token string

    Returns:
        TokenData if valid, None if invalid/expired
    """
    try:
        payload = jwt.decode(token, settings.secret_key, algorithms=[settings.jwt_algorithm])
        return TokenData(**payload)
    except JWTError:
        return None


def create_session(
    operator_id: str,
    session_type: SessionType = SessionType.WEB,
    scopes: list[str] | None = None,
) -> Session:
    """
    Create a new authenticated session with JWT token.

    Args:
        operator_id: Unique identifier for the operator
        session_type: Type of session
        scopes: Permission scopes

    Returns:
        Session object with token
    """
    session = Session(
        operator_id=operator_id,
        type=session_type,
        token="",  # Will be set below
        expires_at=datetime.now(timezone.utc) + timedelta(minutes=settings.access_token_expire_minutes),
        scopes=scopes or ["agent:read"],
    )

    # Generate token with session_id
    token = create_access_token(
        operator_id=operator_id,
        session_id=session.id,
        session_type=session_type,
        scopes=session.scopes,
    )
    session.token = token

    return session


def verify_scope(token_data: TokenData, required_scope: str) -> bool:
    """
    Check if token has required scope.

    Args:
        token_data: Decoded token data
        required_scope: Required permission scope (e.g., "agent:write")

    Returns:
        True if scope is present, False otherwise
    """
    return required_scope in token_data.scopes


# Predefined scope sets
SCOPES_READ_ONLY = ["agent:read", "daemon:read"]
SCOPES_OPERATOR = ["agent:read", "agent:write", "daemon:read"]
SCOPES_ADMIN = [
    "agent:read",
    "agent:write",
    "agent:delete",
    "daemon:read",
    "daemon:write",
    "vscode:launch",
    "telegram:bind",
]

"""
Authentication dependency functions for FastAPI endpoints.

Provides dependency injection for authentication:
- get_current_token: Extract and validate JWT token
- require_auth: Require valid authentication
- optional_auth: Optional authentication
"""

from fastapi import Depends, HTTPException, status
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from .auth import TokenData, decode_access_token

# Security scheme
security = HTTPBearer(auto_error=False)


async def get_current_token(
    credentials: HTTPAuthorizationCredentials | None = Depends(security),
) -> TokenData | None:
    """
    Extract and validate JWT token from Authorization header.

    Returns None if no token or invalid token (for optional auth).
    """
    if not credentials:
        return None

    token_data = decode_access_token(credentials.credentials)
    return token_data


async def require_auth(
    token_data: TokenData | None = Depends(get_current_token),
) -> TokenData:
    """
    Require valid authentication.

    Raises 401 if no valid token.
    """
    if not token_data:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Not authenticated",
            headers={"WWW-Authenticate": "Bearer"},
        )
    return token_data


async def optional_auth(
    token_data: TokenData | None = Depends(get_current_token),
) -> TokenData | None:
    """
    Optional authentication dependency.

    Returns token data if present, None otherwise.
    """
    return token_data

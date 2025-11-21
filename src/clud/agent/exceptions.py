"""
Custom exception classes for agent operations.

This module defines the exception hierarchy used throughout the agent system
for consistent error handling and reporting.
"""


class CludError(Exception):
    """Base exception for clud errors."""

    pass


class ValidationError(CludError):
    """User/validation error."""

    pass


class ConfigError(CludError):
    """Configuration error."""

    pass

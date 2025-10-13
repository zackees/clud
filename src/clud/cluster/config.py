"""
Configuration management for CLUD-CLUSTER.

Uses pydantic-settings for environment variable loading and validation.
"""

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Application settings loaded from environment variables."""

    # Application
    app_name: str = "CLUD-CLUSTER"
    app_version: str = "1.0.0-beta"
    debug: bool = False
    log_level: str = "INFO"

    # Server
    host: str = "0.0.0.0"
    port: int = 8000
    reload: bool = False

    # Database
    database_url: str = "sqlite+aiosqlite:///./clud_cluster.db"

    # Security
    secret_key: str = "change-me-in-production-min-32-characters"
    jwt_algorithm: str = "HS256"
    access_token_expire_minutes: int = 60 * 24  # 24 hours

    # WebSocket
    ws_heartbeat_interval: int = 30  # seconds
    ws_message_max_size: int = 1_048_576  # 1MB

    # PTY
    pty_buffer_size: int = 1_048_576  # 1MB per agent
    pty_coalesce_ms: int = 20  # milliseconds

    # Agents
    max_agents_per_daemon: int = 50
    max_agents_per_pty_connection: int = 5

    # Telegram (optional)
    telegram_bot_token: str | None = None
    telegram_admin_ids: list[int] = []

    # VS Code
    vscode_idle_timeout: int = 3600  # 1 hour
    vscode_startup_timeout: int = 60  # seconds

    # Staleness thresholds (seconds)
    staleness_fresh_threshold: int = 15
    staleness_stale_threshold: int = 90

    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
    )


# Global settings instance
settings = Settings()

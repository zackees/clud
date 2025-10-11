"""Tests for messaging credential integration with clud's credential store."""

import os
import pytest
from pathlib import Path
from unittest.mock import Mock, patch

from clud.messaging.config import (
    load_messaging_config,
    save_messaging_credentials_secure,
    migrate_from_json_to_keyring,
)


class TestCredentialStoreIntegration:
    """Test integration with clud's credential store."""

    def test_load_from_credential_store(self, monkeypatch):
        """Test loading credentials from encrypted credential store."""
        # Mock credential store
        mock_keyring = Mock()
        mock_keyring.get_password = Mock(side_effect=lambda service, username: {
            ("clud", "telegram-bot-token"): "token_from_keyring",
            ("clud", "twilio-account-sid"): "sid_from_keyring",
        }.get((service, username)))

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            config = load_messaging_config()

        assert config["telegram_token"] == "token_from_keyring"
        assert config["twilio_sid"] == "sid_from_keyring"
        mock_keyring.get_password.assert_called()

    def test_priority_env_over_keyring(self, monkeypatch):
        """Test that environment variables override credential store."""
        monkeypatch.setenv("TELEGRAM_BOT_TOKEN", "token_from_env")

        mock_keyring = Mock()
        mock_keyring.get_password = Mock(return_value="token_from_keyring")

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            config = load_messaging_config()

        # Environment variable should win
        assert config["telegram_token"] == "token_from_env"

    def test_fallback_to_key_file(self, tmp_path, monkeypatch):
        """Test fallback to .key files when credential store unavailable."""
        # Create temp .clud directory
        clud_dir = tmp_path / ".clud"
        clud_dir.mkdir()
        key_file = clud_dir / "telegram-bot-token.key"
        key_file.write_text("token_from_keyfile")

        monkeypatch.setattr(Path, "home", lambda: tmp_path)

        # Mock credential store as unavailable
        with patch("clud.messaging.config.get_credential_store", return_value=None):
            config = load_messaging_config()

        assert config["telegram_token"] == "token_from_keyfile"

    def test_save_to_credential_store(self):
        """Test saving credentials to credential store."""
        mock_keyring = Mock()

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            success = save_messaging_credentials_secure(
                telegram_token="test_token",
                twilio_sid="test_sid",
            )

        assert success is True
        mock_keyring.set_password.assert_any_call("clud", "telegram-bot-token", "test_token")
        mock_keyring.set_password.assert_any_call("clud", "twilio-account-sid", "test_sid")

    def test_save_when_credential_store_unavailable(self):
        """Test behavior when credential store is unavailable."""
        with patch("clud.messaging.config.get_credential_store", return_value=None):
            success = save_messaging_credentials_secure(telegram_token="test_token")

        # Should return False (fallback needed)
        assert success is False


class TestMigration:
    """Test migration from JSON to credential store."""

    def test_migrate_from_json(self, tmp_path):
        """Test migration from messaging.json to credential store."""
        # Create messaging.json
        clud_dir = tmp_path / ".clud"
        clud_dir.mkdir()
        json_file = clud_dir / "messaging.json"
        json_file.write_text('{"telegram": {"bot_token": "migrate_me"}}')

        mock_keyring = Mock()

        with patch("clud.messaging.config.get_messaging_config_file", return_value=json_file):
            with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
                result = migrate_from_json_to_keyring()

        assert result is True
        mock_keyring.set_password.assert_called_with("clud", "telegram-bot-token", "migrate_me")

        # Verify backup created
        backup = json_file.with_suffix(".json.backup")
        assert backup.exists()
        assert not json_file.exists()

    def test_migrate_no_json(self):
        """Test migration when no JSON file exists."""
        with patch("clud.messaging.config.get_messaging_config_file") as mock_path:
            mock_path.return_value.exists.return_value = False
            result = migrate_from_json_to_keyring()

        assert result is False


class TestLoadPriority:
    """Test credential loading priority order."""

    def test_priority_order(self, monkeypatch, tmp_path):
        """Test full priority order: env > keyring > keyfile > json."""
        # Set up all sources
        monkeypatch.setenv("TELEGRAM_BOT_TOKEN", "from_env")

        mock_keyring = Mock()
        mock_keyring.get_password = Mock(return_value="from_keyring")

        clud_dir = tmp_path / ".clud"
        clud_dir.mkdir()
        key_file = clud_dir / "telegram-bot-token.key"
        key_file.write_text("from_keyfile")

        json_file = clud_dir / "messaging.json"
        json_file.write_text('{"telegram": {"bot_token": "from_json"}}')

        monkeypatch.setattr(Path, "home", lambda: tmp_path)

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            config = load_messaging_config()

        # Environment variable should win
        assert config["telegram_token"] == "from_env"

    def test_keyring_when_no_env(self, monkeypatch):
        """Test that keyring is used when env var not set."""
        mock_keyring = Mock()
        mock_keyring.get_password = Mock(return_value="from_keyring")

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            config = load_messaging_config()

        assert config.get("telegram_token") == "from_keyring"


class TestBackwardCompatibility:
    """Test backward compatibility with legacy storage methods."""

    def test_legacy_json_still_works(self, tmp_path, monkeypatch):
        """Test that legacy JSON files still work (with warning)."""
        clud_dir = tmp_path / ".clud"
        clud_dir.mkdir()
        json_file = clud_dir / "messaging.json"
        json_file.write_text('{"telegram": {"bot_token": "legacy_token"}}')

        monkeypatch.setattr(Path, "home", lambda: tmp_path)

        # No env vars, no keyring
        with patch("clud.messaging.config.get_credential_store", return_value=None):
            config = load_messaging_config()

        assert config["telegram_token"] == "legacy_token"

    def test_keyfile_backward_compat(self, tmp_path, monkeypatch):
        """Test that .key files are still supported."""
        clud_dir = tmp_path / ".clud"
        clud_dir.mkdir()

        # Create individual key files (old method)
        (clud_dir / "telegram-bot-token.key").write_text("token_from_file")
        (clud_dir / "twilio-account-sid.key").write_text("sid_from_file")

        monkeypatch.setattr(Path, "home", lambda: tmp_path)

        with patch("clud.messaging.config.get_credential_store", return_value=None):
            config = load_messaging_config()

        assert config["telegram_token"] == "token_from_file"
        assert config["twilio_sid"] == "sid_from_file"


class TestErrorHandling:
    """Test error handling in credential operations."""

    def test_load_handles_import_error(self):
        """Test that load works when credential store import fails."""
        with patch("clud.messaging.config.get_credential_store", side_effect=ImportError):
            config = load_messaging_config()

        # Should return empty dict, not crash
        assert isinstance(config, dict)

    def test_save_handles_import_error(self):
        """Test that save handles import errors gracefully."""
        with patch("clud.messaging.config.get_credential_store", side_effect=ImportError):
            success = save_messaging_credentials_secure(telegram_token="test")

        # Should return False (fallback needed)
        assert success is False

    def test_load_handles_keyring_exception(self):
        """Test that load handles keyring exceptions gracefully."""
        mock_keyring = Mock()
        mock_keyring.get_password = Mock(side_effect=Exception("Keyring error"))

        with patch("clud.messaging.config.get_credential_store", return_value=mock_keyring):
            config = load_messaging_config()

        # Should not crash, should return what it can find
        assert isinstance(config, dict)

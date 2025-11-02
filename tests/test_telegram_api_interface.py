"""Unit tests for Telegram API interface and configuration.

Tests the abstract interface, configuration loading, and factory function for:
- TelegramAPIConfig creation and validation
- Environment variable loading
- Factory function behavior with different modes
- Configuration validation and defaults
"""

import os
import unittest

from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_factory import create_telegram_api
from clud.telegram.api_fake import FakeTelegramBotAPI
from clud.telegram.api_interface import TelegramBotAPI


class TestTelegramAPIConfig(unittest.TestCase):
    """Test TelegramAPIConfig class."""

    def test_default_config(self) -> None:
        """Test default configuration values with auto_detect disabled."""
        # Default is "real" which requires bot_token, so we disable auto_detect
        # and provide a token
        config = TelegramAPIConfig(bot_token="test_token", auto_detect_from_env=False)

        self.assertEqual(config.implementation, "real")
        self.assertEqual(config.bot_token, "test_token")
        self.assertFalse(config.auto_detect_from_env)
        self.assertEqual(config.fake_delay_ms, 100)
        self.assertEqual(config.fake_error_rate, 0.0)

    def test_config_with_real_implementation(self) -> None:
        """Test config for real implementation."""
        config = TelegramAPIConfig(
            implementation="real",
            bot_token="test_token_123",
        )

        self.assertEqual(config.implementation, "real")
        self.assertEqual(config.bot_token, "test_token_123")

    def test_config_with_fake_implementation(self) -> None:
        """Test config for fake implementation."""
        config = TelegramAPIConfig(
            implementation="fake",
            fake_delay_ms=50,
            fake_error_rate=0.1,
        )

        self.assertEqual(config.implementation, "fake")
        self.assertEqual(config.fake_delay_ms, 50)
        self.assertEqual(config.fake_error_rate, 0.1)

    def test_config_with_mock_implementation(self) -> None:
        """Test config for mock implementation."""
        config = TelegramAPIConfig(implementation="mock")

        self.assertEqual(config.implementation, "mock")

    def test_for_testing_factory(self) -> None:
        """Test for_testing factory method."""
        config = TelegramAPIConfig.for_testing()

        self.assertEqual(config.implementation, "fake")
        self.assertIsNone(config.bot_token)
        self.assertFalse(config.auto_detect_from_env)
        self.assertEqual(config.fake_delay_ms, 0)
        self.assertEqual(config.fake_error_rate, 0.0)

    def test_for_testing_with_custom_implementation(self) -> None:
        """Test for_testing with custom implementation."""
        config = TelegramAPIConfig.for_testing(implementation="mock")

        self.assertEqual(config.implementation, "mock")
        self.assertFalse(config.auto_detect_from_env)

    def test_from_environment_with_telegram_api_mode(self) -> None:
        """Test loading config from TELEGRAM_API_MODE environment variable."""
        old_value = os.environ.get("TELEGRAM_API_MODE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            config = TelegramAPIConfig.from_environment()

            self.assertEqual(config.implementation, "fake")
        finally:
            if old_value is not None:
                os.environ["TELEGRAM_API_MODE"] = old_value
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)

    def test_from_environment_with_telegram_bot_token(self) -> None:
        """Test loading bot token from TELEGRAM_BOT_TOKEN environment variable."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_token = os.environ.get("TELEGRAM_BOT_TOKEN")
        try:
            # Need to set mode to fake to avoid requiring bot_token validation
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_BOT_TOKEN"] = "test_token_from_env"
            config = TelegramAPIConfig.from_environment()

            self.assertEqual(config.bot_token, "test_token_from_env")
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_token is not None:
                os.environ["TELEGRAM_BOT_TOKEN"] = old_token
            else:
                os.environ.pop("TELEGRAM_BOT_TOKEN", None)

    def test_from_environment_with_fake_delay(self) -> None:
        """Test loading fake_delay_ms from TELEGRAM_FAKE_DELAY environment variable."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_delay = os.environ.get("TELEGRAM_FAKE_DELAY")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_FAKE_DELAY"] = "150"
            config = TelegramAPIConfig.from_environment()

            self.assertEqual(config.fake_delay_ms, 150)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_delay is not None:
                os.environ["TELEGRAM_FAKE_DELAY"] = old_delay
            else:
                os.environ.pop("TELEGRAM_FAKE_DELAY", None)

    def test_from_environment_with_fake_error_rate(self) -> None:
        """Test loading fake_error_rate from TELEGRAM_FAKE_ERROR_RATE environment variable."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_error = os.environ.get("TELEGRAM_FAKE_ERROR_RATE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_FAKE_ERROR_RATE"] = "0.25"
            config = TelegramAPIConfig.from_environment()

            self.assertEqual(config.fake_error_rate, 0.25)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_error is not None:
                os.environ["TELEGRAM_FAKE_ERROR_RATE"] = old_error
            else:
                os.environ.pop("TELEGRAM_FAKE_ERROR_RATE", None)

    def test_from_environment_with_all_variables(self) -> None:
        """Test loading all config from environment variables."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_token = os.environ.get("TELEGRAM_BOT_TOKEN")
        old_delay = os.environ.get("TELEGRAM_FAKE_DELAY")
        old_error = os.environ.get("TELEGRAM_FAKE_ERROR_RATE")

        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_BOT_TOKEN"] = "env_token"
            os.environ["TELEGRAM_FAKE_DELAY"] = "200"
            os.environ["TELEGRAM_FAKE_ERROR_RATE"] = "0.5"

            config = TelegramAPIConfig.from_environment()

            self.assertEqual(config.implementation, "fake")
            self.assertEqual(config.bot_token, "env_token")
            self.assertEqual(config.fake_delay_ms, 200)
            self.assertEqual(config.fake_error_rate, 0.5)
        finally:
            # Restore original values
            for key, old_val in [
                ("TELEGRAM_API_MODE", old_mode),
                ("TELEGRAM_BOT_TOKEN", old_token),
                ("TELEGRAM_FAKE_DELAY", old_delay),
                ("TELEGRAM_FAKE_ERROR_RATE", old_error),
            ]:
                if old_val is not None:
                    os.environ[key] = old_val
                else:
                    os.environ.pop(key, None)

    def test_from_environment_with_invalid_fake_delay(self) -> None:
        """Test that invalid TELEGRAM_FAKE_DELAY falls back to default."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_delay = os.environ.get("TELEGRAM_FAKE_DELAY")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_FAKE_DELAY"] = "invalid"
            config = TelegramAPIConfig.from_environment()

            # Should fall back to default (100)
            self.assertEqual(config.fake_delay_ms, 100)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_delay is not None:
                os.environ["TELEGRAM_FAKE_DELAY"] = old_delay
            else:
                os.environ.pop("TELEGRAM_FAKE_DELAY", None)

    def test_from_environment_with_invalid_fake_error_rate(self) -> None:
        """Test that invalid TELEGRAM_FAKE_ERROR_RATE falls back to default."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_error = os.environ.get("TELEGRAM_FAKE_ERROR_RATE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            os.environ["TELEGRAM_FAKE_ERROR_RATE"] = "invalid"
            config = TelegramAPIConfig.from_environment()

            # Should fall back to default (0.0)
            self.assertEqual(config.fake_error_rate, 0.0)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_error is not None:
                os.environ["TELEGRAM_FAKE_ERROR_RATE"] = old_error
            else:
                os.environ.pop("TELEGRAM_FAKE_ERROR_RATE", None)

    def test_validate_fake_error_rate_range(self) -> None:
        """Test that fake_error_rate is validated to be in range [0.0, 1.0]."""
        # Valid values should work with fake implementation
        TelegramAPIConfig(implementation="fake", fake_error_rate=0.0, auto_detect_from_env=False)
        TelegramAPIConfig(implementation="fake", fake_error_rate=0.5, auto_detect_from_env=False)
        TelegramAPIConfig(implementation="fake", fake_error_rate=1.0, auto_detect_from_env=False)

        # Invalid values should raise ValueError
        with self.assertRaises(ValueError):
            TelegramAPIConfig(implementation="fake", fake_error_rate=-0.1, auto_detect_from_env=False)

        with self.assertRaises(ValueError):
            TelegramAPIConfig(implementation="fake", fake_error_rate=1.1, auto_detect_from_env=False)

    def test_validate_fake_delay_ms_non_negative(self) -> None:
        """Test that fake_delay_ms must be non-negative."""
        # Valid values should work with fake implementation
        TelegramAPIConfig(implementation="fake", fake_delay_ms=0, auto_detect_from_env=False)
        TelegramAPIConfig(implementation="fake", fake_delay_ms=100, auto_detect_from_env=False)

        # Negative value should raise ValueError
        with self.assertRaises(ValueError):
            TelegramAPIConfig(implementation="fake", fake_delay_ms=-1, auto_detect_from_env=False)

    def test_validate_implementation_choices(self) -> None:
        """Test that implementation must be one of the valid choices."""
        # Valid values should work
        TelegramAPIConfig(implementation="real", bot_token="test", auto_detect_from_env=False)
        TelegramAPIConfig(implementation="fake", auto_detect_from_env=False)
        TelegramAPIConfig(implementation="mock", auto_detect_from_env=False)

        # Invalid value should raise ValueError (validated by Literal type at runtime if using pydantic)
        # For dataclass, this would be type-checking only, so we skip runtime validation test


class TestCreateTelegramAPI(unittest.TestCase):
    """Test create_telegram_api factory function."""

    def test_create_with_fake_config(self) -> None:
        """Test creating fake API with explicit config."""
        config = TelegramAPIConfig.for_testing(implementation="fake")
        api = create_telegram_api(config=config)

        self.assertIsInstance(api, FakeTelegramBotAPI)
        self.assertIsInstance(api, TelegramBotAPI)

    def test_create_with_fake_from_explicit_config(self) -> None:
        """Test creating fake API with explicit fake config."""
        # When explicitly using fake config, it should work without token
        config = TelegramAPIConfig(implementation="fake", auto_detect_from_env=False)
        api = create_telegram_api(config=config)

        self.assertIsInstance(api, FakeTelegramBotAPI)

    def test_create_with_explicit_fake_delay(self) -> None:
        """Test creating fake API with explicit delay."""
        config = TelegramAPIConfig(
            implementation="fake",
            fake_delay_ms=100,
        )
        api = create_telegram_api(config=config)

        self.assertIsInstance(api, FakeTelegramBotAPI)
        # Access internal config (safe because we checked isinstance above)
        fake_api: FakeTelegramBotAPI = api  # type: ignore[assignment]
        self.assertEqual(fake_api._config.fake_delay_ms, 100)

    def test_create_with_explicit_error_rate(self) -> None:
        """Test creating fake API with explicit error rate."""
        config = TelegramAPIConfig(
            implementation="fake",
            fake_error_rate=0.3,
        )
        api = create_telegram_api(config=config)

        self.assertIsInstance(api, FakeTelegramBotAPI)
        # Access internal config (safe because we checked isinstance above)
        fake_api: FakeTelegramBotAPI = api  # type: ignore[assignment]
        self.assertEqual(fake_api._config.fake_error_rate, 0.3)

    def test_create_from_environment_fake_mode(self) -> None:
        """Test creating API from TELEGRAM_API_MODE=fake."""
        old_value = os.environ.get("TELEGRAM_API_MODE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            api = create_telegram_api()

            self.assertIsInstance(api, FakeTelegramBotAPI)
        finally:
            if old_value is not None:
                os.environ["TELEGRAM_API_MODE"] = old_value
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)

    def test_create_with_bot_token_parameter(self) -> None:
        """Test creating API with bot_token parameter."""
        # Force fake mode to avoid trying to create real API
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "fake"
            api = create_telegram_api(bot_token="test_token")

            # Should be fake due to env override
            self.assertIsInstance(api, FakeTelegramBotAPI)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)

    def test_bot_token_parameter_overrides_config(self) -> None:
        """Test that bot_token parameter overrides config bot_token."""
        config = TelegramAPIConfig(
            implementation="fake",
            bot_token="config_token",
            auto_detect_from_env=False,
        )
        api = create_telegram_api(config=config, bot_token="override_token")

        # bot_token parameter should override config
        self.assertIsInstance(api, FakeTelegramBotAPI)
        # Access internal config (safe because we checked isinstance above)
        fake_api: FakeTelegramBotAPI = api  # type: ignore[assignment]
        self.assertEqual(fake_api._config.bot_token, "override_token")

    def test_config_parameter_overrides_environment(self) -> None:
        """Test that config parameter overrides environment."""
        old_value = os.environ.get("TELEGRAM_API_MODE")
        try:
            os.environ["TELEGRAM_API_MODE"] = "real"
            config = TelegramAPIConfig(implementation="fake", auto_detect_from_env=False)
            api = create_telegram_api(config=config)

            # config should override environment when auto_detect_from_env=False
            self.assertIsInstance(api, FakeTelegramBotAPI)
        finally:
            if old_value is not None:
                os.environ["TELEGRAM_API_MODE"] = old_value
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)


class TestTelegramAPIInterface(unittest.IsolatedAsyncioTestCase):
    """Test that TelegramBotAPI interface is properly implemented."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures."""
        config = TelegramAPIConfig.for_testing(implementation="fake")
        self.api = create_telegram_api(config=config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        await self.api.shutdown()

    async def test_interface_has_all_required_methods(self) -> None:
        """Test that the API implements all required interface methods."""
        # Test that all required methods exist and are callable
        self.assertTrue(hasattr(self.api, "initialize"))
        self.assertTrue(callable(self.api.initialize))

        self.assertTrue(hasattr(self.api, "shutdown"))
        self.assertTrue(callable(self.api.shutdown))

        self.assertTrue(hasattr(self.api, "send_message"))
        self.assertTrue(callable(self.api.send_message))

        self.assertTrue(hasattr(self.api, "send_typing_action"))
        self.assertTrue(callable(self.api.send_typing_action))

        self.assertTrue(hasattr(self.api, "start_polling"))
        self.assertTrue(callable(self.api.start_polling))

        self.assertTrue(hasattr(self.api, "stop_polling"))
        self.assertTrue(callable(self.api.stop_polling))

        self.assertTrue(hasattr(self.api, "add_command_handler"))
        self.assertTrue(callable(self.api.add_command_handler))

        self.assertTrue(hasattr(self.api, "add_message_handler"))
        self.assertTrue(callable(self.api.add_message_handler))

        self.assertTrue(hasattr(self.api, "add_error_handler"))
        self.assertTrue(callable(self.api.add_error_handler))

        self.assertTrue(hasattr(self.api, "get_me"))
        self.assertTrue(callable(self.api.get_me))

    async def test_initialize_returns_bool(self) -> None:
        """Test that initialize returns a boolean."""
        result = await self.api.initialize()
        self.assertIsInstance(result, bool)

    async def test_send_message_returns_message_result(self) -> None:
        """Test that send_message returns a MessageResult."""
        result = await self.api.send_message(chat_id=123, text="Test")

        self.assertTrue(hasattr(result, "success"))
        self.assertTrue(hasattr(result, "message_id"))
        self.assertTrue(hasattr(result, "error"))
        self.assertIsInstance(result.success, bool)

    async def test_send_typing_action_returns_bool(self) -> None:
        """Test that send_typing_action returns a boolean."""
        result = await self.api.send_typing_action(chat_id=123)
        self.assertIsInstance(result, bool)

    async def test_get_me_returns_telegram_user(self) -> None:
        """Test that get_me returns a TelegramUser."""
        bot_user = await self.api.get_me()

        self.assertTrue(hasattr(bot_user, "id"))
        self.assertTrue(hasattr(bot_user, "username"))
        self.assertTrue(hasattr(bot_user, "first_name"))
        self.assertTrue(hasattr(bot_user, "is_bot"))


class TestTelegramAPIConfigPrecedence(unittest.TestCase):
    """Test configuration precedence and override behavior."""

    def test_precedence_bot_token_parameter_overrides_config(self) -> None:
        """Test that bot_token parameter overrides config."""
        config = TelegramAPIConfig(
            implementation="fake",
            bot_token="config_token",
            auto_detect_from_env=False,
        )

        api = create_telegram_api(config=config, bot_token="param_token")

        # bot_token parameter should override config
        self.assertIsInstance(api, FakeTelegramBotAPI)
        # Access internal config (safe because we checked isinstance above)
        fake_api: FakeTelegramBotAPI = api  # type: ignore[assignment]
        self.assertEqual(fake_api._config.bot_token, "param_token")

    def test_precedence_config_over_environment(self) -> None:
        """Test that explicit config overrides environment when auto_detect=False."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_token = os.environ.get("TELEGRAM_BOT_TOKEN")
        try:
            # Set environment to real with token
            os.environ["TELEGRAM_API_MODE"] = "real"
            os.environ["TELEGRAM_BOT_TOKEN"] = "env_token"

            # Create config with fake and auto_detect_from_env=False
            config = TelegramAPIConfig(
                implementation="fake",
                auto_detect_from_env=False,
            )

            api = create_telegram_api(config=config)

            # Config should win because auto_detect_from_env=False
            self.assertIsInstance(api, FakeTelegramBotAPI)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_token is not None:
                os.environ["TELEGRAM_BOT_TOKEN"] = old_token
            else:
                os.environ.pop("TELEGRAM_BOT_TOKEN", None)

    def test_precedence_environment_over_default(self) -> None:
        """Test that environment variables override defaults."""
        old_mode = os.environ.get("TELEGRAM_API_MODE")
        old_token = os.environ.get("TELEGRAM_BOT_TOKEN")
        try:
            # Set environment to fake
            os.environ["TELEGRAM_API_MODE"] = "fake"
            # Clear any token
            os.environ.pop("TELEGRAM_BOT_TOKEN", None)

            # Create API without explicit config
            api = create_telegram_api()

            # Environment should override default
            self.assertIsInstance(api, FakeTelegramBotAPI)
        finally:
            if old_mode is not None:
                os.environ["TELEGRAM_API_MODE"] = old_mode
            else:
                os.environ.pop("TELEGRAM_API_MODE", None)
            if old_token is not None:
                os.environ["TELEGRAM_BOT_TOKEN"] = old_token


if __name__ == "__main__":
    unittest.main()

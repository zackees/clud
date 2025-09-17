"""Unit tests for clud CLI."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, Mock, patch

from clud.agent_background import (
    ValidationError,
    build_fallback_command,
    build_wrapper_command,
    check_docker_available,
    find_run_claude_docker,
    get_claude_commands_mount,
    get_ssh_dir,
    normalize_path_for_docker,
    validate_path,
)
from clud.agent_foreground import (
    ConfigError,
    get_api_key,
    get_api_key_from_keyring,
    get_clud_config_dir,
    load_api_key_from_config,
    save_api_key_to_config,
    validate_api_key,
)
from clud.agent_foreground import (
    ValidationError as ForegroundValidationError,
)
from clud.cli import (
    create_parser,
    main,
)


class TestCLIParser(unittest.TestCase):
    """Test argument parsing."""

    def setUp(self):
        """Set up test fixtures."""
        self.parser = create_parser()

    def test_basic_usage(self):
        """Test basic command line parsing."""
        args = self.parser.parse_args(["/path/to/project"])
        self.assertEqual(args.path, "/path/to/project")
        self.assertFalse(args.no_dangerous)
        self.assertFalse(args.ssh_keys)
        self.assertEqual(args.shell, "bash")
        self.assertEqual(args.profile, "python")

    def test_no_dangerous_flag(self):
        """Test --no-dangerous flag."""
        args = self.parser.parse_args(["/path", "--no-dangerous"])
        self.assertTrue(args.no_dangerous)

    def test_ssh_keys_flag(self):
        """Test --ssh-keys flag."""
        args = self.parser.parse_args(["/path", "--ssh-keys"])
        self.assertTrue(args.ssh_keys)

    def test_no_sudo_flag(self):
        """Test --no-sudo flag."""
        args = self.parser.parse_args(["/path", "--no-sudo"])
        self.assertTrue(args.no_sudo)

    def test_image_override(self):
        """Test --image option."""
        args = self.parser.parse_args(["/path", "--image", "custom:latest"])
        self.assertEqual(args.image, "custom:latest")

    def test_shell_override(self):
        """Test --shell option."""
        args = self.parser.parse_args(["/path", "--shell", "zsh"])
        self.assertEqual(args.shell, "zsh")

    def test_task_option(self):
        """Test -t/--task option."""
        args = self.parser.parse_args(["-t", "task.md"])
        self.assertEqual(args.task, "task.md")

        args = self.parser.parse_args(["--task", "another_task.md"])
        self.assertEqual(args.task, "another_task.md")

    def test_profile_override(self):
        """Test --profile option."""
        args = self.parser.parse_args(["/path", "--profile", "nodejs"])
        self.assertEqual(args.profile, "nodejs")

    def test_env_variables(self):
        """Test --env option."""
        args = self.parser.parse_args(["/path", "--env", "VAR1=value1", "--env", "VAR2=value2"])
        self.assertEqual(args.env, ["VAR1=value1", "VAR2=value2"])

    def test_api_key_from(self):
        """Test --api-key-from option."""
        args = self.parser.parse_args(["/path", "--api-key-from", "my-key"])
        self.assertEqual(args.api_key_from, "my-key")

    def test_no_firewall(self):
        """Test --no-firewall option."""
        args = self.parser.parse_args(["/path", "--no-firewall"])
        self.assertTrue(args.no_firewall)

    def test_prompt_flag(self):
        """Test -p/--prompt option."""
        args = self.parser.parse_args(["-p", "say hello and exit"])
        self.assertEqual(args.prompt, "say hello and exit")

        args = self.parser.parse_args(["--prompt", "say hello and exit"])
        self.assertEqual(args.prompt, "say hello and exit")


class TestPathValidation(unittest.TestCase):
    """Test path validation functionality."""

    def test_valid_directory(self):
        """Test validation of existing directory."""
        with tempfile.TemporaryDirectory() as temp_dir:
            path = validate_path(temp_dir)
            self.assertEqual(path, Path(temp_dir).resolve())

    def test_nonexistent_directory(self):
        """Test validation of non-existent directory."""
        with self.assertRaises(ValidationError) as cm:
            validate_path("/nonexistent/path")
        self.assertIn("Directory does not exist", str(cm.exception))

    def test_file_instead_of_directory(self):
        """Test validation when path points to a file."""
        with tempfile.NamedTemporaryFile() as temp_file:
            with self.assertRaises(ValidationError) as cm:
                validate_path(temp_file.name)
            self.assertIn("Path is not a directory", str(cm.exception))

    def test_relative_path_resolution(self):
        """Test that relative paths are resolved to absolute."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Change to temp directory and test relative path
            old_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)
                sub_dir = Path("subdir")
                sub_dir.mkdir()
                path = validate_path("subdir")
                self.assertTrue(path.is_absolute())
                self.assertEqual(path.name, "subdir")
            finally:
                os.chdir(old_cwd)


class TestDockerPathNormalization(unittest.TestCase):
    """Test Docker path normalization."""

    @patch("platform.system", return_value="Windows")
    def test_windows_path_normalization(self, mock_system: MagicMock) -> None:
        """Test Windows path conversion for Docker."""
        path = Path("C:\\Users\\test\\project")
        normalized = normalize_path_for_docker(path)
        self.assertEqual(normalized, "C:/Users/test/project")

    @patch("platform.system", return_value="Linux")
    def test_linux_path_normalization(self, mock_system: MagicMock) -> None:
        """Test Linux path normalization (no change)."""
        # Mock the actual path so it doesn't use Windows path separators
        with patch("pathlib.Path") as mock_path:
            mock_path_instance = Mock()
            mock_path_instance.__str__ = Mock(return_value="/home/user/project")
            mock_path.return_value = mock_path_instance

            normalized = normalize_path_for_docker(mock_path_instance)
            self.assertEqual(normalized, "/home/user/project")


class TestDockerDetection(unittest.TestCase):
    """Test Docker availability detection."""

    @patch("subprocess.run")
    def test_docker_available(self, mock_run: MagicMock) -> None:
        """Test when Docker is available."""
        mock_run.return_value = Mock()
        self.assertTrue(check_docker_available())
        mock_run.assert_called_once_with(["docker", "version"], capture_output=True, check=True, timeout=10)

    @patch("subprocess.run")
    def test_docker_unavailable(self, mock_run: MagicMock) -> None:
        """Test when Docker is not available."""
        mock_run.side_effect = FileNotFoundError()
        self.assertFalse(check_docker_available())

    @patch("subprocess.run")
    def test_docker_timeout(self, mock_run: MagicMock) -> None:
        """Test Docker command timeout."""
        mock_run.side_effect = subprocess.TimeoutExpired("docker", 10)
        self.assertFalse(check_docker_available())


class TestWrapperDetection(unittest.TestCase):
    """Test run-claude-docker wrapper detection."""

    @patch("shutil.which")
    def test_wrapper_found(self, mock_which: MagicMock) -> None:
        """Test when wrapper is found."""
        mock_which.return_value = "/usr/local/bin/run-claude-docker"
        self.assertEqual(find_run_claude_docker(), "/usr/local/bin/run-claude-docker")

    @patch("shutil.which")
    def test_wrapper_not_found(self, mock_which: MagicMock) -> None:
        """Test when wrapper is not found."""
        mock_which.return_value = None
        self.assertIsNone(find_run_claude_docker())


class TestSSHDirectory(unittest.TestCase):
    """Test SSH directory detection."""

    @patch("pathlib.Path.home")
    def test_ssh_dir_exists(self, mock_home: MagicMock) -> None:
        """Test when SSH directory exists."""
        temp_home = Path(tempfile.mkdtemp())
        ssh_dir = temp_home / ".ssh"
        ssh_dir.mkdir()
        mock_home.return_value = temp_home

        result = get_ssh_dir()
        self.assertEqual(result, ssh_dir)

        # Cleanup
        ssh_dir.rmdir()
        temp_home.rmdir()

    @patch("pathlib.Path.home")
    def test_ssh_dir_missing(self, mock_home: MagicMock) -> None:
        """Test when SSH directory doesn't exist."""
        temp_home = Path(tempfile.mkdtemp())
        mock_home.return_value = temp_home

        result = get_ssh_dir()
        self.assertIsNone(result)

        # Cleanup
        temp_home.rmdir()


class TestAPIKeyValidation(unittest.TestCase):
    """Test API key validation functionality."""

    def test_valid_api_key(self):
        """Test valid API key format."""
        valid_key = "sk-ant-1234567890123456789012345"
        self.assertTrue(validate_api_key(valid_key))

    def test_invalid_api_key_format(self):
        """Test invalid API key format."""
        # Wrong prefix
        self.assertFalse(validate_api_key("sk-openai-123456789012345"))

        # Too short
        self.assertFalse(validate_api_key("sk-ant-123"))

        # Empty
        self.assertFalse(validate_api_key(""))

        # None
        self.assertFalse(validate_api_key(None))


class TestConfigDirectory(unittest.TestCase):
    """Test config directory functionality."""

    @patch("pathlib.Path.home")
    def test_get_clud_config_dir(self, mock_home: MagicMock) -> None:
        """Test getting/creating config directory."""
        temp_home = Path(tempfile.mkdtemp())
        mock_home.return_value = temp_home

        config_dir = get_clud_config_dir()

        self.assertEqual(config_dir, temp_home / ".clud")
        self.assertTrue(config_dir.exists())

        # Cleanup
        config_dir.rmdir()
        temp_home.rmdir()

    @patch("pathlib.Path.home")
    def test_save_and_load_api_key_config(self, mock_home: MagicMock) -> None:
        """Test saving and loading API key from config."""
        temp_home = Path(tempfile.mkdtemp())
        mock_home.return_value = temp_home

        test_key = "sk-ant-test123456789012345"

        # Save API key
        save_api_key_to_config(test_key, "test-key")

        # Load API key
        loaded_key = load_api_key_from_config("test-key")
        self.assertEqual(loaded_key, test_key)

        # Test default key name
        save_api_key_to_config(test_key)
        loaded_default = load_api_key_from_config()
        self.assertEqual(loaded_default, test_key)

        # Cleanup
        config_dir = temp_home / ".clud"
        for key_file in config_dir.glob("*.key"):
            key_file.unlink()
        config_dir.rmdir()
        temp_home.rmdir()

    @patch("pathlib.Path.home")
    def test_load_missing_api_key_config(self, mock_home: MagicMock) -> None:
        """Test loading non-existent API key from config."""
        temp_home = Path(tempfile.mkdtemp())
        mock_home.return_value = temp_home

        result = load_api_key_from_config("nonexistent")
        self.assertIsNone(result)

        # Cleanup - remove .clud directory if it was created
        config_dir = temp_home / ".clud"
        if config_dir.exists():
            config_dir.rmdir()
        temp_home.rmdir()


class TestKeyringSupport(unittest.TestCase):
    """Test keyring functionality."""

    @patch("clud.agent_foreground.keyring")
    def test_keyring_success(self, mock_keyring: MagicMock) -> None:
        """Test successful keyring retrieval."""
        mock_keyring.get_password.return_value = "test-api-key"
        result = get_api_key_from_keyring("test-entry")
        self.assertEqual(result, "test-api-key")
        mock_keyring.get_password.assert_called_once_with("clud", "test-entry")

    @patch("clud.agent_foreground.keyring")
    def test_keyring_not_found(self, mock_keyring: MagicMock) -> None:
        """Test when keyring entry is not found."""
        mock_keyring.get_password.return_value = None
        with self.assertRaises(ConfigError) as cm:
            get_api_key_from_keyring("missing-entry")
        self.assertIn("No API key found", str(cm.exception))

    @patch("clud.agent_foreground.keyring", None)
    def test_keyring_not_available(self) -> None:
        """Test when keyring package is not available."""
        with self.assertRaises(ConfigError) as cm:
            get_api_key_from_keyring("test-entry")
        self.assertIn("No credential storage available", str(cm.exception))


class TestAPIKeyRetrieval(unittest.TestCase):
    """Test API key retrieval with priority order."""

    def setUp(self):
        """Set up test fixtures."""
        self.parser = create_parser()
        self.valid_key = "sk-ant-test123456789012345"

    @patch.dict(os.environ, {"ANTHROPIC_API_KEY": "sk-ant-env123456789012345"})
    def test_get_api_key_from_env(self):
        """Test getting API key from environment variable."""
        args = self.parser.parse_args(["/test/path"])

        with patch("clud.agent_foreground.load_api_key_from_config", return_value=None):
            api_key = get_api_key(args)
            self.assertEqual(api_key, "sk-ant-env123456789012345")

    @patch.dict(os.environ, {}, clear=True)
    def test_get_api_key_from_config(self):
        """Test getting API key from config file."""
        args = self.parser.parse_args(["/test/path"])

        with patch("clud.agent_foreground.load_api_key_from_config", return_value=self.valid_key):
            api_key = get_api_key(args)
            self.assertEqual(api_key, self.valid_key)

    @patch.dict(os.environ, {}, clear=True)
    def test_get_api_key_from_keyring(self):
        """Test getting API key from keyring via --api-key-from."""
        args = self.parser.parse_args(["/test/path", "--api-key-from", "test-entry"])

        with patch("clud.agent_foreground.keyring") as mock_keyring, patch("clud.agent_foreground.load_api_key_from_config", return_value=None):
            mock_keyring.get_password.return_value = self.valid_key
            api_key = get_api_key(args)
            self.assertEqual(api_key, self.valid_key)

    @patch.dict(os.environ, {}, clear=True)
    def test_get_api_key_prompt(self):
        """Test getting API key from interactive prompt."""
        args = self.parser.parse_args(["/test/path"])

        with patch("clud.agent_foreground.load_api_key_from_config", return_value=None), patch("clud.agent_foreground.prompt_for_api_key", return_value=self.valid_key):
            api_key = get_api_key(args)
            self.assertEqual(api_key, self.valid_key)

    def test_get_api_key_invalid(self):
        """Test validation error with invalid API key."""
        args = self.parser.parse_args(["/test/path"])

        with patch("clud.agent_foreground.load_api_key_from_config", return_value=None), patch("clud.agent_foreground.prompt_for_api_key", return_value="invalid-key"):
            with self.assertRaises(ForegroundValidationError) as cm:
                get_api_key(args)
            self.assertIn("Invalid API key format", str(cm.exception))


class TestWrapperCommand(unittest.TestCase):
    """Test run-claude-docker wrapper command building."""

    def setUp(self):
        """Set up test fixtures."""
        self.parser = create_parser()
        self.project_path = Path("/test/project")

    def test_basic_wrapper_command(self):
        """Test basic wrapper command."""
        args = self.parser.parse_args([str(self.project_path)])
        cmd = build_wrapper_command(args, self.project_path)

        expected = ["run-claude-docker", "--workspace", str(self.project_path), "--dangerously-skip-permissions", "--enable-sudo"]
        self.assertEqual(cmd, expected)

    def test_wrapper_with_no_dangerous(self):
        """Test wrapper command with --no-dangerous flag."""
        args = self.parser.parse_args([str(self.project_path), "--no-dangerous"])
        cmd = build_wrapper_command(args, self.project_path)

        self.assertNotIn("--dangerously-skip-permissions", cmd)

    def test_wrapper_with_custom_shell(self):
        """Test wrapper command with custom shell."""
        args = self.parser.parse_args([str(self.project_path), "--shell", "zsh"])
        cmd = build_wrapper_command(args, self.project_path)

        self.assertIn("--shell", cmd)
        self.assertIn("zsh", cmd)

    def test_wrapper_with_no_firewall(self):
        """Test wrapper command with firewall disabled."""
        args = self.parser.parse_args([str(self.project_path), "--no-firewall"])
        cmd = build_wrapper_command(args, self.project_path)

        self.assertIn("--disable-firewall", cmd)

    def test_wrapper_with_no_sudo(self):
        """Test wrapper command without sudo."""
        args = self.parser.parse_args([str(self.project_path), "--no-sudo"])
        cmd = build_wrapper_command(args, self.project_path)

        self.assertNotIn("--enable-sudo", cmd)


class TestFallbackCommand(unittest.TestCase):
    """Test direct Docker command building."""

    def setUp(self):
        """Set up test fixtures."""
        self.parser = create_parser()
        self.project_path = Path("/test/project")

    @patch.dict(os.environ, {}, clear=True)
    def test_basic_fallback_command(self):
        """Test basic fallback command."""
        args = self.parser.parse_args([str(self.project_path)])

        with patch("clud.agent_background.normalize_path_for_docker", return_value="/test/project"):
            cmd = build_fallback_command(args, self.project_path)

        self.assertEqual(cmd[0:4], ["docker", "run", "-it", "--rm"])
        self.assertIn("--name=clud-project", cmd)
        self.assertIn("--volume=/test/project:/host:rw", cmd)
        self.assertIn("niteris/clud:latest", cmd)
        self.assertIn("claude", cmd)
        self.assertIn("code", cmd)

    @patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"})
    def test_fallback_with_api_key(self):
        """Test fallback command with API key."""
        args = self.parser.parse_args([str(self.project_path)])

        with patch("clud.agent_background.normalize_path_for_docker", return_value="/test/project"):
            cmd = build_fallback_command(args, self.project_path)

        self.assertIn("-e", cmd)
        api_key_index = cmd.index("-e")
        self.assertEqual(cmd[api_key_index + 1], "ANTHROPIC_API_KEY=test-key")

    def test_fallback_with_ssh_keys(self):
        """Test fallback command with SSH keys."""
        args = self.parser.parse_args([str(self.project_path), "--ssh-keys"])

        with patch("clud.agent_background.normalize_path_for_docker") as mock_normalize, patch("clud.agent_background.get_ssh_dir", return_value=Path("/home/user/.ssh")):

            def normalize_side_effect(x: Any) -> str:
                return "/home/user/.ssh" if ".ssh" in str(x) else "/test/project"

            mock_normalize.side_effect = normalize_side_effect
            cmd = build_fallback_command(args, self.project_path)

        ssh_mount_found = any("--volume=/home/user/.ssh:/home/dev/.ssh:ro" in arg for arg in cmd)
        self.assertTrue(ssh_mount_found)

    def test_fallback_with_missing_ssh_keys(self):
        """Test fallback command with SSH keys when SSH dir doesn't exist."""
        args = self.parser.parse_args([str(self.project_path), "--ssh-keys"])

        with patch("clud.agent_background.get_ssh_dir", return_value=None):
            with self.assertRaises(ValidationError) as cm:
                build_fallback_command(args, self.project_path)
            self.assertIn("SSH directory ~/.ssh not found", str(cm.exception))

    @patch("platform.system", return_value="Linux")
    def test_fallback_with_no_sudo(self, mock_system: MagicMock) -> None:
        """Test fallback command without sudo on Linux."""
        args = self.parser.parse_args([str(self.project_path), "--no-sudo"])

        # Skip this test on Windows since getuid/getgid don't exist
        import platform

        if platform.system() == "Windows":
            self.skipTest("Cannot mock os.getuid/getgid on Windows")

        # Use mock.patch.object on the os module to create the attributes
        with (
            patch("clud.agent_background.normalize_path_for_docker", return_value="/test/project"),
            patch.object(os, "getuid", create=True, return_value=1000),
            patch.object(os, "getgid", create=True, return_value=1000),
        ):
            cmd = build_fallback_command(args, self.project_path)

        self.assertIn("--user", cmd)
        user_index = cmd.index("--user")
        self.assertEqual(cmd[user_index + 1], "1000:1000")

    def test_fallback_with_env_vars(self):
        """Test fallback command with custom environment variables."""
        args = self.parser.parse_args([str(self.project_path), "--env", "VAR1=value1", "--env", "VAR2=value2"])

        with patch("clud.agent_background.normalize_path_for_docker", return_value="/test/project"):
            cmd = build_fallback_command(args, self.project_path)

        self.assertIn("VAR1=value1", cmd)
        self.assertIn("VAR2=value2", cmd)

    def test_fallback_with_invalid_env_var(self):
        """Test fallback command with invalid environment variable."""
        args = self.parser.parse_args([str(self.project_path), "--env", "INVALID_FORMAT"])

        with patch("clud.agent_background.normalize_path_for_docker", return_value="/test/project"):
            with self.assertRaises(ValidationError) as cm:
                build_fallback_command(args, self.project_path)
            self.assertIn("Invalid environment variable format", str(cm.exception))


class TestClaudeCommandsMount(unittest.TestCase):
    """Test get_claude_commands_mount function."""

    def test_claude_commands_directory(self):
        """Test --claude-commands with directory."""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)

            # Create test plugin
            test_plugin = temp_path / "test.md"
            test_plugin.write_text("# Test Plugin\nThis is a test.")

            result = get_claude_commands_mount(str(temp_path))
            self.assertIsNotNone(result)
            assert result is not None  # Type checker hint
            host_path, container_path = result

            self.assertEqual(container_path, "/plugins")

    def test_claude_commands_file(self):
        """Test --claude-commands with single file."""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)

            # Create test plugin
            test_plugin = temp_path / "single.md"
            test_plugin.write_text("# Single Plugin\nThis is a single test.")

            result = get_claude_commands_mount(str(test_plugin))
            self.assertIsNotNone(result)
            assert result is not None  # Type checker hint
            host_path, container_path = result

            self.assertEqual(container_path, "/plugins/single.md")

    def test_claude_commands_nonexistent(self):
        """Test --claude-commands with non-existent path."""
        with self.assertRaises(ValidationError):
            get_claude_commands_mount("/nonexistent/path")

    def test_claude_commands_non_md_file(self):
        """Test --claude-commands with non-.md file."""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)

            # Create non-md file
            non_md_file = temp_path / "test.txt"
            non_md_file.write_text("Not a markdown file")

            with self.assertRaises(ValidationError):
                get_claude_commands_mount(str(non_md_file))

    def test_claude_commands_none(self):
        """Test --claude-commands with None."""
        result = get_claude_commands_mount(None)
        self.assertIsNone(result)


class TestMainFunction(unittest.TestCase):
    """Test main function integration."""

    @patch("clud.cli.get_api_key", return_value="sk-ant-test123456789012345")
    @patch("clud.cli.validate_path", return_value=Path("/test/path"))
    @patch("clud.cli.check_docker_available", return_value=False)
    def test_main_docker_unavailable(self, mock_check: MagicMock, mock_validate: MagicMock, mock_api_key: MagicMock) -> None:
        """Test main function when Docker is unavailable."""
        with patch("sys.argv", ["clud", "/test/path"]):
            result = main()
            self.assertEqual(result, 3)  # Docker error exit code

    @patch("clud.cli.validate_path", side_effect=ValidationError("Invalid path"))
    def test_main_validation_error(self, mock_validate: MagicMock) -> None:
        """Test main function with validation error."""
        with patch("sys.argv", ["clud", "/invalid/path"]):
            result = main()
            self.assertEqual(result, 2)  # Validation error exit code

    @patch("clud.cli.validate_path", return_value=Path("/test/path"))
    @patch("clud.cli.get_api_key", side_effect=ConfigError("Config error"))
    def test_main_config_error(self, mock_api_key: MagicMock, mock_validate: MagicMock) -> None:
        """Test main function with config error."""
        with patch("sys.argv", ["clud", "/test/path"]):
            result = main()
            self.assertEqual(result, 4)  # Config error exit code

    @patch("clud.cli.validate_path", return_value=Path("/test/path"))
    @patch("clud.cli.get_api_key", side_effect=ValidationError("Invalid API key"))
    def test_main_api_key_validation_error(self, mock_api_key: MagicMock, mock_validate: MagicMock) -> None:
        """Test main function with API key validation error."""
        with patch("sys.argv", ["clud", "/test/path"]):
            result = main()
            self.assertEqual(result, 2)  # Validation error exit code


if __name__ == "__main__":
    unittest.main()

"""Tests for Claude Code installation manager."""

import unittest
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

from clud.claude_installer import (
    detect_npm_error_type,
    install_claude_local,
    print_installation_troubleshooting,
    try_global_npm_install,
    try_specific_version_install,
)


class TestDetectNpmErrorType(unittest.TestCase):
    """Test npm error type detection."""

    def test_detect_module_not_found(self) -> None:
        """Test detection of module not found errors."""
        stderr = "Error: Cannot find module '../lib/cli.js'"
        self.assertEqual(detect_npm_error_type(stderr), "module_not_found")

    def test_detect_module_not_found_uppercase(self) -> None:
        """Test detection of MODULE_NOT_FOUND errors."""
        stderr = "code: 'MODULE_NOT_FOUND'"
        self.assertEqual(detect_npm_error_type(stderr), "module_not_found")

    def test_detect_permission_error(self) -> None:
        """Test detection of permission errors."""
        stderr = "npm ERR! code EACCES"
        self.assertEqual(detect_npm_error_type(stderr), "permission")

    def test_detect_permission_denied(self) -> None:
        """Test detection of permission denied errors."""
        stderr = "Error: permission denied"
        self.assertEqual(detect_npm_error_type(stderr), "permission")

    def test_detect_network_error(self) -> None:
        """Test detection of network errors."""
        stderr = "npm ERR! code ENOTFOUND"
        self.assertEqual(detect_npm_error_type(stderr), "network")

    def test_detect_network_error_lowercase(self) -> None:
        """Test detection of network-related errors."""
        stderr = "Error: network timeout"
        self.assertEqual(detect_npm_error_type(stderr), "network")

    def test_detect_unknown_error(self) -> None:
        """Test detection of unknown errors."""
        stderr = "Some random error message"
        self.assertEqual(detect_npm_error_type(stderr), "unknown")


class TestPrintInstallationTroubleshooting(unittest.TestCase):
    """Test installation troubleshooting message printing."""

    @patch("sys.stderr")
    def test_print_module_not_found_guidance(self, mock_stderr: Mock) -> None:
        """Test guidance for module not found errors."""
        print_installation_troubleshooting("module_not_found")
        # Verify function runs without errors
        self.assertTrue(True)

    @patch("sys.stderr")
    def test_print_permission_guidance(self, mock_stderr: Mock) -> None:
        """Test guidance for permission errors."""
        print_installation_troubleshooting("permission")
        # Verify function runs without errors
        self.assertTrue(True)

    @patch("sys.stderr")
    def test_print_network_guidance(self, mock_stderr: Mock) -> None:
        """Test guidance for network errors."""
        print_installation_troubleshooting("network")
        # Verify function runs without errors
        self.assertTrue(True)

    @patch("sys.stderr")
    def test_print_unknown_guidance(self, mock_stderr: Mock) -> None:
        """Test guidance for unknown errors."""
        print_installation_troubleshooting("unknown")
        # Verify function runs without errors
        self.assertTrue(True)


class TestTryGlobalNpmInstall(unittest.TestCase):
    """Test global npm installation fallback with isolated prefix."""

    @patch("clud.claude_installer.subprocess.run")
    def test_global_install_success(self, mock_run: Mock) -> None:
        """Test successful global installation with isolated prefix."""
        mock_run.return_value.returncode = 0
        result = try_global_npm_install("/usr/bin/npm", verbose=False)
        self.assertTrue(result)
        # Verify npm -g was called with NPM_CONFIG_PREFIX env var
        mock_run.assert_called_once()
        call_args = mock_run.call_args
        self.assertIn("env", call_args.kwargs)
        self.assertIn("NPM_CONFIG_PREFIX", call_args.kwargs["env"])

    @patch("clud.claude_installer.subprocess.run")
    def test_global_install_failure(self, mock_run: Mock) -> None:
        """Test failed global installation."""
        mock_run.return_value.returncode = 1
        result = try_global_npm_install("/usr/bin/npm", verbose=False)
        self.assertFalse(result)

    @patch("clud.claude_installer.subprocess.run")
    def test_global_install_exception(self, mock_run: Mock) -> None:
        """Test global installation with exception."""
        mock_run.side_effect = Exception("Test exception")
        result = try_global_npm_install("/usr/bin/npm", verbose=False)
        self.assertFalse(result)


class TestTrySpecificVersionInstall(unittest.TestCase):
    """Test specific version installation fallback."""

    @patch("clud.claude_installer.RunningProcess.run_streaming")
    @patch("clud.claude_installer.get_clud_npm_dir")
    def test_specific_version_install_success(self, mock_npm_dir: Mock, mock_run: Mock) -> None:
        """Test successful specific version installation."""
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")
        mock_run.return_value = 0
        result = try_specific_version_install("/usr/bin/npm", "0.6.0", verbose=False)
        self.assertTrue(result)
        mock_run.assert_called_once()

    @patch("clud.claude_installer.RunningProcess.run_streaming")
    @patch("clud.claude_installer.get_clud_npm_dir")
    def test_specific_version_install_failure(self, mock_npm_dir: Mock, mock_run: Mock) -> None:
        """Test failed specific version installation."""
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")
        mock_run.return_value = 1
        result = try_specific_version_install("/usr/bin/npm", "0.6.0", verbose=False)
        self.assertFalse(result)

    @patch("clud.claude_installer.RunningProcess.run_streaming")
    @patch("clud.claude_installer.get_clud_npm_dir")
    def test_specific_version_install_exception(self, mock_npm_dir: Mock, mock_run: Mock) -> None:
        """Test specific version installation with exception."""
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")
        mock_run.side_effect = Exception("Test exception")
        result = try_specific_version_install("/usr/bin/npm", "0.6.0", verbose=False)
        self.assertFalse(result)


class TestInstallClaudeLocal(unittest.TestCase):
    """Test main Claude Code installation function with fallback logic."""

    @patch("clud.claude_installer.get_local_claude_path")
    @patch("clud.claude_installer.subprocess.Popen")
    @patch("clud.claude_installer.get_clud_npm_dir")
    @patch("clud.claude_installer.find_npm_executable")
    def test_install_success_first_try(self, mock_find_npm: Mock, mock_npm_dir: Mock, mock_popen: Mock, mock_claude_path: Mock) -> None:
        """Test successful installation on first try."""
        mock_find_npm.return_value = "/usr/bin/npm"
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")

        # Mock process - stderr must be iterable (empty iterator)
        mock_process = MagicMock()
        mock_process.stderr = iter([])  # Empty iterator for stderr
        mock_process.wait.return_value = 0
        mock_popen.return_value = mock_process

        # Mock Claude path with exists() method
        mock_path = MagicMock()
        mock_path.exists.return_value = True
        mock_claude_path.return_value = mock_path

        result = install_claude_local(verbose=False)
        self.assertTrue(result)

    @patch("clud.claude_installer.get_local_claude_path")
    @patch("clud.claude_installer.try_global_npm_install")
    @patch("clud.claude_installer.subprocess.Popen")
    @patch("clud.claude_installer.get_clud_npm_dir")
    @patch("clud.claude_installer.find_npm_executable")
    def test_install_module_not_found_with_global_fallback_success(
        self,
        mock_find_npm: Mock,
        mock_npm_dir: Mock,
        mock_popen: Mock,
        mock_global: Mock,
        mock_claude_path: Mock,
    ) -> None:
        """Test installation with module_not_found error and successful global fallback."""
        mock_find_npm.return_value = "/usr/bin/npm"
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")

        # Mock process with module not found error - stderr must be iterable
        mock_process = MagicMock()
        mock_process.stderr = iter(["Error: Cannot find module '../lib/cli.js'\n"])
        mock_process.wait.return_value = 1
        mock_popen.return_value = mock_process

        # Global install succeeds
        mock_global.return_value = True

        # Mock Claude path with exists() method
        mock_path = MagicMock()
        mock_path.exists.return_value = True
        mock_claude_path.return_value = mock_path

        result = install_claude_local(verbose=False)
        self.assertTrue(result)
        mock_global.assert_called_once()

    @patch("clud.claude_installer.get_local_claude_path")
    @patch("clud.claude_installer.try_specific_version_install")
    @patch("clud.claude_installer.try_global_npm_install")
    @patch("clud.claude_installer.subprocess.Popen")
    @patch("clud.claude_installer.get_clud_npm_dir")
    @patch("clud.claude_installer.find_npm_executable")
    def test_install_module_not_found_with_version_fallback_success(
        self,
        mock_find_npm: Mock,
        mock_npm_dir: Mock,
        mock_popen: Mock,
        mock_global: Mock,
        mock_version: Mock,
        mock_claude_path: Mock,
    ) -> None:
        """Test installation with module_not_found error, global fails, version succeeds."""
        mock_find_npm.return_value = "/usr/bin/npm"
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")

        # Mock process with module not found error - stderr must be iterable
        mock_process = MagicMock()
        mock_process.stderr = iter(["Error: Cannot find module '../lib/cli.js'\n"])
        mock_process.wait.return_value = 1
        mock_popen.return_value = mock_process

        # Global install fails, version install succeeds
        mock_global.return_value = False
        mock_version.return_value = True

        # Mock Claude path with exists() method
        mock_path = MagicMock()
        mock_path.exists.return_value = True
        mock_claude_path.return_value = mock_path

        result = install_claude_local(verbose=False)
        self.assertTrue(result)
        mock_global.assert_called_once()
        mock_version.assert_called_once()

    @patch("clud.claude_installer.print_installation_troubleshooting")
    @patch("clud.claude_installer.try_specific_version_install")
    @patch("clud.claude_installer.try_global_npm_install")
    @patch("clud.claude_installer.subprocess.Popen")
    @patch("clud.claude_installer.get_clud_npm_dir")
    @patch("clud.claude_installer.find_npm_executable")
    def test_install_all_methods_fail(
        self,
        mock_find_npm: Mock,
        mock_npm_dir: Mock,
        mock_popen: Mock,
        mock_global: Mock,
        mock_version: Mock,
        mock_troubleshoot: Mock,
    ) -> None:
        """Test installation when all methods fail."""
        mock_find_npm.return_value = "/usr/bin/npm"
        mock_npm_dir.return_value = Path("/home/user/.clud/npm")

        # Mock process with module not found error - stderr must be iterable
        mock_process = MagicMock()
        mock_process.stderr = iter(["Error: Cannot find module '../lib/cli.js'\n"])
        mock_process.wait.return_value = 1
        mock_popen.return_value = mock_process

        # Both fallbacks fail
        mock_global.return_value = False
        mock_version.return_value = False

        result = install_claude_local(verbose=False)
        self.assertFalse(result)
        mock_troubleshoot.assert_called_once_with("module_not_found")

    @patch("clud.claude_installer.find_npm_executable")
    def test_install_npm_not_found(self, mock_find_npm: Mock) -> None:
        """Test installation when npm is not found."""
        mock_find_npm.return_value = None
        result = install_claude_local(verbose=False)
        self.assertFalse(result)


if __name__ == "__main__":
    unittest.main()

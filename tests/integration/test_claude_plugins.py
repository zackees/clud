#!/usr/bin/env -S uv run python
"""Integration tests for Claude CLI plugin installation functionality."""

import os
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

from clud.testing.docker_test_utils import DockerTestImageManager


class PluginTestError(Exception):
    """Exception raised when plugin test fails."""

    pass


def test_claude_plugin_mount_directory():
    """Test mounting a directory of Claude plugins."""
    print("Testing Claude plugin directory mounting...")
    print("=" * 60)

    container_name = f"clud-plugin-dir-test-{uuid.uuid4().hex[:8]}"

    # Create temporary test directory with plugin files
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)
        plugins_dir = temp_path / "test_plugins"
        plugins_dir.mkdir()

        # Create test plugin files
        test_plugin = plugins_dir / "test.md"
        test_plugin.write_text("""# Test Plugin

This is a test plugin for integration testing.

## Usage
Type `/test` to use this plugin.
""")

        another_plugin = plugins_dir / "deploy.md"
        another_plugin.write_text("""# Deploy Plugin

This plugin helps with deployment tasks.

## Usage
Type `/deploy` to use this plugin.
""")

        try:
            # Start container with plugin directory mounted
            run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{plugins_dir}:/root/.claude/commands:ro", "clud-test:latest", "--cmd", "sleep 300"]

            # Use MSYS_NO_PATHCONV to prevent Git Bash from converting paths
            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)
            container_id = result.stdout.strip()
            print(f"OK Container started: {container_id[:12]}")

            # Wait for container to be ready
            time.sleep(3)

            # Test 1: Verify plugins directory exists
            check_dir_cmd = ["docker", "exec", container_name, "test", "-d", "/root/.claude/commands"]
            dir_check = subprocess.run(check_dir_cmd, capture_output=True)

            if dir_check.returncode != 0:
                raise PluginTestError("Claude commands directory not found in container")

            print("OK Claude commands directory exists in container")

            # Test 2: Verify test plugin is mounted
            check_test_plugin_cmd = ["docker", "exec", container_name, "cat", "/root/.claude/commands/test.md"]
            test_plugin_result = subprocess.run(check_test_plugin_cmd, capture_output=True, text=True)

            if test_plugin_result.returncode != 0:
                raise PluginTestError("test.md plugin not found in container")

            if "This is a test plugin for integration testing" not in test_plugin_result.stdout:
                raise PluginTestError("test.md plugin content not correct")

            print("OK test.md plugin mounted and accessible")

            # Test 3: Verify deploy plugin is mounted
            check_deploy_plugin_cmd = ["docker", "exec", container_name, "cat", "/root/.claude/commands/deploy.md"]
            deploy_plugin_result = subprocess.run(check_deploy_plugin_cmd, capture_output=True, text=True)

            if deploy_plugin_result.returncode != 0:
                raise PluginTestError("deploy.md plugin not found in container")

            if "This plugin helps with deployment tasks" not in deploy_plugin_result.stdout:
                raise PluginTestError("deploy.md plugin content not correct")

            print("OK deploy.md plugin mounted and accessible")

            # Test 4: List all plugins to verify count
            list_plugins_cmd = ["docker", "exec", container_name, "ls", "-la", "/root/.claude/commands"]
            list_result = subprocess.run(list_plugins_cmd, capture_output=True, text=True)

            if "test.md" not in list_result.stdout or "deploy.md" not in list_result.stdout:
                raise PluginTestError("Not all plugins found in listing")

            print("OK All plugins present in directory listing")

        except subprocess.CalledProcessError as e:
            print(f"Docker command failed: {e}")
            if e.stderr:
                print(f"Error output: {e.stderr}")
            raise PluginTestError(f"Docker command failed: {e}") from e

        finally:
            # Cleanup
            try:
                subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
                print("OK Container cleanup completed")
            except Exception:
                pass


def test_claude_plugin_mount_single_file():
    """Test mounting a single Claude plugin file."""
    print("\nTesting Claude single plugin file mounting...")
    print("=" * 60)

    container_name = f"clud-plugin-file-test-{uuid.uuid4().hex[:8]}"

    # Create temporary test file
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)
        single_plugin = temp_path / "single.md"
        single_plugin.write_text("""# Single Plugin

This is a single plugin file for testing.

## Usage
Type `/single` to use this plugin.

## Features
- Single file mounting
- Integration testing
""")

        try:
            # Start container with single plugin file mounted
            run_cmd = ["docker", "run", "-d", "--name", container_name, "-v", f"{single_plugin}:/root/.claude/commands/single.md:ro", "clud-test:latest", "--cmd", "sleep 300"]

            # Use MSYS_NO_PATHCONV to prevent Git Bash from converting paths
            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            result = subprocess.run(run_cmd, check=True, capture_output=True, text=True, env=env)
            container_id = result.stdout.strip()
            print(f"OK Container started: {container_id[:12]}")

            # Wait for container to be ready
            time.sleep(3)

            # Test 1: Verify single plugin file exists
            check_file_cmd = ["docker", "exec", container_name, "test", "-f", "/root/.claude/commands/single.md"]
            file_check = subprocess.run(check_file_cmd, capture_output=True)

            if file_check.returncode != 0:
                raise PluginTestError("single.md plugin file not found in container")

            print("OK single.md plugin file exists in container")

            # Test 2: Verify plugin content
            check_content_cmd = ["docker", "exec", container_name, "cat", "/root/.claude/commands/single.md"]
            content_result = subprocess.run(check_content_cmd, capture_output=True, text=True)

            if content_result.returncode != 0:
                raise PluginTestError("Failed to read single.md plugin content")

            expected_content = "This is a single plugin file for testing"
            if expected_content not in content_result.stdout:
                raise PluginTestError("single.md plugin content not correct")

            print("OK single.md plugin content is correct")

            # Test 3: Verify the single file is present and accessible
            # (Note: The container may have built-in plugins like example.md)
            list_plugins_cmd = ["docker", "exec", container_name, "ls", "/root/.claude/commands"]
            list_result = subprocess.run(list_plugins_cmd, capture_output=True, text=True)

            plugins_found = list_result.stdout.strip().split("\n")
            plugins_found = [p.strip() for p in plugins_found if p.strip()]

            if "single.md" not in plugins_found:
                raise PluginTestError(f"single.md not found in plugins: {plugins_found}")

            print(f"OK single.md plugin found among plugins: {plugins_found}")

        except subprocess.CalledProcessError as e:
            print(f"Docker command failed: {e}")
            if e.stderr:
                print(f"Error output: {e.stderr}")
            raise PluginTestError(f"Docker command failed: {e}") from e

        finally:
            # Cleanup
            try:
                subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, check=False)
                print("OK Container cleanup completed")
            except Exception:
                pass


def test_claude_plugin_cli_integration():
    """Test CLI --claude-commands option integration."""
    print("\nTesting CLI --claude-commands option integration...")
    print("=" * 60)

    # Create temporary test directory with plugin files
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)
        project_dir = temp_path / "test_project"
        project_dir.mkdir()
        plugins_dir = temp_path / "cli_plugins"
        plugins_dir.mkdir()

        # Create test plugin
        cli_plugin = plugins_dir / "cliplugin.md"
        cli_plugin.write_text("""# CLI Plugin

This plugin tests CLI integration.

## Usage
Type `/cliplugin` to use this plugin.
""")

        # Create a simple project file
        (project_dir / "test.py").write_text("print('hello world')")

        try:
            # Test with our clud CLI using --claude-commands and --cmd
            test_cmd = [
                "python",
                "-m",
                "clud.cli",
                str(project_dir),
                "--claude-commands",
                str(plugins_dir),
                "--cmd",
                "ls -la /root/.claude/commands | grep cliplugin && exit 0 || echo 'FAILED' && exit 1",
            ]

            # Set environment variables
            env = os.environ.copy()
            env["MSYS_NO_PATHCONV"] = "1"
            env["ANTHROPIC_API_KEY"] = "sk-ant-test-key-for-testing-only"

            print(f"Running CLI command: {' '.join(test_cmd)}")
            result = subprocess.run(test_cmd, capture_output=True, text=True, env=env, timeout=120)

            print(f"CLI command exit code: {result.returncode}")
            if result.stdout:
                print(f"Stdout: {result.stdout}")
            if result.stderr:
                print(f"Stderr: {result.stderr}")

            if result.returncode != 0:
                raise PluginTestError(f"CLI command failed with exit code {result.returncode}")

            if "cliplugin.md" not in result.stdout:
                raise PluginTestError("Plugin file not found in container via CLI")

            print("OK CLI --claude-commands option works correctly")

        except subprocess.TimeoutExpired:
            raise PluginTestError("CLI command timed out") from None
        except subprocess.CalledProcessError as e:
            print(f"CLI command failed: {e}")
            if e.stderr:
                print(f"Error output: {e.stderr}")
            raise PluginTestError(f"CLI command failed: {e}") from e


def main():
    """Main test function."""
    print("Starting Claude plugin integration tests...")

    # Check Docker availability
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        print("OK Docker is available")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        print("X Docker is not available")
        return 1

    # Build test image if needed
    try:
        image_manager = DockerTestImageManager()
        image_id = image_manager.ensure_image_ready()
        print(f"OK Test image ready: {image_id[:12]}")
    except Exception as e:
        print(f"Failed to build test image: {e}")
        return 1

    try:
        # Test plugin directory mounting
        test_claude_plugin_mount_directory()
        print("OK Plugin directory mounting test passed")

        # Test single plugin file mounting
        test_claude_plugin_mount_single_file()
        print("OK Single plugin file mounting test passed")

        # Test CLI integration
        test_claude_plugin_cli_integration()
        print("OK CLI integration test passed")

        print("\n" + "=" * 60)
        print("SUCCESS: All Claude plugin integration tests passed!")
        print("This proves that:")
        print("- Plugin directories can be mounted and accessed")
        print("- Single plugin files can be mounted correctly")
        print("- CLI --claude-commands option works as expected")
        print("- Plugins are accessible at /root/.claude/commands")
        return 0

    except PluginTestError as e:
        print(f"\nFAILED: {e}")
        return 1

    except Exception as e:
        print(f"\nERROR: Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())

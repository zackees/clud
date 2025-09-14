"""Unit tests for the docker manager."""

import subprocess
import time
import unittest
from typing import Any
from unittest.mock import MagicMock, patch

from clud.docker.docker_manager import DockerManager, Volume


class TestDockerManager(unittest.TestCase):
    """Test cases for DockerManager."""

    @classmethod
    def setUpClass(cls):
        """Set up test class."""
        cls.test_image = "alpine"
        cls.test_tag = "latest"
        cls.test_container_name = "clud-test-container"

    def setUp(self):
        """Set up each test."""
        self.docker_manager = DockerManager()

    def tearDown(self):
        """Clean up after each test."""
        # Clean up test containers
        try:
            container = self.docker_manager.get_container(self.test_container_name)
            if container:
                container.remove(force=True)
        except Exception as e:
            print(f"Warning: Failed to clean up test container {self.test_container_name}: {e}")

    def test_is_docker_installed(self):
        """Test Docker installation check."""
        result = DockerManager.is_docker_installed()
        self.assertIsInstance(result, bool)
        if result:
            print("Docker is installed and available for testing")
        else:
            self.skipTest("Docker is not installed, skipping Docker tests")

    def test_is_running(self):
        """Test Docker daemon running check."""
        if not DockerManager.is_docker_installed():
            self.skipTest("Docker is not installed")

        running, error = DockerManager.is_running()
        self.assertIsInstance(running, bool)
        if running:
            print("Docker daemon is running")
        else:
            print(f"Docker daemon is not running: {error}")

    def test_validate_or_download_image(self):
        """Test image validation and download."""
        if not DockerManager.is_docker_installed():
            self.skipTest("Docker is not installed")

        running, _ = DockerManager.is_running()
        if not running:
            self.skipTest("Docker daemon is not running")

        # Test with Alpine image (smallest available)
        result = self.docker_manager.validate_or_download_image(self.test_image, self.test_tag)
        self.assertIsInstance(result, bool)
        print(f"Image {self.test_image}:{self.test_tag} validation result: {result}")

    def test_volume_creation(self):
        """Test Volume object creation and conversion."""
        volume = Volume(host_path="/tmp/test", container_path="/app/test", mode="rw")

        self.assertEqual(volume.host_path, "/tmp/test")
        self.assertEqual(volume.container_path, "/app/test")
        self.assertEqual(volume.mode, "rw")

        # Test to_dict conversion
        volume_dict = volume.to_dict()
        expected = {"/tmp/test": {"bind": "/app/test", "mode": "rw"}}
        self.assertEqual(volume_dict, expected)

    def test_volume_from_dict(self):
        """Test Volume creation from dictionary."""
        volume_dict = {"/host/path": {"bind": "/container/path", "mode": "ro"}}

        volumes = Volume.from_dict(volume_dict)
        self.assertEqual(len(volumes), 1)
        self.assertEqual(volumes[0].host_path, "/host/path")
        self.assertEqual(volumes[0].container_path, "/container/path")
        self.assertEqual(volumes[0].mode, "ro")

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_run_container_detached(self):
        """Test running a container in detached mode."""
        # Use Alpine with a simple command that exits quickly
        container = self.docker_manager.run_container_detached(
            image_name=self.test_image, tag=self.test_tag, container_name=self.test_container_name, command="echo 'Hello from Alpine container'", remove_previous=True
        )

        self.assertIsNotNone(container)
        self.assertEqual(container.name, self.test_container_name)

        # Wait a moment for the command to complete
        time.sleep(2)

        # Check if container exists (it might have exited after echo command)
        retrieved_container = self.docker_manager.get_container(self.test_container_name)
        if retrieved_container:
            print(f"Container status: {retrieved_container.status}")

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_long_running_container(self):
        """Test running a long-running container."""
        # Use Alpine with a sleep command
        container = self.docker_manager.run_container_detached(
            image_name=self.test_image, tag=self.test_tag, container_name=self.test_container_name + "-long", command="sleep 10", remove_previous=True
        )

        self.assertIsNotNone(container)

        # Check if container is running
        self.assertIsNotNone(container.name)
        assert container.name is not None  # Type narrowing for mypy/pyright
        is_running = self.docker_manager.is_container_running(container.name)
        self.assertTrue(is_running)

        # Suspend the container
        self.docker_manager.suspend_container(container)

        # Clean up
        try:
            container.remove(force=True)
        except Exception as e:
            print(f"Warning: Failed to remove test container {container.name}: {e}")

    def test_get_container_nonexistent(self):
        """Test getting a non-existent container."""
        container = self.docker_manager.get_container("nonexistent-container")
        self.assertIsNone(container)

    def test_is_container_running_nonexistent(self):
        """Test checking if a non-existent container is running."""
        is_running = self.docker_manager.is_container_running("nonexistent-container")
        self.assertFalse(is_running)

    @patch("clud.docker.docker_manager.subprocess.run")
    def test_is_docker_installed_mocked(self, mock_run: Any):
        """Test Docker installation check with mocked subprocess."""
        # Test successful Docker installation
        mock_run.return_value = MagicMock(returncode=0)
        result = DockerManager.is_docker_installed()
        self.assertTrue(result)

        # Test Docker not installed
        mock_run.side_effect = FileNotFoundError()
        result = DockerManager.is_docker_installed()
        self.assertFalse(result)

        # Test Docker command failure
        mock_run.side_effect = subprocess.CalledProcessError(1, "docker")
        result = DockerManager.is_docker_installed()
        self.assertFalse(result)


if __name__ == "__main__":
    unittest.main()

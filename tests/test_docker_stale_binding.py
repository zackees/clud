"""Unit tests to reproduce Docker manager stale binding issues."""

import contextlib
import subprocess
import threading
import time
import unittest
from unittest.mock import MagicMock, patch

from clud.cli import stop_existing_container
from clud.docker.docker_manager import DockerManager


class TestDockerStaleBinding(unittest.TestCase):
    """Test cases for Docker manager stale binding issues."""

    def setUp(self):
        """Set up each test."""
        self.docker_manager = DockerManager()
        self.test_container_name = "clud-dev-test"

    def tearDown(self):
        """Clean up after each test."""
        # Clean up any test containers using both approaches
        with contextlib.suppress(Exception):
            stop_existing_container(self.test_container_name)

        with contextlib.suppress(Exception):
            container = self.docker_manager.get_container(self.test_container_name)
            if container:
                container.remove(force=True)

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_cli_vs_api_container_management_conflict(self):
        """Test conflict between CLI-based and API-based container management."""
        # Create a container using the Docker API
        self.docker_manager.run_container_detached(image_name="alpine", tag="latest", container_name=self.test_container_name, command="sleep 30", remove_previous=True)

        # Verify container exists via API
        api_container = self.docker_manager.get_container(self.test_container_name)
        self.assertIsNotNone(api_container)

        # Remove container using CLI method (simulating the issue)
        stop_existing_container(self.test_container_name)

        # Now the API reference should be stale
        try:
            # This should fail or return stale data
            api_container.reload()
            status = api_container.status
            # If we get here, the container reference is stale but still appears valid
            self.fail(f"Expected stale container reference, but got status: {status}")
        except Exception as e:
            # This is expected - the container reference is stale
            print(f"Expected stale reference error: {e}")

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_rapid_container_lifecycle_operations(self):
        """Test rapid start/stop/remove operations that could cause race conditions."""
        # Rapidly create and destroy containers
        for i in range(3):
            container_name = f"{self.test_container_name}-{i}"

            # Create container
            self.docker_manager.run_container_detached(image_name="alpine", tag="latest", container_name=container_name, command="echo 'test'", remove_previous=True)

            # Immediately try to manage it with CLI
            time.sleep(0.1)  # Small delay to simulate timing issue
            stop_existing_container(container_name)

            # Verify container is gone
            final_container = self.docker_manager.get_container(container_name)
            self.assertIsNone(final_container)

    def test_stop_existing_container_with_nonexistent_container(self):
        """Test CLI stop function with non-existent container."""
        # Should not raise exception
        try:
            stop_existing_container("nonexistent-container-name")
        except Exception as e:
            self.fail(f"stop_existing_container raised exception for non-existent container: {e}")

    @patch("subprocess.run")
    def test_docker_cli_failure_handling(self, mock_run: MagicMock) -> None:
        """Test handling of Docker CLI command failures."""
        # Simulate docker ps failure
        mock_run.side_effect = subprocess.CalledProcessError(1, "docker ps")

        # Should not raise exception
        try:
            stop_existing_container("test-container")
        except Exception as e:
            self.fail(f"stop_existing_container should handle CLI failures gracefully: {e}")

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_container_state_consistency_after_cli_operations(self):
        """Test that Docker API state remains consistent after CLI operations."""
        # Create container via API
        container = self.docker_manager.run_container_detached(image_name="alpine", tag="latest", container_name=self.test_container_name, command="sleep 10", remove_previous=True)

        # Perform CLI operations
        subprocess.run(["docker", "stop", self.test_container_name], check=True, capture_output=True)
        subprocess.run(["docker", "rm", self.test_container_name], check=True, capture_output=True)

        # Check API consistency
        container_after_cli = self.docker_manager.get_container(self.test_container_name)
        self.assertIsNone(container_after_cli, "Container should not exist after CLI removal")

        # Verify original container reference is stale
        try:
            container.reload()
            self.fail("Original container reference should be stale after CLI removal")
        except Exception:
            # Expected - container reference is stale
            pass

    @unittest.skipUnless(DockerManager.is_docker_installed() and DockerManager.is_running()[0], "Docker is not available")
    def test_concurrent_container_operations(self):
        """Test concurrent container operations that might cause binding issues."""

        results: list[str] = []
        errors: list[str] = []

        def create_container(index: int) -> None:
            try:
                container_name = f"{self.test_container_name}-concurrent-{index}"
                self.docker_manager.run_container_detached(image_name="alpine", tag="latest", container_name=container_name, command="echo 'concurrent test'", remove_previous=True)
                results.append(container_name)

                # Immediately try to stop it
                stop_existing_container(container_name)

            except Exception as e:
                errors.append(f"Thread {index}: {e}")

        # Run multiple threads concurrently
        threads: list[threading.Thread] = []
        for i in range(3):
            thread = threading.Thread(target=create_container, args=(i,))
            threads.append(thread)
            thread.start()

        # Wait for all threads
        for thread in threads:
            thread.join()

        # Check results
        if errors:
            self.fail(f"Concurrent operations failed: {errors}")

        self.assertEqual(len(results), 3, "All concurrent operations should succeed")


if __name__ == "__main__":
    unittest.main()

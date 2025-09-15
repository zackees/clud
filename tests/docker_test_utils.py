"""Docker test utilities for efficient image building and caching."""

import hashlib
import json
import subprocess
import time
from pathlib import Path
from typing import Any


class DockerTestImageManager:
    """Manages Docker test image building with caching and staleness detection."""

    def __init__(self, image_name: str = "clud-test", tag: str = "latest"):
        self.image_name = image_name
        self.tag = tag
        self.full_image_name = f"{image_name}:{tag}"
        self.project_root = Path(__file__).parent.parent
        self.dockerfile_path = self.project_root / "Dockerfile"
        self.cache_file = self.project_root / ".docker_test_cache.json"

    def _get_build_context_hash(self) -> str:
        """Calculate hash of Dockerfile and key build context files."""
        hasher = hashlib.sha256()

        # Hash Dockerfile
        if self.dockerfile_path.exists():
            hasher.update(self.dockerfile_path.read_bytes())

        # Hash key context files that affect the build
        context_files = [
            "pyproject.toml",
            "requirements.txt",  # if exists
            "entrypoint.sh",  # if exists
        ]

        for file_name in context_files:
            file_path = self.project_root / file_name
            if file_path.exists():
                hasher.update(file_path.read_bytes())

        return hasher.hexdigest()

    def _load_cache_info(self) -> dict[str, Any]:
        """Load cached build information."""
        try:
            if self.cache_file.exists():
                return json.loads(self.cache_file.read_text())
        except (json.JSONDecodeError, OSError):
            pass
        return {}

    def _save_cache_info(self, build_hash: str, image_id: str) -> None:
        """Save build cache information."""
        cache_data = {"build_hash": build_hash, "image_id": image_id, "build_time": time.time(), "image_name": self.full_image_name}
        try:
            self.cache_file.write_text(json.dumps(cache_data, indent=2))
        except OSError as e:
            print(f"Warning: Could not save cache info: {e}")

    def _image_exists(self) -> bool:
        """Check if the Docker image exists locally."""
        try:
            result = subprocess.run(["docker", "images", "-q", self.full_image_name], capture_output=True, text=True, check=True)
            return bool(result.stdout.strip())
        except subprocess.CalledProcessError:
            return False

    def _get_image_id(self) -> str | None:
        """Get the current image ID if it exists."""
        try:
            result = subprocess.run(["docker", "images", "-q", self.full_image_name], capture_output=True, text=True, check=True)
            return result.stdout.strip() or None
        except subprocess.CalledProcessError:
            return None

    def _needs_rebuild(self) -> bool:
        """Check if the image needs to be rebuilt."""
        # Check if image exists
        if not self._image_exists():
            print(f"Image {self.full_image_name} does not exist, rebuild needed")
            return True

        # Calculate current build context hash
        current_hash = self._get_build_context_hash()

        # Load cached info
        cache_info = self._load_cache_info()
        cached_hash = cache_info.get("build_hash")
        cached_image_id = cache_info.get("image_id")

        # Check if build context changed
        if current_hash != cached_hash:
            print("Build context changed, rebuild needed")
            return True

        # Check if cached image still exists
        current_image_id = self._get_image_id()
        if current_image_id != cached_image_id:
            print("Cached image ID mismatch, rebuild needed")
            return True

        print(f"Image {self.full_image_name} is up to date")
        return False

    def _build_image(self) -> str:
        """Build the Docker image and return the image ID."""
        print(f"Building Docker image: {self.full_image_name}")
        print("=" * 60)

        # Change to project root for build context
        original_cwd = Path.cwd()
        try:
            # Build command
            cmd = ["docker", "build", "-t", self.full_image_name, "--progress=plain", "."]

            print(f"Running: {' '.join(cmd)}")

            # Run build with timeout
            subprocess.run(
                cmd,
                cwd=self.project_root,
                check=True,
                text=True,
                capture_output=False,  # Show build output in real-time
                timeout=1200,  # 20 minute timeout
            )

            # Get the built image ID
            image_id = self._get_image_id()
            if not image_id:
                raise RuntimeError("Failed to get image ID after build")

            print("=" * 60)
            print(f"[SUCCESS] Docker image built successfully: {image_id[:12]}")

            return image_id

        except subprocess.CalledProcessError as e:
            print("=" * 60)
            print(f"[ERROR] Docker build failed with exit code: {e.returncode}")
            raise RuntimeError(f"Docker build failed with exit code {e.returncode}") from e

        except subprocess.TimeoutExpired as e:
            print("=" * 60)
            print("[ERROR] Docker build timed out after 20 minutes")
            raise RuntimeError("Docker build timed out") from e

        finally:
            # Restore original working directory
            original_cwd and subprocess.run(["cd", str(original_cwd)], shell=True)

    def ensure_image_ready(self) -> str:
        """Ensure the test image is built and ready. Returns the full image name."""
        print(f"Ensuring Docker test image is ready: {self.full_image_name}")

        if self._needs_rebuild():
            # Build the image
            image_id = self._build_image()

            # Update cache
            build_hash = self._get_build_context_hash()
            self._save_cache_info(build_hash, image_id)

            print(f"Image ready: {self.full_image_name} ({image_id[:12]})")
        else:
            print(f"Using cached image: {self.full_image_name}")

        return self.full_image_name


# Global instance for shared use
_default_manager = DockerTestImageManager()


def ensure_test_image() -> str:
    """Ensure the test Docker image is built and ready.

    This is the main entry point for tests to use.
    Returns the full image name that tests can use.
    """
    return _default_manager.ensure_image_ready()


def cleanup_test_containers(container_prefix: str = "clud-test") -> None:
    """Clean up test containers with the given prefix."""
    try:
        # Get list of containers with the prefix
        result = subprocess.run(["docker", "ps", "-a", "--filter", f"name={container_prefix}", "--format", "{{.Names}}"], capture_output=True, text=True, check=True)

        container_names = [name.strip() for name in result.stdout.splitlines() if name.strip()]

        if container_names:
            print(f"Cleaning up {len(container_names)} test containers...")
            for container_name in container_names:
                subprocess.run(
                    ["docker", "rm", "-f", container_name],
                    capture_output=True,
                    check=False,  # Don't fail if container doesn't exist
                )
            print("Test container cleanup completed")

    except subprocess.CalledProcessError:
        # Ignore cleanup failures
        pass


if __name__ == "__main__":
    # Allow running this module directly to build the image
    ensure_test_image()

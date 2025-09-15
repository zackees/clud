#!/usr/bin/env python3
"""
Docker image builder for CLUD development environment.

This script builds the niteris/clud Docker image manually with proper error handling,
progress reporting, and build optimization features.
"""

import argparse
import os
import sys
import threading
import time
import traceback
from pathlib import Path
from typing import Optional

from src.clud.docker.docker_manager import DockerManager, DockerException, ImageNotFound


class DockerBuildError(Exception):
    """Exception raised when Docker build fails."""
    pass


def dump_thread_stacks():
    """Dump stack traces of all threads."""
    print("\n" + "=" * 60)
    print("TIMEOUT: Dumping all thread stack traces:")
    print("=" * 60)

    for thread_id, frame in sys._current_frames().items():
        thread_name = None
        for thread in threading.enumerate():
            if thread.ident == thread_id:
                thread_name = thread.name
                break

        print(f"\nThread {thread_id} ({thread_name or 'Unknown'}):")
        print("-" * 40)
        traceback.print_stack(frame)

    print("=" * 60)


def timeout_monitor(start_time, timeout, stop_event):
    """Monitor for timeout and dump stacks if exceeded."""
    while not stop_event.is_set():
        elapsed = time.time() - start_time
        if elapsed > timeout:
            dump_thread_stacks()
            print(f"\nBuild timed out after {timeout} seconds")
            stop_event.set()

            # Give the main process 5 seconds to exit gracefully
            time.sleep(5)
            print("Process did not exit gracefully, forcing exit")
            os._exit(1)
        time.sleep(1)


def check_docker_available(docker_manager: DockerManager) -> bool:
    """Check if Docker is available and running."""
    is_running, _ = docker_manager.is_running()
    return is_running


def image_exists(docker_manager: DockerManager, image_name: str) -> bool:
    """Check if Docker image already exists."""
    try:
        docker_manager.client.images.get(image_name)
        return True
    except ImageNotFound:
        return False
    except DockerException:
        return False


def get_image_id(docker_manager: DockerManager, image_name: str) -> Optional[str]:
    """Get the image ID of an existing image."""
    try:
        image = docker_manager.client.images.get(image_name)
        return image.id
    except ImageNotFound:
        return None
    except DockerException:
        return None


def build_docker_image(
    docker_manager: DockerManager,
    image_name: str = "niteris/clud:latest",
    dockerfile_path: Optional[Path] = None,
    build_context: Optional[Path] = None,
    no_cache: bool = False,
    quiet: bool = False,
    verbose: bool = False,
    timeout: Optional[int] = 600
) -> str:
    """
    Build the Docker image.

    Args:
        image_name: Name and tag for the Docker image
        dockerfile_path: Path to Dockerfile (defaults to ./Dockerfile)
        build_context: Build context directory (defaults to current directory)
        no_cache: Whether to build without using cache
        quiet: Suppress build output
        verbose: Show verbose build output
        timeout: Build timeout in seconds (None for no timeout)

    Returns:
        Image ID of the built image

    Raises:
        DockerBuildError: If build fails
    """
    # Set defaults
    if dockerfile_path is None:
        dockerfile_path = Path.cwd() / "Dockerfile"
    if build_context is None:
        build_context = Path.cwd()

    # Verify files exist
    if not dockerfile_path.exists():
        raise DockerBuildError(f"Dockerfile not found at {dockerfile_path}")
    if not build_context.exists():
        raise DockerBuildError(f"Build context directory not found at {build_context}")

    # Split image name and tag
    if ":" in image_name:
        name, tag = image_name.split(":", 1)
    else:
        name = image_name
        tag = "latest"

    print(f"Building Docker image: {image_name}")
    print(f"Dockerfile: {dockerfile_path}")
    print(f"Build context: {build_context}")
    print("=" * 60)

    # Execute build
    start_time = time.time()

    # Start timeout monitor thread
    stop_event = threading.Event()
    monitor_thread = threading.Thread(
        target=timeout_monitor,
        args=(start_time, timeout, stop_event),
        daemon=True
    )
    monitor_thread.start()

    try:
        docker_manager.build_image(
            image_name=name,
            tag=tag,
            dockerfile_path=dockerfile_path,
            build_context=build_context,
            no_cache=no_cache,
            quiet=quiet,
            verbose=verbose,
        )

        # Extract image ID from output or use docker images
        image_id = get_image_id(docker_manager, image_name)
        if not image_id:
            raise DockerBuildError("Failed to determine image ID after build")

        build_time = time.time() - start_time

        # Stop the timeout monitor
        stop_event.set()

        print("=" * 60)
        print(f"OK Build completed successfully!")
        print(f"Image: {image_name}")
        print(f"Image ID: {image_id}")
        print(f"Build time: {build_time:.1f} seconds")

        return image_id

    except DockerBuildError as e:
        stop_event.set()
        raise DockerBuildError(f"Build failed: {e}")
    except Exception as e:
        stop_event.set()
        raise DockerBuildError(f"Build failed: {e}")


def remove_image(docker_manager: DockerManager, image_name: str, force: bool = False) -> bool:
    """Remove a Docker image."""
    try:
        docker_manager.client.images.remove(image_name, force=force)
        print(f"Removed image: {image_name}")
        return True
    except ImageNotFound:
        print(f"Image {image_name} not found, skipping removal.")
        return True  # Consider it removed if it doesn't exist
    except DockerException as e:
        print(f"Failed to remove image: {image_name} - {e}")
        return False


def main():
    """Main function."""
    parser = argparse.ArgumentParser(
        description="Build CLUD development Docker image",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python build.py                          # Build with defaults
  python build.py --no-cache               # Build without cache
  python build.py --verbose                # Build with verbose output
  python build.py --quiet                  # Build quietly
  python build.py --force-rebuild          # Remove existing image and rebuild
  python build.py --check                  # Only check if image exists
  python build.py --timeout 1200           # Build with 20-minute timeout
  python build.py --yes                    # Auto-accept all prompts
        """
    )

    parser.add_argument(
        "--image-name",
        default="niteris/clud:latest",
        help="Docker image name and tag (default: niteris/clud:latest)"
    )
    parser.add_argument(
        "--dockerfile",
        type=Path,
        help="Path to Dockerfile (default: ./Dockerfile)"
    )
    parser.add_argument(
        "--build-context",
        type=Path,
        help="Build context directory (default: current directory)"
    )
    parser.add_argument(
        "--no-cache",
        action="store_true",
        help="Build without using cache"
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Suppress build output"
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Show verbose build output"
    )
    parser.add_argument(
        "--force-rebuild",
        action="store_true",
        help="Remove existing image and rebuild"
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Only check if image exists, don't build"
    )
    parser.add_argument(
        "--timeout",
        type=int,
        help="Timeout in seconds. When triggered, dumps all threads and exits (no default - runs forever without this flag)"
    )
    parser.add_argument(
        "--yes", "-y",
        action="store_true",
        help="Auto-accept all prompts (non-interactive mode)"
    )

    args = parser.parse_args()

    docker_manager = DockerManager()

    # Note: timeout is handled within build_docker_image function

    # Check Docker availability
    if not check_docker_available(docker_manager):
        print("ERROR: Docker is not available or not running")
        return 1

    print("OK Docker is available")

    # Check if image exists
    exists = image_exists(docker_manager, args.image_name)
    if exists:
        image_id = get_image_id(docker_manager, args.image_name)
        print(f"Image {args.image_name} already exists (ID: {image_id})")

        if args.check:
            return 0

        if not args.force_rebuild:
            if args.yes:
                print("Auto-accepting rebuild (--yes flag)")
            else:
                response = input("Do you want to rebuild it? [y/N]: ").strip().lower()
                if response not in ('y', 'yes'):
                    print("Skipping build")
                    return 0

        if args.force_rebuild:
            print("Force rebuild requested, removing existing image...")
            if not remove_image(docker_manager, args.image_name, force=True):
                print("Warning: Failed to remove existing image, continuing with build...")
    else:
        print(f"Image {args.image_name} does not exist")

        if args.check:
            return 1

    # Build the image
    try:
        image_id = build_docker_image(
            docker_manager,
            image_name=args.image_name,
            dockerfile_path=args.dockerfile,
            build_context=args.build_context,
            no_cache=args.no_cache,
            quiet=args.quiet,
            verbose=args.verbose,
            timeout=600 if args.timeout is None else args.timeout
        )

        print("\nBuild process completed successfully!")
        print(f"You can now run: docker run --rm -it {args.image_name}")

        return 0

    except DockerBuildError as e:
        print(f"\nBuild failed: {e}")
        return 1
    except KeyboardInterrupt:
        print("\nBuild interrupted by user")
        return 1
    except Exception as e:
        print(f"\nUnexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
#!/usr/bin/env python3
"""
Docker image builder for CLUD development environment.

This script builds the clud-dev Docker image manually with proper error handling,
progress reporting, and build optimization features.
"""

import argparse
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional


class DockerBuildError(Exception):
    """Exception raised when Docker build fails."""
    pass


def check_docker_available() -> bool:
    """Check if Docker is available and running."""
    try:
        result = subprocess.run(
            ["docker", "version"],
            capture_output=True,
            text=True,
            check=True,
            timeout=10
        )
        return True
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return False


def image_exists(image_name: str) -> bool:
    """Check if Docker image already exists."""
    try:
        result = subprocess.run(
            ["docker", "images", "-q", image_name],
            capture_output=True,
            text=True,
            check=True
        )
        return bool(result.stdout.strip())
    except subprocess.CalledProcessError:
        return False


def get_image_id(image_name: str) -> Optional[str]:
    """Get the image ID of an existing image."""
    try:
        result = subprocess.run(
            ["docker", "images", "-q", image_name],
            capture_output=True,
            text=True,
            check=True
        )
        image_id = result.stdout.strip()
        return image_id if image_id else None
    except subprocess.CalledProcessError:
        return None


def build_docker_image(
    image_name: str = "clud-dev:latest",
    dockerfile_path: Optional[Path] = None,
    build_context: Optional[Path] = None,
    no_cache: bool = False,
    quiet: bool = False,
    verbose: bool = False,
    timeout: int = 600
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
        timeout: Build timeout in seconds

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

    # Construct build command
    cmd = ["docker", "build", "-t", image_name]

    if dockerfile_path != build_context / "Dockerfile":
        cmd.extend(["-f", str(dockerfile_path)])

    if no_cache:
        cmd.append("--no-cache")

    if quiet:
        cmd.append("--quiet")
    elif verbose:
        cmd.append("--progress=plain")

    cmd.append(str(build_context))

    print(f"Building Docker image: {image_name}")
    print(f"Dockerfile: {dockerfile_path}")
    print(f"Build context: {build_context}")
    print(f"Command: {' '.join(cmd)}")
    print("=" * 60)

    # Execute build
    start_time = time.time()

    try:
        if quiet:
            # Capture output for quiet mode
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=True,
                timeout=timeout
            )
            image_id = result.stdout.strip()
        else:
            # Show real-time output for normal/verbose mode
            process = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                bufsize=1,
                universal_newlines=True
            )

            output_lines = []
            while True:
                output = process.stdout.readline()
                if output == '' and process.poll() is not None:
                    break
                if output:
                    print(output.strip())
                    output_lines.append(output)

            process.wait()
            if process.returncode != 0:
                raise subprocess.CalledProcessError(process.returncode, cmd)

            # Extract image ID from output
            image_id = get_image_id(image_name)
            if not image_id:
                raise DockerBuildError("Failed to determine image ID after build")

        build_time = time.time() - start_time

        print("=" * 60)
        print(f"OK Build completed successfully!")
        print(f"Image: {image_name}")
        print(f"Image ID: {image_id}")
        print(f"Build time: {build_time:.1f} seconds")

        return image_id

    except subprocess.TimeoutExpired:
        raise DockerBuildError(f"Build timed out after {timeout} seconds")
    except subprocess.CalledProcessError as e:
        raise DockerBuildError(f"Build failed with exit code {e.returncode}")


def remove_image(image_name: str, force: bool = False) -> bool:
    """Remove a Docker image."""
    try:
        cmd = ["docker", "rmi"]
        if force:
            cmd.append("-f")
        cmd.append(image_name)

        subprocess.run(cmd, check=True, capture_output=True)
        print(f"Removed image: {image_name}")
        return True
    except subprocess.CalledProcessError:
        print(f"Failed to remove image: {image_name}")
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
        """
    )

    parser.add_argument(
        "--image-name",
        default="clud-dev:latest",
        help="Docker image name and tag (default: clud-dev:latest)"
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
        default=600,
        help="Build timeout in seconds (default: 600)"
    )

    args = parser.parse_args()

    # Check Docker availability
    if not check_docker_available():
        print("ERROR: Docker is not available or not running")
        return 1

    print("OK Docker is available")

    # Check if image exists
    exists = image_exists(args.image_name)
    if exists:
        image_id = get_image_id(args.image_name)
        print(f"Image {args.image_name} already exists (ID: {image_id})")

        if args.check:
            return 0

        if not args.force_rebuild:
            response = input("Do you want to rebuild it? [y/N]: ").strip().lower()
            if response not in ('y', 'yes'):
                print("Skipping build")
                return 0

        if args.force_rebuild:
            print("Force rebuild requested, removing existing image...")
            if not remove_image(args.image_name, force=True):
                print("Warning: Failed to remove existing image, continuing with build...")
    else:
        print(f"Image {args.image_name} does not exist")

        if args.check:
            return 1

    # Build the image
    try:
        image_id = build_docker_image(
            image_name=args.image_name,
            dockerfile_path=args.dockerfile,
            build_context=args.build_context,
            no_cache=args.no_cache,
            quiet=args.quiet,
            verbose=args.verbose,
            timeout=args.timeout
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
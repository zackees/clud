"""Git worktree management for Docker containers."""

import subprocess
from pathlib import Path

from .agent_background import DockerError, ValidationError, normalize_path_for_docker


def verify_container_directories() -> None:
    """Verify that required directories exist in the container.

    This function is used when running Git worktree commands to ensure
    the container has the expected directory structure.

    Raises:
        DockerError: If required directories are missing
    """
    required_dirs = ["/workspace", "/host"]

    for dir_path in required_dirs:
        if not Path(dir_path).exists():
            raise DockerError(f"Required container directory {dir_path} does not exist. This indicates a problem with the Docker image.")

    print("✓ Container directory structure verified")


def build_verified_command(git_cmd: list[str]) -> list[str]:
    """Build a command that verifies container directories before running git.

    Args:
        git_cmd: The git command to run after verification

    Returns:
        Command list that includes directory verification
    """
    return [
        "bash",
        "-c",
        f"set -e && "
        f"echo 'Verifying container directories...' && "
        f"for dir in /workspace /host; do "
        f'  if [ ! -d "$dir" ]; then '
        f'    echo "ERROR: Required directory $dir does not exist!" >&2; '
        f"    exit 1; "
        f"  fi; "
        f"done && "
        f"echo '✓ Container directories verified' && "
        f"{' '.join(git_cmd)}",
    ]


def ensure_worktree_directory(project_path: Path, worktree_name: str = "worktree") -> Path:
    """DEPRECATED: This function is no longer needed since worktrees are container-only.

    Kept for backward compatibility but no longer creates directories.

    Args:
        project_path: The project root directory containing .git
        worktree_name: Name of the worktree subdirectory (default: "worktree") - DEPRECATED

    Returns:
        Path to the project directory (for compatibility)

    Raises:
        ValidationError: If project_path is not a valid Git repository
    """
    # Validate that project_path contains a .git directory
    git_dir = project_path / ".git"
    if not git_dir.exists():
        raise ValidationError(f"Project directory is not a Git repository: {project_path}")

    print("Note: Worktree directories are now container-only and not created on host")
    return project_path  # Return project path for compatibility


def create_git_worktree_in_container(project_path: Path, branch_name: str, worktree_name: str = "worktree", create_new_branch: bool = False, image_name: str = "niteris/clud:latest") -> bool:
    """Create a Git worktree inside a Docker container.

    Args:
        project_path: The project root directory containing .git
        branch_name: Name of the branch to check out in the worktree
        worktree_name: Name of the worktree subdirectory (default: "worktree") - DEPRECATED, not used
        create_new_branch: If True, create a new branch with the given name
        image_name: Docker image to use for the operation

    Returns:
        True if worktree was created successfully, False otherwise

    Raises:
        ValidationError: If inputs are invalid
        DockerError: If Docker operations fail
    """
    # Validate that project_path contains a .git directory
    git_dir = project_path / ".git"
    if not git_dir.exists():
        raise ValidationError(f"Project directory is not a Git repository: {project_path}")

    # Normalize paths for Docker
    project_docker_path = normalize_path_for_docker(project_path)

    # Build command that verifies directories and then runs git worktree
    git_cmd = ["git", "worktree", "add", "/workspace", branch_name]
    if create_new_branch:
        git_cmd.insert(-1, "-b")  # Insert -b before branch_name

    # Create a compound command that verifies directories first
    verify_and_git_cmd = build_verified_command(git_cmd)

    # Build Docker command - only mount /host, /workspace is container-only
    docker_cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{project_docker_path}:/host:rw",  # Repository with read-write access for Git metadata
        "-w",
        "/host",  # Set working directory to the repository root
        image_name,
        *verify_and_git_cmd,
    ]

    print(f"Creating Git worktree for branch '{branch_name}' in container...")
    print(f"Command: {' '.join(git_cmd)}")

    try:
        # Execute the Docker command
        result = subprocess.run(docker_cmd, capture_output=True, text=True, check=False, timeout=300, encoding="utf-8", errors="replace")

        if result.returncode == 0:
            print(f"✓ Git worktree created successfully for branch '{branch_name}'")
            print(f"  Repository: {project_path}")
            print("  Worktree: /workspace (container-only)")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return True
        else:
            print(f"✗ Failed to create Git worktree (exit code: {result.returncode})")
            if result.stderr.strip():
                print(f"  Error: {result.stderr.strip()}")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return False

    except FileNotFoundError as e:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from e
    except Exception as e:
        raise DockerError(f"Failed to execute Git worktree command: {e}") from e


def remove_git_worktree_in_container(project_path: Path, worktree_name: str = "worktree", image_name: str = "niteris/clud:latest") -> bool:
    """Remove a Git worktree inside a Docker container.

    Args:
        project_path: The project root directory containing .git
        worktree_name: Name of the worktree subdirectory to remove - DEPRECATED, not used
        image_name: Docker image to use for the operation

    Returns:
        True if worktree was removed successfully, False otherwise

    Raises:
        ValidationError: If inputs are invalid
        DockerError: If Docker operations fail
    """
    # Validate that project_path contains a .git directory
    git_dir = project_path / ".git"
    if not git_dir.exists():
        raise ValidationError(f"Project directory is not a Git repository: {project_path}")

    # Normalize paths for Docker
    project_docker_path = normalize_path_for_docker(project_path)

    # Build git worktree remove command
    git_cmd = ["git", "worktree", "remove", "/workspace"]

    # Create a compound command that verifies directories first
    verify_and_git_cmd = build_verified_command(git_cmd)

    # Build Docker command - only mount /host, /workspace is container-only
    docker_cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{project_docker_path}:/host:rw",  # Repository with read-write access for Git metadata
        "-w",
        "/host",  # Set working directory to the repository root
        image_name,
        *verify_and_git_cmd,
    ]

    print(f"Removing Git worktree '{worktree_name}' in container...")

    try:
        # Execute the Docker command
        result = subprocess.run(docker_cmd, capture_output=True, text=True, check=False, timeout=300, encoding="utf-8", errors="replace")

        if result.returncode == 0:
            print("✓ Git worktree removed successfully")
            print("  Worktree: /workspace (container-only)")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return True
        else:
            print(f"✗ Failed to remove Git worktree (exit code: {result.returncode})")
            if result.stderr.strip():
                print(f"  Error: {result.stderr.strip()}")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return False

    except FileNotFoundError as e:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from e
    except Exception as e:
        raise DockerError(f"Failed to execute Git worktree remove command: {e}") from e


def prune_git_worktrees_in_container(project_path: Path, image_name: str = "niteris/clud:latest") -> bool:
    """Prune stale Git worktree entries inside a Docker container.

    Args:
        project_path: The project root directory containing .git
        image_name: Docker image to use for the operation

    Returns:
        True if worktrees were pruned successfully, False otherwise

    Raises:
        ValidationError: If inputs are invalid
        DockerError: If Docker operations fail
    """
    # Validate that project_path contains a .git directory
    git_dir = project_path / ".git"
    if not git_dir.exists():
        raise ValidationError(f"Project directory is not a Git repository: {project_path}")

    # Normalize paths for Docker
    project_docker_path = normalize_path_for_docker(project_path)

    # Build git worktree prune command
    git_cmd = ["git", "worktree", "prune"]

    # Build Docker command
    docker_cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{project_docker_path}:/host:rw",  # Repository with read-write access for Git metadata
        "-w",
        "/host",  # Set working directory to the repository root
        image_name,
        *git_cmd,
    ]

    print("Pruning stale Git worktree entries in container...")

    try:
        # Execute the Docker command
        result = subprocess.run(docker_cmd, capture_output=True, text=True, check=False, timeout=300, encoding="utf-8", errors="replace")

        if result.returncode == 0:
            print("✓ Git worktrees pruned successfully")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return True
        else:
            print(f"✗ Failed to prune Git worktrees (exit code: {result.returncode})")
            if result.stderr.strip():
                print(f"  Error: {result.stderr.strip()}")
            if result.stdout.strip():
                print(f"  Output: {result.stdout.strip()}")
            return False

    except FileNotFoundError as e:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from e
    except Exception as e:
        raise DockerError(f"Failed to execute Git worktree prune command: {e}") from e


def list_git_worktrees_in_container(project_path: Path, image_name: str = "niteris/clud:latest") -> str | None:
    """List Git worktrees inside a Docker container.

    Args:
        project_path: The project root directory containing .git
        image_name: Docker image to use for the operation

    Returns:
        String containing worktree list output, or None if command failed

    Raises:
        ValidationError: If inputs are invalid
        DockerError: If Docker operations fail
    """
    # Validate that project_path contains a .git directory
    git_dir = project_path / ".git"
    if not git_dir.exists():
        raise ValidationError(f"Project directory is not a Git repository: {project_path}")

    # Normalize paths for Docker
    project_docker_path = normalize_path_for_docker(project_path)

    # Build git worktree list command
    git_cmd = ["git", "worktree", "list"]

    # Build Docker command
    docker_cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{project_docker_path}:/host:rw",  # Repository with read-write access for Git metadata
        "-w",
        "/host",  # Set working directory to the repository root
        image_name,
        *git_cmd,
    ]

    try:
        # Execute the Docker command
        result = subprocess.run(docker_cmd, capture_output=True, text=True, check=False, timeout=300, encoding="utf-8", errors="replace")

        if result.returncode == 0:
            return result.stdout
        else:
            print(f"✗ Failed to list Git worktrees (exit code: {result.returncode})")
            if result.stderr.strip():
                print(f"  Error: {result.stderr.strip()}")
            return None

    except FileNotFoundError as e:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from e
    except Exception as e:
        raise DockerError(f"Failed to execute Git worktree list command: {e}") from e


def cleanup_git_worktree(project_path: Path, worktree_name: str = "worktree", image_name: str = "niteris/clud:latest") -> bool:
    """Comprehensive cleanup of Git worktree: remove worktree and prune entries.

    Args:
        project_path: The project root directory containing .git
        worktree_name: Name of the worktree subdirectory to clean up - DEPRECATED, not used
        image_name: Docker image to use for the operations

    Returns:
        True if cleanup was successful, False otherwise

    Raises:
        ValidationError: If inputs are invalid
        DockerError: If Docker operations fail
    """
    print("Starting comprehensive worktree cleanup...")

    # Step 1: Remove the worktree using Git
    print("Step 1: Removing Git worktree...")
    worktree_removed = remove_git_worktree_in_container(project_path, worktree_name, image_name)

    if not worktree_removed:
        print("Warning: Git worktree removal failed, but continuing with cleanup...")

    # Step 2: Prune stale worktree entries
    print("Step 2: Pruning stale Git worktree entries...")
    pruned = prune_git_worktrees_in_container(project_path, image_name)

    if not pruned:
        print("Warning: Git worktree pruning failed, but continuing with cleanup...")

    # Step 3: No host directory cleanup needed (worktrees are container-only)
    print("Step 3: No host directory cleanup needed (worktrees are container-only)")
    directory_removed = True

    # Summary
    if worktree_removed and pruned and directory_removed:
        print("✓ Comprehensive cleanup completed successfully")
        return True
    else:
        print("⚠ Cleanup completed with some warnings")
        print("Please check the messages above for any manual cleanup needed")
        return False

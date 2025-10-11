"""
Test that npx is available through nodejs-wheel package.

This ensures the portable Node.js installation works correctly.
"""

import pytest


def test_npx_command_available() -> None:
    """Test that npx command is available via nodejs-wheel."""
    # Import npx from nodejs_wheel
    from nodejs_wheel import npx

    # Run npx --version to verify it works
    result = npx(["--version"], return_completed_process=True)

    # Verify the command succeeded
    assert result.returncode == 0, f"npx --version failed with return code {result.returncode}"


def test_npm_command_available() -> None:
    """Test that npm command is available via nodejs-wheel."""
    from nodejs_wheel import npm

    # Run npm --version to verify it works
    result = npm(["--version"], return_completed_process=True)

    # Verify the command succeeded
    assert result.returncode == 0, f"npm --version failed with return code {result.returncode}"


def test_node_command_available() -> None:
    """Test that node command is available via nodejs-wheel."""
    from nodejs_wheel import node

    # Run node --version to verify it works
    result = node(["--version"], return_completed_process=True)

    # Verify the command succeeded
    assert result.returncode == 0, f"node --version failed with return code {result.returncode}"


def test_npx_can_run_packages() -> None:
    """Test that npx can run npm packages (using a simple built-in command)."""
    from nodejs_wheel import npx

    # Use npx to run a simple command that's always available
    # 'npm' is available through node, so we can test npx's execution capability
    result = npx(["npm", "--version"], return_completed_process=True)

    # Verify the command succeeded
    assert result.returncode == 0, f"npx npm --version failed with return code {result.returncode}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

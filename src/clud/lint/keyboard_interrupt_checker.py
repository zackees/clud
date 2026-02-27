"""Custom lint checker for KeyboardInterrupt handling in daemon code.

This module provides an AST-based checker that ensures daemon process files
use signal handlers instead of try/except for KeyboardInterrupt.

Rules:
    KI002: KeyboardInterrupt caught in daemon code (should use signal handlers)
    KI003: KeyboardInterrupt caught without using handle_keyboard_interrupt()
           (should use the utility for thread-safe interrupt handling)

Daemon process files are identified by patterns like:
    - daemon/__main__.py
    - cron/__main__.py
    - cron/daemon.py

CLI handlers and entry points are excluded from this rule since they
legitimately need to catch KeyboardInterrupt to perform cleanup.

Usage:
    python -m clud.lint.keyboard_interrupt_checker src/
"""

from __future__ import annotations

import ast
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass
class LintError:
    """A lint error found in the source code."""

    file_path: str
    line: int
    col: int
    code: str
    message: str
    level: str = "error"  # "error" fails the build, "warning" is informational

    def __str__(self) -> str:
        prefix = "warning" if self.level == "warning" else "error"
        return f"{self.file_path}:{self.line}:{self.col}: {self.code} [{prefix}] {self.message}"


class KeyboardInterruptChecker(ast.NodeVisitor):
    """AST visitor that checks for improper KeyboardInterrupt handling."""

    # Files that are actual daemon processes and should use signal handlers.
    # These are __main__.py files within daemon packages, not CLI handlers.
    # Note: cron/daemon.py is excluded because it already has signal handlers.
    DAEMON_PROCESS_FILES = [
        # Pattern: package/daemon/__main__.py or package/cron/__main__.py
        "daemon/__main__.py",
        "cron/__main__.py",
    ]

    # Files that are CLI handlers (client side) - should be allowed to catch KeyboardInterrupt
    CLIENT_PATTERNS = [
        "cli_handler",
        "cli.py",
        "agent_cli",
    ]

    # Files that already have signal handlers set up (backup catch is allowed)
    FILES_WITH_SIGNAL_HANDLERS = [
        "cron/daemon.py",
    ]

    # Files exempt from KI003 (handle_keyboard_interrupt recommendation).
    # These are top-level entry points or utility definitions themselves.
    KI003_EXEMPT_FILES = [
        "cli.py",  # Top-level entry point with excepthook
        "agent_cli.py",  # Top-level entry point
        "util/__init__.py",  # Defines handle_keyboard_interrupt itself
        "util.py",  # Alternative util location
    ]

    def __init__(self, file_path: str) -> None:
        """Initialize the checker.

        Args:
            file_path: Path to the file being checked
        """
        self.file_path = file_path
        self.errors: list[LintError] = []
        self._is_daemon_file = self._check_is_daemon_file()
        self._is_ki003_exempt = self._check_is_ki003_exempt()

    def _check_is_daemon_file(self) -> bool:
        """Check if this file is a daemon process file that should use signal handlers.

        Returns:
            True if file is a daemon process and should use signal handlers
        """
        # Normalize path separators
        path_normalized = self.file_path.replace("\\", "/").lower()

        # CLI handlers are client code, not daemon processes
        if any(pattern in path_normalized for pattern in self.CLIENT_PATTERNS):
            return False

        # Files with signal handlers already set up can have backup catches
        if any(pattern in path_normalized for pattern in self.FILES_WITH_SIGNAL_HANDLERS):
            return False

        # Check if this is an actual daemon process file
        return any(pattern in path_normalized for pattern in self.DAEMON_PROCESS_FILES)

    def _check_is_ki003_exempt(self) -> bool:
        """Check if this file is exempt from KI003 (handle_keyboard_interrupt recommendation).

        Returns:
            True if file should not be checked for KI003
        """
        path_normalized = self.file_path.replace("\\", "/").lower()

        # Exempt top-level entry points and the utility definition itself
        if any(pattern in path_normalized for pattern in self.KI003_EXEMPT_FILES):
            return True

        # Test files are exempt
        return "tests/" in path_normalized or path_normalized.startswith("tests/")

    def visit_ExceptHandler(self, node: ast.ExceptHandler) -> None:
        """Visit an except handler node.

        Checks if KeyboardInterrupt is being caught in daemon process files.
        Daemon processes should use signal handlers instead of try/except.

        Also checks KI003: KeyboardInterrupt handlers should use
        handle_keyboard_interrupt() for thread-safe interrupt handling.

        Note: KI001 (re-raise requirement) is disabled by default as it produces
        too many false positives for CLI entry points and user-facing code where
        catching KeyboardInterrupt without re-raising is legitimate.

        Args:
            node: The except handler AST node
        """
        if not self._catches_keyboard_interrupt(node):
            self.generic_visit(node)
            return

        # Only flag daemon process files (KI002)
        # These should use signal handlers instead of try/except
        if self._is_daemon_file and not self._handler_reraises(node):
            self.errors.append(
                LintError(
                    file_path=self.file_path,
                    line=node.lineno,
                    col=node.col_offset,
                    code="KI002",
                    message=("KeyboardInterrupt caught in daemon code. Use signal handlers instead (see clud.cron.daemon for example)."),
                )
            )

        # KI003: Recommend handle_keyboard_interrupt() for thread-safe handling (warning only)
        if not self._is_ki003_exempt and not self._uses_handle_keyboard_interrupt(node):
            self.errors.append(
                LintError(
                    file_path=self.file_path,
                    line=node.lineno,
                    col=node.col_offset,
                    code="KI003",
                    message=(
                        "KeyboardInterrupt caught without handle_keyboard_interrupt(). "
                        "Use handle_keyboard_interrupt(func, exc=e) from clud.util for "
                        "thread-safe interrupt handling (checks main thread before re-raising)."
                    ),
                    level="warning",
                )
            )

        self.generic_visit(node)

    def _catches_keyboard_interrupt(self, node: ast.ExceptHandler) -> bool:
        """Check if an except handler catches KeyboardInterrupt.

        Args:
            node: The except handler AST node

        Returns:
            True if handler catches KeyboardInterrupt
        """
        if node.type is None:
            # Bare except catches everything including KeyboardInterrupt
            return True

        # Check for single exception type
        if isinstance(node.type, ast.Name):
            return node.type.id == "KeyboardInterrupt"

        # Check for tuple of exception types
        if isinstance(node.type, ast.Tuple):
            for elt in node.type.elts:
                if isinstance(elt, ast.Name) and elt.id == "KeyboardInterrupt":
                    return True

        return False

    def _handler_reraises(self, node: ast.ExceptHandler) -> bool:
        """Check if an exception handler re-raises the exception.

        Args:
            node: The except handler AST node

        Returns:
            True if handler re-raises (has bare 'raise' or 'raise ... from ...')
        """
        for stmt in ast.walk(node):
            if isinstance(stmt, ast.Raise):
                # Bare raise (re-raises current exception)
                if stmt.exc is None:
                    return True
                # raise KeyboardInterrupt or raise ... from ...
                # Check if raising KeyboardInterrupt explicitly
                if isinstance(stmt.exc, ast.Name) and stmt.exc.id == "KeyboardInterrupt":
                    return True
                if isinstance(stmt.exc, ast.Call) and isinstance(stmt.exc.func, ast.Name) and stmt.exc.func.id == "KeyboardInterrupt":
                    return True

        return False

    def _uses_handle_keyboard_interrupt(self, node: ast.ExceptHandler) -> bool:
        """Check if an except handler calls handle_keyboard_interrupt().

        Args:
            node: The except handler AST node

        Returns:
            True if handler calls handle_keyboard_interrupt anywhere in its body
        """
        for stmt in ast.walk(node):
            if isinstance(stmt, ast.Call):
                # Check for direct call: handle_keyboard_interrupt(...)
                if isinstance(stmt.func, ast.Name) and stmt.func.id == "handle_keyboard_interrupt":
                    return True
                # Check for qualified call: util.handle_keyboard_interrupt(...)
                if isinstance(stmt.func, ast.Attribute) and stmt.func.attr == "handle_keyboard_interrupt":
                    return True

        return False


def check_file(file_path: Path) -> list[LintError]:
    """Check a Python file for KeyboardInterrupt handling issues.

    Args:
        file_path: Path to the Python file

    Returns:
        List of lint errors found
    """
    try:
        source = file_path.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(file_path))
    except (SyntaxError, UnicodeDecodeError) as e:
        return [
            LintError(
                file_path=str(file_path),
                line=1,
                col=0,
                code="KI000",
                message=f"Failed to parse file: {e}",
            )
        ]

    checker = KeyboardInterruptChecker(str(file_path))
    checker.visit(tree)
    return checker.errors


def check_directory(directory: Path, exclude_patterns: list[str] | None = None) -> list[LintError]:
    """Check all Python files in a directory for KeyboardInterrupt issues.

    Args:
        directory: Directory to check
        exclude_patterns: Patterns to exclude (e.g., ["__pycache__", ".venv"])

    Returns:
        List of all lint errors found
    """
    if exclude_patterns is None:
        exclude_patterns = ["__pycache__", ".venv", "build", "dist", ".git"]

    errors: list[LintError] = []

    for py_file in directory.rglob("*.py"):
        # Skip excluded patterns
        if any(pattern in str(py_file) for pattern in exclude_patterns):
            continue

        file_errors = check_file(py_file)
        errors.extend(file_errors)

    return errors


def main() -> int:
    """Main entry point for the lint checker.

    Returns:
        Exit code (0 if no errors, 1 if errors found)
    """
    if len(sys.argv) < 2:
        print("Usage: python -m clud.lint.keyboard_interrupt_checker <path> [path ...]")
        return 2

    all_issues: list[LintError] = []

    for path_arg in sys.argv[1:]:
        path = Path(path_arg)
        if path.is_file():
            all_issues.extend(check_file(path))
        elif path.is_dir():
            all_issues.extend(check_directory(path))
        else:
            print(f"Warning: {path} does not exist", file=sys.stderr)

    errors = [e for e in all_issues if e.level == "error"]
    warnings = [e for e in all_issues if e.level == "warning"]

    if warnings:
        print(f"Found {len(warnings)} KeyboardInterrupt recommendation(s):")
        print()
        for warning in sorted(warnings, key=lambda e: (e.file_path, e.line)):
            print(f"  {warning}")
        print()

    if errors:
        print(f"Found {len(errors)} KeyboardInterrupt error(s):")
        print()
        for error in sorted(errors, key=lambda e: (e.file_path, e.line)):
            print(f"  {error}")
        print()
        return 1

    if not warnings:
        print("No KeyboardInterrupt handling issues found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

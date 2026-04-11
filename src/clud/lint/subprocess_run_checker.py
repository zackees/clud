"""Custom lint checker for subprocess.run capture usage.

Rules:
    RP001: Avoid subprocess.run() when capturing output. Use
           running_process.RunningProcess.run() instead.

Allowed subprocess.run() forms:
    - capture_output omitted
    - capture_output=False
    - stdout/stderr redirected to non-PIPE values

Disallowed forms:
    - capture_output=True
    - stdout=subprocess.PIPE
    - stderr=subprocess.PIPE
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
    level: str = "error"

    def __str__(self) -> str:
        prefix = "warning" if self.level == "warning" else "error"
        return f"{self.file_path}:{self.line}:{self.col}: {self.code} [{prefix}] {self.message}"


class SubprocessRunChecker(ast.NodeVisitor):
    """AST visitor that checks for subprocess.run() output capture."""

    def __init__(self, file_path: str) -> None:
        self.file_path = file_path
        self.errors: list[LintError] = []
        self._subprocess_module_aliases = {"subprocess"}
        self._subprocess_run_aliases: set[str] = set()

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            if alias.name == "subprocess":
                self._subprocess_module_aliases.add(alias.asname or alias.name)
        self.generic_visit(node)

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        if node.module == "subprocess":
            for alias in node.names:
                if alias.name == "run":
                    self._subprocess_run_aliases.add(alias.asname or alias.name)
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        if self._is_subprocess_run_call(node) and self._captures_output(node):
            self.errors.append(
                LintError(
                    file_path=self.file_path,
                    line=node.lineno,
                    col=node.col_offset,
                    code="RP001",
                    message=("Avoid subprocess.run() when capturing output. Use running_process.RunningProcess.run() instead."),
                )
            )
        self.generic_visit(node)

    def _is_subprocess_run_call(self, node: ast.Call) -> bool:
        func = node.func
        if isinstance(func, ast.Name):
            return func.id in self._subprocess_run_aliases

        if isinstance(func, ast.Attribute) and func.attr == "run":
            value = func.value
            return isinstance(value, ast.Name) and value.id in self._subprocess_module_aliases

        return False

    def _captures_output(self, node: ast.Call) -> bool:
        capture_output_value = self._keyword_value(node, "capture_output")
        if self._is_true_literal(capture_output_value):
            return True

        stdout_value = self._keyword_value(node, "stdout")
        stderr_value = self._keyword_value(node, "stderr")
        return self._is_subprocess_pipe(stdout_value) or self._is_subprocess_pipe(stderr_value)

    def _keyword_value(self, node: ast.Call, name: str) -> ast.expr | None:
        for keyword in node.keywords:
            if keyword.arg == name:
                return keyword.value
        return None

    def _is_true_literal(self, node: ast.expr | None) -> bool:
        return isinstance(node, ast.Constant) and node.value is True

    def _is_subprocess_pipe(self, node: ast.expr | None) -> bool:
        if not isinstance(node, ast.Attribute):
            return False
        if node.attr != "PIPE":
            return False
        return isinstance(node.value, ast.Name) and node.value.id in self._subprocess_module_aliases


def check_file(file_path: Path) -> list[LintError]:
    """Check a Python file for subprocess.run capture issues."""
    try:
        source = file_path.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(file_path))
    except (SyntaxError, UnicodeDecodeError) as exc:
        return [
            LintError(
                file_path=str(file_path),
                line=1,
                col=0,
                code="RP000",
                message=f"Failed to parse file: {exc}",
            )
        ]

    checker = SubprocessRunChecker(str(file_path))
    checker.visit(tree)
    return checker.errors


def check_directory(directory: Path, exclude_patterns: list[str] | None = None) -> list[LintError]:
    """Check all Python files in a directory for subprocess.run capture issues."""
    if exclude_patterns is None:
        exclude_patterns = ["__pycache__", ".venv", "build", "dist", ".git"]

    errors: list[LintError] = []
    for py_file in directory.rglob("*.py"):
        if any(pattern in str(py_file) for pattern in exclude_patterns):
            continue
        errors.extend(check_file(py_file))
    return errors


def main() -> int:
    """Main entry point for the lint checker."""
    if len(sys.argv) < 2:
        print("Usage: python -m clud.lint.subprocess_run_checker <path> [path ...]")
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

    errors = [issue for issue in all_issues if issue.level == "error"]
    if errors:
        print(f"Found {len(errors)} subprocess.run capture error(s):")
        print()
        for error in sorted(errors, key=lambda item: (item.file_path, item.line)):
            print(f"  {error}")
        print()
        return 1

    print("No subprocess.run capture issues found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

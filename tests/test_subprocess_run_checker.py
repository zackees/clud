"""Unit tests for subprocess.run capture lint checker."""

import ast
import unittest

from clud.lint.subprocess_run_checker import SubprocessRunChecker


class TestSubprocessRunChecker(unittest.TestCase):
    """Test RP001: subprocess.run capture should use RunningProcess.run."""

    def _check_code(self, code: str, file_path: str = "src/clud/example.py") -> list[str]:
        tree = ast.parse(code)
        checker = SubprocessRunChecker(file_path)
        checker.visit(tree)
        return [error.code for error in checker.errors]

    def test_capture_output_true_flagged(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"], capture_output=True)
"""
        errors = self._check_code(code)
        self.assertIn("RP001", errors)

    def test_capture_output_false_allowed(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"], capture_output=False)
"""
        errors = self._check_code(code)
        self.assertNotIn("RP001", errors)

    def test_capture_output_omitted_allowed(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"])
"""
        errors = self._check_code(code)
        self.assertNotIn("RP001", errors)

    def test_stdout_pipe_flagged(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"], stdout=subprocess.PIPE)
"""
        errors = self._check_code(code)
        self.assertIn("RP001", errors)

    def test_stderr_pipe_flagged(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"], stderr=subprocess.PIPE)
"""
        errors = self._check_code(code)
        self.assertIn("RP001", errors)

    def test_non_pipe_redirection_allowed(self) -> None:
        code = """
import subprocess

subprocess.run(["git", "status"], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
"""
        errors = self._check_code(code)
        self.assertNotIn("RP001", errors)

    def test_import_alias_flagged(self) -> None:
        code = """
import subprocess as sp

sp.run(["git", "status"], capture_output=True)
"""
        errors = self._check_code(code)
        self.assertIn("RP001", errors)

    def test_from_import_run_flagged(self) -> None:
        code = """
from subprocess import run

run(["git", "status"], capture_output=True)
"""
        errors = self._check_code(code)
        self.assertIn("RP001", errors)

    def test_other_run_function_not_flagged(self) -> None:
        code = """
def run(*args, **kwargs):
    return None

run(["git", "status"], capture_output=True)
"""
        errors = self._check_code(code)
        self.assertNotIn("RP001", errors)


if __name__ == "__main__":
    unittest.main()

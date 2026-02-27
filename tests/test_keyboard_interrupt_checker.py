"""Unit tests for KeyboardInterrupt lint checker (KI003 rule)."""

import ast
import unittest

from clud.lint.keyboard_interrupt_checker import KeyboardInterruptChecker


class TestKI003Rule(unittest.TestCase):
    """Test KI003: KeyboardInterrupt caught without handle_keyboard_interrupt()."""

    def _check_code(self, code: str, file_path: str = "src/clud/agent/loop_executor.py") -> list[str]:
        """Parse code and return list of error codes found."""
        tree = ast.parse(code)
        checker = KeyboardInterruptChecker(file_path)
        checker.visit(tree)
        return [e.code for e in checker.errors]

    def test_bare_except_keyboard_interrupt_flagged(self) -> None:
        """Test that catching KeyboardInterrupt without handle_keyboard_interrupt is flagged."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    print("interrupted")
    raise
"""
        errors = self._check_code(code)
        self.assertIn("KI003", errors)

    def test_handle_keyboard_interrupt_call_not_flagged(self) -> None:
        """Test that using handle_keyboard_interrupt() in handler is not flagged."""
        code = """
try:
    do_work()
except KeyboardInterrupt as e:
    handle_keyboard_interrupt(cleanup_func, exc=e)
"""
        errors = self._check_code(code)
        self.assertNotIn("KI003", errors)

    def test_qualified_handle_keyboard_interrupt_not_flagged(self) -> None:
        """Test that util.handle_keyboard_interrupt() is also accepted."""
        code = """
try:
    do_work()
except KeyboardInterrupt as e:
    util.handle_keyboard_interrupt(cleanup_func, exc=e)
"""
        errors = self._check_code(code)
        self.assertNotIn("KI003", errors)

    def test_exempt_cli_entry_point(self) -> None:
        """Test that cli.py is exempt from KI003."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    print("interrupted")
"""
        errors = self._check_code(code, file_path="src/clud/cli.py")
        self.assertNotIn("KI003", errors)

    def test_exempt_agent_cli(self) -> None:
        """Test that agent_cli.py is exempt from KI003."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    print("interrupted")
"""
        errors = self._check_code(code, file_path="src/clud/agent_cli.py")
        self.assertNotIn("KI003", errors)

    def test_exempt_util_definition(self) -> None:
        """Test that util/__init__.py is exempt (it defines handle_keyboard_interrupt)."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    raise
"""
        errors = self._check_code(code, file_path="src/clud/util/__init__.py")
        self.assertNotIn("KI003", errors)

    def test_exempt_test_files(self) -> None:
        """Test that test files are exempt from KI003."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    pass
"""
        errors = self._check_code(code, file_path="tests/test_something.py")
        self.assertNotIn("KI003", errors)

    def test_non_keyboard_interrupt_not_flagged(self) -> None:
        """Test that catching non-KeyboardInterrupt exceptions is not flagged."""
        code = """
try:
    do_work()
except ValueError:
    pass
"""
        errors = self._check_code(code)
        self.assertNotIn("KI003", errors)

    def test_ki003_message_mentions_thread_safety(self) -> None:
        """Test that KI003 error message mentions thread-safe handling."""
        code = """
try:
    do_work()
except KeyboardInterrupt:
    raise
"""
        tree = ast.parse(code)
        checker = KeyboardInterruptChecker("src/clud/agent/loop_executor.py")
        checker.visit(tree)
        ki003_errors = [e for e in checker.errors if e.code == "KI003"]
        self.assertTrue(len(ki003_errors) > 0)
        self.assertIn("thread-safe", ki003_errors[0].message)


if __name__ == "__main__":
    unittest.main()

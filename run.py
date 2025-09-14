#!/usr/bin/env -S uv run python
"""
Manual testing runner for clud package.
Provides comprehensive testing utilities for development and debugging.
"""
import argparse
import json
import os
import platform
import subprocess
import sys
import time
import webbrowser
from pathlib import Path
from threading import Thread
from typing import Any, Dict, List, Optional


class TestRunner:
    """Comprehensive test runner for clud package."""

    def __init__(self):
        self.test_results: List[Dict[str, Any]] = []
        self.verbose = False

    def log(self, message: str, level: str = "INFO") -> None:
        """Log a message with optional verbosity control."""
        if self.verbose or level in ["ERROR", "SUCCESS"]:
            prefix = f"[{level}]" if level != "INFO" else ""
            print(f"{prefix} {message}")

    def run_test(self, test_name: str, test_func, *args, **kwargs) -> Dict[str, Any]:
        """Run a single test and record results."""
        self.log(f"Running test: {test_name}")
        start_time = time.time()

        try:
            result = test_func(*args, **kwargs)
            duration = time.time() - start_time
            test_result = {
                "name": test_name,
                "status": "PASS",
                "duration": duration,
                "result": result,
                "error": None
            }
            self.log(f"✓ {test_name} passed ({duration:.2f}s)", "SUCCESS")
        except Exception as e:
            duration = time.time() - start_time
            test_result = {
                "name": test_name,
                "status": "FAIL",
                "duration": duration,
                "result": None,
                "error": str(e)
            }
            self.log(f"✗ {test_name} failed: {e}", "ERROR")

        self.test_results.append(test_result)
        return test_result

    def test_cli_imports(self) -> bool:
        """Test if CLI module imports correctly."""
        try:
            from clud import cli
            return True
        except ImportError as e:
            raise Exception(f"Failed to import clud.cli: {e}")

    def test_cli_argument_parser(self) -> Dict[str, Any]:
        """Test CLI argument parser creation and basic functionality."""
        from clud.cli import create_parser

        parser = create_parser()

        # Test default arguments
        args = parser.parse_args([])
        defaults = {
            "path": None,
            "no_dangerous": False,
            "ssh_keys": False,
            "shell": "bash",
            "profile": "python",
            "port": 8743,
            "ui": False
        }

        for key, expected in defaults.items():
            if getattr(args, key) != expected:
                raise Exception(f"Default value mismatch for {key}: got {getattr(args, key)}, expected {expected}")

        # Test UI flag parsing
        ui_args = parser.parse_args(["--ui"])
        if not ui_args.ui:
            raise Exception("--ui flag not parsed correctly")

        return {"defaults_correct": True, "ui_flag_works": True}

    def test_path_validation(self) -> Dict[str, Any]:
        """Test path validation functionality."""
        from clud.cli import validate_path

        # Test current directory (should always exist)
        current_path = validate_path(None)
        if not current_path.exists():
            raise Exception("Current directory validation failed")

        # Test explicit current directory
        explicit_current = validate_path(".")
        if not explicit_current.exists():
            raise Exception("Explicit current directory validation failed")

        # Test nonexistent path
        try:
            validate_path("/nonexistent/path/12345")
            raise Exception("Nonexistent path should have raised ValidationError")
        except Exception as e:
            if "ValidationError" not in str(type(e)):
                # Re-import to check the actual error type
                from clud.cli import ValidationError
                if not isinstance(e, ValidationError):
                    raise Exception("Wrong exception type for nonexistent path")

        return {"current_dir_ok": True, "explicit_current_ok": True, "nonexistent_handled": True}

    def test_docker_availability(self) -> Dict[str, Any]:
        """Test Docker availability check."""
        from clud.cli import check_docker_available

        docker_available = check_docker_available()
        docker_version = None

        if docker_available:
            try:
                result = subprocess.run(["docker", "version", "--format", "{{.Client.Version}}"],
                                      capture_output=True, text=True, check=True, timeout=5)
                docker_version = result.stdout.strip()
            except Exception:
                pass

        return {
            "docker_available": docker_available,
            "docker_version": docker_version
        }

    def test_api_key_validation(self) -> Dict[str, Any]:
        """Test API key validation logic."""
        from clud.cli import validate_api_key

        # Test valid key format
        valid_key = "sk-ant-" + "x" * 50
        if not validate_api_key(valid_key):
            raise Exception("Valid API key format rejected")

        # Test invalid formats
        invalid_keys = [
            "",
            "invalid",
            "sk-ant-",
            "sk-ant-x",  # too short
            "wrong-prefix-" + "x" * 50
        ]

        for invalid_key in invalid_keys:
            if validate_api_key(invalid_key):
                raise Exception(f"Invalid key accepted: {invalid_key}")

        return {"valid_key_accepted": True, "invalid_keys_rejected": len(invalid_keys)}

    def test_port_utilities(self) -> Dict[str, Any]:
        """Test port availability checking."""
        from clud.cli import is_port_available, find_available_port

        # Test port 80 (likely unavailable on most systems)
        port_80_available = is_port_available(80)

        # Find available port starting from 8743
        available_port = find_available_port(8743)

        # Verify the found port is actually available
        if not is_port_available(available_port):
            raise Exception(f"find_available_port returned unavailable port: {available_port}")

        return {
            "port_80_available": port_80_available,
            "found_available_port": available_port,
            "port_check_consistent": True
        }

    def test_config_directory(self) -> Dict[str, Any]:
        """Test configuration directory operations."""
        from clud.cli import get_clud_config_dir

        config_dir = get_clud_config_dir()

        if not config_dir.exists():
            raise Exception("Config directory was not created")

        if not config_dir.is_dir():
            raise Exception("Config directory is not a directory")

        expected_path = Path.home() / ".clud"
        if config_dir != expected_path:
            raise Exception(f"Config directory path mismatch: {config_dir} != {expected_path}")

        return {
            "config_dir_created": True,
            "config_dir_path": str(config_dir),
            "is_directory": True
        }

    def test_environment_integration(self) -> Dict[str, Any]:
        """Test environment variable integration."""
        # Test current environment
        python_version = sys.version_info
        platform_info = platform.platform()

        # Test if we can set and read environment variables
        test_var = "CLUD_TEST_VAR"
        test_value = "test_value_12345"

        os.environ[test_var] = test_value
        read_value = os.environ.get(test_var)

        # Clean up
        if test_var in os.environ:
            del os.environ[test_var]

        if read_value != test_value:
            raise Exception("Environment variable read/write test failed")

        return {
            "python_version": f"{python_version.major}.{python_version.minor}.{python_version.micro}",
            "platform": platform_info,
            "env_var_test": True
        }

    def test_ui_mode_simulation(self) -> Dict[str, Any]:
        """Test UI mode components without actually starting a container."""
        from clud.cli import create_parser, validate_path

        parser = create_parser()

        # Simulate UI mode arguments
        ui_args = parser.parse_args(["--ui", "--port", "9999"])

        if not ui_args.ui:
            raise Exception("UI mode not enabled")

        if ui_args.port != 9999:
            raise Exception("Custom port not set correctly")

        # Test path validation for current directory
        project_path = validate_path(ui_args.path)

        return {
            "ui_mode_enabled": True,
            "custom_port_set": True,
            "project_path_resolved": str(project_path)
        }

    def run_all_tests(self) -> Dict[str, Any]:
        """Run all available tests."""
        tests = [
            ("CLI Imports", self.test_cli_imports),
            ("CLI Argument Parser", self.test_cli_argument_parser),
            ("Path Validation", self.test_path_validation),
            ("Docker Availability", self.test_docker_availability),
            ("API Key Validation", self.test_api_key_validation),
            ("Port Utilities", self.test_port_utilities),
            ("Config Directory", self.test_config_directory),
            ("Environment Integration", self.test_environment_integration),
            ("UI Mode Simulation", self.test_ui_mode_simulation),
        ]

        for test_name, test_func in tests:
            self.run_test(test_name, test_func)

        # Summary
        total_tests = len(self.test_results)
        passed_tests = sum(1 for r in self.test_results if r["status"] == "PASS")
        failed_tests = total_tests - passed_tests
        total_duration = sum(r["duration"] for r in self.test_results)

        summary = {
            "total_tests": total_tests,
            "passed": passed_tests,
            "failed": failed_tests,
            "success_rate": (passed_tests / total_tests * 100) if total_tests > 0 else 0,
            "total_duration": total_duration
        }

        self.log(f"\nTest Summary:", "INFO")
        self.log(f"Total: {total_tests}, Passed: {passed_tests}, Failed: {failed_tests}", "INFO")
        self.log(f"Success Rate: {summary['success_rate']:.1f}%", "SUCCESS" if failed_tests == 0 else "ERROR")
        self.log(f"Total Duration: {total_duration:.2f}s", "INFO")

        return summary

    def run_specific_test(self, test_name: str) -> Optional[Dict[str, Any]]:
        """Run a specific test by name."""
        test_mapping = {
            "imports": ("CLI Imports", self.test_cli_imports),
            "parser": ("CLI Argument Parser", self.test_cli_argument_parser),
            "path": ("Path Validation", self.test_path_validation),
            "docker": ("Docker Availability", self.test_docker_availability),
            "apikey": ("API Key Validation", self.test_api_key_validation),
            "port": ("Port Utilities", self.test_port_utilities),
            "config": ("Config Directory", self.test_config_directory),
            "env": ("Environment Integration", self.test_environment_integration),
            "ui": ("UI Mode Simulation", self.test_ui_mode_simulation),
        }

        if test_name.lower() in test_mapping:
            name, func = test_mapping[test_name.lower()]
            return self.run_test(name, func)
        else:
            available = ", ".join(test_mapping.keys())
            self.log(f"Unknown test: {test_name}. Available tests: {available}", "ERROR")
            return None

    def interactive_mode(self) -> None:
        """Run interactive test selection mode."""
        while True:
            print("\n=== Clud Manual Testing ===")
            print("1. Run all tests")
            print("2. Run specific test")
            print("3. Start UI mode (with browser)")
            print("4. Test CLI directly")
            print("5. Show test results")
            print("6. Export results to JSON")
            print("0. Exit")

            choice = input("\nSelect option: ").strip()

            if choice == "0":
                break
            elif choice == "1":
                self.run_all_tests()
            elif choice == "2":
                test_name = input("Enter test name (imports/parser/path/docker/apikey/port/config/env/ui): ").strip()
                self.run_specific_test(test_name)
            elif choice == "3":
                self.start_ui_mode()
            elif choice == "4":
                self.test_cli_directly()
            elif choice == "5":
                self.show_test_results()
            elif choice == "6":
                self.export_results()
            else:
                print("Invalid choice")

    def start_ui_mode(self) -> None:
        """Start clud in UI mode with browser launch."""
        try:
            print("Starting clud in UI mode...")

            # Start browser launch in background thread
            browser_thread = Thread(target=self._launch_browser, daemon=True)
            browser_thread.start()

            # Start clud with --ui flag
            subprocess.run("uv run python -m clud.cli --ui", shell=True, check=True)
        except KeyboardInterrupt:
            print("\nShutting down UI mode...")
        except subprocess.CalledProcessError as e:
            print(f"Error starting clud UI: {e}")

    def _launch_browser(self) -> None:
        """Launch browser after delay for UI mode."""
        time.sleep(3)
        try:
            webbrowser.open("http://localhost:8743")
        except Exception as e:
            print(f"Could not open browser: {e}")

    def test_cli_directly(self) -> None:
        """Test CLI with custom arguments."""
        print("\nDirect CLI Testing")
        print("Enter CLI arguments (or 'back' to return):")

        while True:
            args_input = input("clud ").strip()
            if args_input.lower() == "back":
                break
            if not args_input:
                continue

            try:
                subprocess.run(f"uv run python -m clud.cli {args_input}", shell=True)
            except KeyboardInterrupt:
                print("\nCommand interrupted")
            except Exception as e:
                print(f"Error: {e}")

    def show_test_results(self) -> None:
        """Display detailed test results."""
        if not self.test_results:
            print("No test results available. Run tests first.")
            return

        print(f"\n=== Test Results ({len(self.test_results)} tests) ===")
        for result in self.test_results:
            status_symbol = "✓" if result["status"] == "PASS" else "✗"
            print(f"{status_symbol} {result['name']} ({result['duration']:.2f}s)")
            if result["error"]:
                print(f"  Error: {result['error']}")
            if self.verbose and result["result"]:
                print(f"  Result: {result['result']}")

    def export_results(self) -> None:
        """Export test results to JSON file."""
        if not self.test_results:
            print("No test results to export.")
            return

        filename = f"clud_test_results_{int(time.time())}.json"
        try:
            with open(filename, "w") as f:
                json.dump({
                    "timestamp": time.time(),
                    "platform": platform.platform(),
                    "python_version": sys.version,
                    "results": self.test_results
                }, f, indent=2)
            print(f"Results exported to {filename}")
        except Exception as e:
            print(f"Failed to export results: {e}")


def create_parser() -> argparse.ArgumentParser:
    """Create argument parser for test runner."""
    parser = argparse.ArgumentParser(
        description="Manual testing runner for clud package",
        formatter_class=argparse.RawDescriptionHelpFormatter
    )

    parser.add_argument("--test", help="Run specific test by name")
    parser.add_argument("--all", action="store_true", help="Run all tests")
    parser.add_argument("--ui", action="store_true", help="Start UI mode with browser")
    parser.add_argument("--interactive", "-i", action="store_true", help="Interactive mode")
    parser.add_argument("--verbose", "-v", action="store_true", help="Verbose output")
    parser.add_argument("--export", help="Export results to JSON file")

    return parser


def main() -> int:
    """Main entry point for test runner."""
    parser = create_parser()
    args = parser.parse_args()

    runner = TestRunner()
    runner.verbose = args.verbose

    try:
        if args.interactive:
            runner.interactive_mode()
        elif args.all:
            runner.run_all_tests()
            if args.export:
                runner.export_results()
        elif args.test:
            runner.run_specific_test(args.test)
        elif args.ui:
            runner.start_ui_mode()
        else:
            # Default to interactive mode if no specific action
            runner.interactive_mode()

        return 0

    except KeyboardInterrupt:
        print("\nOperation cancelled.")
        return 1
    except Exception as e:
        print(f"Unexpected error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
#!/usr/bin/env -S uv run python
"""Integration test runner for clud package."""

import subprocess
import sys
import time
from pathlib import Path


def run_test_file(test_file: Path) -> tuple[bool, str, str]:
    """Run a single test file and return success status and output."""
    try:
        result = subprocess.run(
            ["uv", "run", "python", str(test_file)],
            capture_output=True,
            text=True,
            timeout=300  # 5 minute timeout per test
        )

        success = result.returncode == 0
        return success, result.stdout, result.stderr

    except subprocess.TimeoutExpired:
        return False, "", "Test timed out after 5 minutes"
    except Exception as e:
        return False, "", f"Failed to run test: {e}"


def main():
    """Run all integration tests."""
    print("=" * 80)
    print("CLUD INTEGRATION TEST RUNNER")
    print("=" * 80)

    test_dir = Path(__file__).parent / "tests" / "integration"

    # List of test files to run
    test_files = [
        test_dir / "test_simple_docker.py",
        test_dir / "test_docker_cli_exit.py",
        test_dir / "test_web_server.py",
    ]

    results = []
    total_start_time = time.time()

    for test_file in test_files:
        if not test_file.exists():
            print(f"! Test file not found: {test_file}")
            continue

        print(f"\nRunning: {test_file.name}")
        print("-" * 60)

        start_time = time.time()
        success, stdout, stderr = run_test_file(test_file)
        duration = time.time() - start_time

        results.append({
            "file": test_file.name,
            "success": success,
            "duration": duration,
            "stdout": stdout,
            "stderr": stderr
        })

        if success:
            print("OK Test passed!")
        else:
            print("X Test failed!")

        print(f"Duration: {duration:.2f}s")

        # Show output for failed tests
        if not success:
            print("\nSTDOUT:")
            print(stdout)
            if stderr:
                print("\nSTDERR:")
                print(stderr)

    # Summary
    total_duration = time.time() - total_start_time
    passed = sum(1 for r in results if r["success"])
    total = len(results)

    print("\n" + "=" * 80)
    print("INTEGRATION TEST SUMMARY")
    print("=" * 80)

    for result in results:
        status = "PASS" if result["success"] else "FAIL"
        print(f"{status:4} {result['file']:30} ({result['duration']:.2f}s)")

    print("-" * 80)
    print(f"Total: {total}, Passed: {passed}, Failed: {total - passed}")
    print(f"Success Rate: {(passed / total * 100):.1f}%" if total > 0 else "N/A")
    print(f"Total Duration: {total_duration:.2f}s")

    if passed == total and total > 0:
        print("\nSUCCESS: All integration tests passed!")
        print("\nThis proves that:")
        print("- Docker containers can be started and stopped reliably")
        print("- Web servers can run in containers and be accessed")
        print("- Container exit functionality works properly")
        print("- The clud development environment infrastructure is working")
        return 0
    else:
        print(f"\nFAILED: {total - passed} out of {total} tests failed")
        return 1


if __name__ == "__main__":
    sys.exit(main())
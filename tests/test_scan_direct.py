"""Direct test of scan_git_changes functionality."""

from pathlib import Path
from typing import Any, cast

from clud.webui.api import DiffHandler


def main() -> None:
    """Test scan_git_changes directly."""
    test_project = Path(__file__).parent / "artifacts" / "test_diff_project"

    print(f"Testing with project: {test_project}")
    print(f"Project exists: {test_project.exists()}")
    print(f"Is git repo: {(test_project / '.git').exists()}")

    handler = DiffHandler()

    try:
        count = handler.scan_git_changes(str(test_project))
        print(f"\nScan found {count} changed files")

        # Get the diff tree
        tree_data = handler.get_diff_tree(str(test_project))
        print(f"\nTree data: {tree_data}")

        # Check if README.md is in the tree
        files_raw: Any = tree_data.get("files", [])
        if isinstance(files_raw, list):
            files_list = cast(list[dict[str, Any]], files_raw)
            print(f"\nFiles in tree: {len(files_list)}")
            for file_info in files_list:
                print(f"  - {file_info['path']}: +{file_info['additions']} -{file_info['deletions']}")

    except Exception as e:
        print(f"Error: {e}")
        import traceback

        traceback.print_exc()


if __name__ == "__main__":
    main()
